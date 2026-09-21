//! Strict visual-settings authority for the desktop product.
//!
//! [`AppSettings`] is the single settings document at `settings.json`: the
//! desktop settings page edits it through typed APIs and it is published with
//! revision compare-and-swap through the hardened owned-file transaction.
//! Secrets never live here; credentials stay in the Host vault.
//!
//! Obsolete product artifacts are not settings inputs. This authority has no
//! migration, compatibility read, layered merge, alias, or fallback.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ConfigError;
use crate::error::ConfigErrorKind;
use crate::secure_fs::owned_file::{locked_update_owned_file, read_owned_file};

/// Settings document path below the owned home.
pub const SETTINGS_PATH: &str = "settings.json";
/// Maximum encoded settings size: 256 KiB.
pub const MAX_SETTINGS_BYTES: usize = 256 * 1024;
/// Settings format version.
pub const SETTINGS_FORMAT_VERSION: u32 = 1;
/// Settings kind tag.
pub const SETTINGS_KIND: &str = "mycode-app-settings";
/// Builds the default outbound User-Agent, matching the pi agent identity
/// `pi (<platform> <release>; <arch>)` (pinned to pi-mono 0.85.1).
#[must_use]
pub fn default_user_agent() -> String {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("pi ({platform} {}; {arch})", os_release())
}

fn os_release() -> String {
    #[cfg(unix)]
    {
        rustix::system::uname()
            .release()
            .to_string_lossy()
            .into_owned()
    }
    #[cfg(windows)]
    {
        windows_release()
    }
    #[cfg(not(any(unix, windows)))]
    {
        "unknown".to_owned()
    }
}

#[cfg(windows)]
fn windows_release() -> String {
    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        dwMajorVersion: 0,
        dwMinorVersion: 0,
        dwBuildNumber: 0,
        dwPlatformId: 0,
        szCSDVersion: [0; 128],
    };
    // SAFETY: `info` is a valid OSVERSIONINFOW with the correct size set;
    // RtlGetVersion only writes into it and reports success via NTSTATUS.
    let status = unsafe { RtlGetVersion(&mut info) };
    if status == 0 {
        format!(
            "{}.{}.{}",
            info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
        )
    } else {
        "unknown".to_owned()
    }
}
/// Maximum provider entries.
pub const MAX_PROVIDERS: usize = 64;
/// Maximum MCP server entries.
pub const MAX_MCP_SERVERS: usize = 64;
/// Maximum web search backends.
pub const MAX_WEB_BACKENDS: usize = 16;
/// Maximum models listed by one provider entry.
pub const MAX_MODELS_PER_PROVIDER: usize = 128;
/// Maximum string field length in bytes.
const MAX_FIELD_BYTES: usize = 8 * 1024;
/// Base URL maximum length.
const MAX_URL_BYTES: usize = 2 * 1024;

/// One configured first-party provider endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderSettings {
    /// Unique provider identity (lowercase portable).
    pub id: String,
    /// Wire protocol: `anthropic-messages`, `openai-completions`, or
    /// `openai-responses`.
    pub kind: String,
    /// Base URL for API calls (`https://` only).
    pub base_url: String,
    /// Models exposed by this provider; the first is the default.
    pub models: Vec<String>,
    /// Enabled in the model picker.
    pub enabled: bool,
    /// Context window override in tokens; absent keeps the catalog value or
    /// the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_limit: Option<u64>,
    /// Max output tokens override; absent keeps the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u64>,
}

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

/// Maximum extra environment variables per stdio server.
pub const MAX_MCP_ENV_VARS: usize = 32;

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

/// Wire families accepted by [`WebBackendSettings::kind`].
pub const VALID_WEB_KINDS: [&str; 3] = ["querit", "anysearch", "custom"];

/// One configured search backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebBackendSettings {
    /// Unique backend identity (lowercase portable).
    pub id: String,
    /// Backend family: `querit`, `anysearch`, or `custom`.
    pub kind: String,
    /// HTTPS API endpoint.
    pub endpoint: String,
    /// Enabled.
    pub enabled: bool,
}

/// Built-in search backends the settings page can add in one click.
///
/// Keys stay in the environment or the vault (`web-<id>`), never here.
#[must_use]
pub fn builtin_web_backends() -> Vec<WebBackendSettings> {
    vec![
        WebBackendSettings {
            id: "querit".to_owned(),
            kind: "querit".to_owned(),
            endpoint: "https://api.querit.ai".to_owned(),
            enabled: false,
        },
        WebBackendSettings {
            id: "anysearch".to_owned(),
            kind: "anysearch".to_owned(),
            endpoint: "https://api.anysearch.com".to_owned(),
            enabled: false,
        },
    ]
}

/// Web search settings: many vendor backends, at most one enabled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSettings {
    /// Configured backends.
    pub backends: Vec<WebBackendSettings>,
}

/// Usage accounting settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageSettings {
    /// Built-in usage accounting enabled.
    pub enabled: bool,
}

/// Appearance settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppearanceSettings {
    /// `light` or `dark`.
    pub theme: String,
}

/// Maximum configured subagent roles.
pub const MAX_SUBAGENT_ROLES: usize = 32;
/// Upper bound on the explicit subagent concurrency setting.
pub const MAX_SUBAGENT_CONCURRENCY: u32 = 6;

/// One role's model route and reasoning override.
///
/// Absent fields mean "inherit": the role runs on the session's own provider
/// and model, at the reasoning level its definition declares. A route names a
/// configured provider id, so it survives a catalog refresh.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubagentRoleSettings {
    /// Role name, matching a catalog role.
    pub role: String,
    /// Whether the parent model may delegate to this role.
    pub enabled: bool,
    /// Provider id this role runs on; absent inherits the session provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model id this role runs on; absent inherits the session model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning effort override: `default`, `low`, `medium`, or `high`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

/// Subagent delegation settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubagentSettings {
    /// Per-role configuration. A catalog role with no entry here is enabled
    /// and fully inherited, so a fresh install has a working team.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<SubagentRoleSettings>,
    /// Simultaneous subagent limit; `0` keeps the automatic capacity.
    #[serde(default)]
    pub max_concurrent: u32,
}

impl SubagentSettings {
    /// Returns the stored entry for one role, if any.
    #[must_use]
    pub fn role(&self, name: &str) -> Option<&SubagentRoleSettings> {
        self.roles.iter().find(|entry| entry.role == name)
    }

    /// Whether the parent model may delegate to one role.
    ///
    /// Roles are opt-out: a role with no stored entry is available.
    #[must_use]
    pub fn is_enabled(&self, name: &str) -> bool {
        self.role(name).is_none_or(|entry| entry.enabled)
    }

    /// Returns the mutable entry for one role, inserting an inherited default.
    pub fn role_mut(&mut self, name: &str) -> &mut SubagentRoleSettings {
        if let Some(index) = self.roles.iter().position(|entry| entry.role == name) {
            return &mut self.roles[index];
        }
        self.roles.push(SubagentRoleSettings {
            role: name.to_owned(),
            enabled: true,
            provider: None,
            model: None,
            thinking: None,
        });
        self.roles.last_mut().expect("just pushed")
    }
}

/// Accepted `tools.shell.kind` values.
pub const VALID_SHELL_KINDS: [&str; 4] = ["pwsh", "powershell", "cmd", "bash"];

/// One resolved platform shell used by the `shell` tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellSettings {
    /// Interpreter family: `pwsh`, `powershell`, `cmd`, or `bash`.
    pub kind: String,
    /// Absolute path of the shell executable.
    pub program: String,
    /// `auto` after first-run detection; `user` after an explicit pick.
    #[serde(default, skip_serializing_if = "shell_source_is_auto")]
    pub source: String,
}

fn shell_source_is_auto(source: &str) -> bool {
    source.is_empty() || source == "auto"
}

impl Default for ShellSettings {
    fn default() -> Self {
        Self {
            kind: String::new(),
            program: String::new(),
            source: "auto".to_owned(),
        }
    }
}

/// Tool-runtime preferences persisted in settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolsSettings {
    /// Platform shell used by the `shell` tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<ShellSettings>,
}

fn tools_are_default(tools: &ToolsSettings) -> bool {
    *tools == ToolsSettings::default()
}

/// The complete settings document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppSettings {
    /// Outbound User-Agent. Defaults to the pi agent identity so the
    /// value is present in `settings.json` and stays configurable.
    pub user_agent: String,
    /// Configured providers.
    pub providers: Vec<ProviderSettings>,
    /// Web search settings.
    pub web: WebSettings,
    /// Usage settings.
    pub usage: UsageSettings,
    /// MCP servers.
    pub mcp_servers: Vec<McpServerSettings>,
    /// Appearance.
    pub appearance: AppearanceSettings,
    /// Requested reasoning effort from models.dev options; absent keeps the
    /// provider default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Subagent delegation settings.
    #[serde(default, skip_serializing_if = "subagents_are_default")]
    pub subagents: SubagentSettings,
    /// Tool-runtime preferences, including the platform shell.
    #[serde(default, skip_serializing_if = "tools_are_default")]
    pub tools: ToolsSettings,
}

fn subagents_are_default(subagents: &SubagentSettings) -> bool {
    *subagents == SubagentSettings::default()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            user_agent: default_user_agent(),
            providers: Vec::new(),
            // Both first-class vendors ship ready; the user pastes an API
            // key and enables one. At most one backend may be enabled.
            web: WebSettings {
                backends: builtin_web_backends(),
            },
            usage: UsageSettings { enabled: true },
            mcp_servers: Vec::new(),
            appearance: AppearanceSettings {
                theme: "dark".to_owned(),
            },
            reasoning_effort: None,
            subagents: SubagentSettings::default(),
            tools: ToolsSettings::default(),
        }
    }
}

impl AppSettings {
    /// Returns the effective User-Agent, falling back to the pi agent
    /// identity when unset.
    #[must_use]
    pub fn effective_user_agent(&self) -> String {
        let configured = self.user_agent.trim();
        if configured.is_empty() {
            default_user_agent()
        } else {
            configured.to_owned()
        }
    }

    /// Returns the effective theme (`light` or `dark`).
    #[must_use]
    pub fn effective_theme(&self) -> &'static str {
        if self.appearance.theme == "light" {
            "light"
        } else {
            "dark"
        }
    }

    /// Validates the complete document.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] for any bound,
    /// grammar, or cross-field violation.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid =
            |detail: &str| ConfigError::authority_rejection().with_detail(detail.to_owned());
        bounded_text(&self.user_agent, MAX_FIELD_BYTES)
            .map_err(|_| invalid("userAgent: too long or contains control characters"))?;
        if let Some(level) = self.reasoning_effort.as_deref()
            && (level == "default" || crate::RoleThinking::parse(level).is_none())
        {
            return Err(invalid(
                "reasoningEffort: must be a models.dev option (off, on, minimal, low, medium, high, xhigh, max)",
            ));
        }
        if self.providers.len() > MAX_PROVIDERS {
            return Err(invalid("providers: too many entries"));
        }
        for (index, provider) in self.providers.iter().enumerate() {
            let field = format!("providers[{index}]");
            if !is_portable_id(&provider.id) {
                return Err(invalid(&format!(
                    "{field}.id: must be letters, digits, dash, dot, or underscore"
                )));
            }
            if !VALID_PROVIDER_KINDS.contains(&provider.kind.as_str()) {
                return Err(invalid(&format!(
                    "{field}.kind: must be one of anthropic-messages, openai-completions, openai-responses"
                )));
            }
            if !is_https_url(&provider.base_url) {
                return Err(invalid(&format!(
                    "{field}.baseUrl: must be an https:// URL"
                )));
            }
            if provider.models.is_empty() || provider.models.len() > MAX_MODELS_PER_PROVIDER {
                return Err(invalid(&format!(
                    "{field}.models: list at least one model id (at most {MAX_MODELS_PER_PROVIDER})"
                )));
            }
            if self.providers[..index].iter().any(|p| p.id == provider.id) {
                return Err(invalid(&format!(
                    "{field}.id: duplicates an earlier provider id"
                )));
            }
            for (model_index, model) in provider.models.iter().enumerate() {
                bounded_text(model, MAX_FIELD_BYTES).map_err(|_| {
                    invalid(&format!(
                        "{field}.models[{model_index}]: too long or contains control characters"
                    ))
                })?;
            }
        }
        if self.web.backends.len() > MAX_WEB_BACKENDS {
            return Err(invalid("web.backends: too many entries"));
        }
        for (index, backend) in self.web.backends.iter().enumerate() {
            let field = format!("web.backends[{index}]");
            if !is_portable_id(&backend.id) {
                return Err(invalid(&format!(
                    "{field}.id: must be letters, digits, dash, dot, or underscore"
                )));
            }
            if !VALID_WEB_KINDS.contains(&backend.kind.as_str()) {
                return Err(invalid(&format!(
                    "{field}.kind: must be querit, anysearch, or custom"
                )));
            }
            if !is_https_url(&backend.endpoint) {
                return Err(invalid(&format!(
                    "{field}.endpoint: must be an https:// URL"
                )));
            }
            if self.web.backends[..index]
                .iter()
                .any(|b| b.id == backend.id)
            {
                return Err(invalid(&format!(
                    "{field}.id: duplicates an earlier backend id"
                )));
            }
        }
        if self.web.backends.iter().filter(|b| b.enabled).count() > 1 {
            return Err(invalid("web.backends: at most one backend may be enabled"));
        }
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
        if self.appearance.theme != "light" && self.appearance.theme != "dark" {
            return Err(invalid("appearance.theme: must be light or dark"));
        }
        if self.subagents.max_concurrent > MAX_SUBAGENT_CONCURRENCY {
            return Err(invalid(&format!(
                "subagents.maxConcurrent: must be 0 (automatic) through {MAX_SUBAGENT_CONCURRENCY}"
            )));
        }
        if self.subagents.roles.len() > MAX_SUBAGENT_ROLES {
            return Err(invalid("subagents.roles: too many entries"));
        }
        for (index, entry) in self.subagents.roles.iter().enumerate() {
            let field = format!("subagents.roles[{index}]");
            if !crate::is_portable_role_name(&entry.role) {
                return Err(invalid(&format!(
                    "{field}.role: must be 1-64 lowercase letters, digits, dash, dot, or underscore"
                )));
            }
            if self.subagents.roles[..index]
                .iter()
                .any(|earlier| earlier.role == entry.role)
            {
                return Err(invalid(&format!(
                    "{field}.role: duplicates an earlier role entry"
                )));
            }
            // A model route without its provider cannot be resolved, and a
            // provider route with no model would silently pick a default the
            // settings page never showed.
            match (entry.provider.as_deref(), entry.model.as_deref()) {
                (Some(provider), Some(model)) => {
                    if !self
                        .providers
                        .iter()
                        .any(|configured| configured.id == provider)
                    {
                        return Err(invalid(&format!(
                            "{field}.provider: \"{provider}\" is not a configured provider"
                        )));
                    }
                    bounded_text(model, MAX_FIELD_BYTES).map_err(|_| {
                        invalid(&format!(
                            "{field}.model: too long or contains control characters"
                        ))
                    })?;
                }
                (None, None) => {}
                _ => {
                    return Err(invalid(&format!(
                        "{field}: set provider and model together, or neither to inherit the session model"
                    )));
                }
            }
            if let Some(level) = entry.thinking.as_deref()
                && crate::RoleThinking::parse(level).is_none()
            {
                return Err(invalid(&format!(
                    "{field}.thinking: must be a models.dev option (default, off, on, minimal, low, medium, high, xhigh, max)"
                )));
            }
        }
        if let Some(shell) = self.tools.shell.as_ref() {
            if !VALID_SHELL_KINDS.contains(&shell.kind.as_str()) {
                return Err(invalid(
                    "tools.shell.kind: must be pwsh, powershell, cmd, or bash",
                ));
            }
            let program = shell.program.trim();
            if program.is_empty() {
                return Err(invalid("tools.shell.program: set an executable path"));
            }
            bounded_text(program, MAX_FIELD_BYTES).map_err(|_| {
                invalid("tools.shell.program: too long or contains control characters")
            })?;
            if !shell.source.is_empty() && shell.source != "auto" && shell.source != "user" {
                return Err(invalid("tools.shell.source: must be auto or user"));
            }
        }
        Ok(())
    }
}

/// Wire protocols accepted by [`ProviderSettings::kind`].
///
/// Vendor differences (DeepSeek, Kimi, Z.AI GLM, custom gateways, …) are data:
/// a base URL plus credentials over one of these protocols. Adding a vendor
/// never adds an adapter family.
pub const VALID_PROVIDER_KINDS: [&str; 3] = [
    "anthropic-messages",
    "openai-completions",
    "openai-responses",
];

/// Reads and validates `settings.json` without creating filesystem objects.
///
/// A missing document yields the defaults; present documents must validate
/// strictly.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security, oversized content, or
/// strict validation failures.
pub fn read_app_settings(home: &crate::HomeLayout) -> Result<AppSettings, ConfigError> {
    let bytes = read_owned_file(home, SETTINGS_PATH, MAX_SETTINGS_BYTES)?;
    let Some(bytes) = bytes else {
        return Ok(AppSettings::default());
    };
    parse_settings(bytes.as_slice())
}

/// Replaces `settings.json` under revision compare-and-swap.
///
/// A missing document has logical revision zero. The replacement is fully
/// validated before publication.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::RevisionConflict`] for a stale expectation and
/// [`ConfigError`] for validation or transaction failures.
pub fn replace_app_settings(
    home: &crate::HomeLayout,
    expected_revision: AuthorityRevision,
    settings: &AppSettings,
) -> Result<AuthorityRevision, ConfigError> {
    settings.validate()?;
    let mut published_revision = None;
    locked_update_owned_file(home, SETTINGS_PATH, MAX_SETTINGS_BYTES, |current| {
        let current_revision = match current {
            Some(bytes) => parse_document_header(bytes)?,
            None => AuthorityRevision::ABSENT,
        };
        if current_revision != expected_revision {
            return Err(ConfigError::new(ConfigErrorKind::RevisionConflict));
        }
        let revision = current_revision.checked_next()?;
        let document = SerializedSettings {
            format_version: SETTINGS_FORMAT_VERSION,
            kind: SETTINGS_KIND,
            revision: revision.get(),
            settings,
        };
        let mut bytes = serde_json::to_vec_pretty(&document)
            .map_err(|_| ConfigError::new(ConfigErrorKind::Serialization))?;
        bytes.push(b'\n');
        if bytes.len() > MAX_SETTINGS_BYTES {
            return Err(ConfigError::new(ConfigErrorKind::Oversized));
        }
        published_revision = Some(revision);
        Ok(bytes)
    })?;
    published_revision.ok_or_else(|| ConfigError::new(ConfigErrorKind::Serialization))
}

/// The persisted settings revision.
pub use crate::authority::AuthorityRevision;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SerializedSettings<'a> {
    format_version: u32,
    kind: &'static str,
    revision: u64,
    #[serde(flatten)]
    settings: &'a AppSettings,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeserializedSettings {
    format_version: u32,
    kind: String,
    revision: u64,
    #[serde(flatten)]
    settings: AppSettings,
}

/// Reads the current revision without full validation of the body.
fn parse_document_header(bytes: &[u8]) -> Result<AuthorityRevision, ConfigError> {
    let header: DeserializedSettings =
        serde_json::from_slice(bytes).map_err(|_| ConfigError::authority_rejection())?;
    if header.format_version != SETTINGS_FORMAT_VERSION || header.kind != SETTINGS_KIND {
        return Err(ConfigError::authority_rejection());
    }
    AuthorityRevision::new(header.revision)
}

fn parse_settings(bytes: &[u8]) -> Result<AppSettings, ConfigError> {
    let document: DeserializedSettings = serde_json::from_slice(bytes).map_err(|_| {
        ConfigError::authority_rejection().with_detail(
            "settings.json: unknown field or wrong value type (check providers, web, mcpServers, usage, appearance)",
        )
    })?;
    if document.format_version != SETTINGS_FORMAT_VERSION || document.kind != SETTINGS_KIND {
        return Err(ConfigError::authority_rejection());
    }
    AuthorityRevision::new(document.revision)?;
    document.settings.validate()?;
    Ok(document.settings)
}

fn bounded_text(value: &str, max: usize) -> Result<(), ConfigError> {
    if value.len() <= max && !value.contains(['\0', '\r', '\n']) {
        Ok(())
    } else {
        Err(ConfigError::authority_rejection())
    }
}

fn is_portable_id(value: &str) -> bool {
    crate::home::is_valid_portable_id(value)
}

fn is_https_url(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("https://") else {
        return false;
    };
    if value.len() > MAX_URL_BYTES {
        return false;
    }
    let host = rest.split('/').next().unwrap_or_default();
    !host.is_empty()
        && !host
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte == 0)
        && host
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HomeLayout;

    fn layout() -> (tempfile::TempDir, HomeLayout) {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        (parent, layout)
    }

    fn provider(id: &str, kind: &str, base: &str) -> ProviderSettings {
        ProviderSettings {
            id: id.to_owned(),
            kind: kind.to_owned(),
            base_url: base.to_owned(),
            models: vec!["model-a".to_owned()],
            enabled: true,
            context_limit: None,
            max_output: None,
        }
    }

    #[test]
    fn builtin_catalog_and_transport_validation() {
        let catalog = builtin_mcp_servers();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].id, "context7");
        assert_eq!(catalog[0].transport, "http");

        let mut settings = AppSettings {
            mcp_servers: catalog,
            ..AppSettings::default()
        };
        assert!(settings.validate().is_ok(), "built-in servers validate");

        settings.mcp_servers[0].enabled = true;
        assert!(settings.validate().is_ok());

        settings.mcp_servers[0].endpoint = Some("http://insecure.example.com".to_owned());
        assert!(settings.validate().is_err(), "http endpoint rejected");
        settings.mcp_servers[0].endpoint = Some("https://mcp.context7.com/mcp".to_owned());
        settings.mcp_servers[0].key_header = Some("cookie".to_owned());
        assert!(settings.validate().is_err(), "unknown key header rejected");
        settings.mcp_servers[0].key_header = Some("x-api-key".to_owned());
        assert!(settings.validate().is_ok());

        settings.mcp_servers[0].transport = "stdio".to_owned();
        assert!(
            settings.validate().is_err(),
            "http fields on stdio rejected"
        );
        settings.mcp_servers[0] = McpServerSettings {
            id: "local".to_owned(),
            enabled: true,
            transport: "stdio".to_owned(),
            command: Some("npx".to_owned()),
            args: vec!["-y".to_owned(), "@x/y".to_owned()],
            env: BTreeMap::from([("LOG_LEVEL".to_owned(), "debug".to_owned())]),
            endpoint: None,
            key_header: None,
        };
        assert!(settings.validate().is_ok(), "valid stdio accepted");
        settings.mcp_servers[0].endpoint = Some("https://x.example.com".to_owned());
        assert!(settings.validate().is_err(), "endpoint on stdio rejected");
        settings.mcp_servers[0].endpoint = None;
        settings.mcp_servers[0]
            .env
            .insert("BAD=NAME".to_owned(), "x".to_owned());
        assert!(settings.validate().is_err(), "env name with '=' rejected");
        settings.mcp_servers[0].env.clear();
        settings.mcp_servers[0].command = Some("   ".to_owned());
        assert!(settings.validate().is_err(), "blank command rejected");
    }

    #[test]
    fn command_lines_split_like_a_shell() {
        assert_eq!(
            split_command_line("npx -y @modelcontextprotocol/server-filesystem \"C:\\My Docs\""),
            vec![
                "npx",
                "-y",
                "@modelcontextprotocol/server-filesystem",
                "C:\\My Docs"
            ]
        );
        assert_eq!(
            split_command_line("  uvx   mcp-server-git --repo 'a b'  "),
            vec!["uvx", "mcp-server-git", "--repo", "a b"]
        );
        assert_eq!(
            split_command_line(r#"node "say \"hi\"" x"#),
            vec!["node", "say \"hi\"", "x"]
        );
        assert_eq!(split_command_line("\"\""), vec![""]);
        assert!(split_command_line("   ").is_empty());
    }

    #[test]
    fn provider_parameter_overrides_round_trip() {
        let (_parent, layout) = layout();
        let mut settings = AppSettings::default();
        settings.providers.push(ProviderSettings {
            id: "custom-main".to_owned(),
            kind: "openai-completions".to_owned(),
            base_url: "https://api.custom.dev/v1".to_owned(),
            models: vec!["custom-x".to_owned()],
            enabled: true,
            context_limit: Some(200_000),
            max_output: Some(8_192),
        });
        let revision =
            replace_app_settings(&layout, AuthorityRevision::ABSENT, &settings).expect("save");
        let read = read_app_settings(&layout).expect("read");
        assert_eq!(read.providers, settings.providers);
        assert_eq!(revision.get(), 1);
    }

    #[test]
    fn missing_settings_default_without_touching_disk() {
        let (parent, layout) = layout();
        let settings = read_app_settings(&layout).expect("defaults");
        assert_eq!(settings, AppSettings::default());
        let default_ua = settings.effective_user_agent();
        assert!(
            default_ua.starts_with("pi (") && default_ua.ends_with(')'),
            "pi agent identity, got {default_ua:?}"
        );
        assert_eq!(settings.effective_theme(), "dark");
        assert_eq!(std::fs::read_dir(parent.path()).expect("parent").count(), 0);
    }

    #[test]
    fn replace_then_read_roundtrip_with_revision_cas() {
        let (_parent, layout) = layout();
        let mut settings = AppSettings::default();
        settings.providers.push(provider(
            "openai-main",
            "openai-completions",
            "https://api.openai.com/v1",
        ));
        settings.web.backends.push(WebBackendSettings {
            id: "querit-main".to_owned(),
            kind: "querit".to_owned(),
            endpoint: "https://querit.example.com".to_owned(),
            enabled: true,
        });
        settings.mcp_servers.push(McpServerSettings {
            id: "docs".to_owned(),
            enabled: false,
            transport: "stdio".to_owned(),
            command: Some("npx".to_owned()),
            args: vec![
                "-y".to_owned(),
                "@modelcontextprotocol/server-docs".to_owned(),
            ],
            env: BTreeMap::new(),
            endpoint: None,
            key_header: None,
        });

        let first = replace_app_settings(&layout, AuthorityRevision::ABSENT, &settings)
            .expect("first publish");
        assert_eq!(first.get(), 1);
        let read = read_app_settings(&layout).expect("read");
        assert_eq!(read, settings);

        let stale = replace_app_settings(&layout, AuthorityRevision::ABSENT, &settings);
        assert_eq!(
            stale.expect_err("stale CAS").kind(),
            ConfigErrorKind::RevisionConflict
        );

        settings.user_agent = "mycode-test/1".to_owned();
        let second = replace_app_settings(&layout, first, &settings).expect("second publish");
        assert_eq!(second.get(), 2);
        assert_eq!(
            read_app_settings(&layout).expect("reread").user_agent,
            "mycode-test/1"
        );
    }

    #[test]
    fn validation_rejects_bad_kinds_urls_duplicates_and_themes() {
        let mut settings = AppSettings::default();
        settings
            .providers
            .push(provider("p1", "unknown-kind", "https://api.example.com"));
        assert!(settings.validate().is_err());

        settings.providers[0].kind = "openai-completions".to_owned();
        settings.providers[0].base_url = "http://insecure.example.com".to_owned();
        assert!(settings.validate().is_err(), "http base URL");

        settings.providers[0].base_url = "https://api.example.com".to_owned();
        settings.providers.push(provider(
            "p1",
            "openai-completions",
            "https://api.moonshot.cn/v1",
        ));
        assert!(settings.validate().is_err(), "duplicate provider id");

        settings.providers.truncate(1);
        settings.providers[0].models.clear();
        assert!(settings.validate().is_err(), "model list required");

        settings.providers[0].models = vec!["m".to_owned()];
        settings.appearance.theme = "blue".to_owned();
        assert!(settings.validate().is_err(), "theme vocabulary");

        settings.appearance.theme = "light".to_owned();
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn web_backends_allow_many_vendors_but_one_active() {
        let mut settings = AppSettings::default();
        for id in ["a", "b", "c"] {
            settings.web.backends.push(WebBackendSettings {
                id: id.to_owned(),
                kind: "querit".to_owned(),
                endpoint: "https://search.example.com".to_owned(),
                enabled: false,
            });
        }
        assert!(settings.validate().is_ok());

        settings.web.backends[0].enabled = true;
        settings.web.backends[1].enabled = true;
        assert!(settings.validate().is_err(), "two active search backends");

        settings.web.backends[1].enabled = false;
        settings.web.backends[2].kind = "unknown".to_owned();
        assert!(settings.validate().is_err(), "backend kind vocabulary");

        settings.web.backends[2].kind = "anysearch".to_owned();
        settings.web.backends[2].endpoint = "https://api.anysearch.com".to_owned();
        assert!(
            settings.validate().is_ok(),
            "anysearch is a first-class kind"
        );

        settings.web.backends[2].kind = "custom".to_owned();
        settings.web.backends[2].endpoint = "http://plain.example.com".to_owned();
        assert!(settings.validate().is_err(), "https only");
    }

    #[test]
    fn builtin_web_backends_are_the_two_first_class_vendors() {
        let backends = builtin_web_backends();
        let kinds: Vec<&str> = backends
            .iter()
            .map(|backend| backend.kind.as_str())
            .collect();
        assert_eq!(kinds, ["querit", "anysearch"]);
        assert!(backends.iter().all(|backend| !backend.enabled));
        assert!(backends.iter().all(settings_https));
    }

    fn settings_https(backend: &WebBackendSettings) -> bool {
        backend.endpoint.starts_with("https://")
    }

    #[test]
    fn malformed_documents_fail_closed() {
        let (_parent, layout) = layout();
        let settings = AppSettings {
            user_agent: "ua".to_owned(),
            ..AppSettings::default()
        };
        replace_app_settings(&layout, AuthorityRevision::ABSENT, &settings).expect("publish");

        let path = layout.owned_join(SETTINGS_PATH).expect("path");
        std::fs::write(&path, b"{\"formatVersion\":2}").expect("tamper");
        assert!(read_app_settings(&layout).is_err());

        std::fs::write(&path, b"not json at all").expect("tamper");
        assert!(read_app_settings(&layout).is_err());
    }

    #[test]
    fn tools_shell_validates_kind_and_program_together() {
        let mut settings = AppSettings::default();
        assert!(settings.validate().is_ok());

        settings.tools.shell = Some(ShellSettings {
            kind: "pwsh".to_owned(),
            program: String::new(),
            source: "auto".to_owned(),
        });
        assert!(settings.validate().is_err(), "empty program");

        settings.tools.shell = Some(ShellSettings {
            kind: "fish".to_owned(),
            program: r"C:\shell\pwsh.exe".to_owned(),
            source: "user".to_owned(),
        });
        assert!(settings.validate().is_err(), "unknown kind");

        settings.tools.shell = Some(ShellSettings {
            kind: "pwsh".to_owned(),
            program: r"C:\Program Files\PowerShell\7\pwsh.exe".to_owned(),
            source: "auto".to_owned(),
        });
        assert!(settings.validate().is_ok());
    }
}
