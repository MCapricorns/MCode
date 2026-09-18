//! Provider scalar and bounded text validation.
use mcode_config::{ProviderId, Sha256Digest};

use crate::provider_routes::{ModelAlias, ModelId, ProviderRouteId, RequestId, TurnId};

use super::{ValidationError, ValidationResult};

pub(super) const KIB: u64 = 1_024;
pub(super) const MIB: u64 = 1_024 * KIB;
pub(super) const MAX_LOGICAL_CHARGE: u64 = 8 * MIB;
pub(super) const MAX_SAFE_TEXT_BYTES: usize = 64 * 1_024;
pub(super) const MAX_TRACKING_BYTES: usize = 128;

pub(super) fn provider_id(value: &str) -> ValidationResult {
    ProviderId::parse(value)
        .map(|_| ())
        .map_err(|_| ValidationError::InvalidArgument)
}

pub(super) fn route_id(value: &str) -> ValidationResult {
    ProviderRouteId::parse(value)
        .map(|_| ())
        .map_err(|_| ValidationError::InvalidArgument)
}

pub(super) fn model_id(value: &str) -> ValidationResult {
    ModelId::parse(value)
        .map(|_| ())
        .map_err(|_| ValidationError::InvalidArgument)
}

pub(super) fn model_alias(value: &str) -> ValidationResult {
    ModelAlias::parse(value)
        .map(|_| ())
        .map_err(|_| ValidationError::InvalidArgument)
}

pub(super) fn operation_id(value: &str) -> ValidationResult {
    if is_valid_operation_id(value) {
        Ok(())
    } else {
        Err(ValidationError::InvalidArgument)
    }
}

/// Returns whether `value` is a canonical declarative operation key.
///
/// The first byte is lowercase ASCII; remaining bytes are lowercase ASCII,
/// digits, or `.`, `_`, and `-`; separators are neither trailing nor
/// adjacent; the length is 1 through 128 bytes.
fn is_valid_operation_id(value: &str) -> bool {
    const MIN_OPERATION_ID_BYTES: usize = 1;
    const MAX_OPERATION_ID_BYTES: usize = 128;
    fn is_separator(byte: u8) -> bool {
        matches!(byte, b'.' | b'_' | b'-')
    }

    let bytes = value.as_bytes();
    (MIN_OPERATION_ID_BYTES..=MAX_OPERATION_ID_BYTES).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || is_separator(*byte))
        && !bytes.last().is_some_and(|byte| is_separator(*byte))
        && !bytes
            .windows(2)
            .any(|pair| is_separator(pair[0]) && is_separator(pair[1]))
}

pub(super) fn request_id(value: &str) -> ValidationResult {
    RequestId::parse(value)
        .map(|_| ())
        .map_err(|_| ValidationError::InvalidArgument)
}

pub(super) fn turn_id(value: &str) -> ValidationResult {
    TurnId::parse(value)
        .map(|_| ())
        .map_err(|_| ValidationError::InvalidArgument)
}

pub(super) fn tracking_id(value: &str) -> ValidationResult {
    let bytes = value.as_bytes();
    let valid = (1..=MAX_TRACKING_BYTES).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'));
    valid.then_some(()).ok_or(ValidationError::InvalidArgument)
}

pub(super) fn digest(value: &str) -> ValidationResult {
    Sha256Digest::parse(value)
        .map(|_| ())
        .map_err(|_| ValidationError::InvalidArgument)
}

pub(super) fn visible_ascii(value: &str, maximum: usize) -> ValidationResult {
    let bytes = value.as_bytes();
    if bytes.len() > maximum {
        return Err(ValidationError::Limit);
    }
    (!bytes.is_empty() && bytes.iter().all(|byte| (b'!'..=b'~').contains(byte)))
        .then_some(())
        .ok_or(ValidationError::InvalidArgument)
}

pub(super) fn safe(value: &str, maximum: usize, nonempty: bool) -> ValidationResult {
    if value.len() > maximum {
        return Err(ValidationError::Limit);
    }
    if (nonempty && value.is_empty()) || value.chars().any(is_unsafe_char) {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(())
}

pub(super) fn label(value: &str, maximum: usize) -> ValidationResult {
    safe(value, maximum, true)?;
    if value.contains(['\t', '\n']) {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(())
}

pub(super) fn stamp(value: &str, prefix: &str) -> ValidationResult {
    let suffix = value
        .strip_prefix(prefix)
        .ok_or(ValidationError::InvalidArgument)?;
    (suffix.len() == 32
        && suffix
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)))
    .then_some(())
    .ok_or(ValidationError::InvalidArgument)
}

fn is_unsafe_char(character: char) -> bool {
    let value = u32::from(character);
    matches!(value, 0x00..=0x08 | 0x0b..=0x1f | 0x7f..=0x9f)
        || matches!(
            value,
            0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069
        )
}
