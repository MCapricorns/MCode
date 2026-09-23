//! Panic-isolation scenarios for tool dispatch: panics during execute,
//! panics in `Drop`, and `panic_any` payloads with panicking
//! destructors never unwind the prompt; dropping dispatch joins search
//! workers and drops the tool value.
//!
//! Part of the loop scenario groups listed in `common/mod.rs`.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use common::local_provider::LocalProvider;
use common::{NoArgs, Rig, text_turn, tool_turn, user};
use mycode_agent::Agent;
use mycode_agent::agent::AgentConfig;
use mycode_core::events::TurnOutcome;
use mycode_core::message::{ContentBlock, Message};
use mycode_tools::{
    Tool, ToolCtx, ToolError, ToolRegistry, ToolResult, ToolStream, live_search_thread_handles,
    live_search_workers, run_search_worker_until_cancel,
};
use serde_json::json;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Dropping dispatch must drop the execute future and join search workers.
struct DropSearchTool {
    dropped: Arc<AtomicBool>,
}

struct ExecGuard(Arc<AtomicBool>);

impl Drop for ExecGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
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
        run_search_worker_until_cancel(ctx.cancel.clone()).await?;
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
    entered: Arc<AtomicBool>,
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
    entered: Arc<AtomicBool>,
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
        self.entered.store(true, Ordering::Release);
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
        self.entered.store(true, Ordering::Release);
        ctx.cancel.cancelled().await;
        Ok(ToolResult::text("should not complete"))
    }
}

fn hang_drop_rig(entered: Arc<AtomicBool>) -> (CancellationToken, Rig, Agent) {
    let registry = ToolRegistry::new();
    registry.register(Arc::new(HangPanicDropTool { entered }));
    let cancel = CancellationToken::new();
    let rig = Rig {
        provider: LocalProvider::new(vec![tool_turn(
            "hang",
            vec![("c1", "hang_drop", json!({}))],
        )]),
        registry,
        hooks: mycode_agent::HookRunner::new(),
        events: broadcast::channel(256).0,
        cancel: cancel.clone(),
    };
    let agent = Agent::new(AgentConfig::new());
    (cancel, rig, agent)
}

async fn wait_until_entered(entered: &AtomicBool) {
    let started = Instant::now();
    loop {
        if entered.load(Ordering::Acquire) {
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
    let entered = Arc::new(AtomicBool::new(false));
    let (cancel, rig, mut agent) = hang_drop_rig(Arc::clone(&entered));
    let task = tokio::spawn(async move { agent.prompt(user("go"), &rig.env()).await });
    wait_until_entered(&entered).await;
    cancel.cancel();
    let outcome = task
        .await
        .expect("prompt task must not unwind from tool Drop panic")
        .expect("abort is a normal outcome");
    assert_eq!(outcome, TurnOutcome::Aborted);
}

#[tokio::test]
async fn dropping_dispatch_of_panic_on_drop_tool_does_not_unwind_prompt() {
    let entered = Arc::new(AtomicBool::new(false));
    let (_cancel, rig, mut agent) = hang_drop_rig(Arc::clone(&entered));
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
        hooks: mycode_agent::HookRunner::new(),
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

fn hang_panic_any_drop_rig(entered: Arc<AtomicBool>) -> (CancellationToken, Rig, Agent) {
    let registry = ToolRegistry::new();
    registry.register(Arc::new(HangPanicAnyDropTool { entered }));
    let cancel = CancellationToken::new();
    let rig = Rig {
        provider: LocalProvider::new(vec![tool_turn(
            "hang",
            vec![("c1", "hang_panic_any_drop", json!({}))],
        )]),
        registry,
        hooks: mycode_agent::HookRunner::new(),
        events: broadcast::channel(256).0,
        cancel: cancel.clone(),
    };
    let agent = Agent::new(AgentConfig::new());
    (cancel, rig, agent)
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
        hooks: mycode_agent::HookRunner::new(),
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
        hooks: mycode_agent::HookRunner::new(),
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
    let entered = Arc::new(AtomicBool::new(false));
    let (cancel, rig, mut agent) = hang_panic_any_drop_rig(Arc::clone(&entered));
    let task = tokio::spawn(async move { agent.prompt(user("go"), &rig.env()).await });
    wait_until_entered(&entered).await;
    cancel.cancel();
    let outcome = task
        .await
        .expect("prompt task must not unwind from payload Drop panic")
        .expect("abort is a normal outcome");
    assert_eq!(outcome, TurnOutcome::Aborted);
}

#[tokio::test]
async fn dropping_dispatch_of_panic_any_on_drop_tool_does_not_unwind_prompt() {
    let entered = Arc::new(AtomicBool::new(false));
    let (_cancel, rig, mut agent) = hang_panic_any_drop_rig(Arc::clone(&entered));
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
    let dropped = Arc::new(AtomicBool::new(false));
    let registry = ToolRegistry::new();
    registry.register(Arc::new(DropSearchTool {
        dropped: Arc::clone(&dropped),
    }));
    let cancel = CancellationToken::new();
    let rig = Rig {
        provider: LocalProvider::new(vec![tool_turn(
            "search",
            vec![("c1", "drop_search", json!({}))],
        )]),
        registry,
        hooks: mycode_agent::HookRunner::new(),
        events: broadcast::channel(256).0,
        cancel: cancel.clone(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let task = tokio::spawn(async move { agent.prompt(user("go"), &rig.env()).await });
    let started = Instant::now();
    loop {
        if live_search_workers() > 0 {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "search worker never started"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    cancel.cancel();
    let _ = task.await;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if live_search_workers() == 0 && live_search_thread_handles() == 0 {
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
