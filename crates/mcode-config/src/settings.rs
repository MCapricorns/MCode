//! Strict visual-settings authority for the desktop product.
//!
//! [`AppSettings`] is the single settings document at `settings.json`: the
//! desktop settings page edits it through typed APIs and it is published with
//! revision compare-and-swap through the hardened owned-file transaction.
//! Secrets never live here; credentials stay in the Host vault.
//!
//! Obsolete product artifacts are not settings inputs. This authority has no
//! migration, compatibility read, layered merge, alias, or fallback.
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
pub const SETTINGS_KIND: &str = "mcode-app-settings";
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
    #[cfg(target_os = "macos")]
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
    #[cfg(not(any(target_os = "macos", windows)))]
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
        enabled: false,
        transport: "http".to_owned(),
        command: None,
        args: Vec::new(),
        endpoint: Some("https://mcp.context7.com/mcp".to_owned()),
        key_header: Some("bearer".to_owned()),
    }]
}

/// One configured search backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebBackendSettings {
    /// Unique backend identity (lowercase portable).
    pub id: String,
    /// Backend family: `querit` or `custom`.
    pub kind: String,
    /// HTTPS API endpoint.
    pub endpoint: String,
    /// Enabled.
    pub enabled: bool,
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

/// The complete settings document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppSettings {
    /// Outbound User-Agent; empty means [`DEFAULT_USER_AGENT`].
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
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            user_agent: String::new(),
            providers: Vec::new(),
            web: WebSettings {
                backends: Vec::new(),
            },
            usage: UsageSettings { enabled: true },
            mcp_servers: Vec::new(),
            appearance: AppearanceSettings {
                theme: "dark".to_owned(),
            },
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
        let invalid = || ConfigError::authority_rejection();
        bounded_text(&self.user_agent, MAX_FIELD_BYTES)?;
        if self.providers.len() > MAX_PROVIDERS {
            return Err(invalid());
        }
        for (index, provider) in self.providers.iter().enumerate() {
            if !is_portable_id(&provider.id)
                || !VALID_PROVIDER_KINDS.contains(&provider.kind.as_str())
                || !is_https_url(&provider.base_url)
                || provider.models.is_empty()
                || provider.models.len() > MAX_MODELS_PER_PROVIDER
            {
                return Err(invalid());
            }
            if self.providers[..index].iter().any(|p| p.id == provider.id) {
                return Err(invalid());
            }
            for model in &provider.models {
                bounded_text(model, MAX_FIELD_BYTES)?;
            }
        }
        if self.web.backends.len() > MAX_WEB_BACKENDS {
            return Err(invalid());
        }
        for (index, backend) in self.web.backends.iter().enumerate() {
            if !is_portable_id(&backend.id)
                || !matches!(backend.kind.as_str(), "querit" | "custom")
                || !is_https_url(&backend.endpoint)
                || self.web.backends[..index]
                    .iter()
                    .any(|b| b.id == backend.id)
            {
                return Err(invalid());
            }
        }
        if self.web.backends.iter().filter(|b| b.enabled).count() > 1 {
            return Err(invalid());
        }
        if self.mcp_servers.len() > MAX_MCP_SERVERS {
            return Err(invalid());
        }
        for (index, server) in self.mcp_servers.iter().enumerate() {
            if !is_portable_id(&server.id) || server.args.len() > 64 {
                return Err(invalid());
            }
            for arg in &server.args {
                bounded_text(arg, MAX_FIELD_BYTES)?;
            }
            match server.transport.as_str() {
                "stdio" => {
                    if server.command.is_none()
                        || server.endpoint.is_some()
                        || server.key_header.is_some()
                    {
                        return Err(invalid());
                    }
                    bounded_text(
                        server.command.as_deref().unwrap_or_default(),
                        MAX_FIELD_BYTES,
                    )?;
                }
                "http" => {
                    let endpoint_ok = server
                        .endpoint
                        .as_deref()
                        .map(is_https_url)
                        .unwrap_or(false);
                    if !endpoint_ok {
                        return Err(invalid());
                    }
                    if server.command.is_some()
                        || !server.args.is_empty()
                        || server
                            .key_header
                            .as_deref()
                            .is_some_and(|header| !matches!(header, "bearer" | "x-api-key"))
                    {
                        return Err(invalid());
                    }
                }
                _ => return Err(invalid()),
            }
            if self.mcp_servers[..index].iter().any(|s| s.id == server.id) {
                return Err(invalid());
            }
        }
        if self.appearance.theme != "light" && self.appearance.theme != "dark" {
            return Err(invalid());
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
        let mut bytes = serde_json::to_vec(&document)
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
    let document: DeserializedSettings =
        serde_json::from_slice(bytes).map_err(|_| ConfigError::authority_rejection())?;
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
            endpoint: None,
            key_header: None,
        };
        assert!(settings.validate().is_ok(), "valid stdio accepted");
        settings.mcp_servers[0].endpoint = Some("https://x.example.com".to_owned());
        assert!(settings.validate().is_err(), "endpoint on stdio rejected");
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

        settings.user_agent = "mcode-test/1".to_owned();
        let second = replace_app_settings(&layout, first, &settings).expect("second publish");
        assert_eq!(second.get(), 2);
        assert_eq!(
            read_app_settings(&layout).expect("reread").user_agent,
            "mcode-test/1"
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

        settings.web.backends[2].kind = "custom".to_owned();
        settings.web.backends[2].endpoint = "http://plain.example.com".to_owned();
        assert!(settings.validate().is_err(), "https only");
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
