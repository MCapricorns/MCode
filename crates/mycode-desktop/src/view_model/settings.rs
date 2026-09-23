//! The editable settings projection and the settings navigation vocabulary.

/// One slash-command skill shown in the settings Skills page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkillEntry {
    /// Command slug without the leading `/`.
    pub slug: String,
    /// One-line title from the skill heading.
    pub title: String,
    /// Absolute path of the skill markdown.
    pub path: String,
    /// Whether this file came from the user-global `.agents` tree.
    pub global: bool,
}

/// An in-flight OAuth device-flow sign-in shown in the settings UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopilotSignIn {
    /// Code the user types at the verification page.
    pub user_code: String,
    /// Verification page opened in the browser.
    pub verification_uri: String,
}

/// The editable settings projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsState {
    /// Revision the editor loaded; `0` when absent.
    pub revision: u64,
    /// Raw configured User-Agent (empty string means the pi default).
    pub user_agent: String,
    /// Effective User-Agent preview.
    pub effective_user_agent: String,
    /// Configured providers.
    pub providers: Vec<mycode_config::ProviderSettings>,
    /// Configured search backends.
    pub web_backends: Vec<mycode_config::WebBackendSettings>,
    /// MCP servers.
    pub mcp_servers: Vec<mycode_config::McpServerSettings>,
    /// Appearance theme: `light` or `dark`.
    pub theme: String,
    /// Appearance palette: slate, ocean, forest, dusk, sand, rose, ink, or moss.
    pub palette: String,
    /// Requested reasoning effort from the selected model's catalog options;
    /// `None` keeps the provider default.
    pub reasoning: Option<String>,
    /// Provider ids that have a stored API key.
    pub providers_with_keys: Vec<String>,
    /// MCP key ids (form `mcp-<server>`) that have a stored key.
    pub mcp_with_keys: Vec<String>,
    /// Whether durable usage records are written.
    pub usage_enabled: bool,
    /// Subagent role enablement, model routes, and thinking overrides.
    pub subagents: mycode_config::SubagentSettings,
    /// Tool-runtime preferences, including the platform shell.
    pub tools: mycode_config::ToolsSettings,
    /// A save is in flight.
    pub saving: bool,
    /// Unsaved local edits exist.
    pub dirty: bool,
}

impl SettingsState {
    /// Projects one settings document plus revision and stored key ids.
    #[must_use]
    pub fn from_settings(
        settings: &mycode_config::AppSettings,
        revision: u64,
        providers_with_keys: Vec<String>,
    ) -> Self {
        Self {
            revision,
            user_agent: settings.user_agent.clone(),
            effective_user_agent: settings.effective_user_agent(),
            providers: settings.providers.clone(),
            web_backends: merge_web_backends(&settings.web.backends),
            mcp_servers: settings.mcp_servers.clone(),
            theme: settings.appearance.theme.clone(),
            palette: settings.effective_palette().to_owned(),
            reasoning: settings.reasoning_effort.clone(),
            providers_with_keys,
            mcp_with_keys: Vec::new(),
            usage_enabled: settings.usage.enabled,
            subagents: settings.subagents.clone(),
            tools: settings.tools.clone(),
            saving: false,
            dirty: false,
        }
    }

    /// Builds the document the editor currently shows.
    #[must_use]
    pub fn to_settings(&self) -> mycode_config::AppSettings {
        mycode_config::AppSettings {
            user_agent: self.user_agent.clone(),
            providers: self.providers.clone(),
            web: mycode_config::WebSettings {
                backends: self.web_backends.clone(),
            },
            usage: mycode_config::UsageSettings {
                enabled: self.usage_enabled,
            },
            mcp_servers: self.mcp_servers.clone(),
            appearance: mycode_config::AppearanceSettings {
                theme: self.theme.clone(),
                palette: self.palette.clone(),
            },
            reasoning_effort: self.reasoning.clone(),
            subagents: self.subagents.clone(),
            tools: self.tools.clone(),
        }
    }
}

/// Built-in Querit / AnySearch rows always appear; user backends append.
fn merge_web_backends(
    configured: &[mycode_config::WebBackendSettings],
) -> Vec<mycode_config::WebBackendSettings> {
    let mut backends = mycode_config::builtin_web_backends();
    for backend in configured {
        if let Some(slot) = backends.iter_mut().find(|item| item.id == backend.id) {
            *slot = backend.clone();
        } else {
            backends.push(backend.clone());
        }
    }
    backends
}

/// The Models settings sub-page: provider list, catalog picker, or the
/// custom-endpoint form.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ModelsSubview {
    /// Configured providers plus the two add buttons.
    #[default]
    List,
    /// The models.dev catalog picker (search + provider rows).
    Catalog,
    /// The custom endpoint form.
    Custom,
}

/// The Web search settings sub-page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WebSubview {
    /// Vendor rows and custom backends, keys hidden behind a lock.
    #[default]
    List,
    /// The custom backend form.
    Custom,
}

/// The MCP settings sub-page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum McpSubview {
    /// Configured servers plus the add buttons.
    #[default]
    List,
    /// Built-in servers that are not configured yet.
    Catalog,
    /// Paste a Claude Desktop / Cursor mcp.json.
    Json,
    /// The custom stdio or HTTP server form.
    Custom,
}

/// One settings navigation section (the secondary menu).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsSection {
    /// Theme and request identity.
    #[default]
    General,
    /// Providers, catalog presets, and custom endpoints.
    Models,
    /// Subagent roles and per-role model routes.
    Agents,
    /// Slash-command skills from `.agents`.
    Skills,
    /// MCP servers.
    Mcp,
    /// Web search backends.
    Web,
    /// Usage records and data export/import.
    Data,
    /// Version, updates, and the provider catalog.
    About,
}

impl SettingsSection {
    /// Stable nav identifier.
    pub fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Models => "models",
            Self::Agents => "agents",
            Self::Skills => "skills",
            Self::Mcp => "mcp",
            Self::Web => "web",
            Self::Data => "data",
            Self::About => "about",
        }
    }

    /// Nav row label.
    pub fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Models => "Models",
            Self::Agents => "Agents",
            Self::Skills => "Skills",
            Self::Mcp => "MCP",
            Self::Web => "Web search",
            Self::Data => "Data",
            Self::About => "About",
        }
    }

    /// Nav row icon.
    pub fn icon(self) -> gpui_kit::assets::IconName {
        use gpui_kit::assets::IconName;
        match self {
            Self::General => IconName::SlidersHorizontal,
            Self::Models => IconName::Bot,
            Self::Agents => IconName::Sparkles,
            Self::Skills => IconName::Terminal,
            Self::Mcp => IconName::PlugZap,
            Self::Web => IconName::Globe,
            Self::Data => IconName::Database,
            Self::About => IconName::Info,
        }
    }

    /// One-line hint under the nav label.
    pub fn hint(self) -> &'static str {
        match self {
            Self::General => "Theme, identity, shell",
            Self::Models => "Providers, keys",
            Self::Agents => "Roles, models",
            Self::Skills => "Slash commands",
            Self::Mcp => "Tool servers",
            Self::Web => "Search backends",
            Self::Data => "Usage, export",
            Self::About => "Version, updates",
        }
    }

    /// Nav groups in display order with their member sections.
    pub const GROUPS: &'static [(&'static str, &'static [SettingsSection])] = &[
        (
            "Workspace",
            &[Self::General, Self::Models, Self::Agents, Self::Skills],
        ),
        ("Connect", &[Self::Mcp, Self::Web]),
        ("System", &[Self::Data, Self::About]),
    ];
}
