//! First-party bounded MCP client.
//!
//! Speaks JSON-RPC 2.0 against configured servers over two transports:
//! newline-delimited stdio for local commands and the MCP Streamable-HTTP
//! wire for remote endpoints. The [`JsonRpcChannel`] seam keeps the protocol
//! client testable without spawning processes or touching the network.
//!
//! Bounds: one MiB per message, 128 tools per server, per-request timeouts,
//! and strict https for HTTP endpoints.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

pub mod http_channel;
pub mod stdio_channel;

pub use http_channel::{HttpChannel, HttpChannelOptions, KeyHeader};
pub use stdio_channel::StdioChannel;

/// One JSON-RPC request/response hop over a configured MCP server.
///
/// Implementations own the wire envelope: they wrap `id`, `method`, and
/// `params` into a JSON-RPC request, wait for the response carrying the same
/// id, map JSON-RPC error objects to [`McpError::ServerError`], and return the
/// bare `result` value.
#[async_trait::async_trait]
pub trait JsonRpcChannel: Send + Sync + 'static {
    /// Performs one request and returns the bare `result` value.
    ///
    /// # Errors
    ///
    /// Returns [`McpError`] for transport, timeout, and protocol failures.
    async fn request(&self, id: u64, method: &str, params: Value) -> Result<Value, McpError>;

    /// Sends one notification that expects no response.
    ///
    /// # Errors
    ///
    /// Returns [`McpError`] for transport failures.
    async fn notify(&self, method: &str) -> Result<(), McpError>;
}

/// Maximum accepted response message bytes.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Maximum tools listed per server.
pub const MAX_TOOLS: usize = 128;
/// Default request timeout shared by every channel implementation.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Protocol revision this client speaks.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Errors surfaced by the MCP client.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpError {
    /// The server violated the MCP wire contract.
    #[error("server protocol error")]
    Protocol,
    /// A request exceeded its deadline.
    #[error("request timed out")]
    Timeout,
    /// The transport failed before a response arrived.
    #[error("transport failure")]
    Transport,
    /// The caller cancelled the request.
    #[error("cancelled")]
    Cancelled,
    /// The server returned a JSON-RPC error object.
    #[error("server returned an error")]
    ServerError,
    /// The response exceeded the message bound.
    #[error("response exceeded the size bound")]
    Oversized,
}

/// One tool exposed by a server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    /// Tool name, unique per server.
    pub name: String,
    /// Human-readable description, when provided.
    pub description: Option<String>,
    /// JSON Schema for the tool arguments.
    pub input_schema: Value,
}

/// A connected, initialized MCP client bound to one channel.
pub struct McpClient {
    channel: Arc<dyn JsonRpcChannel>,
    next_id: u64,
}

impl McpClient {
    /// Creates one client over a transport channel.
    #[must_use]
    pub fn new(channel: Arc<dyn JsonRpcChannel>) -> Self {
        Self {
            channel,
            next_id: 1,
        }
    }

    /// Performs the MCP initialize handshake and sends the initialized
    /// notification.
    ///
    /// # Errors
    ///
    /// Returns [`McpError`] for transport and contract failures.
    pub async fn initialize(&mut self) -> Result<Value, McpError> {
        let result = self
            .call(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "mcode", "version": "0.0.1"},
                }),
            )
            .await?;
        self.channel.notify("notifications/initialized").await?;
        Ok(result)
    }

    /// Lists the tools the server exposes, bounded by [`MAX_TOOLS`].
    ///
    /// # Errors
    ///
    /// Returns [`McpError`] for transport and contract failures.
    pub async fn list_tools(&mut self) -> Result<Vec<McpTool>, McpError> {
        let result = self.call("tools/list", json!({})).await?;
        let Some(tools) = result["tools"].as_array() else {
            return Err(McpError::Protocol);
        };
        if tools.len() > MAX_TOOLS {
            return Err(McpError::Oversized);
        }
        Ok(tools
            .iter()
            .map(|tool| McpTool {
                name: tool["name"].as_str().unwrap_or_default().to_owned(),
                description: tool["description"].as_str().map(str::to_owned),
                input_schema: tool["inputSchema"].clone(),
            })
            .collect())
    }

    /// Calls one tool and returns its concatenated text content.
    ///
    /// # Errors
    ///
    /// Returns [`McpError`] for transport and contract failures.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<String, McpError> {
        let result = self
            .call("tools/call", json!({"name": name, "arguments": arguments}))
            .await?;
        if result["isError"].as_bool() == Some(true) {
            return Err(McpError::ServerError);
        }
        let mut text = String::new();
        if let Some(blocks) = result["content"].as_array() {
            for block in blocks {
                if block["type"].as_str() == Some("text")
                    && let Some(part) = block["text"].as_str()
                {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(part);
                }
            }
        }
        Ok(text)
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        self.channel.request(id, method, params).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct MockChannel {
        script: Mutex<Vec<Result<Value, McpError>>>,
    }

    #[async_trait::async_trait]
    impl JsonRpcChannel for MockChannel {
        async fn request(&self, id: u64, method: &str, _params: Value) -> Result<Value, McpError> {
            let mut script = self.script.lock().expect("script");
            if script.is_empty() {
                return Err(McpError::Protocol);
            }
            assert!(id >= 1, "ids are minted by the client");
            assert!(!method.is_empty());
            script.remove(0)
        }

        async fn notify(&self, _method: &str) -> Result<(), McpError> {
            Ok(())
        }
    }

    fn ok(result: Value) -> Result<Value, McpError> {
        Ok(result)
    }

    #[tokio::test]
    async fn initialize_lists_and_calls_tools() {
        let channel = Arc::new(MockChannel {
            script: Mutex::new(vec![
                ok(json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {}})),
                ok(json!({"tools": [
                    {"name": "resolve", "description": "docs", "inputSchema": {"type": "object"}},
                ]})),
                ok(json!({"content": [{"type": "text", "text": "found"}], "isError": false})),
            ]),
        });
        let mut client = McpClient::new(channel);
        let info = client.initialize().await.expect("initialize");
        assert_eq!(info["protocolVersion"], PROTOCOL_VERSION);

        let tools = client.list_tools().await.expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "resolve");

        let text = client
            .call_tool("resolve", json!({"query": "gpui"}))
            .await
            .expect("call");
        assert_eq!(text, "found");
    }

    #[tokio::test]
    async fn channel_errors_surface_verbatim() {
        let channel = Arc::new(MockChannel {
            script: Mutex::new(vec![Err(McpError::ServerError)]),
        });
        let mut client = McpClient::new(channel);
        assert_eq!(
            client.call("x", json!({})).await,
            Err(McpError::ServerError)
        );
    }

    #[tokio::test]
    async fn tool_error_flag_maps_to_server_error() {
        let channel = Arc::new(MockChannel {
            script: Mutex::new(vec![ok(json!({
                "content": [{"type": "text", "text": "boom"}], "isError": true
            }))]),
        });
        let mut client = McpClient::new(channel);
        assert_eq!(
            client.call_tool("x", json!({})).await,
            Err(McpError::ServerError)
        );
    }
}
