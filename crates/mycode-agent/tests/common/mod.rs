//! Shared integration-test fixtures and helpers for `mycode-agent`.
//!
//! Run from the workspace root:
//! - `cargo test -p mycode-agent` or `cargo t-agent`
//! - `cargo test -p mycode-core` or `cargo t-core`
//!
//! [`local_provider`] is the scripted in-process provider used by every
//! loop scenario file. The scenario groups split out of the original
//! `loop_test.rs` are:
//!
//! - `loop_basic` — single-turn text replies, tool-call loop write-back,
//!   queue-mode drain semantics, and steer queued while idle.
//! - `loop_steer_followup` — steer jumping the queue mid-stream and
//!   follow-ups continuing an agent that is about to stop.
//! - `loop_abort` — env-cancel / agent-handle aborts keeping state
//!   consistent, including multi-call aborts answering every call.
//! - `loop_provider_failures` — provider errors, setup cancellation,
//!   oversized requests, and dangling streams.
//! - `loop_tools` — registered-tool dispatch, progress streaming,
//!   self-terminating and sustained-progress tools, unknown/failing/
//!   panicking tools, and length-truncated calls.
//! - `loop_preflight` — same-name overrides skipping search/file
//!   preflight, hook blocks preceding preflight, and prepared-file
//!   rebinding.
//! - `loop_hooks` — hook rewrites of search paths, including the
//!   Windows share-locked alias case.
//! - `loop_tool_panics` — panic-on-drop and panic-any isolation plus
//!   search-worker cleanup on dropped dispatch.

// Each scenario binary compiles this module independently, so fixtures
// used only by other binaries would otherwise trip dead_code.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mycode_agent::{HookRunner, TurnEnv};
use mycode_core::events::{AgentEvent, MessageDelta};
use mycode_core::message::{
    AssistantMessage, ContentBlock, Message, StopReason, ToolCall, UserMessage,
};
use mycode_tools::{Tool, ToolCtx, ToolError, ToolRegistry, ToolResult, ToolStream};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub mod local_provider;

pub use local_provider::{LocalProvider, LocalTurn};

/// A constant inter-event delay so concurrent test tasks (steerer,
/// canceller) win the race deterministically on the single-threaded
/// test runtime.
pub const DELAY: Duration = Duration::from_millis(2);
/// Absolute guard for dispatcher regressions and their spawned producer cleanup.
pub const DISPATCH_TEST_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------
// Shared test tools
// ---------------------------------------------------------------------

/// Echoes its `text` argument back.
pub struct EchoTool;

#[derive(Deserialize, JsonSchema)]
pub struct EchoArgs {
    /// Text to echo back.
    pub text: String,
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

/// Empty argument set shared by the no-argument fixtures.
#[derive(Deserialize, JsonSchema)]
pub struct NoArgs {}

/// Always fails with an execution error.
pub struct FailingTool;

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
pub struct PanickingTool;

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
pub struct ProgressTool;

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

// ---------------------------------------------------------------------
// Test rig: owns the ambient objects, builds a TurnEnv per call
// ---------------------------------------------------------------------

pub struct Rig {
    pub provider: LocalProvider,
    pub registry: ToolRegistry,
    pub hooks: HookRunner,
    pub events: broadcast::Sender<AgentEvent>,
    pub cancel: CancellationToken,
}

impl Rig {
    pub fn new(provider: LocalProvider) -> Self {
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

    pub fn env(&self) -> TurnEnv<'_> {
        TurnEnv::new(&self.provider, &self.registry, &self.hooks)
            .with_events(self.events.clone())
            .with_cancel(self.cancel.clone())
    }

    pub fn env_at(&self, cwd: std::path::PathBuf) -> TurnEnv<'_> {
        self.env().with_cwd(cwd)
    }
}

// ---------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------

pub fn user(text: &str) -> Message {
    Message::User(UserMessage::text(text))
}

pub fn text_turn(text: &str) -> LocalTurn {
    LocalTurn::Message(AssistantMessage {
        blocks: vec![ContentBlock::Text(text.into())],
        usage: None,
        stop_reason: StopReason::Stop,
    })
}

pub fn tool_turn(text: &str, calls: Vec<(&str, &str, Value)>) -> LocalTurn {
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
pub fn spawn_collector(events: &broadcast::Sender<AgentEvent>) -> JoinHandle<Vec<AgentEvent>> {
    let mut rx = events.subscribe();
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
pub fn spawn_on_first_delta<F: FnOnce() + Send + 'static>(rig: &Rig, f: F) -> JoinHandle<()> {
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
pub fn spawn_on_first_tool_completed<F: FnOnce() + Send + 'static>(
    rig: &Rig,
    f: F,
) -> JoinHandle<()> {
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

pub fn position(events: &[AgentEvent], pred: impl Fn(&AgentEvent) -> bool, what: &str) -> usize {
    events
        .iter()
        .position(pred)
        .unwrap_or_else(|| panic!("missing event: {what}; events: {events:#?}"))
}

pub fn tool_result(events: &[AgentEvent]) -> mycode_core::message::ToolResultMessage {
    events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCompleted { result, .. } => Some(result.clone()),
            _ => None,
        })
        .expect("a ToolCompleted event must exist")
}
