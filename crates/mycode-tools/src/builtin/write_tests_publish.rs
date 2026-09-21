//! Publish, cleanup, and security tests split from `write_tests`.
use super::tests::*;
use super::*;
use crate::builtin::test_support::{ctx_at, run_dyn};
use crate::tool::{Concurrency, ToolDyn};
use serde_json::json;

#[cfg(windows)]
#[tokio::test]
async fn restricted_dacl_failed_publish_leaves_no_temp() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_SUCCESS, GENERIC_READ, INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    fn wide(path: &std::path::Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Replaces `path`'s DACL with a protected one parsed from `sddl`.
    /// The owner keeps the implicit right to run this again.
    fn apply_dacl(path: &std::path::Path, sddl: &str) {
        let text: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `text` is a live NUL-terminated UTF-16 string; on
        // success `sd` is an allocation that `LocalFree` releases.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1, // SDDL_REVISION_1
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0, "SDDL parse failed");
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = std::ptr::null_mut();
        // SAFETY: `sd` is a valid descriptor from the call above.
        let ok = unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) };
        assert_ne!(ok, 0, "GetSecurityDescriptorDacl failed");
        assert_ne!(present, 0, "descriptor must carry a DACL");
        let name = wide(path);
        // SAFETY: `name` is live NUL-terminated; `dacl` aliases the
        // live `sd` allocation during the call.
        let status = unsafe {
            SetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        // SAFETY: release the descriptor allocation after the apply.
        unsafe { LocalFree(sd.cast()) };
        assert_eq!(status, ERROR_SUCCESS, "SetNamedSecurityInfo failed");
    }

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("locked.txt");
    std::fs::write(&target, "v1").unwrap();
    // Set the read-only bit while the DACL is still the inherited
    // full-access one: it is copied onto the temp together with the
    // DACL, so cleanup must also defeat FILE_ATTRIBUTE_READONLY, not
    // just the DACL.
    let mut perms = std::fs::metadata(&target).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&target, perms).unwrap();
    // Everyone may read (prepare and identity checks keep working) but
    // write/delete are denied, so a by-name cleanup open of a temp that
    // inherited this DACL would be refused.
    apply_dacl(&target, "D:PAI(A;;FR;;;WD)");

    // Force the publish (rename-over) to fail: hold the target open
    // without FILE_SHARE_DELETE for the whole write attempt.
    let name = wide(&target);
    // SAFETY: `name` is a live NUL-terminated UTF-16 path; the returned
    // handle is owned and closed below.
    let blocker = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(blocker, INVALID_HANDLE_VALUE, "blocker open failed");

    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &WriteTool,
        json!({"path": "locked.txt", "content": "v2", "overwrite": true}),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)), "{err}");
    assert!(
        err.to_string().contains("failed to publish"),
        "restrictive DACL fixture must fail at publish, not earlier: {err}"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1");
    // Cleanup must run on the retained creation-time DELETE handle
    // (never a fresh by-name open the copied DACL would deny) and must
    // ignore the copied read-only attribute instead of failing with
    // STATUS_CANNOT_DELETE and stranding the temp.
    assert!(
        temp_leftovers(dir.path()).is_empty(),
        "temp files left behind: {:?}",
        temp_leftovers(dir.path())
    );

    // SAFETY: release the blocker handle opened above.
    unsafe { CloseHandle(blocker) };
    // Restore full control, then clear the read-only bit (needs write
    // access), so the tempdir cleanup can delete the file.
    apply_dacl(&target, "D:PAI(A;;FA;;;WD)");
    let mut perms = std::fs::metadata(&target).unwrap().permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "Windows-only test cleanup: this only clears the read-only bit"
    )]
    perms.set_readonly(false);
    std::fs::set_permissions(&target, perms).unwrap();
}

#[tokio::test]
async fn capability_is_consumed_once() {
    use crate::builtin::fs_io::{FileAccess, prepare_file};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let prepared = prepare_file(
        dir.path(),
        "once.txt",
        &tokio_util::sync::CancellationToken::new(),
        FileAccess::ExistingOrMissing,
    )
    .unwrap();
    let ctx = ctx_at(dir.path()).with_prepared_file(Arc::new(prepared));

    run_dyn(
        &WriteTool,
        json!({"path": "once.txt", "content": "first"}),
        &ctx,
    )
    .await
    .unwrap();
    let err = run_dyn(
        &WriteTool,
        json!({"path": "once.txt", "content": "second", "overwrite": true}),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("already consumed"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("once.txt")).unwrap(),
        "first"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn ads_write_target_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &WriteTool,
        json!({"path": "plain.txt:ads", "content": "x"}),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert!(!dir.path().join("plain.txt").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn intermediate_symlink_directory_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink("real", dir.path().join("alias")).unwrap();
    let ctx = ctx_at(dir.path());

    let err = run_dyn(
        &WriteTool,
        json!({"path": "alias/new.txt", "content": "x"}),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert!(!real.join("new.txt").exists());
}

#[cfg(windows)]
#[tokio::test]
async fn junction_component_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    junction::create(&real, dir.path().join("jd")).unwrap();
    let ctx = ctx_at(dir.path());

    let err = run_dyn(
        &WriteTool,
        json!({"path": "jd/new.txt", "content": "x"}),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert!(!real.join("new.txt").exists());
}

#[cfg(windows)]
#[tokio::test]
async fn final_symlink_target_is_rejected_when_creation_is_permitted() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("real.txt"), "x").unwrap();
    // Creating a symbolic link needs Developer Mode or
    // SeCreateSymbolicLink privilege. Skip honestly when the fixture
    // cannot be created; reparse rejection is covered by the junction
    // tests.
    if std::os::windows::fs::symlink_file("real.txt", dir.path().join("link.txt")).is_err() {
        eprintln!("skipped: symbolic link creation is not permitted on this host");
        return;
    }
    let ctx = ctx_at(dir.path());

    let err = run_dyn(
        &WriteTool,
        json!({"path": "link.txt", "content": "y"}),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("real.txt")).unwrap(),
        "x"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn unix_mode_is_preserved_on_overwrite() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("mode.txt");
    std::fs::write(&target, "v1").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o601)).unwrap();
    let ctx = ctx_at(dir.path());

    run_dyn(
        &WriteTool,
        json!({"path": "mode.txt", "content": "v2", "overwrite": true}),
        &ctx,
    )
    .await
    .unwrap();
    let mode = std::fs::metadata(&target).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o601, "mode bits must survive the rewrite");
}

#[cfg(windows)]
#[tokio::test]
async fn protected_dacl_is_preserved_on_overwrite() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
    };

    fn wide(path: &std::path::Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Returns the DACL-protection flag of `path`'s security descriptor.
    fn dacl_protected(path: &std::path::Path) -> bool {
        let name = wide(path);
        let mut owner = std::ptr::null_mut();
        let mut group = std::ptr::null_mut();
        let mut dacl = std::ptr::null_mut();
        let mut sacl = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `name` is a live NUL-terminated UTF-16 path; all output
        // pointers are writable. `sd` aliases every other output pointer,
        // so freeing only `sd` releases the whole allocation.
        let status = unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                &mut owner,
                &mut group,
                &mut dacl,
                &mut sacl,
                &mut sd,
            )
        };
        assert_eq!(status, ERROR_SUCCESS, "GetNamedSecurityInfo failed");
        let mut control = 0u16;
        let mut revision = 0u32;
        // SAFETY: `sd` is live from GetNamedSecurityInfo.
        let ok = unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) };
        assert_ne!(ok, 0, "GetSecurityDescriptorControl failed");
        // SAFETY: `sd` is the allocation root returned above.
        unsafe { LocalFree(sd.cast()) };
        control & SE_DACL_PROTECTED != 0
    }

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("dacl.txt");
    std::fs::write(&target, "v1").unwrap();
    assert!(!dacl_protected(&target), "fresh file DACL must inherit");

    // Mark the DACL protected. The protection flag is the observable
    // marker: a temp file created fresh would always inherit.
    let name = wide(&target);
    let mut owner = std::ptr::null_mut();
    let mut group = std::ptr::null_mut();
    let mut dacl = std::ptr::null_mut();
    let mut sacl = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: as in `dacl_protected`; the returned DACL is applied back
    // while `sd` is still alive.
    let status = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            &mut owner,
            &mut group,
            &mut dacl,
            &mut sacl,
            &mut sd,
        )
    };
    assert_eq!(status, ERROR_SUCCESS, "GetNamedSecurityInfo failed");
    // SAFETY: `dacl` aliases the live `sd` allocation and stays alive
    // through the call.
    let status = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null(),
        )
    };
    // SAFETY: release the descriptor allocation after the apply.
    unsafe { LocalFree(sd.cast()) };
    assert_eq!(status, ERROR_SUCCESS, "SetNamedSecurityInfo failed");
    assert!(dacl_protected(&target));

    let ctx = ctx_at(dir.path());
    run_dyn(
        &WriteTool,
        json!({"path": "dacl.txt", "content": "v2", "overwrite": true}),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v2");
    assert!(
        dacl_protected(&target),
        "published file must keep the protected DACL of the original"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn unprotected_inherited_dacl_is_not_frozen() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, PSECURITY_DESCRIPTOR,
        SE_DACL_PROTECTED,
    };

    fn wide(path: &std::path::Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn dacl_protected(path: &std::path::Path) -> bool {
        let mut owner = std::ptr::null_mut();
        let mut group = std::ptr::null_mut();
        let mut dacl = std::ptr::null_mut();
        let mut sacl = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let name = wide(path);
        // SAFETY: `name` is live NUL-terminated; outputs are writable.
        let status = unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                &mut owner,
                &mut group,
                &mut dacl,
                &mut sacl,
                &mut sd,
            )
        };
        assert_eq!(status, ERROR_SUCCESS, "GetNamedSecurityInfo failed");
        let mut control = 0;
        let mut revision = 0u32;
        // SAFETY: `sd` is a live descriptor from the call above.
        let ok = unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) };
        // SAFETY: release the descriptor after reading control.
        unsafe { LocalFree(sd.cast()) };
        assert_ne!(ok, 0, "GetSecurityDescriptorControl failed");
        control & SE_DACL_PROTECTED != 0
    }

    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("inherit.txt");
    std::fs::write(&existing, "v1").unwrap();
    assert!(!dacl_protected(&existing), "fresh file DACL must inherit");
    let ctx = ctx_at(dir.path());
    run_dyn(
        &WriteTool,
        json!({"path": "inherit.txt", "content": "v2", "overwrite": true}),
        &ctx,
    )
    .await
    .unwrap();
    assert!(
        !dacl_protected(&existing),
        "overwrite must not freeze an inheriting DACL as protected"
    );

    run_dyn(
        &WriteTool,
        json!({"path": "new-inherit.txt", "content": "created"}),
        &ctx,
    )
    .await
    .unwrap();
    assert!(
        !dacl_protected(&dir.path().join("new-inherit.txt")),
        "new file must keep the directory's unprotected inherited DACL"
    );
}

#[cfg(windows)]
#[tokio::test]
#[expect(
    clippy::await_holding_lock,
    reason = "process-global temp-link hook is not async-aware; these tests must not overlap"
)]
async fn missing_target_probe_cleanup_survives_restrictive_dacl() {
    let _serialize = serialize_temp_link_tests();
    use std::os::windows::ffi::OsStrExt;
    use std::sync::Arc;
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR,
    };

    use crate::builtin::fs_io::install_temp_links_hook;

    fn wide(path: &std::path::Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn apply_dacl(path: &std::path::Path, sddl: &str) {
        let text: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `text` is live NUL-terminated UTF-16; `sd` is freed below.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0, "SDDL parse failed");
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = std::ptr::null_mut();
        // SAFETY: `sd` is a valid descriptor from the call above.
        let ok = unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) };
        assert_ne!(ok, 0, "GetSecurityDescriptorDacl failed");
        let name = wide(path);
        // SAFETY: `name` and `dacl` stay live for the call.
        let status = unsafe {
            SetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        // SAFETY: release the descriptor after apply.
        unsafe { LocalFree(sd.cast()) };
        assert_eq!(status, ERROR_SUCCESS, "SetNamedSecurityInfo failed");
    }

    let dir = tempfile::tempdir().unwrap();
    let observe_dir = dir.path().to_path_buf();
    let _hook = install_temp_links_hook(Arc::new(move || {
        for entry in std::fs::read_dir(&observe_dir).unwrap().flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with("mycode-write-") {
                continue;
            }
            // Payload temps deny by-name opens. The inherited probe is
            // still named and can receive a restrictive DACL.
            if std::fs::OpenOptions::new()
                .read(true)
                .open(entry.path())
                .is_ok()
            {
                apply_dacl(&entry.path(), "D:PAI(A;;FR;;;WD)");
            }
        }
    }));
    let ctx = ctx_at(dir.path());
    run_dyn(
        &WriteTool,
        json!({"path": "probed.txt", "content": "ok"}),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("probed.txt")).unwrap(),
        "ok"
    );
    assert!(
        temp_leftovers(dir.path()).is_empty(),
        "restrictive probe DACL must not leave named residue: {:?}",
        temp_leftovers(dir.path())
    );
}

#[test]
fn write_args_debug_redacts_content() {
    const SECRET: &str = "MYCODE-SECRET-SENTINEL-9f3a";
    let args = WriteArgs {
        path: "secret.txt".to_owned(),
        content: SECRET.to_owned(),
        expected_revision: None,
        overwrite: false,
    };
    let rendered = format!("{args:?}");
    assert!(rendered.contains("WriteArgs"), "{rendered}");
    assert!(!rendered.contains(SECRET), "{rendered}");
}

#[cfg(windows)]
#[tokio::test]
#[expect(
    clippy::await_holding_lock,
    reason = "process-global temp-link hook is not async-aware; these tests must not overlap"
)]
async fn permissive_parent_cannot_read_payload_temp() {
    let _serialize = serialize_temp_link_tests();
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use std::sync::Arc;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_SUCCESS, GENERIC_READ, INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };

    use crate::builtin::fs_io::install_temp_links_hook;

    fn wide(path: &std::path::Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn apply_dacl(path: &std::path::Path, sddl: &str) {
        let text: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `text` is live NUL-terminated UTF-16; `sd` is freed below.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0, "SDDL parse failed");
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = std::ptr::null_mut();
        // SAFETY: `sd` is a valid descriptor from the call above.
        let ok = unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) };
        assert_ne!(ok, 0, "GetSecurityDescriptorDacl failed");
        let name = wide(path);
        // SAFETY: `name` and `dacl` stay live for the call.
        let status = unsafe {
            SetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        // SAFETY: release the descriptor after apply.
        unsafe { LocalFree(sd.cast()) };
        assert_eq!(status, ERROR_SUCCESS, "SetNamedSecurityInfo failed");
    }

    let dir = tempfile::tempdir().unwrap();
    apply_dacl(dir.path(), "D:PAI(A;;FA;;;WD)");
    let marker = "MYCODE-PAYLOAD-LEAK-MARKER";
    let ctx = ctx_at(dir.path());

    {
        let observe_dir = dir.path().to_path_buf();
        let denied = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let denied_hook = denied.clone();
        let _hook = install_temp_links_hook(Arc::new(move || {
            let mut denied_opens = 0usize;
            let mut seen = 0usize;
            for entry in std::fs::read_dir(&observe_dir).unwrap().flatten() {
                let name = entry.file_name();
                if !name.to_string_lossy().starts_with("mycode-write-") {
                    continue;
                }
                seen += 1;
                let path = wide(&entry.path());
                // SAFETY: `path` is a live NUL-terminated UTF-16 name.
                let handle = unsafe {
                    CreateFileW(
                        path.as_ptr(),
                        GENERIC_READ,
                        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                        std::ptr::null(),
                        OPEN_EXISTING,
                        FILE_ATTRIBUTE_NORMAL,
                        std::ptr::null_mut(),
                    )
                };
                if handle == INVALID_HANDLE_VALUE {
                    denied_opens += 1;
                    continue;
                }
                // SAFETY: `CreateFileW` returned an owned handle.
                let mut file = unsafe { std::fs::File::from_raw_handle(handle) };
                let mut text = String::new();
                use std::io::Read;
                file.read_to_string(&mut text).unwrap();
                assert_eq!(text, "", "readable temp must be the never-written probe");
                assert!(!text.contains(marker));
            }
            if seen > 0 {
                denied_hook.store(denied_opens, std::sync::atomic::Ordering::SeqCst);
            }
        }));
        run_dyn(
            &WriteTool,
            json!({"path": "new.txt", "content": format!("{marker}\n").repeat(1024)}),
            &ctx,
        )
        .await
        .unwrap();
        assert!(
            denied.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "payload temp must deny foreign read opens"
        );
    }

    std::fs::write(dir.path().join("old.txt"), "old").unwrap();
    {
        let denied = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let denied_hook = denied.clone();
        let observe_dir = dir.path().to_path_buf();
        let _hook = install_temp_links_hook(Arc::new(move || {
            let mut denied_opens = 0usize;
            let mut seen = 0usize;
            for entry in std::fs::read_dir(&observe_dir).unwrap().flatten() {
                let name = entry.file_name();
                if !name.to_string_lossy().starts_with("mycode-write-") {
                    continue;
                }
                seen += 1;
                let path = wide(&entry.path());
                // SAFETY: `path` is a live NUL-terminated UTF-16 name.
                let handle = unsafe {
                    CreateFileW(
                        path.as_ptr(),
                        GENERIC_READ,
                        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                        std::ptr::null(),
                        OPEN_EXISTING,
                        FILE_ATTRIBUTE_NORMAL,
                        std::ptr::null_mut(),
                    )
                };
                if handle == INVALID_HANDLE_VALUE {
                    denied_opens += 1;
                } else {
                    // SAFETY: `CreateFileW` returned an owned handle.
                    unsafe { CloseHandle(handle) };
                }
            }
            if seen > 0 {
                denied_hook.store(denied_opens, std::sync::atomic::Ordering::SeqCst);
            }
        }));
        run_dyn(
            &WriteTool,
            json!({"path": "old.txt", "content": "new", "overwrite": true}),
            &ctx,
        )
        .await
        .unwrap();
        assert!(
            denied.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "existing-target payload temp must deny foreign read opens"
        );
    }
}

#[cfg(windows)]
#[tokio::test]
async fn failed_publish_reports_cleanup_failure() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    use crate::builtin::fs_io::install_delete_fault_under;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("locked.txt");
    std::fs::write(&target, "v1").unwrap();
    let fault = install_delete_fault_under(dir.path()).expect("delete fault must install");
    let mut wide: Vec<u16> = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is a live NUL-terminated UTF-16 path; the handle is
    // closed below.
    let blocker = unsafe {
        CreateFileW(
            wide.as_mut_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(blocker, INVALID_HANDLE_VALUE, "blocker open failed");
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &WriteTool,
        json!({"path": "locked.txt", "content": "v2", "overwrite": true}),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("failed to publish"),
        "must fail at publish: {err}"
    );
    assert!(
        err.to_string().contains("injected mycode delete failure"),
        "cleanup failure must enter the returned error: {err}"
    );
    assert!(
        !temp_leftovers(dir.path()).is_empty(),
        "faulted cleanup must leave documented residue"
    );
    // SAFETY: release the blocker opened above.
    unsafe { CloseHandle(blocker) };
    drop(fault);
    for name in temp_leftovers(dir.path()) {
        std::fs::remove_file(dir.path().join(name)).ok();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn failed_publish_reports_cleanup_failure() {
    use std::os::unix::ffi::OsStrExt;
    use std::sync::Arc;

    use crate::builtin::fs_io::{FileAccess, install_unlink_fault_under, prepare_file};

    let dir = tempfile::tempdir().unwrap();
    let prepared = prepare_file(
        dir.path(),
        "blocked.txt",
        &tokio_util::sync::CancellationToken::new(),
        FileAccess::ExistingOrMissing,
    )
    .unwrap();
    let fifo = dir.path().join("blocked.txt");
    let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: `c_fifo` is a live NUL-terminated path; `mkfifo` only
    // creates the named FIFO after create-only prepare.
    let rc = unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o644) };
    assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
    let fault = install_unlink_fault_under(dir.path(), None).expect("unlink fault must install");
    let ctx = ctx_at(dir.path()).with_prepared_file(Arc::new(prepared));
    let err = run_dyn(
        &WriteTool,
        json!({"path": "blocked.txt", "content": "late"}),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("failed to create"), "{err}");
    assert!(
        err.to_string().contains("injected mycode unlink failure"),
        "cleanup failure must enter the returned error: {err}"
    );
    assert!(
        !temp_leftovers(dir.path()).is_empty(),
        "faulted cleanup must leave documented residue"
    );
    drop(fault);
    for name in temp_leftovers(dir.path()) {
        std::fs::remove_file(dir.path().join(name)).ok();
    }
}

#[test]
fn capability_markers() {
    let tool: &dyn ToolDyn = &WriteTool;
    assert!(tool.mutates_fs());
    assert_eq!(tool.concurrency(), Concurrency::Parallel);
    assert!(tool.requires_file_preflight());
    assert!(!tool.requires_search_preflight());
}
