//! Pure desktop view-model: state, actions, and reducer.
//!
//! This module has no GPUI dependency. The render layer turns
//! [`WorkspaceState`] into elements and feeds [`DesktopAction`]s back, so the
//! product behavior stays testable without a GPU or window. The transitions
//! themselves live in [`reduce`]; this file declares the state they act on.
use std::sync::Arc;

use mycode_app::PreparedUpdate;

mod reduce;
#[cfg(test)]
mod regressions;
#[cfg(test)]
mod tests;

pub use reduce::{
    reasoning_levels_for, reduce, selected_model_supports_reasoning, selected_reasoning_levels,
};

pub(crate) use reduce::close_floating_menus;

// The transcript vocabulary is the core's protocol, not a rendering concern:
// it is defined in `mycode-app` and re-exported here so render code keeps one
// import path.
pub use mycode_app::{
    ActiveConversation, CHAT_CANCELLED, ConversationEntry, EntryKind, SessionSummary,
    StreamingReply,
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

/// The mention trigger parsed from the composer draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MentionKind {
    /// `@` file reference against the session project.
    File,
    /// `/` command at the very start of the draft.
    Command,
}

/// Built-in slash commands offered by the composer menu.
const COMPOSER_COMMANDS: &[(&str, &str)] = &[("/new", "new chat"), ("/settings", "settings")];
/// Recent transcript blocks that stay mounted. Older ones fold.
const TRANSCRIPT_TAIL: usize = 24;
/// How many folded blocks one reveal click mounts.
const TRANSCRIPT_PAGE: usize = 24;

/// First visible display-block index for a folded transcript.
#[must_use]
pub fn transcript_start(block_count: usize, extra: usize) -> usize {
    block_count.saturating_sub(TRANSCRIPT_TAIL.saturating_add(extra))
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
    /// Draft answers aligned with [`Self::pending_ask`].
    pub ask_answers: Vec<String>,
    /// Durable task list rows: (content, status). Completed tasks are dropped.
    pub todo_rows: Vec<(String, String)>,
    /// Extra folded transcript blocks the user asked to mount above the tail.
    pub transcript_extra: usize,
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
    /// Whether the General page's shell-kind dropdown is open.
    pub shell_kind_menu_open: bool,
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
    /// The settings editor toggled a provider row's enabled flag.
    SettingsProviderToggled(usize, bool),
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
    /// The General-page shell-kind dropdown opened or closed.
    ShellKindMenuToggled(bool),
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
    /// The user picked one choice on a pending ask question.
    AskChoicePicked { index: usize, answer: String },
    /// The user submitted answers locally; clear the pending panel.
    AskAnswered,
    /// Mount another page of folded transcript blocks.
    TranscriptRevealMore,
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
    /// A remembered project was removed from the recent list.
    RecentRemoved(String),
    /// The model picker selected a provider.
    ProviderSelected(String),
    /// The model picker selected a model; an unknown model joins the
    /// provider row so the next turn can use it.
    ModelSelected(String),
    /// The model picker dropdown opened or closed.
    ModelMenuToggled(bool),
    /// The thinking-effort submenu opened or closed.
    ReasoningMenuToggled(bool),
    /// The composer's thinking-effort pick; persists through settings.
    SettingsReasoningChanged(String),
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

/// The effective thinking level: the stored pick while the catalog still
/// advertises it, otherwise `default`. One projection shared by the composer
/// chip and the reasoning menu so the two can never disagree.
#[must_use]
pub fn selected_reasoning_level(state: &WorkspaceState) -> &str {
    state
        .settings
        .as_ref()
        .and_then(|settings| settings.reasoning.as_deref())
        .filter(|level| {
            reduce::selected_reasoning_levels(state)
                .iter()
                .any(|item| item == level)
        })
        .unwrap_or("default")
}

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

/// Higher scores are stronger general-purpose models such as o3 and gpt-5.
#[must_use]
pub fn model_strength(id: &str) -> u32 {
    let id = id.to_ascii_lowercase();
    let mut score = 0u32;
    if id.contains("o3-pro") {
        score += 100;
    } else if id.contains("o3") {
        score += 90;
    }
    if id.contains("gpt-6") || id.contains("opus") {
        score += 85;
    }
    if id.contains("gpt-5") {
        score += 70;
    }
    if id.contains("o1") {
        score += 60;
    }
    if id.contains("sonnet") {
        score += 50;
    }
    if id.contains("codex") {
        score += 15;
    }
    if compact_model(&id) {
        score = score.saturating_sub(30);
    }
    score
}

fn compact_model(id: &str) -> bool {
    ["mini", "nano", "flash", "haiku", "spark"]
        .iter()
        .any(|tag| id.contains(tag))
}

/// Stable strongest-first order. Equal scores keep the original order.
#[must_use]
pub fn rank_model_ids(ids: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut indexed: Vec<(usize, String)> = ids.into_iter().enumerate().collect();
    indexed.sort_by(|left, right| {
        model_strength(&right.1)
            .cmp(&model_strength(&left.1))
            .then(left.0.cmp(&right.0))
    });
    indexed.into_iter().map(|(_, id)| id).collect()
}

/// The strongest non-compact models, capped for a suggestion row.
#[must_use]
pub fn suggested_model_ids(ids: impl IntoIterator<Item = String>) -> Vec<String> {
    rank_model_ids(ids)
        .into_iter()
        .filter(|id| model_strength(id) > 0 && !compact_model(&id.to_ascii_lowercase()))
        .take(8)
        .collect()
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
                None => grouped
                    .others
                    .push((project.to_owned(), vec![session.clone()])),
            },
            None => grouped.unbound.push(session.clone()),
        }
    }
    grouped
}
