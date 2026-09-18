//! Host-only provider-route ownership and immutable usage stamps.
//!
//! The ledger accepts only the canonical Providers family, publishes route
//! claims atomically, and mints capability-bound stamps after validating the
//! registered owner, route, request, turn, and terminal state. It contains no
//! guest codec, transport, credential, URL, header, socket, or raw handle.
mod identity;
mod ledger;
mod stamps;

pub use identity::{
    AuthFingerprint, AuthSlotId, EndpointFingerprint, ModelAlias, ModelId, ProviderGeneration,
    ProviderRouteId, RequestId, TokenCount, TurnId, UsageCounters,
};
pub use ledger::{ProviderRouteLedger, ProviderRouteSnapshot};
pub use mcode_config::ProviderId;
pub use stamps::{
    ModelRouteLease, ProviderRouteClaim, ProviderRouteOwner, ProviderRouteOwnership,
    UsageContextSnapshot, UsageSample,
};

/// Maximum claims accepted by one atomic owner registration.
pub const MAX_PROVIDER_ROUTE_CLAIMS_PER_BATCH: usize = 256;
/// Maximum claims retained by one provider-route ledger.
pub const MAX_PROVIDER_ROUTE_CLAIMS: usize = 4_096;
/// Maximum concurrent live usage contexts retained by one ledger.
pub const MAX_USAGE_CONTEXTS_PER_LEDGER: usize = 4_096;

/// Reports provider-route validation or state rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProviderRouteError {
    /// A route ID violated its frozen grammar or bound.
    #[error("provider route ID is invalid")]
    InvalidRouteId,
    /// An authentication-slot ID violated its frozen grammar or bound.
    #[error("provider authentication-slot ID is invalid")]
    InvalidAuthSlotId,
    /// A model ID violated its frozen grammar or bound.
    #[error("provider model ID is invalid")]
    InvalidModelId,
    /// A model alias violated its frozen grammar or bound.
    #[error("provider model alias is invalid")]
    InvalidModelAlias,
    /// A request ID violated its frozen grammar or bound.
    #[error("provider request ID is invalid")]
    InvalidRequestId,
    /// A turn ID violated its frozen grammar or bound.
    #[error("provider turn ID is invalid")]
    InvalidTurnId,
    /// A provider generation was zero or exceeded its bound.
    #[error("provider generation is invalid")]
    InvalidGeneration,
    /// A token count exceeded its bound.
    #[error("provider token count is invalid")]
    InvalidTokenCount,
    /// A non-Providers family attempted to own provider routes.
    #[error("only the canonical Providers family can own provider routes")]
    WrongManagerFamily,
    /// An owner registration contained no claims.
    #[error("provider route claim batch is empty")]
    EmptyClaimBatch,
    /// An owner registration exceeded its claim bound.
    #[error("provider route claim batch exceeds its limit")]
    ClaimBatchTooLarge,
    /// The ledger reached its route-claim capacity.
    #[error("provider route ledger is full")]
    RouteCapacityExceeded,
    /// One owner batch repeated a provider ID.
    #[error("provider route claim batch repeats a provider ID")]
    DuplicateProviderId,
    /// One owner batch repeated a route ID.
    #[error("provider route claim batch repeats a route ID")]
    DuplicateRouteId,
    /// One owner batch repeated an authentication-slot ID.
    #[error("provider route claim batch repeats an authentication-slot ID")]
    DuplicateAuthSlotId,
    /// A provider ID was already globally claimed.
    #[error("provider ID is already claimed")]
    ProviderIdCollision,
    /// A route ID was already globally claimed.
    #[error("provider route ID is already claimed")]
    RouteIdCollision,
    /// An authentication-slot ID was already globally claimed.
    #[error("provider authentication-slot ID is already claimed")]
    AuthSlotIdCollision,
    /// The requested route is not registered.
    #[error("provider route is not registered")]
    UnknownRoute,
    /// The expected owner does not own the stamped route.
    #[error("provider route owner does not match")]
    OwnerMismatch,
    /// The expected provider generation differs from the registered owner.
    #[error("provider route generation is stale")]
    StaleGeneration,
    /// A stamp came from a different ledger instance.
    #[error("provider route stamp belongs to another ledger")]
    ForeignStamp,
    /// The ledger reached its concurrent live-context capacity.
    #[error("provider usage context ledger is full")]
    UsageContextCapacityExceeded,
    /// The request ID already has a live usage context.
    #[error("provider request already has a usage context")]
    RequestAlreadyRegistered,
    /// The supplied request ID differs from the stamped request.
    #[error("provider request does not match its usage context")]
    RequestMismatch,
    /// The supplied turn ID differs from the stamped turn.
    #[error("provider turn does not match its usage context")]
    TurnMismatch,
    /// A newer immutable usage context exists for the request.
    #[error("provider usage context is stale")]
    StaleUsageContext,
    /// A resolved model was already stamped for the request.
    #[error("provider resolved model is already recorded")]
    ResolvedModelAlreadyRecorded,
    /// A terminal sample was already minted for the request instance.
    #[error("provider request is already terminal")]
    RequestAlreadyTerminal,
}
