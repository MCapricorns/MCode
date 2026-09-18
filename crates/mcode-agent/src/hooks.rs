//! `HookRunner` defines the agent loop's hook dispatch points.
//!
//! The production runner currently has no installed hooks. Tests can install a
//! tool-call gate to verify argument rebinding and blocked dispatch.
//!
//! The three dispatch semantics (pi's model):
//!
//! * [`notify`](HookRunner::notify) — fire-and-forget broadcast.
//! * [`transform`](HookRunner::transform) — middleware chain: value in,
//!   possibly rewritten value out.
//! * [`gate`](HookRunner::gate) — may rewrite the payload in place and/or
//!   block the action ([`GateResult::Block`]).

use serde_json::Value;
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

/// Hook runner with no production hook implementation. Every method passes through:
/// [`notify`](HookRunner::notify) does nothing, [`transform`](HookRunner::transform)
/// returns its value unchanged, and [`gate`](HookRunner::gate) always
/// passes except when tests install [`HookRunner::with_test_gate`], which
/// may rewrite arguments or block.
type TestGate = Arc<dyn Fn(&mut Value) -> GateResult + Send + Sync>;
type BeforeToolObserver = Arc<dyn Fn(&str, &Value) + Send + Sync>;

pub struct HookRunner {
    test_gate: Option<TestGate>,
    before_tool: Option<BeforeToolObserver>,
}

impl HookRunner {
    /// An empty runner.
    pub fn new() -> Self {
        Self {
            test_gate: None,
            before_tool: None,
        }
    }

    /// Installs a synchronous observer fired after tool-call admission and
    /// argument validation, immediately before dispatch. Observer panics
    /// are contained and never affect the turn.
    pub fn with_before_tool(
        mut self,
        observer: impl Fn(&str, &Value) + Send + Sync + 'static,
    ) -> Self {
        self.before_tool = Some(Arc::new(observer));
        self
    }

    /// Fires the before-tool observer, if installed.
    pub fn observe_before_tool(&self, tool: &str, args: &Value) {
        if let Some(observer) = &self.before_tool {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                observer(tool, args);
            }));
            if outcome.is_err() {
                eprintln!("mcode-agent: before_tool observer panicked (contained)");
            }
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
