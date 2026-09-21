//! `HookRunner` defines the agent loop's hook dispatch points.
//!
//! Production installs two of them through
//! [`HookRunner::with_before_request`] (history compaction immediately
//! before a provider request) and [`HookRunner::with_before_tool`] (an
//! observer fired after tool-call admission, right before dispatch).
//! Tests can additionally install a tool-call gate via
//! [`HookRunner::with_test_gate`] to verify argument rebinding and
//! blocked dispatch.
//!
//! The three dispatch semantics (pi's model):
//!
//! * [`notify`](HookRunner::notify) — fire-and-forget broadcast.
//! * [`transform`](HookRunner::transform) — middleware chain: value in,
//!   possibly rewritten value out.
//! * [`gate`](HookRunner::gate) — may rewrite the payload in place and/or
//!   block the action ([`GateResult::Block`]).

use mycode_core::Request;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Outcome of a [`HookRunner::gate`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateResult {
    /// No objection; continue (payload possibly rewritten).
    Pass,
    /// Block the action. The reason is surfaced to the model as an
    /// `is_error` tool result; stop-gate reasons are not surfaced.
    Block(String),
}

/// The loop node at which a hook is invoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    /// A turn started (Notify).
    TurnStart,
    /// A turn ended (Notify).
    TurnEnd,
    /// User input is about to enter the context — prompt, steer, or
    /// follow-up (Transform).
    UserPrompt,
    /// Before every LLM request (Transform: may rewrite the request).
    BeforeProviderRequest,
    /// An assistant message started streaming (Notify).
    MessageStart,
    /// An assistant message finished streaming (Transform: may rewrite
    /// the whole message before it enters history).
    MessageEnd,
    /// A tool call is about to be dispatched (Gate: may rewrite arguments
    /// or block).
    ToolCall,
    /// A tool result is about to be written back into the context
    /// (Transform: redaction, summarization, truncation).
    ToolResult,
    /// The agent is about to stop (Gate: plugins may block the stop and
    /// inject follow-ups).
    StopGate,
}

/// Hook runner for the loop's dispatch points.
///
/// [`notify`](HookRunner::notify), [`transform`](HookRunner::transform),
/// and the [`GateResult::Pass`] arm of [`gate`](HookRunner::gate) are
/// pass-throughs with no production subscriber today. The live production
/// paths are [`prepare_request`](HookRunner::prepare_request), which runs
/// the installed before-request rewrite (history compaction), and
/// [`observe_before_tool`](HookRunner::observe_before_tool), which fires
/// the installed before-tool observer immediately before dispatch;
/// panics in the observer are contained and never affect the turn.
/// [`with_test_gate`](HookRunner::with_test_gate) adds the test-only
/// tool-call gate that rewrites arguments or blocks the dispatch.
type TestGate = Arc<dyn Fn(&mut Value) -> GateResult + Send + Sync>;
/// The observer clones what it needs while invoked and returns a future;
/// asynchronous observers may offload blocking work (file snapshots) onto
/// `spawn_blocking` instead of stalling the calling executor.
type BeforeToolFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type BeforeToolObserver = Arc<dyn Fn(&str, &Value) -> BeforeToolFuture + Send + Sync>;
type BeforeRequestFuture = Pin<Box<dyn Future<Output = Request> + Send>>;
type BeforeRequest = Arc<dyn Fn(Request) -> BeforeRequestFuture + Send + Sync>;

pub struct HookRunner {
    test_gate: Option<TestGate>,
    before_tool: Option<BeforeToolObserver>,
    before_request: Option<BeforeRequest>,
}

impl HookRunner {
    /// An empty runner.
    pub fn new() -> Self {
        Self {
            test_gate: None,
            before_tool: None,
            before_request: None,
        }
    }

    /// Installs an observer fired after tool-call admission and argument
    /// validation, immediately before dispatch. The observer returns a
    /// future that is awaited before the tool runs; panics — during the
    /// call or while polling — are contained and never affect the turn.
    pub fn with_before_tool<Fut>(
        mut self,
        observer: impl Fn(&str, &Value) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.before_tool = Some(Arc::new(move |tool, args| Box::pin(observer(tool, args))));
        self
    }

    /// Rewrites the provider request immediately before it is sent.
    ///
    /// Used for history compaction: the rewritten `messages` are written
    /// back onto the in-memory agent history after this hook returns.
    pub fn with_before_request<Fut>(
        mut self,
        hook: impl Fn(Request) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        Fut: Future<Output = Request> + Send + 'static,
    {
        self.before_request = Some(Arc::new(move |request| Box::pin(hook(request))));
        self
    }

    /// Runs the before-request rewrite, or returns the request unchanged.
    pub async fn prepare_request(&self, request: Request) -> Request {
        match &self.before_request {
            Some(hook) => hook(request).await,
            None => request,
        }
    }

    /// Fires the before-tool observer, if installed.
    pub async fn observe_before_tool(&self, tool: &str, args: &Value) {
        let Some(observer) = &self.before_tool else {
            return;
        };
        let Ok(future) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer(tool, args)))
        else {
            eprintln!("mycode-agent: before_tool observer panicked (contained)");
            return;
        };
        // Polling is isolated through a task so an observer panic unwinds
        // into a JoinError instead of the dispatch path.
        if tokio::spawn(future).await.is_err() {
            eprintln!("mycode-agent: before_tool observer panicked (contained)");
        }
    }

    /// Install a tool-call gate used by tests to rewrite or block arguments.
    pub fn with_test_gate(
        mut self,
        gate: impl Fn(&mut Value) -> GateResult + Send + Sync + 'static,
    ) -> Self {
        self.test_gate = Some(Arc::new(gate));
        self
    }
}

impl Default for HookRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl HookRunner {
    /// Broadcast an event; the return value is ignored (Notify).
    pub async fn notify(&self, _event: HookEvent) {}

    /// Passes `value` through the transform point.
    pub async fn transform<T>(&self, _event: HookEvent, value: T) -> T {
        value
    }

    /// Inspects a gate payload. Production passes; tests may rewrite or block.
    ///
    /// Call sites: `ToolCall` payloads are the call's arguments (may be
    /// rewritten before execution); `StopGate` currently receives `Value::Null`.
    pub async fn gate(&self, event: HookEvent, payload: &mut Value) -> GateResult {
        if event == HookEvent::ToolCall
            && let Some(gate) = &self.test_gate
        {
            return gate(payload);
        }
        GateResult::Pass
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn placeholder_methods_pass_through() {
        let hooks = HookRunner::new();
        hooks.notify(HookEvent::TurnStart).await;
        assert_eq!(
            hooks
                .transform(HookEvent::UserPrompt, "unchanged".to_string())
                .await,
            "unchanged"
        );
        let mut payload = serde_json::json!({"command": "ls"});
        assert_eq!(
            hooks.gate(HookEvent::ToolCall, &mut payload).await,
            GateResult::Pass
        );
        assert_eq!(payload, serde_json::json!({"command": "ls"}));
        // Default constructible (the loop stores it in TurnEnv).
        let _hooks: HookRunner = Default::default();
    }
}
