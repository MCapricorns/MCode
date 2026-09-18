//! Exposes Host-vault mechanics without exposing credential material.
//!
//! The private model validates complete future nonempty documents. Public APIs
//! reveal only absence or the bounded persisted revision and can create the
//! exact empty revision-zero document.

mod model;
mod reducer;

#[cfg(test)]
mod source_audit_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
use std::path::Path;

use crate::secure_fs::owned_file::read_owned_file;
use crate::{ConfigError, ConfigErrorKind, HomeLayout};

use self::model::parse_vault;

/// Maximum encoded size of Host-only `auth.json`.
pub const MAX_HOST_VAULT_BYTES: usize = 1024 * 1024;
/// Exact Host-vault document kind.
pub const HOST_VAULT_KIND: &str = "mcode-host-auth";
/// Exact Host-vault format version.
pub const HOST_VAULT_FORMAT_VERSION: u32 = 1;

const HOST_VAULT_PATH: &str = "plugins/.host/auth.json";
#[cfg(test)]
const EMPTY_VAULT_BYTES: &[u8] = b"{\"formatVersion\":1,\"kind\":\"mcode-host-auth\",\"revision\":0,\"credentials\":[],\"grants\":[]}\n";

/// Identifies a bounded persisted Host-vault revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VaultRevision(u64);

impl VaultRevision {
    /// Revision of the explicitly initialized empty vault.
    pub const EMPTY: Self = Self(0);

    /// Creates a revision in `0..=i64::MAX`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] above `i64::MAX`.
    pub fn new(value: u64) -> Result<Self, ConfigError> {
        if value > i64::MAX as u64 {
            return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
        }
        Ok(Self(value))
    }

    /// Returns the numeric revision.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Reports only whether the Host vault exists and its revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostVaultState {
    /// No Host-vault file exists.
    Absent,
    /// A complete valid Host-vault file exists.
    Present {
        /// Persisted document revision.
        revision: VaultRevision,
    },
}

/// Reads strict Host-vault status without creating filesystem objects.
///
/// Credential and grant contents remain private to this crate and are dropped
/// before this function returns.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security, size, UTF-8, strict JSON,
/// or private schema and relationship validation failures.
pub fn read_host_vault_state(home: &HomeLayout) -> Result<HostVaultState, ConfigError> {
    let target = home.host_auth_json();
    let bytes = read_owned_file(home, HOST_VAULT_PATH, MAX_HOST_VAULT_BYTES)
        .map_err(|error| error.with_path(&target))?;
    match bytes.as_deref() {
        None => Ok(HostVaultState::Absent),
        Some(bytes) => {
            let document = parse_vault(bytes).map_err(|error| error.with_path(&target))?;
            Ok(HostVaultState::Present {
                revision: document.revision(),
            })
        }
    }
}

/// Creates the exact empty revision-zero Host vault when it is absent.
///
/// The operation creates only the lazy `.host` ancestor, persistent lock, and
/// target required by the crate-private owned-file transaction. Any existing
/// bytes are parsed completely before a conflict is returned and are never
/// replaced by this operation.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::RevisionConflict`] when a valid vault already
/// exists, the strict parse error for malformed existing bytes, or
/// [`ConfigError`] for owned-path security and atomic publication failures.
pub fn initialize_empty_host_vault(home: &HomeLayout) -> Result<VaultRevision, ConfigError> {
    reducer::initialize_empty(home)
}

#[cfg(test)]
fn relative_path() -> &'static Path {
    Path::new(HOST_VAULT_PATH)
}
