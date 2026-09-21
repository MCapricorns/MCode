//! Strict visual-settings authority for the desktop product.
//!
//! [`AppSettings`] is the single settings document at `settings.json`: the
//! desktop settings page edits it through typed APIs and it is published with
//! revision compare-and-swap through the hardened owned-file transaction.
//! Secrets never live here; credentials stay in the Host vault.
//!
//! Obsolete product artifacts are not settings inputs. This authority has no
//! migration, compatibility read, layered merge, alias, or fallback.

mod mcp;
mod providers;
mod subagent_roles;
mod tools_shell;
mod user_agent;
mod web;

pub use mcp::{
    MAX_MCP_ENV_VARS, MAX_MCP_SERVERS, McpServerSettings, builtin_mcp_servers, split_command_line,
};
pub use providers::{
    MAX_MODELS_PER_PROVIDER, MAX_PROVIDERS, ProviderSettings, VALID_PROVIDER_KINDS,
};
pub use subagent_roles::{
    MAX_SUBAGENT_CONCURRENCY, MAX_SUBAGENT_ROLES, SubagentRoleSettings, SubagentSettings,
};
pub use tools_shell::{ShellSettings, ToolsSettings, VALID_SHELL_KINDS};
pub use user_agent::default_user_agent;
pub use web::{
    MAX_WEB_BACKENDS, VALID_WEB_KINDS, WebBackendSettings, WebSettings, builtin_web_backends,
};

use serde::{Deserialize, Serialize};

use subagent_roles::subagents_are_default;
use tools_shell::tools_are_default;

use crate::ConfigError;
use crate::authority::AuthorityRevision;
use crate::error::ConfigErrorKind;
use crate::secure_fs::owned_file::{locked_update_owned_file, read_owned_file};

/// Settings document path below the owned home.
pub const SETTINGS_PATH: &str = "settings.json";
/// Maximum encoded authority document size: 256 KiB.
///
/// The cap is domain-neutral on purpose: the todos, compaction, and export
/// authorities bound their documents with the same limit.
pub const MAX_AUTHORITY_DOCUMENT_BYTES: usize = 256 * 1024;
/// Back-compat alias of [`MAX_AUTHORITY_DOCUMENT_BYTES`] under the previous
/// settings-scoped name, kept so existing cross-crate callers keep compiling.
pub const MAX_SETTINGS_BYTES: usize = MAX_AUTHORITY_DOCUMENT_BYTES;
/// Settings format version.
pub const SETTINGS_FORMAT_VERSION: u32 = 1;
/// Settings kind tag.
pub const SETTINGS_KIND: &str = "mycode-app-settings";
/// Maximum string field length in bytes.
pub(super) const MAX_FIELD_BYTES: usize = 8 * 1024;
/// Base URL maximum length.
pub(super) const MAX_URL_BYTES: usize = 2 * 1024;

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
    /// Family-specific bounds, grammar, and cross-field rules live in the
    /// owning submodule and are invoked in document order.
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
        self.validate_providers()?;
        self.validate_web()?;
        self.validate_mcp()?;
        if self.appearance.theme != "light" && self.appearance.theme != "dark" {
            return Err(invalid("appearance.theme: must be light or dark"));
        }
        self.validate_subagent_roles()?;
        self.validate_tools_shell()?;
        Ok(())
    }
}

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
    let bytes = read_owned_file(home, SETTINGS_PATH, MAX_AUTHORITY_DOCUMENT_BYTES)?;
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
    locked_update_owned_file(
        home,
        SETTINGS_PATH,
        MAX_AUTHORITY_DOCUMENT_BYTES,
        |current| {
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
            if bytes.len() > MAX_AUTHORITY_DOCUMENT_BYTES {
                return Err(ConfigError::new(ConfigErrorKind::Oversized));
            }
            published_revision = Some(revision);
            Ok(bytes)
        },
    )?;
    published_revision.ok_or_else(|| ConfigError::new(ConfigErrorKind::Serialization))
}

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
    use std::collections::BTreeMap;

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
}
