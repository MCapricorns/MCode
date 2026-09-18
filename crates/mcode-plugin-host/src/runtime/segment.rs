//! Cancellation-safe guest-active segment ownership.
use wasmtime::Store;
#[cfg(test)]
use wasmtime::{TypedFunc, WasmParams, WasmResults};

use super::epoch::{arm_guest_deadline, park_guest_deadline};
use super::owner::{ActiveSegment, OperationLease, StoreData};
#[cfg(test)]
use super::owner::{CorePluginInstance, OwnerIdentity};
use super::{PluginOwner, RuntimeError};

#[cfg(test)]
pub(super) struct GuestFunction<Params, Results> {
    owner: OwnerIdentity,
    function: TypedFunc<Params, Results>,
}

#[cfg(test)]
impl CorePluginInstance {
    pub(super) fn typed_function<Params, Results>(
        &self,
        owner: &mut PluginOwner,
        name: &str,
    ) -> Result<GuestFunction<Params, Results>, RuntimeError>
    where
        Params: WasmParams,
        Results: WasmResults,
    {
        if self.owner != owner.identity {
            return Err(RuntimeError::InstanceMismatch);
        }
        let store = owner.store.as_mut().ok_or(RuntimeError::StoreDisposed)?;
        let function = self
            .instance
            .get_typed_func(store, name)
            .map_err(|_| RuntimeError::Guest)?;
        Ok(GuestFunction {
            owner: self.owner.clone(),
            function,
        })
    }
}

#[cfg(test)]
impl PluginOwner {
    pub(super) async fn call_typed<Params, Results>(
        &mut self,
        lease: &mut OperationLease,
        function: &GuestFunction<Params, Results>,
        params: Params,
    ) -> Result<Results, RuntimeError>
    where
        Params: WasmParams + Sync,
        Results: WasmResults + Sync,
    {
        if lease.owner != self.identity {
            return Err(RuntimeError::OwnerMismatch);
        }
        if function.owner != self.identity {
            return Err(RuntimeError::InstanceMismatch);
        }

        let mut segment = SegmentExecution::start(self, lease)?;
        let guest_result = function
            .function
            .call_async(segment.store_mut(), params)
            .await;
        segment.complete()?;
        guest_result.map_err(|_| RuntimeError::Guest)
    }
}
pub(super) struct SegmentExecution<'a> {
    owner: &'a mut PluginOwner,
    lease: &'a mut OperationLease,
    store: Option<Store<StoreData>>,
    restore_if_incomplete: bool,
}

#[expect(
    dead_code,
    reason = "T9+ typed guest-call boundaries consume the segment pipeline"
)]
impl<'a> SegmentExecution<'a> {
    #[cfg(test)]
    pub(super) fn start(
        owner: &'a mut PluginOwner,
        lease: &'a mut OperationLease,
    ) -> Result<Self, RuntimeError> {
        Self::start_with_disposition(owner, lease, true)
    }

    pub(super) fn start_plugin_call(
        owner: &'a mut PluginOwner,
        lease: &'a mut OperationLease,
    ) -> Result<Self, RuntimeError> {
        // A cancelled plugin call may have mutated guest-owned state before
        // its fiber is synchronously cancelled. Park and account for its fuel,
        // but never return that Store to the owner.
        Self::start_with_disposition(owner, lease, false)
    }

    fn start_with_disposition(
        owner: &'a mut PluginOwner,
        lease: &'a mut OperationLease,
        restore_if_incomplete: bool,
    ) -> Result<Self, RuntimeError> {
        let mut store = owner.take_store()?;
        if store.data().active_segment.is_some() {
            return Err(RuntimeError::SegmentActive);
        }
        if store.data().limiter.is_poisoned() {
            return Err(RuntimeError::StoreDisposed);
        }
        let installed = lease.remaining;
        if store.set_fuel(installed).is_err() {
            return Err(RuntimeError::Fuel);
        }
        arm_guest_deadline(&mut store);
        store.data_mut().active_segment = Some(ActiveSegment {
            owner: owner.identity.clone(),
            operation: lease.operation,
            installed,
        });
        Ok(Self {
            owner,
            lease,
            store: Some(store),
            restore_if_incomplete,
        })
    }

    pub(super) fn store_mut(&mut self) -> &mut Store<StoreData> {
        self.store
            .as_mut()
            .expect("segment guard owns its Store until completion")
    }

    pub(super) fn complete(mut self) -> Result<(), RuntimeError> {
        self.complete_inner(true)
    }

    pub(super) fn dispose(mut self) -> Result<(), RuntimeError> {
        self.complete_inner(false)
    }

    fn complete_inner(&mut self, restore: bool) -> Result<(), RuntimeError> {
        let mut store = self
            .store
            .take()
            .expect("segment guard completes exactly once");
        let actual = store.get_fuel();
        let fuel_parked = store.set_fuel(0);
        park_guest_deadline(&mut store);
        let active = store.data_mut().active_segment.take();

        let Ok(actual) = actual else {
            return Err(RuntimeError::Fuel);
        };
        if fuel_parked.is_err() {
            return Err(RuntimeError::Fuel);
        }
        if store.data().limiter.is_poisoned() {
            return Err(RuntimeError::StoreDisposed);
        }
        let Some(active) = active else {
            return Err(RuntimeError::SegmentMismatch);
        };
        if active.owner != self.owner.identity
            || active.owner != self.lease.owner
            || active.operation != self.lease.operation
            || active.installed != self.lease.remaining
        {
            return Err(RuntimeError::SegmentMismatch);
        }
        if actual > active.installed {
            return Err(RuntimeError::FuelIncreased);
        }

        self.lease.remaining = actual;
        if restore {
            self.owner.restore_store(store);
        }
        Ok(())
    }
}

impl Drop for SegmentExecution<'_> {
    fn drop(&mut self) {
        if self.store.is_some() {
            // Dropping a pending call future reaches this synchronous fiber
            // finalizer. Generic test hooks restore a checked Store; production
            // plugin calls dispose it because guest state may be partial.
            // Any policy-invariant failure also leaves the Store disposed.
            let _ = self.complete_inner(self.restore_if_incomplete);
        }
    }
}
