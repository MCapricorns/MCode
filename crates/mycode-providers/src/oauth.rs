//! GitHub Copilot OAuth: device-code sign-in and bearer-token exchange.
//!
//! Copilot is an OpenAI-compatible endpoint whose credential is not a pasted
//! API key: the user authorizes MYCode through GitHub's device flow, the
//! resulting OAuth token is stored in the secret vault, and each turn
//! exchanges it for a short-lived Copilot bearer token. Error messages carry
//! statuses and field names only — never token values.
use serde_json::Value;

/// Provider id that authenticates through this module.
pub const COPILOT_PROVIDER_ID: &str = "github-copilot";
/// Public device-flow client id of the GitHub Copilot CLI app (no secret).
const DEVICE_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
/// Device authorization endpoint.
const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
/// Device token polling endpoint.
const DEVICE_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
/// Copilot bearer-token exchange endpoint.
const COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
/// Requested device-flow scope.
const DEVICE_SCOPE: &str = "read:user";
/// Bounded string fields from GitHub responses.
const MAX_FIELD_BYTES: usize = 8 * 1024;

/// A started device authorization: what the user sees and what we poll with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCodeStart {
    /// Opaque poll credential.
    pub device_code: String,
    /// Code the user types at the verification page.
    pub user_code: String,
    /// Page the user opens.
    pub verification_uri: String,
    /// Polling cadence in seconds.
    pub interval_secs: u64,
    /// Flow lifetime in seconds.
    pub expires_in_secs: u64,
}

/// One poll attempt against the device token endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceTokenPoll {
    /// Authorization granted; carries the OAuth access token.
    Granted(String),
    /// The user has not approved yet; keep polling.
    Pending,
    /// GitHub asked us to slow down; extend the interval.
    SlowDown,
    /// The flow is dead (denied, expired, or unknown error kind).
    Denied(&'static str),
}

/// A short-lived Copilot API bearer token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopilotToken {
    /// Bearer value for chat completions.
    pub token: String,
    /// Unix seconds when the token stops working.
    pub expires_at_unix: u64,
}

fn bounded(value: &str) -> Option<&str> {
    if value.is_empty() || value.len() > MAX_FIELD_BYTES {
        None
    } else {
        Some(value)
    }
}

fn field<'a>(payload: &'a Value, name: &str) -> Option<&'a str> {
    payload.get(name).and_then(Value::as_str).and_then(bounded)
}

/// Starts the device authorization flow.
///
/// # Errors
///
/// Returns a transport or endpoint-shape failure without embedded secrets.
pub async fn start_device_flow(client: &reqwest::Client) -> Result<DeviceCodeStart, String> {
    let response = client
        .post(DEVICE_CODE_URL)
        .header("accept", "application/json")
        .form(&[("client_id", DEVICE_CLIENT_ID), ("scope", DEVICE_SCOPE)])
        .send()
        .await
        .map_err(|error| format!("device-code request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "device-code endpoint returned {}",
            response.status()
        ));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|error| format!("device-code response was not JSON: {error}"))?;
    parse_device_start(&payload).ok_or_else(|| "device-code response was missing fields".to_owned())
}

/// Parses a device authorization response.
#[must_use]
pub fn parse_device_start(payload: &Value) -> Option<DeviceCodeStart> {
    Some(DeviceCodeStart {
        device_code: field(payload, "device_code")?.to_owned(),
        user_code: field(payload, "user_code")?.to_owned(),
        verification_uri: field(payload, "verification_uri")?.to_owned(),
        interval_secs: payload.get("interval").and_then(Value::as_u64).unwrap_or(5),
        expires_in_secs: payload
            .get("expires_in")
            .and_then(Value::as_u64)
            .unwrap_or(900),
    })
}

/// Polls once for the granted OAuth token.
///
/// # Errors
///
/// Returns a transport failure; grant states arrive as [`DeviceTokenPoll`].
pub async fn poll_device_token(
    client: &reqwest::Client,
    device_code: &str,
) -> Result<DeviceTokenPoll, String> {
    let response = client
        .post(DEVICE_TOKEN_URL)
        .header("accept", "application/json")
        .form(&[
            ("client_id", DEVICE_CLIENT_ID),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .send()
        .await
        .map_err(|error| format!("device-token poll failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "device-token endpoint returned {}",
            response.status()
        ));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|error| format!("device-token response was not JSON: {error}"))?;
    Ok(classify_device_poll(&payload))
}

/// Classifies one device-token response payload.
#[must_use]
pub fn classify_device_poll(payload: &Value) -> DeviceTokenPoll {
    if let Some(token) = field(payload, "access_token") {
        return DeviceTokenPoll::Granted(token.to_owned());
    }
    match field(payload, "error") {
        Some("authorization_pending") => DeviceTokenPoll::Pending,
        Some("slow_down") => DeviceTokenPoll::SlowDown,
        Some("expired_token") => DeviceTokenPoll::Denied("the sign-in code expired"),
        Some("access_denied") => DeviceTokenPoll::Denied("the request was denied on GitHub"),
        _ => DeviceTokenPoll::Denied("the sign-in could not be completed"),
    }
}

/// Exchanges the stored OAuth token for a short-lived Copilot bearer token.
///
/// # Errors
///
/// Returns a transport failure or a rejection that means "sign in again".
pub async fn copilot_bearer(
    client: &reqwest::Client,
    github_token: &str,
) -> Result<CopilotToken, String> {
    let response = client
        .get(COPILOT_TOKEN_URL)
        .header("authorization", format!("Bearer {github_token}"))
        .header("accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| format!("copilot token request failed: {error}"))?;
    if let Some(reason) = match response.status().as_u16() {
        401 | 403 => Some("copilot rejected the saved sign-in — sign in again".to_owned()),
        404 => Some("copilot token exchange is unavailable for this account".to_owned()),
        status if !(200..300).contains(&status) => {
            Some(format!("copilot token endpoint returned {status}"))
        }
        _ => None,
    } {
        return Err(reason);
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|error| format!("copilot token response was not JSON: {error}"))?;
    parse_copilot_token(&payload)
        .ok_or_else(|| "copilot token response was missing fields".to_owned())
}

/// Parses a Copilot token exchange response.
#[must_use]
pub fn parse_copilot_token(payload: &Value) -> Option<CopilotToken> {
    Some(CopilotToken {
        token: field(payload, "token")?.to_owned(),
        expires_at_unix: payload.get("expires_at").and_then(Value::as_u64)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn device_start_parses_and_applies_cadence_defaults() {
        let start = parse_device_start(&json!({
            "device_code": "dc", "user_code": "AB12-CD34",
            "verification_uri": "https://github.com/login/device"
        }))
        .expect("fields");
        assert_eq!(start.user_code, "AB12-CD34");
        assert_eq!(start.interval_secs, 5);
        assert_eq!(start.expires_in_secs, 900);

        assert!(parse_device_start(&json!({"user_code": "x"})).is_none());
    }

    #[test]
    fn poll_states_map_to_pending_slowdown_or_grant() {
        let granted = classify_device_poll(&json!({"access_token": "gho_x"}));
        assert_eq!(granted, DeviceTokenPoll::Granted("gho_x".to_owned()));
        assert_eq!(
            classify_device_poll(&json!({"error": "authorization_pending"})),
            DeviceTokenPoll::Pending
        );
        assert_eq!(
            classify_device_poll(&json!({"error": "slow_down"})),
            DeviceTokenPoll::SlowDown
        );
        assert_eq!(
            classify_device_poll(&json!({"error": "expired_token"})),
            DeviceTokenPoll::Denied("the sign-in code expired")
        );
        assert_eq!(
            classify_device_poll(&json!({"error": "weird"})),
            DeviceTokenPoll::Denied("the sign-in could not be completed")
        );
    }

    #[test]
    fn copilot_token_requires_token_and_expiry() {
        let token = parse_copilot_token(&json!({"token": "tid=abc", "expires_at": 1790000000}))
            .expect("fields");
        assert_eq!(token.expires_at_unix, 1_790_000_000);
        assert!(parse_copilot_token(&json!({"token": "t"})).is_none());
        assert!(parse_copilot_token(&json!({"expires_at": 1})).is_none());
    }

    #[tokio::test]
    #[ignore = "live network call against github.com"]
    async fn live_device_flow_starts() {
        let client = reqwest::Client::new();
        let start = start_device_flow(&client)
            .await
            .expect("device flow starts");
        assert!(!start.user_code.is_empty());
        assert_eq!(start.verification_uri, "https://github.com/login/device");
        assert!(start.expires_in_secs > 0);
    }
}
