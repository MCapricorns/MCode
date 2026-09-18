//! Stable preflight and caller-binding failures.
/// Classifies an ambient or non-contract component import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportCategory {
    /// A filesystem interface.
    Filesystem,
    /// A network or socket interface.
    Network,
    /// A process, environment, or command interface.
    Process,
    /// A terminal or standard-stream interface.
    Terminal,
    /// An HTTP interface.
    Http,
    /// A random-number interface.
    Random,
    /// A clock interface.
    Clocks,
    /// A secret, key-value, or credential interface.
    Secret,
    /// A logging or diagnostic interface.
    Logging,
    /// A user-interface or rendering interface.
    UserInterface,
    /// A raw Host handle interface.
    RawHost,
    /// Another WASI interface.
    Wasi,
    /// A noncurrent MCode plugin interface version.
    MCodeVersion,
    /// Any other non-contract interface.
    Extra,
}

/// Reports bounded component scanning, compilation, or shape failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PreflightError {
    /// A caller requested an invalid component-size limit.
    #[error("component limits are invalid")]
    InvalidLimits,
    /// Encoded component bytes exceeded the configured hard bound.
    #[error("component exceeds its size limit")]
    ComponentTooLarge,
    /// A disabled WebAssembly proposal was used.
    #[error("component uses a disabled WebAssembly feature")]
    DisabledFeature,
    /// A nested core memory omitted its finite maximum.
    #[error("component contains an unbounded core memory")]
    UnboundedMemory,
    /// A nested core memory exceeded 64 MiB.
    #[error("component core memory exceeds 64 MiB")]
    MemoryLimit,
    /// More than two nested core memories were declared.
    #[error("component contains too many core memories")]
    MemoryCount,
    /// Aggregate nested core-memory maxima exceeded 128 MiB.
    #[error("component aggregate core memory exceeds 128 MiB")]
    MemoryAggregateLimit,
    /// A nested core table omitted its finite maximum.
    #[error("component contains an unbounded core table")]
    UnboundedTable,
    /// A nested core table exceeded 65,536 elements.
    #[error("component core table exceeds 65,536 elements")]
    TableLimit,
    /// More than four nested core tables were declared.
    #[error("component contains too many core tables")]
    TableCount,
    /// Aggregate nested core-table maxima exceeded 65,536 elements.
    #[error("component aggregate core table exceeds 65,536 elements")]
    TableAggregateLimit,
    /// More than 64 core instances were declared.
    #[error("component contains too many core instances")]
    CoreInstanceLimit,
    /// Wasmtime could not create the isolated component engine.
    #[error("component engine is unavailable")]
    Engine,
    /// Input was not a valid WebAssembly component.
    #[error("component is invalid")]
    InvalidComponent,
    /// The component imported an ambient, noncurrent, or extra interface.
    #[error("component imports a denied interface")]
    DeniedImport(ImportCategory),
    /// A required world import was absent.
    #[error("component is missing a required import")]
    MissingImport,
    /// An imported interface did not match the selected current world.
    #[error("component import shape is invalid")]
    ImportShape,
    /// The selected world's export was absent.
    #[error("component is missing its required export")]
    MissingExport,
    /// The component exported another interface or root item.
    #[error("component exports a non-contract item")]
    UnexpectedExport,
    /// An exported interface did not match the selected current world.
    #[error("component export shape is invalid")]
    ExportShape,
}
