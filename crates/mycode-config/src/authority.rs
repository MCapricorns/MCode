//! Shared strict authority value types reused by every persisted document.
//!
//! Each type owns one frozen grammar and fails closed on any noncanonical
//! spelling. Documents built on these values keep their own exact schemas;
//! this module defines no file format.

use std::fmt::{self, Display, Formatter};

use semver::Version;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use crate::{ConfigError, ConfigErrorKind};

const MAX_CANONICAL_VERSION_BYTES: usize = 128;
const MAX_AUTHORITY_REVISION: u64 = i64::MAX as u64;
const SHA256_PREFIX: &str = "sha256:";
const SHA256_HEX_LENGTH: usize = 64;

/// Identifies a logical or persisted authority document revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorityRevision(u64);

impl AuthorityRevision {
    /// Logical revision used when the authority document is absent.
    pub const ABSENT: Self = Self(0);

    /// Creates a bounded authority revision, including logical absence.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] above `i64::MAX`.
    pub fn new(value: u64) -> Result<Self, ConfigError> {
        if value > MAX_AUTHORITY_REVISION {
            return Err(authority_error());
        }
        Ok(Self(value))
    }

    /// Returns the numeric revision.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    pub(crate) fn checked_next(self) -> Result<Self, ConfigError> {
        if self.0 >= MAX_AUTHORITY_REVISION {
            return Err(ConfigError::new(ConfigErrorKind::RevisionExhausted));
        }
        Ok(Self(self.0 + 1))
    }
}

/// Identifies one signed source binding without assigning signed semantics.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceBindingId(String);

impl SourceBindingId {
    /// Parses a strict portable source binding ID.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] when `value` is not 1
    /// through 64 ASCII bytes matching the frozen lowercase grammar or is a
    /// DOS reserved device name.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ConfigError> {
        let value = value.as_ref();
        let bytes = value.as_bytes();
        let valid_length = (1..=64).contains(&bytes.len());
        let valid_first = bytes.first().is_some_and(u8::is_ascii_lowercase);
        let valid_tail = bytes.last().is_some_and(u8::is_ascii_alphanumeric);
        let valid_bytes = bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-');
        let valid_hyphens = !bytes.windows(2).any(|pair| pair == b"--");
        if !valid_length
            || !valid_first
            || !valid_tail
            || !valid_bytes
            || !valid_hyphens
            || is_dos_reserved(value)
        {
            return Err(authority_error());
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the opaque source binding ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SourceBindingId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SourceBindingId")
            .field(&self.0)
            .finish()
    }
}

impl Display for SourceBindingId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for SourceBindingId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

/// Contains a canonical SemVer spelling.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalVersion(String);

impl CanonicalVersion {
    /// Parses an exact canonical SemVer value.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] for invalid SemVer or
    /// any accepted spelling that differs from its canonical rendering.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ConfigError> {
        let value = value.as_ref();
        if value.is_empty() || value.len() > MAX_CANONICAL_VERSION_BYTES || !value.is_ascii() {
            return Err(authority_error());
        }
        let parsed = Version::parse(value).map_err(|_| authority_error())?;
        if parsed.to_string() != value {
            return Err(authority_error());
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the canonical SemVer spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for CanonicalVersion {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, formatter)
    }
}

impl Serialize for CanonicalVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Contains a lowercase `sha256:` artifact digest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    /// Parses the exact lowercase SHA-256 digest spelling.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] unless `value` is
    /// `sha256:` followed by exactly 64 lowercase hexadecimal digits.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ConfigError> {
        let value = value.as_ref();
        if !is_valid_sha256_digest(value) {
            return Err(authority_error());
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the canonical digest spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for Sha256Digest {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

/// Selects one canonical active artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRef {
    version: CanonicalVersion,
    digest: Sha256Digest,
}

impl ArtifactRef {
    /// Creates an active artifact selection.
    #[must_use]
    pub fn new(version: CanonicalVersion, digest: Sha256Digest) -> Self {
        Self { version, digest }
    }

    /// Returns the selected canonical version.
    #[must_use]
    pub fn version(&self) -> &CanonicalVersion {
        &self.version
    }

    /// Returns the selected artifact digest.
    #[must_use]
    pub fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

impl Serialize for ArtifactRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("ArtifactRef", 2)?;
        state.serialize_field("version", &self.version)?;
        state.serialize_field("digest", &self.digest)?;
        state.end()
    }
}

/// Records the accepted signed manifest high-water mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustHighWater {
    sequence: u64,
    manifest_digest: Sha256Digest,
}

impl TrustHighWater {
    /// Creates a positive signed-manifest high-water mark.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] when `sequence` is not
    /// in `1..=i64::MAX`.
    pub fn new(sequence: u64, manifest_digest: Sha256Digest) -> Result<Self, ConfigError> {
        if sequence == 0 || sequence > MAX_AUTHORITY_REVISION {
            return Err(authority_error());
        }
        Ok(Self {
            sequence,
            manifest_digest,
        })
    }

    /// Returns the signed sequence number.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the signed manifest digest.
    #[must_use]
    pub fn manifest_digest(&self) -> &Sha256Digest {
        &self.manifest_digest
    }
}

impl Serialize for TrustHighWater {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("TrustHighWater", 2)?;
        state.serialize_field("sequence", &self.sequence)?;
        state.serialize_field("manifestDigest", &self.manifest_digest)?;
        state.end()
    }
}

pub(crate) fn authority_error() -> ConfigError {
    ConfigError::new(ConfigErrorKind::AuthorityValidation)
}

pub(crate) fn is_valid_sha256_digest(value: &str) -> bool {
    value.strip_prefix(SHA256_PREFIX).is_some_and(|hex| {
        hex.len() == SHA256_HEX_LENGTH
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn is_dos_reserved(value: &str) -> bool {
    matches!(value, "con" | "prn" | "aux" | "nul")
        || value
            .strip_prefix("com")
            .or_else(|| value.strip_prefix("lpt"))
            .is_some_and(|unit| matches!(unit, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
}
