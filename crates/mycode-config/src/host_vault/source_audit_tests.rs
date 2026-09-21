//! Audits production Host-vault Rust syntax for security-sensitive regressions.

use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use syn::punctuated::Punctuated;
use syn::visit::Visit;
use syn::{
    Attribute, Expr, ExprMethodCall, ExprPath, ImplItem, Item, ItemEnum, ItemMod, ItemUse, Lit,
    Macro, Meta, Path as SynPath, Token, TypePath, UseTree, Visibility, parse_file,
};

#[path = "source_audit_tests/guarded_types.rs"]
mod guarded_types;

use self::guarded_types::guarded_type_violations;

const APPROVED_DERIVES: &[&str] = &[
    "Clone",
    "Copy",
    "Debug",
    "Eq",
    "Hash",
    "Ord",
    "PartialEq",
    "PartialOrd",
    "Serialize",
];

const FORBIDDEN_SOURCE_TOKENS: &[&str] = &[
    "derive(Deserialize",
    ", Deserialize,",
    ", Deserialize)]",
    "deny_unknown_fields",
    "unknown_field",
    "unknown_variant",
    "invalid_type",
    "invalid_value",
    "invalid_length",
    "Unexpected",
    "serde_json::Value",
    "serde_json::to_vec",
    "serde_json::to_string",
    "encode_string",
];

#[derive(Default)]
struct AliasCollector {
    deserialize_aliases: BTreeSet<String>,
    forbidden_alias_declarations: BTreeSet<String>,
    forbidden_json_aliases: BTreeSet<String>,
    include_aliases: BTreeSet<String>,
    json_modules: BTreeSet<String>,
    has_glob: bool,
}

impl<'ast> Visit<'ast> for AliasCollector {
    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        if item.ident == "serde_json" && item.rename.is_some() {
            self.forbidden_alias_declarations
                .insert("serde_json extern-crate alias".into());
        }
        syn::visit::visit_item_extern_crate(self, item);
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        collect_use_tree(&item.tree, &mut Vec::new(), self);
        syn::visit::visit_item_use(self, item);
    }
}

fn collect_use_tree(tree: &UseTree, prefix: &mut Vec<String>, collector: &mut AliasCollector) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            collect_use_tree(&path.tree, prefix, collector);
            prefix.pop();
        }
        UseTree::Name(name) => {
            collect_import_alias(prefix, &name.ident.to_string(), None, collector)
        }
        UseTree::Rename(rename) => collect_import_alias(
            prefix,
            &rename.ident.to_string(),
            Some(rename.rename.to_string()),
            collector,
        ),
        UseTree::Group(group) => {
            for item in &group.items {
                collect_use_tree(item, prefix, collector);
            }
        }
        UseTree::Glob(_) => collector.has_glob = true,
    }
}

fn collect_import_alias(
    prefix: &[String],
    imported: &str,
    renamed: Option<String>,
    collector: &mut AliasCollector,
) {
    let is_renamed = renamed.is_some();
    let local = renamed.unwrap_or_else(|| imported.to_owned());
    if imported == "Deserialize" {
        collector.deserialize_aliases.insert(local.clone());
    }
    let prefix_ends_with_json = prefix.last().is_some_and(|segment| segment == "serde_json");
    if imported == "serde_json" || (prefix_ends_with_json && imported == "self") {
        if is_renamed {
            collector
                .forbidden_alias_declarations
                .insert("serde_json module alias".into());
        }
        collector.json_modules.insert(local.clone());
    }
    if prefix_ends_with_json && matches!(imported, "Value" | "to_vec" | "to_string") {
        collector.forbidden_json_aliases.insert(local.clone());
    }
    if imported == "include" {
        collector
            .forbidden_alias_declarations
            .insert("include macro import".into());
        collector.include_aliases.insert(local);
    }
}

struct SecurityVisitor<'a> {
    aliases: &'a AliasCollector,
    inspect_deserializers: bool,
    violations: Vec<String>,
}

impl SecurityVisitor<'_> {
    fn inspect_attribute(&mut self, attribute: &Attribute) {
        self.inspect_meta(&attribute.meta);
    }

    fn inspect_meta(&mut self, meta: &Meta) {
        if meta.path().is_ident("derive") {
            let Meta::List(list) = meta else {
                self.violations.push("malformed derive attribute".into());
                return;
            };
            let paths = list
                .parse_args_with(Punctuated::<SynPath, Token![,]>::parse_terminated)
                .expect("derive syntax");
            for path in paths {
                let approved = path.segments.len() == 1
                    && path
                        .get_ident()
                        .is_some_and(|name| APPROVED_DERIVES.contains(&name.to_string().as_str()))
                    && path.get_ident().is_none_or(|name| {
                        !self.aliases.deserialize_aliases.contains(&name.to_string())
                    });
                if !approved {
                    self.violations
                        .push(format!("unapproved derive path: {}", path_text(&path)));
                }
            }
            return;
        }

        if meta.path().is_ident("cfg_attr") {
            let Meta::List(list) = meta else {
                self.violations.push("malformed cfg_attr attribute".into());
                return;
            };
            let nested = list
                .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                .expect("cfg_attr syntax");
            for attribute in nested.iter().skip(1) {
                self.inspect_meta(attribute);
            }
        }
    }

    fn inspect_deserializer_name(&mut self, name: &str) {
        if self.inspect_deserializers
            && name.starts_with("deserialize_")
            && name != "deserialize_any"
        {
            self.violations
                .push(format!("typed deserializer entrypoint: {name}"));
        }
        if matches!(
            name,
            "unknown_field"
                | "unknown_variant"
                | "invalid_type"
                | "invalid_value"
                | "invalid_length"
        ) {
            self.violations
                .push(format!("forbidden serde diagnostic helper: {name}"));
        }
    }

    fn inspect_json_path(&mut self, path: &SynPath) {
        let segments = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        let imported_alias =
            segments.len() == 1 && self.aliases.forbidden_json_aliases.contains(&segments[0]);
        let module_member = segments.len() >= 2
            && self.aliases.json_modules.contains(&segments[0])
            && matches!(segments[1].as_str(), "Value" | "to_vec" | "to_string");
        if imported_alias || module_member {
            self.violations
                .push(format!("forbidden serde_json path: {}", path_text(path)));
        }
    }

    fn inspect_macro(&mut self, item: &Macro) {
        let invokes_include = item.path.segments.last().is_some_and(|segment| {
            segment.ident == "include"
                || self
                    .aliases
                    .include_aliases
                    .contains(&segment.ident.to_string())
        });
        let passes_include_through_tokens = item
            .tokens
            .to_string()
            .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
            .any(|token| token == "include");
        if invokes_include || passes_include_through_tokens {
            self.violations.push("include! is forbidden".into());
        }
    }
}

impl<'ast> Visit<'ast> for SecurityVisitor<'_> {
    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        self.inspect_attribute(attribute);
        syn::visit::visit_attribute(self, attribute);
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        self.inspect_deserializer_name(&call.method.to_string());
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_path(&mut self, path: &'ast ExprPath) {
        if let Some(segment) = path.path.segments.last() {
            self.inspect_deserializer_name(&segment.ident.to_string());
        }
        self.inspect_json_path(&path.path);
        syn::visit::visit_expr_path(self, path);
    }

    fn visit_type_path(&mut self, path: &'ast TypePath) {
        self.inspect_json_path(&path.path);
        syn::visit::visit_type_path(self, path);
    }

    fn visit_macro(&mut self, item: &'ast Macro) {
        self.inspect_macro(item);
        syn::visit::visit_macro(self, item);
    }
}

fn path_text(path: &SynPath) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn has_conditional_compilation(attributes: &[Attribute]) -> bool {
    attributes
        .iter()
        .any(|attribute| meta_introduces_cfg(&attribute.meta))
}

fn meta_introduces_cfg(meta: &Meta) -> bool {
    if meta.path().is_ident("cfg") {
        return true;
    }
    if !meta.path().is_ident("cfg_attr") {
        return false;
    }
    let Meta::List(list) = meta else {
        return true;
    };
    let Ok(nested) = list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated) else {
        return true;
    };
    nested.iter().skip(1).any(meta_introduces_cfg)
}

fn required_enum<'a>(file: &'a syn::File, name: &str) -> &'a ItemEnum {
    file.items
        .iter()
        .find_map(|item| match item {
            Item::Enum(item) if item.ident == name => Some(item),
            _ => None,
        })
        .unwrap_or_else(|| panic!("required reducer enum is missing: {name}"))
}

fn assert_variants_unconditional(item: &ItemEnum, required: &[&str]) {
    for name in required {
        let variant = item
            .variants
            .iter()
            .find(|variant| variant.ident == *name)
            .unwrap_or_else(|| panic!("required reducer variant is missing: {name}"));
        assert!(
            !has_conditional_compilation(&variant.attrs),
            "reducer variant became conditionally compiled: {name}"
        );
    }
}

fn required_method_is_unconditional(file: &syn::File, type_name: &str, name: &str) -> bool {
    file.items
        .iter()
        .filter_map(|item| match item {
            Item::Impl(item) => Some(item),
            _ => None,
        })
        .filter(|item| {
            matches!(
                item.self_ty.as_ref(),
                syn::Type::Path(path)
                    if path.path.segments.last().is_some_and(|segment| segment.ident == type_name)
            )
        })
        .find_map(|implementation| {
            implementation.items.iter().find_map(|item| match item {
                ImplItem::Fn(method) if method.sig.ident == name => Some(
                    !has_conditional_compilation(&implementation.attrs)
                        && !has_conditional_compilation(&method.attrs),
                ),
                _ => None,
            })
        })
        .unwrap_or_else(|| panic!("required reducer method is missing: {type_name}::{name}"))
}

fn meta_contains_attribute(meta: &Meta, name: &str) -> bool {
    if meta.path().is_ident(name) {
        return true;
    }
    let Meta::List(list) = meta else {
        return false;
    };
    if !list.path.is_ident("cfg_attr") {
        return false;
    }
    list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
        .map_or(true, |nested| {
            nested
                .iter()
                .skip(1)
                .any(|meta| meta_contains_attribute(meta, name))
        })
}

fn source_has_conditional_compilation(source: &str) -> bool {
    struct ConditionalCompilationVisitor {
        found: bool,
    }

    impl<'ast> Visit<'ast> for ConditionalCompilationVisitor {
        fn visit_attribute(&mut self, attribute: &'ast Attribute) {
            self.found |= meta_introduces_cfg(&attribute.meta);
            syn::visit::visit_attribute(self, attribute);
        }
    }

    let file = parse_file(source).expect("mutated reducer source parses");
    let mut visitor = ConditionalCompilationVisitor { found: false };
    visitor.visit_file(&file);
    visitor.found
}

struct PendingModule {
    path: PathBuf,
    module_dir: PathBuf,
    guard_reducer_types: bool,
}

fn production_sources() -> Vec<(String, String, bool)> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("src/host_vault.rs");
    collect_production_sources(&root, &manifest.join("src/host_vault"))
}

fn collect_production_sources(root: &Path, root_module_dir: &Path) -> Vec<(String, String, bool)> {
    let root = fs::canonicalize(root).expect("Host-vault root exists");
    let root_module_dir = fs::canonicalize(root_module_dir).expect("Host-vault module directory");
    let mut pending = VecDeque::from([PendingModule {
        path: root.clone(),
        module_dir: root_module_dir.clone(),
        guard_reducer_types: false,
    }]);
    let mut visited = BTreeSet::new();
    let mut sources = Vec::new();

    while let Some(module) = pending.pop_front() {
        let path = fs::canonicalize(&module.path).expect("production module exists");
        assert!(
            visited.insert(path.clone()),
            "production module path was loaded more than once: {}",
            path.display()
        );
        let source = fs::read_to_string(&path).expect("production module is readable UTF-8");
        let file = parse_file(&source).expect("production Rust parses");
        enqueue_external_modules(
            &file.items,
            &path,
            &module.module_dir,
            module.guard_reducer_types,
            &mut pending,
        );
        let name = if path == root {
            root.file_name()
                .expect("Host-vault root file name")
                .to_string_lossy()
                .into_owned()
        } else {
            path.strip_prefix(&root_module_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/")
        };
        sources.push((name, source, module.guard_reducer_types));
    }

    sources.sort_by(|left, right| left.0.cmp(&right.0));
    sources
}

fn enqueue_external_modules(
    items: &[Item],
    source_path: &Path,
    module_dir: &Path,
    guard_reducer_types: bool,
    pending: &mut VecDeque<PendingModule>,
) {
    for item in items {
        if let Item::Macro(item) = item
            && item.ident.is_none()
        {
            if !attributes_are_test_only(&item.attrs) {
                panic!("item-position macro invocations are forbidden in production modules");
            }
            continue;
        }
        let Item::Mod(module) = item else {
            continue;
        };
        assert!(
            !has_conditional_module_path(module),
            "conditional module paths are forbidden: {}",
            module.ident
        );
        if is_test_only(module) {
            continue;
        }
        let child_guards_reducer = guard_reducer_types || module.ident == "reducer";
        if let Some((_, nested)) = &module.content {
            enqueue_external_modules(
                nested,
                source_path,
                &module_dir.join(module.ident.to_string()),
                child_guards_reducer,
                pending,
            );
            continue;
        }
        let mut child = resolve_external_module(module, source_path, module_dir);
        child.guard_reducer_types = child_guards_reducer;
        pending.push_back(child);
    }
}

fn has_conditional_module_path(module: &ItemMod) -> bool {
    module.attrs.iter().any(|attribute| {
        attribute.path().is_ident("cfg_attr") && meta_contains_attribute(&attribute.meta, "path")
    })
}

fn is_test_only(module: &ItemMod) -> bool {
    attributes_are_test_only(&module.attrs)
}

fn attributes_are_test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<SynPath>()
                .is_ok_and(|condition| condition.is_ident("test"))
    })
}

fn resolve_external_module(
    module: &ItemMod,
    source_path: &Path,
    module_dir: &Path,
) -> PendingModule {
    if let Some(relative) = explicit_module_path(module) {
        let path = source_path
            .parent()
            .expect("module source directory")
            .join(relative);
        let child_dir = module_directory_for_file(&path);
        return PendingModule {
            path,
            module_dir: child_dir,
            guard_reducer_types: false,
        };
    }

    let name = module.ident.to_string();
    let file_path = module_dir.join(format!("{name}.rs"));
    let mod_path = module_dir.join(&name).join("mod.rs");
    match (file_path.is_file(), mod_path.is_file()) {
        (true, false) => PendingModule {
            path: file_path,
            module_dir: module_dir.join(name),
            guard_reducer_types: false,
        },
        (false, true) => PendingModule {
            path: mod_path,
            module_dir: module_dir.join(name),
            guard_reducer_types: false,
        },
        (true, true) => panic!("ambiguous external module: {name}"),
        (false, false) => panic!("missing external module: {name}"),
    }
}

fn explicit_module_path(module: &ItemMod) -> Option<PathBuf> {
    module
        .attrs
        .iter()
        .find(|attribute| attribute.path().is_ident("path"))
        .map(|attribute| match &attribute.meta {
            Meta::NameValue(name_value) => match &name_value.value {
                Expr::Lit(expression) => match &expression.lit {
                    Lit::Str(path) => PathBuf::from(path.value()),
                    _ => panic!("module path must be a string literal"),
                },
                _ => panic!("module path must be a string literal"),
            },
            _ => panic!("module path must use name-value syntax"),
        })
}

fn module_directory_for_file(path: &Path) -> PathBuf {
    if path.file_name().is_some_and(|name| name == "mod.rs") {
        return path
            .parent()
            .expect("module source directory")
            .to_path_buf();
    }
    path.parent()
        .expect("module source directory")
        .join(path.file_stem().expect("module source file stem"))
}

fn audit_source(source: &str, inspect_deserializers: bool) -> Vec<String> {
    let file = parse_file(source).expect("production Rust parses");
    let mut aliases = AliasCollector::default();
    aliases.json_modules.insert("serde_json".into());
    aliases.include_aliases.insert("include".into());
    aliases.visit_file(&file);
    let mut visitor = SecurityVisitor {
        aliases: &aliases,
        inspect_deserializers,
        violations: Vec::new(),
    };
    if aliases.has_glob {
        visitor
            .violations
            .push("wildcard imports are forbidden in vault production source".into());
    }
    visitor
        .violations
        .extend(aliases.forbidden_alias_declarations.iter().cloned());
    for alias in &aliases.forbidden_json_aliases {
        visitor
            .violations
            .push(format!("forbidden serde_json import: {alias}"));
    }
    visitor.visit_file(&file);
    visitor.violations
}

#[test]
fn production_vault_sources_pass_ast_and_static_security_gates() {
    let sources = production_sources();
    let names = sources
        .iter()
        .map(|(name, _, _)| name.as_str())
        .collect::<BTreeSet<_>>();
    assert!(
        names.contains("reducer.rs"),
        "production reducer was omitted"
    );
    assert!(
        names.contains("model/serializer.rs"),
        "production serializer was omitted"
    );

    for (name, source, guard_reducer_types) in sources {
        let inspect_deserializers = name.starts_with("model/parser");
        let mut violations = audit_source(&source, inspect_deserializers);
        if guard_reducer_types {
            violations.extend(guarded_type_violations(&source, name == "reducer.rs"));
        }
        assert!(
            violations.is_empty(),
            "AST security violation in {name}: {violations:?}"
        );
        for token in FORBIDDEN_SOURCE_TOKENS {
            assert!(
                !source.contains(token),
                "forbidden vault source token in {name}: {token}"
            );
        }
    }
}

#[path = "source_audit_tests/mutation_tests.rs"]
mod mutation_tests;

#[test]
fn production_module_walk_follows_path_and_skips_test_modules() {
    let temp = tempfile::TempDir::new().expect("temporary source tree");
    let module_dir = temp.path().join("host_vault");
    fs::create_dir(&module_dir).expect("module directory");
    let root = temp.path().join("host_vault.rs");
    fs::write(
        &root,
        "#[path = \"host_vault/alternate.rs\"] mod included;\n#[cfg(test)] mod skipped;\n",
    )
    .expect("root source");
    fs::write(module_dir.join("alternate.rs"), "struct Included;\n").expect("alternate module");

    let sources = collect_production_sources(&root, &module_dir);
    assert_eq!(
        sources
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["alternate.rs", "host_vault.rs"]
    );
}

#[test]
fn conditional_module_paths_are_rejected_before_walking_defaults() {
    let temp = tempfile::TempDir::new().expect("temporary source tree");
    let module_dir = temp.path().join("host_vault");
    fs::create_dir(&module_dir).expect("module directory");
    let root = temp.path().join("host_vault.rs");
    fs::write(
        &root,
        "#[cfg_attr(not(test), path = \"host_vault/attack.rs\")] mod included;\n",
    )
    .expect("root source");
    fs::write(module_dir.join("included.rs"), "struct Default;\n").expect("default module");
    fs::write(module_dir.join("attack.rs"), "struct Attack;\n").expect("alternate module");

    assert!(
        std::panic::catch_unwind(|| collect_production_sources(&root, &module_dir)).is_err(),
        "conditional module path silently walked the default module"
    );
}

#[test]
fn cfg_hiding_mutations_are_rejected() {
    for mutation in [
        "#[cfg(test)] struct SecretInput;",
        "enum VaultCommand { #[cfg(test)] Insert }",
        "struct SecretInput; impl SecretInput { #[cfg_attr(all(), cfg(test))] fn new() {} }",
    ] {
        assert!(
            source_has_conditional_compilation(mutation),
            "conditional compilation bypassed source audit: {mutation}"
        );
    }
    assert!(
        !source_has_conditional_compilation(
            "#[cfg_attr(not(test), expect(dead_code, reason = \"future production caller\"))] struct Pending;"
        ),
        "dead-code expectation was mistaken for conditional compilation"
    );

    let enclosing_impl =
        parse_file("struct SecretInput; #[cfg(test)] impl SecretInput { fn new() {} }")
            .expect("mutated reducer source parses");
    assert!(
        !required_method_is_unconditional(&enclosing_impl, "SecretInput", "new"),
        "conditional impl bypassed required-method audit"
    );
}

#[test]
fn reducer_module_and_command_items_remain_private() {
    let root = parse_file(include_str!("../host_vault.rs")).expect("Host-vault root parses");
    let reducer_module = root
        .items
        .iter()
        .find_map(|item| match item {
            Item::Mod(module) if module.ident == "reducer" => Some(module),
            _ => None,
        })
        .expect("reducer module");
    assert!(matches!(reducer_module.vis, Visibility::Inherited));
    assert!(
        reducer_module.content.is_none(),
        "production reducer must remain an externally audited module"
    );
    assert!(
        !reducer_module
            .attrs
            .iter()
            .any(|attribute| matches!(attribute.path().get_ident(), Some(name) if name == "cfg" || name == "cfg_attr")),
        "production reducer became conditionally compiled"
    );

    let model_module = root
        .items
        .iter()
        .find_map(|item| match item {
            Item::Mod(module) if module.ident == "model" => Some(module),
            _ => None,
        })
        .expect("model module");
    assert!(
        !model_module
            .attrs
            .iter()
            .any(|attribute| matches!(attribute.path().get_ident(), Some(name) if name == "cfg" || name == "cfg_attr")),
        "production model became conditionally compiled"
    );
    let model = parse_file(include_str!("model.rs")).expect("model parses");
    let serializer_module = model
        .items
        .iter()
        .find_map(|item| match item {
            Item::Mod(module) if module.ident == "serializer" => Some(module),
            _ => None,
        })
        .expect("serializer module");
    assert!(matches!(serializer_module.vis, Visibility::Inherited));
    assert!(
        !serializer_module
            .attrs
            .iter()
            .any(|attribute| matches!(attribute.path().get_ident(), Some(name) if name == "cfg" || name == "cfg_attr")),
        "production serializer became conditionally compiled"
    );

    let reducer = parse_file(include_str!("reducer.rs")).expect("reducer parses");
    let required = [
        "BindApproval",
        "CredentialDescriptor",
        "CredentialTarget",
        "ExpectedVaultState",
        "GrantApproval",
        "GrantKey",
        "MutationResult",
        "PersistedBinding",
        "SecretInput",
        "VaultCommand",
        "initialize_empty",
        "persist_command",
    ];
    let mut found = BTreeSet::new();
    for item in &reducer.items {
        let named = match item {
            Item::Enum(item) => Some((item.ident.to_string(), &item.vis, item.attrs.as_slice())),
            Item::Fn(item) => Some((item.sig.ident.to_string(), &item.vis, item.attrs.as_slice())),
            Item::Struct(item) => Some((item.ident.to_string(), &item.vis, item.attrs.as_slice())),
            _ => None,
        };
        if let Some((name, visibility, attributes)) = named {
            if name == "initialize_empty" {
                assert!(
                    matches!(visibility, Visibility::Restricted(restricted) if restricted.path.is_ident("super")),
                    "initializer bridge escaped Host-vault"
                );
            } else {
                assert!(
                    matches!(visibility, Visibility::Inherited),
                    "reducer item became visible: {name}"
                );
            }
            if required.contains(&name.as_str()) {
                assert!(
                    !has_conditional_compilation(attributes),
                    "required reducer item became conditionally compiled: {name}"
                );
                found.insert(name);
            }
        }
    }
    assert_eq!(found, required.into_iter().map(str::to_owned).collect());

    assert_variants_unconditional(
        required_enum(&reducer, "ExpectedVaultState"),
        &["Absent", "Present"],
    );
    assert_variants_unconditional(
        required_enum(&reducer, "VaultCommand"),
        &[
            "InitializeEmpty",
            "Insert",
            "Rotate",
            "Revoke",
            "Bind",
            "Rebind",
            "Unbind",
        ],
    );
    assert!(
        required_method_is_unconditional(&reducer, "SecretInput", "new"),
        "SecretInput::new became conditionally compiled"
    );
}
