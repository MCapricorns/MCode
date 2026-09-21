//! Agent double-loop integration tests driven by the scripted
//! `LocalProvider` (zero
//! network):
//!
//! 1. Single-turn text reply stops.
//! 2. Multi-turn tool loop: tool call → execute → write back → second
//!    response stops.
//! 3. Steer: queued mid-stream, jumps the queue to become the next user
//!    input after the current response.
//! 4. Follow-up: delivered when the agent is about to stop.
//! 5. Abort: `CancellationToken` / `agent.abort()` mid-turn; state
//!    stays consistent (no half `TurnEnded::Completed`).
//! 6. Tool errors (unknown tool, failing tool, truncated length) become
//!    `is_error` tool results; the loop continues. Registered tools
//!    dispatch without a permission callback.
//!
//! Plus event-sequence assertions (subscribing to the broadcast bus)
//! and queue-mode drain semantics.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mycode_agent::{
    Agent, AgentConfig, GateResult, HookRunner, QueueMode, TurnEnv, build_system_prompt,
};
use mycode_core::events::{AgentEvent, MessageDelta, TurnOutcome};
use mycode_core::message::{
    AssistantMessage, ContentBlock, Message, StopReason, ToolCall, UserMessage,
};
use mycode_core::{
    EventStream, MAX_REQUEST_ENCODED_BYTES, Provider, ProviderError, ProviderErrorKind, Request,
    StreamEvent,
};

mod common;
use common::local_provider::{LocalProvider, LocalTurn};
use mycode_tools::{
    FileAccess, FindTool, ReadTool, Tool, ToolCtx, ToolDyn, ToolError, ToolRegistry, ToolResult,
    ToolStream, WriteTool, read_file_async,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// A constant inter-event delay so concurrent test tasks (steerer,
/// canceller) win the race deterministically on the single-threaded
/// test runtime.
const DELAY: Duration = Duration::from_millis(2);
/// Absolute guard for dispatcher regressions and their spawned producer cleanup.
const DISPATCH_TEST_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------
// Test tools
// ---------------------------------------------------------------------

/// Echoes its `text` argument back.
struct EchoTool;

#[derive(Deserialize, JsonSchema)]
struct EchoArgs {
    /// Text to echo back.
    text: String,
}

#[async_trait]
impl Tool for EchoTool {
    type Args = EchoArgs;
    type Output = ();

    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Echo text back (test fixture)."
    }
    async fn execute(
        &self,
        args: Self::Args,
        _ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::text(format!("echo: {}", args.text)))
    }
}

/// Always fails with an execution error.
struct FailingTool;

#[derive(Deserialize, JsonSchema)]
struct NoArgs {}

#[async_trait]
impl Tool for FailingTool {
    type Args = NoArgs;
    type Output = ();
    fn name(&self) -> &str {
        "failing"
    }
    fn description(&self) -> &str {
        "Always fails (test fixture)."
    }
    async fn execute(
        &self,
        _args: Self::Args,
        _ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        Err(ToolError::Execution(
            "boom: intentional test failure".into(),
        ))
    }
}

/// Panics so dispatch must catch_unwind into an error result.
struct PanickingTool;

#[async_trait]
impl Tool for PanickingTool {
    type Args = NoArgs;
    type Output = ();
    fn name(&self) -> &str {
        "panicking"
    }
    fn description(&self) -> &str {
        "Panics (test fixture)."
    }
    async fn execute(
        &self,
        _args: Self::Args,
        _ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        panic!("intentional tool panic");
    }
}

/// Pushes two progress items, then returns.
struct ProgressTool;

#[async_trait]
impl Tool for ProgressTool {
    type Args = NoArgs;
    type Output = ();
    fn name(&self) -> &str {
        "progress"
    }
    fn description(&self) -> &str {
        "Emits progress, then completes (test fixture)."
    }
    async fn execute(
        &self,
        _args: Self::Args,
        _ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        out.progress("step 1");
        out.progress("step 2");
        Ok(ToolResult::text("progress done"))
    }
}

/// Sends its own terminal result before returning a different result.
struct SelfTerminatingTool;

#[async_trait]
impl Tool for SelfTerminatingTool {
    type Args = NoArgs;
    type Output = ();

    fn name(&self) -> &str {
        "self_terminating"
    }

    fn description(&self) -> &str {
        "Terminates its stream before returning (test fixture)."
    }

    async fn execute(
        &self,
        _args: Self::Args,
        _ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        out.progress("before streamed terminal");
        if !out.terminal(ToolResult::text("streamed result")) {
            return Err(ToolError::Execution(
                "self-terminating fixture lost the terminal claim".to_owned(),
            ));
        }
        Ok(ToolResult::text("returned result"))
    }
}

/// Floods progress from a clone until dispatch claims the terminal.
struct SustainedProgressTool {
    stop: Arc<AtomicBool>,
    producer: Mutex<Option<JoinHandle<()>>>,
}

impl SustainedProgressTool {
    fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            producer: Mutex::new(None),
        }
    }

    fn stop_producer(&self) {
        self.stop.store(true, Ordering::Release);
    }

    fn take_producer(&self) -> Option<JoinHandle<()>> {
        self.producer
            .lock()
            .expect("producer handle lock must not be poisoned")
            .take()
    }
}

#[async_trait]
impl Tool for SustainedProgressTool {
    type Args = NoArgs;
    type Output = ();

    fn name(&self) -> &str {
        "sustained_progress"
    }

    fn description(&self) -> &str {
        "Emits progress from a clone until terminal state closes (test fixture)."
    }

    async fn execute(
        &self,
        _args: Self::Args,
        _ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let producer = out.clone();
        let stop = Arc::clone(&self.stop);
        let ready = Arc::new(tokio::sync::Notify::new());
        let producer_ready = Arc::clone(&ready);
        let producer_task = tokio::task::spawn_blocking(move || {
            let mut sent = 0_usize;
            loop {
                if stop.load(Ordering::Acquire) {
                    producer_ready.notify_one();
                    break;
                }
                if !producer.progress(format!("sustained step {sent}")) {
                    producer_ready.notify_one();
                    break;
                }
                sent += 1;
                if sent == 1 {
                    producer_ready.notify_one();
                }
            }
        });
        *self
            .producer
            .lock()
            .expect("producer handle lock must not be poisoned") = Some(producer_task);
        ready.notified().await;
        Ok(ToolResult::text("sustained progress done"))
    }
}

// ---------------------------------------------------------------------
// Test rig: owns the ambient objects, builds a TurnEnv per call
// ---------------------------------------------------------------------

struct Rig {
    provider: LocalProvider,
    registry: ToolRegistry,
    hooks: HookRunner,
    events: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
}

impl Rig {
    fn new(provider: LocalProvider) -> Self {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        registry.register(Arc::new(FailingTool));
        registry.register(Arc::new(PanickingTool));
        registry.register(Arc::new(ProgressTool));
        Self {
            provider,
            registry,
            hooks: HookRunner::new(),
            events: broadcast::channel(256).0,
            cancel: CancellationToken::new(),
        }
    }

    fn env(&self) -> TurnEnv<'_> {
        TurnEnv::new(&self.provider, &self.registry, &self.hooks)
            .with_events(self.events.clone())
            .with_cancel(self.cancel.clone())
    }

    fn env_at(&self, cwd: std::path::PathBuf) -> TurnEnv<'_> {
        self.env().with_cwd(cwd)
    }
}

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

fn user(text: &str) -> Message {
    Message::User(UserMessage::text(text))
}

fn text_turn(text: &str) -> LocalTurn {
    LocalTurn::Message(AssistantMessage {
        blocks: vec![ContentBlock::Text(text.into())],
        usage: None,
        stop_reason: StopReason::Stop,
    })
}

fn tool_turn(text: &str, calls: Vec<(&str, &str, Value)>) -> LocalTurn {
    let mut blocks = vec![ContentBlock::Text(text.into())];
    for (id, name, args) in calls {
        blocks.push(ContentBlock::ToolCall(ToolCall::new(id, name, args)));
    }
    LocalTurn::Message(AssistantMessage {
        blocks,
        usage: None,
        stop_reason: StopReason::ToolUse,
    })
}

/// Collect events until the turn ends.
fn spawn_collector(rig: &Rig) -> JoinHandle<Vec<AgentEvent>> {
    let mut rx = rig.events.subscribe();
    tokio::spawn(async move {
        let mut out = Vec::new();
        while let Ok(event) = rx.recv().await {
            let done = matches!(event, AgentEvent::TurnEnded(_));
            out.push(event);
            if done {
                break;
            }
        }
        out
    })
}

/// Spawn a task that waits for the first streamed text delta and then
/// runs `f` (steer / follow-up / abort / cancel) mid-stream.
fn spawn_on_first_delta<F: FnOnce() + Send + 'static>(rig: &Rig, f: F) -> JoinHandle<()> {
    let mut rx = rig.events.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            if matches!(event, AgentEvent::MessageDelta(MessageDelta::TextDelta(_))) {
                break;
            }
        }
        f();
    })
}

/// Spawn a task that waits for the first `ToolCompleted` event and
/// then runs `f` (abort mid-dispatch of a multi-call response).
fn spawn_on_first_tool_completed<F: FnOnce() + Send + 'static>(rig: &Rig, f: F) -> JoinHandle<()> {
    let mut rx = rig.events.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            if matches!(event, AgentEvent::ToolCompleted { .. }) {
                break;
            }
        }
        f();
    })
}

fn position(events: &[AgentEvent], pred: impl Fn(&AgentEvent) -> bool, what: &str) -> usize {
    events
        .iter()
        .position(pred)
        .unwrap_or_else(|| panic!("missing event: {what}; events: {events:#?}"))
}

fn tool_result(events: &[AgentEvent]) -> mycode_core::message::ToolResultMessage {
    events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCompleted { result, .. } => Some(result.clone()),
            _ => None,
        })
        .expect("a ToolCompleted event must exist")
}

// ---------------------------------------------------------------------
// 1. Single-turn text reply stops
// ---------------------------------------------------------------------

#[tokio::test]
async fn single_text_reply_stops_and_streams_events() {
    let rig = Rig::new(LocalProvider::new(vec![text_turn("Hello there!")]));
    let mut agent = Agent::new(AgentConfig::new().with_system_prompt("be terse"));
    let collector = spawn_collector(&rig);

    let outcome = agent
        .prompt(user("hi"), &rig.env())
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(
        agent.state().messages,
        vec![
            user("hi"),
            Message::Assistant(AssistantMessage {
                blocks: vec![ContentBlock::Text("Hello there!".into())],
                usage: None,
                stop_reason: StopReason::Stop,
            })
        ]
    );
    assert!(!agent.state().is_streaming);

    // Request shape: system prompt, history, registry specs flow in.
    let requests = rig.provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].system_prompt, vec!["be terse".to_string()]);
    assert_eq!(requests[0].messages, vec![user("hi")]);
    assert!(requests[0].tools.iter().any(|spec| spec.name == "echo"));

    // Event order: TurnStarted → MessageAdded(user) → deltas →
    // MessageAdded(assistant) → TurnEnded(Completed).
    let events = collector.await.expect("collector must finish");
    assert_eq!(events.first(), Some(&AgentEvent::TurnStarted));
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Completed))
    );
    let user_pos = position(
        &events,
        |e| matches!(e, AgentEvent::MessageAdded(Message::User(_))),
        "MessageAdded(user)",
    );
    let delta_pos = position(
        &events,
        |e| matches!(e, AgentEvent::MessageDelta(MessageDelta::TextDelta(_))),
        "TextDelta",
    );
    let assistant_pos = position(
        &events,
        |e| matches!(e, AgentEvent::MessageAdded(Message::Assistant(_))),
        "MessageAdded(assistant)",
    );
    assert_eq!(user_pos, 1);
    assert!(user_pos < delta_pos);
    assert!(delta_pos < assistant_pos);
    // No tool events in a pure text turn.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolStarted { .. }))
    );
}

#[tokio::test]
async fn provider_request_receives_canonical_builtin_specs() {
    let provider = LocalProvider::new(vec![text_turn("done")]);
    let registry = ToolRegistry::new();
    mycode_tools::builtin::register_builtins(&registry);
    let hooks = HookRunner::new();
    let env = TurnEnv::new(&provider, &registry, &hooks);
    let mut agent = Agent::new(AgentConfig::new());

    let outcome = agent
        .prompt(user("inspect tools"), &env)
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    let names: Vec<&str> = requests[0]
        .tools
        .iter()
        .map(|spec| spec.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["edit", "exec", "find", "grep", "read", "shell", "write"]
    );
    assert!(!names.contains(&"bash"));
}

// ---------------------------------------------------------------------
// 2. Multi-turn tool loop
// ---------------------------------------------------------------------

#[tokio::test]
async fn tool_call_loop_executes_writes_back_and_stops() {
    let rig = Rig::new(LocalProvider::new(vec![
        tool_turn("let me echo", vec![("c1", "echo", json!({"text": "hi"}))]),
        text_turn("I echoed the text."),
    ]));
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);

    let outcome = agent
        .prompt(user("run the echo"), &rig.env())
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    let messages = &agent.state().messages;
    assert_eq!(messages.len(), 4); // user, assistant(call), result, assistant(final)
    let Message::ToolResult(result) = &messages[2] else {
        panic!("message 3 must be the tool result: {messages:#?}");
    };
    assert_eq!(result.tool_call_id, "c1");
    assert!(!result.is_error);
    assert_eq!(result.content, vec![ContentBlock::Text("echo: hi".into())]);

    // The second request carries the full history including the result.
    let requests = rig.provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].system_prompt,
        vec![build_system_prompt(&rig.registry)]
    );
    assert_eq!(requests[1].system_prompt, requests[0].system_prompt);
    assert_eq!(requests[1].messages.len(), 3);
    assert!(matches!(&requests[1].messages[2], Message::ToolResult(_)));
    assert_eq!(requests[1].messages, messages[..3]);

    let events = collector.await.expect("collector must finish");
    let started = position(
        &events,
        |e| matches!(e, AgentEvent::ToolStarted { call_id, name } if call_id.as_str() == "c1" && name == "echo"),
        "ToolStarted(c1)",
    );
    let completed = position(
        &events,
        |e| matches!(e, AgentEvent::ToolCompleted { result, .. } if !result.is_error),
        "ToolCompleted(c1)",
    );
    let result_added = position(
        &events,
        |e| matches!(e, AgentEvent::MessageAdded(Message::ToolResult(_))),
        "MessageAdded(ToolResult)",
    );
    assert!(started < completed);
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Completed))
    );
    let _ = result_added;
}

// ---------------------------------------------------------------------
// 3. Steer: queued mid-stream, becomes the next user input
// ---------------------------------------------------------------------

#[tokio::test]
async fn steer_jumps_the_queue_after_the_current_response() {
    let rig = Rig::new(
        LocalProvider::new(vec![
            tool_turn(
                "fetching the value",
                vec![("c1", "echo", json!({"text": "data"}))],
            ),
            text_turn("Understood, pivoting now."),
        ])
        .with_delay(DELAY),
    );
    let mut agent = Agent::new(AgentConfig::new());
    let handle = agent.handle();
    let collector = spawn_collector(&rig);
    let steerer = spawn_on_first_delta(&rig, move || {
        handle.steer(user("stop, do this instead"));
    });

    let outcome = agent
        .prompt(user("compute something"), &rig.env())
        .await
        .expect("prompt must succeed");
    steerer.await.expect("steerer must finish");

    assert_eq!(outcome, TurnOutcome::Steered);

    // History: user, assistant(call), result, steer-user, final assistant.
    let messages = &agent.state().messages;
    assert_eq!(messages.len(), 5);
    assert_eq!(messages[3], user("stop, do this instead"));
    assert!(matches!(messages[4], Message::Assistant(_)));

    // The steer message was injected as the next user input before the
    // second response — the queue-jump assertion.
    let requests = rig.provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages, messages[..4]);
    assert_eq!(requests[1].messages[3], user("stop, do this instead"));
    assert!(agent.state().messages().iter().all(|_| true)); // sanity access

    let events = collector.await.expect("collector must finish");
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Steered))
    );
    // The tool from response 1 still executed before the steer landed.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCompleted { .. }))
    );
}

// ---------------------------------------------------------------------
// 4. Follow-up: continues the agent that is about to stop
// ---------------------------------------------------------------------

#[tokio::test]
async fn follow_up_continues_when_agent_would_stop() {
    let rig = Rig::new(
        LocalProvider::new(vec![
            text_turn("First answer, complete."),
            text_turn("Follow-up answer, also done."),
        ])
        .with_delay(DELAY),
    );
    let mut agent = Agent::new(AgentConfig::new());
    let handle = agent.handle();
    let followupper = spawn_on_first_delta(&rig, move || {
        handle.follow_up(user("and also check the tests"));
    });

    let outcome = agent
        .prompt(user("answer the question"), &rig.env())
        .await
        .expect("prompt must succeed");
    followupper.await.expect("follow-up task must finish");

    // Follow-ups do not mark the turn as steered.
    assert_eq!(outcome, TurnOutcome::Completed);

    let messages = &agent.state().messages;
    assert_eq!(messages.len(), 4); // user, assistant, follow-up user, assistant
    assert_eq!(messages[2], user("and also check the tests"));

    let requests = rig.provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages[2], user("and also check the tests"));
}

// ---------------------------------------------------------------------
// 5. Abort: cancellation ends the turn with consistent state
// ---------------------------------------------------------------------

#[tokio::test]
async fn abort_via_env_cancel_mid_stream_keeps_state_consistent() {
    let long = "x".repeat(600); // many shards → ample cancel window
    let rig = Rig::new(LocalProvider::new(vec![text_turn(&long)]).with_delay(DELAY));
    let cancel = rig.cancel.clone();
    let canceller = spawn_on_first_delta(&rig, move || cancel.cancel());
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);

    let outcome = agent
        .prompt(user("an essay please"), &rig.env())
        .await
        .expect("abort is a normal outcome, not an error");
    canceller.await.expect("canceller must finish");

    assert_eq!(outcome, TurnOutcome::Aborted);
    assert!(!agent.state().is_streaming);
    // No partial assistant message enters the history.
    assert_eq!(agent.state().messages, vec![user("an essay please")]);

    let events = collector.await.expect("collector must finish");
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Aborted))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnEnded(TurnOutcome::Completed)))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::MessageAdded(Message::Assistant(_))))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Error(_))),
        "cancellation is an aborted outcome, not a provider error: {events:#?}"
    );

    // The agent recovers: a fresh caller token starts a fresh turn
    // (the original env token stays cancelled, by design).
    rig.provider.push_turn(text_turn("recovered"));
    let outcome = agent
        .prompt(
            user("try again"),
            &rig.env().with_cancel(CancellationToken::new()),
        )
        .await
        .expect("second prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    // user1, user2, recovered assistant (no partial aborted response).
    assert_eq!(agent.state().messages.len(), 3);
}

#[tokio::test]
async fn abort_via_agent_handle_mid_stream() {
    let long = "x".repeat(600);
    let rig = Rig::new(LocalProvider::new(vec![text_turn(&long)]).with_delay(DELAY));
    let mut agent = Agent::new(AgentConfig::new());
    let handle = agent.handle();
    let canceller = spawn_on_first_delta(&rig, move || handle.abort());

    let outcome = agent
        .prompt(user("an essay please"), &rig.env())
        .await
        .expect("abort is a normal outcome");
    canceller.await.expect("canceller must finish");

    assert_eq!(outcome, TurnOutcome::Aborted);
    assert_eq!(agent.state().messages.len(), 1);

    // abort() while idle is a no-op.
    agent.abort();
    rig.provider.push_turn(text_turn("fine"));
    let outcome = agent
        .prompt(user("again"), &rig.env())
        .await
        .expect("prompt after idle abort must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
}

/// Aborting mid-dispatch of a multi-call response must still answer
/// every `tool_call` in the history (the OpenAI wire format requires
/// one tool message per call id after an assistant `tool_calls`
/// message; chat-completions history has no pairing guard).
#[tokio::test]
async fn abort_mid_multi_call_answers_every_tool_call() {
    let rig = Rig::new(
        LocalProvider::new(vec![tool_turn(
            "three calls",
            vec![
                ("c1", "echo", json!({"text": "one"})),
                ("c2", "echo", json!({"text": "two"})),
                ("c3", "echo", json!({"text": "three"})),
            ],
        )])
        .with_delay(DELAY),
    );
    let cancel = rig.cancel.clone();
    let canceller = spawn_on_first_tool_completed(&rig, move || cancel.cancel());
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);

    let outcome = agent
        .prompt(user("run three calls"), &rig.env())
        .await
        .expect("abort is a normal outcome, not an error");
    canceller.await.expect("canceller must finish");

    assert_eq!(outcome, TurnOutcome::Aborted);

    // History: user, assistant(3 calls), then exactly one tool result
    // per call id, in call order — regardless of where the abort cut
    // the dispatch short. The calls after the cut are synthesized
    // cancellation results.
    let messages = &agent.state().messages;
    assert_eq!(messages.len(), 5);
    let Message::Assistant(assistant) = &messages[1] else {
        panic!("message 2 must be the assistant message: {messages:#?}");
    };
    let call_ids: Vec<String> = assistant
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call.id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(call_ids, vec!["c1", "c2", "c3"]);
    for (offset, id) in call_ids.iter().enumerate() {
        let Message::ToolResult(result) = &messages[2 + offset] else {
            panic!(
                "message {} must be a tool result: {messages:#?}",
                3 + offset
            );
        };
        assert_eq!(&result.tool_call_id, id);
    }
    // c1 completed before the abort fired.
    let Message::ToolResult(first) = &messages[2] else {
        panic!("must be a result")
    };
    assert!(!first.is_error);
    // The undispatched tail is a cancellation error result.
    let Message::ToolResult(last) = &messages[4] else {
        panic!("must be a result")
    };
    assert!(last.is_error);
    let ContentBlock::Text(text) = &last.content[0] else {
        panic!("error content must be text: {last:#?}");
    };
    assert!(text.text.contains("aborted"));

    let events = collector.await.expect("collector must finish");
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Aborted))
    );

    // The next turn sends this history to the provider as-is: the
    // assistant `tool_calls` message is immediately followed by one
    // tool result per call id — the wire invariant the OpenAI provider
    // relies on.
    let aborted_history: Vec<Message> = agent.state().messages.clone();
    rig.provider.push_turn(text_turn("recovered"));
    let outcome = agent
        .prompt(
            user("try again"),
            &rig.env().with_cancel(CancellationToken::new()),
        )
        .await
        .expect("second prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    let requests = rig.provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let second = &requests[1].messages;
    assert_eq!(second.len(), 6); // user, assistant(3 calls), 3 results, user2
    assert_eq!(&second[1], &aborted_history[1]);
    for (offset, id) in call_ids.iter().enumerate() {
        let Message::ToolResult(result) = &second[2 + offset] else {
            panic!("wire history must answer every call");
        };
        assert_eq!(&result.tool_call_id, id);
    }
}

// ---------------------------------------------------------------------
// 6. Tool errors as is_error results; the loop continues
// ---------------------------------------------------------------------

#[tokio::test]
async fn registered_tool_dispatches_without_permission_callback() {
    let provider = LocalProvider::new(vec![
        tool_turn("calling echo", vec![("c1", "echo", json!({"text": "hi"}))]),
        text_turn("echoed."),
    ]);
    let registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));
    let hooks = HookRunner::new();
    let events = broadcast::channel(256).0;
    // TurnEnv::new takes only provider, tools, and hooks — no permission
    // engine, prompt, or grant state.
    let env = TurnEnv::new(&provider, &registry, &hooks).with_events(events.clone());
    let mut rx = events.subscribe();
    let collector = tokio::spawn(async move {
        let mut out = Vec::new();
        while let Ok(event) = rx.recv().await {
            let done = matches!(event, AgentEvent::TurnEnded(_));
            out.push(event);
            if done {
                break;
            }
        }
        out
    });
    let mut agent = Agent::new(AgentConfig::new());

    let outcome = agent
        .prompt(user("echo something"), &env)
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(!result.is_error, "{result:#?}");
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("result content must be text: {result:#?}");
    };
    assert_eq!(text.text, "echo: hi");
    assert_eq!(provider.recorded_requests().len(), 2);
}

#[tokio::test]
async fn tool_progress_streams_between_start_and_completion() {
    let rig = Rig::new(LocalProvider::new(vec![
        tool_turn(
            "running the progress tool",
            vec![("c1", "progress", json!({}))],
        ),
        text_turn("All steps finished."),
    ]));
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);

    let outcome = agent
        .prompt(user("run steps"), &rig.env())
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    // Live progress was forwarded between start and completion.
    let started = position(
        &events,
        |e| matches!(e, AgentEvent::ToolStarted { call_id, .. } if call_id.as_str() == "c1"),
        "ToolStarted",
    );
    let step1 = position(
        &events,
        |e| matches!(e, AgentEvent::ToolProgress { message, .. } if message == "step 1"),
        "ToolProgress(step 1)",
    );
    let completed = position(
        &events,
        |e| matches!(e, AgentEvent::ToolCompleted { result, .. } if !result.is_error),
        "ToolCompleted",
    );
    assert!(started < step1);
    assert!(step1 < completed);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolCompleted { .. }))
            .count(),
        1,
        "a returning tool must emit exactly one completion"
    );
    let result = tool_result(&events);
    assert_eq!(
        result.content,
        vec![ContentBlock::Text("progress done".into())]
    );
}

#[tokio::test]
async fn self_terminating_tool_keeps_its_first_streamed_terminal() {
    let rig = Rig::new(LocalProvider::new(vec![
        tool_turn(
            "running the self-terminating tool",
            vec![("c1", "self_terminating", json!({}))],
        ),
        text_turn("Streamed result accepted."),
    ]));
    rig.registry.register(Arc::new(SelfTerminatingTool));
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);

    let outcome = agent
        .prompt(user("self terminate"), &rig.env())
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let completions: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCompleted { result, .. } => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(completions.len(), 1, "exactly one terminal must complete");
    assert_eq!(
        completions[0].content,
        vec![ContentBlock::Text("streamed result".into())]
    );
}

#[tokio::test]
async fn sustained_clone_progress_cannot_starve_ready_tool_completion() {
    let rig = Rig::new(LocalProvider::new(vec![
        tool_turn(
            "running sustained progress",
            vec![("c1", "sustained_progress", json!({}))],
        ),
        text_turn("Sustained progress finished."),
    ]));
    let tool = Arc::new(SustainedProgressTool::new());
    rig.registry.register(tool.clone());
    let mut agent = Agent::new(AgentConfig::new());

    let prompt = tokio::time::timeout(
        DISPATCH_TEST_TIMEOUT,
        agent.prompt(user("keep reporting"), &rig.env()),
    )
    .await;

    if prompt.is_err() {
        tool.stop_producer();
    }
    let Some(mut producer) = tool.take_producer() else {
        panic!("sustained progress producer must start");
    };
    match tokio::time::timeout(DISPATCH_TEST_TIMEOUT, &mut producer).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => panic!("sustained progress producer failed: {error}"),
        Err(_) => {
            tool.stop_producer();
            let cleanup = tokio::time::timeout(DISPATCH_TEST_TIMEOUT, &mut producer).await;
            match cleanup {
                Ok(Ok(())) => {
                    panic!("sustained progress producer did not stop after terminal state closed")
                }
                Ok(Err(error)) => panic!("sustained progress producer failed: {error}"),
                Err(_) => panic!("sustained progress producer ignored the cleanup signal"),
            }
        }
    }
    let outcome = match prompt {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) => panic!("prompt failed: {error}"),
        Err(_) => panic!("ready tool execution was starved by sustained progress"),
    };
    assert_eq!(outcome, TurnOutcome::Completed);
    let results: Vec<_> = agent
        .state()
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1, "exactly one terminal must complete");
    assert_eq!(
        results[0].content,
        vec![ContentBlock::Text("sustained progress done".into())]
    );
}

#[tokio::test]
async fn unknown_tool_and_failing_tool_become_error_results() {
    let rig = Rig::new(LocalProvider::new(vec![
        tool_turn(
            "one unknown, one doomed",
            vec![("c1", "missing", json!({})), ("c2", "failing", json!({}))],
        ),
        text_turn("Both calls failed; understood."),
    ]));
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);

    let outcome = agent
        .prompt(user("try both tools"), &rig.env())
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    let messages = &agent.state().messages;
    assert_eq!(messages.len(), 5); // user, assistant(2 calls), 2 results, assistant
    let Message::ToolResult(unknown_result) = &messages[2] else {
        panic!("first result must be a tool result");
    };
    let Message::ToolResult(failing_result) = &messages[3] else {
        panic!("second result must be a tool result");
    };
    assert!(unknown_result.is_error);
    let ContentBlock::Text(unknown_text) = &unknown_result.content[0] else {
        panic!("must be text")
    };
    assert!(unknown_text.text.contains("unknown tool"));
    assert!(failing_result.is_error);
    let ContentBlock::Text(failing_text) = &failing_result.content[0] else {
        panic!("must be text")
    };
    assert!(failing_text.text.contains("intentional test failure"));

    let events = collector.await.expect("collector must finish");
    let started = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolStarted { .. }))
        .count();
    assert_eq!(started, 2);
    assert_eq!(rig.provider.recorded_requests().len(), 2);
}

#[tokio::test]
async fn panicking_tool_becomes_error_result_and_loop_continues() {
    let rig = Rig::new(LocalProvider::new(vec![
        tool_turn("this will panic", vec![("c1", "panicking", json!({}))]),
        text_turn("The tool trapped; understood."),
    ]));
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);

    let outcome = agent
        .prompt(user("panic please"), &rig.env())
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    let Message::ToolResult(result) = &agent.state().messages[2] else {
        panic!("history must contain the panic tool result");
    };
    assert!(result.is_error);
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("must be text");
    };
    assert!(
        text.text.contains("plugin trap") && text.text.contains("intentional tool panic"),
        "{text:?}"
    );
    assert_eq!(rig.provider.recorded_requests().len(), 2);
    let _ = collector.await.expect("collector must finish");
}

#[tokio::test]
async fn length_truncated_tool_calls_are_failed_not_executed() {
    let truncated = LocalTurn::Message(AssistantMessage {
        blocks: vec![
            ContentBlock::Text("truncated".into()),
            ContentBlock::ToolCall(ToolCall::new(
                "c1",
                "echo",
                json!({"text": "cut off mid-way"}),
            )),
        ],
        usage: None,
        stop_reason: StopReason::Length,
    });
    let rig = Rig::new(LocalProvider::new(vec![
        truncated,
        text_turn("Re-issuing with complete arguments next time."),
    ]));
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);

    let outcome = agent
        .prompt(user("do it"), &rig.env())
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(result.is_error);
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("must be text")
    };
    assert!(text.text.contains("token limit"));
    assert_eq!(rig.provider.recorded_requests().len(), 2);
}

// ---------------------------------------------------------------------
// Provider failure: error event + aborted turn + Err return
// ---------------------------------------------------------------------

#[tokio::test]
async fn provider_error_returns_err_without_half_completed_turn() {
    let rig = Rig::new(LocalProvider::new(vec![LocalTurn::Fail(
        ProviderError::with_message(ProviderErrorKind::Unavailable, "boom"),
    )]));
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);

    let err = agent
        .prompt(user("hi"), &rig.env())
        .await
        .expect_err("provider failure must surface as Err");
    assert!(matches!(err, mycode_core::MycodeError::Provider(_)));
    assert!(!agent.state().is_streaming);
    assert_eq!(agent.state().messages, vec![user("hi")]);

    let events = collector.await.expect("collector must finish");
    let error_pos = position(
        &events,
        |event| {
            matches!(
                event,
                AgentEvent::Error(mycode_core::MycodeError::Provider(_))
            )
        },
        "AgentEvent::Error(Provider)",
    );
    let ended_pos = position(
        &events,
        |event| matches!(event, AgentEvent::TurnEnded(_)),
        "TurnEnded",
    );
    assert!(error_pos < ended_pos);
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Aborted))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnEnded(TurnOutcome::Completed)))
    );
}

/// A provider that fails before streaming begins (`stream()` returns
/// `Err` — connect/config failure; concretely, a local provider whose
/// script is exhausted).
#[tokio::test]
async fn provider_request_failure_emits_error_event() {
    // Empty script: the first `stream()` call fails with a typed Rejected error.
    let rig = Rig::new(LocalProvider::new(vec![]));
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);

    let err = agent
        .prompt(user("hi"), &rig.env())
        .await
        .expect_err("pre-stream provider failure must surface as Err");
    assert!(matches!(err, mycode_core::MycodeError::Provider(_)));
    assert!(!agent.state().is_streaming);
    assert_eq!(agent.state().messages, vec![user("hi")]);

    // The telemetry contract: Error event emitted at the failure site,
    // then TurnEnded(Aborted) — never a silent mislabel as a plain abort.
    let events = collector.await.expect("collector must finish");
    let error_pos = position(
        &events,
        |e| matches!(e, AgentEvent::Error(mycode_core::MycodeError::Provider(_))),
        "AgentEvent::Error(Provider)",
    );
    let ended_pos = position(
        &events,
        |e| matches!(e, AgentEvent::TurnEnded(_)),
        "TurnEnded",
    );
    assert!(error_pos < ended_pos);
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Aborted))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnEnded(TurnOutcome::Completed)))
    );
}

struct SetupCancelledProvider;

#[async_trait]
impl Provider for SetupCancelledProvider {
    async fn stream(
        &self,
        _request: &Request,
        _cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        Err(ProviderError::new(ProviderErrorKind::Cancelled))
    }
}

#[tokio::test]
async fn provider_setup_cancelled_maps_to_aborted_without_error_event() {
    let provider = SetupCancelledProvider;
    let registry = ToolRegistry::new();
    let hooks = HookRunner::new();
    let events = broadcast::channel(256).0;
    let env = TurnEnv::new(&provider, &registry, &hooks).with_events(events.clone());
    let mut receiver = events.subscribe();
    let mut agent = Agent::new(AgentConfig::new());

    let outcome = agent
        .prompt(user("hi"), &env)
        .await
        .expect("setup cancellation is a normal abort");
    assert_eq!(outcome, TurnOutcome::Aborted);
    assert_eq!(agent.state().messages, vec![user("hi")]);

    let mut observed = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        observed.push(event);
    }
    assert_eq!(
        observed.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Aborted))
    );
    assert!(
        !observed
            .iter()
            .any(|event| matches!(event, AgentEvent::Error(_))),
        "setup cancellation must not emit an error: {observed:#?}"
    );
}

#[tokio::test]
async fn oversized_request_fails_closed_before_provider_call() {
    let provider = LocalProvider::new(vec![text_turn("must not run")]);
    let registry = ToolRegistry::new();
    let hooks = HookRunner::new();
    let events = broadcast::channel(256).0;
    let env = TurnEnv::new(&provider, &registry, &hooks).with_events(events.clone());
    let mut receiver = events.subscribe();
    let mut agent =
        Agent::new(AgentConfig::new().with_system_prompt("x".repeat(MAX_REQUEST_ENCODED_BYTES)));

    let error = agent
        .prompt(user("hi"), &env)
        .await
        .expect_err("oversized request must fail closed");
    assert!(matches!(error, mycode_core::MycodeError::Provider(_)));
    assert!(
        provider.recorded_requests().is_empty(),
        "provider must not be called after validation fails"
    );

    let mut observed = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        observed.push(event);
    }
    let error_pos = position(
        &observed,
        |event| {
            matches!(
                event,
                AgentEvent::Error(mycode_core::MycodeError::Provider(_))
            )
        },
        "AgentEvent::Error(Provider)",
    );
    let ended_pos = position(
        &observed,
        |event| matches!(event, AgentEvent::TurnEnded(TurnOutcome::Aborted)),
        "TurnEnded(Aborted)",
    );
    assert!(error_pos < ended_pos);
}

/// A provider whose producer exits without sending `Done` or `Error`.
///
/// The neutral stream converts that drop into one Protocol terminal.
struct DanglingStreamProvider;

#[async_trait]
impl Provider for DanglingStreamProvider {
    async fn stream(
        &self,
        _req: &Request,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let (sender, stream) = EventStream::channel(cancel);
        let _ = sender.send(StreamEvent::TextDelta("partial".into())).await;
        // Dropping every sender synthesizes one Protocol terminal.
        drop(sender);
        Ok(stream)
    }
}

#[tokio::test]
async fn producer_drop_becomes_protocol_error_event() {
    let provider = DanglingStreamProvider;
    let registry = ToolRegistry::new();
    let hooks = HookRunner::new();
    let events = broadcast::channel(256).0;
    let env = TurnEnv::new(&provider, &registry, &hooks)
        .with_events(events.clone())
        .with_cancel(CancellationToken::new());
    let mut rx = events.subscribe();
    let collector = tokio::spawn(async move {
        let mut out = Vec::new();
        while let Ok(event) = rx.recv().await {
            let done = matches!(event, AgentEvent::TurnEnded(_));
            out.push(event);
            if done {
                break;
            }
        }
        out
    });

    let mut agent = Agent::new(AgentConfig::new());
    let err = agent
        .prompt(user("hi"), &env)
        .await
        .expect_err("producer drop must surface as Err");
    assert!(
        matches!(err, mycode_core::MycodeError::Provider(message) if message
        .contains("producer ended without a terminal event"))
    );
    assert_eq!(agent.state().messages, vec![user("hi")]);

    let events = collector.await.expect("collector must finish");
    let error_pos = position(
        &events,
        |e| {
            matches!(
                e,
                AgentEvent::Error(mycode_core::MycodeError::Provider(message))
                    if message.contains("producer ended without a terminal event")
            )
        },
        "AgentEvent::Error(producer ended without a terminal event)",
    );
    let ended_pos = position(
        &events,
        |e| matches!(e, AgentEvent::TurnEnded(_)),
        "TurnEnded",
    );
    assert!(error_pos < ended_pos);
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Aborted))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnEnded(TurnOutcome::Completed)))
    );
}

// ---------------------------------------------------------------------
// Queue modes
// ---------------------------------------------------------------------

#[tokio::test]
async fn queue_mode_one_at_a_time_delivers_each_follow_up_in_own_request() {
    let rig = Rig::new(LocalProvider::new(vec![
        text_turn("answer one"),
        text_turn("answer two"),
        text_turn("answer three"),
    ]));
    let mut agent = Agent::new(AgentConfig::new());
    assert_eq!(agent.queue_mode(), QueueMode::OneAtATime);
    // Queued while idle (a subagent callback before the next prompt).
    agent.follow_up(user("task one"));
    agent.follow_up(user("task two"));

    let outcome = agent
        .prompt(user("start"), &rig.env())
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(rig.provider.recorded_requests().len(), 3);
    let messages = &agent.state().messages;
    // user, asst, task1, asst, task2, asst
    assert_eq!(messages.len(), 6);
    assert_eq!(messages[2], user("task one"));
    assert_eq!(messages[4], user("task two"));
}

#[tokio::test]
async fn queue_mode_all_batches_follow_ups_into_one_request() {
    let rig = Rig::new(LocalProvider::new(vec![
        text_turn("answer one"),
        text_turn("answer both"),
    ]));
    let mut agent = Agent::new(AgentConfig::new());
    agent.set_queue_mode(QueueMode::All);
    agent.follow_up(user("task one"));
    agent.follow_up(user("task two"));

    let outcome = agent
        .prompt(user("start"), &rig.env())
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(rig.provider.recorded_requests().len(), 2);
    let requests = rig.provider.recorded_requests();
    // Both follow-ups were injected before the second response.
    assert_eq!(requests[1].messages[2], user("task one"));
    assert_eq!(requests[1].messages[3], user("task two"));
}

// ---------------------------------------------------------------------
// Steer queued while idle is delivered before the first response
// ---------------------------------------------------------------------

#[tokio::test]
async fn steer_queued_while_idle_lands_before_the_first_response() {
    let rig = Rig::new(LocalProvider::new(vec![text_turn("combined answer")]));
    let mut agent = Agent::new(AgentConfig::new());
    agent.steer(user("context update before you start"));

    let outcome = agent
        .prompt(user("initial prompt"), &rig.env())
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Steered);
    let requests = rig.provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].messages.len(), 2);
    assert_eq!(requests[0].messages[0], user("initial prompt"));
    assert_eq!(
        requests[0].messages[1],
        user("context update before you start")
    );
}

#[tokio::test]
async fn hook_rewrite_binds_search_and_executes() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("safe")).unwrap();
    std::fs::write(directory.path().join("safe").join("keep.txt"), "x").unwrap();
    std::fs::create_dir(directory.path().join("secrets")).unwrap();
    std::fs::write(directory.path().join("secrets").join("leak.txt"), "x").unwrap();

    let registry = ToolRegistry::new();
    registry.register(Arc::new(FindTool));
    let hooks = HookRunner::new().with_test_gate(|args| {
        args["path"] = json!("secrets");
        GateResult::Pass
    });
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn(
                "search",
                vec![("c1", "find", json!({"pattern": "*.txt", "path": "safe"}))],
            ),
            text_turn("rewritten; noted."),
        ]),
        registry,
        hooks,
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);
    let outcome = agent
        .prompt(
            user("find files"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(!result.is_error, "{result:#?}");
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("result content must be text: {result:#?}");
    };
    assert!(text.text.contains("leak.txt"), "{text:?}");
    assert!(!text.text.contains("keep.txt"), "{text:?}");
}

#[tokio::test]
async fn hook_rewrite_to_missing_path_does_not_execute_after_it_appears() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("safe")).unwrap();
    std::fs::write(directory.path().join("safe").join("keep.txt"), "x").unwrap();

    let registry = ToolRegistry::new();
    registry.register(Arc::new(FindTool));
    let hooks = HookRunner::new().with_test_gate(|args| {
        args["path"] = json!("later");
        GateResult::Pass
    });
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn(
                "search",
                vec![("c1", "find", json!({"pattern": "*.txt", "path": "safe"}))],
            ),
            text_turn("noted."),
        ]),
        registry,
        hooks,
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);
    let outcome = agent
        .prompt(
            user("find files"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    std::fs::create_dir(directory.path().join("later")).unwrap();
    std::fs::write(directory.path().join("later").join("leak.txt"), "x").unwrap();
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(result.is_error, "{result:#?}");
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("error content must be text: {result:#?}");
    };
    assert!(
        text.text.contains("does not exist") || text.text.contains("inaccessible"),
        "{text:?}"
    );
    assert!(!text.text.contains("leak.txt"), "{text:?}");
}

#[cfg(windows)]
#[tokio::test]
async fn hook_rewrite_to_share_locked_alias_does_not_execute() {
    use std::os::windows::fs::OpenOptionsExt;

    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("safe")).unwrap();
    std::fs::write(directory.path().join("safe").join("keep.txt"), "x").unwrap();
    std::fs::create_dir(directory.path().join("Visible")).unwrap();
    std::fs::write(directory.path().join("Visible").join("leak.txt"), "secret").unwrap();
    // FILE_FLAG_BACKUP_SEMANTICS: required to lock a directory handle.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(directory.path().join("Visible"))
        .expect("exclusive directory lock");

    let registry = ToolRegistry::new();
    registry.register(Arc::new(FindTool));
    let hooks = HookRunner::new().with_test_gate(|args| {
        args["path"] = json!("visible");
        GateResult::Pass
    });
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn(
                "search",
                vec![("c1", "find", json!({"pattern": "*.txt", "path": "safe"}))],
            ),
            text_turn("noted."),
        ]),
        registry,
        hooks,
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);
    let outcome = agent
        .prompt(
            user("find files"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    drop(lock);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(result.is_error, "{result:#?}");
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("error content must be text: {result:#?}");
    };
    assert!(!text.text.contains("leak.txt"), "{text:?}");
    assert!(
        text.text.contains("does not exist")
            || text.text.contains("inaccessible")
            || text.text.to_ascii_lowercase().contains("sharing"),
        "{text:?}"
    );
}

/// A same-name override must not inherit builtin search preflight.
struct VirtualFind;

#[derive(Deserialize, JsonSchema)]
struct VirtualFindArgs {
    /// Glob accepted only to match the builtin schema shape.
    pattern: String,
    /// Virtual path that need not exist on disk.
    path: Option<String>,
}

#[async_trait]
impl Tool for VirtualFind {
    type Args = VirtualFindArgs;
    type Output = ();

    fn name(&self) -> &str {
        "find"
    }

    fn description(&self) -> &str {
        "Virtual find override (test fixture)."
    }

    async fn execute(
        &self,
        args: Self::Args,
        _ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::text(format!(
            "virtual:{}:{}",
            args.pattern,
            args.path.unwrap_or_else(|| ".".to_owned())
        )))
    }
}

#[tokio::test]
async fn same_name_override_skips_search_preflight() {
    let directory = tempfile::tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry.register(Arc::new(FindTool));
    registry.register(Arc::new(VirtualFind));
    let override_tool = registry.get("find").unwrap();
    assert!(!ToolDyn::requires_search_preflight(&*override_tool));

    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn(
                "search",
                vec![(
                    "c1",
                    "find",
                    json!({"pattern": "*", "path": "missing-nowhere"}),
                )],
            ),
            text_turn("noted."),
        ]),
        registry,
        hooks: HookRunner::new(),
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);
    let outcome = agent
        .prompt(
            user("find files"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(!result.is_error, "{result:#?}");
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("result content must be text: {result:#?}");
    };
    assert_eq!(text.text, "virtual:*:missing-nowhere");
}

/// Dropping dispatch must drop the execute future and join search workers.
struct DropSearchTool {
    dropped: Arc<std::sync::atomic::AtomicBool>,
}

struct ExecGuard(Arc<std::sync::atomic::AtomicBool>);

impl Drop for ExecGuard {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

#[derive(Deserialize, JsonSchema)]
struct DropSearchArgs {}

#[async_trait]
impl Tool for DropSearchTool {
    type Args = DropSearchArgs;
    type Output = ();
    fn name(&self) -> &str {
        "drop_search"
    }
    fn description(&self) -> &str {
        "Blocks in a search worker until cancelled (test fixture)."
    }
    async fn execute(
        &self,
        _args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let _guard = ExecGuard(Arc::clone(&self.dropped));
        mycode_tools::run_search_worker_until_cancel(ctx.cancel.clone()).await?;
        Ok(ToolResult::text("done"))
    }
}

/// Completes, then panics in Drop so dispatch must map that to an error.
struct CompletingPanicDropTool;

#[async_trait]
impl Tool for CompletingPanicDropTool {
    type Args = NoArgs;
    type Output = ();
    fn name(&self) -> &str {
        "complete_drop"
    }
    fn description(&self) -> &str {
        "Completes then panics in Drop (test fixture)."
    }
    async fn execute(
        &self,
        _args: Self::Args,
        _ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let _guard = PanicOnDropGuard;
        Ok(ToolResult::text("should be discarded"))
    }
}

struct HangPanicDropTool {
    entered: Arc<std::sync::atomic::AtomicBool>,
}

struct PanicOnDropGuard;

impl Drop for PanicOnDropGuard {
    fn drop(&mut self) {
        panic!("intentional tool drop panic");
    }
}

/// Panic payload whose destructor panics again if dispatch drops the box.
struct PanickingPayload;

impl Drop for PanickingPayload {
    fn drop(&mut self) {
        panic!("payload drop must not escape isolation");
    }
}

struct PanicAnyOnDropGuard;

impl Drop for PanicAnyOnDropGuard {
    fn drop(&mut self) {
        std::panic::panic_any(PanickingPayload);
    }
}

/// `panic_any` so dispatch must forget the payload at the catch boundary.
struct PanicAnyTool;

#[async_trait]
impl Tool for PanicAnyTool {
    type Args = NoArgs;
    type Output = ();
    fn name(&self) -> &str {
        "panic_any"
    }
    fn description(&self) -> &str {
        "Panics with a panicking-Drop payload (test fixture)."
    }
    async fn execute(
        &self,
        _args: Self::Args,
        _ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        std::panic::panic_any(PanickingPayload);
    }
}

/// Completes, then `panic_any` in Drop so dispatch must map that to an error.
struct CompletingPanicAnyDropTool;

#[async_trait]
impl Tool for CompletingPanicAnyDropTool {
    type Args = NoArgs;
    type Output = ();
    fn name(&self) -> &str {
        "complete_panic_any_drop"
    }
    fn description(&self) -> &str {
        "Completes then panics in Drop with a panicking payload (test fixture)."
    }
    async fn execute(
        &self,
        _args: Self::Args,
        _ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let _guard = PanicAnyOnDropGuard;
        Ok(ToolResult::text("should be discarded"))
    }
}

struct HangPanicAnyDropTool {
    entered: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl Tool for HangPanicDropTool {
    type Args = NoArgs;
    type Output = ();
    fn name(&self) -> &str {
        "hang_drop"
    }
    fn description(&self) -> &str {
        "Stays pending with a panicking Drop (test fixture)."
    }
    async fn execute(
        &self,
        _args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let _guard = PanicOnDropGuard;
        self.entered
            .store(true, std::sync::atomic::Ordering::Release);
        ctx.cancel.cancelled().await;
        Ok(ToolResult::text("should not complete"))
    }
}

#[async_trait]
impl Tool for HangPanicAnyDropTool {
    type Args = NoArgs;
    type Output = ();
    fn name(&self) -> &str {
        "hang_panic_any_drop"
    }
    fn description(&self) -> &str {
        "Stays pending with a panicking-payload Drop (test fixture)."
    }
    async fn execute(
        &self,
        _args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let _guard = PanicAnyOnDropGuard;
        self.entered
            .store(true, std::sync::atomic::Ordering::Release);
        ctx.cancel.cancelled().await;
        Ok(ToolResult::text("should not complete"))
    }
}

fn hang_drop_rig(
    entered: Arc<std::sync::atomic::AtomicBool>,
) -> (Rig, Agent, mycode_agent::AgentHandle) {
    let registry = ToolRegistry::new();
    registry.register(Arc::new(HangPanicDropTool { entered }));
    let rig = Rig {
        provider: LocalProvider::new(vec![tool_turn(
            "hang",
            vec![("c1", "hang_drop", json!({}))],
        )]),
        registry,
        hooks: HookRunner::new(),
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let agent = Agent::new(AgentConfig::new());
    let handle = agent.handle();
    (rig, agent, handle)
}

async fn wait_until_entered(entered: &std::sync::atomic::AtomicBool) {
    let started = std::time::Instant::now();
    loop {
        if entered.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "hanging drop tool never entered execute"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn aborting_pending_panic_on_drop_tool_does_not_unwind_prompt() {
    use std::sync::atomic::AtomicBool;

    let entered = Arc::new(AtomicBool::new(false));
    let (rig, mut agent, handle) = hang_drop_rig(Arc::clone(&entered));
    let task = tokio::spawn(async move { agent.prompt(user("go"), &rig.env()).await });
    wait_until_entered(&entered).await;
    handle.abort();
    let outcome = task
        .await
        .expect("prompt task must not unwind from tool Drop panic")
        .expect("abort is a normal outcome");
    assert_eq!(outcome, TurnOutcome::Aborted);
}

#[tokio::test]
async fn dropping_dispatch_of_panic_on_drop_tool_does_not_unwind_prompt() {
    use std::sync::atomic::AtomicBool;

    let entered = Arc::new(AtomicBool::new(false));
    let (rig, mut agent, _handle) = hang_drop_rig(Arc::clone(&entered));
    let task = tokio::spawn(async move { agent.prompt(user("go"), &rig.env()).await });
    wait_until_entered(&entered).await;
    task.abort();
    let join = task.await;
    assert!(
        join.as_ref()
            .err()
            .is_some_and(|error| error.is_cancelled()),
        "prompt task must be cancelled, not panicked: {join:?}"
    );
}

#[tokio::test]
async fn completing_panic_on_drop_tool_becomes_error_result() {
    let registry = ToolRegistry::new();
    registry.register(Arc::new(CompletingPanicDropTool));
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn("done", vec![("c1", "complete_drop", json!({}))]),
            text_turn("The tool trapped; understood."),
        ]),
        registry,
        hooks: HookRunner::new(),
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let outcome = agent
        .prompt(user("complete then drop"), &rig.env())
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    let Message::ToolResult(result) = &agent.state().messages[2] else {
        panic!("history must contain the drop-panic tool result");
    };
    assert!(result.is_error);
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("must be text");
    };
    assert!(
        text.text.contains("plugin trap") && text.text.contains("intentional tool drop panic"),
        "{text:?}"
    );
    assert_eq!(rig.provider.recorded_requests().len(), 2);
}

fn hang_panic_any_drop_rig(
    entered: Arc<std::sync::atomic::AtomicBool>,
) -> (Rig, Agent, mycode_agent::AgentHandle) {
    let registry = ToolRegistry::new();
    registry.register(Arc::new(HangPanicAnyDropTool { entered }));
    let rig = Rig {
        provider: LocalProvider::new(vec![tool_turn(
            "hang",
            vec![("c1", "hang_panic_any_drop", json!({}))],
        )]),
        registry,
        hooks: HookRunner::new(),
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let agent = Agent::new(AgentConfig::new());
    let handle = agent.handle();
    (rig, agent, handle)
}

#[tokio::test]
async fn panic_any_payload_drop_becomes_error_result_and_loop_continues() {
    let registry = ToolRegistry::new();
    registry.register(Arc::new(PanicAnyTool));
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn("this will panic", vec![("c1", "panic_any", json!({}))]),
            text_turn("The tool trapped; understood."),
        ]),
        registry,
        hooks: HookRunner::new(),
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let outcome = agent
        .prompt(user("panic any please"), &rig.env())
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    let Message::ToolResult(result) = &agent.state().messages[2] else {
        panic!("history must contain the panic_any tool result");
    };
    assert!(result.is_error);
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("must be text");
    };
    assert!(
        text.text.contains("plugin trap") && text.text.contains("tool panicked"),
        "{text:?}"
    );
    assert_eq!(rig.provider.recorded_requests().len(), 2);
}

#[tokio::test]
async fn completing_panic_any_on_drop_tool_becomes_error_result() {
    let registry = ToolRegistry::new();
    registry.register(Arc::new(CompletingPanicAnyDropTool));
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn("done", vec![("c1", "complete_panic_any_drop", json!({}))]),
            text_turn("The tool trapped; understood."),
        ]),
        registry,
        hooks: HookRunner::new(),
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let outcome = agent
        .prompt(user("complete then drop"), &rig.env())
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    let Message::ToolResult(result) = &agent.state().messages[2] else {
        panic!("history must contain the drop-panic tool result");
    };
    assert!(result.is_error);
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("must be text");
    };
    assert!(
        text.text.contains("plugin trap") && text.text.contains("tool panicked"),
        "{text:?}"
    );
    assert_eq!(rig.provider.recorded_requests().len(), 2);
}

#[tokio::test]
async fn aborting_pending_panic_any_on_drop_tool_does_not_unwind_prompt() {
    use std::sync::atomic::AtomicBool;

    let entered = Arc::new(AtomicBool::new(false));
    let (rig, mut agent, handle) = hang_panic_any_drop_rig(Arc::clone(&entered));
    let task = tokio::spawn(async move { agent.prompt(user("go"), &rig.env()).await });
    wait_until_entered(&entered).await;
    handle.abort();
    let outcome = task
        .await
        .expect("prompt task must not unwind from payload Drop panic")
        .expect("abort is a normal outcome");
    assert_eq!(outcome, TurnOutcome::Aborted);
}

#[tokio::test]
async fn dropping_dispatch_of_panic_any_on_drop_tool_does_not_unwind_prompt() {
    use std::sync::atomic::AtomicBool;

    let entered = Arc::new(AtomicBool::new(false));
    let (rig, mut agent, _handle) = hang_panic_any_drop_rig(Arc::clone(&entered));
    let task = tokio::spawn(async move { agent.prompt(user("go"), &rig.env()).await });
    wait_until_entered(&entered).await;
    task.abort();
    let join = task.await;
    assert!(
        join.as_ref()
            .err()
            .is_some_and(|error| error.is_cancelled()),
        "prompt task must be cancelled, not panicked: {join:?}"
    );
}

#[tokio::test]
async fn aborting_dispatch_drops_tool_and_joins_search_workers() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    let dropped = Arc::new(AtomicBool::new(false));
    let registry = ToolRegistry::new();
    registry.register(Arc::new(DropSearchTool {
        dropped: Arc::clone(&dropped),
    }));
    let rig = Rig {
        provider: LocalProvider::new(vec![tool_turn(
            "search",
            vec![("c1", "drop_search", json!({}))],
        )]),
        registry,
        hooks: HookRunner::new(),
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let handle = agent.handle();
    let task = tokio::spawn(async move { agent.prompt(user("go"), &rig.env()).await });
    let started = Instant::now();
    loop {
        if mycode_tools::live_search_workers() > 0 {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "search worker never started"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    handle.abort();
    let _ = task.await;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if mycode_tools::live_search_workers() == 0
            && mycode_tools::live_search_thread_handles() == 0
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "search worker or thread handle leaked"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        dropped.load(Ordering::Acquire),
        "tool value was not dropped"
    );
}

/// A same-name override must not inherit builtin file preflight.
struct VirtualRead;

#[derive(Deserialize, JsonSchema)]
struct VirtualReadArgs {
    /// Path that need not exist on disk.
    path: String,
}

#[async_trait]
impl Tool for VirtualRead {
    type Args = VirtualReadArgs;
    type Output = ();
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "Virtual read override (test fixture)."
    }
    async fn execute(
        &self,
        args: Self::Args,
        _ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::text(format!("virtual-read:{}", args.path)))
    }
}

#[tokio::test]
async fn same_name_override_skips_file_preflight() {
    let directory = tempfile::tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry.register(Arc::new(ReadTool));
    registry.register(Arc::new(VirtualRead));
    let override_tool = registry.get("read").unwrap();
    assert!(!ToolDyn::requires_file_preflight(&*override_tool));

    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn(
                "read",
                vec![("c1", "read", json!({"path": "missing-nowhere"}))],
            ),
            text_turn("noted."),
        ]),
        registry,
        hooks: HookRunner::new(),
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);
    let outcome = agent
        .prompt(
            user("read file"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(!result.is_error, "{result:#?}");
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("result content must be text: {result:#?}");
    };
    assert_eq!(text.text, "virtual-read:missing-nowhere");
}

/// File tool that records execute() and only uses the dispatcher-bound capability.
struct SentinelRead {
    executed: Arc<AtomicBool>,
}

#[derive(Deserialize, JsonSchema)]
struct SentinelReadArgs {
    /// Path bound at file preflight.
    path: String,
}

#[async_trait]
impl Tool for SentinelRead {
    type Args = SentinelReadArgs;
    type Output = ();
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "Sentinel read that requires dispatcher file preflight."
    }
    fn file_access(&self) -> Option<FileAccess> {
        Some(FileAccess::ExistingContent)
    }
    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        self.executed.store(true, Ordering::SeqCst);
        let Some(prepared) = ctx.prepared_file.clone() else {
            return Err(ToolError::Execution(
                "dispatcher did not bind a prepared file capability".to_owned(),
            ));
        };
        let outcome = read_file_async(
            Some(prepared),
            ctx.cwd.clone(),
            args.path,
            None,
            None,
            ctx.cancel.clone(),
        )
        .await?;
        Ok(ToolResult::text(outcome.displayed))
    }
}

#[tokio::test]
async fn hook_block_precedes_file_preflight() {
    let directory = tempfile::tempdir().unwrap();
    let executed = Arc::new(AtomicBool::new(false));
    let registry = ToolRegistry::new();
    registry.register(Arc::new(SentinelRead {
        executed: executed.clone(),
    }));
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn(
                "read",
                vec![("c1", "read", json!({"path": "../outside.txt"}))],
            ),
            text_turn("blocked; noted."),
        ]),
        registry,
        hooks: HookRunner::new().with_test_gate(|_| GateResult::Block("blocked".into())),
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);

    let outcome = agent
        .prompt(
            user("read file"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(result.is_error, "{result:#?}");
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("error content must be text: {result:#?}");
    };
    assert!(text.text.contains("blocked by hook"), "{text:?}");
    assert!(
        !executed.load(Ordering::SeqCst),
        "blocked hook must prevent tool execution"
    );
}

#[tokio::test]
async fn file_hook_rewrite_binds_prepared_file() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("a.txt"), "from-a").unwrap();
    std::fs::write(directory.path().join("b.txt"), "from-b").unwrap();

    let executed = Arc::new(AtomicBool::new(false));
    let registry = ToolRegistry::new();
    registry.register(Arc::new(SentinelRead {
        executed: executed.clone(),
    }));
    let hooks = HookRunner::new().with_test_gate(|args| {
        args["path"] = json!("b.txt");
        GateResult::Pass
    });
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn("read", vec![("c1", "read", json!({"path": "a.txt"}))]),
            text_turn("rebound; noted."),
        ]),
        registry,
        hooks,
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);
    let outcome = agent
        .prompt(
            user("read file"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(!result.is_error, "{result:#?}");
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("result content must be text: {result:#?}");
    };
    assert!(executed.load(Ordering::SeqCst), "rewrite must execute");
    assert!(text.text.contains("from-b"), "{text:?}");
    assert!(!text.text.contains("from-a"), "{text:?}");
}

#[tokio::test]
async fn file_preflight_missing_path_does_not_execute() {
    let directory = tempfile::tempdir().unwrap();
    let executed = Arc::new(AtomicBool::new(false));
    let registry = ToolRegistry::new();
    registry.register(Arc::new(SentinelRead {
        executed: executed.clone(),
    }));
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn(
                "read",
                vec![("c1", "read", json!({"path": "missing-nowhere.txt"}))],
            ),
            text_turn("noted."),
        ]),
        registry,
        hooks: HookRunner::new(),
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);
    let outcome = agent
        .prompt(
            user("write file"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(result.is_error, "{result:#?}");
    assert!(
        !executed.load(Ordering::SeqCst),
        "failed file preflight must not execute the tool"
    );
}

#[tokio::test]
async fn hook_block_does_not_echo_write_content() {
    const SECRET: &str = "MYCODE-SECRET-SENTINEL-9f3a";
    let directory = tempfile::tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool));
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn(
                "write",
                vec![(
                    "c1",
                    "write",
                    json!({"path": "secret.txt", "content": SECRET}),
                )],
            ),
            text_turn("blocked; noted."),
        ]),
        registry,
        hooks: HookRunner::new().with_test_gate(|_| GateResult::Block("blocked".into())),
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig);
    let outcome = agent
        .prompt(
            user("write file"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(result.is_error, "{result:#?}");
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("error content must be text: {result:#?}");
    };
    assert!(text.text.contains("blocked by hook"), "{text:?}");
    assert!(
        !text.text.contains(SECRET),
        "write content must not appear in the model-visible error: {text:?}"
    );
}
