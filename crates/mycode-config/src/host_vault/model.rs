//! Defines and validates the private secret-bearing Host-vault model.
//!
//! The owned-file layer owns input in a `Zeroizing<Vec<u8>>`; this parser only
//! borrows that input. Decoded secrets and serialization intermediates use
//! zeroizing allocations. Every return path drops these owners before the input
//! or replacement leaves its scope.

mod parser;
mod serializer;

use std::borrow::Cow;
use std::fmt::{self, Debug, Formatter};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Serialize, Serializer};
use zeroize::Zeroizing;

use super::{HOST_VAULT_FORMAT_VERSION, HOST_VAULT_KIND, VaultRevision};
use crate::authority::is_valid_sha256_digest;
use crate::home::is_valid_portable_id;
use crate::{ConfigError, ConfigErrorKind, PluginFamily};

pub(super) const MAX_ACTIVE_CREDENTIALS: usize = 64;
// Format v1 originally allowed 64 active plus 256 revoked credentials.
pub(super) const MAX_CREDENTIALS: usize = 320;
pub(super) const MAX_GRANTS: usize = 1024;
pub(super) const MAX_SECRET_BYTES: usize = 8192;
// Eight KiB encodes to at most 10,923 unpadded Base64 characters.
const MAX_ENCODED_SECRET_BYTES: usize = 10_923;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct VaultDocument<'a> {
    pub(super) format_version: u32,
    pub(super) kind: Cow<'a, str>,
    pub(super) revision: u64,
    pub(super) credentials: Vec<Credential<'a>>,
    pub(super) grants: Vec<Grant<'a>>,
}

impl VaultDocument<'_> {
    pub(super) fn revision(&self) -> VaultRevision {
        VaultRevision(self.revision)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Credential<'a> {
    pub(super) service_id: Cow<'a, str>,
    pub(super) account_id: Cow<'a, str>,
    pub(super) issuer_id: Cow<'a, str>,
    pub(super) auth_schema_id: Cow<'a, str>,
    pub(super) credential_version: CredentialVersion,
    pub(super) state: CredentialState,
    pub(super) secret_base64_url: Option<Secret>,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(super) enum CredentialState {
    Active,
    Revoked,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Grant<'a> {
    pub(super) consumer_family: ConsumerFamily,
    pub(super) manager_id: Cow<'a, str>,
    pub(super) pack_id: Cow<'a, str>,
    pub(super) operation_id: Cow<'a, str>,
    pub(super) service_id: Cow<'a, str>,
    pub(super) account_id: Cow<'a, str>,
    pub(super) authority_digest: Cow<'a, str>,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(super) enum ConsumerFamily {
    Providers,
    Web,
    Usage,
}

impl ConsumerFamily {
    pub(super) const fn plugin_family(self) -> PluginFamily {
        match self {
            Self::Providers => PluginFamily::Providers,
            Self::Web => PluginFamily::Web,
            Self::Usage => PluginFamily::Usage,
        }
    }

    pub(super) const fn sort_key(self) -> &'static str {
        match self {
            Self::Providers => "providers",
            Self::Web => "web",
            Self::Usage => "usage",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CredentialVersion(u64);

impl CredentialVersion {
    pub(super) const FIRST: Self = Self(1);

    pub(super) fn new(value: u64) -> Result<Self, ConfigError> {
        if !(1..=i64::MAX as u64).contains(&value) {
            return Err(authority_error());
        }
        Ok(Self(value))
    }

    pub(super) const fn get(self) -> u64 {
        self.0
    }

    pub(super) fn checked_increment(self) -> Result<Self, ConfigError> {
        self.0
            .checked_add(1)
            .filter(|value| *value <= i64::MAX as u64)
            .map(Self)
            .ok_or_else(|| ConfigError::new(ConfigErrorKind::RevisionExhausted))
    }
}

impl Serialize for CredentialVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(self.get())
    }
}

pub(super) struct Secret(Zeroizing<Vec<u8>>);

impl Secret {
    pub(super) fn from_input(bytes: Zeroizing<Vec<u8>>) -> Result<Self, ConfigError> {
        if !(1..=MAX_SECRET_BYTES).contains(&bytes.len()) {
            return Err(authority_error());
        }
        Ok(Self(bytes))
    }
}

impl Debug for Secret {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([REDACTED])")
    }
}

impl Serialize for Secret {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let encoded_length = unpadded_base64_length(self.0.len())
            .ok_or_else(|| serde::ser::Error::custom("secret encoding failed"))?;
        let mut encoded = Zeroizing::new(vec![0_u8; encoded_length]);
        let written = URL_SAFE_NO_PAD
            .encode_slice(self.0.as_slice(), encoded.as_mut_slice())
            .map_err(|_| serde::ser::Error::custom("secret encoding failed"))?;
        if written != encoded_length {
            return Err(serde::ser::Error::custom("secret encoding failed"));
        }
        let ascii = std::str::from_utf8(encoded.as_slice())
            .map_err(|_| serde::ser::Error::custom("secret encoding failed"))?;
        serializer.serialize_str(ascii)
    }
}

fn unpadded_base64_length(decoded_length: usize) -> Option<usize> {
    let complete = decoded_length.checked_div(3)?.checked_mul(4)?;
    complete.checked_add(match decoded_length % 3 {
        0 => 0,
        1 => 2,
        2 => 3,
        _ => unreachable!("remainder modulo three is bounded"),
    })
}

fn decode_secret(encoded: &str) -> Result<Secret, ()> {
    if encoded.is_empty() || encoded.len() > MAX_ENCODED_SECRET_BYTES {
        return Err(());
    }
    let decoded_capacity = encoded
        .len()
        .checked_div(4)
        .and_then(|groups| groups.checked_mul(3))
        .and_then(|bytes| {
            bytes.checked_add(match encoded.len() % 4 {
                0 => 0,
                2 => 1,
                3 => 2,
                _ => return None,
            })
        })
        .ok_or(())?;
    if !(1..=MAX_SECRET_BYTES).contains(&decoded_capacity) {
        return Err(());
    }

    let mut decoded = Zeroizing::new(vec![0_u8; decoded_capacity]);
    let written = URL_SAFE_NO_PAD
        .decode_slice(encoded.as_bytes(), decoded.as_mut_slice())
        .map_err(|_| ())?;
    if written != decoded_capacity {
        return Err(());
    }
    Ok(Secret(decoded))
}

pub(super) fn parse_vault(bytes: &[u8]) -> Result<VaultDocument<'_>, ConfigError> {
    std::str::from_utf8(bytes).map_err(|_| ConfigError::new(ConfigErrorKind::NonUtf8))?;
    if bytes.contains(&b'\\') {
        return Err(authority_error());
    }

    parser::deserialize_vault(bytes)
}

pub(super) fn validate_document(document: &VaultDocument<'_>) -> Result<(), ConfigError> {
    if document.format_version != HOST_VAULT_FORMAT_VERSION
        || document.kind != HOST_VAULT_KIND
        || document.revision > i64::MAX as u64
    {
        return Err(authority_error());
    }

    let mut active = 0_usize;
    for credential in &document.credentials {
        validate_local_id(&credential.service_id)?;
        validate_local_id(&credential.account_id)?;
        validate_local_id(&credential.issuer_id)?;
        validate_local_id(&credential.auth_schema_id)?;
        match (credential.state, credential.secret_base64_url.as_ref()) {
            (CredentialState::Active, Some(_)) => active += 1,
            (CredentialState::Revoked, None) => {}
            _ => return Err(authority_error()),
        }
    }
    if active > MAX_ACTIVE_CREDENTIALS || document.credentials.len() > MAX_CREDENTIALS {
        return Err(authority_error());
    }
    if document
        .credentials
        .windows(2)
        .any(|pair| credential_key(&pair[0]) >= credential_key(&pair[1]))
    {
        return Err(authority_error());
    }

    if document.grants.len() > MAX_GRANTS {
        return Err(authority_error());
    }
    for grant in &document.grants {
        validate_grant(grant)?;
        let references = document
            .credentials
            .iter()
            .filter(|credential| {
                credential.state == CredentialState::Active
                    && credential.service_id == grant.service_id
                    && credential.account_id == grant.account_id
            })
            .count();
        if references != 1 {
            return Err(authority_error());
        }
    }
    if document
        .grants
        .windows(2)
        .any(|pair| grant_key(&pair[0]) >= grant_key(&pair[1]))
    {
        return Err(authority_error());
    }
    Ok(())
}

pub(super) fn validate_grant(grant: &Grant<'_>) -> Result<(), ConfigError> {
    validate_grant_coordinates(
        grant.consumer_family,
        &grant.manager_id,
        &grant.pack_id,
        &grant.operation_id,
    )?;
    validate_local_id(&grant.service_id)?;
    validate_local_id(&grant.account_id)?;
    if !is_valid_sha256_digest(&grant.authority_digest) {
        return Err(authority_error());
    }
    Ok(())
}

pub(super) fn validate_grant_coordinates(
    consumer_family: ConsumerFamily,
    manager_id: &str,
    pack_id: &str,
    operation_id: &str,
) -> Result<(), ConfigError> {
    if !is_valid_operation_id(operation_id) {
        return Err(authority_error());
    }
    if manager_id != consumer_family.plugin_family().id() || !is_valid_portable_id(pack_id) {
        return Err(authority_error());
    }
    Ok(())
}

/// Returns whether `value` is a canonical declarative operation key.
///
/// The grammar is 1 through 128 ASCII bytes starting with a lowercase letter,
/// continuing with lowercase letters, digits, or single `.`, `_`, `-`
/// separators, and ending with an alphanumeric byte.
fn is_valid_operation_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        && !bytes
            .last()
            .is_some_and(|byte| matches!(byte, b'.' | b'_' | b'-'))
        && !bytes.windows(2).any(|pair| {
            matches!(pair[0], b'.' | b'_' | b'-') && matches!(pair[1], b'.' | b'_' | b'-')
        })
}

pub(super) fn validate_local_id(value: &str) -> Result<(), ConfigError> {
    let bytes = value.as_bytes();
    if !(1..=128).contains(&bytes.len())
        || !bytes[0].is_ascii_lowercase()
        || !bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        || matches!(bytes.last(), Some(b'.' | b'_' | b'-'))
        || bytes.windows(2).any(|pair| {
            matches!(pair[0], b'.' | b'_' | b'-') && matches!(pair[1], b'.' | b'_' | b'-')
        })
    {
        return Err(authority_error());
    }
    Ok(())
}

fn credential_key<'a>(credential: &'a Credential<'a>) -> (&'a [u8], &'a [u8]) {
    (
        credential.service_id.as_bytes(),
        credential.account_id.as_bytes(),
    )
}

fn grant_key<'a>(grant: &'a Grant<'a>) -> (&'a [u8], &'a [u8], &'a [u8], &'a [u8]) {
    (
        grant.consumer_family.sort_key().as_bytes(),
        grant.manager_id.as_bytes(),
        grant.pack_id.as_bytes(),
        grant.operation_id.as_bytes(),
    )
}

fn classify_json_error(error: serde_json::Error) -> ConfigError {
    if error.is_syntax() || error.is_eof() {
        ConfigError::new(ConfigErrorKind::InvalidJson)
    } else {
        authority_error()
    }
}

fn authority_error() -> ConfigError {
    ConfigError::new(ConfigErrorKind::AuthorityValidation)
}

pub(super) use serializer::serialize_document;
#[cfg(test)]
pub(super) use serializer::{serialization_count_for_test, serialize_for_test};

#[cfg(test)]
mod tests {
    use super::decode_secret;

    #[test]
    fn secret_debug_is_redacted() {
        let secret = decode_secret("c2VudGluZWw").expect("secret");
        let rendered = format!("{secret:?}");
        assert_eq!(rendered, "Secret([REDACTED])");
        assert!(!rendered.contains("sentinel"));
    }
}
