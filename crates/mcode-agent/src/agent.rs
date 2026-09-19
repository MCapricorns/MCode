//! The agent double loop (design doc `01-agent-core.md` §3, adopting
//! pi's `runAgentLoop` structure): outer loop drains the follow-up
//! queue, inner loop runs stream→tool cycles until the model stops
//! calling tools.
//!
//! # Turn model
//!
//! One `prompt()` call is one **turn** (`TurnStarted` … `TurnEnded`).
//! A turn consists of any number of LLM response cycles; between cycles
//! the loop may inject queued user input.
//!
//! # Steer vs follow-up (the loop-level capability)
//!
//! * **steer** — the user's interruption: "stop what you're doing". The
//!   message is queued from any task ([`Agent::steer`] /
//!   [`AgentHandle::steer`]) and delivered at the first boundary after
//!   the current response's tool calls complete — it **jumps the queue**
//!   and becomes the next user input the model sees, ahead of any
//!   follow-ups. The model answers it inside the same turn, and the
//!   turn ends with [`TurnOutcome::Steered`].
//! * **follow_up** — a nudge for when the agent would otherwise stop
//!   (subagent callbacks, timers). The message is only ever delivered
//!   at a natural stop; the turn continues and its outcome is
//!   unaffected.
//! * [`QueueMode`] controls how many queued messages one boundary
//!   consumes (`OneAtATime` by default, pi's default).
//!
//! # Abort
//!
//! `abort()` (or firing `TurnEnv::cancel`) cancels the turn's
//! cancellation token — a child of the caller's token. The in-flight
//! provider stream terminates with `Cancelled`, partial assistant
//! messages are *not* kept (only completed messages enter history),
//! `is_streaming` resets, and `prompt()` returns
//! [`TurnOutcome::Aborted`] — never a half `TurnEnded::Completed`.
//! Queued steer/follow-up messages survive an abort; callers clear them
//! explicitly when intended.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use mcode_core::McodeError;
use mcode_core::events::{AgentEvent, TurnOutcome};
use mcode_core::message::{ContentBlock, Message, StopReason, ToolCall};
use tokio_util::sync::CancellationToken;

use crate::env::TurnEnv;
use crate::hooks::{GateResult, HookEvent};
use crate::turn::{self, TurnFailure};

/// How many queued messages one delivery boundary consumes.
///
/// `OneAtATime` gives each queued message its own model boundary (the
/// model can act on it before the next arrives); `All` batches them
/// into one request. Applies to both the steer and follow-up queues.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QueueMode {
    /// Deliver every queued message at the next boundary (one request).
    All,
    /// Deliver one queued message per boundary (pi's default).
    #[default]
    OneAtATime,
}

/// Static provider-neutral agent configuration.
#[derive(Debug, Clone, Default)]
pub struct AgentConfig {
    /// System prompt parts, emitted in order ahead of the history.
    pub system_prompt: Vec<String>,
    /// Requested reasoning effort for providers that support it.
    pub reasoning: Option<mcode_provider_api::ReasoningLevel>,
}

impl AgentConfig {
    /// Creates a config with no explicit system prompt.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one system prompt part.
    #[must_use]
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt.push(prompt.into());
        self
    }

    /// Requests a reasoning effort level.
    #[must_use]
    pub fn with_reasoning(
        mut self,
        level: impl Into<Option<mcode_provider_api::ReasoningLevel>>,
    ) -> Self {
        self.reasoning = level.into();
        self
    }
}

/// The agent's conversation state.
#[derive(Debug, Default)]
pub struct AgentState {
    /// Conversation history: user inputs, assistant messages, tool
    /// results. Only completed messages live here — a response aborted
    /// mid-stream never enters.
    pub messages: Vec<Message>,
    /// Whether a turn is currently streaming.
    pub is_streaming: bool,
}

impl AgentState {
    /// The message history.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
}

/// Interior-mutability control block shared between the [`Agent`] and
/// any number of [`AgentHandle`]s: the steer/follow-up queues (writable
/// from other tasks while a turn streams) and the active turn's
/// cancellation token.
pub(crate) struct AgentControl {
    steer_queue: Mutex<VecDeque<Message>>,
    followup_queue: Mutex<VecDeque<Message>>,
    /// Cancellation token of the turn currently streaming, if any.
    /// `abort()` fires it. Replaced (not reset) every turn.
    active_cancel: Mutex<Option<CancellationToken>>,
}

impl AgentControl {
    pub(crate) fn new() -> Self {
        Self {
            steer_queue: Mutex::new(VecDeque::new()),
            followup_queue: Mutex::new(VecDeque::new()),
            active_cancel: Mutex::new(None),
        }
    }

    pub(crate) fn steer(&self, msg: Message) {
        self.steer_queue
            .lock()
            .expect("steer queue lock poisoned")
            .push_back(msg);
    }

    pub(crate) fn follow_up(&self, msg: Message) {
        self.followup_queue
            .lock()
            .expect("follow-up queue lock poisoned")
            .push_back(msg);
    }

    pub(crate) fn abort(&self) {
        if let Some(token) = self
            .active_cancel
            .lock()
            .expect("active-cancel lock poisoned")
            .as_ref()
        {
            token.cancel();
        }
    }

    pub(crate) fn set_active(&self, token: Option<CancellationToken>) {
        *self
            .active_cancel
            .lock()
            .expect("active-cancel lock poisoned") = token;
    }

    pub(crate) fn drain_steer(&self, mode: QueueMode) -> Vec<Message> {
        drain_queue(&self.steer_queue, mode)
    }

    pub(crate) fn drain_followup(&self, mode: QueueMode) -> Vec<Message> {
        drain_queue(&self.followup_queue, mode)
    }

    pub(crate) fn has_queued(&self) -> bool {
        let steer = self.steer_queue.lock().expect("steer queue lock poisoned");
        let followup = self
            .followup_queue
            .lock()
            .expect("follow-up queue lock poisoned");
        !steer.is_empty() || !followup.is_empty()
    }

    pub(crate) fn clear(&self) {
        self.steer_queue
            .lock()
            .expect("steer queue lock poisoned")
            .clear();
        self.followup_queue
            .lock()
            .expect("follow-up queue lock poisoned")
            .clear();
    }
}

impl Default for AgentControl {
    fn default() -> Self {
        Self::new()
    }
}

/// Drain a queue according to [`QueueMode`].
fn drain_queue(queue: &Mutex<VecDeque<Message>>, mode: QueueMode) -> Vec<Message> {
    let mut queue = queue.lock().expect("agent queue lock poisoned");
    match mode {
        QueueMode::All => queue.drain(..).collect(),
        QueueMode::OneAtATime => queue.pop_front().into_iter().collect(),
    }
}

/// A shareable handle to an [`Agent`]'s queues and abort switch.
///
/// The loop borrows the agent exclusively while a turn streams; user
/// input still has to reach it (a TUI, a caller task, or a test). The
/// handle is a cheap clone carrying only the interior-mutability
/// control block, so `steer` / `follow_up` / `abort` work from any task
/// while `prompt()` runs — pi's reference-semantics `Agent.steer()`
/// translated to Rust.
#[derive(Clone)]
pub struct AgentHandle {
    control: Arc<AgentControl>,
}

impl AgentHandle {
    /// Queue a steering message (see [`Agent::steer`]).
    pub fn steer(&self, msg: Message) {
        self.control.steer(msg);
    }

    /// Queue a follow-up message (see [`Agent::follow_up`]).
    pub fn follow_up(&self, msg: Message) {
        self.control.follow_up(msg);
    }

    /// Abort the in-flight turn (see [`Agent::abort`]).
    pub fn abort(&self) {
        self.control.abort();
    }

    /// Whether either queue holds pending messages.
    pub fn has_queued_messages(&self) -> bool {
        self.control.has_queued()
    }

    /// Drop every queued steer/follow-up message.
    pub fn clear_queues(&self) {
        self.control.clear();
    }
}

/// The UI-free, session-free agent: state + queues + the double loop.
///
/// ```text
/// prompt(msg)
/// ├─ TurnStarted, msg enters history
/// ├─ outer loop ─────────────────────────────────────────────┐
/// │  inner loop: while (tool calls || pending input)          │
/// │    inject pending steer/follow-up as user messages        │
/// │    stream response (MessageDelta …) → history             │
/// │    dispatch tool calls (registry → ToolResult → hist.)    │
/// │    drain steer queue → pending (jumps ahead)              │←┘
/// │  would-stop: stop_gate → follow-up queue → else break     │
/// └─ TurnEnded(Completed | Steered | Aborted)
/// ```
pub struct Agent {
    config: AgentConfig,
    state: AgentState,
    control: Arc<AgentControl>,
    queue_mode: QueueMode,
}

impl Agent {
    /// An agent with empty history and the given static config.
    pub fn new(config: AgentConfig) -> Self {
        Self {
            config,
            state: AgentState::default(),
            control: Arc::new(AgentControl::new()),
            queue_mode: QueueMode::default(),
        }
    }

    /// A handle through which other tasks can steer / follow up /
    /// abort while a turn streams.
    pub fn handle(&self) -> AgentHandle {
        AgentHandle {
            control: Arc::clone(&self.control),
        }
    }

    /// Queue a steering message: delivered at the first boundary after
    /// the current response's tool calls complete; becomes the next
    /// user input, ahead of follow-ups. Safe to call while a turn
    /// streams (from a handle) or while idle (injected before the
    /// first response of the next turn).
    pub fn steer(&self, msg: Message) {
        self.control.steer(msg);
    }

    /// Queue a follow-up message: delivered only when the agent would
    /// otherwise stop, pushing the turn forward.
    pub fn follow_up(&self, msg: Message) {
        self.control.follow_up(msg);
    }

    /// Abort the in-flight turn, if any: cancels its token; the turn
    /// unwinds at the next await point and ends
    /// [`TurnOutcome::Aborted`]. A no-op when idle. Queued messages
    /// are left untouched.
    ///
    /// [`TurnOutcome::Aborted`]: mcode_core::events::TurnOutcome::Aborted
    pub fn abort(&self) {
        self.control.abort();
    }

    /// Read-only access to the conversation state.
    /// Loads replay history before the first prompt.
    pub fn seed_history(&mut self, messages: impl IntoIterator<Item = Message>) {
        self.state.messages.extend(messages);
    }

    pub fn state(&self) -> &AgentState {
        &self.state
    }

    /// Mutable access for callers restoring or rewinding Agent state.
    pub fn state_mut(&mut self) -> &mut AgentState {
        &mut self.state
    }

    /// The static agent config.
    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// The queue delivery mode (default [`QueueMode::OneAtATime`]).
    pub fn queue_mode(&self) -> QueueMode {
        self.queue_mode
    }

    /// Change the queue delivery mode.
    pub fn set_queue_mode(&mut self, mode: QueueMode) {
        self.queue_mode = mode;
    }

    /// Whether either queue holds pending messages.
    pub fn has_queued_messages(&self) -> bool {
        self.control.has_queued()
    }

    /// Drop every queued steer/follow-up message.
    pub fn clear_queues(&self) {
        self.control.clear();
    }

    /// Run one turn: push `msg` (a user message) into the history,
    /// stream responses and dispatch tools until the model stops and no
    /// steer/follow-up is pending.
    ///
    /// Cancellation (`env.cancel` or `abort()`) ends the turn with
    /// [`TurnOutcome::Aborted`]. A provider failure emits
    /// [`AgentEvent::Error`] + `TurnEnded(Aborted)` and returns
    /// `Err`; tool-level failures never end the turn (they become
    /// `is_error` tool results the model can react to).
    pub async fn prompt(
        &mut self,
        msg: Message,
        env: &TurnEnv<'_>,
    ) -> Result<TurnOutcome, McodeError> {
        // Child token: fires on env.cancel OR our abort(); a cancelled
        // child never leaks into the parent or the next turn.
        let token = env.cancel.child_token();
        self.control.set_active(Some(token.clone()));
        self.state.is_streaming = true;

        let result = run_turn(
            &self.config,
            &mut self.state,
            &self.control,
            self.queue_mode,
            msg,
            env,
            &token,
        )
        .await;

        self.state.is_streaming = false;
        self.control.set_active(None);
        result
    }
}

/// One full turn: `TurnStarted … TurnEnded` bracketing the double loop.
async fn run_turn(
    config: &AgentConfig,
    state: &mut AgentState,
    control: &AgentControl,
    queue_mode: QueueMode,
    msg: Message,
    env: &TurnEnv<'_>,
    token: &CancellationToken,
) -> Result<TurnOutcome, McodeError> {
    turn::emit(env, AgentEvent::TurnStarted);
    env.hooks.notify(HookEvent::TurnStart).await;

    let outcome = double_loop(config, state, control, queue_mode, msg, env, token).await;

    env.hooks.notify(HookEvent::TurnEnd).await;
    match &outcome {
        Ok(outcome) => turn::emit(env, AgentEvent::TurnEnded(*outcome)),
        // The Error event was already emitted at the failure site; the
        // turn still ends for event subscribers.
        Err(_) => turn::emit(env, AgentEvent::TurnEnded(TurnOutcome::Aborted)),
    }
    outcome
}

/// The double loop itself (no turn bracket events).
async fn double_loop(
    config: &AgentConfig,
    state: &mut AgentState,
    control: &AgentControl,
    queue_mode: QueueMode,
    prompt_msg: Message,
    env: &TurnEnv<'_>,
    token: &CancellationToken,
) -> Result<TurnOutcome, McodeError> {
    // The user prompt enters the context (hooks may rewrite it first).
    let prompt_msg = env.hooks.transform(HookEvent::UserPrompt, prompt_msg).await;
    turn::push_message(env, state, prompt_msg);

    // Steering queued while the agent was idle is injected before the
    // first response (pi polls the steering queue at loop entry).
    let mut pending: Vec<Message> = control.drain_steer(queue_mode);
    let mut steered = !pending.is_empty();
    let mut aborted = false;

    'outer: loop {
        // Inner loop: stream→tool cycles while the model calls tools or
        // input is waiting to be injected.
        let mut has_tool_calls = true;
        while has_tool_calls || !pending.is_empty() {
            for msg in pending.drain(..) {
                let msg = env.hooks.transform(HookEvent::UserPrompt, msg).await;
                turn::push_message(env, state, msg);
            }
            if token.is_cancelled() {
                aborted = true;
                break 'outer;
            }

            let assistant = match turn::stream_assistant(env, token, config, state).await {
                Ok(message) => message,
                Err(TurnFailure::Aborted) => {
                    aborted = true;
                    break 'outer;
                }
                Err(TurnFailure::Error(err)) => return Err(err),
            };

            let calls: Vec<ToolCall> = assistant
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolCall(call) => Some(call.clone()),
                    _ => None,
                })
                .collect();

            if calls.is_empty() {
                has_tool_calls = false;
            } else if assistant.stop_reason == StopReason::Length {
                // Truncated arguments are never executed (pi parity);
                // the model re-issues the calls.
                for call in &calls {
                    let message = turn::fail_truncated_call(env, call);
                    turn::push_message(env, state, Message::ToolResult(message));
                }
                has_tool_calls = true;
            } else {
                for (index, call) in calls.iter().enumerate() {
                    if token.is_cancelled() {
                        // Abort mid-dispatch of a multi-call response.
                        // The assistant message carrying *all* the calls
                        // is already in history. Every announced tool call
                        // must have a corresponding ToolResult before the
                        // next request, so write cancellation results for
                        // the undispatched remainder before unwinding (pi
                        // parity).
                        for call in &calls[index..] {
                            let message = turn::fail_cancelled_call(env, call);
                            turn::push_message(env, state, Message::ToolResult(message));
                        }
                        aborted = true;
                        break 'outer;
                    }
                    let message = turn::dispatch_tool_call(env, token, call).await;
                    turn::push_message(env, state, Message::ToolResult(message));
                }
                has_tool_calls = true;
            }

            if !has_tool_calls {
                // The current stop gate always passes. Its payload remains
                // reserved for a hook-provided follow-up.
                let mut payload = serde_json::Value::Null;
                if !matches!(
                    env.hooks.gate(HookEvent::StopGate, &mut payload).await,
                    GateResult::Pass
                ) {
                    has_tool_calls = true;
                }
            }

            // Steering: delivered after the current response's tool
            // calls finished — the steer message jumps ahead of
            // everything else to become the next user input.
            let steered_now = control.drain_steer(queue_mode);
            steered = steered || !steered_now.is_empty();
            pending.extend(steered_now);
        }

        // The agent would stop here: follow-ups push it forward.
        let followups = control.drain_followup(queue_mode);
        if followups.is_empty() {
            break;
        }
        pending.extend(followups);
    }

    if aborted {
        return Ok(TurnOutcome::Aborted);
    }
    if steered {
        return Ok(TurnOutcome::Steered);
    }
    Ok(TurnOutcome::Completed)
}
