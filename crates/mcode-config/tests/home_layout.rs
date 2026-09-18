use std::ffi::OsString;
use std::path::{Path, PathBuf};

use mcode_config::{
    ConfigErrorKind, HomeEnv, HomeLayout, MCODE_DIR_NAME, MCODE_HOME_ENV, PluginFamily,
    TransactionId,
};

#[test]
fn nested_top_level_plugin_hierarchy_is_exact() {
    let root = absolute_dummy_path("hierarchy");
    let layout = HomeLayout::from_root(&root).expect("valid root");

    assert_eq!(layout.root(), root);
    assert_eq!(layout.config_json(), root.join("config.json"));
    assert_eq!(layout.plugins_dir(), root.join("plugins"));

    let built_ins = [
        (PluginFamily::Providers, "providers"),
        (PluginFamily::Web, "web"),
        (PluginFamily::Mcp, "mcp"),
        (PluginFamily::Usage, "usage"),
        (PluginFamily::Ui, "ui"),
    ];
    assert_eq!(PluginFamily::ALL, built_ins.map(|(family, _)| family));
    for (family, directory_name) in built_ins {
        assert_eq!(family.id(), format!("com.mcode.{directory_name}"));
        assert_eq!(
            layout.plugin_dir(family),
            root.join("plugins").join(directory_name)
        );
    }

    let plugin = root.join("plugins").join("providers");
    let packs = plugin.join("packs");

    let pack = packs.join("auth.json");
    assert_eq!(layout.packs_dir(PluginFamily::Providers), packs);
    assert_eq!(
        layout
            .pack_dir(PluginFamily::Providers, "auth.json")
            .expect("pack"),
        pack
    );
    assert_eq!(
        layout
            .pack_installation_json(PluginFamily::Providers, "auth.json")
            .expect("pack installation"),
        pack.join("installation.json")
    );
    assert_eq!(
        layout
            .pack_data_dir(PluginFamily::Providers, "auth.json")
            .expect("pack data"),
        pack.join("data")
    );
    assert_eq!(
        layout
            .pack_versions_dir(PluginFamily::Providers, "auth.json")
            .expect("pack versions"),
        pack.join("versions")
    );

    assert_eq!(layout.host_dir(), root.join("plugins").join(".host"));
    assert_eq!(layout.host_auth_json(), layout.host_dir().join("auth.json"));
    assert_eq!(
        layout.host_staging_lock(),
        root.join("plugins").join(".staging.lock")
    );
    assert_eq!(
        layout.host_staging_dir(),
        root.join("plugins").join(".staging")
    );
    let transaction_id = TransactionId::generate().expect("transaction ID");
    assert_eq!(
        layout.transaction_staging_dir(&transaction_id),
        root.join("plugins")
            .join(".staging")
            .join(transaction_id.as_str())
    );
}

#[test]
fn path_construction_creates_nothing() {
    let root = absolute_dummy_path("must-not-exist").join("nested-owned-home");
    assert!(!root.exists(), "dummy root must start absent");

    let layout = HomeLayout::from_root(&root).expect("valid root");
    let _ = layout.config_json();
    let _ = layout.plugins_dir();
    let _ = layout.plugin_dir(PluginFamily::Web);
    let _ = layout.host_dir();
    let _ = layout.host_auth_json();
    let _ = layout.host_staging_lock();
    let _ = layout.host_staging_dir();
    let _ = layout
        .pack_dir(PluginFamily::Web, "pack.example")
        .expect("pack");
    let transaction_id = TransactionId::generate().expect("transaction ID");
    let _ = layout.transaction_staging_dir(&transaction_id);
    let _ = layout.owned_join("controlled/relative/path").expect("join");

    assert!(!root.exists(), "path construction must not create the root");
}

#[test]
fn environment_resolution_is_lexical_even_with_wrong_case_aliases() {
    let user_home = tempfile::tempdir().expect("user home");
    std::fs::create_dir(user_home.path().join(".MCODE")).expect("wrong-case alias");

    let layout = HomeLayout::from_env(HomeEnv {
        mcode_home: None,
        home: Some(user_home.path().as_os_str().to_os_string()),
        user_profile: None,
    })
    .expect("lexical layout");

    assert_eq!(layout.root(), user_home.path().join(MCODE_DIR_NAME));
    let names = std::fs::read_dir(user_home.path())
        .expect("listing")
        .map(|entry| entry.expect("entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(names, [OsString::from(".MCODE")]);
}

#[test]
fn environment_precedence_is_fail_closed() {
    let override_root = absolute_dummy_path("override");
    let user_home = absolute_dummy_path("user-home");
    let profile = absolute_dummy_path("profile");

    let layout = HomeLayout::from_env(HomeEnv {
        mcode_home: Some(override_root.clone().into_os_string()),
        home: Some(user_home.clone().into_os_string()),
        user_profile: Some(profile.clone().into_os_string()),
    })
    .expect("override");
    assert_eq!(layout.root(), override_root);

    let error = HomeLayout::from_env(HomeEnv {
        mcode_home: Some(OsString::from("relative-override")),
        home: Some(user_home.clone().into_os_string()),
        user_profile: Some(profile.clone().into_os_string()),
    })
    .expect_err("invalid override must not fall back");
    assert_eq!(error.kind(), ConfigErrorKind::InvalidHome);
    assert_eq!(error.path(), Some(Path::new("relative-override")));

    let from_home = HomeLayout::from_env(HomeEnv {
        mcode_home: Some(OsString::new()),
        home: Some(user_home.clone().into_os_string()),
        user_profile: Some(profile.clone().into_os_string()),
    })
    .expect("empty override");
    assert_eq!(from_home.root(), user_home.join(MCODE_DIR_NAME));

    assert_eq!(MCODE_HOME_ENV, "MCODE_HOME");
    assert_eq!(MCODE_DIR_NAME, ".mcode");
}

#[cfg(windows)]
#[test]
fn windows_userprofile_is_only_the_last_nonempty_choice() {
    let profile = absolute_dummy_path("profile");

    let from_profile = HomeLayout::from_env(HomeEnv {
        mcode_home: None,
        home: Some(OsString::new()),
        user_profile: Some(profile.clone().into_os_string()),
    })
    .expect("profile fallback");
    assert_eq!(from_profile.root(), profile.join(MCODE_DIR_NAME));

    let error = HomeLayout::from_env(HomeEnv {
        mcode_home: None,
        home: Some(OsString::from("relative-home")),
        user_profile: Some(profile.into_os_string()),
    })
    .expect_err("invalid HOME must not fall back");
    assert_eq!(error.kind(), ConfigErrorKind::InvalidHome);

    let error = HomeLayout::from_env(HomeEnv {
        mcode_home: None,
        home: None,
        user_profile: Some(OsString::from("relative-profile")),
    })
    .expect_err("invalid profile");
    assert_eq!(error.kind(), ConfigErrorKind::InvalidHome);
}

#[cfg(not(windows))]
#[test]
fn non_windows_ignores_userprofile() {
    let error = HomeLayout::from_env(HomeEnv {
        mcode_home: None,
        home: Some(OsString::new()),
        user_profile: Some(absolute_dummy_path("profile").into_os_string()),
    })
    .expect_err("profile is Windows-only");
    assert_eq!(error.kind(), ConfigErrorKind::InvalidHome);
    assert!(error.path().is_none());
}

#[test]
fn missing_environment_values_are_invalid() {
    let error = HomeLayout::from_env(HomeEnv::default()).expect_err("missing home");
    assert_eq!(error.kind(), ConfigErrorKind::InvalidHome);
    assert!(error.path().is_none());
}

#[test]
fn process_environment_snapshot_matches_platform_contract() {
    let env = HomeEnv::from_process();
    assert_eq!(env.mcode_home, std::env::var_os(MCODE_HOME_ENV));
    assert_eq!(env.home, std::env::var_os("HOME"));
    #[cfg(windows)]
    assert_eq!(env.user_profile, std::env::var_os("USERPROFILE"));
    #[cfg(not(windows))]
    assert!(env.user_profile.is_none());
}

#[test]
fn roots_are_absolute_normalized_and_cwd_independent() {
    for root in [
        PathBuf::from("relative"),
        PathBuf::from("."),
        PathBuf::from(".."),
        absolute_dummy_path("a").join("..").join("b"),
    ] {
        let error = HomeLayout::from_root(&root).expect_err("invalid root");
        assert_eq!(error.kind(), ConfigErrorKind::InvalidHome, "{root:?}");
    }

    let root = absolute_dummy_path("cwd-stable");
    let layout = HomeLayout::from_root(&root).expect("absolute root");
    let expected_config = root.join("config.json");
    let original = std::env::current_dir().expect("current directory");
    let alternate = original.parent().expect("current directory has a parent");
    let _guard = CurrentDirGuard::enter(alternate);
    assert_eq!(layout.root(), root);
    assert_eq!(layout.config_json(), expected_config);
}

#[cfg(windows)]
#[test]
fn windows_roots_accept_only_safe_drive_unc_and_verbatim_forms() {
    for root in [
        PathBuf::from(r"C:\mcode\home"),
        PathBuf::from(r"\\server\share\mcode\home"),
        PathBuf::from(r"\\?\C:\mcode\home"),
        PathBuf::from(r"\\?\UNC\server\share\mcode\home"),
    ] {
        assert_eq!(
            HomeLayout::from_root(&root)
                .expect("valid Windows root")
                .root(),
            root
        );
    }

    let dotted = HomeLayout::from_root(r"C:\mcode\.\home").expect("normal dot");
    assert_eq!(dotted.root(), Path::new(r"C:\mcode\home"));

    for root in [
        PathBuf::from(r"C:\"),
        PathBuf::from(r"\\server\share\"),
        PathBuf::from(r"\\?\C:\"),
        PathBuf::from(r"\\?\UNC\server\share\"),
        PathBuf::from(r"C:relative"),
        PathBuf::from(r"\root-relative"),
        PathBuf::from("/root-relative"),
        PathBuf::from(r"\\server"),
        PathBuf::from(r"\\.\C:\mcode\home"),
        PathBuf::from(r"\\?\GLOBALROOT\Device\HarddiskVolume1\home"),
        PathBuf::from(r"C:\mcode\..\home"),
        PathBuf::from(r"\\?\C:\mcode\.\home"),
        PathBuf::from(r"C:\mcode\name."),
        PathBuf::from("C:\\mcode\\name "),
        PathBuf::from(r"C:\mcode\foo:bar"),
        PathBuf::from(r"C:\mcode\na*me"),
        PathBuf::from("C:\\mcode\\nul\0name"),
        PathBuf::from(r"C:\mcode\CON"),
        PathBuf::from(r"C:\mcode\NuL.json"),
        PathBuf::from(r"C:\mcode\com1.cache"),
        PathBuf::from("C:\\mcode\\COM\u{00B9}"),
        PathBuf::from(r"C:\mcode\CLOCK$"),
        PathBuf::from(r"C:\mcode\clock$.txt"),
        PathBuf::from(r"\\server.\share\home"),
        PathBuf::from(r"\\server\share.\home"),
        PathBuf::from(r"\\con\share\home"),
    ] {
        let error = HomeLayout::from_root(&root).expect_err("unsafe Windows root");
        assert_eq!(error.kind(), ConfigErrorKind::InvalidHome, "{root:?}");
    }
}

#[cfg(not(windows))]
#[test]
fn unix_filesystem_root_is_not_an_owned_home() {
    let error = HomeLayout::from_root("/").expect_err("filesystem root");
    assert_eq!(error.kind(), ConfigErrorKind::InvalidHome);

    let dotted = HomeLayout::from_root("/mcode/./home").expect("normalized dot");
    assert_eq!(dotted.root(), Path::new("/mcode/home"));
}

#[test]
fn pack_ids_retain_the_portable_grammar() {
    let layout = HomeLayout::from_root(absolute_dummy_path("ids")).expect("layout");
    let maximum = format!("a{}", "b".repeat(127));
    assert_eq!(maximum.len(), 128);

    for valid in [
        "a",
        "a0",
        "a.b_c-d9",
        "auth.json",
        "com.mcode.providers",
        &maximum,
    ] {
        assert!(
            layout.pack_dir(PluginFamily::Web, valid).is_ok(),
            "rejected {valid:?}"
        );
    }

    let too_long = format!("a{}", "b".repeat(128));
    for invalid in [
        "",
        "0abc",
        "Aabc",
        "abc.",
        "abc-",
        "abc_",
        ".abc",
        ".host",
        ".staging",
        "a/b",
        "a\\b",
        "a:b",
        "a*b",
        "a?b",
        "a\0b",
        "a\nb",
        "a b",
        "café",
        "con",
        "nul.json",
        "com1",
        "lpt9.cache",
        "com\u{00B9}",
        &too_long,
    ] {
        let pack_error = layout
            .pack_dir(PluginFamily::Web, invalid)
            .expect_err("invalid Pack ID");
        assert_eq!(
            pack_error.kind(),
            ConfigErrorKind::PathEscape,
            "{invalid:?}"
        );
    }

    assert_eq!(
        layout
            .pack_dir(PluginFamily::Providers, "auth.json")
            .expect("auth.json is a valid Pack ID"),
        absolute_dummy_path("ids")
            .join("plugins")
            .join("providers")
            .join("packs")
            .join("auth.json")
    );
    assert_eq!(
        layout.host_auth_json(),
        absolute_dummy_path("ids")
            .join("plugins")
            .join(".host")
            .join("auth.json")
    );
}

#[test]
fn generated_transaction_id_has_the_only_public_spelling() {
    let transaction_id = TransactionId::generate().expect("transaction ID");
    let spelling = transaction_id.as_str();

    assert_eq!(spelling.len(), 36);
    assert_eq!(&spelling[..4], "tx1-");
    assert!(
        spelling[4..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    assert_eq!(transaction_id.to_string(), spelling);
    assert!(format!("{transaction_id:?}").contains(spelling));
}

#[test]
fn generated_transaction_id_routes_directly_below_staging() {
    let layout = HomeLayout::from_root(absolute_dummy_path("transaction-route")).expect("layout");
    let transaction_id = TransactionId::generate().expect("transaction ID");
    let staging = layout.host_staging_dir();
    let transaction = layout.transaction_staging_dir(&transaction_id);

    assert_eq!(transaction.parent(), Some(staging.as_path()));
    assert_eq!(
        transaction,
        staging.join(transaction_id.as_str()),
        "transaction path must use only the generated ID component"
    );
}

#[test]
fn owned_join_rejects_every_unsafe_component() {
    let root = absolute_dummy_path("owned-join");
    let layout = HomeLayout::from_root(&root).expect("layout");
    assert_eq!(
        layout
            .owned_join("controlled/relative/file.json")
            .expect("controlled path"),
        root.join("controlled").join("relative").join("file.json")
    );

    for invalid in [
        "",
        ".",
        "..",
        "a/./b",
        "a/../b",
        "/absolute",
        r"C:\absolute",
        r"\\server\share\path",
        "a//b",
        "a\\\\b",
        "a/",
        "a\\",
        "name.",
        "name ",
        "name:stream",
        "na*me",
        "na?me",
        "na\0me",
        "na\nme",
        "CON",
        "NuL.json",
        "com1.cache",
        "COM\u{00B9}",
        "CLOCK$",
        "clock$.txt",
    ] {
        let error = layout.owned_join(invalid).expect_err("unsafe join");
        assert_eq!(error.kind(), ConfigErrorKind::PathEscape, "{invalid:?}");
        assert!(error.path().is_none());
    }
}

#[test]
fn path_error_display_is_value_free() {
    let value = "relative-private-path";
    let error = HomeLayout::from_root(value).expect_err("relative root");
    assert_eq!(error.path(), Some(Path::new(value)));
    assert!(!error.to_string().contains(value));

    let escape = HomeLayout::from_root(absolute_dummy_path("display"))
        .expect("layout")
        .pack_dir(PluginFamily::Providers, "private/value")
        .expect_err("invalid ID");
    assert!(!escape.to_string().contains("private"));
}

fn absolute_dummy_path(name: &str) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(r"C:\mcode-home-layout-dummy").join(name)
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/mcode-home-layout-dummy").join(name)
    }
}

struct CurrentDirGuard {
    original: PathBuf,
}

impl CurrentDirGuard {
    fn enter(path: &Path) -> Self {
        let original = std::env::current_dir().expect("current directory");
        std::env::set_current_dir(path).expect("change current directory");
        Self { original }
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.original).expect("restore current directory");
    }
}
