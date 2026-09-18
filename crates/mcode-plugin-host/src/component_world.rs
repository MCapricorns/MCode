//! Closed selection of sole-current component worlds.
/// One sole-current plugin component world accepted by static preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ComponentWorld {
    /// Web capability Pack world.
    Web,
    /// MCP capability Pack world.
    Mcp,
    /// Usage capability Pack world.
    Usage,
    /// ProviderPack world.
    Provider,
}

impl ComponentWorld {
    /// Every accepted world in stable family order.
    pub const ALL: [Self; 4] = [Self::Web, Self::Mcp, Self::Usage, Self::Provider];

    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    pub(crate) const fn imports(self) -> &'static [&'static str] {
        match self {
            Self::Web => &["mcode:feature-pack/web-host@0.0.1"],
            Self::Mcp => &["mcode:feature-pack/mcp-host@0.0.1"],
            Self::Usage => &["mcode:feature-pack/usage-host@0.0.1"],
            Self::Provider => &[],
        }
    }

    pub(crate) const fn exports(self) -> &'static [&'static str] {
        match self {
            Self::Web => &["mcode:feature-pack/web-pack@0.0.1"],
            Self::Mcp => &["mcode:feature-pack/mcp-pack@0.0.1"],
            Self::Usage => &["mcode:feature-pack/usage-pack@0.0.1"],
            Self::Provider => &["mcode:provider-pack/provider-api@0.0.1"],
        }
    }

    pub(crate) const fn reference_bytes(self) -> &'static [u8] {
        match self {
            Self::Web => include_bytes!(concat!(env!("OUT_DIR"), "/web.wasm")),
            Self::Mcp => include_bytes!(concat!(env!("OUT_DIR"), "/mcp.wasm")),
            Self::Usage => include_bytes!(concat!(env!("OUT_DIR"), "/usage.wasm")),
            Self::Provider => include_bytes!(concat!(env!("OUT_DIR"), "/provider.wasm")),
        }
    }
}
