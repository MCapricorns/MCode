//! Immutable provider ownership, route leases, and usage stamps.
use std::fmt::{self, Debug, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mcode_config::{ArtifactRef, PackId, PluginFamily, ProviderId, SourceBindingId};

use super::identity::{
    AuthFingerprint, AuthSlotId, EndpointFingerprint, ModelAlias, ModelId, ProviderGeneration,
    ProviderRouteId, RequestId, TurnId, UsageCounters,
};

/// Identifies one Manager and Provider Pack generation claiming routes.
///
/// Construction validates each component through canonical `mcode-config`
/// types. Registration separately enforces [`PluginFamily::Providers`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRouteOwner {
    manager_family: PluginFamily,
    manager_artifact: ArtifactRef,
    pack_id: PackId,
    pack_source: SourceBindingId,
    pack_artifact: ArtifactRef,
    generation: ProviderGeneration,
}

impl ProviderRouteOwner {
    /// Creates one exact Manager and Pack generation identity.
    #[must_use]
    pub const fn new(
        manager_family: PluginFamily,
        manager_artifact: ArtifactRef,
        pack_id: PackId,
        pack_source: SourceBindingId,
        pack_artifact: ArtifactRef,
        generation: ProviderGeneration,
    ) -> Self {
        Self {
            manager_family,
            manager_artifact,
            pack_id,
            pack_source,
            pack_artifact,
            generation,
        }
    }

    /// Returns the canonical Manager family.
    #[must_use]
    pub const fn manager_family(&self) -> PluginFamily {
        self.manager_family
    }

    /// Returns the canonical Manager ID derived from its family.
    #[must_use]
    pub const fn manager_id(&self) -> &'static str {
        self.manager_family.id()
    }

    /// Returns the exact Manager version and hash.
    #[must_use]
    pub const fn manager_artifact(&self) -> &ArtifactRef {
        &self.manager_artifact
    }

    /// Returns the exact Provider Pack ID.
    #[must_use]
    pub const fn pack_id(&self) -> &PackId {
        &self.pack_id
    }

    /// Returns the exact Provider Pack source binding.
    #[must_use]
    pub const fn pack_source(&self) -> &SourceBindingId {
        &self.pack_source
    }

    /// Returns the exact Provider Pack version and hash.
    #[must_use]
    pub const fn pack_artifact(&self) -> &ArtifactRef {
        &self.pack_artifact
    }

    /// Returns the exact Provider Pack generation.
    #[must_use]
    pub const fn generation(&self) -> ProviderGeneration {
        self.generation
    }

    pub(super) fn matches_except_generation(&self, other: &Self) -> bool {
        self.manager_family == other.manager_family
            && self.manager_artifact == other.manager_artifact
            && self.pack_id == other.pack_id
            && self.pack_source == other.pack_source
            && self.pack_artifact == other.pack_artifact
    }
}

/// Declares one provider, route, and authentication-slot ownership claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRouteClaim {
    provider_id: ProviderId,
    route_id: ProviderRouteId,
    auth_slot_id: AuthSlotId,
    endpoint_fingerprint: EndpointFingerprint,
    auth_fingerprint: AuthFingerprint,
}

impl ProviderRouteClaim {
    /// Creates one complete bounded route claim.
    #[must_use]
    pub const fn new(
        provider_id: ProviderId,
        route_id: ProviderRouteId,
        auth_slot_id: AuthSlotId,
        endpoint_fingerprint: EndpointFingerprint,
        auth_fingerprint: AuthFingerprint,
    ) -> Self {
        Self {
            provider_id,
            route_id,
            auth_slot_id,
            endpoint_fingerprint,
            auth_fingerprint,
        }
    }

    /// Returns the globally claimed provider ID.
    #[must_use]
    pub const fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    /// Returns the globally claimed route ID.
    #[must_use]
    pub const fn route_id(&self) -> &ProviderRouteId {
        &self.route_id
    }

    /// Returns the globally claimed authentication-slot ID.
    #[must_use]
    pub const fn auth_slot_id(&self) -> &AuthSlotId {
        &self.auth_slot_id
    }

    /// Returns the exact signed endpoint fingerprint.
    #[must_use]
    pub const fn endpoint_fingerprint(&self) -> &EndpointFingerprint {
        &self.endpoint_fingerprint
    }

    /// Returns the exact signed authentication fingerprint.
    #[must_use]
    pub const fn auth_fingerprint(&self) -> &AuthFingerprint {
        &self.auth_fingerprint
    }
}

/// Contains one immutable registered route ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRouteOwnership {
    pub(super) owner: ProviderRouteOwner,
    pub(super) claim: ProviderRouteClaim,
}

impl ProviderRouteOwnership {
    /// Returns the registered Manager and Pack generation.
    #[must_use]
    pub const fn owner(&self) -> &ProviderRouteOwner {
        &self.owner
    }

    /// Returns the registered global route claim.
    #[must_use]
    pub const fn claim(&self) -> &ProviderRouteClaim {
        &self.claim
    }
}

#[derive(Debug)]
pub(super) struct LedgerSeal;

/// Proves one current model was selected through a registered host route.
///
/// Fields are private, and only [`crate::ProviderRouteLedger::mint_model_route_lease`]
/// creates this type.
///
/// ```compile_fail
/// use mcode_plugin_host::ModelRouteLease;
/// let _forged = ModelRouteLease {};
/// ```
///
/// ```compile_fail
/// use mcode_plugin_host::ModelRouteLease;
/// fn require_guest_decode<T: serde::de::DeserializeOwned>() {}
/// require_guest_decode::<ModelRouteLease>();
/// ```
#[derive(Clone)]
pub struct ModelRouteLease {
    seal: Arc<LedgerSeal>,
    ownership: ProviderRouteOwnership,
    current_model: ModelId,
}

impl ModelRouteLease {
    pub(super) fn new(
        seal: Arc<LedgerSeal>,
        ownership: ProviderRouteOwnership,
        current_model: ModelId,
    ) -> Self {
        Self {
            seal,
            ownership,
            current_model,
        }
    }

    /// Returns the immutable registered route ownership.
    #[must_use]
    pub const fn ownership(&self) -> &ProviderRouteOwnership {
        &self.ownership
    }

    /// Returns the exact current model selected for the route.
    #[must_use]
    pub const fn current_model(&self) -> &ModelId {
        &self.current_model
    }

    pub(super) fn belongs_to(&self, seal: &Arc<LedgerSeal>) -> bool {
        Arc::ptr_eq(&self.seal, seal)
    }

    fn same_stamp(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.seal, &other.seal)
            && self.ownership == other.ownership
            && self.current_model == other.current_model
    }
}

impl Debug for ModelRouteLease {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelRouteLease")
            .field("ownership", &self.ownership)
            .field("current_model", &self.current_model)
            .finish_non_exhaustive()
    }
}

struct UsageStamp {
    ledger: Arc<LedgerSeal>,
    terminal: AtomicBool,
}

impl UsageStamp {
    fn new(ledger: Arc<LedgerSeal>) -> Self {
        Self {
            ledger,
            terminal: AtomicBool::new(false),
        }
    }

    fn belongs_to(&self, ledger: &Arc<LedgerSeal>) -> bool {
        Arc::ptr_eq(&self.ledger, ledger)
    }

    fn is_terminal(&self) -> bool {
        self.terminal.load(Ordering::Acquire)
    }

    fn mark_terminal(&self) {
        assert!(
            !self.terminal.swap(true, Ordering::AcqRel),
            "a terminal usage stamp cannot transition"
        );
    }
}

/// Captures immutable request and model identity at one usage boundary.
///
/// Recording a resolved model returns a new snapshot rather than modifying the
/// prior value. Clones share one private request-instance state so terminal
/// replay remains rejected after the request ID is reused.
///
/// ```compile_fail
/// use mcode_plugin_host::UsageContextSnapshot;
/// let _forged = UsageContextSnapshot {};
/// ```
///
/// ```compile_fail
/// use mcode_plugin_host::UsageContextSnapshot;
/// fn require_guest_decode<T: serde::de::DeserializeOwned>() {}
/// require_guest_decode::<UsageContextSnapshot>();
/// ```
#[derive(Clone)]
pub struct UsageContextSnapshot {
    lease: ModelRouteLease,
    request_id: RequestId,
    turn_id: TurnId,
    requested_model: ModelId,
    requested_alias: Option<ModelAlias>,
    resolved_model: Option<ModelId>,
    stamp: Arc<UsageStamp>,
}

impl UsageContextSnapshot {
    pub(super) fn new(
        ledger: Arc<LedgerSeal>,
        lease: ModelRouteLease,
        request_id: RequestId,
        turn_id: TurnId,
        requested_model: ModelId,
        requested_alias: Option<ModelAlias>,
    ) -> Self {
        Self {
            lease,
            request_id,
            turn_id,
            requested_model,
            requested_alias,
            resolved_model: None,
            stamp: Arc::new(UsageStamp::new(ledger)),
        }
    }

    /// Returns the registered route lease.
    #[must_use]
    pub const fn lease(&self) -> &ModelRouteLease {
        &self.lease
    }

    /// Returns the exact request ID.
    #[must_use]
    pub const fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    /// Returns the exact turn ID.
    #[must_use]
    pub const fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    /// Returns the exact requested model.
    #[must_use]
    pub const fn requested_model(&self) -> &ModelId {
        &self.requested_model
    }

    /// Returns the optional requested alias without deriving one.
    #[must_use]
    pub const fn requested_alias(&self) -> Option<&ModelAlias> {
        self.requested_alias.as_ref()
    }

    /// Returns the optional resolved model without deriving one.
    #[must_use]
    pub const fn resolved_model(&self) -> Option<&ModelId> {
        self.resolved_model.as_ref()
    }

    pub(super) fn belongs_to(&self, ledger: &Arc<LedgerSeal>) -> bool {
        self.lease.belongs_to(ledger) && self.stamp.belongs_to(ledger)
    }

    pub(super) fn is_terminal(&self) -> bool {
        self.stamp.is_terminal()
    }

    pub(super) fn mark_terminal(&self) {
        self.stamp.mark_terminal();
    }

    pub(super) fn same_request_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.stamp, &other.stamp)
    }

    pub(super) fn with_resolved_model(&self, resolved_model: ModelId) -> Self {
        Self {
            lease: self.lease.clone(),
            request_id: self.request_id.clone(),
            turn_id: self.turn_id.clone(),
            requested_model: self.requested_model.clone(),
            requested_alias: self.requested_alias.clone(),
            resolved_model: Some(resolved_model),
            stamp: self.stamp.clone(),
        }
    }

    pub(super) fn same_snapshot(&self, other: &Self) -> bool {
        self.same_request_instance(other)
            && self.lease.same_stamp(&other.lease)
            && self.request_id == other.request_id
            && self.turn_id == other.turn_id
            && self.requested_model == other.requested_model
            && self.requested_alias == other.requested_alias
            && self.resolved_model == other.resolved_model
    }
}

impl Debug for UsageContextSnapshot {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UsageContextSnapshot")
            .field("lease", &self.lease)
            .field("request_id", &self.request_id)
            .field("turn_id", &self.turn_id)
            .field("requested_model", &self.requested_model)
            .field("requested_alias", &self.requested_alias)
            .field("resolved_model", &self.resolved_model)
            .finish_non_exhaustive()
    }
}

/// Contains one immutable, exactly-once terminal usage stamp.
///
/// Fields are private, and only [`crate::ProviderRouteLedger::mint_terminal_sample`]
/// creates this type. The host-only stamps deliberately implement no serde
/// construction contract.
///
/// ```compile_fail
/// use mcode_plugin_host::UsageSample;
/// let _forged = UsageSample {};
/// ```
///
/// ```compile_fail
/// use mcode_plugin_host::UsageSample;
/// fn require_guest_decode<T: serde::de::DeserializeOwned>() {}
/// require_guest_decode::<UsageSample>();
/// ```
#[derive(Debug)]
pub struct UsageSample {
    context: UsageContextSnapshot,
    counters: UsageCounters,
}

impl UsageSample {
    pub(super) const fn new(context: UsageContextSnapshot, counters: UsageCounters) -> Self {
        Self { context, counters }
    }

    /// Returns the exact immutable route and request context.
    #[must_use]
    pub const fn context(&self) -> &UsageContextSnapshot {
        &self.context
    }

    /// Returns the exact optional terminal counters.
    #[must_use]
    pub const fn counters(&self) -> &UsageCounters {
        &self.counters
    }
}
