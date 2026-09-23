//! Unix no-follow owned-directory primitives with private modes and durability.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use rustix::fs::{self as rfs, AtFlags, Mode, OFlags};
use rustix::io::Errno;

use crate::home::validate_path_component;
use crate::{ConfigError, ConfigErrorKind};

#[path = "unix_file.rs"]
pub(super) mod unix_file;

const DIRECTORY_MODE: rfs::RawMode = 0o700;

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
    open_component_with_mounts(parent, name, flags, Mode::empty())
}

/// Creates one component with the private mode in the creating call itself.
///
/// Darwin resolves a contested `O_CREAT` without `O_EXCL` into `EACCES` (after
/// another thread created a zero-mode entry) or a spurious `ENOENT`, so a
/// caller that must create passes the final mode up front and treats an
/// `EEXIST` from its `O_EXCL` attempt as a lost race to retry as an opener.
pub(super) fn create_component(
    parent: &File,
    name: &OsStr,
    flags: OFlags,
    mode: Mode,
) -> Result<std::os::fd::OwnedFd, ConfigError> {
    open_component_with_mounts(parent, name, flags, mode)
}

fn open_component_with_mounts(
    parent: &File,
    name: &OsStr,
    flags: OFlags,
    mode: Mode,
) -> Result<std::os::fd::OwnedFd, ConfigError> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let resolve = rfs::ResolveFlags::BENEATH | rfs::ResolveFlags::NO_SYMLINKS;
        rfs::openat2(parent.as_fd(), name, flags, mode, resolve).map_err(|error| {
            if error == Errno::NOSYS {
                ConfigError::new(ConfigErrorKind::AccessControl)
                    .with_io_kind(io::ErrorKind::Unsupported)
            } else {
                map_errno(error, ConfigErrorKind::Io)
            }
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        // The caller supplies one validated component and O_NOFOLLOW, so the
        // portable openat fallback remains anchored to `parent`.
        rfs::openat(parent.as_fd(), name, flags, mode)
            .map_err(|error| map_errno(error, ConfigErrorKind::Io))
    }
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
mod tests {
    #[cfg(target_vendor = "apple")]
    #[test]
    fn apple_directory_sync_uses_fullfsync_helper() {
        let helper: fn(&std::fs::File) -> Result<(), crate::ConfigError> = super::sync_directory;
        let _ = helper;
    }
}
