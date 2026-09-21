//! Streamable-HTTP transport for remote MCP endpoints.
//!
//! One JSON-RPC request per POST; the response body may be a single JSON
//! object or an SSE stream whose `data:` frames carry the reply. A returned
//! `Mcp-Session-Id` header is captured and replayed on later requests.
//! Credential headers come from the server binding; keys live in the secret
//! store and never travel through settings.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::RwLock;

use super::{JsonRpcChannel, MAX_MESSAGE_BYTES, McpError};

/// Credential header styles accepted for HTTP MCP servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyHeader {
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// `x-api-key: <key>`.
    XApiKey,
}

impl KeyHeader {
    /// Parses the settings vocabulary.
    #[must_use]
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("x-api-key") => Self::XApiKey,
            _ => Self::Bearer,
        }
    }

    fn apply(self, request: reqwest::RequestBuilder, key: &str) -> reqwest::RequestBuilder {
        let key = strip_bearer_prefix(key);
        match self {
            Self::Bearer => request.header("Authorization", format!("Bearer {key}")),
            Self::XApiKey => request.header("x-api-key", key),
        }
    }
}

/// Users paste either the raw key or `Bearer <key>`; the header always
/// carries exactly one `Bearer ` prefix.
fn strip_bearer_prefix(key: &str) -> &str {
    let trimmed = key.trim();
    trimmed
        .split_once(char::is_whitespace)
        .and_then(|(scheme, rest)| scheme.eq_ignore_ascii_case("bearer").then_some(rest.trim()))
        .unwrap_or(trimmed)
}

/// Options for one HTTP channel.
#[derive(Clone)]
pub struct HttpChannelOptions {
    /// Credential header style.
    pub key_header: KeyHeader,
    /// API key, when the server requires one.
    pub api_key: Option<String>,
    /// Per-request timeout.
    pub timeout: Duration,
}

/// One HTTP MCP session bound to an endpoint.
pub struct HttpChannel {
    client: reqwest::Client,
    endpoint: String,
    options: HttpChannelOptions,
    session_id: RwLock<Option<String>>,
    /// Test seam: when set, posts are served from this table keyed by
    /// `method` instead of hitting the network.
    responder: Option<Arc<dyn Fn(String) -> Value + Send + Sync>>,
}

impl HttpChannel {
    /// Creates one channel over the endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::Transport`] when the HTTP client cannot build.
    pub fn new(endpoint: &str, options: HttpChannelOptions) -> Result<Self, McpError> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|error| McpError::transport(format!("HTTP client unavailable: {error}")))?;
        Ok(Self {
            client,
            endpoint: endpoint.to_owned(),
            options,
            session_id: RwLock::new(None),
            responder: None,
        })
    }

    /// Installs the test responder seam; production channels never set this.
    #[cfg(test)]
    #[must_use]
    pub fn with_test_responder(
        mut self,
        responder: Arc<dyn Fn(String) -> Value + Send + Sync>,
    ) -> Self {
        self.responder = Some(responder);
        self
    }

    async fn post_envelope(&self, envelope: Value) -> Result<Option<Value>, McpError> {
        let body =
            serde_json::to_vec(&envelope).map_err(|error| McpError::protocol(error.to_string()))?;
        if body.len() > MAX_MESSAGE_BYTES {
            return Err(McpError::Oversized);
        }
        if let Some(responder) = &self.responder {
            let method = envelope["method"].as_str().unwrap_or_default().to_owned();
            return Ok(Some(responder(method)));
        }
        let mut request = self
            .client
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", super::PROTOCOL_VERSION)
            .timeout(self.options.timeout)
            .body(body);
        if let Some(session) = self.session_id.read().await.as_deref() {
            request = request.header("Mcp-Session-Id", session);
        }
        if let Some(key) = &self.options.api_key {
            request = self.options.key_header.apply(request, key);
        }
        let response = request.send().await.map_err(|error| {
            if error.is_timeout() {
                McpError::Timeout
            } else {
                McpError::transport(format!("request to {} failed: {error}", self.endpoint))
            }
        })?;
        let status = response.status();
        if !status.is_success() {
            // The body usually names the cause (bad key, unknown session);
            // keep a short excerpt so the settings page can show it.
            let excerpt = response
                .bytes()
                .await
                .map(|bytes| String::from_utf8_lossy(&bytes[..bytes.len().min(300)]).into_owned())
                .unwrap_or_default();
            let hint = match status.as_u16() {
                401 | 403 => " — check the API key stored for this server",
                404 => " — the session expired or the endpoint path is wrong",
                _ => "",
            };
            return Err(McpError::transport(format!(
                "HTTP {status}{hint}: {}",
                excerpt.trim()
            )));
        }
        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
        {
            *self.session_id.write().await = Some(session.to_owned());
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| McpError::transport(format!("response body failed: {error}")))?;
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(McpError::Oversized);
        }
        if content_type.contains("text/event-stream") {
            return sse_reply(&bytes);
        }
        if bytes.is_empty() {
            return Ok(None);
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| McpError::protocol(format!("response is not JSON: {error}")))?;
        Ok(Some(value))
    }
}

/// Extracts the first JSON-RPC response object from SSE frames.
fn sse_reply(bytes: &[u8]) -> Result<Option<Value>, McpError> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| McpError::protocol("SSE body is not UTF-8"))?;
    for line in text.lines() {
        let Some(payload) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim_start();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let value: Value = serde_json::from_str(payload)
            .map_err(|error| McpError::protocol(format!("SSE frame is not JSON: {error}")))?;
        if value["id"].is_u64() || value["error"].as_object().is_some() {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn finish_reply(value: Option<Value>, id: u64) -> Result<Value, McpError> {
    let Some(value) = value else {
        // HTTP notifications return 202 with no body.
        return Ok(Value::Null);
    };
    if let Some(error) = value.get("error").filter(|error| error.is_object()) {
        return Err(McpError::server(error));
    }
    if value["id"].as_u64() != Some(id) {
        return Err(McpError::protocol(format!(
            "reply id {} does not match request id {id}",
            value["id"]
        )));
    }
    Ok(value["result"].clone())
}

#[async_trait::async_trait]
impl JsonRpcChannel for HttpChannel {
    async fn request(&self, id: u64, method: &str, params: Value) -> Result<Value, McpError> {
        let envelope = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let reply = self.post_envelope(envelope).await?;
        finish_reply(reply, id)
    }

    async fn notify(&self, method: &str) -> Result<(), McpError> {
        let envelope = json!({
            "jsonrpc": "2.0",
            "method": method,
        });
        // 202/empty responses are success; transport failures propagate.
        self.post_envelope(envelope).await?;
        Ok(())
    }
}

/// Test helper covering the SSE and JSON reply shapes.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_reply_extracts_the_jsonrpc_object() {
        let body = b"event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{}}\n\n";
        let reply = sse_reply(body).expect("frames").expect("reply");
        assert_eq!(reply["id"], 7);
        assert!(finish_reply(Some(reply), 7).is_ok());
    }

    #[test]
    fn id_mismatch_and_errors_fail_closed() {
        let value = json!({"jsonrpc": "2.0", "id": 9, "result": {}});
        assert!(matches!(
            finish_reply(Some(value), 7),
            Err(McpError::Protocol(_))
        ));
        let value = json!({"jsonrpc": "2.0", "id": 7, "error": {"code": -1, "message": "nope"}});
        assert_eq!(
            finish_reply(Some(value), 7),
            Err(McpError::Server("nope (code -1)".to_owned()))
        );
        assert_eq!(finish_reply(None, 7), Ok(Value::Null));
    }

    #[tokio::test]
    async fn request_roundtrips_through_the_seam() {
        let channel = HttpChannel::new(
            "https://mcp.example.com/mcp",
            HttpChannelOptions {
                key_header: KeyHeader::Bearer,
                api_key: Some("k".to_owned()),
                timeout: super::super::DEFAULT_REQUEST_TIMEOUT,
            },
        )
        .expect("channel")
        .with_test_responder(Arc::new(|method| {
            if method == "tools/list" {
                json!({"jsonrpc": "2.0", "id": 1,
                       "result": {"tools": [{"name": "resolve", "inputSchema": {}}]}})
            } else {
                json!({"jsonrpc": "2.0", "id": 1, "result": {}})
            }
        }));
        let result = channel
            .request(1, "tools/list", json!({}))
            .await
            .expect("reply");
        assert!(result["tools"].as_array().is_some());
    }

    #[test]
    fn key_header_parsing_covers_the_settings_vocabulary() {
        assert_eq!(KeyHeader::parse(Some("x-api-key")), KeyHeader::XApiKey);
        assert_eq!(KeyHeader::parse(Some("bearer")), KeyHeader::Bearer);
        assert_eq!(KeyHeader::parse(None), KeyHeader::Bearer);
    }
}
