//! Exact component-interface member, label, version, and topology mutations.

#[path = "support/preflight_fixtures.rs"]
mod fixtures;

use std::collections::BTreeSet;

use mcode_plugin_host::{
    ComponentLimits, ComponentWorld, ImportCategory, PreflightError, preflight_component,
};

#[derive(Debug)]
struct ParameterizedFunction {
    line: usize,
    label: String,
    expected: PreflightError,
}

fn interface_directions(source: &str) -> (BTreeSet<&str>, BTreeSet<&str>) {
    let mut imports = BTreeSet::new();
    let mut exports = BTreeSet::new();
    for line in source.lines().map(str::trim) {
        if let Some(name) = line
            .strip_prefix("import ")
            .and_then(|line| line.strip_suffix(';'))
        {
            assert!(imports.insert(name), "duplicate world import {name}");
        }
        if let Some(name) = line
            .strip_prefix("export ")
            .and_then(|line| line.strip_suffix(';'))
        {
            assert!(exports.insert(name), "duplicate world export {name}");
        }
    }
    (imports, exports)
}

fn parameterized_functions(source: &str) -> Vec<ParameterizedFunction> {
    let (imports, exports) = interface_directions(source);
    let mut functions = Vec::new();
    let mut depth = 0_usize;
    let mut interface: Option<(&str, usize)> = None;
    let mut function_syntax = 0_usize;

    for (line_index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if let Some(declaration) = trimmed.strip_prefix("interface ") {
            let name = declaration
                .strip_suffix('{')
                .expect("interface declaration must be one line")
                .trim();
            assert!(interface.is_none(), "interfaces must not nest");
            interface = Some((name, depth));
        }

        if trimmed.contains(": func") {
            function_syntax += 1;
            let (declaration, tail) = trimmed
                .split_once(": func(")
                .expect("every func syntax must be a one-line named declaration");
            assert!(!declaration.is_empty(), "function name must be present");
            let (parameters, _) = tail
                .split_once(')')
                .expect("function parameter list must close on the same line");
            assert!(
                !parameters.contains(','),
                "at most one parameter is supported"
            );
            if !parameters.is_empty() {
                let (label, ty) = parameters
                    .split_once(':')
                    .expect("the sole function parameter must have a label");
                assert!(!label.trim().is_empty(), "parameter label must be present");
                assert!(!ty.trim().is_empty(), "parameter type must be present");
                let interface = interface
                    .expect("freestanding and resource functions must belong to an interface")
                    .0;
                let expected = match (imports.contains(interface), exports.contains(interface)) {
                    (true, false) => PreflightError::ImportShape,
                    (false, true) => PreflightError::ExportShape,
                    _ => panic!("interface {interface} must have one world direction"),
                };
                functions.push(ParameterizedFunction {
                    line: line_index,
                    label: label.trim().to_owned(),
                    expected,
                });
            }
        }

        depth = depth
            .checked_add(line.matches('{').count())
            .and_then(|value| value.checked_sub(line.matches('}').count()))
            .expect("balanced WIT braces");
        if interface.is_some_and(|(_, outer_depth)| depth == outer_depth) {
            interface = None;
        }
    }

    assert_eq!(depth, 0, "WIT braces must balance");
    assert!(
        function_syntax > 0,
        "each current world must declare functions"
    );
    functions
}

fn mutate_parameter_label(
    source: &str,
    function: &ParameterizedFunction,
    ordinal: usize,
) -> String {
    let mut lines: Vec<_> = source.lines().map(str::to_owned).collect();
    let anchor = format!("func({}:", function.label);
    let replacement = format!("func(frozen-label-{ordinal}:");
    assert_eq!(
        lines[function.line].matches(&anchor).count(),
        1,
        "target line must contain exactly one frozen label"
    );
    lines[function.line] = lines[function.line].replacen(&anchor, &replacement, 1);
    let mut mutated = lines.join("\n");
    if source.ends_with('\n') {
        mutated.push('\n');
    }
    mutated
}

#[test]
fn every_parameterized_function_rejects_its_own_frozen_label_mutation() {
    let mut mutation_count = 0;
    for world in ComponentWorld::ALL {
        let (name, source) = fixtures::world_source(world);
        for function in parameterized_functions(source) {
            let mutated = mutate_parameter_label(source, &function, mutation_count);
            let bytes = fixtures::component_from_wit(name, &mutated);
            assert_eq!(
                preflight_component(&bytes, world, ComponentLimits::default())
                    .expect_err("parameter label mutation must fail exact shape"),
                function.expected,
                "{world:?} line {} label {}",
                function.line + 1,
                function.label,
            );
            mutation_count += 1;
        }
    }
    assert_eq!(
        mutation_count, 12,
        "all frozen parameter labels must be tested"
    );
}

#[test]
fn every_world_rejects_the_same_shape_at_version_zero_zero_two() {
    for world in ComponentWorld::ALL {
        let (name, source) = fixtures::world_source(world);
        let mutated = source.replacen("@0.0.1", "@0.0.2", 1);
        assert_ne!(mutated, source, "{world:?} package version anchor");
        let bytes = fixtures::component_from_wit(name, &mutated);
        let error = preflight_component(&bytes, world, ComponentLimits::default())
            .expect_err("semver-compatible package version must not pass");
        assert!(
            matches!(
                error,
                PreflightError::DeniedImport(ImportCategory::MCodeVersion)
                    | PreflightError::UnexpectedExport
            ),
            "{world:?} returned {error:?}",
        );
    }
}

#[test]
fn imported_and_exported_instance_member_declaration_order_is_exact() {
    let (name, source) = fixtures::world_source(ComponentWorld::Web);
    for (first, second, expected) in [
        (
            "start-search: func",
            "start-fetch: func",
            PreflightError::ImportShape,
        ),
        ("completed: u8,", "total: u8,", PreflightError::ExportShape),
    ] {
        let mut lines: Vec<_> = source.lines().map(str::to_owned).collect();
        let first = lines
            .iter()
            .position(|line| line.contains(first))
            .expect("first declaration");
        let second = lines
            .iter()
            .position(|line| line.contains(second))
            .expect("second declaration");
        lines.swap(first, second);
        let bytes = fixtures::component_from_wit(name, &format!("{}\n", lines.join("\n")));
        assert_eq!(
            preflight_component(&bytes, ComponentWorld::Web, ComponentLimits::default())
                .expect_err("declaration order mutation"),
            expected,
        );
    }
}

#[test]
fn missing_import_is_distinct_from_import_shape() {
    let (name, source) = fixtures::world_source(ComponentWorld::Web);
    let mutated = source.replacen("    import web-host;\n", "", 1);
    assert_ne!(mutated, source, "web import anchor");
    let bytes = fixtures::component_from_wit(name, &mutated);
    assert_eq!(
        preflight_component(&bytes, ComponentWorld::Web, ComponentLimits::default())
            .expect_err("missing web-host import"),
        PreflightError::MissingImport,
    );
}

#[test]
fn provider_has_zero_imports_and_rejects_an_extra_export() {
    let canonical = fixtures::canonical_component(ComponentWorld::Provider);
    preflight_component(
        &canonical,
        ComponentWorld::Provider,
        ComponentLimits::default(),
    )
    .expect("canonical Provider with zero imports");

    let (name, source) = fixtures::world_source(ComponentWorld::Provider);
    let mutated = source
        .replacen(
            "/// Sole current ProviderPack world.\nworld provider {",
            "interface extra {}\n\n/// Sole current ProviderPack world.\nworld provider {",
            1,
        )
        .replacen(
            "    export provider-api;",
            "    export provider-api;\n    export extra;",
            1,
        );
    assert!(mutated.contains("export extra;"), "Provider export anchor");
    let bytes = fixtures::component_from_wit(name, &mutated);
    assert_eq!(
        preflight_component(&bytes, ComponentWorld::Provider, ComponentLimits::default(),)
            .expect_err("extra Provider export"),
        PreflightError::UnexpectedExport,
    );
}
