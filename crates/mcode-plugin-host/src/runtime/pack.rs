//! Typed instantiation for the current external Pack worlds.
use wasmtime::component::{HasSelf, Linker, Resource};

use crate::ComponentWorld;
use crate::pack_wit;
use crate::pack_wit::mcp::mcode::feature_pack::mcp_host;
use crate::pack_wit::usage::mcode::feature_pack::usage_host;
use crate::pack_wit::web::mcode::feature_pack::web_host;
use crate::provider_wit;

use super::owner::{CompiledPackComponent, InstantiationExecution, OwnerIdentity, StoreData};
use super::{PluginOwner, RuntimeError};

macro_rules! instantiate_with {
    ($owner:expr, $component:expr, $linker:expr, $bindings:path, $variant:path) => {{
        let mut execution = InstantiationExecution::start($owner)?;
        let result =
            <$bindings>::instantiate_async(execution.store_mut(), $component.component(), &$linker)
                .await;
        match result {
            Ok(bindings) => {
                execution.complete()?;
                Ok($variant(bindings))
            }
            Err(_) => Err(RuntimeError::Instantiation),
        }
    }};
}

macro_rules! instantiate_importing {
    ($owner:expr, $component:expr, $bindings:path, $variant:path) => {{
        let mut linker = Linker::new($owner.runtime.engine()?);
        <$bindings>::add_to_linker::<_, HasSelf<_>>(&mut linker, |data| data)
            .map_err(|_| RuntimeError::Instantiation)?;
        instantiate_with!($owner, $component, linker, $bindings, $variant)
    }};
}

macro_rules! instantiate_zero_import {
    ($owner:expr, $component:expr, $bindings:path, $variant:path) => {{
        let linker = Linker::new($owner.runtime.engine()?);
        instantiate_with!($owner, $component, linker, $bindings, $variant)
    }};
}

/// Holds one typed Pack instance bound to its exclusive Store owner.
#[expect(
    dead_code,
    reason = "T9+ typed guest-call boundaries consume Pack instances"
)]
pub(crate) struct PackInstance {
    owner: OwnerIdentity,
    bindings: PackBindings,
}

impl PackInstance {
    /// Returns the exact typed world instantiated in this Store.
    #[cfg(test)]
    pub(crate) const fn world(&self) -> ComponentWorld {
        self.bindings.world()
    }
}

#[expect(
    dead_code,
    reason = "typed bindings must remain alive for the later guest-call boundary"
)]
enum PackBindings {
    Web(pack_wit::web::Web),
    Mcp(pack_wit::mcp::Mcp),
    Usage(pack_wit::usage::Usage),
    Provider(provider_wit::Provider),
}

impl PackBindings {
    #[cfg(test)]
    const fn world(&self) -> ComponentWorld {
        match self {
            Self::Web(_) => ComponentWorld::Web,
            Self::Mcp(_) => ComponentWorld::Mcp,
            Self::Usage(_) => ComponentWorld::Usage,
            Self::Provider(_) => ComponentWorld::Provider,
        }
    }
}

impl PluginOwner {
    /// Instantiates one typed FeaturePack or ProviderPack component.
    ///
    /// The compiled component supplies its sealed world and runtime identity.
    /// Any failed or cancelled Wasmtime instantiation disposes this owner's Store.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::RuntimeMismatch`] for an artifact from another
    /// runtime, [`RuntimeError::InstanceActive`] after this Store has already
    /// instantiated a component, or a Store, fuel, linker, or instantiation
    /// error from the fail-closed runtime boundary.
    pub(crate) async fn instantiate_pack(
        &mut self,
        component: &CompiledPackComponent,
    ) -> Result<PackInstance, RuntimeError> {
        if !std::sync::Arc::ptr_eq(&self.runtime, component.runtime()) {
            return Err(RuntimeError::RuntimeMismatch);
        }
        if self.store.is_none() {
            return Err(RuntimeError::StoreDisposed);
        }
        if self.instance_instantiated {
            return Err(RuntimeError::InstanceActive);
        }

        let owner = self.identity.clone();
        let bindings = match component.world() {
            ComponentWorld::Web => {
                instantiate_importing!(self, component, pack_wit::web::Web, PackBindings::Web)
            }
            ComponentWorld::Mcp => {
                instantiate_importing!(self, component, pack_wit::mcp::Mcp, PackBindings::Mcp)
            }
            ComponentWorld::Usage => {
                instantiate_importing!(self, component, pack_wit::usage::Usage, PackBindings::Usage)
            }
            ComponentWorld::Provider => instantiate_zero_import!(
                self,
                component,
                provider_wit::Provider,
                PackBindings::Provider
            ),
        }?;
        self.instance_instantiated = true;
        Ok(PackInstance { owner, bindings })
    }
}

impl web_host::HostWebExchange for StoreData {
    fn drop(&mut self, _rep: Resource<web_host::WebExchange>) -> wasmtime::Result<()> {
        Ok(())
    }

    fn pull(&mut self, _self_: Resource<web_host::WebExchange>) -> web_host::WebExchangePull {
        web_host::WebExchangePull::Pending
    }
}

impl web_host::Host for StoreData {
    fn start_search(
        &mut self,
        _request: web_host::TypedSearch,
    ) -> Result<Resource<web_host::WebExchange>, web_host::WebHostError> {
        Err(web_host::WebHostError::RemoteUnavailable)
    }

    fn start_fetch(
        &mut self,
        _request: web_host::TypedFetch,
    ) -> Result<Resource<web_host::WebExchange>, web_host::WebHostError> {
        Err(web_host::WebHostError::RemoteUnavailable)
    }
}

impl mcp_host::HostMcpExchange for StoreData {
    fn drop(&mut self, _rep: Resource<mcp_host::McpExchange>) -> wasmtime::Result<()> {
        Ok(())
    }

    fn pull(&mut self, _self_: Resource<mcp_host::McpExchange>) -> mcp_host::McpExchangePull {
        mcp_host::McpExchangePull::Pending
    }
}

impl mcp_host::Host for StoreData {
    fn start_invoke(
        &mut self,
        _request: mcp_host::TypedInvocation,
    ) -> Result<Resource<mcp_host::McpExchange>, mcp_host::McpHostError> {
        Err(mcp_host::McpHostError::TransportUnavailable)
    }
}

impl usage_host::HostUsageExchange for StoreData {
    fn drop(&mut self, _rep: Resource<usage_host::UsageExchange>) -> wasmtime::Result<()> {
        Ok(())
    }

    fn pull(
        &mut self,
        _self_: Resource<usage_host::UsageExchange>,
    ) -> usage_host::UsageExchangePull {
        usage_host::UsageExchangePull::Pending
    }
}

impl usage_host::Host for StoreData {
    fn start_refresh(
        &mut self,
    ) -> Result<Resource<usage_host::UsageExchange>, usage_host::UsageHostError> {
        Err(usage_host::UsageHostError::SourceUnavailable)
    }
}
