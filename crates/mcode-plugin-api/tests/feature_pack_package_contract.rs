//! Aggregate sole-current FeaturePack package contract tests.

use std::collections::BTreeSet;
use std::path::Path;

use wit_parser::{Resolve, WorldItem, WorldKey};

const PACKAGE: &str = "mcode:feature-pack@0.0.1";
const WORLD_CONTRACTS: [(&str, Option<&str>, &str); 3] = [
    ("mcp", Some("mcp-host"), "mcp-pack"),
    ("usage", Some("usage-host"), "usage-pack"),
    ("web", Some("web-host"), "web-pack"),
];

#[test]
fn feature_pack_sources_resolve_as_one_exact_package() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("wit/feature-pack");
    let mut resolve = Resolve::default();
    let (package_id, _) = resolve
        .push_dir(path)
        .expect("FeaturePack WIT directory must resolve as one package");

    assert_eq!(resolve.packages.len(), 1);
    assert_eq!(resolve.package_names.len(), 1);
    let package = &resolve.packages[package_id];
    assert_eq!(package.name.to_string(), PACKAGE);

    let expected_worlds = WORLD_CONTRACTS
        .iter()
        .map(|(world, _, _)| *world)
        .collect::<BTreeSet<_>>();
    assert_eq!(expected_worlds.len(), 3);
    assert_eq!(
        package
            .worlds
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        expected_worlds
    );

    let mut expected_interfaces = BTreeSet::new();
    for (_, host, pack) in WORLD_CONTRACTS {
        if let Some(host) = host {
            assert!(expected_interfaces.insert(host));
        }
        assert!(expected_interfaces.insert(pack));
    }
    assert_eq!(expected_interfaces.len(), 6);
    assert_eq!(
        package
            .interfaces
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        expected_interfaces
    );

    for (family, host, pack) in WORLD_CONTRACTS {
        let world = &resolve.worlds[package.worlds[family]];
        match host {
            Some(host) => {
                assert_eq!(world.imports.len(), 1, "{family} import count");
                assert_interface_item(
                    world.imports.first().expect("host import"),
                    package.interfaces[host],
                );
            }
            None => assert!(world.imports.is_empty(), "{family} must be zero-import"),
        }
        assert_eq!(world.exports.len(), 1, "{family} export count");
        assert_interface_item(
            world.exports.first().expect("pack export"),
            package.interfaces[pack],
        );
    }
}

fn assert_interface_item(item: (&WorldKey, &WorldItem), expected: wit_parser::InterfaceId) {
    assert_eq!(item.0, &WorldKey::Interface(expected));
    let WorldItem::Interface { id, .. } = item.1 else {
        panic!("world item must be an interface");
    };
    assert_eq!(*id, expected);
}
