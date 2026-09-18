//! First-party built-in Session service.
//!
//! Event-sourced branch/resume/rewind over a durable per-session ledger:
//! strict manifests are published through the hardened owned-file
//! transaction, branch logs are append-only framed records with payload
//! digests, and staged payloads make reservations single-use under
//! expected-head compare-and-swap. The service runs on the T8 typed task
//! runtime inside a Host-owned generation fence; recovery is chunked per
//! pull and reports the frozen `recovering`/`replaying` progress phases.
//! The typed surface is the first-party Rust projection of design doc
//! `07-pack-abi-session-resources.md` §3.

// Rust guideline compliant 2026-09-18.

mod actor;
mod digest;
mod dto;
mod fs;
mod ids;
mod ledger;
mod service;
mod store;

#[doc(inline)]
pub use digest::{DIGEST_PREFIX, is_canonical_digest};
#[doc(inline)]
pub use dto::{
    AppendedResult, BranchHead, BranchMutationKind, BranchedResult, ConflictResult, CreatedResult,
    EventKind, EventReservationView, EventsResult, HeadStamp, LoadedEvent, MAX_BRANCHES,
    MAX_EVENT_PAYLOAD_BYTES, MAX_READ_LIMIT, MAX_USAGE_PAYLOAD_BYTES, OpenedResult, SessionError,
    SessionEvent, SessionProgress, SessionPull, SessionRequest, SessionResult,
};
#[doc(inline)]
pub use ids::{BranchId, BranchReservationId, SessionCallId, SessionEventId, SessionId};
#[doc(inline)]
pub use service::SessionService;
#[doc(inline)]
pub use store::{MAX_MANIFEST_BYTES, MAX_SESSION_TOTAL_BYTES};
