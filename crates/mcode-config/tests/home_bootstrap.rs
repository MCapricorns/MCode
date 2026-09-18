use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;

#[cfg(unix)]
use mcode_config::OwnedKind;
use mcode_config::{
    AccessControlEvidence, ConfigErrorKind, HomeEnv, HomeLayout, PluginFamily, ensure_home_layout,
    probe_access_control,
};
use tempfile::tempdir;

const OLD_FAMILY_ROOTS: [&str; 12] = [
    "provider_plugins",
    "session_plugins",
    "compaction_plugins",
    "resource_plugins",
    "ask_plugins",
    "todo_plugins",
    "web_plugins",
    "mcp_plugins",
    "usage_plugins",
    "subagent_plugins",
    "workspace_plugins",
    "ui_plugins",
];

const CANONICAL_LAZY_ROOT_ENTRIES: [&str; 1] = ["config.json"];

const OBSOLETE_ROOT_ENTRIES: [&str; 10] = [
    "settings.json",
    "models.json",
    "auth.json",
    "credentials.json",
    "plugins.lock",
    "plugins.lock.json",
    "plugins.json",
    "sessions",
    "auth-state",
    ".staging",
];

#[test]
fn bootstrap_creates_only_the_exact_eager_directory_set() {
    let parent = tempdir().expect("parent");
    let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");

    ensure_home_layout(&layout).expect("bootstrap");

    let entries = fs::read_dir(layout.root())
        .expect("home listing")
        .map(|entry| entry.expect("entry").file_name())
        .collect::<BTreeSet<_>>();
    assert_eq!(entries, [OsString::from("plugins")].into_iter().collect());
    assert_eq!(
        fs::read_dir(layout.plugins_dir())
            .expect("plugins listing")
            .count(),
        0
    );
    for relative in CANONICAL_LAZY_ROOT_ENTRIES
        .into_iter()
        .chain(OBSOLETE_ROOT_ENTRIES)
        .chain(OLD_FAMILY_ROOTS)
    {
        assert!(!layout.root().join(relative).exists(), "created {relative}");
    }
    for family in PluginFamily::ALL {
        assert!(
            !layout.plugin_dir(family).exists(),
            "created {}",
            family.id()
        );
    }
    assert!(!layout.host_dir().exists());
    assert!(!layout.host_auth_json().exists());
    assert!(!layout.host_staging_dir().exists());
    assert!(
        !layout
            .pack_dir(PluginFamily::Providers, "pi")
            .expect("pack")
            .exists()
    );
    assert!(!parent.path().join(".mcode").exists());
}

#[test]
fn bootstrap_is_idempotent_and_concurrent() {
    let parent = tempdir().expect("parent");
    let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
    let threads = (0..8)
        .map(|_| {
            let layout = layout.clone();
            std::thread::spawn(move || ensure_home_layout(&layout))
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().expect("bootstrap thread").expect("bootstrap");
    }
    ensure_home_layout(&layout).expect("idempotent bootstrap");

    assert!(layout.root().is_dir());
    assert!(layout.plugins_dir().is_dir());
    assert_eq!(
        fs::read_dir(layout.root()).expect("listing").count(),
        1,
        "concurrency must not create coordination files"
    );
}

#[test]
fn home_derived_wrong_case_mcode_is_rejected_only_at_bootstrap() {
    let user_home = tempdir().expect("user home");
    fs::create_dir(user_home.path().join(".MCODE")).expect("wrong case sibling");

    let layout = HomeLayout::from_env(HomeEnv {
        mcode_home: None,
        home: Some(user_home.path().as_os_str().to_os_string()),
        user_profile: None,
    })
    .expect("constructor remains lexical");
    let error = ensure_home_layout(&layout).expect_err("wrong case .mcode");

    assert_eq!(error.kind(), ConfigErrorKind::InvalidHome);
    assert!(!error.to_string().contains(".MCODE"));
    assert_eq!(fs::read_dir(user_home.path()).expect("listing").count(), 1);
}

#[test]
fn missing_home_derived_parent_stays_lexical_without_creation() {
    let parent = tempdir().expect("parent");
    let missing = parent.path().join("missing-user-home");

    let layout = HomeLayout::from_env(HomeEnv {
        mcode_home: None,
        home: Some(missing.as_os_str().to_os_string()),
        user_profile: None,
    })
    .expect("missing parent remains lexical");

    assert_eq!(layout.root(), missing.join(".mcode"));
    assert!(!missing.exists());
}

#[cfg(unix)]
#[test]
fn permission_denied_home_parent_remains_lexical_when_fixture_is_reliable() {
    use std::os::unix::fs::PermissionsExt;

    if rustix::process::geteuid().as_raw() == 0 {
        eprintln!("skip: euid 0 can bypass the permission-denied fixture");
        return;
    }
    let parent = tempdir().expect("parent");
    let user_home = parent.path().join("user-home");
    fs::create_dir(&user_home).expect("user home");
    fs::set_permissions(&user_home, fs::Permissions::from_mode(0o000)).expect("deny fixture");
    struct Restore<'a>(&'a std::path::Path);
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            let _ = fs::set_permissions(self.0, fs::Permissions::from_mode(0o700));
        }
    }
    let _restore = Restore(&user_home);

    let layout = HomeLayout::from_env(HomeEnv {
        mcode_home: None,
        home: Some(user_home.as_os_str().to_os_string()),
        user_profile: None,
    })
    .expect("permission-denied parent remains lexical");

    assert_eq!(layout.root(), user_home.join(".mcode"));
    assert!(!layout.root().exists());
}

#[cfg(unix)]
#[test]
fn unix_bootstrap_tightens_all_owned_modes() {
    use std::os::unix::fs::PermissionsExt;

    let parent = tempdir().expect("parent");
    let root = parent.path().join("home");
    fs::create_dir(&root).expect("root");
    fs::create_dir(root.join("plugins")).expect("plugins");
    for path in [&root, &root.join("plugins")] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("permissive mode");
    }
    let layout = HomeLayout::from_root(&root).expect("layout");

    ensure_home_layout(&layout).expect("tighten");

    for path in [layout.root(), &layout.plugins_dir()] {
        assert_eq!(
            probe_access_control(path),
            AccessControlEvidence::UnixMode {
                kind: OwnedKind::Directory,
                mode: 0o700,
            }
        );
    }
}

#[cfg(unix)]
#[test]
fn unix_owned_links_are_rejected_but_prefix_links_are_followed() {
    use std::os::unix::fs::symlink;

    let parent = tempdir().expect("parent");
    let outside = parent.path().join("outside");
    fs::create_dir(&outside).expect("outside");

    let final_link = parent.path().join("final-link");
    symlink(&outside, &final_link).expect("final symlink");
    let final_layout = HomeLayout::from_root(&final_link).expect("layout");
    assert_eq!(
        ensure_home_layout(&final_layout)
            .expect_err("final symlink")
            .kind(),
        ConfigErrorKind::LinkEscape
    );
    let explicit_layout = HomeLayout::from_env(HomeEnv {
        mcode_home: Some(final_link.as_os_str().to_os_string()),
        home: None,
        user_profile: None,
    })
    .expect("explicit root remains lexical");
    assert_eq!(
        ensure_home_layout(&explicit_layout)
            .expect_err("explicit root symlink")
            .kind(),
        ConfigErrorKind::LinkEscape
    );
    let derived = HomeLayout::from_env(HomeEnv {
        mcode_home: None,
        home: Some(final_link.as_os_str().to_os_string()),
        user_profile: None,
    })
    .expect("symlink user home remains lexical");
    ensure_home_layout(&derived).expect("user-home prefix symlink followed");
    assert!(outside.join(".mcode").is_dir());

    let ancestor = parent.path().join("ancestor");
    symlink(&outside, &ancestor).expect("ancestor symlink");
    let ancestor_layout = HomeLayout::from_root(ancestor.join("home")).expect("layout");
    ensure_home_layout(&ancestor_layout).expect("external prefix symlink followed");
    assert!(outside.join("home").is_dir());

    let real_base = parent.path().join("real-base");
    fs::create_dir(&real_base).expect("real base");
    fs::create_dir(real_base.join("real")).expect("real trailing");
    let prefix_link = parent.path().join("prefix-link");
    symlink(&real_base, &prefix_link).expect("prefix symlink");
    let prefix_layout =
        HomeLayout::from_root(prefix_link.join("real").join("home")).expect("layout");
    ensure_home_layout(&prefix_layout).expect("prefix symlink followed");
    assert!(real_base.join("real").join("home").is_dir());
    let derived = HomeLayout::from_env(HomeEnv {
        mcode_home: None,
        home: Some(prefix_link.join("real").as_os_str().to_os_string()),
        user_profile: None,
    })
    .expect("prefix symlink user home");
    assert_eq!(derived.root(), prefix_link.join("real").join(".mcode"));

    let wrong_type = parent.path().join("wrong-type");
    fs::create_dir(&wrong_type).expect("wrong type root");
    fs::write(wrong_type.join("plugins"), b"file").expect("wrong type child");
    let wrong_layout = HomeLayout::from_root(&wrong_type).expect("layout");
    assert_eq!(
        ensure_home_layout(&wrong_layout)
            .expect_err("wrong type")
            .kind(),
        ConfigErrorKind::Io
    );
}

#[cfg(windows)]
#[test]
fn windows_user_home_junction_is_followed_outside_owned_boundary() {
    let parent = tempdir().expect("parent");
    let outside = parent.path().join("outside");
    fs::create_dir(&outside).expect("outside");
    let linked_home = parent.path().join("linked-home");
    junction::create(&outside, &linked_home).expect("junction fixture");

    let explicit_layout = HomeLayout::from_env(HomeEnv {
        mcode_home: Some(linked_home.as_os_str().to_os_string()),
        home: None,
        user_profile: None,
    })
    .expect("explicit root remains lexical");
    assert_eq!(
        ensure_home_layout(&explicit_layout)
            .expect_err("explicit root junction")
            .kind(),
        ConfigErrorKind::LinkEscape
    );

    let layout = HomeLayout::from_env(HomeEnv {
        mcode_home: None,
        home: Some(linked_home.as_os_str().to_os_string()),
        user_profile: None,
    })
    .expect("junction user home remains lexical");
    ensure_home_layout(&layout).expect("junction prefix followed");

    assert!(outside.join(".mcode").is_dir());
}

#[cfg(windows)]
#[test]
fn windows_intermediate_prefix_junction_user_home_stays_usable() {
    let parent = tempdir().expect("parent");
    let real_base = parent.path().join("real-base");
    fs::create_dir(&real_base).expect("real base");
    fs::create_dir(real_base.join("real")).expect("real trailing");
    let prefix_link = parent.path().join("prefix-link");
    junction::create(&real_base, &prefix_link).expect("prefix junction fixture");

    let layout = HomeLayout::from_env(HomeEnv {
        mcode_home: None,
        home: Some(prefix_link.join("real").as_os_str().to_os_string()),
        user_profile: None,
    })
    .expect("prefix junction user home");
    ensure_home_layout(&layout).expect("bootstrap through prefix junction");

    assert_eq!(layout.root(), prefix_link.join("real").join(".mcode"));
    assert!(real_base.join("real").join(".mcode").is_dir());
    assert!(!prefix_link.join(".mcode").exists());
}

#[cfg(windows)]
#[test]
fn windows_bootstrap_reports_exact_protected_dacl_evidence() {
    let parent = tempdir().expect("parent");
    let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
    ensure_home_layout(&layout).expect("bootstrap");

    for path in [layout.root(), &layout.plugins_dir()] {
        assert!(matches!(
            probe_access_control(path),
            AccessControlEvidence::WindowsProtectedDacl {
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
}

#[test]
fn owned_root_wrong_type_is_rejected() {
    let parent = tempdir().expect("parent");
    let root = parent.path().join("home");
    fs::write(&root, b"not a directory").expect("wrong-type fixture");
    let layout = HomeLayout::from_root(&root).expect("layout");

    let error = ensure_home_layout(&layout).expect_err("wrong-type root");

    assert_eq!(error.kind(), ConfigErrorKind::Io);
}

#[test]
fn home_error_display_is_value_free() {
    let private = "private-owned-home-value";
    let error = HomeLayout::from_root(private).expect_err("relative root");
    assert!(!error.to_string().contains(private));
}
