//! Defines bounded, redacted provider errors.

use std::fmt;
use std::ops::Range;

use mcode_core::McodeError;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Maximum source characters retained in a provider error message.
const MAX_ERROR_MESSAGE_CHARS: usize = 512;
/// Maximum raw bytes scanned while constructing a provider error.
const MAX_ERROR_SCAN_BYTES: usize = 64 * 1_024;
const REDACTED: &str = "[REDACTED]";

/// Stable provider failure categories visible to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    /// The request was cancelled cooperatively.
    Cancelled,
    /// The provider is temporarily unavailable.
    Unavailable,
    /// The request exceeded its time budget.
    Timeout,
    /// The provider rejected the request.
    Rejected,
    /// The provider or adapter violated the stream contract.
    Protocol,
}

impl ProviderErrorKind {
    fn summary(self) -> &'static str {
        match self {
            Self::Cancelled => "provider request cancelled",
            Self::Unavailable => "provider unavailable",
            Self::Timeout => "provider request timed out",
            Self::Rejected => "provider request rejected",
            Self::Protocol => "provider protocol error",
        }
    }
}

/// A typed provider failure with an optional safe message.
///
/// Messages are redacted and bounded during construction and deserialization.
/// Raw input is never retained, so `Debug`, `Display`, serialization, cloning,
/// and conversion to `McodeError` cannot expose the original credential text.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ProviderError {
    kind: ProviderErrorKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl ProviderError {
    /// Creates an error without additional context.
    #[must_use]
    pub const fn new(kind: ProviderErrorKind) -> Self {
        Self {
            kind,
            message: None,
        }
    }

    /// Creates an error with immediately redacted, bounded context.
    #[must_use]
    pub fn with_message(kind: ProviderErrorKind, message: impl AsRef<str>) -> Self {
        let message = sanitize_message(message.as_ref());
        Self {
            kind,
            message: (!message.is_empty()).then_some(message),
        }
    }

    /// Returns the stable failure category.
    #[must_use]
    pub const fn kind(&self) -> ProviderErrorKind {
        self.kind
    }

    /// Returns the already-sanitized optional context.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Returns whether this error represents cancellation.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        matches!(self.kind, ProviderErrorKind::Cancelled)
    }
}

impl<'de> Deserialize<'de> for ProviderError {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Representation {
            kind: ProviderErrorKind,
            #[serde(default)]
            message: Option<String>,
        }

        let representation = Representation::deserialize(deserializer)?;
        Ok(match representation.message {
            Some(message) => Self::with_message(representation.kind, message),
            None => Self::new(representation.kind),
        })
    }
}

impl fmt::Debug for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderError")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.kind.summary())?;
        if let Some(message) = &self.message {
            write!(formatter, ": {message}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ProviderError {}

impl From<ProviderError> for McodeError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error.to_string())
    }
}

fn sanitize_message(raw: &str) -> String {
    let mut input_end = raw.len().min(MAX_ERROR_SCAN_BYTES);
    while !raw.is_char_boundary(input_end) {
        input_end -= 1;
    }
    let redacted = redact_sensitive(&raw[..input_end]);
    if redacted.chars().count() <= MAX_ERROR_MESSAGE_CHARS {
        redacted
    } else {
        let truncated: String = redacted.chars().take(MAX_ERROR_MESSAGE_CHARS).collect();
        format!("{truncated}… [truncated]")
    }
}

fn redact_sensitive(message: &str) -> String {
    if let Ok(mut value) = serde_json::from_str::<Value>(message) {
        redact_json(&mut value);
        return value.to_string();
    }
    redact_plain_text(message)
}

fn redact_json(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_sensitive_key(key) {
                    *value = Value::String(REDACTED.into());
                } else {
                    redact_json(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_json(value);
            }
        }
        Value::String(text) => *text = redact_plain_text(text),
        _ => {}
    }
}

/// Classifies credential-like header and field names without provider policy.
fn is_sensitive_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace(['-', '_'], "");
    normalized.ends_with("authorization")
        || normalized.ends_with("apikey")
        || normalized.ends_with("authkey")
        || normalized.ends_with("accesskey")
        || normalized.ends_with("subscriptionkey")
        || normalized == "token"
        || normalized.ends_with("token")
        || normalized.contains("secret")
        || normalized.contains("credential")
        || normalized.contains("password")
        || matches!(normalized.as_str(), "cookie" | "setcookie")
}

/// Redacts credential assignments and common standalone token forms.
///
/// Confirmed quoted values are scanned once and then skipped, keeping total
/// work linear even for malformed input containing many escaped quotes.
fn redact_plain_text(message: &str) -> String {
    let mut output = String::with_capacity(message.len());
    let mut copied_until = 0;
    let mut cursor = 0;
    while cursor < message.len() {
        let redaction = bare_sensitive_assignment_value(message, cursor)
            .or_else(|| quoted_sensitive_assignment_value(message, cursor))
            .or_else(|| standalone_bearer_value(message, cursor))
            .or_else(|| prefixed_api_key(message, cursor));
        if let Some(redaction) = redaction {
            output.push_str(&message[copied_until..redaction.start]);
            output.push_str(REDACTED);
            cursor = redaction.end;
            copied_until = redaction.end;
        } else {
            cursor = next_char_boundary(message, cursor);
        }
    }
    output.push_str(&message[copied_until..]);
    output
}

fn bare_sensitive_assignment_value(message: &str, start: usize) -> Option<Range<usize>> {
    let bytes = message.as_bytes();
    if !bytes.get(start).is_some_and(|byte| is_key_byte(*byte))
        || (start > 0 && is_key_byte(bytes[start - 1]))
    {
        return None;
    }
    let mut key_end = start + 1;
    while bytes.get(key_end).is_some_and(|byte| is_key_byte(*byte)) {
        key_end += 1;
    }
    let key = &message[start..key_end];
    is_sensitive_key(key).then(|| assignment_value(message, key_end, key))?
}

fn quoted_sensitive_assignment_value(message: &str, closing: usize) -> Option<Range<usize>> {
    let bytes = message.as_bytes();
    let quote = match bytes.get(closing).copied()? {
        quote @ (b'"' | b'\'') => quote,
        _ => return None,
    };
    let mut key_start = closing;
    while key_start > 0 && is_key_byte(bytes[key_start - 1]) {
        key_start -= 1;
    }
    let opening = key_start.checked_sub(1)?;
    if bytes[opening] != quote {
        return None;
    }
    let key = &message[key_start..closing];
    is_sensitive_key(key).then(|| assignment_value(message, closing + 1, key))?
}

fn assignment_value(message: &str, after_key: usize, key: &str) -> Option<Range<usize>> {
    let mut cursor = skip_ascii_whitespace(message, after_key);
    if !matches!(message.as_bytes().get(cursor), Some(b'=' | b':')) {
        return None;
    }
    cursor = skip_ascii_whitespace(message, cursor + 1);
    match message.as_bytes().get(cursor).copied() {
        Some(quote @ (b'"' | b'\'')) => {
            let start = cursor + 1;
            let end = find_closing_quote(message, cursor, quote).unwrap_or(message.len());
            Some(start..end)
        }
        _ => {
            let end = if is_cookie_key(key) || is_authorization_key(key) {
                line_value_end(message, cursor)
            } else {
                field_value_end(message, cursor)
            };
            Some(cursor..end)
        }
    }
}

fn find_closing_quote(message: &str, open: usize, quote: u8) -> Option<usize> {
    let mut cursor = open + 1;
    let mut escaped = false;
    while cursor < message.len() {
        let byte = message.as_bytes()[cursor];
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == quote {
            return Some(cursor);
        }
        cursor += 1;
    }
    None
}

fn bearer_value_end(message: &str, start: usize) -> Option<usize> {
    const BEARER: &str = "Bearer";
    let scheme_end = start.checked_add(BEARER.len())?;
    if !message.get(start..scheme_end)?.eq_ignore_ascii_case(BEARER)
        || !message
            .as_bytes()
            .get(scheme_end)
            .is_some_and(u8::is_ascii_whitespace)
    {
        return None;
    }
    let credential_start = skip_ascii_whitespace(message, scheme_end);
    match message.as_bytes().get(credential_start).copied() {
        Some(quote @ (b'"' | b'\'')) => Some(
            find_closing_quote(message, credential_start, quote)
                .map_or(message.len(), |end| end + 1),
        ),
        Some(_) => Some(credential_token_end(message, credential_start)),
        None => Some(credential_start),
    }
}

fn standalone_bearer_value(message: &str, start: usize) -> Option<Range<usize>> {
    if start > 0 && is_word_byte(message.as_bytes()[start - 1]) {
        return None;
    }
    bearer_value_end(message, start).map(|end| start..end)
}

fn prefixed_api_key(message: &str, start: usize) -> Option<Range<usize>> {
    let end = start.checked_add(3)?;
    let prefix = message.get(start..end)?;
    if !prefix.eq_ignore_ascii_case("sk-") && !prefix.eq_ignore_ascii_case("sk_") {
        return None;
    }
    Some(start..credential_token_end(message, start))
}

fn credential_token_end(message: &str, start: usize) -> usize {
    let mut cursor = start;
    while cursor < message.len() {
        if is_value_delimiter(message.as_bytes()[cursor]) {
            break;
        }
        cursor = next_char_boundary(message, cursor);
    }
    cursor
}

fn field_value_end(message: &str, start: usize) -> usize {
    let mut cursor = start;
    while cursor < message.len() {
        let byte = message.as_bytes()[cursor];
        if matches!(byte, b'\r' | b'\n')
            || (byte.is_ascii_control() && !byte.is_ascii_whitespace())
            || matches!(
                byte,
                b'&' | b',' | b';' | b'"' | b'\'' | b'}' | b']' | b'|' | b'#'
            )
        {
            break;
        }
        if byte.is_ascii_whitespace() {
            let next = skip_ascii_whitespace(message, cursor);
            if starts_assignment(message, next) {
                break;
            }
            cursor = next;
        } else {
            cursor = next_char_boundary(message, cursor);
        }
    }
    cursor
}

fn starts_assignment(message: &str, start: usize) -> bool {
    let bytes = message.as_bytes();
    let Some(first) = bytes.get(start).copied() else {
        return false;
    };
    let mut cursor = match first {
        quote @ (b'"' | b'\'') => {
            let mut closing = start + 1;
            while bytes.get(closing).is_some_and(|byte| is_key_byte(*byte)) {
                closing += 1;
            }
            if bytes.get(closing) != Some(&quote) {
                return false;
            }
            closing + 1
        }
        byte if is_key_byte(byte) => {
            let mut end = start + 1;
            while bytes.get(end).is_some_and(|byte| is_key_byte(*byte)) {
                end += 1;
            }
            end
        }
        _ => return false,
    };
    cursor = skip_ascii_whitespace(message, cursor);
    matches!(bytes.get(cursor), Some(b'=' | b':'))
}

fn line_value_end(message: &str, start: usize) -> usize {
    let mut cursor = start;
    while cursor < message.len() {
        let byte = message.as_bytes()[cursor];
        if byte.is_ascii_control() || matches!(byte, b'&' | b'}' | b']' | b'|' | b'#') {
            break;
        }
        cursor = next_char_boundary(message, cursor);
    }
    cursor
}

fn is_cookie_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace(['-', '_'], "");
    matches!(normalized.as_str(), "cookie" | "setcookie")
}

fn is_authorization_key(key: &str) -> bool {
    key.to_ascii_lowercase()
        .replace(['-', '_'], "")
        .ends_with("authorization")
}

const fn is_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

const fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

const fn is_value_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || byte.is_ascii_control()
        || matches!(
            byte,
            b'&' | b';' | b',' | b'"' | b'\'' | b'}' | b']' | b'|' | b'#'
        )
}

fn skip_ascii_whitespace(message: &str, mut cursor: usize) -> usize {
    while message
        .as_bytes()
        .get(cursor)
        .is_some_and(u8::is_ascii_whitespace)
    {
        cursor += 1;
    }
    cursor
}

fn next_char_boundary(message: &str, cursor: usize) -> usize {
    cursor
        + message[cursor..]
            .chars()
            .next()
            .expect("cursor is before the string end")
            .len_utf8()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENTINEL_A: &str = "dummy-credential-alpha";
    const SENTINEL_B: &str = "dummy-credential-beta";

    #[test]
    fn kinds_roundtrip_and_cancel_helper_is_exact() {
        for kind in [
            ProviderErrorKind::Cancelled,
            ProviderErrorKind::Unavailable,
            ProviderErrorKind::Timeout,
            ProviderErrorKind::Rejected,
            ProviderErrorKind::Protocol,
        ] {
            let error = ProviderError::new(kind);
            let encoded = serde_json::to_string(&error).expect("error must encode");
            let decoded: ProviderError = serde_json::from_str(&encoded).expect("error must decode");
            assert_eq!(decoded, error);
            assert_eq!(decoded.is_cancelled(), kind == ProviderErrorKind::Cancelled);
        }
    }

    #[test]
    fn debug_display_and_serde_never_expose_raw_credentials() {
        let raw = format!(
            "{{\"api_key\":\"{SENTINEL_A}\",\"nested\":{{\"authorization\":\"Bearer {SENTINEL_B}\"}}}}"
        );
        let error = ProviderError::with_message(ProviderErrorKind::Rejected, raw);
        for rendered in [
            format!("{error:?}"),
            error.to_string(),
            serde_json::to_string(&error).expect("error must encode"),
        ] {
            assert!(rendered.contains("REDACTED"), "{rendered}");
            assert!(!rendered.contains(SENTINEL_A), "{rendered}");
            assert!(!rendered.contains(SENTINEL_B), "{rendered}");
        }
    }

    #[test]
    fn deserialization_sanitizes_untrusted_messages() {
        let encoded = format!("{{\"kind\":\"unavailable\",\"message\":\"token={SENTINEL_A}\"}}");
        let error: ProviderError = serde_json::from_str(&encoded).expect("error must decode");
        let rendered = format!(
            "{error:?} {error} {}",
            serde_json::to_string(&error).expect("error must encode")
        );
        assert!(!rendered.contains(SENTINEL_A));
        assert!(rendered.contains("REDACTED"));
    }

    #[test]
    fn deserialization_rejects_raw_provider_fields() {
        for field in ["body", "status"] {
            let mut encoded = serde_json::json!({"kind": "rejected"});
            encoded
                .as_object_mut()
                .expect("error object")
                .insert(field.into(), serde_json::json!("raw"));
            assert!(
                serde_json::from_value::<ProviderError>(encoded).is_err(),
                "raw {field} field must be rejected"
            );
        }
    }

    #[test]
    fn header_key_classifier_covers_credential_variants() {
        let raw = format!(
            concat!(
                "AUTH-KEY={0} access_key={0} Ocp-Apim-Subscription-Key={0} ",
                "password={0} credential={0} cookie=session={1}"
            ),
            SENTINEL_A, SENTINEL_B
        );
        let safe = sanitize_message(&raw);
        assert!(!safe.contains(SENTINEL_A), "{safe}");
        assert!(!safe.contains(SENTINEL_B), "{safe}");
        assert!(safe.contains(REDACTED), "{safe}");
    }

    #[test]
    fn json_and_plain_text_forms_are_redacted() {
        let json =
            format!("{{\"client_secret\":\"{SENTINEL_A}\",\"message\":\"Bearer {SENTINEL_B}\"}}");
        let plain = format!(
            "GET /?api-key={SENTINEL_A}&ok=yes Authorization='Bearer {SENTINEL_B}' sk-dummy-inline"
        );
        for safe in [sanitize_message(&json), sanitize_message(&plain)] {
            assert!(!safe.contains(SENTINEL_A), "{safe}");
            assert!(!safe.contains(SENTINEL_B), "{safe}");
            assert!(!safe.contains("dummy-inline"), "{safe}");
            assert!(safe.contains(REDACTED), "{safe}");
        }
    }

    #[test]
    fn long_valid_json_is_structurally_redacted_before_truncation() {
        let raw = serde_json::json!({
            "client_secret": SENTINEL_A,
            "api_key": SENTINEL_B,
            "detail": "x".repeat(4_000),
        })
        .to_string();
        assert!(raw.chars().count() > 2_048);

        let safe = sanitize_message(&raw);
        let prefix = safe
            .strip_suffix("… [truncated]")
            .expect("long message must be truncated");
        assert_eq!(prefix.chars().count(), MAX_ERROR_MESSAGE_CHARS);
        assert!(safe.contains(REDACTED), "{safe}");
        assert!(!safe.contains(SENTINEL_A), "{safe}");
        assert!(!safe.contains(SENTINEL_B), "{safe}");
    }

    #[test]
    fn invalid_json_redacts_spaced_and_unterminated_quoted_values() {
        let unterminated = format!(r#"prefix {{"client_secret": "{SENTINEL_A}"#);
        let spaced = format!(r#"echo {{"api_key": "{SENTINEL_B}", "ok": "visible"}}"#);

        let unterminated_safe = sanitize_message(&unterminated);
        assert!(unterminated_safe.contains(REDACTED), "{unterminated_safe}");
        assert!(
            !unterminated_safe.contains(SENTINEL_A),
            "{unterminated_safe}"
        );

        let spaced_safe = sanitize_message(&spaced);
        assert!(spaced_safe.contains(REDACTED), "{spaced_safe}");
        assert!(!spaced_safe.contains(SENTINEL_B), "{spaced_safe}");
        assert!(spaced_safe.contains("visible"), "{spaced_safe}");
    }

    #[test]
    fn plain_assignments_and_cookie_values_preserve_visible_fields() {
        let raw = format!(
            concat!(
                "status=401 AUTH-KEY={0} access-key : '{1} access' ",
                "Ocp-Apim-Subscription-Key = \"{0} subscription\" ",
                "Authorization=Bearer {1} API_KEY:{0} token={1} ",
                "client_secret: {0} PASSWORD=\"{1} password\"\n",
                "credential={0} multi word status=403\n",
                "COOKIE=session={0}; theme=dark\n",
                "Set-Cookie: session={1}; Path=/; HttpOnly\n",
                "message=visible"
            ),
            SENTINEL_A, SENTINEL_B
        );
        let safe = sanitize_message(&raw);

        assert!(safe.contains("status=401"), "{safe}");
        assert!(safe.contains("status=403"), "{safe}");
        assert!(safe.contains("message=visible"), "{safe}");
        assert!(safe.contains(REDACTED), "{safe}");
        assert!(!safe.contains(SENTINEL_A), "{safe}");
        assert!(!safe.contains(SENTINEL_B), "{safe}");
        assert!(!safe.contains("theme=dark"), "{safe}");
        assert!(!safe.contains("HttpOnly"), "{safe}");
    }

    #[test]
    fn url_query_and_multiple_quoted_fields_are_redacted() {
        let raw = format!(
            concat!(
                "GET /fail?Api-Key={0}&ToKeN={1}#fragment ",
                "\"auth-key\" : \"{0} quoted\" access-key='{1} spaced' ",
                "authorization='Bearer {0}' message=visible"
            ),
            SENTINEL_A, SENTINEL_B
        );
        let safe = sanitize_message(&raw);

        assert!(safe.contains("message=visible"), "{safe}");
        assert!(safe.contains(REDACTED), "{safe}");
        assert!(!safe.contains(SENTINEL_A), "{safe}");
        assert!(!safe.contains(SENTINEL_B), "{safe}");
    }

    #[test]
    fn boundary_secret_is_redacted_before_character_truncation() {
        let raw = format!(
            "{} token={SENTINEL_A} message=visible {}",
            "x".repeat(470),
            "y".repeat(1_000)
        );
        let safe = sanitize_message(&raw);
        let prefix = safe
            .strip_suffix("… [truncated]")
            .expect("long message must be truncated");

        assert_eq!(prefix.chars().count(), MAX_ERROR_MESSAGE_CHARS);
        assert!(safe.contains(REDACTED), "{safe}");
        assert!(safe.contains("message=visible"), "{safe}");
        assert!(!safe.contains(SENTINEL_A), "{safe}");
    }

    #[test]
    fn unicode_truncation_is_bounded_and_char_safe() {
        let raw = "界".repeat(2_000);
        let error = ProviderError::with_message(ProviderErrorKind::Protocol, raw);
        let message = error.message().expect("message retained");
        assert!(message.ends_with("… [truncated]"));
        assert!(message.chars().count() < 540);
        assert_eq!(message.chars().take(MAX_ERROR_MESSAGE_CHARS).count(), 512);
    }

    #[test]
    fn scan_input_is_bounded_before_unicode_boundary() {
        let raw = format!(
            "{}界{}",
            "x".repeat(MAX_ERROR_SCAN_BYTES - 1),
            "y".repeat(1_024)
        );
        let safe = sanitize_message(&raw);
        assert!(safe.ends_with("… [truncated]"));
        assert!(safe.is_char_boundary(safe.len()));
    }

    #[test]
    fn malformed_quote_adversary_remains_linear_and_safe() {
        let raw = format!("token={SENTINEL_A} {}", "\\\"".repeat(30_000));
        let safe = sanitize_message(&raw);
        assert!(!safe.contains(SENTINEL_A), "{safe}");
        assert!(safe.contains(REDACTED), "{safe}");
        assert!(safe.chars().count() < 540);
    }
}
