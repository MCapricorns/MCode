//! First-party session substrate: durable session service on the typed task
//! runtime inside a Host-owned generation fence.
//!
//! This crate is core-owned product infrastructure, not a plugin surface. It
//! carries no guest runtime: `generation` fences durable work, `runtime`
//! provides admission and the serialized task-actor protocol, and `session`
//! exposes the strict facade consumed by the desktop bridge.
#![warn(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]
#![forbid(unsafe_code)]

/// Host-owned generation fencing for durable publication.
pub mod generation;
/// Atomic admission and the serialized first-party task-actor protocol.
pub mod runtime;
/// First-party built-in Session service over the typed task runtime.
pub mod session;
