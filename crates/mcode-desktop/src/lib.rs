//! GPUI desktop frontend for MCode.
//!
//! The desktop is a thin frontend over the shared core: a background
//! [`bridge::CoreBridge`] thread owns the tokio-backed core services, the
//! pure [`view_model`] holds every UI state transition, and the GPUI layer
//! only renders state and dispatches commands. No core type, file path, or
//! credential ever reaches this crate's render code.
pub mod bridge;
pub mod export;
pub mod ui;
pub mod view_model;
pub mod workspace;
