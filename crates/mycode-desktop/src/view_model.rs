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
        && !fragment.is_empty()
        && !fragment.contains('@')
    {
        return Some(ComposerMention {
            kind: MentionKind::File,
            fragment: fragment.to_owned(),
            items: Vec::new(),
        });
    }
    if let Some(fragment) = text.strip_prefix('/')
        && !fragment.is_empty()
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
    /// Requested reasoning effort: `low`, `medium`, `high`; `None` keeps the
    /// provider default.
    pub reasoning: Option<String>,
    /// Provider ids that have a stored API key.
    pub providers_with_keys: Vec<String>,
    /// MCP key ids (form `mcp-<server>`) that have a stored key.
    pub mcp_with_keys: Vec<String>,
    /// Whether durable usage records are written.
    pub usage_enabled: bool,
    /// Subagent role enablement, model routes, and thinking overrides.
    pub subagents: mycode_config::SubagentSettings,
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
            Self::Mcp => IconName::PlugZap,
            Self::Web => IconName::Globe,
            Self::Data => IconName::Database,
            Self::About => IconName::Info,
        }
    }

    /// One-line hint under the nav label.
    pub fn hint(self) -> &'static str {
        match self {
            Self::General => "Theme, identity",
            Self::Models => "Providers, keys",
            Self::Agents => "Roles, models",
            Self::Mcp => "Tool servers",
            Self::Web => "Search backends",
            Self::Data => "Usage, export",
            Self::About => "Version, updates",
        }
    }

    /// Nav groups in display order with their member sections.
    pub const GROUPS: &'static [(&'static str, &'static [SettingsSection])] = &[
        ("WORKSPACE", &[Self::General, Self::Models, Self::Agents]),
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
    /// A send is in flight.
    pub sending: bool,
    /// Per-server tool names from the last listing, keyed by server id.
    pub mcp_tools: Vec<(String, Vec<String>)>,
    /// MCP server ids with a tools probe in flight.
    pub mcp_probing: Vec<String>,
    /// Prompt resources for the open session: (name, path).
    pub resources: Vec<(String, String)>,
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
    /// A committed tool-result entry arrived.
    ToolResultAppended(ConversationEntry),
    /// Prompt resources discovered for the open session.
    ResourcesLoaded(Vec<(String, String)>),
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
        }
        DesktopAction::SessionDeleted => {
            state.active = None;
            state.sending = false;
            state.pending_ask = None;
            state.error = None;
        }
        DesktopAction::ConversationOpened(conversation) => {
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
        DesktopAction::MessageSent { head, entry } => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.head = head;
                conversation.entries.push(entry);
            }
            state.composer_draft.clear();
        }
        DesktopAction::ChatDelta(delta) => {
            append_streaming(state, false, delta);
        }
        DesktopAction::ToolStarted { call_id, name } => {
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
        DesktopAction::ToolResultAppended(entry) => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.entries.push(entry);
            }
        }
        DesktopAction::ResourcesLoaded(files) => state.resources = files,
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
                conversation.streaming = None;
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
            // The last project stays in the recents list only: a fresh start
            // with no session open must not claim a project (the composer
            // shows "Set folder" until the user picks one).
            let _ = last_project;
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
        }
        DesktopAction::ModelSelected(model) => {
            state.selected_model = Some(model);
            state.model_menu_open = false;
        }
        DesktopAction::ModelMenuToggled(open) => state.model_menu_open = open,
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

/// Buffers one streaming fragment into the active conversation.
fn append_streaming(state: &mut WorkspaceState, thinking: bool, delta: String) {
    if delta.is_empty() {
        return;
    }
    if let Some(conversation) = state.active.as_mut() {
        let streaming = conversation
            .streaming
            .get_or_insert_with(StreamingReply::default);
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
}
