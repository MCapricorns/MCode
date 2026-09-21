//! Bridges enabled MCP servers into the agent's tool registry.
//!
//! Each configured server is connected at turn start (initialize +
//! tools/list); every listed tool becomes a [`ToolDyn`] whose spec carries
//! the server's own `inputSchema` verbatim. A failed server is skipped —
//! the turn proceeds with the remaining tools rather than failing.

use std::sync::Arc;
use std::time::Duration;

use mycode_config::AppSettings;
use mycode_core::ToolSpec;
use mycode_tools::{ToolCtx, ToolDyn, ToolError, ToolResult, ToolStream};
use serde_json::Value;

/// How long the per-server connect (spawn + initialize + tools/list) runs.
const MCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// One MCP tool exposed by a connected server, registered as a raw-JSON tool.
///
/// Implements `ToolDyn` directly instead of `Tool` because the server's
/// `inputSchema` must reach the model verbatim through `spec()`; the blanket
/// `impl<T: Tool> ToolDyn` would advertise the schemars schema of
/// `Args = serde_json::Value`, which is just `true`.
pub(crate) struct DynamicMcpTool {
    server_id: String,
    tool: crate::mcp_client::McpTool,
    /// Compiled once from the server's schema; `None` when it does not
    /// compile — args then pass through and the server's rejection is the
    /// backstop.
    validator: Option<jsonschema::Validator>,
    client: Arc<tokio::sync::Mutex<crate::mcp_client::McpClient>>,
}

impl DynamicMcpTool {
    fn new(
        server_id: String,
        tool: crate::mcp_client::McpTool,
        client: Arc<tokio::sync::Mutex<crate::mcp_client::McpClient>>,
    ) -> Self {
        let validator = jsonschema::validator_for(&tool.input_schema).ok();
        Self {
            server_id,
            tool,
            validator,
            client,
        }
    }
}

#[async_trait::async_trait]
impl ToolDyn for DynamicMcpTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.tool.name.clone(),
            description: self.tool.description.clone().unwrap_or_else(|| {
                format!(
                    "MCP tool '{}' on server '{}'.",
                    self.tool.name, self.server_id
                )
            }),
            params_schema: self.tool.input_schema.clone(),
        }
    }

    async fn execute_dyn(
        &self,
        args: Value,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        if let Some(validator) = &self.validator {
            let errors: Vec<String> = validator
                .iter_errors(&args)
                .map(|error| format!("{error} (at {})", error.instance_path()))
                .collect();
            if !errors.is_empty() {
                return Err(ToolError::InvalidArgs(errors.join("; ")));
            }
        }
        out.progress(format!(
            "calling {} on MCP server {}",
            self.tool.name, self.server_id
        ));
        let mut client = self.client.lock().await;
        // The dispatcher does not select on the turn token while a tool
        // runs; bound the call here so Escape actually cancels an MCP call.
        let call = client.call_tool(&self.tool.name, args);
        let output = tokio::select! {
            biased;
            () = ctx.cancel.cancelled() => {
                return Err(ToolError::Execution("MCP call cancelled".to_owned()));
            }
            result = call => result.map_err(|error| {
                let detail = if error.is_connection_lost() {
                    format!(
                        "MCP server '{}' lost the connection: {error}",
                        self.server_id
                    )
                } else {
                    format!("MCP server '{}': {error}", self.server_id)
                };
                ToolError::Execution(detail)
            })?,
        };
        // A server that flags `isError` ran the tool and reported failure as
        // data. The model needs to read it to correct its arguments, so it
        // becomes an error *result* rather than a dispatcher failure.
        Ok(if output.is_error {
            ToolResult::error(output.text)
        } else {
            ToolResult::text(output.text)
        })
    }
}

/// Connects every enabled MCP server and flattens its tools.
///
/// Any per-server failure (spawn, handshake, listing) skips that server.
/// Runs inside the spawned turn task on the single-threaded core runtime —
/// plain `.await` only, never `block_on`.
pub(crate) async fn connect_mcp_tools(
    home: &mycode_config::HomeLayout,
    settings: &AppSettings,
) -> Vec<Arc<DynamicMcpTool>> {
    let mut tools = Vec::new();
    let Ok(secrets) = mycode_config::read_provider_secrets(home) else {
        return tools;
    };
    for server in settings.mcp_servers.iter().filter(|server| server.enabled) {
        let api_key = secrets
            .key(&format!("mcp-{}", server.id))
            .map(str::to_owned);
        let channel: Arc<dyn crate::mcp_client::JsonRpcChannel> = match server.transport.as_str() {
            "stdio" => {
                let Some(command) = server.command.as_deref() else {
                    continue;
                };
                let env: Vec<(String, String)> = server
                    .env
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                let Ok(channel) = crate::mcp_client::StdioChannel::spawn(
                    command,
                    &server.args,
                    &env,
                    MCP_CONNECT_TIMEOUT,
                )
                .await
                else {
                    continue;
                };
                Arc::new(channel)
            }
            "http" => {
                let Some(endpoint) = server.endpoint.as_deref() else {
                    continue;
                };
                let Ok(channel) = crate::mcp_client::HttpChannel::new(
                    endpoint,
                    crate::mcp_client::HttpChannelOptions {
                        key_header: crate::mcp_client::KeyHeader::parse(
                            server.key_header.as_deref(),
                        ),
                        api_key,
                        timeout: MCP_CONNECT_TIMEOUT,
                    },
                ) else {
                    continue;
                };
                Arc::new(channel)
            }
            _ => continue,
        };
        let mut client = crate::mcp_client::McpClient::new(channel);
        if client.initialize().await.is_err() {
            continue;
        }
        let Ok(listed) = client.list_tools().await else {
            continue;
        };
        let client = Arc::new(tokio::sync::Mutex::new(client));
        for tool in listed {
            tools.push(Arc::new(DynamicMcpTool::new(
                server.id.clone(),
                tool,
                client.clone(),
            )));
        }
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_client::McpError;
    use async_trait::async_trait;
    use serde_json::json;

    /// Channel that answers JSON-RPC requests from a canned map.
    struct ScriptedChannel {
        call_result: String,
    }

    #[async_trait]
    impl crate::mcp_client::JsonRpcChannel for ScriptedChannel {
        async fn request(&self, _id: u64, method: &str, _params: Value) -> Result<Value, McpError> {
            match method {
                "tools/list" => Ok(json!({"tools": [
                    {"name": "search_docs",
                     "description": "Search the docs",
                     "inputSchema": {"type": "object", "properties": {
                         "query": {"type": "string"}
                     }, "required": ["query"]}}
                ]})),
                "tools/call" => Ok(json!({"content": [
                    {"type": "text", "text": self.call_result}
                ]})),
                _ => Ok(json!({})),
            }
        }

        async fn notify(&self, _method: &str) -> Result<(), McpError> {
            Ok(())
        }
    }

    fn scripted(call_result: &str) -> Arc<ScriptedChannel> {
        Arc::new(ScriptedChannel {
            call_result: call_result.to_owned(),
        })
    }

    async fn tool_over(channel: Arc<ScriptedChannel>) -> Arc<DynamicMcpTool> {
        let mut client = crate::mcp_client::McpClient::new(channel);
        client.initialize().await.expect("init");
        let listed = client.list_tools().await.expect("list");
        let client = Arc::new(tokio::sync::Mutex::new(client));
        Arc::new(DynamicMcpTool::new(
            "ctx7".to_owned(),
            listed.into_iter().next().expect("one tool"),
            client,
        ))
    }

    #[tokio::test]
    async fn spec_carries_the_server_schema_and_calls_round_trip() {
        let tool = tool_over(scripted("found 3 pages")).await;
        let spec = tool.spec();
        assert_eq!(spec.name, "search_docs");
        assert_eq!(spec.params_schema["required"], json!(["query"]));

        let ctx = ToolCtx::new(".");
        let mut stream = mycode_tools::stream::ToolStream::channel().0;
        let result = tool
            .execute_dyn(json!({"query": "rust"}), &ctx, &mut stream)
            .await
            .expect("call");
        match &result.content[0] {
            mycode_core::ContentBlock::Text(text) => assert_eq!(text.text, "found 3 pages"),
            other => panic!("unexpected block: {other:?}"),
        }

        // Schema violation is rejected before the server round trip.
        let err = tool
            .execute_dyn(json!({"q": 1}), &ctx, &mut stream)
            .await
            .expect_err("invalid args");
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }
}
