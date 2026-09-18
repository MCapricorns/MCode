//! Private Wasmtime bindings for the current FeaturePack worlds.
macro_rules! feature_bindings {
    ($module:ident, $world:literal) => {
        #[allow(missing_docs, reason = "generated Wasmtime bindings")]
        pub(crate) mod $module {
            wasmtime::component::bindgen!({
                path: "../mcode-plugin-api/wit/feature-pack",
                world: $world,
                exports: { default: async },
            });
        }
    };
}

feature_bindings!(web, "web");
feature_bindings!(mcp, "mcp");
feature_bindings!(usage, "usage");
