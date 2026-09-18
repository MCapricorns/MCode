//! Unix no-follow directory bootstrap with private modes and durability.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use rustix::fs::{self as rfs, AtFlags, Mode, OFlags};
use rustix::io::Errno;

use super::{AccessControlEvidence, NativeUnavailableReason, OwnedKind};
use crate::home::validate_path_component;
use crate::{ConfigError, ConfigErrorKind};

#[path = "unix_staging.rs"]
pub(crate) mod staging;
#[path = "unix_file.rs"]
pub(super) mod unix_file;

const DIRECTORY_MODE: rfs::RawMode = 0o700;
const EAGER_CHILD: &str = "plugins";

pub(super) fn ensure_home_layout(
    root: &Path,
    expected_root_name: Option<&str>,
) -> Result<(), ConfigError> {
    let root = create_owned_root(root, expected_root_name)?;
    reject_wrong_case_child(&root, EAGER_CHILD, ConfigErrorKind::AccessControl)?;
    let _ = create_or_open_directory(&root, OsStr::new(EAGER_CHILD), true)?;
    reject_wrong_case_child(&root, EAGER_CHILD, ConfigErrorKind::AccessControl)
}

pub(super) fn find_wrong_case_child(
    directory: &File,
    expected: &str,
) -> Result<Option<OsString>, ConfigError> {
    let entries = rfs::Dir::read_from(directory.as_fd())
        .map_err(|error| map_errno(error, ConfigErrorKind::Io))?;
    for entry in entries {
        let entry = entry.map_err(|error| map_errno(error, ConfigErrorKind::Io))?;
        let name = OsStr::from_bytes(entry.file_name().to_bytes());
        let Some(text) = name.to_str() else {
            continue;
        };
        if text != expected && text.eq_ignore_ascii_case(expected) {
            return Ok(Some(name.to_os_string()));
        }
    }
    Ok(None)
}

pub(super) fn probe_access_control(path: &Path) -> AccessControlEvidence {
    use std::os::unix::fs::PermissionsExt;

    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {
            AccessControlEvidence::UnixMode {
                kind: OwnedKind::Directory,
                mode: metadata.permissions().mode() & 0o777,
            }
        }
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => {
            AccessControlEvidence::UnixMode {
                kind: OwnedKind::File,
                mode: metadata.permissions().mode() & 0o777,
            }
        }
        Ok(_) | Err(_) => AccessControlEvidence::Unavailable {
            platform: std::env::consts::OS,
            reason: NativeUnavailableReason::QueryFailed,
        },
    }
}

pub(super) fn create_owned_root(
    root: &Path,
    expected_root_name: Option<&str>,
) -> Result<File, ConfigError> {
    if !root.is_absolute()
        || !root
            .components()
            .any(|component| matches!(component, Component::Normal(_)))
    {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, root));
    }
    let Some(parent_path) = root.parent() else {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, root));
    };
    let Some(name) = root.file_name() else {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, root));
    };
    validate_path_component(name)?;
    let parent = open_trailing_directory(parent_path)?;
    if let Some(expected) = expected_root_name {
        reject_wrong_case_child(&parent, expected, ConfigErrorKind::InvalidHome)?;
    }
    let root = create_or_open_directory(&parent, name, true)?;
    if let Some(expected) = expected_root_name {
        reject_wrong_case_child(&parent, expected, ConfigErrorKind::InvalidHome)?;
    }
    Ok(root)
}

pub(super) fn open_trailing_directory(path: &Path) -> Result<File, ConfigError> {
    if !path.is_absolute() {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, path));
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    match rfs::open(path, flags, Mode::empty()) {
        Ok(descriptor) => Ok(File::from(descriptor)),
        Err(error) => Err(map_path_errno(path, error, ConfigErrorKind::Io)),
    }
}

pub(super) fn reject_wrong_case_child(
    directory: &File,
    expected: &str,
    kind: ConfigErrorKind,
) -> Result<(), ConfigError> {
    if find_wrong_case_child(directory, expected)?.is_some() {
        return Err(ConfigError::new(kind));
    }
    Ok(())
}

pub(super) fn create_or_open_directory(
    parent: &File,
    name: &OsStr,
    owned: bool,
) -> Result<File, ConfigError> {
    reject_link_or_wrong_type(parent, name, true)?;
    let created = match rfs::mkdirat(parent.as_fd(), name, Mode::from_raw_mode(DIRECTORY_MODE)) {
        Ok(()) => true,
        Err(Errno::EXIST) => false,
        Err(error) => return Err(map_errno(error, ConfigErrorKind::Io)),
    };
    let directory = open_existing_directory(parent, name)?;
    if created || owned {
        enforce_owned_directory(&directory)?;
    }
    if created {
        sync_created_directory(&directory, parent)?;
    }
    Ok(directory)
}

pub(super) fn open_existing_directory(parent: &File, name: &OsStr) -> Result<File, ConfigError> {
    reject_link_or_wrong_type(parent, name, false)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    match open_component(parent, name, flags) {
        Ok(descriptor) => {
            let directory = File::from(descriptor);
            let stat = rfs::fstat(directory.as_fd())
                .map_err(|error| map_errno(error, ConfigErrorKind::Io))?;
            if rfs::FileType::from_raw_mode(stat.st_mode) != rfs::FileType::Directory {
                return Err(ConfigError::new(ConfigErrorKind::Io)
                    .with_io_kind(io::ErrorKind::NotADirectory));
            }
            Ok(directory)
        }
        Err(error) => {
            reject_link_or_wrong_type(parent, name, false)?;
            Err(error)
        }
    }
}

fn reject_link_or_wrong_type(
    parent: &File,
    name: &OsStr,
    missing_allowed: bool,
) -> Result<(), ConfigError> {
    match rfs::statat(parent.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => {
            match rfs::FileType::from_raw_mode(stat.st_mode) {
                rfs::FileType::Symlink => Err(ConfigError::new(ConfigErrorKind::LinkEscape)),
                rfs::FileType::Directory => Ok(()),
                _ => Err(ConfigError::new(ConfigErrorKind::Io)
                    .with_io_kind(io::ErrorKind::NotADirectory)),
            }
        }
        Err(Errno::NOENT) if missing_allowed => Ok(()),
        Err(error) => Err(map_errno(error, ConfigErrorKind::Io)),
    }
}

pub(super) fn open_component(
    parent: &File,
    name: &OsStr,
    flags: OFlags,
) -> Result<std::os::fd::OwnedFd, ConfigError> {
    open_component_with_mounts(parent, name, flags, true)
}

pub(super) fn open_staging_component(
    parent: &File,
    name: &OsStr,
    flags: OFlags,
) -> Result<std::os::fd::OwnedFd, ConfigError> {
    open_component_with_mounts(parent, name, flags, false)
}

pub(super) fn open_staging_directory(parent: &File, name: &OsStr) -> Result<File, ConfigError> {
    reject_link_or_wrong_type(parent, name, false)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let directory = File::from(open_staging_component(parent, name, flags)?);
    let stat =
        rfs::fstat(directory.as_fd()).map_err(|error| map_errno(error, ConfigErrorKind::Io))?;
    if rfs::FileType::from_raw_mode(stat.st_mode) != rfs::FileType::Directory {
        return Err(
            ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::NotADirectory)
        );
    }
    Ok(directory)
}

fn open_component_with_mounts(
    parent: &File,
    name: &OsStr,
    flags: OFlags,
    allow_mounts: bool,
) -> Result<std::os::fd::OwnedFd, ConfigError> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let mut resolve = rfs::ResolveFlags::BENEATH | rfs::ResolveFlags::NO_SYMLINKS;
        if !allow_mounts {
            resolve |= rfs::ResolveFlags::NO_XDEV;
        }
        rfs::openat2(parent.as_fd(), name, flags, Mode::empty(), resolve).map_err(|error| {
            if error == Errno::NOSYS {
                ConfigError::new(ConfigErrorKind::AccessControl)
                    .with_io_kind(io::ErrorKind::Unsupported)
            } else {
                map_errno(error, ConfigErrorKind::Io)
            }
        })
    }
    #[cfg(target_vendor = "apple")]
    {
        // O_NOFOLLOW anchors the one-component open; fstatfs then identifies
        // the mount instance rather than merely the underlying volume.
        let opened = rfs::openat(parent.as_fd(), name, flags, Mode::empty())
            .map_err(|error| map_errno(error, ConfigErrorKind::Io))?;
        if !allow_mounts {
            verify_same_apple_mount(parent, &opened)?;
        }
        Ok(opened)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    {
        if !allow_mounts {
            return Err(ConfigError::new(ConfigErrorKind::AccessControl)
                .with_io_kind(io::ErrorKind::Unsupported));
        }
        // The caller supplies one validated component and O_NOFOLLOW, so the
        // portable openat fallback remains anchored to `parent`.
        rfs::openat(parent.as_fd(), name, flags, Mode::empty())
            .map_err(|error| map_errno(error, ConfigErrorKind::Io))
    }
}

#[cfg(target_vendor = "apple")]
fn verify_same_apple_mount(
    parent: &File,
    opened: &std::os::fd::OwnedFd,
) -> Result<(), ConfigError> {
    let parent_stat =
        rfs::fstat(parent.as_fd()).map_err(|error| map_errno(error, ConfigErrorKind::Io))?;
    let opened_stat =
        rfs::fstat(opened.as_fd()).map_err(|error| map_errno(error, ConfigErrorKind::Io))?;
    let parent_file_system =
        rfs::fstatfs(parent.as_fd()).map_err(|error| map_errno(error, ConfigErrorKind::Io))?;
    let opened_file_system =
        rfs::fstatfs(opened.as_fd()).map_err(|error| map_errno(error, ConfigErrorKind::Io))?;
    if !apple_mount_identity_matches(
        parent_stat.st_dev == opened_stat.st_dev,
        &parent_file_system.f_mntonname,
        &opened_file_system.f_mntonname,
    ) {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(())
}

#[cfg(target_vendor = "apple")]
fn apple_mount_identity_matches(
    same_device: bool,
    parent_mount: &[std::ffi::c_char],
    opened_mount: &[std::ffi::c_char],
) -> bool {
    same_device && apple_mount_name(parent_mount) == apple_mount_name(opened_mount)
}

#[cfg(target_vendor = "apple")]
fn apple_mount_name(bytes: &[std::ffi::c_char]) -> &[std::ffi::c_char] {
    let length = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    &bytes[..length]
}

fn enforce_owned_directory(directory: &File) -> Result<(), ConfigError> {
    verify_owned_directory_owner(directory)?;
    rfs::fchmod(directory.as_fd(), Mode::from_raw_mode(DIRECTORY_MODE))
        .map_err(|error| map_errno(error, ConfigErrorKind::AccessControl))?;
    verify_owned_directory(directory)
}

pub(super) fn verify_owned_directory(directory: &File) -> Result<(), ConfigError> {
    let stat = verify_owned_directory_owner(directory)?;
    if stat.st_mode & 0o777 != DIRECTORY_MODE {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(())
}

fn verify_owned_directory_owner(directory: &File) -> Result<rfs::Stat, ConfigError> {
    let stat =
        rfs::fstat(directory.as_fd()).map_err(|error| map_errno(error, ConfigErrorKind::Io))?;
    if rfs::FileType::from_raw_mode(stat.st_mode) != rfs::FileType::Directory {
        return Err(
            ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::NotADirectory)
        );
    }
    if stat.st_uid != rustix::process::geteuid().as_raw() {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(stat)
}

fn sync_created_directory(directory: &File, parent: &File) -> Result<(), ConfigError> {
    sync_directory(directory)?;
    #[cfg(test)]
    if FAIL_NEXT_PARENT_SYNC.with(|fail| fail.replace(false)) {
        return Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::Other));
    }
    sync_directory(parent)
}

#[cfg(target_vendor = "apple")]
pub(super) fn sync_directory(directory: &File) -> Result<(), ConfigError> {
    rfs::fcntl_fullfsync(directory.as_fd()).map_err(|error| map_errno(error, ConfigErrorKind::Io))
}

#[cfg(not(target_vendor = "apple"))]
pub(super) fn sync_directory(directory: &File) -> Result<(), ConfigError> {
    rfs::fsync(directory.as_fd()).map_err(|error| map_errno(error, ConfigErrorKind::Io))
}

pub(super) fn map_errno(error: Errno, kind: ConfigErrorKind) -> ConfigError {
    if error == Errno::LOOP {
        return ConfigError::new(ConfigErrorKind::LinkEscape);
    }
    ConfigError::new(kind).with_io_kind(io::Error::from(error).kind())
}

fn map_path_errno(path: &Path, error: Errno, kind: ConfigErrorKind) -> ConfigError {
    if error == Errno::LOOP {
        return ConfigError::for_path(ConfigErrorKind::LinkEscape, path);
    }
    ConfigError::for_path(kind, path).with_io_kind(io::Error::from(error).kind())
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_PARENT_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use super::{FAIL_NEXT_PARENT_SYNC, ensure_home_layout};
    use crate::{ConfigErrorKind, HomeLayout};

    #[test]
    fn created_parent_sync_failure_is_reported() {
        let parent = tempfile::tempdir().expect("parent");
        let root = parent.path().join("home");
        FAIL_NEXT_PARENT_SYNC.with(|fail| fail.set(true));
        let error = ensure_home_layout(&root, None).expect_err("parent sync failure");
        assert_eq!(error.kind(), ConfigErrorKind::Io);
    }

    #[test]
    fn foreign_owned_final_directory_is_not_changed() {
        if rustix::process::geteuid().as_raw() != 0 {
            eprintln!("skip: safe foreign-owner fixture requires euid 0");
            return;
        }
        let parent = tempfile::tempdir().expect("parent");
        let root = parent.path().join("foreign");
        fs::create_dir(&root).expect("foreign directory");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("mode");
        std::os::unix::fs::chown(&root, Some(65_534), None).expect("chown fixture");
        let layout = HomeLayout::from_root(&root).expect("layout");

        let error = super::ensure_home_layout(layout.root(), None).expect_err("foreign owner");
        assert_eq!(error.kind(), ConfigErrorKind::AccessControl);
        let metadata = fs::metadata(&root).expect("metadata");
        assert_eq!(metadata.uid(), 65_534);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o755);
    }

    #[test]
    fn wrong_case_fixed_child_is_rejected_on_case_sensitive_filesystems() {
        let parent = tempfile::tempdir().expect("parent");
        let root = parent.path().join("home");
        fs::create_dir(&root).expect("root");
        fs::create_dir(root.join("Plugins")).expect("different sibling");
        let error = ensure_home_layout(&root, None).expect_err("wrong-case child");
        assert_eq!(error.kind(), ConfigErrorKind::AccessControl);
        let names = fs::read_dir(&root)
            .expect("listing")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<Vec<_>>();
        assert!(
            names.iter().any(|name| name == "Plugins"),
            "wrong-case sibling must remain {names:?}"
        );
        assert!(
            names.iter().all(|name| name != "plugins"),
            "must not create an exact plugins name {names:?}"
        );
    }

    #[test]
    fn intermediate_prefix_symlink_is_followed_when_trailing_component_is_real() {
        let parent = tempfile::tempdir().expect("parent");
        let real_base = parent.path().join("real-base");
        fs::create_dir(&real_base).expect("real base");
        fs::create_dir(real_base.join("real")).expect("real");
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&real_base, &link).expect("prefix symlink");
        let root = link.join("real").join("home");

        ensure_home_layout(&root, None).expect("prefix symlink followed");

        assert!(real_base.join("real").join("home").is_dir());
        assert!(real_base.join("real").join("home").join("plugins").is_dir());
        assert!(!link.join("home").exists());
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn apple_directory_sync_uses_fullfsync_helper() {
        let helper: fn(&std::fs::File) -> Result<(), crate::ConfigError> = super::sync_directory;
        let _ = helper;
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn apple_same_volume_distinct_mount_names_are_rejected() {
        assert!(!super::apple_mount_identity_matches(
            true,
            &[b'/' as std::ffi::c_char, 0],
            &[b'/' as std::ffi::c_char, b'm' as std::ffi::c_char, 0],
        ));
        assert!(super::apple_mount_identity_matches(
            true,
            &[b'/' as std::ffi::c_char, 0],
            &[b'/' as std::ffi::c_char, 0],
        ));
    }
}
