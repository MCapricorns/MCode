use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;

use mcode_config::{
    AccessControlEvidence, ArtifactRef, AuthorityRevision, BundlePath, CanonicalVersion,
    ConfigErrorKind, HomeLayout, InventoryEntry, MAX_PACK_INSTALLATION_BYTES, OwnedKind, PackId,
    PackInstallation, PluginFamily, Sha256Digest, SourceBindingId, TrustHighWater,
    probe_access_control, read_pack_installation, replace_pack_installation,
};
use serde_json::{Value, json};

const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const OTHER_DIGEST: &str =
    "sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

fn layout() -> (tempfile::TempDir, HomeLayout) {
    let parent = tempfile::tempdir().expect("temporary parent");
    let home = HomeLayout::from_root(parent.path().join("home")).expect("valid layout");
    (parent, home)
}

fn pack_id(value: &str) -> PackId {
    PackId::parse(value).expect("valid Pack ID")
}

fn revision(value: u64) -> AuthorityRevision {
    AuthorityRevision::new(value).expect("valid revision")
}

fn digest(value: &str) -> Sha256Digest {
    Sha256Digest::parse(value).expect("valid digest")
}

fn artifact(version: &str, digest_value: &str) -> ArtifactRef {
    ArtifactRef::new(
        CanonicalVersion::parse(version).expect("canonical version"),
        digest(digest_value),
    )
}

fn installation(family: PluginFamily, id: &PackId) -> PackInstallation {
    PackInstallation::new(
        family,
        id.clone(),
        SourceBindingId::parse("official-source").expect("source"),
        artifact("1.2.3-alpha.1+build.7", DIGEST),
        TrustHighWater::new(7, digest(OTHER_DIGEST)).expect("high-water"),
        vec![
            InventoryEntry::new(
                BundlePath::parse("bin/main.wasm").expect("path"),
                digest(DIGEST),
            ),
            InventoryEntry::new(
                BundlePath::parse("themes/dark.json").expect("path"),
                digest(OTHER_DIGEST),
            ),
        ],
    )
    .expect("installation")
}

#[test]
fn executable_digest_comes_only_from_the_canonical_inventory_row() {
    let id = pack_id("runtime");
    let executable = PackInstallation::new(
        PluginFamily::Ui,
        id.clone(),
        SourceBindingId::parse("official-source").expect("source"),
        artifact("1.0.0", DIGEST),
        TrustHighWater::new(7, digest(DIGEST)).expect("high-water"),
        vec![
            InventoryEntry::new(
                BundlePath::parse("component.wasm").expect("component path"),
                digest(OTHER_DIGEST),
            ),
            InventoryEntry::new(
                BundlePath::parse("themes/dark.json").expect("theme path"),
                digest(DIGEST),
            ),
        ],
    )
    .expect("executable installation");
    assert_eq!(
        executable.component_digest().map(Sha256Digest::as_str),
        Some(OTHER_DIGEST)
    );

    let declarative = PackInstallation::new(
        PluginFamily::Ui,
        id,
        SourceBindingId::parse("official-source").expect("source"),
        artifact("1.0.0", DIGEST),
        TrustHighWater::new(7, digest(DIGEST)).expect("high-water"),
        vec![InventoryEntry::new(
            BundlePath::parse("themes/dark.json").expect("theme path"),
            digest(OTHER_DIGEST),
        )],
    )
    .expect("declarative installation");
    assert!(declarative.component_digest().is_none());
}

fn target(home: &HomeLayout, family: PluginFamily, id: &PackId) -> std::path::PathBuf {
    home.pack_installation_json(family, id.as_str())
        .expect("target")
}

fn create_value(home: &HomeLayout, family: PluginFamily, id: &PackId) -> Value {
    replace_pack_installation(
        home,
        family,
        id,
        AuthorityRevision::ABSENT,
        &installation(family, id),
    )
    .expect("initial installation");
    serde_json::from_slice(&fs::read(target(home, family, id)).expect("authority bytes"))
        .expect("authority JSON")
}

fn write_value(home: &HomeLayout, family: PluginFamily, id: &PackId, value: &Value) {
    fs::write(
        target(home, family, id),
        serde_json::to_vec(value).expect("fixture JSON"),
    )
    .expect("write fixture");
}

fn assert_invalid(home: &HomeLayout, family: PluginFamily, id: &PackId, value: &Value) {
    write_value(home, family, id, value);
    let error = read_pack_installation(home, family, id).expect_err("authority must fail");
    assert_eq!(error.kind(), ConfigErrorKind::AuthorityValidation);
    assert_eq!(error.path(), Some(target(home, family, id).as_path()));
}

#[test]
fn canonical_round_trip_binds_family_and_pack_path() {
    let (_parent, home) = layout();
    for (family, raw_id) in [
        (PluginFamily::Providers, "official-openai"),
        (PluginFamily::Ui, "theme.dark-2"),
    ] {
        let id = pack_id(raw_id);
        let value = installation(family, &id);
        let document =
            replace_pack_installation(&home, family, &id, AuthorityRevision::ABSENT, &value)
                .expect("write authority");
        assert_eq!(document.revision().get(), 1);
        assert_eq!(document.installation(), &value);
        assert_eq!(
            read_pack_installation(&home, family, &id)
                .expect("read")
                .expect("present"),
            document
        );
        let expected = format!(
            "{{\"formatVersion\":1,\"kind\":\"mcode-pack-installation\",\"revision\":1,\"family\":\"{}\",\"packId\":\"{raw_id}\",\"source\":\"official-source\",\"selected\":{{\"version\":\"1.2.3-alpha.1+build.7\",\"digest\":\"{DIGEST}\"}},\"trustHighWater\":{{\"sequence\":7,\"manifestDigest\":\"{OTHER_DIGEST}\"}},\"inventory\":[{{\"path\":\"bin/main.wasm\",\"digest\":\"{DIGEST}\"}},{{\"path\":\"themes/dark.json\",\"digest\":\"{OTHER_DIGEST}\"}}]}}\n",
            family.directory_name()
        );
        assert_eq!(
            fs::read(target(&home, family, &id)).expect("bytes"),
            expected.as_bytes()
        );
    }
}

#[test]
fn missing_read_creates_nothing() {
    let (_parent, home) = layout();
    assert_eq!(
        read_pack_installation(&home, PluginFamily::Usage, &pack_id("questions"))
            .expect("missing read"),
        None
    );
    assert!(!home.root().exists());
}

fn assert_private_file(path: &std::path::Path) {
    #[cfg(unix)]
    assert_eq!(
        probe_access_control(path),
        AccessControlEvidence::UnixMode {
            kind: OwnedKind::File,
            mode: 0o600,
        }
    );
    #[cfg(windows)]
    assert!(matches!(
        probe_access_control(path),
        AccessControlEvidence::WindowsProtectedDacl {
            kind: OwnedKind::File,
            owner_allowed: true,
            owner_current_user: true,
            current_user: true,
            system: true,
            protected: true,
            ace_count: 1 | 2,
            extra_aces: 0,
            ..
        }
    ));
}

#[test]
fn mutation_creates_only_exact_pack_ancestors_target_and_lock() {
    let (_parent, home) = layout();
    let id = pack_id("web-search");
    replace_pack_installation(
        &home,
        PluginFamily::Web,
        &id,
        AuthorityRevision::ABSENT,
        &installation(PluginFamily::Web, &id),
    )
    .expect("write authority");

    let pack = home.pack_dir(PluginFamily::Web, id.as_str()).expect("pack");
    let authority = pack.join("installation.json");
    let lock = pack.join("installation.json.lock");
    assert_private_file(&authority);
    assert_private_file(&lock);
    assert_eq!(fs::read_dir(&pack).expect("pack listing").count(), 2);
    assert!(
        !home
            .pack_data_dir(PluginFamily::Web, id.as_str())
            .expect("data")
            .exists()
    );
    assert!(
        !home
            .pack_versions_dir(PluginFamily::Web, id.as_str())
            .expect("versions")
            .exists()
    );
    for family in PluginFamily::ALL {
        if family != PluginFamily::Web {
            assert!(!home.plugin_dir(family).exists());
        }
    }
}

#[test]
fn persisted_and_supplied_identity_must_match_path() {
    let (_parent, home) = layout();
    let id = pack_id("one");
    let mut value = create_value(&home, PluginFamily::Web, &id);
    for (field, wrong) in [("family", json!("mcp")), ("packId", json!("two"))] {
        let mut changed = value.clone();
        changed[field] = wrong;
        assert_invalid(&home, PluginFamily::Web, &id, &changed);
        value = create_value_after_fixture(&home, PluginFamily::Web, &id);
    }

    let before = fs::read(target(&home, PluginFamily::Web, &id)).expect("before");
    let other_id = pack_id("other");
    let error = replace_pack_installation(
        &home,
        PluginFamily::Web,
        &id,
        revision(1),
        &installation(PluginFamily::Web, &other_id),
    )
    .expect_err("supplied Pack mismatch");
    assert_eq!(error.kind(), ConfigErrorKind::AuthorityValidation);
    assert_eq!(
        fs::read(target(&home, PluginFamily::Web, &id)).expect("after"),
        before
    );
}

fn create_value_after_fixture(home: &HomeLayout, family: PluginFamily, id: &PackId) -> Value {
    let canonical = create_value_shape(family, id, 1);
    write_value(home, family, id, &canonical);
    canonical
}

fn create_value_shape(family: PluginFamily, id: &PackId, revision: u64) -> Value {
    json!({
        "formatVersion": 1,
        "kind": "mcode-pack-installation",
        "revision": revision,
        "family": family.directory_name(),
        "packId": id.as_str(),
        "source": "official-source",
        "selected": {"version": "1.2.3-alpha.1+build.7", "digest": DIGEST},
        "trustHighWater": {"sequence": 7, "manifestDigest": OTHER_DIGEST},
        "inventory": [
            {"path": "bin/main.wasm", "digest": DIGEST},
            {"path": "themes/dark.json", "digest": OTHER_DIGEST}
        ]
    })
}

#[test]
fn envelope_nested_and_inventory_objects_are_exact_and_typed() {
    let (_parent, home) = layout();
    let id = pack_id("strict");
    let base = create_value(&home, PluginFamily::Providers, &id);
    let root_fields = [
        "formatVersion",
        "kind",
        "revision",
        "family",
        "packId",
        "source",
        "selected",
        "trustHighWater",
        "inventory",
    ];
    for field in root_fields {
        let mut value = base.clone();
        value.as_object_mut().expect("root").remove(field);
        assert_invalid(&home, PluginFamily::Providers, &id, &value);
    }
    for (field, wrong) in [
        ("formatVersion", json!("1")),
        ("kind", json!(1)),
        ("revision", json!(1.0)),
        ("family", json!(1)),
        ("packId", json!(1)),
        ("source", json!(1)),
        ("selected", json!([])),
        ("trustHighWater", json!([])),
        ("inventory", json!({})),
    ] {
        let mut value = base.clone();
        value[field] = wrong;
        assert_invalid(&home, PluginFamily::Providers, &id, &value);
    }
    for pointer in ["selected", "trustHighWater"] {
        let fields = if pointer == "selected" {
            ["version", "digest"]
        } else {
            ["sequence", "manifestDigest"]
        };
        for field in fields {
            let mut value = base.clone();
            value[pointer]
                .as_object_mut()
                .expect("nested")
                .remove(field);
            assert_invalid(&home, PluginFamily::Providers, &id, &value);
        }
        let mut extra = base.clone();
        extra[pointer]["extra"] = Value::Null;
        assert_invalid(&home, PluginFamily::Providers, &id, &extra);
    }
    for field in ["path", "digest"] {
        let mut missing = base.clone();
        missing["inventory"][0]
            .as_object_mut()
            .expect("entry")
            .remove(field);
        assert_invalid(&home, PluginFamily::Providers, &id, &missing);
        let mut wrong_type = base.clone();
        wrong_type["inventory"][0][field] = json!(1);
        assert_invalid(&home, PluginFamily::Providers, &id, &wrong_type);
    }
    for (object, field) in [
        ("selected", "version"),
        ("selected", "digest"),
        ("trustHighWater", "sequence"),
        ("trustHighWater", "manifestDigest"),
    ] {
        let mut value = base.clone();
        value[object][field] = Value::Null;
        assert_invalid(&home, PluginFamily::Providers, &id, &value);
    }
    let mut root_extra = base.clone();
    root_extra["extra"] = Value::Null;
    assert_invalid(&home, PluginFamily::Providers, &id, &root_extra);
    let mut entry_extra = base.clone();
    entry_extra["inventory"][0]["extra"] = Value::Null;
    assert_invalid(&home, PluginFamily::Providers, &id, &entry_extra);

    let raw = serde_json::to_string(&base).expect("base JSON");
    for duplicate in [
        raw.replacen("{", "{\"kind\":\"duplicate\",", 1),
        raw.replacen(
            "\"path\":\"bin/main.wasm\"",
            "\"path\":\"other\",\"path\":\"bin/main.wasm\"",
            1,
        ),
    ] {
        fs::write(target(&home, PluginFamily::Providers, &id), duplicate).expect("duplicate");
        assert_eq!(
            read_pack_installation(&home, PluginFamily::Providers, &id)
                .expect_err("duplicate rejection")
                .kind(),
            ConfigErrorKind::DuplicateKey
        );
    }
}

#[test]
fn bundle_path_rejects_unsafe_reserved_and_noncanonical_spellings() {
    let invalid = [
        "",
        "/absolute",
        "a//b",
        "a/",
        "./a",
        "../a",
        "a/../b",
        "a\\b",
        "C:/x",
        "a:b",
        "Upper/file",
        "a/UPPER",
        "a/$",
        "a/.hidden",
        "a/trailing.",
        "a/trailing-",
        "con",
        "aux.txt",
        "a/COM1.bin",
        "data",
        "data/file",
        "installation.json",
        "a/installation.json",
        "a/ installation",
        "a/space name",
        "a/\u{7f}",
    ];
    for value in invalid {
        assert!(BundlePath::parse(value).is_err(), "accepted {value:?}");
    }
    assert!(BundlePath::parse("a".repeat(513)).is_err());
    assert!(BundlePath::parse(format!("a/{}", "b".repeat(129))).is_err());
    assert!(
        BundlePath::parse(std::iter::repeat_n("a", 129).collect::<Vec<_>>().join("/")).is_err()
    );
    for valid in ["a", "bin/main.wasm", "themes/dark_2.json", "9/a-1"] {
        assert_eq!(BundlePath::parse(valid).expect("valid").as_str(), valid);
    }
}

#[test]
fn inventory_constructor_and_parser_require_bounded_strict_order() {
    let id = pack_id("inventory");
    let entry = || InventoryEntry::new(BundlePath::parse("a").expect("path"), digest(DIGEST));
    let make = |entries| {
        PackInstallation::new(
            PluginFamily::Usage,
            id.clone(),
            SourceBindingId::parse("source").expect("source"),
            artifact("1.0.0", DIGEST),
            TrustHighWater::new(1, digest(DIGEST)).expect("trust"),
            entries,
        )
    };
    assert!(make(vec![]).is_err());
    assert!(make(vec![entry(), entry()]).is_err());
    assert!(
        make(vec![
            InventoryEntry::new(BundlePath::parse("b").expect("path"), digest(DIGEST)),
            entry(),
        ])
        .is_err()
    );
    let too_many = (0..4097)
        .map(|index| {
            InventoryEntry::new(
                BundlePath::parse(format!("f/{index:04}")).expect("path"),
                digest(DIGEST),
            )
        })
        .collect();
    assert!(make(too_many).is_err());

    let (_parent, home) = layout();
    let base = create_value(&home, PluginFamily::Usage, &id);
    for inventory in [
        json!([]),
        json!([base["inventory"][0].clone(), base["inventory"][0].clone()]),
        json!([base["inventory"][1].clone(), base["inventory"][0].clone()]),
    ] {
        let mut value = base.clone();
        value["inventory"] = inventory;
        assert_invalid(&home, PluginFamily::Usage, &id, &value);
    }
    let mut value = base;
    value["inventory"] = Value::Array(
        (0..4097)
            .map(|index| json!({"path": format!("f/{index:04}"), "digest": DIGEST}))
            .collect(),
    );
    assert_invalid(&home, PluginFamily::Usage, &id, &value);
}

#[test]
fn source_artifact_digest_and_high_water_grammars_are_reused() {
    let (_parent, home) = layout();
    let id = pack_id("grammar");
    let base = create_value(&home, PluginFamily::Mcp, &id);
    for (pointer, invalid) in [
        ("source", json!("Bad-Source")),
        ("selected.version", json!("01.2.3")),
        ("selected.digest", json!("sha256:0123")),
        ("trustHighWater.sequence", json!(0)),
        ("trustHighWater.manifestDigest", json!("SHA256:bad")),
        ("inventory.0.digest", json!("sha256:abcd")),
    ] {
        let mut value = base.clone();
        set_pointer(&mut value, pointer, invalid);
        assert_invalid(&home, PluginFamily::Mcp, &id, &value);
    }
}

fn set_pointer(root: &mut Value, pointer: &str, value: Value) {
    let mut current = root;
    let parts = pointer.split('.').collect::<Vec<_>>();
    for part in &parts[..parts.len() - 1] {
        current = if let Ok(index) = part.parse::<usize>() {
            &mut current[index]
        } else {
            &mut current[*part]
        };
    }
    current[*parts.last().expect("last")] = value;
}

fn assert_same_revision_cas_round(
    home: &Arc<HomeLayout>,
    id: &Arc<PackId>,
    expected: AuthorityRevision,
    round: usize,
) {
    let barrier = Arc::new(Barrier::new(9));
    let handles = (0..8)
        .map(|_| {
            let home = Arc::clone(home);
            let id = Arc::clone(id);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                replace_pack_installation(
                    &home,
                    PluginFamily::Mcp,
                    &id,
                    expected,
                    &installation(PluginFamily::Mcp, &id),
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("worker"))
        .collect::<Vec<_>>();
    let successes = results.iter().filter(|result| result.is_ok()).count();
    let revision_conflicts = results
        .iter()
        .filter(|result| {
            result
                .as_ref()
                .is_err_and(|error| error.kind() == ConfigErrorKind::RevisionConflict)
        })
        .count();
    let error_outcomes = results
        .iter()
        .filter_map(|result| {
            result
                .as_ref()
                .err()
                .map(|error| (error.kind(), error.io_kind()))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        (successes, revision_conflicts),
        (1, 7),
        "CAS round {round} error outcomes: {error_outcomes:?}"
    );
}

#[test]
fn revisions_advance_stale_cas_preserves_and_concurrency_has_one_winner() {
    let (_parent, home) = layout();
    let id = pack_id("cas");
    let first = replace_pack_installation(
        &home,
        PluginFamily::Mcp,
        &id,
        AuthorityRevision::ABSENT,
        &installation(PluginFamily::Mcp, &id),
    )
    .expect("revision one");
    let second = replace_pack_installation(
        &home,
        PluginFamily::Mcp,
        &id,
        first.revision(),
        &installation(PluginFamily::Mcp, &id),
    )
    .expect("revision two");
    assert_eq!(second.revision().get(), 2);
    let before = fs::read(target(&home, PluginFamily::Mcp, &id)).expect("before");
    assert_eq!(
        replace_pack_installation(
            &home,
            PluginFamily::Mcp,
            &id,
            revision(1),
            &installation(PluginFamily::Mcp, &id)
        )
        .expect_err("stale")
        .kind(),
        ConfigErrorKind::RevisionConflict
    );
    assert_eq!(
        fs::read(target(&home, PluginFamily::Mcp, &id)).expect("after"),
        before
    );

    assert_same_revision_cas_round(&Arc::new(home), &Arc::new(id), revision(2), 1);
}

#[cfg(windows)]
#[test]
fn same_revision_cas_has_one_winner_for_thirty_rounds() {
    let (_parent, home) = layout();
    let home = Arc::new(home);
    let id = Arc::new(pack_id("cas-stress"));
    let first = replace_pack_installation(
        &home,
        PluginFamily::Mcp,
        &id,
        AuthorityRevision::ABSENT,
        &installation(PluginFamily::Mcp, &id),
    )
    .expect("revision one");

    for round in 1..=30 {
        let expected = revision(first.revision().get() + round - 1);
        assert_same_revision_cas_round(&home, &id, expected, round as usize);
    }
}

#[test]
fn exhausted_and_malformed_current_authority_preserve_target() {
    let (_parent, home) = layout();
    let id = pack_id("preserve");
    let mut value = create_value(&home, PluginFamily::Usage, &id);
    value["revision"] = json!(i64::MAX);
    write_value(&home, PluginFamily::Usage, &id, &value);
    let path = target(&home, PluginFamily::Usage, &id);
    let before = fs::read(&path).expect("before");
    assert_eq!(
        replace_pack_installation(
            &home,
            PluginFamily::Usage,
            &id,
            revision(i64::MAX as u64),
            &installation(PluginFamily::Usage, &id)
        )
        .expect_err("exhausted")
        .kind(),
        ConfigErrorKind::RevisionExhausted
    );
    assert_eq!(fs::read(&path).expect("after"), before);

    fs::write(&path, b"not JSON").expect("malformed");
    let before = fs::read(&path).expect("before malformed");
    assert_eq!(
        replace_pack_installation(
            &home,
            PluginFamily::Usage,
            &id,
            revision(1),
            &installation(PluginFamily::Usage, &id)
        )
        .expect_err("malformed")
        .kind(),
        ConfigErrorKind::InvalidJson
    );
    assert_eq!(fs::read(&path).expect("after malformed"), before);
}

#[test]
fn bounded_parser_rejects_oversized_deep_node_heavy_and_non_utf8() {
    let (_parent, home) = layout();
    let id = pack_id("bounds");
    create_value(&home, PluginFamily::Providers, &id);
    let path = target(&home, PluginFamily::Providers, &id);
    let fixtures = [
        (
            vec![b' '; MAX_PACK_INSTALLATION_BYTES + 1],
            ConfigErrorKind::Oversized,
        ),
        (
            format!("{}0{}", "[".repeat(10), "]".repeat(10)).into_bytes(),
            ConfigErrorKind::TooDeep,
        ),
        (
            format!(
                "[{}]",
                std::iter::repeat_n("null", 21_000)
                    .collect::<Vec<_>>()
                    .join(",")
            )
            .into_bytes(),
            ConfigErrorKind::TooManyNodes,
        ),
        (vec![b'{', 0xff, b'}'], ConfigErrorKind::NonUtf8),
    ];
    for (bytes, expected) in fixtures {
        fs::write(&path, bytes).expect("fixture");
        assert_eq!(
            read_pack_installation(&home, PluginFamily::Providers, &id)
                .expect_err("bounded rejection")
                .kind(),
            expected
        );
    }
}

#[test]
fn parser_errors_are_redacted_to_target_path_and_kind() {
    let (_parent, home) = layout();
    let id = pack_id("redaction");
    create_value(&home, PluginFamily::Usage, &id);
    let sentinel = "MEMBER-NAME-MUST-NOT-APPEAR-29e4";
    fs::write(
        target(&home, PluginFamily::Usage, &id),
        format!(r#"{{"{sentinel}":null,"{sentinel}":null}}"#),
    )
    .expect("fixture");
    let error = read_pack_installation(&home, PluginFamily::Usage, &id).expect_err("duplicate");
    assert_eq!(error.kind(), ConfigErrorKind::DuplicateKey);
    assert_eq!(
        error.path(),
        Some(target(&home, PluginFamily::Usage, &id).as_path())
    );
    assert!(!format!("{error:?}").contains(sentinel));
    assert!(!error.to_string().contains(sentinel));
}

#[test]
fn wrong_target_type_is_rejected() {
    let (_parent, home) = layout();
    let id = pack_id("wrong-type");
    fs::create_dir_all(target(&home, PluginFamily::Web, &id)).expect("directory target");
    let error = read_pack_installation(&home, PluginFamily::Web, &id).expect_err("wrong type");
    assert!(matches!(
        error.kind(),
        ConfigErrorKind::Io | ConfigErrorKind::AccessControl
    ));
}

#[cfg(unix)]
#[test]
fn final_symlink_is_rejected_without_touching_target() {
    use std::os::unix::fs::symlink;

    let (parent, home) = layout();
    let id = pack_id("linked");
    create_value(&home, PluginFamily::Ui, &id);
    let authority = target(&home, PluginFamily::Ui, &id);
    fs::remove_file(&authority).expect("remove authority");
    let outside = parent.path().join("outside.json");
    fs::write(&outside, b"outside").expect("outside");
    symlink(&outside, &authority).expect("symlink");
    assert_eq!(
        read_pack_installation(&home, PluginFamily::Ui, &id)
            .expect_err("link")
            .kind(),
        ConfigErrorKind::LinkEscape
    );
    assert_eq!(fs::read(outside).expect("outside bytes"), b"outside");
}

#[cfg(windows)]
#[test]
fn final_reparse_point_is_rejected_without_touching_target() {
    let (parent, home) = layout();
    let id = pack_id("linked");
    create_value(&home, PluginFamily::Ui, &id);
    let authority = target(&home, PluginFamily::Ui, &id);
    fs::remove_file(&authority).expect("remove authority");
    let outside = parent.path().join("outside");
    fs::create_dir(&outside).expect("outside directory");
    fs::write(outside.join("marker"), b"outside").expect("outside marker");
    junction::create(&outside, &authority).expect("junction");
    assert_eq!(
        read_pack_installation(&home, PluginFamily::Ui, &id)
            .expect_err("reparse")
            .kind(),
        ConfigErrorKind::LinkEscape
    );
    assert_eq!(
        fs::read(outside.join("marker")).expect("outside bytes"),
        b"outside"
    );
}
