//! Nested core-resource, feature, ambient-import, and malformed-binary gates.

#[path = "support/preflight_fixtures.rs"]
mod fixtures;

use mcode_plugin_host::{
    ComponentLimits, ComponentWorld, ImportCategory, MAX_COMPONENT_BYTES, PreflightError,
    preflight_component,
};
use wasm_encoder::{
    Component, ComponentExportKind, ComponentExportSection, ComponentImportSection,
    ComponentInstanceSection, ComponentType, ComponentTypeRef, ComponentTypeSection,
    CoreTypeSection, EntityType, InstanceType, MemoryType, ModuleType, NestedComponentSection,
};

fn preflight_fixture(wat: &str) -> Result<(), PreflightError> {
    let bytes = fixtures::component_binary(wat);
    preflight_component(&bytes, ComponentWorld::Provider, ComponentLimits::default())
}

fn core_module(items: &str) -> String {
    format!("(component (core module {items}))")
}

fn append_empty_web_export(component: &mut Component) {
    let empty = Component::new();
    component.section(&NestedComponentSection(&empty));
    let mut instances = ComponentInstanceSection::new();
    instances.instantiate(0, [] as [(&str, ComponentExportKind, u32); 0]);
    component.section(&instances);
    let mut exports = ComponentExportSection::new();
    exports.export(
        "mcode:feature-pack/web-pack@0.0.1",
        ComponentExportKind::Instance,
        0,
        None,
    );
    component.section(&exports);
}

fn component_with_module_import(name: &str, web_export: bool) -> Vec<u8> {
    let mut types = CoreTypeSection::new();
    types.ty().module(&ModuleType::new());
    let mut imports = ComponentImportSection::new();
    imports.import(name, ComponentTypeRef::Module(0));
    let mut component = Component::new();
    component.section(&types);
    component.section(&imports);
    if web_export {
        append_empty_web_export(&mut component);
    }
    component.finish()
}

fn component_with_instance_import(name: &str, web_export: bool) -> Vec<u8> {
    let mut types = ComponentTypeSection::new();
    types.instance(&InstanceType::new());
    let mut imports = ComponentImportSection::new();
    imports.import(name, ComponentTypeRef::Instance(0));
    let mut component = Component::new();
    component.section(&types);
    component.section(&imports);
    if web_export {
        append_empty_web_export(&mut component);
    }
    component.finish()
}

#[test]
fn memory_declarations_and_imports_require_finite_32_bit_maxima() {
    assert_eq!(
        preflight_fixture(&core_module("(memory 0)")).expect_err("unbounded memory"),
        PreflightError::UnboundedMemory,
    );
    assert_eq!(
        preflight_fixture(&core_module("(memory 0 1025)")).expect_err("memory over 64 MiB"),
        PreflightError::MemoryLimit,
    );
    assert_eq!(
        preflight_fixture(&core_module("(memory i64 0 1)")).expect_err("memory64"),
        PreflightError::DisabledFeature,
    );
    assert_eq!(
        preflight_fixture(&core_module(r#"(import "adapter" "memory" (memory 0))"#,))
            .expect_err("unbounded imported memory"),
        PreflightError::UnboundedMemory,
    );
    assert_eq!(
        preflight_fixture(&core_module("(memory 0 1024)")).expect_err("missing world export"),
        PreflightError::MissingExport,
    );
}

#[test]
fn memory_aggregate_and_count_have_exact_independent_boundaries() {
    let aggregate_boundary = r#"(component
        (core module (memory 0 1024))
        (core module (memory 0 1024))
    )"#;
    assert_eq!(
        preflight_fixture(aggregate_boundary).expect_err("exact aggregate boundary"),
        PreflightError::MissingExport,
    );

    let aggregate_overflow = r#"(component
        (core module (memory 0 1024))
        (core module (memory 0 1024))
        (core module (memory 0 1))
    )"#;
    assert_eq!(
        preflight_fixture(aggregate_overflow).expect_err("aggregate boundary plus one page"),
        PreflightError::MemoryAggregateLimit,
    );

    let count_boundary = r#"(component
        (core module (memory 0 0))
        (core module (memory 0 0))
    )"#;
    assert_eq!(
        preflight_fixture(count_boundary).expect_err("exact memory-count boundary"),
        PreflightError::MissingExport,
    );

    let count_overflow = r#"(component
        (core module (memory 0 0))
        (core module (memory 0 0))
        (core module (memory 0 0))
    )"#;
    assert_eq!(
        preflight_fixture(count_overflow).expect_err("memory-count boundary plus one"),
        PreflightError::MemoryCount,
    );
}

#[test]
fn tables_require_finite_bounded_32_bit_maxima_and_global_totals() {
    assert_eq!(
        preflight_fixture(&core_module("(table 0 funcref)")).expect_err("unbounded table"),
        PreflightError::UnboundedTable,
    );
    assert_eq!(
        preflight_fixture(&core_module("(table 0 65537 funcref)")).expect_err("oversized table"),
        PreflightError::TableLimit,
    );
    assert_eq!(
        preflight_fixture(&core_module("(table i64 0 1 funcref)")).expect_err("table64"),
        PreflightError::DisabledFeature,
    );
    assert_eq!(
        preflight_fixture(&core_module("(table 0 65536 funcref)"))
            .expect_err("missing world export"),
        PreflightError::MissingExport,
    );

    let too_many = core_module(
        "(table 0 1 funcref) (table 0 1 funcref) (table 0 1 funcref) \
         (table 0 1 funcref) (table 0 1 funcref)",
    );
    assert_eq!(
        preflight_fixture(&too_many).expect_err("fifth table"),
        PreflightError::TableCount,
    );

    let aggregate = core_module("(table 0 32769 funcref) (table 0 32768 funcref)");
    assert_eq!(
        preflight_fixture(&aggregate).expect_err("aggregate table maximum"),
        PreflightError::TableAggregateLimit,
    );

    let imported = core_module(r#"(import "adapter" "callbacks" (table 0 65537 funcref))"#);
    assert_eq!(
        preflight_fixture(&imported).expect_err("oversized imported table"),
        PreflightError::TableLimit,
    );
}

#[test]
fn component_model_async_types_are_disabled() {
    assert_eq!(
        preflight_fixture("(component (type (future u8)))").expect_err("component-model future"),
        PreflightError::DisabledFeature,
    );
}

#[test]
fn threads_shared_memory_and_atomic_operators_are_disabled() {
    let shared = core_module("(memory 1 1 shared)");
    assert_eq!(
        preflight_fixture(&shared).expect_err("shared memory"),
        PreflightError::DisabledFeature,
    );

    let atomic = core_module(
        "(memory 1 1 shared) \
         (func (drop (i32.atomic.load (i32.const 0))))",
    );
    assert_eq!(
        preflight_fixture(&atomic).expect_err("atomic operator"),
        PreflightError::DisabledFeature,
    );
}

#[test]
fn core_instance_count_has_an_exact_sixty_four_boundary() {
    let instances = |count: usize| {
        let mut component = String::from("(component (core module $m)");
        for _ in 0..count {
            component.push_str(" (core instance (instantiate $m))");
        }
        component.push(')');
        component
    };

    assert_eq!(
        preflight_fixture(&instances(64)).expect_err("missing world export"),
        PreflightError::MissingExport,
    );
    assert_eq!(
        preflight_fixture(&instances(65)).expect_err("65th core instance"),
        PreflightError::CoreInstanceLimit,
    );
}

#[test]
fn nested_component_ambient_imports_are_rejected_at_every_depth() {
    for (name, category) in [
        ("wasi:filesystem/types@0.2.0", ImportCategory::Filesystem),
        ("vendor:secret/store@1.0.0", ImportCategory::Secret),
        ("vendor:log/diagnostic@1.0.0", ImportCategory::Logging),
        ("vendor:ui/view@1.0.0", ImportCategory::UserInterface),
    ] {
        for depth in 1..=3 {
            let mut nested = format!(r#"(import "{name}" (func))"#);
            for _ in 0..depth {
                nested = format!("(component {nested})");
            }
            let component = format!("(component {nested})");
            assert_eq!(
                preflight_fixture(&component).expect_err("nested ambient component import"),
                PreflightError::DeniedImport(category),
                "{name} at depth {depth}",
            );
        }
    }
}

#[test]
fn only_lowercase_mcode_trusts_nested_component_imports() {
    for name in ["MCODE:wasi/filesystem", "mCoDe:WASI/io"] {
        let component = format!(r#"(component (component (import "{name}" (func))))"#);
        assert_eq!(
            preflight_fixture(&component).expect_err("wrong-case nested component import"),
            PreflightError::InvalidComponent,
            "{name}",
        );
    }

    let component = r#"(component (component (import "mcode:wasi/filesystem" (func))))"#;
    assert_eq!(
        preflight_fixture(component).expect_err("world export remains absent"),
        PreflightError::MissingExport,
    );
}

#[test]
fn core_import_module_and_field_names_are_both_classified() {
    let fixtures = [
        (
            r#"(import "wasi_snapshot_preview1" "fd-read" (func))"#,
            ImportCategory::Wasi,
        ),
        (
            r#"(import "adapter" "wasi_snapshot_preview1" (func))"#,
            ImportCategory::Wasi,
        ),
        (r#"(import "env" "callback" (func))"#, ImportCategory::Wasi),
        (
            r#"(import "x-mcode:wasi:filesystem" "callback" (func))"#,
            ImportCategory::Filesystem,
        ),
        (
            r#"(import "adapter" "x-mcode:wasi:filesystem" (func))"#,
            ImportCategory::Filesystem,
        ),
        (
            r#"(import "vendor:host/ui" "callback" (func))"#,
            ImportCategory::UserInterface,
        ),
        (
            r#"(import "adapter" "vendor:host/log" (func))"#,
            ImportCategory::Logging,
        ),
        (
            r#"(import "vendor:log/callback" "callback" (func))"#,
            ImportCategory::Logging,
        ),
        (
            r#"(import "vendor:ui/callback" "callback" (func))"#,
            ImportCategory::UserInterface,
        ),
    ];
    for (import, category) in fixtures {
        let component = core_module(import);
        assert_eq!(
            preflight_fixture(&component).expect_err(import),
            PreflightError::DeniedImport(category),
            "{import}",
        );
    }
}

#[test]
fn only_lowercase_mcode_trusts_nested_core_module_and_field_imports() {
    for (import, category) in [
        (
            r#"(import "MCODE:wasi:filesystem" "callback" (func))"#,
            ImportCategory::Filesystem,
        ),
        (
            r#"(import "adapter" "mCoDe:WASI:io" (func))"#,
            ImportCategory::Wasi,
        ),
    ] {
        let component = format!("(component (component {}))", core_module(import));
        assert_eq!(
            preflight_fixture(&component).expect_err("wrong-case nested core import"),
            PreflightError::DeniedImport(category),
            "{import}",
        );
    }
}

#[test]
fn lowercase_mcode_internal_core_imports_are_not_ambient() {
    for import in [
        r#"(import "adapter" "callback" (func))"#,
        r#"(import "mcode:wasi:filesystem" "callback" (func))"#,
        r#"(import "adapter" "mcode:wasi:filesystem" (func))"#,
    ] {
        let component = core_module(import);
        assert_eq!(
            preflight_fixture(&component).expect_err("world export remains absent"),
            PreflightError::MissingExport,
            "{import}",
        );
    }
}

#[test]
fn root_core_module_and_instance_imports_fail_topology_or_exact_shape() {
    for bytes in [
        component_with_module_import("external-module", false),
        component_with_instance_import("external-instance", false),
    ] {
        assert_eq!(
            preflight_component(&bytes, ComponentWorld::Provider, ComponentLimits::default(),)
                .expect_err("external root core import"),
            PreflightError::DeniedImport(ImportCategory::Extra),
        );
    }

    const WEB_HOST_IMPORT: &str = "mcode:feature-pack/web-host@0.0.1";
    for bytes in [
        component_with_module_import(WEB_HOST_IMPORT, true),
        component_with_instance_import(WEB_HOST_IMPORT, true),
    ] {
        assert_eq!(
            preflight_component(&bytes, ComponentWorld::Web, ComponentLimits::default())
                .expect_err("wrong-kind exact-name web-host import"),
            PreflightError::ImportShape,
        );
    }
}

#[test]
fn uninstantiated_component_type_resources_are_not_live_declarations() {
    let memory = MemoryType {
        minimum: 0,
        maximum: Some(1_024),
        memory64: false,
        shared: false,
        page_size_log2: None,
    };
    let mut module = ModuleType::new();
    module
        .import("types", "first", EntityType::Memory(memory))
        .import("types", "second", EntityType::Memory(memory))
        .import("types", "third", EntityType::Memory(memory));
    let mut component_type = ComponentType::new();
    component_type.core_type().module(&module);
    let mut types = ComponentTypeSection::new();
    types.component(&component_type);
    let mut component = Component::new();
    component.section(&types);
    assert_eq!(
        preflight_component(
            &component.finish(),
            ComponentWorld::Provider,
            ComponentLimits::default()
        )
        .expect_err("no live world export"),
        PreflightError::MissingExport,
    );
}

#[test]
fn malformed_mutations_core_wasm_wat_and_oversize_input_are_binary_rejected() {
    let provider = fixtures::canonical_component(ComponentWorld::Provider);
    let mut truncated = provider.clone();
    truncated.truncate(truncated.len() - 1);
    assert_eq!(
        preflight_component(
            &truncated,
            ComponentWorld::Provider,
            ComponentLimits::default()
        )
        .expect_err("truncated binary"),
        PreflightError::InvalidComponent,
    );

    let core = wat::parse_str("(module)").expect("core binary");
    assert_eq!(
        preflight_component(&core, ComponentWorld::Provider, ComponentLimits::default())
            .expect_err("core binary"),
        PreflightError::InvalidComponent,
    );
    assert_eq!(
        preflight_component(
            b"(component)",
            ComponentWorld::Provider,
            ComponentLimits::default(),
        )
        .expect_err("plaintext WAT"),
        PreflightError::InvalidComponent,
    );

    let oversized = vec![0; MAX_COMPONENT_BYTES + 1];
    assert_eq!(
        preflight_component(
            &oversized,
            ComponentWorld::Provider,
            ComponentLimits::default(),
        )
        .expect_err("hard size maximum"),
        PreflightError::ComponentTooLarge,
    );
}
