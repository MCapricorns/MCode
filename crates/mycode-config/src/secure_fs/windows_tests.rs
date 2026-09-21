use std::ffi::OsStr;
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::os::windows::fs::OpenOptionsExt;

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_CALL_NOT_IMPLEMENTED, ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED,
    GENERIC_READ,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, WRITE_DAC};

use super::{
    FAIL_PARENT_BARRIER, NEXT_BARRIER_ERROR, classify_directory_flush_error,
    ensure_home_layout as platform_ensure_home_layout,
};
use crate::{AccessControlEvidence, ConfigErrorKind, HomeLayout, probe_access_control};

fn assert_exact_current_owner(path: &std::path::Path) {
    assert!(matches!(
        probe_access_control(path),
        AccessControlEvidence::WindowsProtectedDacl {
            owner_allowed: true,
            owner_current_user: true,
            current_user: true,
            system: true,
            protected: true,
            extra_aces: 0,
            ace_count: 1 | 2,
            ..
        }
    ));
}

fn create_secure_empty_root(root: &std::path::Path) {
    platform_ensure_home_layout(root, None).expect("secure root bootstrap");
    fs::remove_dir(root.join("plugins")).expect("remove bootstrap child");
}

fn apply_test_dacl(path: &std::path::Path, sddl: &str) {
    let handle = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .access_mode(GENERIC_READ | WRITE_DAC)
        .open(path)
        .expect("open for test DACL");
    super::windows_acl::apply_sddl_dacl_for_tests(&handle, sddl).expect("apply test DACL");
}

#[test]
fn created_directories_have_explicit_current_owner_and_exact_dacl() {
    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("home");
    platform_ensure_home_layout(&root, None).expect("bootstrap");

    assert_exact_current_owner(&root);
    assert_exact_current_owner(&root.join("plugins"));

    fs::remove_dir_all(&root).expect("all bootstrap handles were released");
}

#[test]
fn read_only_current_owned_root_is_repaired_then_children_created() {
    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("home");
    create_secure_empty_root(&root);

    let sid = super::windows_acl::current_user_sid_string().expect("current SID");
    // GR+GX allow the trailing no-follow read open; WD allows DACL repair; GW is absent.
    let sddl = format!("D:P(A;;GRGXWD;;;{sid})");
    apply_test_dacl(&root, &sddl);

    platform_ensure_home_layout(&root, None).expect("repair then create");

    assert_exact_current_owner(&root);
    assert_exact_current_owner(&root.join("plugins"));
}

#[test]
fn exact_owned_file_does_not_require_write_dac() {
    let parent = tempfile::tempdir().expect("parent");
    let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
    crate::secure_fs::owned_file::replace_owned_file(&layout, "config.json", b"value")
        .expect("secure file fixture");
    let read_only = OpenOptions::new()
        .access_mode(GENERIC_READ)
        .open(layout.config_json())
        .expect("open without WRITE_DAC");

    super::windows_acl::secure_existing_object(&read_only)
        .expect("exact descriptor requires no repair");
}

#[test]
fn permissive_current_owned_directories_are_tightened() {
    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("home");
    platform_ensure_home_layout(&root, None).expect("secure fixture bootstrap");

    let sid = super::windows_acl::current_user_sid_string().expect("current SID");
    let sddl = format!("D:P(A;;FA;;;{sid})(A;;FA;;;SY)(A;;FA;;;WD)");
    for path in [&root, &root.join("plugins")] {
        apply_test_dacl(path, &sddl);
        assert!(matches!(
            probe_access_control(path),
            AccessControlEvidence::WindowsProtectedDacl {
                owner_current_user: true,
                extra_aces: 1..,
                ..
            }
        ));
    }

    platform_ensure_home_layout(&root, None).expect("tighten");

    assert_exact_current_owner(&root);
    assert_exact_current_owner(&root.join("plugins"));
}

#[test]
fn owned_junctions_are_rejected_and_prefix_junctions_are_followed() {
    let parent = tempfile::tempdir().expect("parent");
    let outside = parent.path().join("outside");
    fs::create_dir(&outside).expect("outside");

    let final_link = parent.path().join("final-link");
    junction::create(&outside, &final_link).expect("final junction fixture");
    let final_error = platform_ensure_home_layout(&final_link, None).expect_err("final junction");
    assert_eq!(final_error.kind(), ConfigErrorKind::LinkEscape);

    let ancestor_link = parent.path().join("ancestor-link");
    junction::create(&outside, &ancestor_link).expect("ancestor junction fixture");
    platform_ensure_home_layout(&ancestor_link.join("home"), None)
        .expect("external prefix junction followed");
    assert!(outside.join("home").is_dir());

    let child_root = parent.path().join("child-link");
    create_secure_empty_root(&child_root);
    junction::create(&outside, child_root.join("plugins")).expect("child junction fixture");
    let child_error = platform_ensure_home_layout(&child_root, None).expect_err("child junction");
    assert_eq!(child_error.kind(), ConfigErrorKind::LinkEscape);
    assert!(!outside.join("plugins").exists());
}

#[test]
fn intermediate_prefix_junction_is_followed_when_trailing_component_is_real() {
    let parent = tempfile::tempdir().expect("parent");
    let real_base = parent.path().join("real-base");
    fs::create_dir(&real_base).expect("real base");
    fs::create_dir(real_base.join("real")).expect("real");
    let link = parent.path().join("link");
    junction::create(&real_base, &link).expect("prefix junction fixture");
    let root = link.join("real").join("home");

    platform_ensure_home_layout(&root, None).expect("prefix junction followed");

    assert!(real_base.join("real").join("home").is_dir());
    assert!(real_base.join("real").join("home").join("plugins").is_dir());
    assert!(!link.join("home").exists());
}

#[test]
fn wrong_type_and_wrong_case_fixed_children_are_rejected() {
    let parent = tempfile::tempdir().expect("parent");
    let wrong_type_root = parent.path().join("wrong-type");
    create_secure_empty_root(&wrong_type_root);
    fs::write(wrong_type_root.join("plugins"), b"not a directory").expect("wrong type fixture");
    let type_error = platform_ensure_home_layout(&wrong_type_root, None).expect_err("wrong type");
    assert_eq!(type_error.kind(), ConfigErrorKind::Io);

    let wrong_case_root = parent.path().join("wrong-case");
    create_secure_empty_root(&wrong_case_root);
    fs::create_dir(wrong_case_root.join("Plugins")).expect("wrong case fixture");
    let case_error = platform_ensure_home_layout(&wrong_case_root, None).expect_err("wrong case");
    assert_eq!(case_error.kind(), ConfigErrorKind::AccessControl);
    assert_eq!(
        fs::read_dir(&wrong_case_root)
            .expect("listing")
            .filter_map(Result::ok)
            .filter(|entry| entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case("plugins"))
            .count(),
        1
    );
}

#[test]
fn file_open_probe_reports_absence_without_creating_child() {
    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("home");
    fs::create_dir(&root).expect("root");
    let root_handle =
        super::windows_open::open_existing_directory_nofollow(&root).expect("open existing root");

    let error = super::windows_open::open_dacl_relative(&root_handle, OsStr::new("plugins"))
        .expect_err("missing child");

    assert_eq!(error.io_kind(), Some(io::ErrorKind::NotFound));
    assert!(!root.join("plugins").exists());
}

#[test]
fn root_parent_barrier_failure_is_propagated() {
    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("home");
    FAIL_PARENT_BARRIER.with(|fail| fail.set(true));

    let error = platform_ensure_home_layout(&root, None).expect_err("root parent barrier");

    assert_eq!(error.kind(), ConfigErrorKind::Io);
    assert_eq!(error.io_kind(), Some(io::ErrorKind::PermissionDenied));
}

#[test]
fn missing_child_always_executes_parent_publication_barrier() {
    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("home");
    create_secure_empty_root(&root);
    FAIL_PARENT_BARRIER.with(|fail| fail.set(true));

    let error = platform_ensure_home_layout(&root, None).expect_err("child parent barrier");

    assert_eq!(error.kind(), ConfigErrorKind::Io);
    assert_eq!(error.io_kind(), Some(io::ErrorKind::PermissionDenied));
    assert!(root.join("plugins").is_dir());
}

#[test]
fn unexpected_directory_barrier_failure_is_propagated() {
    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("home");
    NEXT_BARRIER_ERROR.with(|error| error.set(Some(ERROR_ACCESS_DENIED as i32)));

    let error = platform_ensure_home_layout(&root, None).expect_err("unexpected barrier failure");

    assert_eq!(error.kind(), ConfigErrorKind::Io);
    assert_eq!(error.io_kind(), Some(io::ErrorKind::PermissionDenied));
}

#[test]
fn unsupported_directory_barrier_errors_fail_closed() {
    for code in [
        ERROR_INVALID_FUNCTION,
        ERROR_NOT_SUPPORTED,
        ERROR_CALL_NOT_IMPLEMENTED,
        ERROR_ACCESS_DENIED,
    ] {
        let error = classify_directory_flush_error(io::Error::from_raw_os_error(code as i32))
            .expect_err("directory flush failures are never accepted");
        assert_eq!(error.kind(), ConfigErrorKind::Io);
    }
}

#[test]
fn public_layout_bootstrap_uses_the_same_native_contract() {
    let parent = tempfile::tempdir().expect("parent");
    let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
    crate::ensure_home_layout(&layout).expect("bootstrap");
    assert_exact_current_owner(layout.root());
}
