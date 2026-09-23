use std::ffi::OsString;
use std::path::{Path, PathBuf};

use mycode_config::{
    ConfigErrorKind, HomeEnv, HomeLayout, MYCODE_DIR_NAME, MYCODE_HOME_ENV, SCRATCH_DIR,
    SESSIONS_DIR, project_folder_name, session_relative,
};

#[test]
fn path_construction_creates_nothing() {
    let root = absolute_dummy_path("must-not-exist").join("nested-owned-home");
    assert!(!root.exists(), "dummy root must start absent");

    let layout = HomeLayout::from_root(&root).expect("valid root");
    let _ = layout
        .owned_join("sessions/ses-1/todos.json")
        .expect("join");

    assert_eq!(layout.root(), root);
    assert!(!root.exists(), "path construction must not create the root");
}

#[test]
fn environment_resolution_is_lexical_even_with_wrong_case_aliases() {
    let user_home = tempfile::tempdir().expect("user home");
    std::fs::create_dir(user_home.path().join(".MYCODE")).expect("wrong-case alias");

    let layout = HomeLayout::from_env(HomeEnv {
        mycode_home: None,
        home: Some(user_home.path().as_os_str().to_os_string()),
        user_profile: None,
    })
    .expect("lexical layout");

    assert_eq!(layout.root(), user_home.path().join(MYCODE_DIR_NAME));
    let names = std::fs::read_dir(user_home.path())
        .expect("listing")
        .map(|entry| entry.expect("entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(names, [OsString::from(".MYCODE")]);
}

#[test]
fn environment_precedence_is_fail_closed() {
    let override_root = absolute_dummy_path("override");
    let user_home = absolute_dummy_path("user-home");
    let profile = absolute_dummy_path("profile");

    let layout = HomeLayout::from_env(HomeEnv {
        mycode_home: Some(override_root.clone().into_os_string()),
        home: Some(user_home.clone().into_os_string()),
        user_profile: Some(profile.clone().into_os_string()),
    })
    .expect("override");
    assert_eq!(layout.root(), override_root);

    let error = HomeLayout::from_env(HomeEnv {
        mycode_home: Some(OsString::from("relative-override")),
        home: Some(user_home.clone().into_os_string()),
        user_profile: Some(profile.clone().into_os_string()),
    })
    .expect_err("invalid override must not fall back");
    assert_eq!(error.kind(), ConfigErrorKind::InvalidHome);
    assert_eq!(error.path(), Some(Path::new("relative-override")));

    let from_home = HomeLayout::from_env(HomeEnv {
        mycode_home: Some(OsString::new()),
        home: Some(user_home.clone().into_os_string()),
        user_profile: Some(profile.into_os_string()),
    })
    .expect("empty override");
    assert_eq!(from_home.root(), user_home.join(MYCODE_DIR_NAME));

    assert_eq!(MYCODE_HOME_ENV, "MYCODE_HOME");
    assert_eq!(MYCODE_DIR_NAME, ".mycode");
}

#[cfg(windows)]
#[test]
fn windows_userprofile_is_only_the_last_nonempty_choice() {
    let profile = absolute_dummy_path("profile");

    let from_profile = HomeLayout::from_env(HomeEnv {
        mycode_home: None,
        home: Some(OsString::new()),
        user_profile: Some(profile.clone().into_os_string()),
    })
    .expect("profile fallback");
    assert_eq!(from_profile.root(), profile.join(MYCODE_DIR_NAME));

    let error = HomeLayout::from_env(HomeEnv {
        mycode_home: None,
        home: Some(OsString::from("relative-home")),
        user_profile: Some(profile.into_os_string()),
    })
    .expect_err("invalid HOME must not fall back");
    assert_eq!(error.kind(), ConfigErrorKind::InvalidHome);

    let error = HomeLayout::from_env(HomeEnv {
        mycode_home: None,
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
        mycode_home: None,
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
    assert_eq!(env.mycode_home, std::env::var_os(MYCODE_HOME_ENV));
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
    let expected_session = root.join("sessions").join("ses-1");
    let original = std::env::current_dir().expect("current directory");
    let alternate = original.parent().expect("current directory has a parent");
    let _guard = CurrentDirGuard::enter(alternate);
    assert_eq!(layout.root(), root);
    assert_eq!(
        layout.owned_join("sessions/ses-1").expect("session path"),
        expected_session
    );
}

#[cfg(windows)]
#[test]
fn windows_roots_accept_only_safe_drive_unc_and_verbatim_forms() {
    for root in [
        PathBuf::from(r"C:\mycode\home"),
        PathBuf::from(r"\\server\share\mycode\home"),
        PathBuf::from(r"\\?\C:\mycode\home"),
        PathBuf::from(r"\\?\UNC\server\share\mycode\home"),
    ] {
        assert_eq!(
            HomeLayout::from_root(&root)
                .expect("valid Windows root")
                .root(),
            root
        );
    }

    let dotted = HomeLayout::from_root(r"C:\mycode\.\home").expect("normal dot");
    assert_eq!(dotted.root(), Path::new(r"C:\mycode\home"));

    for root in [
        PathBuf::from(r"C:\"),
        PathBuf::from(r"\\server\share\"),
        PathBuf::from(r"\\?\C:\"),
        PathBuf::from(r"\\?\UNC\server\share\"),
        PathBuf::from(r"C:relative"),
        PathBuf::from(r"\root-relative"),
        PathBuf::from("/root-relative"),
        PathBuf::from(r"\\server"),
        PathBuf::from(r"\\.\C:\mycode\home"),
        PathBuf::from(r"\\?\GLOBALROOT\Device\HarddiskVolume1\home"),
        PathBuf::from(r"C:\mycode\..\home"),
        PathBuf::from(r"\\?\C:\mycode\.\home"),
        PathBuf::from(r"C:\mycode\name."),
        PathBuf::from("C:\\mycode\\name "),
        PathBuf::from(r"C:\mycode\foo:bar"),
        PathBuf::from(r"C:\mycode\na*me"),
        PathBuf::from("C:\\mycode\\nul\0name"),
        PathBuf::from(r"C:\mycode\CON"),
        PathBuf::from(r"C:\mycode\NuL.json"),
        PathBuf::from(r"C:\mycode\com1.cache"),
        PathBuf::from("C:\\mycode\\COM\u{00B9}"),
        PathBuf::from(r"C:\mycode\CLOCK$"),
        PathBuf::from(r"C:\mycode\clock$.txt"),
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

    let dotted = HomeLayout::from_root("/mycode/./home").expect("normalized dot");
    assert_eq!(dotted.root(), Path::new("/mycode/home"));
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
        .owned_join("private/../value")
        .expect_err("invalid join");
    assert_eq!(escape.kind(), ConfigErrorKind::PathEscape);
    assert!(!escape.to_string().contains("private"));
}

#[test]
fn session_files_live_under_sessions_not_workspace() {
    let path = session_relative("ses1-abc", "todos.json").expect("path");
    assert_eq!(path, format!("{SESSIONS_DIR}/ses1-abc/todos.json"));
    assert_eq!(SCRATCH_DIR, "scratch");
    assert_eq!(SESSIONS_DIR, "sessions");

    for (session_id, file) in [
        ("", "todos.json"),
        ("a/b", "todos.json"),
        ("a\\b", "todos.json"),
        ("ses1", ""),
        ("ses1", "a/b"),
        ("ses1", "a\\b"),
    ] {
        let error = session_relative(session_id, file)
            .expect_err("session id and file must be single components");
        assert_eq!(error.kind(), ConfigErrorKind::AuthorityValidation);
    }

    assert_eq!(project_folder_name("/work/MCode"), "MCode");
    assert_eq!(project_folder_name("/tmp/web-app"), "web-app");
    assert_eq!(project_folder_name("/tmp/trimmed "), "trimmed");
    assert_eq!(project_folder_name("/"), "/");
    assert_eq!(project_folder_name(""), "");
}

fn absolute_dummy_path(name: &str) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(r"C:\mycode-home-layout-dummy").join(name)
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/mycode-home-layout-dummy").join(name)
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
