//! Shared FeaturePack WIT contract-test support.

#![expect(
    dead_code,
    reason = "each integration-test crate uses a family-specific subset"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde_json::Value;
use sha2::{Digest as _, Sha256};
use wit_parser::{
    Function, FunctionKind, Handle, Interface, PackageId, Resolve, Type, TypeDefKind, WorldItem,
    WorldKey,
};

pub(crate) fn assert_lf(path: &str, source: &str) {
    assert!(
        !source.as_bytes().contains(&b'\r'),
        "{path} must contain only LF"
    );
    assert_eq!(
        source.as_bytes().last(),
        Some(&b'\n'),
        "{path} must end in LF"
    );
}

pub(crate) fn assert_semantic_sha256(
    path: &str,
    source: &str,
    expected: &str,
    mutation: (&str, &str),
) {
    let actual = sha256_hex(source);
    assert_eq!(actual, expected, "{path} SHA-256");
    assert_ne!(
        mutation.0, mutation.1,
        "{path} mutation must change a semantic value"
    );
    assert_eq!(
        source.matches(mutation.0).count(),
        1,
        "{path} mutation source must occur exactly once"
    );

    let mutated = source.replacen(mutation.0, mutation.1, 1);
    drop(semantic_rules(&mutated));
    assert_ne!(
        sha256_hex(&mutated),
        expected,
        "{path} semantic mutation must fail the exact content lock"
    );
}

fn sha256_hex(source: &str) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in Sha256::digest(source.as_bytes()) {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

pub(crate) fn parse(path: &str, source: &str) -> (Resolve, PackageId) {
    let mut resolve = Resolve::default();
    let package_id = resolve
        .push_str(path, source)
        .unwrap_or_else(|error| panic!("{path} must parse with wit-parser 0.254.0: {error:#}"));
    (resolve, package_id)
}

pub(crate) fn assert_component_encoding(path: &str, resolve: &Resolve, package_id: PackageId) {
    let component = wit_component::encode(resolve, package_id)
        .unwrap_or_else(|error| panic!("{path} must encode with wit-component 0.254.0: {error:#}"));
    wasmparser::validate(&component).unwrap_or_else(|error| {
        panic!("{path} component must validate with wasmparser 0.254.0: {error:#}")
    });
}

pub(crate) fn assert_world_topology(
    resolve: &Resolve,
    package_id: PackageId,
    package_name: &str,
    family: &str,
    interface_order: &[&str],
    host_name: &str,
    pack_name: &str,
) -> (wit_parser::InterfaceId, wit_parser::InterfaceId) {
    let package = &resolve.packages[package_id];
    assert_eq!(package.name.to_string(), package_name);
    assert_eq!(interface_keys(resolve, package_id), interface_order);
    assert_eq!(
        package
            .worlds
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [family]
    );

    let host_id = package.interfaces[host_name];
    let pack_id = package.interfaces[pack_name];
    let world = &resolve.worlds[package.worlds[family]];
    assert_eq!(world.imports.len(), 1);
    assert_eq!(world.exports.len(), 1);
    assert_world_interface(world.imports.first().expect("host import"), host_id);
    assert_world_interface(world.exports.first().expect("pack export"), pack_id);
    (pack_id, host_id)
}

pub(crate) fn assert_zero_import_world_topology(
    resolve: &Resolve,
    package_id: PackageId,
    package_name: &str,
    family: &str,
    pack_name: &str,
) -> wit_parser::InterfaceId {
    let package = &resolve.packages[package_id];
    assert_eq!(package.name.to_string(), package_name);
    assert_eq!(interface_keys(resolve, package_id), [pack_name]);
    assert_eq!(
        package
            .worlds
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [family]
    );

    let pack_id = package.interfaces[pack_name];
    let world = &resolve.worlds[package.worlds[family]];
    assert!(world.imports.is_empty(), "{family} must remain zero-import");
    assert_eq!(world.exports.len(), 1);
    assert_world_interface(world.exports.first().expect("pack export"), pack_id);
    pack_id
}

pub(crate) fn package_interface<'a>(
    resolve: &'a Resolve,
    package_id: PackageId,
    name: &str,
) -> &'a Interface {
    &resolve.interfaces[resolve.packages[package_id].interfaces[name]]
}

pub(crate) fn interface_keys(resolve: &Resolve, package_id: PackageId) -> Vec<&str> {
    resolve.packages[package_id]
        .interfaces
        .keys()
        .map(String::as_str)
        .collect()
}

pub(crate) fn assert_invoke_and_pull(
    resolve: &Resolve,
    interface: &Interface,
    operation_name: &str,
    request_type: &str,
    error_type: &str,
    pull_type: &str,
) {
    assert_owned_function(
        resolve,
        interface,
        "invoke",
        "request",
        request_type,
        operation_name,
        error_type,
    );
    assert_pull(resolve, interface, operation_name, pull_type);
}

pub(crate) fn assert_owned_function(
    resolve: &Resolve,
    interface: &Interface,
    name: &str,
    parameter: &str,
    parameter_type: &str,
    resource: &str,
    error: &str,
) {
    let function = &interface.functions[name];
    assert_eq!(function.kind, FunctionKind::Freestanding);
    assert_parameter(resolve, function, 0, parameter, parameter_type);
    assert_owned_result(resolve, function, interface.types[resource], error);
}

pub(crate) fn assert_zero_parameter_owned_function(
    resolve: &Resolve,
    interface: &Interface,
    name: &str,
    resource: &str,
    error: &str,
) {
    let function = &interface.functions[name];
    assert_eq!(function.kind, FunctionKind::Freestanding);
    assert!(function.params.is_empty());
    assert_owned_result(resolve, function, interface.types[resource], error);
}

pub(crate) fn assert_pull(
    resolve: &Resolve,
    interface: &Interface,
    resource: &str,
    pull_type: &str,
) {
    let resource_id = interface.types[resource];
    let pull_name = format!("[method]{resource}.pull");
    let pull = &interface.functions[pull_name.as_str()];
    assert_eq!(pull.kind, FunctionKind::Method(resource_id));
    assert_method_self(resolve, pull, resource_id);
    assert_eq!(
        named_type(resolve, pull.result.as_ref().expect("pull result")),
        Some(pull_type)
    );
}

pub(crate) fn assert_freestanding_function(
    resolve: &Resolve,
    interface: &Interface,
    name: &str,
    parameter: &str,
    parameter_type: &str,
    ok: &str,
    error: &str,
) {
    let function = &interface.functions[name];
    assert_eq!(function.kind, FunctionKind::Freestanding);
    assert_parameter(resolve, function, 0, parameter, parameter_type);
    let result = result_type(resolve, function);
    assert_eq!(
        named_type(resolve, result.ok.as_ref().expect("result ok")),
        Some(ok)
    );
    assert_eq!(
        named_type(resolve, result.err.as_ref().expect("result error")),
        Some(error)
    );
}

pub(crate) fn assert_named_aliases(
    resolve: &Resolve,
    aliases: &Interface,
    source: &Interface,
    names: &[&str],
) {
    for name in names {
        let TypeDefKind::Type(Type::Id(source_id)) = resolve.types[aliases.types[*name]].kind
        else {
            panic!("Pack use {name} must be a named alias");
        };
        assert_eq!(
            source_id, source.types[*name],
            "Pack use must retain {name} identity"
        );
    }
}

pub(crate) fn type_inventory(resolve: &Resolve, interface: &Interface) -> String {
    interface
        .types
        .iter()
        .map(|(name, id)| inventory_line(resolve, name, *id))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

pub(crate) fn type_inventory_in_order(
    resolve: &Resolve,
    interface: &Interface,
    expected: &str,
) -> String {
    let types = interface
        .types
        .iter()
        .map(|(name, id)| (name.as_str(), *id))
        .collect::<BTreeMap<_, _>>();
    ordered_type_inventory(resolve, types, expected)
}

pub(crate) fn type_inventory_with_resource(
    resolve: &Resolve,
    interface: &Interface,
    resource_interface: &Interface,
    resource: &str,
    expected: &str,
) -> String {
    let mut types = interface
        .types
        .iter()
        .map(|(name, id)| (name.as_str(), *id))
        .collect::<BTreeMap<_, _>>();
    types.insert(resource, resource_interface.types[resource]);
    ordered_type_inventory(resolve, types, expected)
}

pub(crate) fn variant_cases<'a>(
    resolve: &'a Resolve,
    interface: &Interface,
    name: &str,
) -> Vec<&'a str> {
    let TypeDefKind::Variant(value) = &resolve.types[interface.types[name]].kind else {
        panic!("{name} must be a variant");
    };
    value.cases.iter().map(|case| case.name.as_str()).collect()
}

pub(crate) fn allowed_vocabulary(source: &str, denied: &[&str]) -> bool {
    let lower = source.to_ascii_lowercase();
    !denied.iter().any(|word| lower.contains(word))
}

pub(crate) fn assert_denied_names(
    resolve: &Resolve,
    package_id: PackageId,
    denied: &[&str],
    denied_prefixes: &[&str],
) {
    let package = &resolve.packages[package_id];
    let names = package
        .interfaces
        .keys()
        .chain(package.worlds.keys())
        .chain(
            package
                .interfaces
                .values()
                .flat_map(|id| resolve.interfaces[*id].types.keys()),
        )
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for name in names {
        assert!(!denied.contains(&name), "denied name {name}");
        assert!(
            !denied_prefixes
                .iter()
                .any(|prefix| name.starts_with(prefix)),
            "denied name {name}"
        );
    }
}

pub(crate) fn assert_denied_types(resolve: &Resolve) {
    for (_, definition) in resolve.types.iter() {
        assert!(!matches!(
            definition.kind,
            TypeDefKind::Map(_, _)
                | TypeDefKind::FixedLengthList(_, _)
                | TypeDefKind::Future(_)
                | TypeDefKind::Stream(_)
        ));
    }
}

pub(crate) fn semantic_rules(source: &str) -> BTreeMap<String, Value> {
    let mut rules = BTreeMap::new();
    for (index, line) in source.lines().enumerate() {
        let value: Value = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("semantic line {}: {error}", index + 1));
        let name = value["rule"].as_str().expect("rule name").to_owned();
        assert!(
            rules.insert(name.clone(), value).is_none(),
            "duplicate rule {name}"
        );
    }
    rules
}

pub(crate) fn assert_rule_inventory<'a>(
    rules: &BTreeMap<String, Value>,
    expected: impl IntoIterator<Item = &'a str>,
) {
    assert_eq!(
        rules.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        expected.into_iter().collect::<BTreeSet<_>>()
    );
}

pub(crate) fn assert_json(
    rules: &BTreeMap<String, Value>,
    rule: &str,
    pointer: &str,
    expected: &str,
) {
    let expected: Value = serde_json::from_str(expected).expect("expected JSON");
    assert_eq!(
        rules[rule].pointer(pointer),
        Some(&expected),
        "{rule}{pointer}"
    );
}

fn assert_world_interface(item: (&WorldKey, &WorldItem), expected: wit_parser::InterfaceId) {
    assert_eq!(item.0, &WorldKey::Interface(expected));
    let WorldItem::Interface { id, .. } = item.1 else {
        panic!("world item must be an interface");
    };
    assert_eq!(*id, expected);
}

fn assert_method_self(resolve: &Resolve, function: &Function, resource: wit_parser::TypeId) {
    assert_eq!(function.params.len(), 1);
    assert_eq!(function.params[0].name, "self");
    let Type::Id(self_id) = function.params[0].ty else {
        panic!("method self must be a borrowed handle");
    };
    assert_eq!(
        resolve.types[self_id].kind,
        TypeDefKind::Handle(Handle::Borrow(resource))
    );
}

fn assert_parameter(resolve: &Resolve, function: &Function, index: usize, name: &str, ty: &str) {
    assert_eq!(function.params.len(), index + 1);
    assert_eq!(function.params[index].name, name);
    assert_eq!(named_type(resolve, &function.params[index].ty), Some(ty));
}

fn assert_owned_result(
    resolve: &Resolve,
    function: &Function,
    resource: wit_parser::TypeId,
    error: &str,
) {
    let result = result_type(resolve, function);
    let Type::Id(ok_id) = result.ok.expect("result ok") else {
        panic!("function ok must be an owned handle");
    };
    assert_eq!(
        resolve.types[ok_id].kind,
        TypeDefKind::Handle(Handle::Own(resource))
    );
    assert_eq!(
        named_type(resolve, result.err.as_ref().expect("result error")),
        Some(error)
    );
}

fn result_type<'a>(resolve: &'a Resolve, function: &Function) -> &'a wit_parser::Result_ {
    let Type::Id(id) = function.result.expect("function result") else {
        panic!("function must return a typed result");
    };
    let TypeDefKind::Result(result) = &resolve.types[id].kind else {
        panic!("function must return a typed result");
    };
    result
}

fn named_type<'a>(resolve: &'a Resolve, ty: &Type) -> Option<&'a str> {
    let Type::Id(id) = ty else { return None };
    resolve.types[*id].name.as_deref()
}

fn ordered_type_inventory(
    resolve: &Resolve,
    mut types: BTreeMap<&str, wit_parser::TypeId>,
    expected: &str,
) -> String {
    let mut lines = Vec::new();
    for expected_line in expected.lines() {
        let name = expected_line.split_once('=').expect("inventory entry").0;
        let id = types
            .remove(name)
            .unwrap_or_else(|| panic!("missing named type {name}"));
        lines.push(inventory_line(resolve, name, id));
    }
    lines.extend(
        types
            .into_iter()
            .map(|(name, id)| inventory_line(resolve, name, id)),
    );
    lines.join("\n") + "\n"
}

fn inventory_line(resolve: &Resolve, name: &str, id: wit_parser::TypeId) -> String {
    format!(
        "{name}={}",
        definition_shape(resolve, &resolve.types[id].kind)
    )
}

fn definition_shape(resolve: &Resolve, kind: &TypeDefKind) -> String {
    match kind {
        TypeDefKind::Type(ty) => format!("alias({})", type_shape(resolve, ty)),
        TypeDefKind::Record(value) => format!(
            "record({})",
            value
                .fields
                .iter()
                .map(|field| format!("{}:{}", field.name, type_shape(resolve, &field.ty)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        TypeDefKind::Variant(value) => format!(
            "variant({})",
            value
                .cases
                .iter()
                .map(|case| case.ty.as_ref().map_or_else(
                    || case.name.clone(),
                    |ty| format!("{}:{}", case.name, type_shape(resolve, ty))
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
        TypeDefKind::Enum(value) => format!(
            "enum({})",
            value
                .cases
                .iter()
                .map(|case| case.name.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ),
        TypeDefKind::Flags(value) => format!(
            "flags({})",
            value
                .flags
                .iter()
                .map(|flag| flag.name.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ),
        TypeDefKind::Resource => "resource".to_owned(),
        other => panic!("unexpected named type {other:?}"),
    }
}

fn type_shape(resolve: &Resolve, ty: &Type) -> String {
    match ty {
        Type::Bool => "bool".to_owned(),
        Type::U8 => "u8".to_owned(),
        Type::U16 => "u16".to_owned(),
        Type::U32 => "u32".to_owned(),
        Type::U64 => "u64".to_owned(),
        Type::S16 => "s16".to_owned(),
        Type::String => "string".to_owned(),
        Type::Id(id) => {
            resolve.types[*id]
                .name
                .clone()
                .unwrap_or_else(|| match &resolve.types[*id].kind {
                    TypeDefKind::List(inner) => format!("list<{}>", type_shape(resolve, inner)),
                    TypeDefKind::Option(inner) => format!("option<{}>", type_shape(resolve, inner)),
                    TypeDefKind::Handle(Handle::Own(resource)) => format!(
                        "own<{}>",
                        resolve.types[*resource]
                            .name
                            .as_deref()
                            .expect("named resource")
                    ),
                    TypeDefKind::Type(inner) => type_shape(resolve, inner),
                    other => panic!("unexpected anonymous type {other:?}"),
                })
        }
        other => panic!("unexpected field type {other:?}"),
    }
}
