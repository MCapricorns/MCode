use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;

use mycode_config::{
    AuthorityRevision, ConfigErrorKind, DefaultRoute, HomeLayout, MAX_PROVIDER_ID_BYTES, PackId,
    PluginFamily, ProviderId, RootComposition, UiSelection, read_root_composition,
    replace_root_composition,
};
use serde_json::{Value, json};

fn layout() -> (tempfile::TempDir, HomeLayout) {
    let parent = tempfile::tempdir().expect("temporary parent");
    let home = HomeLayout::from_root(parent.path().join("home")).expect("valid layout");
    (parent, home)
}

fn pack(value: &str) -> PackId {
    PackId::parse(value).expect("valid Pack ID")
}

fn provider(value: &str) -> ProviderId {
    ProviderId::parse(value).expect("valid provider ID")
}

fn revision(value: u64) -> AuthorityRevision {
    AuthorityRevision::new(value).expect("valid revision")
}

fn dense_composition() -> RootComposition {
    let providers = vec![pack("zeta.provider"), pack("alpha-provider")];
    let usage = vec![pack("usage.second"), pack("usage-first")];
    let ui = UiSelection::new(vec![pack("theme-a"), pack("theme.b"), pack("theme_c")])
        .expect("UI selection");
    let mut composition = RootComposition::new(
        Some(DefaultRoute::new(provider("provider-primary"), "model/v1@exact").expect("route")),
        providers,
        usage,
        ui,
    )
    .expect("composition");
    for family in PluginFamily::SINGLETONS {
        composition
            .set_singleton(
                family,
                Some(pack(&format!("{}.selected", family.directory_name()))),
            )
            .expect("singleton");
    }
    composition
}

fn create_value(home: &HomeLayout) -> Value {
    replace_root_composition(home, AuthorityRevision::ABSENT, &RootComposition::empty())
        .expect("initial composition");
    serde_json::from_slice(&fs::read(home.config_json()).expect("composition bytes"))
        .expect("composition JSON")
}

fn write_value(home: &HomeLayout, value: &Value) {
    fs::write(
        home.config_json(),
        serde_json::to_vec(value).expect("serialize malformed fixture"),
    )
    .expect("write malformed fixture");
}

fn assert_authority_invalid(home: &HomeLayout, value: &Value) {
    write_value(home, value);
    let error = read_root_composition(home).expect_err("authority must fail");
    assert_eq!(error.kind(), ConfigErrorKind::AuthorityValidation);
    assert_eq!(error.path(), Some(home.config_json().as_path()));
}

#[test]
fn empty_and_dense_round_trips_are_deterministic_and_newline_terminated() {
    let (_parent, home) = layout();
    let empty =
        replace_root_composition(&home, AuthorityRevision::ABSENT, &RootComposition::empty())
            .expect("empty write");
    assert_eq!(empty.revision().get(), 1);
    assert_eq!(empty.composition(), &RootComposition::empty());
    let empty_bytes = fs::read(home.config_json()).expect("empty bytes");
    assert_eq!(empty_bytes.last(), Some(&b'\n'));
    assert_eq!(
        std::str::from_utf8(&empty_bytes).expect("UTF-8"),
        "{\"formatVersion\":1,\"kind\":\"mycode-root-composition\",\"revision\":1,\"defaultRoute\":null,\"providers\":[],\"usage\":[],\"ui\":{\"themes\":[]},\"singletons\":{\"web\":null,\"mcp\":null}}\n"
    );

    let dense = dense_composition();
    let written = replace_root_composition(&home, empty.revision(), &dense).expect("dense write");
    let first_bytes = fs::read(home.config_json()).expect("dense bytes");
    let read = read_root_composition(&home)
        .expect("read")
        .expect("document");
    assert_eq!(read, written);
    assert_eq!(read.composition(), &dense);
    let rewritten = replace_root_composition(&home, read.revision(), read.composition())
        .expect("deterministic rewrite");
    let second_bytes = fs::read(home.config_json()).expect("rewritten bytes");
    let first_text = std::str::from_utf8(&first_bytes).expect("UTF-8");
    let second_text = std::str::from_utf8(&second_bytes).expect("UTF-8");
    assert_eq!(
        first_text.replace("\"revision\":2", "\"revision\":3"),
        second_text
    );
    assert_eq!(rewritten.revision().get(), 3);
}

#[test]
fn missing_read_creates_nothing() {
    let (_parent, home) = layout();
    assert_eq!(read_root_composition(&home).expect("missing read"), None);
    assert!(!home.root().exists());
}

#[test]
fn pack_and_route_bounds_are_exact() {
    for valid in ["a", "a0", "a.b-c_d", &format!("a{}z", "x".repeat(126))] {
        assert_eq!(pack(valid).as_str(), valid);
    }
    for invalid in [
        "",
        "A",
        "1a",
        "a.",
        "a/child",
        "a\\child",
        ".host",
        "con",
        "com1.txt",
        "a:b",
        "é",
        &format!("a{}", "x".repeat(128)),
    ] {
        assert_eq!(
            PackId::parse(invalid).expect_err("invalid Pack ID").kind(),
            ConfigErrorKind::AuthorityValidation
        );
    }

    let exact_provider = format!("a{}z", "x".repeat(MAX_PROVIDER_ID_BYTES - 2));
    let route = DefaultRoute::new(provider(&exact_provider), "!~").expect("route bounds");
    assert_eq!(route.provider_id().as_str(), exact_provider);
    assert_eq!(route.model_id(), "!~");

    for invalid in [
        String::new(),
        "x".repeat(MAX_PROVIDER_ID_BYTES + 1),
        "x".repeat(256),
        "Provider:Primary".to_owned(),
        "provider--primary".to_owned(),
        "provider-".to_owned(),
        "é".to_owned(),
    ] {
        assert_eq!(
            ProviderId::parse(&invalid)
                .expect_err("invalid provider ID")
                .kind(),
            ConfigErrorKind::AuthorityValidation
        );
    }

    for invalid in [
        String::new(),
        "x".repeat(257),
        "has space".to_owned(),
        "tab\t".to_owned(),
        "é".to_owned(),
    ] {
        assert_eq!(
            DefaultRoute::new(provider("provider"), &invalid)
                .expect_err("invalid model ID")
                .kind(),
            ConfigErrorKind::AuthorityValidation
        );
    }
}

#[test]
fn persisted_default_route_rejects_former_visible_ascii_provider_id() {
    let (_parent, home) = layout();
    let mut value = create_value(&home);
    value["defaultRoute"] = json!({"providerId":"Provider:Primary","modelId":"model/v1@exact"});
    assert_authority_invalid(&home, &value);
}

#[test]
fn ordered_lists_preserve_order_and_reject_duplicates_and_overflow() {
    let mut composition = RootComposition::empty();
    composition
        .set_providers(vec![pack("z"), pack("a")])
        .expect("provider order");
    composition
        .set_usage(vec![pack("widget-z"), pack("widget-a")])
        .expect("widget order");
    assert_eq!(composition.providers(), &[pack("z"), pack("a")]);
    assert_eq!(composition.usage(), &[pack("widget-z"), pack("widget-a")]);

    let before = composition.clone();
    assert_eq!(
        composition
            .set_providers(vec![pack("same"), pack("same")])
            .expect_err("duplicate providers")
            .kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert_eq!(composition, before);
    let overflow = (0..257).map(|index| pack(&format!("p{index}"))).collect();
    assert_eq!(
        composition
            .set_usage(overflow)
            .expect_err("usage overflow")
            .kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert_eq!(composition, before);
}

#[test]
fn themes_are_strictly_sorted_and_unique() {
    UiSelection::new(vec![pack("a"), pack("b"), pack("c")]).expect("sorted themes");
    for themes in [vec![pack("b"), pack("a")], vec![pack("a"), pack("a")]] {
        assert_eq!(
            UiSelection::new(themes)
                .expect_err("invalid theme order")
                .kind(),
            ConfigErrorKind::AuthorityValidation
        );
    }
    let overflow = (0..257)
        .map(|index| pack(&format!("theme-{index:03}")))
        .collect();
    assert_eq!(
        UiSelection::new(overflow)
            .expect_err("theme overflow")
            .kind(),
        ConfigErrorKind::AuthorityValidation
    );
}

#[test]
fn singleton_set_is_exact_and_non_singleton_api_is_rejected() {
    let mut composition = RootComposition::empty();
    assert_eq!(PluginFamily::SINGLETONS.len(), 2);
    for family in PluginFamily::SINGLETONS {
        let selected = pack(&format!("{}.pack", family.directory_name()));
        composition
            .set_singleton(family, Some(selected.clone()))
            .expect("set singleton");
        assert_eq!(
            composition.singleton(family).expect("get singleton"),
            Some(&selected)
        );
    }
    let before = composition.clone();
    for family in [
        PluginFamily::Providers,
        PluginFamily::Usage,
        PluginFamily::Ui,
    ] {
        assert_eq!(
            composition
                .singleton(family)
                .expect_err("non-singleton get")
                .kind(),
            ConfigErrorKind::AuthorityValidation
        );
        assert_eq!(
            composition
                .set_singleton(family, Some(pack("rejected")))
                .expect_err("non-singleton set")
                .kind(),
            ConfigErrorKind::AuthorityValidation
        );
        assert_eq!(composition, before);
    }
}

#[test]
fn root_and_nested_members_are_exact_and_typed() {
    let (_parent, home) = layout();
    let base = create_value(&home);
    let mut cases = Vec::new();
    for field in [
        "formatVersion",
        "kind",
        "revision",
        "defaultRoute",
        "providers",
        "usage",
        "ui",
        "singletons",
    ] {
        let mut value = base.clone();
        value.as_object_mut().expect("root").remove(field);
        cases.push(value);
    }
    let mut extra = base.clone();
    extra["extra"] = Value::Null;
    cases.push(extra);
    for (field, wrong) in [
        ("formatVersion", json!(2)),
        ("formatVersion", json!("1")),
        ("kind", json!("other")),
        ("kind", json!(1)),
        ("revision", json!(0)),
        ("revision", json!(-1)),
        ("revision", json!(1.0)),
        ("defaultRoute", json!([])),
        ("providers", json!({})),
        ("usage", json!(null)),
        ("ui", json!([])),
        ("singletons", json!([])),
    ] {
        let mut value = base.clone();
        value[field] = wrong;
        cases.push(value);
    }
    for value in cases {
        assert_authority_invalid(&home, &value);
    }

    let duplicate = serde_json::to_string(&base).expect("base JSON").replacen(
        "{",
        "{\"kind\":\"mycode-root-composition\",",
        1,
    );
    fs::write(home.config_json(), duplicate).expect("duplicate root");
    assert_eq!(
        read_root_composition(&home)
            .expect_err("duplicate root")
            .kind(),
        ConfigErrorKind::DuplicateKey
    );
}

#[test]
fn route_ui_and_singleton_members_reject_missing_extra_duplicate_and_wrong_types() {
    let (_parent, home) = layout();
    let mut base = create_value(&home);
    base["defaultRoute"] = json!({"providerId":"provider","modelId":"model"});
    base["ui"] = json!({"themes":["theme-a","theme-b"]});
    base["singletons"]["web"] = json!("web-pack");

    for (object, fields) in [
        ("defaultRoute", &["providerId", "modelId"][..]),
        ("ui", &["themes"][..]),
    ] {
        for field in fields {
            let mut value = base.clone();
            value[object]
                .as_object_mut()
                .expect("object")
                .remove(*field);
            assert_authority_invalid(&home, &value);
        }
        let mut value = base.clone();
        value[object]["extra"] = Value::Null;
        assert_authority_invalid(&home, &value);
    }
    for (object, field, wrong) in [
        ("defaultRoute", "providerId", json!(1)),
        ("defaultRoute", "modelId", json!(null)),
        ("ui", "themes", json!({})),
    ] {
        let mut value = base.clone();
        value[object][field] = wrong;
        assert_authority_invalid(&home, &value);
    }

    for family in PluginFamily::SINGLETONS {
        let mut value = base.clone();
        value["singletons"]
            .as_object_mut()
            .expect("singletons")
            .remove(family.directory_name());
        assert_authority_invalid(&home, &value);
    }
    let mut extra = base.clone();
    extra["singletons"]["providers"] = Value::Null;
    assert_authority_invalid(&home, &extra);
    let mut wrong = base.clone();
    wrong["singletons"]["web"] = json!(false);
    assert_authority_invalid(&home, &wrong);

    for object in ["defaultRoute", "ui", "singletons"] {
        let encoded = serde_json::to_string(&base[object]).expect("nested JSON");
        let first_field = match object {
            "defaultRoute" => "\"providerId\":\"duplicate\",",
            "ui" => "\"themes\":[],",
            _ => "\"web\":null,",
        };
        let duplicate = encoded.replacen("{", &format!("{{{first_field}"), 1);
        let raw = serde_json::to_string(&base)
            .expect("base JSON")
            .replacen(&encoded, &duplicate, 1);
        fs::write(home.config_json(), raw).expect("duplicate nested member");
        assert_eq!(
            read_root_composition(&home)
                .expect_err("duplicate nested member")
                .kind(),
            ConfigErrorKind::DuplicateKey
        );
    }
}

#[test]
fn explicit_nulls_remain_null_without_defaults() {
    let (_parent, home) = layout();
    let value = create_value(&home);
    assert!(value["defaultRoute"].is_null());
    for family in PluginFamily::SINGLETONS {
        assert!(value["singletons"][family.directory_name()].is_null());
    }
    let read = read_root_composition(&home)
        .expect("read")
        .expect("document");
    assert_eq!(read.composition().default_route(), None);
}

#[test]
fn list_validation_is_enforced_while_parsing() {
    let (_parent, home) = layout();
    let base = create_value(&home);
    for (path, invalid) in [
        ("providers", json!(["same", "same"])),
        ("usage", json!(["same", "same"])),
    ] {
        let mut value = base.clone();
        value[path] = invalid;
        assert_authority_invalid(&home, &value);
    }
    for themes in [json!(["b", "a"]), json!(["a", "a"])] {
        let mut value = base.clone();
        value["ui"]["themes"] = themes;
        assert_authority_invalid(&home, &value);
    }
    let mut extra_field = base.clone();
    extra_field["ui"] = json!({"themes":["a","b"],"runtime":"b"});
    assert_authority_invalid(&home, &extra_field);
}

#[test]
fn revisions_advance_and_stale_cas_preserves_bytes() {
    let (_parent, home) = layout();
    let first =
        replace_root_composition(&home, AuthorityRevision::ABSENT, &RootComposition::empty())
            .expect("revision one");
    let second = replace_root_composition(&home, first.revision(), &dense_composition())
        .expect("revision two");
    assert_eq!(second.revision().get(), 2);
    let before = fs::read(home.config_json()).expect("before stale write");
    let error = replace_root_composition(&home, revision(1), &RootComposition::empty())
        .expect_err("stale CAS");
    assert_eq!(error.kind(), ConfigErrorKind::RevisionConflict);
    assert_eq!(
        fs::read(home.config_json()).expect("after stale write"),
        before
    );
}

#[test]
fn concurrent_same_revision_has_exactly_one_success() {
    let (_parent, home) = layout();
    replace_root_composition(&home, AuthorityRevision::ABSENT, &RootComposition::empty())
        .expect("revision one");
    let home = Arc::new(home);
    let barrier = Arc::new(Barrier::new(9));
    let handles = (0..8)
        .map(|index| {
            let home = Arc::clone(&home);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let mut composition = RootComposition::empty();
                composition
                    .set_providers(vec![pack(&format!("provider-{index}"))])
                    .expect("provider");
                barrier.wait();
                replace_root_composition(&home, revision(1), &composition)
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("worker did not panic"))
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| result
                .as_ref()
                .is_err_and(|error| error.kind() == ConfigErrorKind::RevisionConflict))
            .count(),
        7
    );
    assert_eq!(
        read_root_composition(&home)
            .expect("final read")
            .expect("document")
            .revision()
            .get(),
        2
    );
}

#[test]
fn exhausted_and_malformed_authorities_preserve_bytes() {
    let (_parent, home) = layout();
    let mut value = create_value(&home);
    value["revision"] = json!(i64::MAX);
    write_value(&home, &value);
    let before = fs::read(home.config_json()).expect("exhausted bytes");
    let error = replace_root_composition(&home, revision(i64::MAX as u64), &dense_composition())
        .expect_err("exhausted revision");
    assert_eq!(error.kind(), ConfigErrorKind::RevisionExhausted);
    assert_eq!(
        fs::read(home.config_json()).expect("preserved bytes"),
        before
    );

    let mut malformed = value;
    malformed["providers"] = json!(["same", "same"]);
    write_value(&home, &malformed);
    let before = fs::read(home.config_json()).expect("malformed bytes");
    let error =
        replace_root_composition(&home, revision(i64::MAX as u64), &RootComposition::empty())
            .expect_err("malformed current authority");
    assert_eq!(error.kind(), ConfigErrorKind::AuthorityValidation);
    assert_eq!(
        fs::read(home.config_json()).expect("preserved malformed"),
        before
    );
}

#[test]
fn bounded_parser_rejects_oversized_deep_node_heavy_and_non_utf8() {
    let (_parent, home) = layout();
    create_value(&home);

    fs::write(home.config_json(), vec![b' '; 64 * 1024 + 1]).expect("oversized fixture");
    assert_eq!(
        read_root_composition(&home).expect_err("oversized").kind(),
        ConfigErrorKind::Oversized
    );

    let mut nested = "null".to_owned();
    for _ in 0..10 {
        nested = format!("[{nested}]");
    }
    let deep = format!(
        "{{\"formatVersion\":1,\"kind\":\"mycode-root-composition\",\"revision\":1,\"defaultRoute\":null,\"providers\":{nested},\"usage\":[],\"ui\":{{\"themes\":[]}},\"singletons\":{{\"web\":null,\"mcp\":null}}}}"
    );
    fs::write(home.config_json(), deep).expect("deep fixture");
    assert_eq!(
        read_root_composition(&home).expect_err("deep").kind(),
        ConfigErrorKind::TooDeep
    );

    let nodes = (0..2_100).map(|_| "null").collect::<Vec<_>>().join(",");
    fs::write(home.config_json(), format!("[{nodes}]")).expect("node fixture");
    assert_eq!(
        read_root_composition(&home).expect_err("node-heavy").kind(),
        ConfigErrorKind::TooManyNodes
    );

    fs::write(home.config_json(), [0xff, 0xfe]).expect("non-UTF-8 fixture");
    assert_eq!(
        read_root_composition(&home).expect_err("non-UTF-8").kind(),
        ConfigErrorKind::NonUtf8
    );
}

#[test]
fn parser_errors_do_not_retain_untrusted_member_name() {
    let (_parent, home) = layout();
    create_value(&home);
    let secret_member = "untrusted-secret-member-name";
    let raw = format!("{{\"{secret_member}\":null,\"{secret_member}\":null}}");
    fs::write(home.config_json(), raw).expect("duplicate secret member");
    let error = read_root_composition(&home).expect_err("duplicate member");
    assert_eq!(error.kind(), ConfigErrorKind::DuplicateKey);
    assert_eq!(error.path(), Some(home.config_json().as_path()));
    assert!(!format!("{error}").contains(secret_member));
    assert!(!format!("{error:?}").contains(secret_member));
}

#[test]
fn wrong_target_type_is_rejected() {
    let (_parent, home) = layout();
    create_value(&home);
    fs::remove_file(home.config_json()).expect("remove regular target");
    fs::create_dir(home.config_json()).expect("directory target");
    let error = read_root_composition(&home).expect_err("wrong target type");
    assert_eq!(error.kind(), ConfigErrorKind::Io);
    assert_eq!(error.path(), None);
}
