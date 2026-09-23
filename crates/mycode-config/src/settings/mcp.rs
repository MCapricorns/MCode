//! MCP-server settings family.
//!
//! Stdio servers spawn a local command, so the command-line splitter used by
//! the settings form lives here next to its consumer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{AppSettings, MAX_FIELD_BYTES, bounded_text, is_https_url, is_portable_id};
use crate::ConfigError;

/// Maximum MCP server entries.
pub const MAX_MCP_SERVERS: usize = 64;
/// Maximum extra environment variables per stdio server.
pub const MAX_MCP_ENV_VARS: usize = 32;

/// One configured MCP server binding.
///
/// Stdio servers spawn a local command; HTTP servers speak the MCP
/// Streamable-HTTP wire against one https endpoint. The optional API key is
/// stored in the secret store under `mcp-<id>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerSettings {
    /// Unique server identity (lowercase portable).
    pub id: String,
    /// Enabled.
    pub enabled: bool,
    /// Transport: `stdio` or `http`.
    #[serde(rename = "transport")]
    pub transport: String,
    /// Stdio: executable command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Stdio: bounded argument list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Stdio: extra environment variables for the child process. Secrets
    /// belong in the key vault, not here; this is for switches and paths.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// HTTP: full https endpoint URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// HTTP: credential header style, `bearer` (default) or `x-api-key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_header: Option<String>,
}

/// The built-in recommended MCP servers offered by the settings UI.
///
/// Users add them with one click and supply their own API keys; keys live in
/// the secret store, never in settings.
pub fn builtin_mcp_servers() -> Vec<McpServerSettings> {
    vec![McpServerSettings {
        id: "context7".to_owned(),
        enabled: true,
        transport: "http".to_owned(),
        command: None,
        args: Vec::new(),
        env: BTreeMap::new(),
        endpoint: Some("https://mcp.context7.com/mcp".to_owned()),
        key_header: Some("bearer".to_owned()),
    }]
}

/// Splits one command line into the program and its arguments.
///
/// Whitespace separates words; single or double quotes group a word. Inside
/// double quotes a backslash escapes only `"` and `\`, so Windows paths such
/// as `"C:\My Docs"` survive. This is the shape MCP server docs publish
/// (`npx -y @scope/server --flag "a b"`), so the settings form can take the
/// whole line in one field.
#[must_use]
pub fn split_command_line(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some('"') if ch == '\\' && matches!(chars.peek(), Some('"' | '\\')) => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                in_word = true;
            }
            None if ch.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            None => {
                current.push(ch);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(current);
    }
    words
}

impl AppSettings {
    /// Validates the MCP family: entry bounds, transport-specific fields,
    /// environment-variable grammar, and duplicate ids.
    pub(super) fn validate_mcp(&self) -> Result<(), ConfigError> {
        let invalid =
            |detail: &str| ConfigError::authority_rejection().with_detail(detail.to_owned());
        if self.mcp_servers.len() > MAX_MCP_SERVERS {
            return Err(invalid("mcpServers: too many entries"));
        }
        for (index, server) in self.mcp_servers.iter().enumerate() {
            let field = format!("mcpServers[{index}]");
            if !is_portable_id(&server.id) {
                return Err(invalid(&format!(
                    "{field}.id: must be letters, digits, dash, dot, or underscore"
                )));
            }
            if server.args.len() > 64 {
                return Err(invalid(&format!("{field}.args: too many entries")));
            }
            for (arg_index, arg) in server.args.iter().enumerate() {
                bounded_text(arg, MAX_FIELD_BYTES).map_err(|_| {
                    invalid(&format!(
                        "{field}.args[{arg_index}]: too long or contains control characters"
                    ))
                })?;
            }
            match server.transport.as_str() {
                "stdio" => {
                    let command = server.command.as_deref().unwrap_or_default().trim();
                    if command.is_empty() {
                        return Err(invalid(&format!(
                            "{field}: stdio transport requires a command"
                        )));
                    }
                    if server.endpoint.is_some() || server.key_header.is_some() {
                        return Err(invalid(&format!(
                            "{field}: stdio transport must not set endpoint or keyHeader"
                        )));
                    }
                    bounded_text(command, MAX_FIELD_BYTES)
                        .map_err(|_| invalid(&format!("{field}.command: too long")))?;
                    if server.env.len() > MAX_MCP_ENV_VARS {
                        return Err(invalid(&format!("{field}.env: too many entries")));
                    }
                    for (key, value) in &server.env {
                        if key.is_empty()
                            || key.contains('=')
                            || key.chars().any(|ch| ch.is_control() || ch == '\0')
                        {
                            return Err(invalid(&format!(
                                "{field}.env: variable names must be nonempty and free of '='"
                            )));
                        }
                        bounded_text(value, MAX_FIELD_BYTES).map_err(|_| {
                            invalid(&format!(
                                "{field}.env.{key}: too long or contains control characters"
                            ))
                        })?;
                    }
                }
                "http" => {
                    let endpoint_ok = server
                        .endpoint
                        .as_deref()
                        .map(is_https_url)
                        .unwrap_or(false);
                    if !endpoint_ok {
                        return Err(invalid(&format!(
                            "{field}.endpoint: must be an https:// URL"
                        )));
                    }
                    if server.command.is_some() || !server.args.is_empty() || !server.env.is_empty()
                    {
                        return Err(invalid(&format!(
                            "{field}: http transport must not set command, args, or env"
                        )));
                    }
                    if server
                        .key_header
                        .as_deref()
                        .is_some_and(|header| !matches!(header, "bearer" | "x-api-key"))
                    {
                        return Err(invalid(&format!(
                            "{field}.keyHeader: must be exactly \"bearer\" or \"x-api-key\" — keep the API key in the app key vault, not in this field"
                        )));
                    }
                }
                _ => {
                    return Err(invalid(&format!(
                        "{field}.transport: must be stdio or http"
                    )));
                }
            }
            if self.mcp_servers[..index].iter().any(|s| s.id == server.id) {
                return Err(invalid(&format!(
                    "{field}.id: duplicates an earlier server id"
                )));
            }
        }
        Ok(())
    }
}
