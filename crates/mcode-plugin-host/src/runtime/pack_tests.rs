//! Pack compilation boundary tests.

// Rust guideline compliant 2026-09-05.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use wasm_encoder::reencode::{Error, Reencode, utils};
use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
use wit_parser::{ManglingAndAbi, Resolve};

use crate::{ComponentLimits, ComponentWorld, PreflightError};

use super::{PluginRuntime, RuntimeError};

fn bounded_component(name: &str, source: &str) -> Vec<u8> {
    let mut resolve = Resolve::default();
    let package = resolve
        .push_str(name, source)
        .expect("canonical Pack fixture WIT must parse");
    let world = resolve
        .select_world(&[package], Some(name))
        .expect("canonical Pack fixture world must exist");
    let module = wit_component::dummy_module(&resolve, world, ManglingAndAbi::Standard32);
    let mut module = bounded_dummy_module(&module);
    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8)
        .expect("canonical Pack fixture metadata must embed");
    ComponentEncoder::default()
        .module(&module)
        .expect("canonical Pack fixture module must decode")
        .validate(true)
        .encode()
        .expect("canonical Pack fixture component must encode")
}

fn component_with_start(name: &str, source: &str, start: &str) -> Vec<u8> {
    let mut resolve = Resolve::default();
    let package = resolve
        .push_str(name, source)
        .expect("canonical Pack fixture WIT must parse");
    let world = resolve
        .select_world(&[package], Some(name))
        .expect("canonical Pack fixture world must exist");
    let module = wit_component::dummy_module(&resolve, world, ManglingAndAbi::Standard32);
    let mut wat = wasmprinter::print_bytes(bounded_dummy_module(&module))
        .expect("dummy Pack core module must print");
    let module_end = wat
        .rfind(')')
        .expect("dummy Pack core module must have a closing delimiter");
    wat.insert_str(module_end, &format!("{start}\n"));
    let mut module = wat::parse_str(wat).expect("Pack start fixture must parse");
    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8)
        .expect("Pack start fixture metadata must embed");
    ComponentEncoder::default()
        .module(&module)
        .expect("Pack start fixture module must decode")
        .validate(true)
        .encode()
        .expect("Pack start fixture component must encode")
}

fn bounded_dummy_module(module: &[u8]) -> Vec<u8> {
    let mut output = wasm_encoder::Module::new();
    BoundedMemory
        .parse_core_module(&mut output, wasmparser::Parser::new(0), module)
        .expect("dummy Pack core module must reencode");
    output.finish()
}

struct BoundedMemory;

impl Reencode for BoundedMemory {
    type Error = Infallible;

    fn memory_type(
        &mut self,
        memory: wasmparser::MemoryType,
    ) -> Result<wasm_encoder::MemoryType, Error<Self::Error>> {
        let mut memory = utils::memory_type(self, memory);
        memory.maximum = Some(1_024);
        Ok(memory)
    }
}

fn pack_worlds() -> [(ComponentWorld, &'static str, &'static str); 4] {
    [
        (
            ComponentWorld::Web,
            "web",
            include_str!("../../../mcode-plugin-api/wit/feature-pack/web.wit"),
        ),
        (
            ComponentWorld::Mcp,
            "mcp",
            include_str!("../../../mcode-plugin-api/wit/feature-pack/mcp.wit"),
        ),
        (
            ComponentWorld::Usage,
            "usage",
            include_str!("../../../mcode-plugin-api/wit/feature-pack/usage.wit"),
        ),
        (
            ComponentWorld::Provider,
            "provider",
            include_str!("../../../mcode-plugin-api/wit/provider/provider.wit"),
        ),
    ]
}

fn web_component() -> Vec<u8> {
    bounded_component(
        "web",
        include_str!("../../../mcode-plugin-api/wit/feature-pack/web.wit"),
    )
}

fn provider_component() -> Vec<u8> {
    bounded_component(
        "provider",
        include_str!("../../../mcode-plugin-api/wit/provider/provider.wit"),
    )
}
fn provider_source() -> &'static str {
    include_str!("../../../mcode-plugin-api/wit/provider/provider.wit")
}

fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    let mut context = Context::from_waker(Waker::noop());
    future.as_mut().poll(&mut context)
}

#[tokio::test]
async fn every_pack_world_instantiates_through_its_typed_binding() {
    for (world, name, source) in pack_worlds() {
        let runtime = PluginRuntime::new();
        let component = runtime
            .compile_pack(
                bounded_component(name, source),
                world,
                ComponentLimits::default(),
            )
            .expect("exact Pack compile");
        let mut owner = runtime.new_owner().expect("Pack owner");

        let instance = owner
            .instantiate_pack(&component)
            .await
            .unwrap_or_else(|error| panic!("typed {world:?} Pack instantiation: {error:?}"));

        assert_eq!(instance.world(), world);
        assert!(owner.is_available());
    }
}

#[tokio::test]
async fn foreign_runtime_pack_is_rejected_without_consuming_the_owner() {
    let foreign_runtime = PluginRuntime::new();
    let foreign = foreign_runtime
        .compile_pack(
            web_component(),
            ComponentWorld::Web,
            ComponentLimits::default(),
        )
        .expect("foreign Pack compile");
    let runtime = PluginRuntime::new();
    let local = runtime
        .compile_pack(
            provider_component(),
            ComponentWorld::Provider,
            ComponentLimits::default(),
        )
        .expect("local Pack compile");
    let mut owner = runtime.new_owner().expect("local owner");

    assert!(matches!(
        owner.instantiate_pack(&foreign).await,
        Err(RuntimeError::RuntimeMismatch)
    ));
    assert!(owner.is_available());
    owner
        .instantiate_pack(&local)
        .await
        .expect("local Pack remains instantiable");
}

#[tokio::test]
async fn pack_owner_rejects_a_second_pack_instance() {
    let runtime = PluginRuntime::new();
    let web = runtime
        .compile_pack(
            web_component(),
            ComponentWorld::Web,
            ComponentLimits::default(),
        )
        .expect("Web Pack compile");
    let provider = runtime
        .compile_pack(
            provider_component(),
            ComponentWorld::Provider,
            ComponentLimits::default(),
        )
        .expect("Provider Pack compile");
    let mut owner = runtime.new_owner().expect("Pack owner");

    owner
        .instantiate_pack(&web)
        .await
        .expect("first Pack instance");
    assert!(matches!(
        owner.instantiate_pack(&provider).await,
        Err(RuntimeError::InstanceActive)
    ));
}

#[tokio::test]
async fn failed_pack_instantiation_disposes_the_store() {
    let runtime = PluginRuntime::new();
    let component = runtime
        .compile_pack(
            component_with_start(
                "provider",
                provider_source(),
                "  (func $test-start unreachable)\n  (start $test-start)",
            ),
            ComponentWorld::Provider,
            ComponentLimits::default(),
        )
        .expect("trapping Pack compile");
    let mut owner = runtime.new_owner().expect("Pack owner");

    assert!(matches!(
        owner.instantiate_pack(&component).await,
        Err(RuntimeError::Instantiation)
    ));
    assert!(!owner.is_available());
}

#[test]
fn dropped_pack_instantiation_disposes_the_store() {
    let runtime = PluginRuntime::new();
    let component = runtime
        .compile_pack(
            component_with_start(
                "provider",
                provider_source(),
                "  (func $test-start (loop $forever (br $forever)))\n  (start $test-start)",
            ),
            ComponentWorld::Provider,
            ComponentLimits::default(),
        )
        .expect("CPU-bound Pack compile");
    let mut owner = runtime.new_owner().expect("Pack owner");

    let mut pending = Box::pin(owner.instantiate_pack(&component));
    assert!(matches!(poll_once(pending.as_mut()), Poll::Pending));
    drop(pending);

    assert!(!owner.is_available());
}

#[test]
fn exact_pack_world_compilation_initializes_runtime_readiness() {
    let runtime = PluginRuntime::new();
    let bytes = web_component();

    let _compiled = runtime
        .compile_pack(bytes, ComponentWorld::Web, ComponentLimits::default())
        .expect("exact Web Pack compile");

    assert!(runtime.new_owner().is_ok());
}

#[test]
fn crossed_pack_world_does_not_initialize_runtime_readiness() {
    let runtime = PluginRuntime::new();
    let bytes = web_component();

    let result = runtime.compile_pack(bytes, ComponentWorld::Mcp, ComponentLimits::default());

    assert!(matches!(result, Err(RuntimeError::Preflight(_))));
    assert!(matches!(
        runtime.new_owner(),
        Err(RuntimeError::RuntimeUninitialized)
    ));
}

#[test]
fn oversized_pack_does_not_initialize_runtime_readiness() {
    let runtime = PluginRuntime::new();
    let bytes = provider_component();
    let limit = ComponentLimits::new(bytes.len() - 1).expect("smaller nonzero bound");

    let result = runtime.compile_pack(&bytes, ComponentWorld::Provider, limit);

    assert!(matches!(
        result,
        Err(RuntimeError::Preflight(PreflightError::ComponentTooLarge))
    ));
    assert!(matches!(
        runtime.new_owner(),
        Err(RuntimeError::RuntimeUninitialized)
    ));
}
