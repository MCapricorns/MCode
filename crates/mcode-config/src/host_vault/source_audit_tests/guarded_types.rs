//! Audits protected Host-vault reducer types across module boundaries.

use std::collections::BTreeSet;

use syn::visit::Visit;
use syn::{
    Attribute, ImplItem, ImplItemType, ItemEnum, ItemUse, Path as SynPath, UseTree, Visibility,
    parse_file,
};

use super::meta_contains_attribute;

const GUARDED_TYPES: &[&str] = &["SecretInput", "VaultCommand"];
const GUARDED_METHODS: &[(&str, &str)] = &[
    ("SecretInput", "new"),
    ("VaultCommand", "can_create"),
    ("VaultCommand", "expected"),
];

struct GuardedTypeVisitor {
    require_method_whitelist: bool,
    found_methods: Vec<(String, String)>,
    violations: Vec<String>,
}

impl GuardedTypeVisitor {
    fn inspect_declaration(&mut self, name: &str, attributes: &[Attribute]) {
        if GUARDED_TYPES.contains(&name)
            && attributes
                .iter()
                .any(|attribute| meta_contains_attribute(&attribute.meta, "derive"))
        {
            self.violations
                .push(format!("derive is forbidden on {name}"));
        }
    }

    fn inspect_implementation(&mut self, implementation: &syn::ItemImpl) {
        let syn::Type::Path(self_type) = implementation.self_ty.as_ref() else {
            return;
        };
        let Some(type_name) = guarded_path_name(&self_type.path) else {
            return;
        };
        if !self.require_method_whitelist {
            self.violations.push(format!(
                "guarded implementation outside reducer root: {type_name}"
            ));
        }
        if implementation.trait_.is_some() {
            self.violations
                .push(format!("trait impl is forbidden for {type_name}"));
            return;
        }
        for item in &implementation.items {
            let ImplItem::Fn(method) = item else {
                self.violations
                    .push(format!("non-method inherent item on {type_name}"));
                continue;
            };
            let method_name = method.sig.ident.to_string();
            if !GUARDED_METHODS.contains(&(type_name, method_name.as_str())) {
                self.violations
                    .push(format!("extra inherent method: {type_name}::{method_name}"));
            }
            if !matches!(method.vis, Visibility::Inherited) {
                self.violations.push(format!(
                    "guarded method became visible: {type_name}::{method_name}"
                ));
            }
            self.found_methods.push((type_name.to_owned(), method_name));
        }
    }
}

impl<'ast> Visit<'ast> for GuardedTypeVisitor {
    fn visit_item_enum(&mut self, item: &'ast ItemEnum) {
        self.inspect_declaration(&item.ident.to_string(), &item.attrs);
        syn::visit::visit_item_enum(self, item);
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        self.inspect_implementation(item);
        syn::visit::visit_item_impl(self, item);
    }

    fn visit_impl_item_type(&mut self, item: &'ast ImplItemType) {
        if matches!(&item.ty, syn::Type::Path(path) if guarded_path_name(&path.path).is_some()) {
            self.violations.push(format!(
                "guarded associated type alias is forbidden: {}",
                item.ident
            ));
        }
        syn::visit::visit_impl_item_type(self, item);
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        self.inspect_declaration(&item.ident.to_string(), &item.attrs);
        syn::visit::visit_item_struct(self, item);
    }

    fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
        if matches!(item.ty.as_ref(), syn::Type::Path(path) if guarded_path_name(&path.path).is_some())
        {
            self.violations
                .push(format!("guarded type alias is forbidden: {}", item.ident));
        }
        syn::visit::visit_item_type(self, item);
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        collect_guarded_imports(&item.tree, &mut self.violations);
        syn::visit::visit_item_use(self, item);
    }
}

fn guarded_path_name(path: &SynPath) -> Option<&str> {
    path.segments
        .last()
        .map(|segment| segment.ident.to_string())
        .filter(|name| GUARDED_TYPES.contains(&name.as_str()))
        .map(|name| {
            GUARDED_TYPES
                .iter()
                .copied()
                .find(|guarded| *guarded == name)
                .expect("filtered guarded type exists")
        })
}

fn collect_guarded_imports(tree: &UseTree, violations: &mut Vec<String>) {
    match tree {
        UseTree::Path(path) => collect_guarded_imports(&path.tree, violations),
        UseTree::Name(name) if GUARDED_TYPES.contains(&name.ident.to_string().as_str()) => {
            violations.push(format!("guarded type import is forbidden: {}", name.ident));
        }
        UseTree::Rename(rename) if GUARDED_TYPES.contains(&rename.ident.to_string().as_str()) => {
            violations.push(format!(
                "guarded type alias is forbidden: {}",
                rename.rename
            ));
        }
        UseTree::Group(group) => {
            for item in &group.items {
                collect_guarded_imports(item, violations);
            }
        }
        UseTree::Name(_) | UseTree::Rename(_) | UseTree::Glob(_) => {}
    }
}

pub(super) fn guarded_type_violations(source: &str, require_method_whitelist: bool) -> Vec<String> {
    let file = parse_file(source).expect("reducer source parses");
    let mut visitor = GuardedTypeVisitor {
        require_method_whitelist,
        found_methods: Vec::new(),
        violations: Vec::new(),
    };
    visitor.visit_file(&file);

    if require_method_whitelist {
        let expected = GUARDED_METHODS
            .iter()
            .map(|(type_name, method)| ((*type_name).to_owned(), (*method).to_owned()))
            .collect::<BTreeSet<_>>();
        let found = visitor
            .found_methods
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if found != expected || visitor.found_methods.len() != GUARDED_METHODS.len() {
            visitor
                .violations
                .push("guarded method whitelist changed".into());
        }
    }
    visitor.violations
}
