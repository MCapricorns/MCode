//! Secures the eager owned-home directory bootstrap.
//!
//! The module creates only the owned root and `plugins/`. Existing ancestors
//! outside the owned boundary may be followed, while the owned root and child
//! are opened or created relative to a trusted ancestor without following
//! links. Platform implementations verify
//! ownership, apply private access control, and durably publish newly created
//! directories. The owned-file machinery adds bounded reads, persistent
//! locks, and handle-relative atomic replacement without defining any
//! document schema; Host-substrate stores consume it publicly.

use std::path::Path;

use crate::{ConfigError, HomeLayout};

pub(crate) mod owned_file;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use unix::staging as staging_platform;
#[cfg(windows)]
pub(crate) use windows::staging as staging_platform;

#[cfg(not(any(unix, windows)))]
pub(crate) mod fallback_staging;
#[cfg(not(any(unix, windows)))]
pub(crate) use fallback_staging as staging_platform;

/// Identifies the owned object represented by access-control evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnedKind {
    /// An owned directory.
    Directory,
    /// An owned regular file, including a persistent lock file.
    File,
}

/// Explains why native access-control evidence is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeUnavailableReason {
    /// The running platform has no supported native implementation.
    NotApplicable,
    /// The process lacked permission to query the native control.
    InsufficientPrivilege,
    /// A native query failed or the object was not a supported owned kind.
    QueryFailed,
}

/// Reports native evidence without treating unavailability as success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessControlEvidence {
    /// Unix ownership mode observed for an owned object.
    UnixMode {
        /// Object kind represented by this evidence.
        kind: OwnedKind,
        /// Permission bits after masking to `0o777`.
        mode: u32,
    },
    /// Windows owner and protected-DACL evidence for an owned object.
    WindowsProtectedDacl {
        /// Object kind represented by this evidence.
        kind: OwnedKind,
        /// The owner is the current user or `SYSTEM`.
        owner_allowed: bool,
        /// The owner is explicitly the current user.
        owner_current_user: bool,
        /// The owner is explicitly `SYSTEM`.
        owner_system: bool,
        /// An exact, non-inherited current-user full-control ACE exists.
        current_user: bool,
        /// An exact, non-inherited `SYSTEM` full-control ACE exists.
        system: bool,
        /// `SE_DACL_PROTECTED` is set.
        protected: bool,
        /// Total ACE count in the DACL.
        ace_count: u32,
        /// ACEs not belonging to the exact current-user/`SYSTEM` allow set.
        extra_aces: u32,
    },
    /// Native evidence could not be collected.
    Unavailable {
        /// Platform that attempted the probe.
        platform: &'static str,
        /// Reason evidence was unavailable.
        reason: NativeUnavailableReason,
    },
}

/// Creates and secures the exact eager owned-home directory set.
///
/// The operation creates the home root and `plugins/` only. Existing owned
/// directories are tightened to the platform contract. All other layout paths
/// remain lazy and absent. A root derived from a user home rejects wrong-case
/// `.mcode` aliases; every layout rejects wrong-case `plugins` aliases.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::LinkEscape`] when the opened directory itself or
/// an owned child name is a symlink or reparse point,
/// [`ConfigErrorKind::AccessControl`] for an ownership or native access-control
/// failure, and [`ConfigErrorKind::Io`] for other directory or durability
/// failures.
pub fn ensure_home_layout(home: &HomeLayout) -> Result<(), ConfigError> {
    platform_ensure_home_layout(home.root(), home.expected_root_name())
}

/// Observes native owned-object access control without modifying the path.
///
/// Unavailable evidence is returned explicitly and must not be interpreted as
/// successful validation.
#[must_use]
pub fn probe_access_control(path: &Path) -> AccessControlEvidence {
    platform_probe_access_control(path)
}

#[cfg(unix)]
use unix::{
    ensure_home_layout as platform_ensure_home_layout,
    probe_access_control as platform_probe_access_control,
};

#[cfg(windows)]
use windows::{
    ensure_home_layout as platform_ensure_home_layout,
    probe_access_control as platform_probe_access_control,
};

#[cfg(not(any(unix, windows)))]
fn platform_ensure_home_layout(
    _root: &Path,
    _expected_root_name: Option<&str>,
) -> Result<(), ConfigError> {
    Err(ConfigError::new(crate::ConfigErrorKind::AccessControl))
}

#[cfg(not(any(unix, windows)))]
fn platform_probe_access_control(_path: &Path) -> AccessControlEvidence {
    AccessControlEvidence::Unavailable {
        platform: std::env::consts::OS,
        reason: NativeUnavailableReason::NotApplicable,
    }
}
