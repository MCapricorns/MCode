//! Shared strict authority value types reused by every persisted document.
//!
//! Each type owns one frozen grammar and fails closed on any noncanonical
//! spelling. Documents built on these values keep their own exact schemas;
//! this module defines no file format.

use std::fmt::{self, Display, Formatter};

use semver::Version;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

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

    pub(crate) fn into_string(self) -> String {
        self.0
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

pub(crate) fn parse_active(value: Value) -> Result<ArtifactRef, ConfigError> {
    let mut active = exact_object(value, &["version", "digest"])?;
    let version = CanonicalVersion::parse(take_string(&mut active, "version")?)?;
    let digest = Sha256Digest::parse(take_string(&mut active, "digest")?)?;
    Ok(ArtifactRef::new(version, digest))
}

pub(crate) fn parse_trust_high_water(value: Value) -> Result<TrustHighWater, ConfigError> {
    let mut trust = exact_object(value, &["sequence", "manifestDigest"])?;
    let sequence = take_positive_u64(&mut trust, "sequence")?;
    let digest = Sha256Digest::parse(take_string(&mut trust, "manifestDigest")?)?;
    TrustHighWater::new(sequence, digest)
}

pub(crate) fn exact_object(
    value: Value,
    fields: &[&str],
) -> Result<Map<String, Value>, ConfigError> {
    let Value::Object(object) = value else {
        return Err(authority_error());
    };
    if object.len() != fields.len() || !fields.iter().all(|field| object.contains_key(*field)) {
        return Err(authority_error());
    }
    Ok(object)
}

pub(crate) fn take_string(
    object: &mut Map<String, Value>,
    field: &str,
) -> Result<String, ConfigError> {
    object
        .remove(field)
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(authority_error)
}

pub(crate) fn take_u32(object: &mut Map<String, Value>, field: &str) -> Result<u32, ConfigError> {
    object
        .remove(field)
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(authority_error)
}

pub(crate) fn take_positive_revision(
    object: &mut Map<String, Value>,
    field: &str,
) -> Result<AuthorityRevision, ConfigError> {
    AuthorityRevision::new(take_positive_u64(object, field)?)
}

pub(crate) fn take_positive_u64(
    object: &mut Map<String, Value>,
    field: &str,
) -> Result<u64, ConfigError> {
    let value = object
        .remove(field)
        .and_then(|value| value.as_u64())
        .ok_or_else(authority_error)?;
    if value == 0 || value > MAX_AUTHORITY_REVISION {
        return Err(authority_error());
    }
    Ok(value)
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

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn source_version_digest_and_high_water_are_strict() {
        for invalid in [
            "",
            "A",
            "a_1",
            "a--b",
            "a-",
            "1a",
            "é",
            "con",
            "prn",
            "aux",
            "nul",
            "com1",
            "lpt9",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklm",
        ] {
            assert_eq!(
                SourceBindingId::parse(invalid)
                    .expect_err("invalid source")
                    .kind(),
                ConfigErrorKind::AuthorityValidation
            );
        }
        for invalid in [
            "1", "01.2.3", "1.02.3", "1.2.03", "v1.2.3", "1.2.3-01", "1.2.3 ",
        ] {
            assert_eq!(
                CanonicalVersion::parse(invalid)
                    .expect_err("noncanonical version")
                    .kind(),
                ConfigErrorKind::AuthorityValidation
            );
        }
        assert_eq!(
            CanonicalVersion::parse(format!("1.2.3+{}", "a".repeat(129)))
                .expect_err("oversized version")
                .kind(),
            ConfigErrorKind::AuthorityValidation
        );
        for invalid in [
            "",
            "sha256:",
            "SHA256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "sha256:0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef",
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde",
        ] {
            assert_eq!(
                Sha256Digest::parse(invalid)
                    .expect_err("invalid digest")
                    .kind(),
                ConfigErrorKind::AuthorityValidation
            );
        }
        assert_eq!(
            TrustHighWater::new(0, Sha256Digest::parse(DIGEST).expect("digest"))
                .expect_err("zero sequence")
                .kind(),
            ConfigErrorKind::AuthorityValidation
        );
        assert_eq!(
            TrustHighWater::new(
                i64::MAX as u64 + 1,
                Sha256Digest::parse(DIGEST).expect("digest"),
            )
            .expect_err("oversized sequence")
            .kind(),
            ConfigErrorKind::AuthorityValidation
        );
        assert_eq!(AuthorityRevision::ABSENT.get(), 0);
        assert_eq!(
            AuthorityRevision::new(i64::MAX as u64 + 1)
                .expect_err("oversized revision")
                .kind(),
            ConfigErrorKind::AuthorityValidation
        );
    }

    #[test]
    fn canonical_values_round_trip_through_their_accessors() {
        let source = SourceBindingId::parse("official-source").expect("source");
        assert_eq!(source.as_str(), "official-source");
        assert_eq!(source.to_string(), "official-source");

        let version = CanonicalVersion::parse("1.2.3-alpha.1+build.7").expect("version");
        assert_eq!(version.as_str(), "1.2.3-alpha.1+build.7");
        assert_eq!(version.to_string(), "1.2.3-alpha.1+build.7");

        let digest = Sha256Digest::parse(DIGEST).expect("digest");
        assert_eq!(digest.as_str(), DIGEST);
        assert_eq!(digest.to_string(), DIGEST);
        assert_eq!(digest.clone().into_string(), DIGEST);

        let artifact = ArtifactRef::new(version.clone(), digest.clone());
        assert_eq!(artifact.version(), &version);
        assert_eq!(artifact.digest(), &digest);

        let trust = TrustHighWater::new(7, digest.clone()).expect("high-water");
        assert_eq!(trust.sequence(), 7);
        assert_eq!(trust.manifest_digest(), &digest);

        assert_eq!(AuthorityRevision::new(5).expect("revision").get(), 5);
        assert_eq!(
            AuthorityRevision::new(5)
                .expect("revision")
                .checked_next()
                .expect("next")
                .get(),
            6
        );
    }
}
