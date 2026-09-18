//! Bounded identities used by the host provider-route ledger.
use std::fmt::{self, Display, Formatter};

use mcode_config::Sha256Digest;

use super::ProviderRouteError;

const MAX_ROUTE_ID_BYTES: usize = 256;
const MAX_AUTH_SLOT_ID_BYTES: usize = 64;
const MAX_MODEL_ID_BYTES: usize = 256;
const MAX_TRACKING_ID_BYTES: usize = 128;
// The target ABI represents nonnegative generations and counters as signed i64 values.
const MAX_SIGNED_INTEGER: u64 = i64::MAX as u64;

/// Identifies one globally claimed provider route.
///
/// Values contain 1 through 256 visible ASCII bytes from `!` through `~`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderRouteId(String);

impl ProviderRouteId {
    /// Parses one route identifier in the frozen visible-ASCII grammar.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderRouteError::InvalidRouteId`] when `value` violates
    /// the route identifier grammar or bound.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ProviderRouteError> {
        let value = value.as_ref();
        if !is_visible_ascii(value, MAX_ROUTE_ID_BYTES) {
            return Err(ProviderRouteError::InvalidRouteId);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the exact route identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ProviderRouteId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Identifies one globally claimed signed authentication slot.
///
/// Values contain 1 through 64 ASCII bytes. They start with a letter, end with
/// an alphanumeric byte, and otherwise use only letters, digits, or single
/// hyphens. Case is retained exactly.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthSlotId(String);

impl AuthSlotId {
    /// Parses one authentication-slot identifier in the frozen grammar.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderRouteError::InvalidAuthSlotId`] when `value` violates
    /// the authentication-slot grammar or bound.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ProviderRouteError> {
        let value = value.as_ref();
        if !is_auth_slot_id(value) {
            return Err(ProviderRouteError::InvalidAuthSlotId);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the exact authentication-slot identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for AuthSlotId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Identifies one exact current, requested, or resolved model.
///
/// Values contain 1 through 256 visible ASCII bytes from `!` through `~`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelId(String);

impl ModelId {
    /// Parses one model identifier in the frozen visible-ASCII grammar.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderRouteError::InvalidModelId`] when `value` violates
    /// the model identifier grammar or bound.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ProviderRouteError> {
        let value = value.as_ref();
        if !is_visible_ascii(value, MAX_MODEL_ID_BYTES) {
            return Err(ProviderRouteError::InvalidModelId);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the exact model identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ModelId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Identifies one exact requested model alias.
///
/// Values use the same 1-through-256 visible-ASCII grammar as [`ModelId`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelAlias(String);

impl ModelAlias {
    /// Parses one requested alias in the frozen visible-ASCII grammar.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderRouteError::InvalidModelAlias`] when `value` violates
    /// the requested-alias grammar or bound.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ProviderRouteError> {
        let value = value.as_ref();
        if !is_visible_ascii(value, MAX_MODEL_ID_BYTES) {
            return Err(ProviderRouteError::InvalidModelAlias);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the exact requested alias.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ModelAlias {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Identifies one provider request.
///
/// Values contain 1 through 128 ASCII bytes. They start and end with an
/// alphanumeric byte and otherwise use only letters, digits, `.`, `_`, `:`, or
/// `-`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(String);

impl RequestId {
    /// Parses one request identifier in the frozen tracking grammar.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderRouteError::InvalidRequestId`] when `value` violates
    /// the request identifier grammar or bound.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ProviderRouteError> {
        let value = value.as_ref();
        if !is_tracking_id(value) {
            return Err(ProviderRouteError::InvalidRequestId);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the exact request identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for RequestId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Identifies one agent turn.
///
/// Values use the same 1-through-128 ASCII tracking grammar as [`RequestId`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TurnId(String);

impl TurnId {
    /// Parses one turn identifier in the frozen tracking grammar.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderRouteError::InvalidTurnId`] when `value` violates the
    /// turn identifier grammar or bound.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ProviderRouteError> {
        let value = value.as_ref();
        if !is_tracking_id(value) {
            return Err(ProviderRouteError::InvalidTurnId);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the exact turn identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for TurnId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Identifies one nonzero bounded provider generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderGeneration(u64);

impl ProviderGeneration {
    /// Creates a generation from 1 through `i64::MAX`.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderRouteError::InvalidGeneration`] for zero or values
    /// above `i64::MAX`.
    pub fn new(value: u64) -> Result<Self, ProviderRouteError> {
        if value == 0 || value > MAX_SIGNED_INTEGER {
            return Err(ProviderRouteError::InvalidGeneration);
        }
        Ok(Self(value))
    }

    /// Returns the numeric generation.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Contains one bounded token count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenCount(u64);

impl TokenCount {
    /// Creates a token count from zero through `i64::MAX`.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderRouteError::InvalidTokenCount`] above `i64::MAX`.
    pub fn new(value: u64) -> Result<Self, ProviderRouteError> {
        if value > MAX_SIGNED_INTEGER {
            return Err(ProviderRouteError::InvalidTokenCount);
        }
        Ok(Self(value))
    }

    /// Returns the numeric token count.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Contains a canonical endpoint identity fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EndpointFingerprint(Sha256Digest);

impl EndpointFingerprint {
    /// Creates an endpoint fingerprint from a canonical SHA-256 digest.
    #[must_use]
    pub const fn new(digest: Sha256Digest) -> Self {
        Self(digest)
    }

    /// Returns the canonical SHA-256 digest.
    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.0
    }
}

/// Contains a canonical authentication-authority fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthFingerprint(Sha256Digest);

impl AuthFingerprint {
    /// Creates an authentication fingerprint from a canonical SHA-256 digest.
    #[must_use]
    pub const fn new(digest: Sha256Digest) -> Self {
        Self(digest)
    }

    /// Returns the canonical SHA-256 digest.
    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.0
    }
}

/// Contains exact optional terminal token counters.
///
/// Missing counters remain absent and are never derived from other counters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageCounters {
    input_tokens: Option<TokenCount>,
    output_tokens: Option<TokenCount>,
    cache_read_tokens: Option<TokenCount>,
    cache_write_tokens: Option<TokenCount>,
}

impl UsageCounters {
    /// Creates terminal counters while retaining every absent value.
    #[must_use]
    pub const fn new(
        input_tokens: Option<TokenCount>,
        output_tokens: Option<TokenCount>,
        cache_read_tokens: Option<TokenCount>,
        cache_write_tokens: Option<TokenCount>,
    ) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
        }
    }

    /// Creates counters with every value absent.
    #[must_use]
    pub const fn none() -> Self {
        Self::new(None, None, None, None)
    }

    /// Returns the exact optional input-token count.
    #[must_use]
    pub const fn input_tokens(&self) -> Option<TokenCount> {
        self.input_tokens
    }

    /// Returns the exact optional output-token count.
    #[must_use]
    pub const fn output_tokens(&self) -> Option<TokenCount> {
        self.output_tokens
    }

    /// Returns the exact optional cache-read-token count.
    #[must_use]
    pub const fn cache_read_tokens(&self) -> Option<TokenCount> {
        self.cache_read_tokens
    }

    /// Returns the exact optional cache-write-token count.
    #[must_use]
    pub const fn cache_write_tokens(&self) -> Option<TokenCount> {
        self.cache_write_tokens
    }
}

fn is_visible_ascii(value: &str, maximum: usize) -> bool {
    let bytes = value.as_bytes();
    (1..=maximum).contains(&bytes.len()) && bytes.iter().all(|byte| (b'!'..=b'~').contains(byte))
}

fn is_auth_slot_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=MAX_AUTH_SLOT_ID_BYTES).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        && !bytes.windows(2).any(|pair| pair == b"--")
}

fn is_tracking_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=MAX_TRACKING_ID_BYTES).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'_' | b':' | b'-'))
}
