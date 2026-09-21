//! Exercises adversarial Host-vault source mutations.

use std::fs;

use super::{audit_source, collect_production_sources, guarded_type_violations};

#[test]
fn derive_and_deserializer_mutations_are_rejected() {
    for mutation in [
        "#[derive(serde::Deserialize)] struct Attack;",
        "#[cfg_attr(all(), derive(serde::Deserialize))] struct Attack;",
        "use serde::Deserialize as Borrowed; #[derive(Borrowed)] struct Attack;",
        "use serde::*; #[derive(Deserialize)] struct Attack;",
        "fn attack<D: serde::Deserializer<'static>>(d: D) { let _ = d.deserialize_str(serde::de::IgnoredAny); }",
        "fn attack<D: serde::Deserializer<'static>>(d: D) { let typed = D::deserialize_str; let _ = typed(d, serde::de::IgnoredAny); }",
    ] {
        assert!(
            !audit_source(mutation, true).is_empty(),
            "mutated source bypassed AST audit: {mutation}"
        );
    }
}

#[test]
fn serde_json_aliases_and_include_injection_are_rejected() {
    for mutation in [
        "fn attack(value: serde_json::Value) {}",
        "use serde_json::{Value as V}; fn attack(value: V) {}",
        "use serde_json::{to_vec as encode}; fn attack() { let _ = encode(0); }",
        "use serde_json as json; fn attack() { let _ = json::to_string(&0); }",
        "extern crate serde_json as json; fn attack(value: json::Value) {}",
        "extern crate serde_json; use self::serde_json as json; fn attack(value: json::Value) {}",
        "extern crate serde_json; use self::serde_json::Value as V; fn attack(value: V) {}",
        "extern crate serde_json; mod nested { use super::serde_json as json; fn attack(value: json::Value) {} }",
        "extern crate serde_json; mod nested { use super::serde_json::to_string as encode; fn attack() { let _ = encode(&0); } }",
        "use serde_json as json; use json::Value as V; fn attack(value: V) {}",
        "include!(\"attack.rs\");",
        "use include as inject; inject!(\"attack.rs\");",
        "use core::include as inject; fn attack() { inject!(\"attack.rs\"); }",
        "use std::include as inject; fn attack() { inject!(\"attack.rs\"); }",
        "macro_rules! inject { () => { include!(\"attack.rs\"); } } inject!();",
        "macro_rules! invoke { ($macro:ident) => { $macro!(\"attack.rs\"); } } invoke!(include);",
    ] {
        assert!(
            !audit_source(mutation, false).is_empty(),
            "source injection bypassed AST audit: {mutation}"
        );
    }

    let allowed = "use serde_json::{Deserializer, Error, Serializer}; fn allowed(_: Option<Error>, _: Option<Deserializer<serde_json::de::StrRead<'_>>>, _: Option<Serializer<Vec<u8>>>) {}";
    assert!(
        audit_source(allowed, false).is_empty(),
        "approved serde_json APIs were rejected"
    );
}

#[test]
fn guarded_reducer_types_reject_surface_expansion() {
    let reducer = include_str!("../reducer.rs");
    let mutations = [
        reducer.replacen(
            "struct SecretInput(",
            "#[derive(Debug)]\nstruct SecretInput(",
            1,
        ),
        format!(
            "{reducer}\nimpl Clone for SecretInput {{ fn clone(&self) -> Self {{ Self(Zeroizing::new(Vec::new())) }} }}"
        ),
        format!(
            "{reducer}\nmod leak {{ impl std::fmt::Debug for super::SecretInput {{ fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{ formatter.write_str(\"leak\") }} }} }}"
        ),
        reducer.replacen(
            "impl SecretInput {",
            "impl SecretInput {\n    pub fn secret(&self) -> &[u8] { &self.0 }",
            1,
        ),
        reducer.replacen(
            "impl VaultCommand {",
            "impl VaultCommand {\n    fn command_name(&self) -> &str { \"command\" }",
            1,
        ),
        format!(
            "{reducer}\ntrait Select {{ type Output; }} struct Marker; impl Select for Marker {{ type Output = SecretInput; }} trait Leak {{ fn leak(&self); }} impl Leak for <Marker as Select>::Output {{ fn leak(&self) {{}} }}"
        ),
    ];
    for mutation in mutations {
        assert!(
            !guarded_type_violations(&mutation, true).is_empty(),
            "guarded type mutation bypassed audit"
        );
    }
}

#[test]
fn external_reducer_modules_cannot_expand_guarded_types() {
    for mutation in [
        "impl std::fmt::Debug for super::SecretInput { fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { formatter.write_str(\"leak\") } }",
        "use super::SecretInput as Input; impl std::fmt::Debug for Input { fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { formatter.write_str(\"leak\") } }",
        "type Input = super::SecretInput;",
    ] {
        assert!(
            !guarded_type_violations(mutation, false).is_empty(),
            "external guarded-type mutation bypassed audit: {mutation}"
        );
    }
}

#[test]
fn explicit_reducer_children_retain_guarded_type_auditing() {
    let temp = tempfile::TempDir::new().expect("temporary source tree");
    let module_dir = temp.path().join("host_vault");
    fs::create_dir(&module_dir).expect("module directory");
    let root = temp.path().join("host_vault.rs");
    fs::write(&root, "mod reducer;\n").expect("root source");
    fs::write(
        module_dir.join("reducer.rs"),
        "#[path = \"../leak.rs\"] mod leak;\n",
    )
    .expect("reducer source");
    fs::write(
        temp.path().join("leak.rs"),
        "impl std::fmt::Debug for super::SecretInput {}\n",
    )
    .expect("external reducer child");

    let sources = collect_production_sources(&root, &module_dir);
    let (_, leak, guarded) = sources
        .iter()
        .find(|(_, source, _)| source.contains("super::SecretInput"))
        .expect("external reducer child was walked");
    assert!(
        *guarded,
        "external reducer child lost guarded-type auditing"
    );
    assert!(
        !guarded_type_violations(leak, false).is_empty(),
        "external reducer child bypassed guarded-type auditing"
    );
}

#[test]
fn item_position_macros_cannot_hide_production_modules() {
    let temp = tempfile::TempDir::new().expect("temporary source tree");
    let module_dir = temp.path().join("host_vault");
    fs::create_dir(&module_dir).expect("module directory");
    let root = temp.path().join("host_vault.rs");
    fs::write(
        &root,
        "macro_rules! load { () => { mod attack; } }\nload!();\n",
    )
    .expect("root source");

    assert!(
        std::panic::catch_unwind(|| collect_production_sources(&root, &module_dir)).is_err(),
        "item-position macro silently hid a production module"
    );
}
