//! Darwin O_EXEC launch descriptor bound to the retained readable pin.
//!
//! libc 0.2.189 Darwin `O_EXEC` is `0x40000000`. rustix 1.1.4 `OFlags` has no
//! `O_EXEC`, so launch uses `libc::open` rather than `rustix::fs::open`. The
//! retained pin stays `O_RDONLY` for digest rechecks. After file actions the
//! only `posix_spawn` path is `/dev/fd/3` (`HOLD_FD`); there is no
//! canonical-path fallback.
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

use super::PinnedImage;
use super::normalize_spawn_source;
use crate::tool::ToolError;

/// libc 0.2.189 Darwin: `O_EXEC | O_CLOEXEC | O_NOFOLLOW`.
///
/// `O_EXEC=0x40000000`, `O_CLOEXEC=0x01000000`, `O_NOFOLLOW=0x100`. An
/// `O_RDONLY` descriptor is not a viable Darwin executable fd; `posix_spawn`
/// of `/dev/fd/<read-fd>` returns `EACCES`.
const EXEC_LAUNCH_FLAGS: libc::c_int = libc::O_EXEC | libc::O_CLOEXEC | libc::O_NOFOLLOW;

/// Opens an executable-capable descriptor for `path` and proves it is the
/// same regular vnode as `pinned`'s retained readable fd.
///
/// The descriptor is normalized above child `dup2` targets so later
/// `adddup2` of stdin/stdout/stderr/hold cannot clobber it.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] when the path cannot be opened as
/// `O_EXEC`, is not a regular file, or does not match the retained fd.
pub(super) fn bind_exec_launch_fd(pinned: &PinnedImage) -> Result<OwnedFd, ToolError> {
    let launch = normalize_spawn_source(
        open_exec_launch_fd(&pinned.canonical_path)?,
        "executable launch",
    )?;
    let launch_stat = fstat_raw_fd(launch.as_raw_fd(), "executable launch")?;
    if (launch_stat.st_mode & libc::S_IFMT) != libc::S_IFREG {
        return Err(ToolError::Execution(
            "executable launch descriptor is not a regular file".into(),
        ));
    }
    let pin_stat = fstat_raw_fd(pinned.file.as_raw_fd(), "retained executable")?;
    let launch_device = crate::builtin::fs_search::unix_device_identity(launch_stat.st_dev)
        .map_err(|error| {
            ToolError::Execution(format!(
                "executable launch device identity is unavailable: {error}"
            ))
        })?;
    let launch_inode =
        crate::builtin::fs_search::unix_inode_identity(launch_stat.st_ino).map_err(|error| {
            ToolError::Execution(format!(
                "executable launch inode identity is unavailable: {error}"
            ))
        })?;
    let pin_device =
        crate::builtin::fs_search::unix_device_identity(pin_stat.st_dev).map_err(|error| {
            ToolError::Execution(format!(
                "retained executable device identity is unavailable: {error}"
            ))
        })?;
    let pin_inode =
        crate::builtin::fs_search::unix_inode_identity(pin_stat.st_ino).map_err(|error| {
            ToolError::Execution(format!(
                "retained executable inode identity is unavailable: {error}"
            ))
        })?;
    if launch_device != pin_device || launch_inode != pin_inode {
        return Err(ToolError::Execution(
            "executable launch descriptor identity does not match the retained executable".into(),
        ));
    }
    Ok(launch)
}

fn open_exec_launch_fd(path: &Path) -> Result<OwnedFd, ToolError> {
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        ToolError::Execution("canonical executable path contains an interior NUL".into())
    })?;
    // SAFETY: `c_path` is a NUL-terminated pathname. Flags are the Darwin
    // `O_EXEC|O_CLOEXEC|O_NOFOLLOW` combination from libc 0.2.189; `O_CREAT`
    // is absent, so the variadic mode argument is omitted. `open` returns
    // `-1` on failure; a non-negative fd is uniquely owned here. No ANSI or
    // ordinary-path spawn fallback exists if this open fails.
    let raw = unsafe { libc::open(c_path.as_ptr(), EXEC_LAUNCH_FLAGS) };
    if raw == -1 {
        return Err(ToolError::Execution(format!(
            "executable launch descriptor could not be opened: {}",
            io::Error::last_os_error()
        )));
    }
    // SAFETY: `open` returned a fresh descriptor that this function uniquely owns.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

fn fstat_raw_fd(fd: RawFd, what: &str) -> Result<libc::stat, ToolError> {
    // SAFETY: `stat` is a C POD filled by `fstat`.
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    // SAFETY: `fd` is a live descriptor; `stat` is the documented output buffer.
    // A non-zero return is failure; errno is captured immediately.
    if unsafe { libc::fstat(fd, &raw mut stat) } != 0 {
        return Err(ToolError::Execution(format!(
            "{what} identity is unavailable: {}",
            io::Error::last_os_error()
        )));
    }
    Ok(stat)
}
