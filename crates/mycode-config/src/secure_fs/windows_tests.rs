use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::os::windows::fs::OpenOptionsExt;

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_CALL_NOT_IMPLEMENTED, ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED,
    GENERIC_READ,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, WRITE_DAC};

use super::windows_acl::assert_exact_private_for_tests;
use super::{NEXT_BARRIER_ERROR, classify_directory_flush_error};
use crate::secure_fs::owned_file::{ensure_owned_directory, replace_owned_file};
use crate::{ConfigErrorKind, HomeLayout};

fn owned(layout: &HomeLayout, relative: &str) -> std::path::PathBuf {
    layout.owned_join(relative).expect("owned path")
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
    let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");

    ensure_owned_directory(&layout, "sessions/ses-1").expect("owned directories");

    assert_exact_private_for_tests(layout.root());
    assert_exact_private_for_tests(&owned(&layout, "sessions"));
    assert_exact_private_for_tests(&owned(&layout, "sessions/ses-1"));
}

#[test]
fn permissive_current_owned_root_is_tightened_by_first_mutation() {
    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("home");
    fs::create_dir(&root).expect("permissive fixture");
    let layout = HomeLayout::from_root(&root).expect("layout");

    let sid = super::windows_acl::current_user_sid_string().expect("current SID");
    apply_test_dacl(
        &root,
        &format!("D:P(A;;FA;;;{sid})(A;;FA;;;SY)(A;;FA;;;WD)"),
    );

    replace_owned_file(&layout, "settings.json", b"value").expect("tightening mutation");

    assert_exact_private_for_tests(&root);
    assert_exact_private_for_tests(&owned(&layout, "settings.json"));
}

#[test]
fn read_only_current_owned_root_is_repaired_by_first_mutation() {
    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("home");
    fs::create_dir(&root).expect("read-only fixture");
    let layout = HomeLayout::from_root(&root).expect("layout");

    let sid = super::windows_acl::current_user_sid_string().expect("current SID");
    // GR+GX allow the trailing no-follow read open; WD allows DACL repair; GW is absent.
    apply_test_dacl(&root, &format!("D:P(A;;GRGXWD;;;{sid})"));

    replace_owned_file(&layout, "settings.json", b"value").expect("repair then mutate");

    assert_exact_private_for_tests(&root);
    assert_exact_private_for_tests(&owned(&layout, "settings.json"));
}

#[test]
fn exact_owned_file_does_not_require_write_dac() {
    let parent = tempfile::tempdir().expect("parent");
    let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
    replace_owned_file(&layout, "settings.json", b"value").expect("secure file fixture");
    let read_only = OpenOptions::new()
        .access_mode(GENERIC_READ)
        .open(owned(&layout, "settings.json"))
        .expect("open without WRITE_DAC");

    super::windows_acl::secure_existing_object(&read_only)
        .expect("exact descriptor requires no repair");
}

#[test]
fn owned_root_junction_is_rejected_by_live_mutation() {
    let parent = tempfile::tempdir().expect("parent");
    let outside = parent.path().join("outside");
    fs::create_dir(&outside).expect("outside");
    let link = parent.path().join("home-link");
    junction::create(&outside, &link).expect("root junction fixture");

    let layout = HomeLayout::from_root(&link).expect("lexical layout");
    let error =
        replace_owned_file(&layout, "settings.json", b"value").expect_err("owned root junction");

    assert_eq!(error.kind(), ConfigErrorKind::LinkEscape);
    assert!(!outside.join("settings.json").exists());
}

#[test]
fn unexpected_directory_barrier_failure_is_propagated() {
    let parent = tempfile::tempdir().expect("parent");
    let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
    NEXT_BARRIER_ERROR.with(|error| error.set(Some(ERROR_ACCESS_DENIED as i32)));

    let error =
        ensure_owned_directory(&layout, "sessions").expect_err("unexpected barrier failure");

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
