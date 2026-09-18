//! Wasmtime bindings for the sole current Provider world.
#![allow(missing_docs, reason = "generated Wasmtime bindings")]

wasmtime::component::bindgen!({
    path: "../mcode-plugin-api/wit/provider",
    world: "provider",
    exports: { default: async },
});
