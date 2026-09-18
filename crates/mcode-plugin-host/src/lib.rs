//! External Pack/asset ABI substrates over a fail-closed Wasmtime runtime.
//!
//! Scanner-first preflight validates the four sole-current external component
//! worlds (Provider, Web, MCP, Usage). Exact Pack IDs load only through a
//! matching current Host generation, with canonical inventory digest
//! verification and no directory discovery. Exact configured Pack sets are
//! prepared in private Stores and atomically replace one generation's active
//! set only after final authority revalidation under the Host-owned generation
//! fence. Private Provider bindings and pure DTO validators remain Store-free.
//! The runtime module also carries the first-party typed task runtime
//! substrate shared by every built-in feature family.

// Rust guideline compliant 2026-09-05.

#![warn(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]
#![forbid(unsafe_code)]

mod component;
mod component_scanner;
mod component_shape;
mod component_world;
mod error;
mod generation;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "T10+ consumes generation-fenced Pack activation")
)]
mod pack_activation;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "T10+ consumes the generation-bound Pack candidate boundary"
    )
)]
mod pack_loading;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "T10+ consumes exact configured Pack selection")
)]
mod pack_selection;
mod pack_wit;
mod provider_routes;
mod provider_validation;
mod provider_wit;
/// Scanner-gated, fail-closed Wasmtime ownership and admission.
pub mod runtime;
/// First-party built-in Session service over the typed task runtime.
pub mod session;

#[doc(inline)]
pub use component::{ComponentLimits, MAX_COMPONENT_BYTES, preflight_component};
#[doc(inline)]
pub use component_world::ComponentWorld;
#[doc(inline)]
pub use error::{ImportCategory, PreflightError};
#[doc(inline)]
pub use pack_selection::PackConfigurationError;
#[doc(inline)]
pub use provider_routes::{
    AuthFingerprint, AuthSlotId, EndpointFingerprint, MAX_PROVIDER_ROUTE_CLAIMS,
    MAX_PROVIDER_ROUTE_CLAIMS_PER_BATCH, MAX_USAGE_CONTEXTS_PER_LEDGER, ModelAlias, ModelId,
    ModelRouteLease, ProviderGeneration, ProviderId, ProviderRouteClaim, ProviderRouteError,
    ProviderRouteId, ProviderRouteLedger, ProviderRouteOwner, ProviderRouteOwnership,
    ProviderRouteSnapshot, RequestId, TokenCount, TurnId, UsageContextSnapshot, UsageCounters,
    UsageSample,
};
