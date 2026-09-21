//! Shared integration-test helpers for `mycode-agent`.
//!
//! Run from the workspace root:
//! - `cargo test -p mycode-agent` or `cargo t-agent`
//! - `cargo test -p mycode-core` or `cargo t-core`
//!
//! [`local_provider`] is the scripted in-process provider used by `loop_test`.

pub mod local_provider;
