//! Atomic provider-route registration and live usage-context transitions.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Debug, Formatter};
use std::sync::{Arc, Mutex, MutexGuard};

use mcode_config::{PluginFamily, ProviderId};

use super::identity::{
    AuthSlotId, ModelAlias, ModelId, ProviderRouteId, RequestId, TurnId, UsageCounters,
};
use super::stamps::{
    LedgerSeal, ModelRouteLease, ProviderRouteClaim, ProviderRouteOwner, ProviderRouteOwnership,
    UsageContextSnapshot, UsageSample,
};
use super::{
    MAX_PROVIDER_ROUTE_CLAIMS, MAX_PROVIDER_ROUTE_CLAIMS_PER_BATCH, MAX_USAGE_CONTEXTS_PER_LEDGER,
    ProviderRouteError,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RouteTable {
    revision: u64,
    routes: BTreeMap<ProviderRouteId, ProviderRouteOwnership>,
    providers: BTreeMap<ProviderId, ProviderRouteId>,
    auth_slots: BTreeMap<AuthSlotId, ProviderRouteId>,
}

/// Provides an immutable view of all accepted route claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRouteSnapshot {
    inner: Arc<RouteTable>,
}

impl ProviderRouteSnapshot {
    /// Returns the monotonic registration revision.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.inner.revision
    }

    /// Returns the number of registered routes.
    #[must_use]
    pub fn route_count(&self) -> usize {
        self.inner.routes.len()
    }

    /// Reports whether no route is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.routes.is_empty()
    }

    /// Returns ownership by exact route ID.
    #[must_use]
    pub fn route(&self, route_id: &ProviderRouteId) -> Option<&ProviderRouteOwnership> {
        self.inner.routes.get(route_id)
    }

    /// Returns ownership by exact provider ID.
    #[must_use]
    pub fn provider(&self, provider_id: &ProviderId) -> Option<&ProviderRouteOwnership> {
        self.inner
            .providers
            .get(provider_id)
            .and_then(|route_id| self.inner.routes.get(route_id))
    }

    /// Returns ownership by exact authentication-slot ID.
    #[must_use]
    pub fn auth_slot(&self, auth_slot_id: &AuthSlotId) -> Option<&ProviderRouteOwnership> {
        self.inner
            .auth_slots
            .get(auth_slot_id)
            .and_then(|route_id| self.inner.routes.get(route_id))
    }
}

#[derive(Debug)]
struct LedgerState {
    table: Arc<RouteTable>,
    requests: BTreeMap<RequestId, UsageContextSnapshot>,
}

/// Atomically owns provider routes and mints immutable usage stamps.
///
/// Clones share one synchronized host ledger. Only live request contexts count
/// toward [`MAX_USAGE_CONTEXTS_PER_LEDGER`]; a terminal transition removes its
/// live record while the private shared stamp keeps old clones terminal.
///
/// # Panics
///
/// The [`Debug`] implementation fails closed by panicking if the shared ledger
/// state mutex is poisoned.
#[derive(Clone)]
pub struct ProviderRouteLedger {
    seal: Arc<LedgerSeal>,
    state: Arc<Mutex<LedgerState>>,
}

impl ProviderRouteLedger {
    /// Creates an empty host ledger.
    #[must_use]
    pub fn new() -> Self {
        Self {
            seal: Arc::new(LedgerSeal),
            state: Arc::new(Mutex::new(LedgerState {
                table: Arc::new(RouteTable::default()),
                requests: BTreeMap::new(),
            })),
        }
    }

    /// Returns a cheap immutable route snapshot.
    ///
    /// # Panics
    ///
    /// Panics if the shared ledger state mutex is poisoned.
    #[must_use]
    pub fn snapshot(&self) -> ProviderRouteSnapshot {
        ProviderRouteSnapshot {
            inner: self.lock().table.clone(),
        }
    }

    /// Registers one owner's claims as a single atomic mutation.
    ///
    /// All same-batch duplicates and global provider, route, and
    /// authentication-slot collisions are checked before the published route
    /// table changes.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderRouteError`] for a non-Providers owner, empty or
    /// oversized batch, duplicate or colliding claim, or full ledger. Every
    /// error leaves the published snapshot unchanged.
    ///
    /// # Panics
    ///
    /// Panics if the shared ledger state mutex is poisoned.
    pub fn register_owner_claims(
        &self,
        owner: ProviderRouteOwner,
        claims: impl IntoIterator<Item = ProviderRouteClaim>,
    ) -> Result<ProviderRouteSnapshot, ProviderRouteError> {
        if owner.manager_family() != PluginFamily::Providers {
            return Err(ProviderRouteError::WrongManagerFamily);
        }
        let claims = collect_claims(claims)?;
        let mut state = self.lock();
        validate_registration(&state.table, &claims)?;

        let mut candidate = state.table.as_ref().clone();
        candidate.revision = candidate
            .revision
            .checked_add(1)
            .expect("bounded append-only route table revision cannot overflow");
        for claim in claims {
            let provider_id = claim.provider_id().clone();
            let route_id = claim.route_id().clone();
            let auth_slot_id = claim.auth_slot_id().clone();
            candidate.providers.insert(provider_id, route_id.clone());
            candidate.auth_slots.insert(auth_slot_id, route_id.clone());
            candidate.routes.insert(
                route_id,
                ProviderRouteOwnership {
                    owner: owner.clone(),
                    claim,
                },
            );
        }
        let published = Arc::new(candidate);
        state.table = published.clone();
        Ok(ProviderRouteSnapshot { inner: published })
    }

    /// Mints a current-model lease from one registered route.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderRouteError::UnknownRoute`] when `route_id` is absent,
    /// [`ProviderRouteError::StaleGeneration`] for an expected generation
    /// mismatch, or [`ProviderRouteError::OwnerMismatch`] for another mismatch.
    ///
    /// # Panics
    ///
    /// Panics if the shared ledger state mutex is poisoned.
    pub fn mint_model_route_lease(
        &self,
        owner: &ProviderRouteOwner,
        route_id: &ProviderRouteId,
        current_model: ModelId,
    ) -> Result<ModelRouteLease, ProviderRouteError> {
        let state = self.lock();
        let ownership = state
            .table
            .routes
            .get(route_id)
            .ok_or(ProviderRouteError::UnknownRoute)?;
        validate_expected_owner(ownership.owner(), owner)?;
        Ok(ModelRouteLease::new(
            self.seal.clone(),
            ownership.clone(),
            current_model,
        ))
    }

    /// Mints an initial immutable context from one valid route lease.
    ///
    /// The initial resolved model is always absent. A later validated response
    /// may create a replacement snapshot through [`Self::record_resolved_model`].
    /// Request IDs become reusable after their current instance is terminal.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderRouteError`] for a foreign lease, wrong owner or
    /// generation, duplicate live request ID, or full live-context ledger.
    ///
    /// # Panics
    ///
    /// Panics if the shared ledger state mutex is poisoned.
    pub fn mint_usage_context(
        &self,
        owner: &ProviderRouteOwner,
        lease: &ModelRouteLease,
        request_id: RequestId,
        turn_id: TurnId,
        requested_model: ModelId,
        requested_alias: Option<ModelAlias>,
    ) -> Result<UsageContextSnapshot, ProviderRouteError> {
        let mut state = self.lock();
        self.validate_lease(owner, lease)?;
        if state.requests.contains_key(&request_id) {
            return Err(ProviderRouteError::RequestAlreadyRegistered);
        }
        if state.requests.len() >= MAX_USAGE_CONTEXTS_PER_LEDGER {
            return Err(ProviderRouteError::UsageContextCapacityExceeded);
        }
        let context = UsageContextSnapshot::new(
            self.seal.clone(),
            lease.clone(),
            request_id.clone(),
            turn_id,
            requested_model,
            requested_alias,
        );
        state.requests.insert(request_id, context.clone());
        Ok(context)
    }

    /// Records one exact resolved model as a new immutable context snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderRouteError`] for a foreign, stale, mismatched,
    /// already-resolved, or terminal context. Errors leave the live request
    /// record unchanged.
    ///
    /// # Panics
    ///
    /// Panics if the shared ledger state mutex is poisoned.
    pub fn record_resolved_model(
        &self,
        context: &UsageContextSnapshot,
        request_id: &RequestId,
        turn_id: &TurnId,
        resolved_model: ModelId,
    ) -> Result<UsageContextSnapshot, ProviderRouteError> {
        validate_request_and_turn(context, request_id, turn_id)?;
        self.validate_context(context)?;

        let mut state = self.lock();
        if context.is_terminal() {
            return Err(ProviderRouteError::RequestAlreadyTerminal);
        }
        let live_context = state
            .requests
            .get_mut(request_id)
            .ok_or(ProviderRouteError::StaleUsageContext)?;
        if !live_context.same_snapshot(context) {
            return Err(ProviderRouteError::StaleUsageContext);
        }
        if live_context.resolved_model().is_some() {
            return Err(ProviderRouteError::ResolvedModelAlreadyRecorded);
        }
        let updated = context.with_resolved_model(resolved_model);
        *live_context = updated.clone();
        Ok(updated)
    }

    /// Mints one exactly-once terminal usage sample.
    ///
    /// Counters and an absent resolved model are copied exactly without
    /// inference. The terminal state and live-record removal are one mutation
    /// under the ledger lock, and concurrent attempts linearize around it.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderRouteError`] for a foreign, stale, mismatched, or
    /// already-terminal context. Errors do not consume terminal eligibility.
    ///
    /// # Panics
    ///
    /// Panics if the shared ledger state mutex is poisoned.
    pub fn mint_terminal_sample(
        &self,
        context: &UsageContextSnapshot,
        request_id: &RequestId,
        turn_id: &TurnId,
        counters: UsageCounters,
    ) -> Result<UsageSample, ProviderRouteError> {
        validate_request_and_turn(context, request_id, turn_id)?;
        self.validate_context(context)?;

        let mut state = self.lock();
        if context.is_terminal() {
            return Err(ProviderRouteError::RequestAlreadyTerminal);
        }
        let live_context = state
            .requests
            .get(request_id)
            .ok_or(ProviderRouteError::StaleUsageContext)?;
        if !live_context.same_snapshot(context) {
            return Err(ProviderRouteError::StaleUsageContext);
        }

        let sample = UsageSample::new(context.clone(), counters);
        context.mark_terminal();
        let removed = state
            .requests
            .remove(request_id)
            .expect("validated live usage context must remain under the ledger lock");
        assert!(
            removed.same_request_instance(context),
            "removed usage context must match the terminal request instance"
        );
        Ok(sample)
    }

    fn validate_lease(
        &self,
        expected_owner: &ProviderRouteOwner,
        lease: &ModelRouteLease,
    ) -> Result<(), ProviderRouteError> {
        if !lease.belongs_to(&self.seal) {
            return Err(ProviderRouteError::ForeignStamp);
        }
        validate_expected_owner(lease.ownership().owner(), expected_owner)
    }

    fn validate_context(&self, context: &UsageContextSnapshot) -> Result<(), ProviderRouteError> {
        if !context.belongs_to(&self.seal) {
            return Err(ProviderRouteError::ForeignStamp);
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, LedgerState> {
        self.state
            .lock()
            .expect("provider route ledger state must not be poisoned")
    }
}

impl Default for ProviderRouteLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl Debug for ProviderRouteLedger {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let (revision, route_count, request_count) = {
            let state = self.lock();
            (
                state.table.revision,
                state.table.routes.len(),
                state.requests.len(),
            )
        };
        formatter
            .debug_struct("ProviderRouteLedger")
            .field("revision", &revision)
            .field("route_count", &route_count)
            .field("request_count", &request_count)
            .finish_non_exhaustive()
    }
}

fn collect_claims(
    claims: impl IntoIterator<Item = ProviderRouteClaim>,
) -> Result<Vec<ProviderRouteClaim>, ProviderRouteError> {
    let mut collected = Vec::new();
    for claim in claims {
        if collected.len() == MAX_PROVIDER_ROUTE_CLAIMS_PER_BATCH {
            return Err(ProviderRouteError::ClaimBatchTooLarge);
        }
        collected.push(claim);
    }
    if collected.is_empty() {
        return Err(ProviderRouteError::EmptyClaimBatch);
    }
    Ok(collected)
}

fn validate_registration(
    table: &RouteTable,
    claims: &[ProviderRouteClaim],
) -> Result<(), ProviderRouteError> {
    let mut providers = BTreeSet::new();
    let mut routes = BTreeSet::new();
    let mut auth_slots = BTreeSet::new();
    for claim in claims {
        if !providers.insert(claim.provider_id()) {
            return Err(ProviderRouteError::DuplicateProviderId);
        }
        if !routes.insert(claim.route_id()) {
            return Err(ProviderRouteError::DuplicateRouteId);
        }
        if !auth_slots.insert(claim.auth_slot_id()) {
            return Err(ProviderRouteError::DuplicateAuthSlotId);
        }
    }
    if claims
        .iter()
        .any(|claim| table.providers.contains_key(claim.provider_id()))
    {
        return Err(ProviderRouteError::ProviderIdCollision);
    }
    if claims
        .iter()
        .any(|claim| table.routes.contains_key(claim.route_id()))
    {
        return Err(ProviderRouteError::RouteIdCollision);
    }
    if claims
        .iter()
        .any(|claim| table.auth_slots.contains_key(claim.auth_slot_id()))
    {
        return Err(ProviderRouteError::AuthSlotIdCollision);
    }
    if table
        .routes
        .len()
        .checked_add(claims.len())
        .is_none_or(|count| count > MAX_PROVIDER_ROUTE_CLAIMS)
    {
        return Err(ProviderRouteError::RouteCapacityExceeded);
    }
    Ok(())
}

fn validate_expected_owner(
    stamped: &ProviderRouteOwner,
    expected: &ProviderRouteOwner,
) -> Result<(), ProviderRouteError> {
    if stamped == expected {
        return Ok(());
    }
    if stamped.matches_except_generation(expected) {
        return Err(ProviderRouteError::StaleGeneration);
    }
    Err(ProviderRouteError::OwnerMismatch)
}

fn validate_request_and_turn(
    context: &UsageContextSnapshot,
    request_id: &RequestId,
    turn_id: &TurnId,
) -> Result<(), ProviderRouteError> {
    if context.request_id() != request_id {
        return Err(ProviderRouteError::RequestMismatch);
    }
    if context.turn_id() != turn_id {
        return Err(ProviderRouteError::TurnMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::*;

    struct PanicWriter;

    impl Write for PanicWriter {
        fn write_str(&mut self, _value: &str) -> fmt::Result {
            panic!("formatter panic")
        }
    }

    #[test]
    fn debug_formatter_panic_does_not_poison_ledger() {
        let ledger = ProviderRouteLedger::new();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut writer = PanicWriter;
            let _ = std::fmt::write(&mut writer, format_args!("{ledger:?}"));
        }));

        assert!(result.is_err());
        assert!(!ledger.state.is_poisoned());
        assert!(ledger.snapshot().is_empty());
    }

    #[test]
    fn genuine_ledger_poison_fails_closed() {
        let ledger = ProviderRouteLedger::new();
        let poisoned = ledger.clone();
        let result = catch_unwind(AssertUnwindSafe(move || {
            let _state = poisoned.state.lock().expect("initial ledger lock");
            panic!("poison ledger state")
        }));

        assert!(result.is_err());
        assert!(ledger.state.is_poisoned());
        assert!(catch_unwind(AssertUnwindSafe(|| ledger.snapshot())).is_err());
        assert!(catch_unwind(AssertUnwindSafe(|| format!("{ledger:?}"))).is_err());
    }
}
