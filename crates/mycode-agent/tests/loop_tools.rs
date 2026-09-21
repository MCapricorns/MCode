//! Tool dispatch scenarios: registered tools dispatch without a
//! permission callback, progress streams live, self-terminating and
//! sustained-progress tools keep the single-terminal invariant, and
//! unknown/failing/panicking tools or length-truncated calls become
//! `is_error` tool results while the loop continues.
//!
//! Part of the loop scenario groups listed in `common/mod.rs`.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::local_provider::{LocalProvider, LocalTurn};
use common::{
    DISPATCH_TEST_TIMEOUT, NoArgs, Rig, position, spawn_collector, text_turn, tool_result,
    tool_turn, user,
};
use mycode_agent::{Agent, AgentConfig, HookRunner, TurnEnv};
use mycode_core::events::{AgentEvent, TurnOutcome};
use mycode_core::message::{
    AssistantMessage, ContentBlock, Message, StopReason, ToolCall, ToolResultMessage,
};
use mycode_tools::{Tool, ToolCtx, ToolError, ToolRegistry, ToolResult, ToolStream};
use serde_json::json;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

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

#[tokio::test]
async fn registered_tool_dispatches_without_permission_callback() {
    let provider = LocalProvider::new(vec![
        tool_turn("calling echo", vec![("c1", "echo", json!({"text": "hi"}))]),
        text_turn("echoed."),
    ]);
    let registry = ToolRegistry::new();
    registry.register(Arc::new(common::EchoTool));
    let hooks = HookRunner::new();
    let events = broadcast::channel(256).0;
    // TurnEnv::new takes only provider, tools, and hooks — no permission
    // engine, prompt, or grant state.
    let env = TurnEnv::new(&provider, &registry, &hooks).with_events(events.clone());
    let collector = spawn_collector(&events);
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
    let collector = spawn_collector(&rig.events);

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
    let collector = spawn_collector(&rig.events);

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
    let results: Vec<&ToolResultMessage> = agent
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
    let collector = spawn_collector(&rig.events);

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
        panic!("must be text");
    };
    assert!(unknown_text.text.contains("unknown tool"));
    assert!(failing_result.is_error);
    let ContentBlock::Text(failing_text) = &failing_result.content[0] else {
        panic!("must be text");
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
    let collector = spawn_collector(&rig.events);

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
    let collector = spawn_collector(&rig.events);

    let outcome = agent
        .prompt(user("do it"), &rig.env())
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(result.is_error);
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("must be text");
    };
    assert!(text.text.contains("token limit"));
    assert_eq!(rig.provider.recorded_requests().len(), 2);
}
