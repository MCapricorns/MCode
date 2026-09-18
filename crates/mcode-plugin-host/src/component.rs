//! Scanner-first binary preflight for all sole-current component worlds.
use std::sync::OnceLock;

use wasmtime::component::Component;
use wasmtime::{Config, Engine};

use crate::component_scanner::{ScannedComponent, scan_component};
use crate::component_shape::validate_shape;
use crate::component_world::ComponentWorld;
use crate::error::PreflightError;

/// Hard maximum encoded component size (4 MiB).
pub const MAX_COMPONENT_BYTES: usize = 4 * 1024 * 1024;

/// Bounds one component preflight attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentLimits {
    encoded_bytes: usize,
}

impl ComponentLimits {
    /// Creates a nonzero compile bound no larger than the hard maximum.
    ///
    /// # Errors
    ///
    /// Returns [`PreflightError::InvalidLimits`] for zero or a value greater
    /// than [`MAX_COMPONENT_BYTES`].
    pub const fn new(encoded_bytes: usize) -> Result<Self, PreflightError> {
        if encoded_bytes == 0 || encoded_bytes > MAX_COMPONENT_BYTES {
            return Err(PreflightError::InvalidLimits);
        }
        Ok(Self { encoded_bytes })
    }

    /// Returns the maximum accepted encoded bytes.
    #[must_use]
    pub const fn encoded_bytes(self) -> usize {
        self.encoded_bytes
    }
}

impl Default for ComponentLimits {
    fn default() -> Self {
        Self {
            encoded_bytes: MAX_COMPONENT_BYTES,
        }
    }
}

/// Preflights one binary for an explicitly selected current world.
///
/// The scanner validates and bounds every nested core module before a private
/// engine can be created. Private Wasmtime component types are then compared
/// against the build-time canonical WIT projection. This function never
/// creates a store, instantiates a component, or calls a guest function.
///
/// # Errors
///
/// Returns [`PreflightError`] for an invalid bound or binary, a disabled
/// feature, an unbounded core resource, ambient surface, or any selected-world
/// mismatch.
pub fn preflight_component(
    bytes: &[u8],
    world: ComponentWorld,
    limits: ComponentLimits,
) -> Result<(), PreflightError> {
    static COMPONENTS: ComponentCache = ComponentCache::preflight();

    let scanned = scan_bounded_component(bytes, world, limits)?;
    COMPONENTS.compile(scanned).map(|_| ())
}

pub(crate) fn scan_bounded_component(
    bytes: &[u8],
    world: ComponentWorld,
    limits: ComponentLimits,
) -> Result<ScannedComponent<'_>, PreflightError> {
    if bytes.len() > limits.encoded_bytes {
        return Err(PreflightError::ComponentTooLarge);
    }
    if !wasmparser::Parser::is_component(bytes) {
        return Err(PreflightError::InvalidComponent);
    }
    scan_component(bytes, world)
}

// Requiring the scanner-issued token makes Engine creation, reference-cache
// construction, and candidate compilation unreachable before scanning succeeds.
pub(crate) struct ComponentCache {
    runtime_policy: bool,
    trusted: OnceLock<Result<TrustedComponents, PreflightError>>,
}

impl ComponentCache {
    const fn preflight() -> Self {
        Self {
            runtime_policy: false,
            trusted: OnceLock::new(),
        }
    }

    pub(crate) const fn runtime() -> Self {
        Self {
            runtime_policy: true,
            trusted: OnceLock::new(),
        }
    }

    pub(crate) fn compile(
        &self,
        scanned: ScannedComponent<'_>,
    ) -> Result<Component, PreflightError> {
        let trusted = match self
            .trusted
            .get_or_init(|| TrustedComponents::build(self.runtime_policy))
        {
            Ok(trusted) => trusted,
            Err(error) => return Err(*error),
        };
        let (bytes, world) = scanned.into_parts();
        let component = Component::from_binary(&trusted.engine, bytes)
            .map_err(|_| PreflightError::InvalidComponent)?;
        validate_shape(&component, trusted.reference(world), world)?;
        Ok(component)
    }

    pub(crate) fn engine(&self) -> Result<Option<&Engine>, PreflightError> {
        match self.trusted.get() {
            None => Ok(None),
            Some(Ok(trusted)) => Ok(Some(&trusted.engine)),
            Some(Err(error)) => Err(*error),
        }
    }

    #[cfg(test)]
    pub(crate) fn is_initialized(&self) -> bool {
        self.trusted.get().is_some()
    }
}

struct TrustedComponents {
    engine: Engine,
    references: [Component; ComponentWorld::ALL.len()],
}

impl TrustedComponents {
    fn build(runtime_policy: bool) -> Result<Self, PreflightError> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.wasm_threads(false);
        config.wasm_memory64(false);
        config.wasm_shared_everything_threads(false);
        config.wasm_component_model_memory64(false);
        config.wasm_component_model_async(false);
        // Concurrent component calls survive future cancellation; owners require
        // synchronous fiber cancellation before a Store can be restored.
        config.concurrency_support(false);
        config.wasm_component_model_threading(false);
        config.wasm_component_model_error_context(false);
        config.consume_fuel(runtime_policy);
        config.epoch_interruption(runtime_policy);
        let engine = Engine::new(&config).map_err(|_| PreflightError::Engine)?;
        let references = ComponentWorld::ALL
            .into_iter()
            .map(|world| {
                Component::from_binary(&engine, world.reference_bytes())
                    .map_err(|_| PreflightError::Engine)
            })
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| PreflightError::Engine)?;
        Ok(Self { engine, references })
    }

    fn reference(&self, world: ComponentWorld) -> &Component {
        &self.references[world.index()]
    }
}
