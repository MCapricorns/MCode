//! Windows no-reparse directory bootstrap with exact protected DACLs.

#[path = "windows_staging.rs"]
pub(crate) mod staging;
#[path = "windows_acl.rs"]
pub(super) mod windows_acl;
#[path = "windows_file.rs"]
pub(super) mod windows_file;
#[path = "windows_open.rs"]
pub(super) mod windows_open;

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows_sys::Wdk::Storage::FileSystem::NtFlushBuffersFileEx;
use windows_sys::Win32::Foundation::RtlNtStatusToDosError;
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use self::windows_acl::{
    inspect_handle, protected_descriptor, require_allowed_owner, secure_existing_object,
    unavailable_reason, verify_fixed_descriptor,
};
use self::windows_open::{
    create_owned_child, create_owned_root, open_dacl_relative, open_existing_object_nofollow,
    open_owned_relative,
};
use super::{AccessControlEvidence, NativeUnavailableReason};
use crate::{ConfigError, ConfigErrorKind};

const EAGER_CHILD: &str = "plugins";

pub(super) fn ensure_home_layout(
    root: &Path,
    expected_root_name: Option<&str>,
) -> Result<(), ConfigError> {
    let Some(name) = root.file_name() else {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, root));
    };
    let descriptor = protected_descriptor()?;
    let opened_root = create_owned_root(
        root,
        expected_root_name,
        &descriptor,
        require_allowed_owner,
        verify_fixed_descriptor,
        sync_created_directory,
    )?;
    reject_wrong_case_child(&opened_root.root, EAGER_CHILD)?;
    let child = OsStr::new(EAGER_CHILD);
    // Protecting the root DACL can leave an existing `plugins/` unopenable
    // (inherited ACEs are not replaced by this non-inheritable DACL), so open
    // that child before tightening. FILE_OPEN never recreates an observed
    // child. A missing child is created only after root repair and reopen.
    match open_dacl_relative(&opened_root.root, child) {
        Ok(existing_child) => {
            secure_existing_object(&existing_child)?;
            let root_dacl = open_dacl_relative(&opened_root.parent, name)?;
            secure_existing_object(&root_dacl)?;
            reject_wrong_case_child(&opened_root.root, EAGER_CHILD)
        }
        Err(error) if error.io_kind() == Some(io::ErrorKind::NotFound) => {
            let root_dacl = open_dacl_relative(&opened_root.parent, name)?;
            secure_existing_object(&root_dacl)?;
            let writable_root = open_owned_relative(&opened_root.parent, name)?;
            let opened_child = create_owned_child(&writable_root, child, &descriptor)?;
            secure_existing_object(&opened_child.file)?;
            // This caller observed absence, so it participates in publication
            // even when another bootstrap won FILE_OPEN_IF creation.
            sync_created_directory(&opened_child.file, &writable_root)?;
            reject_wrong_case_child(&writable_root, EAGER_CHILD)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn probe_access_control(path: &Path) -> AccessControlEvidence {
    match open_existing_object_nofollow(path).and_then(|file| inspect_handle(&file)) {
        Ok(evidence) => evidence,
        Err(error) => AccessControlEvidence::Unavailable {
            platform: std::env::consts::OS,
            reason: unavailable_reason(&error),
        },
    }
}

fn reject_wrong_case_child(directory: &File, expected: &str) -> Result<(), ConfigError> {
    if windows_open::find_wrong_case_child(directory, expected)?.is_some() {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(())
}

fn sync_created_directory(directory: &File, parent: &File) -> Result<(), ConfigError> {
    flush_directory(directory)?;
    #[cfg(test)]
    if FAIL_PARENT_BARRIER.with(|fail| fail.replace(false)) {
        return Err(
            ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::PermissionDenied)
        );
    }
    flush_directory(parent)
}

pub(super) fn flush_directory(directory: &File) -> Result<(), ConfigError> {
    #[cfg(test)]
    if let Some(code) = NEXT_BARRIER_ERROR.with(std::cell::Cell::take) {
        return classify_directory_flush_error(io::Error::from_raw_os_error(code));
    }
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: `directory` is a live synchronous directory handle opened with
    // write-data access. Parameters are absent as required for flags zero, and
    // `status_block` is writable for the duration of this native flush.
    let status = unsafe {
        NtFlushBuffersFileEx(
            directory.as_raw_handle(),
            0,
            std::ptr::null(),
            0,
            &mut status_block,
        )
    };
    if status >= 0 {
        return Ok(());
    }
    // SAFETY: RtlNtStatusToDosError accepts every NTSTATUS and does not use
    // GetLastError.
    let code = unsafe { RtlNtStatusToDosError(status) };
    classify_directory_flush_error(io::Error::from_raw_os_error(code as i32))
}

fn classify_directory_flush_error(error: io::Error) -> Result<(), ConfigError> {
    Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(error.kind()))
}

#[cfg(test)]
thread_local! {
    static NEXT_BARRIER_ERROR: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
    static FAIL_PARENT_BARRIER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
