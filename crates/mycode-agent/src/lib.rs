//! `mycode-agent` — the UI-free agent runtime: the double loop plus the
//! durable session ledger a conversation is recorded in.
//!
//! The two layers do not depend on each other. The loop below runs happily
//! against no storage at all, which is how the tests drive it; [`session`]
//! is the event-sourced ledger a host writes turns into, on its own task
//! runtime inside a generation fence. They share a crate because they share
//! a lifetime — one conversation, one durable history — and nothing else in
//! the workspace needs one without the other.
//!
//! ```text
//! caller ──Message──► Agent::prompt(msg, &TurnEnv)
//!                        │
//!                        ▼ double loop
//!        outer: drain follow-up queue whenever the agent would stop
//!          inner: build request → provider.stream → mirror deltas as
//!                 AgentEvent stream → dispatch registered tools → write
//!                 results back
//!                        │
//!                        ▼
//!        steer queue drained after every response cycle (jumps the
//!        queue ahead of follow-ups); abort() / env.cancel ends the
//!        turn with TurnOutcome::Aborted
//! ```
//!
//! * [`Agent`] owns the conversation state and the steer/follow-up
//!   queues; [`agent::AgentHandle`] lets other tasks steer, follow up, or
//!   abort while a turn streams.
//! * [`TurnEnv`] injects everything ambient — provider, tool registry,
//!   hooks, cancellation, and the event bus. Registered schema-valid
//!   tools execute directly; no permission callback is required.
//! * [`HookRunner`] owns the loop's hook points: production installs a
//!   before-request rewrite (history compaction) and a before-tool
//!   observer; tests additionally install a tool-call gate that rewrites
//!   or blocks arguments.

pub mod agent;
pub mod env;
pub mod hooks;
mod prompt;
pub mod session;
mod turn;

pub use agent::{Agent, AgentConfig};
pub use env::TurnEnv;
pub use hooks::HookRunner;
pub use prompt::build_system_prompt;
