//! Bridges enabled MCP servers into the agent's tool registry.
//!
//! Servers are connected at turn start. Their tools are not inlined into
//! the model tool list. The model calls `search_tool` for one schema, then
//! `use_tool` with arguments that match it. A failed server is skipped.

use std::collections::HashMap;
use std::sync::Arc;

use mycode_config::AppSettings;
use mycode_core::ToolSpec;
use mycode_tools::{ToolCtx, ToolDyn, ToolError, ToolResult, ToolStream};
use serde_json::Value;

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
    snippet: String,
}

impl DynamicMcpTool {
    fn new(
        server_id: String,
        tool: crate::mcp_client::McpTool,
        client: Arc<tokio::sync::Mutex<crate::mcp_client::McpClient>>,
    ) -> Self {
        let validator = jsonschema::validator_for(&tool.input_schema).ok();
        let snippet = format!(
            "MCP tool on server '{server_id}'. Call it when the task matches; do not wait to be asked."
        );
        Self {
            server_id,
            tool,
            validator,
            client,
            snippet,
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

    fn prompt_snippet_dyn(&self) -> Option<&str> {
        Some(self.snippet.as_str())
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
        // The channel timeout bounds every request over the channel's
        // lifetime — including each tools/call during the turn — so it must
        // be the full request budget, not a connect-phase bound. A 10s value
        // here made every tool call that outlasted it fail with Timeout and
        // drop the connection mid-turn.
        let Ok(mut client) = open_mcp_client(server, api_key).await else {
            continue;
        };
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

/// Connected MCP tools, addressed by name. Schemas stay here until
/// `search_tool` is called.
pub(crate) struct McpCatalog {
    by_name: HashMap<String, Arc<DynamicMcpTool>>,
    names: String,
}

impl McpCatalog {
    pub(crate) fn from_tools(tools: Vec<Arc<DynamicMcpTool>>) -> Option<Arc<Self>> {
        if tools.is_empty() {
            return None;
        }
        let mut by_name = HashMap::new();
        let mut names = Vec::new();
        for tool in tools {
            names.push(tool.tool.name.clone());
            by_name.insert(tool.tool.name.clone(), tool);
        }
        names.sort();
        names.dedup();
        let extra = names.len().saturating_sub(40);
        let mut listed = names.into_iter().take(40).collect::<Vec<_>>().join(", ");
        if extra > 0 {
            listed.push_str(&format!(", and {extra} more"));
        }
        Some(Arc::new(Self {
            by_name,
            names: listed,
        }))
    }

    /// Comma-separated names injected into the system prompt.
    pub(crate) fn index(&self) -> &str {
        &self.names
    }

    fn get(&self, name: &str) -> Option<&Arc<DynamicMcpTool>> {
        self.by_name.get(name)
    }
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn arg_name(args: &Value) -> Result<String, ToolError> {
    args.get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ToolError::InvalidArgs("name is required".to_owned()))
}

/// Grok-style schema lookup. The model must call this before `use_tool`.
pub(crate) struct SearchTool {
    catalog: Arc<McpCatalog>,
}

impl SearchTool {
    pub(crate) fn new(catalog: Arc<McpCatalog>) -> Self {
        Self { catalog }
    }
}

#[async_trait::async_trait]
impl ToolDyn for SearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search_tool".to_owned(),
            description: format!(
                "Retrieve one connected MCP tool's description and inputSchema before calling use_tool. Never guess parameter names. Connected: {}.",
                self.catalog.names
            ),
            params_schema: object_schema(
                serde_json::json!({
                    "name": {"type": "string", "description": "Exact tool name from the connected list."}
                }),
                &["name"],
            ),
        }
    }

    fn prompt_snippet_dyn(&self) -> Option<&str> {
        Some("search_tool: fetch one MCP inputSchema before use_tool. Never guess parameters.")
    }

    async fn execute_dyn(
        &self,
        args: Value,
        _ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let name = arg_name(&args)?;
        let Some(tool) = self.catalog.get(&name) else {
            return Err(ToolError::InvalidArgs(format!(
                "no MCP tool named {name}. Connected: {}",
                self.catalog.names
            )));
        };
        let spec = tool.spec();
        Ok(ToolResult::text(format!(
            "name: {}\nserver: {}\ndescription: {}\ninputSchema: {}",
            spec.name, tool.server_id, spec.description, spec.params_schema
        )))
    }
}

/// Calls one MCP tool whose schema was retrieved with `search_tool`.
pub(crate) struct UseTool {
    catalog: Arc<McpCatalog>,
}

impl UseTool {
    pub(crate) fn new(catalog: Arc<McpCatalog>) -> Self {
        Self { catalog }
    }
}

#[async_trait::async_trait]
impl ToolDyn for UseTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "use_tool".to_owned(),
            description: "Call a connected MCP tool. Arguments must match the inputSchema returned by search_tool for that name. Do not call this before search_tool.".to_owned(),
            params_schema: object_schema(
                serde_json::json!({
                    "name": {"type": "string"},
                    "arguments": {"type": "object", "description": "Arguments matching search_tool's inputSchema."}
                }),
                &["name", "arguments"],
            ),
        }
    }

    fn prompt_snippet_dyn(&self) -> Option<&str> {
        Some("use_tool: call an MCP tool only after search_tool returned its schema.")
    }

    async fn execute_dyn(
        &self,
        args: Value,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let name = arg_name(&args)?;
        let Some(tool) = self.catalog.get(&name) else {
            return Err(ToolError::InvalidArgs(format!(
                "no MCP tool named {name}. Connected: {}",
                self.catalog.names
            )));
        };
        let arguments = args
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if !arguments.is_object() {
            return Err(ToolError::InvalidArgs(
                "arguments must be an object".to_owned(),
            ));
        }
        tool.execute_dyn(arguments, ctx, out).await
    }
}

/// Builds and initializes one MCP client for a server row over stdio or
/// HTTP. The channel is constructed with
/// [`crate::mcp_client::DEFAULT_REQUEST_TIMEOUT`], which bounds every
/// request for the channel's lifetime — handshake, tools/list, and each
/// tools/call during a turn.
async fn open_mcp_client(
    server: &mycode_config::McpServerSettings,
    api_key: Option<String>,
) -> Result<crate::mcp_client::McpClient, String> {
    let timeout = crate::mcp_client::DEFAULT_REQUEST_TIMEOUT;
    let channel: Arc<dyn crate::mcp_client::JsonRpcChannel> = match server.transport.as_str() {
        "stdio" => {
            let command = server
                .command
                .as_deref()
                .ok_or("stdio server is missing its command")?;
            let env: Vec<(String, String)> = server
                .env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            Arc::new(
                crate::mcp_client::StdioChannel::spawn(command, &server.args, &env, timeout)
                    .await
                    .map_err(|error| format!("MCP spawn failed: {error}"))?,
            )
        }
        "http" => {
            let endpoint = server
                .endpoint
                .as_deref()
                .ok_or("http server is missing its endpoint")?;
            Arc::new(
                crate::mcp_client::HttpChannel::new(
                    endpoint,
                    crate::mcp_client::HttpChannelOptions {
                        key_header: crate::mcp_client::KeyHeader::parse(
                            server.key_header.as_deref(),
                        ),
                        api_key,
                        timeout,
                    },
                )
                .map_err(|error| format!("MCP channel failed: {error}"))?,
            )
        }
        _ => return Err("unknown MCP transport".to_owned()),
    };
    let mut client = crate::mcp_client::McpClient::new(channel);
    client
        .initialize()
        .await
        .map_err(|error| format!("MCP handshake failed: {error}"))?;
    Ok(client)
}

/// Lists the tools of one MCP server binding over stdio or HTTP.
///
/// The caller supplies the row, so a settings form can test a binding it has
/// not saved yet. Only the API key is read from the vault, because a key is
/// never carried in a command.
///
/// Runs on the caller's runtime; never builds a nested one (a nested
/// `Runtime::block_on` panics and takes the core thread down with it).
pub(crate) async fn mcp_list_tools(
    home: &mycode_config::HomeLayout,
    server: &mycode_config::McpServerSettings,
) -> Result<Vec<String>, String> {
    let secrets = mycode_config::read_provider_secrets(home)
        .map_err(|error| crate::settings_io::render_config_error(&error))?;
    let api_key = secrets
        .key(&format!("mcp-{}", server.id))
        .map(str::to_owned);
    let mut client = open_mcp_client(server, api_key).await?;
    let tools = client
        .list_tools()
        .await
        .map_err(|error| format!("MCP tools listing failed: {error}"))?;
    client.shutdown().await;
    Ok(tools.iter().map(|tool| tool.name.clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_client::McpError;
    use async_trait::async_trait;
    use serde_json::json;

    /// Regression for the 10s connect-phase timeout: the session channel
    /// used to be built with a connect bound, so any tools/call slower than
    /// 10s failed with Timeout and dropped the channel mid-turn. The
    /// scripted stdio server delays its tools/call reply past the old bound;
    /// the call must still succeed under the full request budget.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_slow_tools_call_outlives_the_connect_phase_bound() {
        let script = "\
while ($null -ne ($line = [Console]::In.ReadLine())) {\n\
    if ($line.Trim().Length -eq 0) { continue }\n\
    $req = $line | ConvertFrom-Json\n\
    $result = '{}'\n\
    switch ($req.method) {\n\
        'initialize' { $result = '{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{}}' }\n\
        'tools/list' { $result = '{\"tools\":[{\"name\":\"slow\",\"description\":\"slow tool\",\"inputSchema\":{\"type\":\"object\"}}]}' }\n\
        'tools/call' { Start-Sleep -Seconds 11; $result = '{\"content\":[{\"type\":\"text\",\"text\":\"finally\"}],\"isError\":false}' }\n\
    }\n\
    if ($null -ne $req.id) {\n\
        [Console]::Out.WriteLine('{\"jsonrpc\":\"2.0\",\"id\":' + $req.id + ',\"result\":' + $result + '}')\n\
        [Console]::Out.Flush()\n\
    }\n\
}\n";
        let server = mycode_config::McpServerSettings {
            id: "slow".to_owned(),
            enabled: true,
            transport: "stdio".to_owned(),
            command: Some("powershell".to_owned()),
            args: vec![
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                script.to_owned(),
            ],
            env: std::collections::BTreeMap::new(),
            endpoint: None,
            key_header: None,
        };
        let mut client = open_mcp_client(&server, None).await.expect("client");
        let output = tokio::time::timeout(
            crate::mcp_client::DEFAULT_REQUEST_TIMEOUT,
            client.call_tool("slow", json!({})),
        )
        .await
        .expect("the call completes within the request budget")
        .expect("the slow call succeeds instead of timing out");
        assert_eq!(output.text, "finally");
    }

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

    #[tokio::test]
    async fn search_tool_returns_schema_and_use_tool_calls_it() {
        let tool = tool_over(scripted("found 3 pages")).await;
        let catalog = McpCatalog::from_tools(vec![tool]).expect("catalog");
        let search = SearchTool::new(Arc::clone(&catalog));
        let spec = search.spec();
        assert_eq!(spec.name, "search_tool");
        assert!(spec.params_schema.get("inputSchema").is_none());
        assert!(!spec.description.contains("\"query\""));

        let ctx = ToolCtx::new(".");
        let mut stream = mycode_tools::stream::ToolStream::channel().0;
        let found = search
            .execute_dyn(json!({"name": "search_docs"}), &ctx, &mut stream)
            .await
            .expect("schema");
        let text = match &found.content[0] {
            mycode_core::ContentBlock::Text(text) => text.text.clone(),
            other => panic!("unexpected block: {other:?}"),
        };
        assert!(text.contains("inputSchema"), "{text}");
        assert!(text.contains("query"), "{text}");

        let used = UseTool::new(catalog)
            .execute_dyn(
                json!({"name": "search_docs", "arguments": {"query": "rust"}}),
                &ctx,
                &mut stream,
            )
            .await
            .expect("call");
        match &used.content[0] {
            mycode_core::ContentBlock::Text(text) => assert_eq!(text.text, "found 3 pages"),
            other => panic!("unexpected block: {other:?}"),
        }
    }
}
