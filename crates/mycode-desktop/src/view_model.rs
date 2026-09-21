//! Pure desktop view-model: state, actions, and reducer.
//!
//! This module has no GPUI dependency. The render layer turns
//! [`WorkspaceState`] into elements and feeds [`DesktopAction`]s back, so the
//! product behavior stays testable without a GPU or window.
use std::sync::Arc;

use mycode_app::PreparedUpdate;

// The transcript vocabulary is the core's protocol, not a rendering concern:
// it is defined in `mycode-app` and re-exported here so render code keeps one
// import path.
pub use mycode_app::{
    ActiveConversation, CHAT_CANCELLED, ConversationEntry, EntryKind, MAX_STREAMING_CHARS,
    SessionSummary, StreamingReply,
};

/// Composer mention autocomplete: `@` files or `/` commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposerMention {
    /// Trigger kind active in the draft.
    pub kind: MentionKind,
    /// Typed text after the trigger character.
    pub fragment: String,
    /// (insert, display) rows, bounded.
    pub items: Vec<(String, String)>,
}

/// One in-flight or just-finished `task` subagent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveJob {
    /// Provider-assigned call id.
    pub call_id: String,
    /// Catalog role when known (`scout`, `artisan`, …).
    pub role: String,
    /// Human-facing brief from the parent turn.
    pub label: String,
    /// Latest nested tool or progress line.
    pub step: String,
    /// Whether the child has returned its answer.
    pub done: bool,
}

/// One slash-command skill shown in settings and the inspector.
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

/// The mention trigger parsed from the composer draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MentionKind {
    /// `@` file reference against the session project.
    File,
    /// `/` command at the very start of the draft.
    Command,
}

/// Built-in slash commands offered by the composer menu.
pub const COMPOSER_COMMANDS: &[(&str, &str)] = &[("/new", "new chat"), ("/settings", "settings")];

/// Parses the composer draft into an active mention, if any: a trailing
/// `@fragment` token selects files; a leading `/name` (still the whole
/// draft) selects commands.
fn parse_mention(text: &str) -> Option<ComposerMention> {
    if text.ends_with(char::is_whitespace) || text.is_empty() {
        return None;
    }
    let token = text.split_whitespace().last().unwrap_or_default();
    if let Some(fragment) = token.strip_prefix('@')
        && !fragment.contains('@')
    {
        return Some(ComposerMention {
            kind: MentionKind::File,
            fragment: fragment.to_owned(),
            items: Vec::new(),
        });
    }
    if let Some(fragment) = text.strip_prefix('/')
        && !fragment.contains(char::is_whitespace)
    {
        let items = COMPOSER_COMMANDS
            .iter()
            .filter(|(name, _)| name[1..].starts_with(fragment))
            .map(|(name, label)| ((*name).to_owned(), format!("{name} \u{b7} {label}")))
            .collect();
        return Some(ComposerMention {
            kind: MentionKind::Command,
            fragment: fragment.to_owned(),
            items,
        });
    }
    None
}

/// Cumulative token usage for one `provider/model` pair.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageTotal {
    /// `provider/model` spelling this row accounts for.
    pub key: String,
    /// Input (prompt) tokens summed across turns.
    pub input: u64,
    /// Output (completion) tokens summed across turns.
    pub output: u64,
    /// Prompt tokens served from the provider cache, summed across turns.
    pub cache: u64,
    /// Completed turns folded into this row.
    pub requests: u64,
}

/// Share of prompt tokens served from cache, as a whole percent.
///
/// Providers report cache reads as a subset of the input count, so the ratio
/// is only meaningful once input tokens exist.
#[must_use]
pub fn cache_percent(cache: u64, input: u64) -> Option<u64> {
    (input > 0 && cache > 0).then(|| (cache.min(input) * 100) / input)
}

/// Metrics for one completed model turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnStats {
    /// Model id the turn ran on.
    pub model: String,
    /// Input (prompt) tokens reported by the provider.
    pub input: u64,
    /// Output (completion) tokens reported by the provider.
    pub output: u64,
    /// Prompt tokens served from the provider cache, when reported.
    pub cache: Option<u64>,
    /// Wall-clock duration in milliseconds.
    pub elapsed_ms: u64,
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

/// The main area view: chat or full-page settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MainView {
    /// Conversation with the agent.
    #[default]
    Chat,
    /// Full-page visual settings.
    Settings,
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
            "WORKSPACE",
            &[Self::General, Self::Models, Self::Agents, Self::Skills],
        ),
        ("CONNECT", &[Self::Mcp, Self::Web]),
        ("SYSTEM", &[Self::Data, Self::About]),
    ];
}

/// Self-update progress shown in settings and banners.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UpdateState {
    /// No check has run yet.
    #[default]
    Idle,
    /// A check is in flight.
    Checking,
    /// The running version is the latest.
    UpToDate,
    /// A newer release is published.
    Available {
        /// New version (no `v` prefix).
        version: String,
        /// Release page URL.
        notes_url: String,
    },
    /// The update is downloading and verifying.
    Downloading {
        /// New version (no `v` prefix).
        version: String,
    },
    /// The staged update is ready; a restart installs it.
    Ready {
        /// New version (no `v` prefix).
        version: String,
    },
    /// The last check or download failed.
    Failed(String),
}

/// Whole-window state.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceState {
    /// Sidebar sessions, newest-relevant order preserved from the core.
    pub sessions: Vec<SessionSummary>,
    /// Open conversation, when any.
    pub active: Option<ActiveConversation>,
    /// Composer draft text.
    pub composer_draft: String,
    /// Follow-ups waiting for the in-flight turn to finish.
    pub queued: Vec<String>,
    /// A send is in flight.
    pub sending: bool,
    /// Per-server tool names from the last listing, keyed by server id.
    pub mcp_tools: Vec<(String, Vec<String>)>,
    /// MCP server ids with a tools probe in flight.
    pub mcp_probing: Vec<String>,
    /// Prompt resources for the open session: (name, path).
    pub resources: Vec<(String, String)>,
    /// Slash-command skills from the project and user `.agents` trees.
    pub skills: Vec<SkillEntry>,
    /// Live `task` subagent runs for the inspector panel.
    pub live_jobs: Vec<LiveJob>,
    /// Pending ask rows awaiting user answers.
    pub pending_ask: Option<Vec<(String, Vec<String>, bool)>>,
    /// Durable task list rows: (content, status).
    pub todo_rows: Vec<(String, String)>,
    /// Cumulative usage per provider/model, in first-seen order.
    pub usage_totals: Vec<UsageTotal>,
    /// Most recent turn's timing and token metrics, when usage is enabled.
    pub last_turn: Option<TurnStats>,
    /// Active composer mention autocomplete, when a trigger is typed.
    pub mention: Option<ComposerMention>,
    /// In-flight Copilot device-flow sign-in, when any.
    pub copilot_sign_in: Option<CopilotSignIn>,
    /// The last Copilot sign-in failure, shown in the sign-in panel.
    pub copilot_error: Option<String>,
    /// The editable settings projection.
    pub settings: Option<SettingsState>,
    /// True when the window uses the dark theme.
    pub dark_theme: bool,
    /// Last terminal error surfaced to the user.
    pub error: Option<String>,
    /// Chat or settings main view.
    pub view: MainView,
    /// Settings navigation section.
    pub settings_section: SettingsSection,
    /// Whether the sidebar project switcher dropdown is open.
    pub project_menu_open: bool,
    /// The resolved provider catalog.
    pub catalog: Option<Arc<mycode_providers::catalog::CatalogDocument>>,
    /// Unix seconds of the catalog's last successful cloud fetch.
    pub catalog_fetched_at: u64,
    /// Project directory bound to the open session.
    pub project_dir: Option<String>,
    /// Recent project directories, most recent first.
    pub recents: Vec<String>,
    /// Session-to-project bindings (session id, project path), most recent
    /// first; drives the project-grouped sidebar.
    pub session_projects: Vec<(String, String)>,
    /// Whether update checks run automatically.
    pub auto_update: bool,
    /// Self-update progress.
    pub update: UpdateState,
    /// The staged update waiting for a restart, when any.
    pub prepared_update: Option<PreparedUpdate>,
    /// The newest release offer, when one is available.
    pub last_offer: Option<mycode_app::UpdateOffer>,
    /// Selected provider id in the model picker.
    pub selected_provider: Option<String>,
    /// Selected model id for the selected provider.
    pub selected_model: Option<String>,
    /// Whether the model picker dropdown is open.
    pub model_menu_open: bool,
    /// Whether the thinking-effort submenu is open.
    pub reasoning_menu_open: bool,
    /// Open Agents-page dropdown: (role name, field) where field is
    /// `provider`, `model`, or `thinking`.
    pub subagent_menu: Option<(String, String)>,
    /// Filter text for the provider preset picker.
    pub preset_search: String,
    /// The catalog provider currently being added, when any.
    pub active_preset: Option<String>,
    /// Models checked in the active preset form; empty means the catalog's
    /// first model is used as the sole default.
    pub preset_models: Vec<String>,
    /// Whether the preset form's model dropdown is open.
    pub preset_model_menu_open: bool,
    /// The Models settings sub-page.
    pub models_subview: ModelsSubview,
    /// Whether the custom provider form's protocol dropdown is open.
    pub provider_kind_menu_open: bool,
    /// Whether the custom MCP form's transport dropdown is open.
    pub mcp_transport_menu_open: bool,
}
/// Everything the UI can do to the state.
#[derive(Clone, Debug, PartialEq)]
pub enum DesktopAction {
    /// The core returned the session list.
    SessionsLoaded(Vec<SessionSummary>),
    /// A session was created and opened.
    SessionCreated(SessionSummary),
    /// A session finished recovery and its conversation is ready.
    ConversationOpened(ActiveConversation),
    /// The open session's data was deleted; drop the conversation.
    SessionDeleted,
    /// The composer text changed.
    ComposerChanged(String),
    /// The composer sent; the entry was durably committed.
    MessageSent {
        /// New head spelling.
        head: String,
        /// The committed entry.
        entry: ConversationEntry,
    },
    /// A request failed.
    Failed(String),
    /// Settings loaded from the core.
    SettingsLoaded(SettingsState),
    /// The user picked the light or dark theme; persists with settings.
    SettingsThemeSelected(bool),
    /// The user dismissed the composer mention menu without picking a row.
    MentionDismissed,
    /// The settings editor changed the User-Agent.
    SettingsUserAgentChanged(String),
    /// The settings editor changed a provider row.
    SettingsProviderChanged(usize, mycode_config::ProviderSettings),
    /// The settings editor added a provider row.
    SettingsProviderAdded(mycode_config::ProviderSettings),
    /// The settings editor removed a provider row.
    SettingsProviderRemoved(usize),
    /// The settings editor added a web backend row.
    SettingsBackendAdded(mycode_config::WebBackendSettings),
    /// The settings editor removed a web backend row.
    SettingsBackendRemoved(usize),
    /// The settings editor toggled a web backend.
    SettingsBackendToggled(usize, bool),
    /// The settings editor changed subagent role routes.
    SettingsSubagentsChanged(mycode_config::SubagentSettings),
    /// The settings editor changed the platform shell.
    SettingsToolsChanged(mycode_config::ToolsSettings),
    /// The Agents-page provider/model/thinking dropdown opened or closed.
    SubagentMenuToggled(Option<(String, String)>),
    /// Unbound sessions inherit the active project (repairs the missing bind).
    UnboundSessionsAssigned(String),
    /// The settings editor toggled durable usage records.
    SettingsUsageToggled(bool),
    /// The settings editor added an MCP server.
    SettingsMcpAdded(mycode_config::McpServerSettings),
    /// The settings editor removed an MCP server.
    SettingsMcpRemoved(usize),
    /// The settings editor toggled an MCP server.
    SettingsMcpToggled(usize, bool),
    /// A tools probe started for one MCP server (the row shows a spinner
    /// state until the listing or a failure arrives).
    McpProbeStarted(String),
    /// A server's tools listing arrived.
    McpToolsListed {
        server_id: String,
        tools: Vec<String>,
    },
    /// A server's tools probe failed; the row shows the reason inline.
    McpProbeFailed { server_id: String, message: String },
    /// A tool call started on the open conversation.
    ToolStarted { call_id: String, name: String },
    /// Incremental tool or subagent progress for the live status line.
    ToolProgress {
        call_id: String,
        name: String,
        message: String,
    },
    /// A committed tool-result entry arrived.
    ToolResultAppended(ConversationEntry),
    /// Prompt resources discovered for the open session.
    ResourcesLoaded(Vec<(String, String)>),
    /// Discovered slash-command skills for the current project.
    SkillsLoaded(Vec<SkillEntry>),
    /// File matches for the active `@` mention arrived from the bridge.
    MentionFiles(Vec<String>),
    /// The Copilot device flow started; show the user code.
    CopilotSignInStarted(CopilotSignIn),
    /// The Copilot device flow finished; `Err` keeps the panel with a message.
    CopilotSignInFinished(Result<(), String>),
    /// A durable usage record arrived.
    UsageRecorded {
        provider: String,
        model: String,
        input: u64,
        output: u64,
        cache: Option<u64>,
        elapsed_ms: u64,
        entry: ConversationEntry,
    },
    /// The durable task list changed.
    TodoUpdated(Vec<(String, String)>),
    /// The agent asked the user structured questions.
    AskRequested(Vec<(String, Vec<String>, bool)>),
    /// The user submitted answers locally; clear the pending panel.
    AskAnswered,
    /// Settings were persisted under CAS; carries the new revision.
    SettingsSaved(u64),
    /// One provider's API key was stored or cleared; refreshes key markers.
    ProviderKeySaved {
        /// Provider ids with a stored key.
        provider_keys: Vec<String>,
        /// MCP server ids with a stored key.
        mcp_keys: Vec<String>,
    },
    /// The user queued a follow-up while a turn is in flight.
    MessageQueued(String),
    /// The user dismissed one queued follow-up.
    QueuedMessageRemoved(usize),
    /// The next queued follow-up was taken to send.
    QueuedMessageTaken,
    /// The user sent a prompt; show a working status before the first token.
    TurnArmed,
    /// Incremental assistant text from the active model turn.
    ChatDelta(String),
    /// Incremental assistant reasoning from the active model turn.
    ChatThinkingDelta(String),
    /// An intermediate assistant step (with tool calls) was committed; the
    /// streamed text so far belongs to it, so the live bubble resets.
    AssistantStepCommitted(ConversationEntry),
    /// The model turn finished and its entry was committed.
    ChatDone {
        head: String,
        entry: ConversationEntry,
    },
    /// The model turn failed without committing anything.
    ChatFailed(String),
    /// Clear the surfaced error.
    DismissError,
    /// Switch the main area between chat and settings.
    ShowMainView(MainView),
    /// Switch the settings secondary menu.
    ShowSettingsSection(SettingsSection),
    /// The sidebar project switcher opened or closed.
    ProjectMenuToggled(bool),
    /// The provider catalog resolved (bundled or cloud).
    CatalogLoaded {
        /// The catalog document.
        document: Arc<mycode_providers::catalog::CatalogDocument>,
        /// Unix seconds of the last cloud fetch.
        fetched_at: u64,
    },
    /// The durable UI state loaded.
    UiStateLoaded {
        /// Recent project directories.
        recents: Vec<String>,
        /// Last opened project directory.
        last_project: Option<String>,
        /// Whether update checks run automatically.
        auto_update: bool,
        /// Last selected provider id.
        selected_provider: Option<String>,
        /// Last selected model id.
        selected_model: Option<String>,
        /// Session-to-project bindings.
        session_projects: Vec<(String, String)>,
    },
    /// A project directory was bound to the open session.
    ProjectOpened(String),
    /// A session was bound to a project in the durable map.
    SessionProjectBound {
        /// Session identity spelling.
        session_id: String,
        /// Project directory path.
        project: String,
    },
    /// The active project filter changed (sidebar project switcher).
    ActiveProjectChanged(Option<String>),
    /// The model picker selected a provider.
    ProviderSelected(String),
    /// The model picker selected a model.
    ModelSelected(String),
    /// The model picker dropdown opened or closed.
    ModelMenuToggled(bool),
    /// The thinking-effort submenu opened or closed.
    ReasoningMenuToggled(bool),
    /// The preset picker filter changed.
    PresetSearchChanged(String),
    /// A preset form opened or closed.
    ActivePresetChanged(Option<String>),
    /// The active preset form toggled one model's checkbox.
    PresetModelToggled(String),
    /// The preset form's model dropdown opened or closed.
    PresetModelMenuToggled(bool),
    /// The Models settings sub-page changed.
    ShowModelsSubview(ModelsSubview),
    /// The custom provider form's protocol dropdown opened or closed.
    ProviderKindMenuToggled(bool),
    /// The custom MCP form's transport dropdown opened or closed.
    McpTransportMenuToggled(bool),
    /// Self-update progress changed.
    UpdateStateChanged(UpdateState),
    /// A release offer was resolved for the available update.
    UpdateOfferFound(mycode_app::UpdateOffer),
    /// The auto-update preference changed.
    AutoUpdateToggled(bool),
    /// A verified update is staged and waiting for a restart.
    UpdateStaged(PreparedUpdate),
}

/// Maximum composer text before the send is rejected locally.
pub const MAX_COMPOSER_CHARS: usize = 64 * 1024;
/// Follow-ups waiting behind one in-flight turn.
pub const MAX_QUEUED_MESSAGES: usize = 8;

/// Sidebar grouping of sessions relative to the active project filter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupedSessions {
    /// Sessions bound to the active project.
    pub current: Vec<SessionSummary>,
    /// Sessions with no project binding.
    pub unbound: Vec<SessionSummary>,
    /// Sessions bound to some other project, grouped by that path.
    pub others: Vec<(String, Vec<SessionSummary>)>,
}

/// Compare project paths the way the sidebar groups them.
#[must_use]
pub fn same_project_path(left: &str, right: &str) -> bool {
    normalize_project_key(left) == normalize_project_key(right)
}

fn normalize_project_key(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches(['/', '\\']);
    if cfg!(windows) {
        trimmed.replace('/', "\\").to_ascii_lowercase()
    } else {
        trimmed.to_owned()
    }
}

/// Groups sessions for the project-centric sidebar.
#[must_use]
pub fn group_sessions(
    sessions: &[SessionSummary],
    bindings: &[(String, String)],
    active_project: Option<&str>,
) -> GroupedSessions {
    let mut grouped = GroupedSessions::default();
    for session in sessions {
        let project = bindings
            .iter()
            .find(|(id, _)| id == &session.session_id)
            .map(|(_, project)| project.as_str());
        match project {
            Some(project)
                if active_project.is_some_and(|active| same_project_path(active, project)) =>
            {
                grouped.current.push(session.clone());
            }
            Some(project) => match grouped
                .others
                .iter_mut()
                .find(|(key, _)| same_project_path(key, project))
            {
                Some((_, rows)) => rows.push(session.clone()),
                None => grouped.others.push((project.to_owned(), vec![session.clone()])),
            },
            None => grouped.unbound.push(session.clone()),
        }
    }
    grouped
}

fn assign_unbound_sessions(state: &mut WorkspaceState, project: &str) {
    let bound: std::collections::HashSet<String> = state
        .session_projects
        .iter()
        .map(|(id, _)| id.clone())
        .collect();
    for session in &state.sessions {
        if bound.contains(&session.session_id) {
            continue;
        }
        state
            .session_projects
            .insert(0, (session.session_id.clone(), project.to_owned()));
    }
    if let Some(active) = state.active.as_ref()
        && !state
            .session_projects
            .iter()
            .any(|(id, _)| id == &active.session_id)
    {
        state
            .session_projects
            .insert(0, (active.session_id.clone(), project.to_owned()));
    }
    state
        .session_projects
        .truncate(mycode_config::MAX_SESSION_PROJECTS);
}

/// Applies one action to the state.
pub fn reduce(state: &mut WorkspaceState, action: DesktopAction) {
    let touches_providers = matches!(
        action,
        DesktopAction::SettingsLoaded(_)
            | DesktopAction::SettingsProviderAdded(_)
            | DesktopAction::SettingsProviderRemoved(_)
            | DesktopAction::SettingsSaved(_)
            | DesktopAction::UiStateLoaded { .. }
    );
    match action {
        DesktopAction::SessionsLoaded(mut sessions) => {
            let active_id = state.active.as_ref().map(|c| c.session_id.as_str());
            for session in &mut sessions {
                session.active = active_id == Some(session.session_id.as_str());
            }
            state.sessions = sessions;
        }
        DesktopAction::SessionCreated(mut summary) => {
            state
                .sessions
                .retain(|s| s.session_id != summary.session_id);
            summary.active = true;
            state.sessions.insert(0, summary.clone());
            state.active = Some(ActiveConversation {
                session_id: summary.session_id,
                branch_id: summary.root_branch_id,
                head: "empty".to_owned(),
                entries: Vec::new(),
                streaming: None,
            });
            state.live_jobs.clear();
        }
        DesktopAction::SessionDeleted => {
            state.active = None;
            state.sending = false;
            state.queued.clear();
            state.live_jobs.clear();
            state.pending_ask = None;
            state.error = None;
        }
        DesktopAction::ConversationOpened(conversation) => {
            let switched = state
                .active
                .as_ref()
                .map(|active| active.session_id.as_str())
                != Some(conversation.session_id.as_str());
            if switched {
                // Queue and sending belong to the previous session's turn.
                state.queued.clear();
                state.live_jobs.clear();
                state.sending = false;
            }
            let session_id = conversation.session_id.clone();
            state.active = Some(conversation);
            for session in &mut state.sessions {
                session.active = session.session_id == session_id;
            }
        }
        DesktopAction::ComposerChanged(text) => {
            let bounded: String = text.chars().take(MAX_COMPOSER_CHARS).collect();
            state.mention = parse_mention(&bounded);
            state.composer_draft = bounded;
        }
        DesktopAction::MentionFiles(files) => {
            if let Some(mention) = state.mention.as_mut()
                && mention.kind == MentionKind::File
            {
                mention.items = files
                    .into_iter()
                    .map(|path| {
                        let display = path.clone();
                        (path, display)
                    })
                    .collect();
            }
        }
        DesktopAction::CopilotSignInStarted(info) => {
            state.copilot_sign_in = Some(info);
            state.copilot_error = None;
        }
        DesktopAction::CopilotSignInFinished(outcome) => {
            state.copilot_sign_in = None;
            state.copilot_error = outcome.err();
        }
        DesktopAction::MessageQueued(text) => {
            let bounded: String = text.chars().take(MAX_COMPOSER_CHARS).collect();
            if !bounded.trim().is_empty() && state.queued.len() < MAX_QUEUED_MESSAGES {
                state.queued.push(bounded);
            }
            state.composer_draft.clear();
            state.mention = None;
        }
        DesktopAction::QueuedMessageRemoved(index) => {
            if index < state.queued.len() {
                state.queued.remove(index);
            }
        }
        DesktopAction::QueuedMessageTaken => {
            if !state.queued.is_empty() {
                state.queued.remove(0);
            }
        }
        DesktopAction::MessageSent { head, entry } => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.head = head;
                conversation.entries.push(entry);
            }
            state.composer_draft.clear();
        }
        DesktopAction::TurnArmed => {
            state.sending = true;
            state.error = None;
            set_streaming_status(state, "Waiting for the model");
        }
        DesktopAction::ChatDelta(delta) => {
            append_streaming(state, false, delta);
        }
        DesktopAction::ToolStarted { call_id, name } => {
            set_streaming_status(state, &format!("Running {name}"));
            if name == "task" {
                upsert_live_job(state, &call_id, "", "starting", false);
            }
            if let Some(conversation) = state.active.as_mut() {
                conversation.entries.push(ConversationEntry {
                    event_id: format!("call-{call_id}"),
                    kind: EntryKind::ToolCall,
                    text: name.into(),
                    call_id: Some(call_id),
                    thinking: String::new(),
                });
            }
        }
        DesktopAction::ToolProgress {
            call_id,
            name,
            message,
        } => {
            let status = if name.is_empty() {
                message.clone()
            } else {
                format!("{name}: {message}")
            };
            set_streaming_status(state, &status);
            apply_live_job_progress(state, &call_id, &name, &message);
        }
        DesktopAction::ToolResultAppended(entry) => {
            if let Some(call_id) = entry.call_id.as_deref() {
                finish_live_job(state, call_id);
            }
            if let Some(conversation) = state.active.as_mut() {
                conversation.entries.push(entry);
            }
        }
        DesktopAction::ResourcesLoaded(files) => state.resources = files,
        DesktopAction::SkillsLoaded(skills) => state.skills = skills,
        DesktopAction::UsageRecorded {
            provider,
            model,
            input,
            output,
            cache,
            elapsed_ms,
            entry,
        } => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.entries.push(entry);
            }
            state.last_turn = Some(TurnStats {
                model: model.clone(),
                input,
                output,
                cache,
                elapsed_ms,
            });
            let key = format!("{provider}/{model}");
            let row = match state.usage_totals.iter_mut().find(|row| row.key == key) {
                Some(row) => row,
                None => {
                    state.usage_totals.push(UsageTotal {
                        key,
                        ..UsageTotal::default()
                    });
                    state.usage_totals.last_mut().expect("just pushed")
                }
            };
            row.input = row.input.saturating_add(input);
            row.output = row.output.saturating_add(output);
            row.cache = row.cache.saturating_add(cache.unwrap_or_default());
            row.requests = row.requests.saturating_add(1);
        }
        DesktopAction::TodoUpdated(tasks) => state.todo_rows = tasks,
        DesktopAction::AskRequested(rows) => state.pending_ask = Some(rows),
        DesktopAction::AskAnswered => state.pending_ask = None,
        DesktopAction::ChatThinkingDelta(delta) => {
            append_streaming(state, true, delta);
        }
        DesktopAction::AssistantStepCommitted(entry) => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.entries.push(entry);
            }
            set_streaming_status(state, "Waiting for the next step");
            if let Some(conversation) = state.active.as_mut()
                && let Some(streaming) = conversation.streaming.as_mut()
            {
                streaming.text.clear();
                streaming.thinking.clear();
            }
        }
        DesktopAction::ChatDone { head, entry } => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.head = head;
                // A turn that ended on a committed tool step reports that
                // step again as its last message; it is already listed.
                let already_listed = conversation
                    .entries
                    .iter()
                    .any(|existing| existing.event_id == entry.event_id);
                if !already_listed {
                    conversation.entries.push(entry);
                }
                conversation.streaming = None;
            }
            state.live_jobs.clear();
            state.sending = false;
        }
        DesktopAction::ChatFailed(message) => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.streaming = None;
            }
            // A user-initiated cancel resets the turn without an error
            // banner; the sentinel travels as the failure message.
            if message != CHAT_CANCELLED {
                state.error = Some(message);
            }
            state.live_jobs.clear();
            state.sending = false;
        }
        DesktopAction::Failed(message) => {
            state.error = Some(message);
            state.sending = false;
        }
        DesktopAction::SettingsLoaded(settings) => {
            let dark = settings.theme != "light";
            state.settings = Some(settings);
            state.dark_theme = dark;
        }
        DesktopAction::SettingsThemeSelected(dark) => {
            if let Some(settings) = state.settings.as_mut() {
                settings.theme = if dark { "dark" } else { "light" }.to_owned();
                settings.dirty = true;
            }
            state.dark_theme = dark;
        }
        DesktopAction::MentionDismissed => state.mention = None,
        DesktopAction::SettingsUserAgentChanged(user_agent) => {
            if let Some(settings) = state.settings.as_mut() {
                settings.user_agent = user_agent;
                settings.dirty = true;
            }
        }
        DesktopAction::SettingsProviderChanged(index, provider) => {
            if let Some(settings) = state.settings.as_mut()
                && settings.providers.get_mut(index).is_some()
            {
                settings.providers[index] = provider;
                settings.dirty = true;
            }
        }
        DesktopAction::SettingsProviderAdded(provider) => {
            if let Some(settings) = state.settings.as_mut()
                && settings.providers.len() < mycode_config::MAX_PROVIDERS
            {
                settings.providers.push(provider);
                settings.dirty = true;
            }
        }
        DesktopAction::SettingsProviderRemoved(index) => {
            if let Some(settings) = state.settings.as_mut()
                && index < settings.providers.len()
            {
                settings.providers.remove(index);
                settings.dirty = true;
            }
        }
        DesktopAction::SettingsBackendAdded(backend) => {
            if let Some(settings) = state.settings.as_mut()
                && settings.web_backends.len() < mycode_config::MAX_WEB_BACKENDS
            {
                settings.web_backends.push(backend);
                settings.dirty = true;
            }
        }
        DesktopAction::SettingsBackendRemoved(index) => {
            if let Some(settings) = state.settings.as_mut()
                && index < settings.web_backends.len()
            {
                settings.web_backends.remove(index);
                settings.dirty = true;
            }
        }
        DesktopAction::SettingsUsageToggled(enabled) => {
            if let Some(settings) = state.settings.as_mut() {
                settings.usage_enabled = enabled;
                settings.dirty = true;
            }
        }
        DesktopAction::SettingsBackendToggled(index, enabled) => {
            if let Some(settings) = state.settings.as_mut()
                && index < settings.web_backends.len()
            {
                if enabled {
                    for (slot, backend) in settings.web_backends.iter_mut().enumerate() {
                        backend.enabled = slot == index;
                    }
                } else {
                    settings.web_backends[index].enabled = false;
                }
                settings.dirty = true;
            }
        }
        DesktopAction::SettingsSubagentsChanged(subagents) => {
            if let Some(settings) = state.settings.as_mut() {
                settings.subagents = subagents;
                settings.dirty = true;
            }
        }
        DesktopAction::SettingsToolsChanged(tools) => {
            if let Some(settings) = state.settings.as_mut() {
                settings.tools = tools;
                settings.dirty = true;
            }
        }
        DesktopAction::SubagentMenuToggled(menu) => state.subagent_menu = menu,
        DesktopAction::UnboundSessionsAssigned(project) => {
            assign_unbound_sessions(state, &project);
        }
        DesktopAction::SettingsSaved(revision) => {
            if let Some(settings) = state.settings.as_mut() {
                settings.revision = revision;
                settings.saving = false;
                settings.dirty = false;
                settings.effective_user_agent = settings.to_settings().effective_user_agent();
            }
        }
        DesktopAction::ProviderKeySaved {
            provider_keys,
            mcp_keys,
        } => {
            if let Some(settings) = state.settings.as_mut() {
                settings.providers_with_keys = provider_keys;
                settings.mcp_with_keys = mcp_keys;
            }
        }
        DesktopAction::SettingsMcpAdded(server) => {
            if let Some(settings) = state.settings.as_mut()
                && settings.mcp_servers.len() < mycode_config::MAX_MCP_SERVERS
            {
                settings.mcp_servers.push(server);
                settings.dirty = true;
            }
        }
        DesktopAction::SettingsMcpRemoved(index) => {
            if let Some(settings) = state.settings.as_mut()
                && index < settings.mcp_servers.len()
            {
                let removed = settings.mcp_servers.remove(index);
                settings.dirty = true;
                // A stale listing for a deleted row must not resurface if a
                // server with the same id is added again later.
                state.mcp_tools.retain(|(id, _)| *id != removed.id);
                state.mcp_probing.retain(|id| *id != removed.id);
            }
        }
        DesktopAction::SettingsMcpToggled(index, enabled) => {
            if let Some(settings) = state.settings.as_mut()
                && let Some(server) = settings.mcp_servers.get_mut(index)
            {
                server.enabled = enabled;
                settings.dirty = true;
            }
        }
        DesktopAction::McpProbeStarted(server_id) => {
            if !state.mcp_probing.contains(&server_id) {
                state.mcp_probing.push(server_id);
            }
        }
        DesktopAction::McpToolsListed { server_id, tools } => {
            state.mcp_probing.retain(|id| *id != server_id);
            if let Some(entry) = state.mcp_tools.iter_mut().find(|(id, _)| *id == server_id) {
                entry.1 = tools;
            } else {
                state.mcp_tools.push((server_id, tools));
            }
        }
        DesktopAction::McpProbeFailed { server_id, message } => {
            state.mcp_probing.retain(|id| *id != server_id);
            state.mcp_tools.retain(|(id, _)| *id != server_id);
            state.error = Some(message);
        }
        DesktopAction::DismissError => state.error = None,
        DesktopAction::ShowMainView(view) => {
            state.view = view;
            // Crossing views closes every floating menu so no stale layer
            // renders above the destination view.
            state.project_menu_open = false;
            state.model_menu_open = false;
            state.reasoning_menu_open = false;
            state.preset_model_menu_open = false;
            state.mention = None;
        }
        DesktopAction::ShowSettingsSection(section) => state.settings_section = section,
        DesktopAction::ProjectMenuToggled(open) => state.project_menu_open = open,
        DesktopAction::CatalogLoaded {
            document,
            fetched_at,
        } => {
            state.catalog = Some(document);
            state.catalog_fetched_at = fetched_at;
        }
        DesktopAction::UiStateLoaded {
            recents,
            last_project,
            auto_update,
            selected_provider,
            selected_model,
            session_projects,
        } => {
            state.recents = recents;
            state.project_dir = last_project.filter(|path| !path.trim().is_empty());
            state.session_projects = session_projects;
            state.auto_update = auto_update;
            if selected_provider.is_some() {
                state.selected_provider = selected_provider;
                state.selected_model = selected_model;
            }
            ensure_model_selection(state);
        }
        DesktopAction::ProjectOpened(project) => {
            state.project_dir = Some(project.clone());
            state.recents.retain(|existing| existing != &project);
            state.recents.insert(0, project);
            state.recents.truncate(mycode_config::MAX_RECENT_PROJECTS);
        }
        DesktopAction::SessionProjectBound {
            session_id,
            project,
        } => {
            state
                .session_projects
                .retain(|(existing, _)| existing != &session_id);
            state
                .session_projects
                .insert(0, (session_id, project.clone()));
            state
                .session_projects
                .truncate(mycode_config::MAX_SESSION_PROJECTS);
        }
        DesktopAction::ActiveProjectChanged(project) => {
            state.project_dir = project;
        }
        DesktopAction::ProviderSelected(provider) => {
            state.selected_provider = Some(provider.clone());
            state.selected_model = state
                .catalog
                .as_ref()
                .and_then(|catalog| catalog.provider(&provider))
                .and_then(|provider| provider.models.first())
                .map(|model| model.id.clone())
                .or_else(|| {
                    state
                        .settings
                        .as_ref()
                        .and_then(|settings| settings.providers.iter().find(|p| p.id == provider))
                        .and_then(|provider| provider.models.first().cloned())
                });
            state.model_menu_open = false;
            if !selected_model_supports_reasoning(state) {
                state.reasoning_menu_open = false;
            }
            clamp_reasoning_to_catalog(state);
        }
        DesktopAction::ModelSelected(model) => {
            state.selected_model = Some(model);
            state.model_menu_open = false;
            if !selected_model_supports_reasoning(state) {
                state.reasoning_menu_open = false;
            }
            clamp_reasoning_to_catalog(state);
        }
        DesktopAction::ModelMenuToggled(open) => {
            state.model_menu_open = open;
            if open {
                state.reasoning_menu_open = false;
            }
        }
        DesktopAction::ReasoningMenuToggled(open) => {
            state.reasoning_menu_open = open;
            if open {
                state.model_menu_open = false;
            }
        }
        DesktopAction::PresetSearchChanged(text) => state.preset_search = text,
        DesktopAction::ActivePresetChanged(preset) => {
            state.active_preset = preset.clone();
            state.preset_model_menu_open = false;
            // Opening a provider pre-checks its whole model list (capped by
            // the settings limit) so every advertised model starts selected
            // instead of a single default.
            state.preset_models = preset
                .as_ref()
                .and_then(|id| {
                    state
                        .catalog
                        .as_ref()
                        .and_then(|catalog| catalog.provider(id))
                })
                .map(|provider| {
                    provider
                        .models
                        .iter()
                        .map(|model| model.id.clone())
                        .take(mycode_config::MAX_MODELS_PER_PROVIDER)
                        .collect()
                })
                .unwrap_or_default();
        }
        DesktopAction::PresetModelToggled(model) => {
            if let Some(position) = state.preset_models.iter().position(|m| *m == model) {
                state.preset_models.remove(position);
            } else {
                state.preset_models.push(model);
            }
        }
        DesktopAction::PresetModelMenuToggled(open) => state.preset_model_menu_open = open,
        DesktopAction::ShowModelsSubview(view) => {
            state.models_subview = view;
            state.active_preset = None;
            state.preset_model_menu_open = false;
            state.provider_kind_menu_open = false;
            state.mcp_transport_menu_open = false;
        }
        DesktopAction::ProviderKindMenuToggled(open) => state.provider_kind_menu_open = open,
        DesktopAction::McpTransportMenuToggled(open) => state.mcp_transport_menu_open = open,
        DesktopAction::UpdateStateChanged(update) => state.update = update,
        DesktopAction::UpdateOfferFound(offer) => state.last_offer = Some(offer),
        DesktopAction::AutoUpdateToggled(auto_update) => state.auto_update = auto_update,
        DesktopAction::UpdateStaged(prepared) => {
            state.prepared_update = Some(prepared);
            state.update = UpdateState::Ready {
                version: mycode_app::current_version().to_owned(),
            };
        }
    }
    if touches_providers {
        ensure_model_selection(state);
    }
}

/// Keeps the model picker on a provider/model that actually exists.
fn ensure_model_selection(state: &mut WorkspaceState) {
    let Some(settings) = state.settings.as_ref() else {
        return;
    };
    let selected_valid = state
        .selected_provider
        .as_ref()
        .and_then(|provider_id| {
            settings
                .providers
                .iter()
                .find(|provider| &provider.id == provider_id)
        })
        .is_some_and(|provider| {
            provider.enabled
                && state
                    .selected_model
                    .as_ref()
                    .is_some_and(|model| provider.models.contains(model))
        });
    if selected_valid {
        return;
    }
    let fallback = settings
        .providers
        .iter()
        .find(|provider| provider.enabled && !provider.models.is_empty());
    state.selected_provider = fallback.map(|provider| provider.id.clone());
    state.selected_model = fallback.and_then(|provider| provider.models.first().cloned());
}

/// Whether the selected catalog model advertises reasoning.
///
/// Unknown catalog rows keep the control visible so custom providers are
/// not locked out; the menu itself is built from `reasoning_options`.
#[must_use]
pub fn selected_model_supports_reasoning(state: &WorkspaceState) -> bool {
    let Some(catalog) = state.catalog.as_ref() else {
        return true;
    };
    let Some(provider) = state
        .selected_provider
        .as_deref()
        .and_then(|id| catalog.provider(id))
    else {
        return true;
    };
    match state.selected_model.as_deref() {
        Some(model_id) => provider
            .models
            .iter()
            .find(|model| model.id == model_id)
            .map(|model| model.reasoning)
            .unwrap_or(true),
        None => provider.models.iter().any(|model| model.reasoning),
    }
}

/// Thinking choices advertised for one catalog model.
///
/// Missing catalog rows yield only `default` so the UI never invents
/// low/medium/high.
#[must_use]
pub fn reasoning_levels_for(
    state: &WorkspaceState,
    provider_id: Option<&str>,
    model_id: Option<&str>,
) -> Vec<String> {
    let Some(catalog) = state.catalog.as_ref() else {
        return vec!["default".to_owned()];
    };
    let (Some(provider_id), Some(model_id)) = (provider_id, model_id) else {
        return vec!["default".to_owned()];
    };
    match catalog.model(provider_id, model_id) {
        Some(model) => model.reasoning_levels(),
        None => vec!["default".to_owned()],
    }
}

/// Thinking choices for the composer chip's selected model.
#[must_use]
pub fn selected_reasoning_levels(state: &WorkspaceState) -> Vec<String> {
    reasoning_levels_for(
        state,
        state.selected_provider.as_deref(),
        state.selected_model.as_deref(),
    )
}

fn clamp_reasoning_to_catalog(state: &mut WorkspaceState) {
    let levels = selected_reasoning_levels(state);
    let Some(settings) = state.settings.as_mut() else {
        return;
    };
    let Some(current) = settings.reasoning.as_deref() else {
        return;
    };
    if !levels.iter().any(|level| level == current) {
        settings.reasoning = None;
        settings.dirty = true;
    }
}

fn parse_task_progress(message: &str) -> Option<(&str, &str, &str)> {
    let mut parts = message.splitn(4, '|');
    if parts.next()? != "task" {
        return None;
    }
    Some((parts.next()?, parts.next()?, parts.next().unwrap_or("")))
}

fn upsert_live_job(state: &mut WorkspaceState, call_id: &str, role: &str, step: &str, done: bool) {
    if let Some(job) = state
        .live_jobs
        .iter_mut()
        .find(|job| !call_id.is_empty() && job.call_id == call_id)
    {
        if !role.is_empty() {
            job.role = role.to_owned();
        }
        if !step.is_empty() {
            job.step = step.to_owned();
        }
        job.done = done;
        return;
    }
    if let Some(job) = state.live_jobs.iter_mut().rev().find(|job| !job.done)
        && (call_id.is_empty() || job.call_id.is_empty())
    {
        if !call_id.is_empty() {
            job.call_id = call_id.to_owned();
        }
        if !role.is_empty() {
            job.role = role.to_owned();
        }
        if !step.is_empty() {
            job.step = step.to_owned();
        }
        job.done = done;
        return;
    }
    state.live_jobs.push(LiveJob {
        call_id: call_id.to_owned(),
        role: role.to_owned(),
        label: String::new(),
        step: if step.is_empty() {
            "starting".to_owned()
        } else {
            step.to_owned()
        },
        done,
    });
}

fn apply_live_job_progress(state: &mut WorkspaceState, call_id: &str, name: &str, message: &str) {
    if name != "task" && !message.starts_with("task|") {
        return;
    }
    if let Some((role, phase, detail)) = parse_task_progress(message) {
        match phase {
            "queued" => {
                upsert_live_job(state, call_id, role, "queued", false);
                if let Some(job) = live_job_mut(state, call_id) {
                    job.label = detail.to_owned();
                }
            }
            "done" => upsert_live_job(state, call_id, role, "done", true),
            "tool" | "step" => upsert_live_job(state, call_id, role, detail, false),
            other => upsert_live_job(state, call_id, role, other, false),
        }
        return;
    }
    upsert_live_job(state, call_id, "", message, false);
}

fn live_job_mut<'a>(state: &'a mut WorkspaceState, call_id: &str) -> Option<&'a mut LiveJob> {
    if call_id.is_empty() {
        return state.live_jobs.iter_mut().rev().find(|job| !job.done);
    }
    state
        .live_jobs
        .iter_mut()
        .find(|job| job.call_id == call_id)
}

fn finish_live_job(state: &mut WorkspaceState, call_id: &str) {
    if let Some(job) = live_job_mut(state, call_id) {
        job.done = true;
        if job.step.is_empty() || job.step == "starting" || job.step == "queued" {
            job.step = "done".to_owned();
        }
    }
}

/// Ensures a live streaming bubble exists and updates its status line.
fn set_streaming_status(state: &mut WorkspaceState, status: &str) {
    if let Some(conversation) = state.active.as_mut() {
        let streaming = conversation
            .streaming
            .get_or_insert_with(StreamingReply::default);
        streaming.status = status.to_owned();
    }
}

/// Buffers one streaming fragment into the active conversation.
fn append_streaming(state: &mut WorkspaceState, thinking: bool, delta: String) {
    if delta.is_empty() {
        return;
    }
    if let Some(conversation) = state.active.as_mut() {
        let streaming = conversation
            .streaming
            .get_or_insert_with(StreamingReply::default);
        streaming.status = if thinking {
            "Thinking".to_owned()
        } else {
            "Writing".to_owned()
        };
        let buffer = if thinking {
            &mut streaming.thinking
        } else {
            &mut streaming.text
        };
        if buffer.chars().count() < MAX_STREAMING_CHARS {
            for unit in delta.chars() {
                if buffer.chars().count() >= MAX_STREAMING_CHARS {
                    break;
                }
                buffer.push(unit);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: &str, count: u64) -> SessionSummary {
        SessionSummary {
            session_id: id.to_owned(),
            root_branch_id: format!("br-{id}"),
            title: format!("chat {id}"),
            event_count: count,
            active: false,
        }
    }

    #[test]
    fn session_list_marks_the_open_session() {
        let mut state = WorkspaceState::default();
        reduce(
            &mut state,
            DesktopAction::SessionsLoaded(vec![summary("ses1-a", 1), summary("ses1-b", 2)]),
        );
        assert!(!state.sessions[0].active);

        reduce(
            &mut state,
            DesktopAction::SessionCreated(summary("ses1-a", 0)),
        );
        assert!(state.sessions[0].active);
        assert_eq!(state.sessions.len(), 2);
        assert_eq!(
            state.active.as_ref().map(|c| c.head.as_str()),
            Some("empty")
        );

        reduce(
            &mut state,
            DesktopAction::SessionsLoaded(vec![summary("ses1-a", 1), summary("ses1-b", 2)]),
        );
        assert!(state.sessions[0].active, "reload keeps the open mark");
        assert!(!state.sessions[1].active);
    }

    #[test]
    fn copilot_sign_in_lifecycle_shows_code_then_error_or_clear() {
        let mut state = WorkspaceState::default();
        reduce(
            &mut state,
            DesktopAction::CopilotSignInStarted(CopilotSignIn {
                user_code: "AB12-CD34".to_owned(),
                verification_uri: "https://github.com/login/device".to_owned(),
            }),
        );
        assert_eq!(
            state
                .copilot_sign_in
                .as_ref()
                .map(|sign_in| sign_in.user_code.as_str()),
            Some("AB12-CD34")
        );
        assert!(state.copilot_error.is_none());

        reduce(
            &mut state,
            DesktopAction::CopilotSignInFinished(
                Err("the request was denied on GitHub".to_owned()),
            ),
        );
        assert!(state.copilot_sign_in.is_none());
        assert_eq!(
            state.copilot_error.as_deref(),
            Some("the request was denied on GitHub")
        );

        reduce(
            &mut state,
            DesktopAction::CopilotSignInStarted(CopilotSignIn {
                user_code: "ZZ99".to_owned(),
                verification_uri: "https://github.com/login/device".to_owned(),
            }),
        );
        reduce(&mut state, DesktopAction::CopilotSignInFinished(Ok(())));
        assert!(state.copilot_sign_in.is_none());
        assert!(state.copilot_error.is_none(), "success clears the error");
    }

    #[test]
    fn message_send_appends_and_clears_the_draft() {
        let mut state = WorkspaceState::default();
        reduce(
            &mut state,
            DesktopAction::SessionCreated(summary("ses1-a", 0)),
        );
        reduce(
            &mut state,
            DesktopAction::ComposerChanged("hello".to_owned()),
        );
        reduce(
            &mut state,
            DesktopAction::MessageSent {
                head: "evt1-1".to_owned(),
                entry: ConversationEntry {
                    event_id: "evt1-1".to_owned(),
                    kind: EntryKind::UserMessage,
                    text: "hello".into(),
                    call_id: None,
                    thinking: String::new(),
                },
            },
        );
        let conversation = state.active.as_ref().expect("open");
        assert_eq!(conversation.head, "evt1-1");
        assert_eq!(conversation.entries.len(), 1);
        assert!(state.composer_draft.is_empty());
        assert!(!state.sending);
    }

    #[test]
    fn composer_is_bounded_by_characters() {
        let mut state = WorkspaceState::default();
        reduce(
            &mut state,
            DesktopAction::ComposerChanged("你好".repeat(40_000)),
        );
        assert_eq!(
            state.composer_draft.chars().count(),
            MAX_COMPOSER_CHARS,
            "CJK input is truncated by chars, not bytes"
        );
    }

    #[test]
    fn settings_round_trip_marks_dirty_and_saves() {
        let mut state = WorkspaceState::default();
        let settings =
            SettingsState::from_settings(&mycode_config::AppSettings::default(), 0, Vec::new());
        assert!(settings.effective_user_agent.starts_with("pi ("));
        reduce(&mut state, DesktopAction::SettingsLoaded(settings));

        reduce(
            &mut state,
            DesktopAction::SettingsUserAgentChanged("ua-x".to_owned()),
        );
        let settings = state.settings.as_ref().expect("settings");
        assert!(settings.dirty);
        assert_eq!(settings.user_agent, "ua-x");

        reduce(
            &mut state,
            DesktopAction::SettingsProviderAdded(mycode_config::ProviderSettings {
                id: "openai-main".to_owned(),
                kind: "openai-completions".to_owned(),
                base_url: "https://api.openai.com/v1".to_owned(),
                models: vec!["gpt-x".to_owned()],
                enabled: true,
                context_limit: None,
                max_output: None,
            }),
        );
        reduce(&mut state, DesktopAction::SettingsProviderRemoved(0));
        assert!(
            state
                .settings
                .as_ref()
                .expect("settings")
                .providers
                .is_empty()
        );

        reduce(&mut state, DesktopAction::SettingsSaved(3));
        let settings = state.settings.expect("settings");
        assert_eq!(settings.revision, 3);
        assert!(!settings.dirty);
        assert!(!settings.saving);
    }

    #[test]
    fn key_save_reply_updates_markers_without_touching_rows() {
        let mut state = WorkspaceState::default();
        let settings =
            SettingsState::from_settings(&mycode_config::AppSettings::default(), 0, Vec::new());
        reduce(&mut state, DesktopAction::SettingsLoaded(settings));

        // A freshly added provider sits dirty in the editor while its key
        // save is in flight; the reply must not wipe the row.
        reduce(
            &mut state,
            DesktopAction::SettingsProviderAdded(mycode_config::ProviderSettings {
                id: "openai-main".to_owned(),
                kind: "openai-completions".to_owned(),
                base_url: "https://api.openai.com/v1".to_owned(),
                models: vec!["gpt-x".to_owned()],
                enabled: true,
                context_limit: None,
                max_output: None,
            }),
        );
        reduce(
            &mut state,
            DesktopAction::ProviderKeySaved {
                provider_keys: vec!["openai-main".to_owned()],
                mcp_keys: vec!["fs".to_owned()],
            },
        );
        let settings = state.settings.as_ref().expect("settings");
        assert_eq!(settings.providers.len(), 1, "row survives the key reply");
        assert!(
            settings.dirty,
            "dirty edits are not clobbered by the key reply"
        );
        assert_eq!(settings.providers_with_keys, vec!["openai-main"]);
        assert_eq!(settings.mcp_with_keys, vec!["fs"]);
    }

    #[test]
    fn failure_and_tabs_round_trip() {
        let mut state = WorkspaceState::default();
        reduce(&mut state, DesktopAction::Failed("boom".to_owned()));
        assert_eq!(state.error.as_deref(), Some("boom"));
        reduce(&mut state, DesktopAction::DismissError);
        assert_eq!(state.error, None);

        reduce(&mut state, DesktopAction::ShowMainView(MainView::Settings));
        assert_eq!(state.view, MainView::Settings);
        reduce(&mut state, DesktopAction::ShowMainView(MainView::Chat));
        assert_eq!(state.view, MainView::Chat);
    }

    #[test]
    fn theme_selection_updates_settings_doc_and_dark_flag() {
        let mut state = WorkspaceState::default();
        let settings =
            SettingsState::from_settings(&mycode_config::AppSettings::default(), 0, Vec::new());
        reduce(&mut state, DesktopAction::SettingsLoaded(settings));
        assert!(state.dark_theme, "default settings spell a dark theme");

        reduce(&mut state, DesktopAction::SettingsThemeSelected(false));
        let settings = state.settings.as_ref().expect("settings");
        assert!(!state.dark_theme);
        assert_eq!(settings.theme, "light");
        assert!(settings.dirty, "the choice persists through a save");

        reduce(&mut state, DesktopAction::SettingsThemeSelected(true));
        let settings = state.settings.as_ref().expect("settings");
        assert!(state.dark_theme);
        assert_eq!(settings.theme, "dark");
    }

    #[test]
    fn switching_views_closes_floating_menus() {
        let mut state = WorkspaceState {
            project_menu_open: true,
            model_menu_open: true,
            reasoning_menu_open: true,
            preset_model_menu_open: true,
            mention: Some(ComposerMention {
                kind: MentionKind::File,
                fragment: "src".to_owned(),
                items: Vec::new(),
            }),
            ..WorkspaceState::default()
        };
        reduce(&mut state, DesktopAction::ShowMainView(MainView::Settings));
        assert!(!state.project_menu_open);
        assert!(!state.model_menu_open);
        assert!(!state.reasoning_menu_open);
        assert!(!state.preset_model_menu_open);
        assert!(state.mention.is_none());

        // Dismissal also clears a mention on its own.
        state.mention = Some(ComposerMention {
            kind: MentionKind::Command,
            fragment: "new".to_owned(),
            items: Vec::new(),
        });
        reduce(&mut state, DesktopAction::MentionDismissed);
        assert!(state.mention.is_none());
    }

    #[test]
    fn reasoning_and_model_menus_are_exclusive() {
        let mut state = WorkspaceState::default();
        reduce(&mut state, DesktopAction::ModelMenuToggled(true));
        assert!(state.model_menu_open);
        reduce(&mut state, DesktopAction::ReasoningMenuToggled(true));
        assert!(state.reasoning_menu_open);
        assert!(!state.model_menu_open);
        reduce(&mut state, DesktopAction::ModelMenuToggled(true));
        assert!(state.model_menu_open);
        assert!(!state.reasoning_menu_open);
    }

    #[test]
    fn catalog_reasoning_flag_gates_the_thinking_control() {
        let mut state = WorkspaceState {
            selected_provider: Some("acme".to_owned()),
            selected_model: Some("plain".to_owned()),
            reasoning_menu_open: true,
            ..WorkspaceState::default()
        };
        let mut document = mycode_providers::catalog::CatalogDocument::default();
        document
            .providers
            .push(mycode_providers::catalog::CatalogProvider {
                id: "acme".to_owned(),
                name: "Acme".to_owned(),
                kind: "openai-completions".to_owned(),
                base_url: "https://api.acme.dev/v1".to_owned(),
                doc: None,
                auth: String::new(),
                models: vec![
                    mycode_providers::catalog::CatalogModel {
                        id: "plain".to_owned(),
                        reasoning: false,
                        ..mycode_providers::catalog::CatalogModel::default()
                    },
                    mycode_providers::catalog::CatalogModel {
                        id: "thinker".to_owned(),
                        reasoning: true,
                        ..mycode_providers::catalog::CatalogModel::default()
                    },
                ],
            });
        state.catalog = Some(std::sync::Arc::new(document));
        assert!(!selected_model_supports_reasoning(&state));
        reduce(
            &mut state,
            DesktopAction::ModelSelected("thinker".to_owned()),
        );
        assert!(selected_model_supports_reasoning(&state));
        reduce(&mut state, DesktopAction::ReasoningMenuToggled(true));
        reduce(&mut state, DesktopAction::ModelSelected("plain".to_owned()));
        assert!(!selected_model_supports_reasoning(&state));
        assert!(!state.reasoning_menu_open);
    }

    #[test]
    fn reasoning_menu_follows_catalog_options() {
        let mut state = WorkspaceState {
            selected_provider: Some("acme".to_owned()),
            selected_model: Some("toggle".to_owned()),
            ..WorkspaceState::default()
        };
        let mut document = mycode_providers::catalog::CatalogDocument::default();
        document
            .providers
            .push(mycode_providers::catalog::CatalogProvider {
                id: "acme".to_owned(),
                name: "Acme".to_owned(),
                kind: "openai-completions".to_owned(),
                base_url: "https://api.acme.dev/v1".to_owned(),
                doc: None,
                auth: String::new(),
                models: vec![
                    mycode_providers::catalog::CatalogModel {
                        id: "toggle".to_owned(),
                        reasoning: true,
                        reasoning_toggle: true,
                        ..mycode_providers::catalog::CatalogModel::default()
                    },
                    mycode_providers::catalog::CatalogModel {
                        id: "efforts".to_owned(),
                        reasoning: true,
                        reasoning_efforts: vec!["low".to_owned(), "high".to_owned()],
                        ..mycode_providers::catalog::CatalogModel::default()
                    },
                ],
            });
        state.catalog = Some(std::sync::Arc::new(document));
        assert_eq!(
            selected_reasoning_levels(&state),
            vec!["default", "off", "on"]
        );
        reduce(
            &mut state,
            DesktopAction::ModelSelected("efforts".to_owned()),
        );
        assert_eq!(
            selected_reasoning_levels(&state),
            vec!["default", "low", "high"]
        );
    }

    #[test]
    fn task_progress_fills_the_subagent_panel() {
        let mut state = opened_conversation();
        reduce(
            &mut state,
            DesktopAction::ToolStarted {
                call_id: "call-task".to_owned(),
                name: "task".to_owned(),
            },
        );
        reduce(
            &mut state,
            DesktopAction::ToolProgress {
                call_id: "call-task".to_owned(),
                name: String::new(),
                message: "task|scout|queued|audit the parser".to_owned(),
            },
        );
        reduce(
            &mut state,
            DesktopAction::ToolProgress {
                call_id: "call-task".to_owned(),
                name: String::new(),
                message: "task|scout|tool|read".to_owned(),
            },
        );
        assert_eq!(state.live_jobs.len(), 1);
        assert_eq!(state.live_jobs[0].role, "scout");
        assert_eq!(state.live_jobs[0].label, "audit the parser");
        assert_eq!(state.live_jobs[0].step, "read");
        assert!(!state.live_jobs[0].done);
        reduce(
            &mut state,
            DesktopAction::ToolResultAppended(ConversationEntry {
                event_id: "evt-1".to_owned(),
                kind: EntryKind::ToolResult,
                text: "done".into(),
                call_id: Some("call-task".to_owned()),
                thinking: String::new(),
            }),
        );
        assert!(state.live_jobs[0].done);
        reduce(
            &mut state,
            DesktopAction::ChatDone {
                head: "head-2".to_owned(),
                entry: ConversationEntry {
                    event_id: "evt-2".to_owned(),
                    kind: EntryKind::AssistantMessage,
                    text: "ok".into(),
                    call_id: None,
                    thinking: String::new(),
                },
            },
        );
        assert!(state.live_jobs.is_empty());
    }

    #[test]
    fn at_trigger_indexes_cwd_even_with_an_empty_fragment() {
        let mut state = WorkspaceState::default();
        reduce(&mut state, DesktopAction::ComposerChanged("@".to_owned()));
        let mention = state.mention.expect("mention");
        assert_eq!(mention.kind, MentionKind::File);
        assert!(mention.fragment.is_empty());
    }

    #[test]
    fn opening_a_preset_pre_checks_its_model_list() {
        let mut state = WorkspaceState::default();
        let mut document = mycode_providers::catalog::CatalogDocument::default();
        document
            .providers
            .push(mycode_providers::catalog::CatalogProvider {
                id: "acme".to_owned(),
                name: "Acme".to_owned(),
                kind: "openai-completions".to_owned(),
                base_url: "https://api.acme.dev/v1".to_owned(),
                doc: None,
                auth: String::new(),
                models: ["m1", "m2", "m3", "m4"]
                    .iter()
                    .map(|id| mycode_providers::catalog::CatalogModel {
                        id: (*id).to_owned(),
                        ..mycode_providers::catalog::CatalogModel::default()
                    })
                    .collect(),
            });
        state.catalog = Some(std::sync::Arc::new(document));

        reduce(
            &mut state,
            DesktopAction::ActivePresetChanged(Some("acme".to_owned())),
        );
        assert_eq!(
            state.preset_models,
            vec!["m1", "m2", "m3", "m4"],
            "every advertised model starts checked"
        );

        reduce(
            &mut state,
            DesktopAction::PresetModelToggled("m2".to_owned()),
        );
        assert_eq!(state.preset_models.len(), 3);

        // Reopening resets the selection back to the full list.
        reduce(
            &mut state,
            DesktopAction::ActivePresetChanged(Some("acme".to_owned())),
        );
        assert_eq!(state.preset_models.len(), 4);

        // An unknown provider id leaves the form empty rather than stale.
        reduce(
            &mut state,
            DesktopAction::ActivePresetChanged(Some("missing".to_owned())),
        );
        assert!(state.preset_models.is_empty());
    }

    #[test]
    fn models_subview_switches_reset_transient_form_state() {
        use crate::view_model::ModelsSubview;

        let mut state = WorkspaceState {
            active_preset: Some("openai".to_owned()),
            preset_model_menu_open: true,
            provider_kind_menu_open: true,
            mcp_transport_menu_open: true,
            ..WorkspaceState::default()
        };
        reduce(
            &mut state,
            DesktopAction::ShowModelsSubview(ModelsSubview::Catalog),
        );
        assert_eq!(state.models_subview, ModelsSubview::Catalog);
        assert!(state.active_preset.is_none());
        assert!(!state.preset_model_menu_open);
        assert!(!state.provider_kind_menu_open);
        assert!(!state.mcp_transport_menu_open);

        reduce(
            &mut state,
            DesktopAction::ShowModelsSubview(ModelsSubview::Custom),
        );
        assert_eq!(state.models_subview, ModelsSubview::Custom);

        reduce(&mut state, DesktopAction::ProviderKindMenuToggled(true));
        assert!(state.provider_kind_menu_open);
        reduce(&mut state, DesktopAction::McpTransportMenuToggled(true));
        assert!(state.mcp_transport_menu_open);
    }

    #[test]
    fn model_selection_falls_back_to_an_enabled_provider() {
        let mut state = WorkspaceState::default();
        let mut settings =
            SettingsState::from_settings(&mycode_config::AppSettings::default(), 0, Vec::new());
        settings.providers.push(mycode_config::ProviderSettings {
            id: "acme".to_owned(),
            kind: "openai-completions".to_owned(),
            base_url: "https://api.acme.dev/v1".to_owned(),
            models: vec!["m1".to_owned(), "m2".to_owned()],
            enabled: true,
            context_limit: None,
            max_output: None,
        });
        reduce(&mut state, DesktopAction::SettingsLoaded(settings));
        assert_eq!(state.selected_provider.as_deref(), Some("acme"));
        assert_eq!(state.selected_model.as_deref(), Some("m1"));

        reduce(&mut state, DesktopAction::ModelSelected("m2".to_owned()));
        assert_eq!(state.selected_model.as_deref(), Some("m2"));

        // Removing the provider clears the selection back to the fallback.
        reduce(&mut state, DesktopAction::SettingsProviderRemoved(0));
        assert_eq!(state.selected_provider, None);
        assert_eq!(state.selected_model, None);
    }

    fn opened_conversation() -> WorkspaceState {
        WorkspaceState {
            active: Some(ActiveConversation {
                session_id: "ses1-a".to_owned(),
                branch_id: "br1-a".to_owned(),
                head: "empty".to_owned(),
                entries: Vec::new(),
                streaming: None,
            }),
            sending: true,
            ..WorkspaceState::default()
        }
    }
    #[test]
    fn turn_armed_shows_working_status_before_the_first_token() {
        let mut state = opened_conversation();
        state.sending = false;
        reduce(&mut state, DesktopAction::TurnArmed);
        assert!(state.sending);
        let streaming = state
            .active
            .as_ref()
            .expect("conversation")
            .streaming
            .as_ref()
            .expect("streaming");
        assert_eq!(streaming.status, "Waiting for the model");
        assert!(streaming.text.is_empty());
        assert!(streaming.thinking.is_empty());
    }

    #[test]
    fn chat_stream_buffers_then_commits() {
        let mut state = opened_conversation();
        reduce(&mut state, DesktopAction::ChatDelta("hel".to_owned()));
        reduce(&mut state, DesktopAction::ChatDelta("lo".to_owned()));
        reduce(
            &mut state,
            DesktopAction::ChatThinkingDelta("why".to_owned()),
        );
        let conversation = state.active.as_ref().expect("conversation");
        assert_eq!(
            conversation.streaming.as_ref().expect("streaming").text,
            "hello"
        );
        assert_eq!(
            conversation.streaming.as_ref().expect("streaming").thinking,
            "why"
        );
        assert_eq!(
            conversation.streaming.as_ref().expect("streaming").status,
            "Thinking"
        );
        assert!(state.sending, "sending until the turn ends");

        reduce(
            &mut state,
            DesktopAction::ChatDone {
                head: "evt1-x".to_owned(),
                entry: ConversationEntry {
                    event_id: "evt1-x".to_owned(),
                    kind: EntryKind::AssistantMessage,
                    text: "hello".into(),
                    call_id: None,
                    thinking: "why".into(),
                },
            },
        );
        let conversation = state.active.as_ref().expect("conversation");
        assert_eq!(conversation.head, "evt1-x");
        assert!(conversation.streaming.is_none());
        assert_eq!(conversation.entries.len(), 1);
        assert_eq!(conversation.entries[0].thinking, "why");
        assert!(!state.sending);
    }

    #[test]
    fn chat_failure_clears_streaming_and_flags_error() {
        let mut state = opened_conversation();
        reduce(&mut state, DesktopAction::ChatDelta("partial".to_owned()));
        reduce(
            &mut state,
            DesktopAction::ChatFailed("provider down".to_owned()),
        );
        assert!(
            state
                .active
                .as_ref()
                .expect("conversation")
                .streaming
                .is_none()
        );
        assert_eq!(state.error.as_deref(), Some("provider down"));
        assert!(!state.sending);
    }

    #[test]
    fn chat_cancel_resets_the_turn_without_an_error_banner() {
        let mut state = opened_conversation();
        reduce(&mut state, DesktopAction::ChatDelta("partial".to_owned()));
        reduce(
            &mut state,
            DesktopAction::ChatFailed(CHAT_CANCELLED.to_owned()),
        );
        assert!(
            state
                .active
                .as_ref()
                .expect("conversation")
                .streaming
                .is_none()
        );
        assert_eq!(state.error, None, "a cancel is not a failure");
        assert!(!state.sending);
    }

    #[test]
    fn chat_deltas_are_dropped_without_a_conversation() {
        let mut state = WorkspaceState::default();
        reduce(&mut state, DesktopAction::ChatDelta("ignored".to_owned()));
        assert!(state.active.is_none());
        assert!(!state.sending);
    }

    #[test]
    fn follow_ups_queue_while_a_turn_is_in_flight() {
        let mut state = opened_conversation();
        reduce(
            &mut state,
            DesktopAction::MessageQueued("then check tests".to_owned()),
        );
        reduce(
            &mut state,
            DesktopAction::MessageQueued("then commit".to_owned()),
        );
        assert_eq!(
            state.queued,
            vec!["then check tests".to_owned(), "then commit".to_owned()]
        );
        assert!(state.composer_draft.is_empty());
        reduce(&mut state, DesktopAction::QueuedMessageTaken);
        assert_eq!(state.queued, vec!["then commit".to_owned()]);
        reduce(&mut state, DesktopAction::QueuedMessageRemoved(0));
        assert!(state.queued.is_empty());
    }

    #[test]
    fn switching_sessions_drops_the_follow_up_queue() {
        let mut state = opened_conversation();
        state.queued.push("later".to_owned());
        reduce(
            &mut state,
            DesktopAction::ConversationOpened(ActiveConversation {
                session_id: "ses2-b".to_owned(),
                branch_id: "br2-b".to_owned(),
                head: "empty".to_owned(),
                entries: Vec::new(),
                streaming: None,
            }),
        );
        assert!(state.queued.is_empty());
        assert!(!state.sending);
    }

    #[test]
    fn skills_panel_keeps_discovered_slash_commands() {
        let mut state = WorkspaceState::default();
        reduce(
            &mut state,
            DesktopAction::SkillsLoaded(vec![SkillEntry {
                slug: "review".to_owned(),
                title: "Review".to_owned(),
                path: "/tmp/review/SKILL.md".to_owned(),
                global: true,
            }]),
        );
        assert_eq!(state.skills.len(), 1);
        assert_eq!(state.skills[0].slug, "review");
        assert!(state.skills[0].global);
    }

    #[test]
    fn a_full_queue_rejects_another_follow_up() {
        let mut state = opened_conversation();
        for index in 0..MAX_QUEUED_MESSAGES {
            reduce(
                &mut state,
                DesktopAction::MessageQueued(format!("item-{index}")),
            );
        }
        reduce(
            &mut state,
            DesktopAction::MessageQueued("overflow".to_owned()),
        );
        assert_eq!(state.queued.len(), MAX_QUEUED_MESSAGES);
        assert!(!state.queued.iter().any(|item| item == "overflow"));
    }

    #[test]
    fn session_project_bound_is_what_groups_this_project() {
        let mut state = WorkspaceState::default();
        state.sessions = vec![summary("ses-a", 0), summary("ses-b", 0)];
        reduce(
            &mut state,
            DesktopAction::ProjectOpened(r"D:\my_private_pro\MCode".to_owned()),
        );
        let grouped = group_sessions(
            &state.sessions,
            &state.session_projects,
            state.project_dir.as_deref(),
        );
        assert!(grouped.current.is_empty(), "ProjectOpened does not bind");
        assert_eq!(grouped.unbound.len(), 2);

        reduce(
            &mut state,
            DesktopAction::SessionProjectBound {
                session_id: "ses-a".to_owned(),
                project: r"D:\my_private_pro\MCode".to_owned(),
            },
        );
        let grouped = group_sessions(
            &state.sessions,
            &state.session_projects,
            Some(r"D:\my_private_pro\Mcode"),
        );
        assert_eq!(grouped.current.len(), 1);
        assert_eq!(grouped.current[0].session_id, "ses-a");
        assert_eq!(grouped.unbound.len(), 1);
    }

    #[test]
    fn unbound_sessions_are_assigned_to_the_active_project() {
        let mut state = WorkspaceState::default();
        state.sessions = vec![summary("ses-a", 0), summary("ses-b", 0)];
        reduce(
            &mut state,
            DesktopAction::UnboundSessionsAssigned(r"D:\proj".to_owned()),
        );
        let grouped = group_sessions(&state.sessions, &state.session_projects, Some(r"D:\proj"));
        assert_eq!(grouped.current.len(), 2);
        assert!(grouped.unbound.is_empty());
    }

    #[test]
    fn paired_subagent_route_stays_valid() {
        let mut settings = SettingsState::from_settings(&mycode_config::AppSettings::default(), 0, Vec::new());
        settings.providers.push(mycode_config::ProviderSettings {
            id: "openai-main".to_owned(),
            kind: "openai-completions".to_owned(),
            base_url: "https://api.openai.com/v1".to_owned(),
            models: vec!["gpt-4.1".to_owned()],
            enabled: true,
            context_limit: None,
            max_output: None,
        });
        settings.subagents.role_mut("scout").provider = Some("openai-main".to_owned());
        settings.subagents.role_mut("scout").model = Some("gpt-4.1".to_owned());
        assert!(settings.to_settings().validate().is_ok());
        settings.subagents.role_mut("scout").model = None;
        assert!(settings.to_settings().validate().is_err());
    }
}
