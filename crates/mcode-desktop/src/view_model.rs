//! Pure desktop view-model: state, actions, and reducer.
//!
//! This module has no GPUI dependency. The render layer turns
//! [`WorkspaceState`] into elements and feeds [`DesktopAction`]s back, so the
//! product behavior stays testable without a GPU or window.
use std::sync::Arc;

use mcode_updates::PreparedUpdate;

/// One sidebar session row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    /// Session identity spelling (`ses1-…`).
    pub session_id: String,
    /// Root branch identity spelling.
    pub root_branch_id: String,
    /// Display title: the session's first user message, trimmed.
    pub title: String,
    /// Total committed events across branches.
    pub event_count: u64,
    /// Whether this session is currently open.
    pub active: bool,
}

/// How one conversation entry renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// A user-authored message.
    UserMessage,
    /// An assistant message (wired with providers at T12).
    AssistantMessage,
    /// An issued tool call.
    ToolCall,
    /// A completed tool result.
    ToolResult,
    /// A usage record.
    Usage,
}

/// One rendered conversation entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationEntry {
    /// Event identity spelling.
    pub event_id: String,
    /// Rendering class.
    pub kind: EntryKind,
    /// Display text (already lossy-decoded).
    pub text: String,
    /// Call identity for tool entries.
    pub call_id: Option<String>,
}

/// The currently open conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveConversation {
    /// Session identity spelling.
    pub session_id: String,
    /// Root branch identity spelling.
    pub branch_id: String,
    /// Current committed head: `empty` or an event identity.
    pub head: String,
    /// Entries in ledger order.
    pub entries: Vec<ConversationEntry>,
    /// Live assistant reply while a model turn streams.
    pub streaming: Option<StreamingReply>,
}

/// Buffered streaming reply fragments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamingReply {
    /// Visible assistant text so far.
    pub text: String,
    /// Reasoning text so far.
    pub thinking: String,
}

/// Upper bound kept for one streamed reply before further deltas are dropped.
pub const MAX_STREAMING_CHARS: usize = 256 * 1024;

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
    pub providers: Vec<mcode_config::ProviderSettings>,
    /// Configured search backends.
    pub web_backends: Vec<mcode_config::WebBackendSettings>,
    /// MCP servers.
    pub mcp_servers: Vec<mcode_config::McpServerSettings>,
    /// Appearance theme: `light` or `dark`.
    pub theme: String,
    /// Provider ids that have a stored API key.
    pub providers_with_keys: Vec<String>,
    /// MCP key ids (form `mcp-<server>`) that have a stored key.
    pub mcp_with_keys: Vec<String>,
    /// Whether durable usage records are written.
    pub usage_enabled: bool,
    /// A save is in flight.
    pub saving: bool,
    /// Unsaved local edits exist.
    pub dirty: bool,
}

impl SettingsState {
    /// Projects one settings document plus revision and stored key ids.
    #[must_use]
    pub fn from_settings(
        settings: &mcode_config::AppSettings,
        revision: u64,
        providers_with_keys: Vec<String>,
    ) -> Self {
        let _ = providers_with_keys;
        Self {
            revision,
            user_agent: settings.user_agent.clone(),
            effective_user_agent: settings.effective_user_agent(),
            providers: settings.providers.clone(),
            web_backends: settings.web.backends.clone(),
            mcp_servers: settings.mcp_servers.clone(),
            theme: settings.appearance.theme.clone(),
            providers_with_keys,
            mcp_with_keys: Vec::new(),
            usage_enabled: settings.usage.enabled,
            saving: false,
            dirty: false,
        }
    }

    /// Builds the document the editor currently shows.
    #[must_use]
    pub fn to_settings(&self) -> mcode_config::AppSettings {
        mcode_config::AppSettings {
            user_agent: self.user_agent.clone(),
            providers: self.providers.clone(),
            web: mcode_config::WebSettings {
                backends: self.web_backends.clone(),
            },
            usage: mcode_config::UsageSettings {
                enabled: self.usage_enabled,
            },
            mcp_servers: self.mcp_servers.clone(),
            appearance: mcode_config::AppearanceSettings {
                theme: self.theme.clone(),
            },
        }
    }
}

/// Right context panel tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContextTab {
    /// Session overview.
    #[default]
    Overview,
    /// Bounded web search.
    Web,
    /// Changed files and diffs from tool activity.
    Changes,
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

/// One settings navigation section (the secondary menu).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsSection {
    /// Theme and request identity.
    #[default]
    General,
    /// Providers, catalog presets, and custom endpoints.
    Models,
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
            Self::Mcp => IconName::PlugZap,
            Self::Web => IconName::Globe,
            Self::Data => IconName::Database,
            Self::About => IconName::Info,
        }
    }
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
    /// Selected right-panel tab.
    pub context_tab: ContextTab,
    /// Latest web search results.
    pub web_results: Vec<mcode_web::SearchResult>,
    /// Per-server tool names from the last listing, keyed by server id.
    pub mcp_tools: Vec<(String, Vec<String>)>,
    /// Prompt resources for the open session: (name, path).
    pub resources: Vec<(String, String)>,
    /// Pending ask rows awaiting user answers.
    pub pending_ask: Option<Vec<(String, Vec<String>, bool)>>,
    /// Durable task list rows: (content, status).
    pub todo_rows: Vec<(String, String)>,
    /// Cumulative usage per provider/model: (key, input, output, requests).
    pub usage_totals: Vec<(String, u64, u64, u64)>,
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
    pub catalog: Option<Arc<mcode_catalog::CatalogDocument>>,
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
    pub last_offer: Option<mcode_updates::UpdateOffer>,
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
    /// Switch the right-panel tab.
    ShowContextTab(ContextTab),
    /// Settings loaded from the core.
    SettingsLoaded(SettingsState),
    /// The settings editor changed the User-Agent.
    SettingsUserAgentChanged(String),
    /// The settings editor changed a provider row.
    SettingsProviderChanged(usize, mcode_config::ProviderSettings),
    /// The settings editor added a provider row.
    SettingsProviderAdded(mcode_config::ProviderSettings),
    /// The settings editor removed a provider row.
    SettingsProviderRemoved(usize),
    /// The settings editor added a web backend row.
    SettingsBackendAdded(mcode_config::WebBackendSettings),
    /// The settings editor removed a web backend row.
    SettingsBackendRemoved(usize),
    /// The settings editor toggled a web backend.
    SettingsBackendToggled(usize, bool),
    /// The settings editor toggled durable usage records.
    SettingsUsageToggled(bool),
    /// The settings editor added an MCP server.
    SettingsMcpAdded(mcode_config::McpServerSettings),
    /// The settings editor removed an MCP server.
    SettingsMcpRemoved(usize),
    /// The settings editor toggled an MCP server.
    SettingsMcpToggled(usize, bool),
    /// A server's tools listing arrived.
    McpToolsListed {
        server_id: String,
        tools: Vec<String>,
    },
    /// A tool call started on the open conversation.
    ToolStarted { call_id: String, name: String },
    /// A committed tool-result entry arrived.
    ToolResultAppended(ConversationEntry),
    /// Prompt resources discovered for the open session.
    ResourcesLoaded(Vec<(String, String)>),
    /// A durable usage record arrived.
    UsageRecorded {
        provider: String,
        model: String,
        input: u64,
        output: u64,
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
    ProviderKeySaved(Vec<String>),
    /// Incremental assistant text from the active model turn.
    ChatDelta(String),
    /// Incremental assistant reasoning from the active model turn.
    ChatThinkingDelta(String),
    /// The model turn finished and its entry was committed.
    ChatDone {
        head: String,
        entry: ConversationEntry,
    },
    /// The model turn failed without committing anything.
    ChatFailed(String),
    /// Web search completed.
    WebSearched(Vec<mcode_web::SearchResult>),
    /// Toggle light/dark theme.
    ToggleTheme,
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
        document: Arc<mcode_catalog::CatalogDocument>,
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
    /// Self-update progress changed.
    UpdateStateChanged(UpdateState),
    /// A release offer was resolved for the available update.
    UpdateOfferFound(mcode_updates::UpdateOffer),
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
        DesktopAction::ConversationOpened(conversation) => {
            let session_id = conversation.session_id.clone();
            state.active = Some(conversation);
            for session in &mut state.sessions {
                session.active = session.session_id == session_id;
            }
        }
        DesktopAction::ComposerChanged(text) => {
            state.composer_draft = text.chars().take(MAX_COMPOSER_CHARS).collect();
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
                    text: name,
                    call_id: Some(call_id),
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
            entry,
        } => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.entries.push(entry);
            }
            let key = format!("{provider}/{model}");
            if let Some(row) = state
                .usage_totals
                .iter_mut()
                .find(|(existing, _, _, _)| *existing == key)
            {
                row.1 += input;
                row.2 += output;
                row.3 += 1;
            } else {
                state.usage_totals.push((key, input, output, 1));
            }
        }
        DesktopAction::TodoUpdated(tasks) => state.todo_rows = tasks,
        DesktopAction::AskRequested(rows) => state.pending_ask = Some(rows),
        DesktopAction::AskAnswered => state.pending_ask = None,
        DesktopAction::ChatThinkingDelta(delta) => {
            append_streaming(state, true, delta);
        }
        DesktopAction::ChatDone { head, entry } => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.head = head;
                conversation.entries.push(entry);
                conversation.streaming = None;
            }
            state.sending = false;
        }
        DesktopAction::ChatFailed(message) => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.streaming = None;
            }
            state.error = Some(message);
            state.sending = false;
        }
        DesktopAction::Failed(message) => {
            state.error = Some(message);
            state.sending = false;
        }
        DesktopAction::ShowContextTab(tab) => state.context_tab = tab,
        DesktopAction::SettingsLoaded(settings) => {
            let dark = settings.theme != "light";
            state.settings = Some(settings);
            state.dark_theme = dark;
        }
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
                && settings.providers.len() < mcode_config::MAX_PROVIDERS
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
                && settings.web_backends.len() < mcode_config::MAX_WEB_BACKENDS
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
                && let Some(backend) = settings.web_backends.get_mut(index)
            {
                backend.enabled = enabled;
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
        DesktopAction::ProviderKeySaved(keyed_ids) => {
            if let Some(settings) = state.settings.as_mut() {
                settings.providers_with_keys = keyed_ids;
            }
        }
        DesktopAction::WebSearched(results) => state.web_results = results,
        DesktopAction::SettingsMcpAdded(server) => {
            if let Some(settings) = state.settings.as_mut()
                && settings.mcp_servers.len() < mcode_config::MAX_MCP_SERVERS
            {
                settings.mcp_servers.push(server);
                settings.dirty = true;
            }
        }
        DesktopAction::SettingsMcpRemoved(index) => {
            if let Some(settings) = state.settings.as_mut()
                && index < settings.mcp_servers.len()
            {
                settings.mcp_servers.remove(index);
                settings.dirty = true;
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
        DesktopAction::McpToolsListed { server_id, tools } => {
            if let Some(entry) = state.mcp_tools.iter_mut().find(|(id, _)| *id == server_id) {
                entry.1 = tools;
            } else {
                state.mcp_tools.push((server_id, tools));
            }
        }
        DesktopAction::ToggleTheme => state.dark_theme = !state.dark_theme,
        DesktopAction::DismissError => state.error = None,
        DesktopAction::ShowMainView(view) => state.view = view,
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
            state.project_dir = last_project;
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
            state.recents.truncate(mcode_config::MAX_RECENT_PROJECTS);
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
                .truncate(mcode_config::MAX_SESSION_PROJECTS);
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
            state.active_preset = preset;
            state.preset_model_menu_open = false;
            state.preset_models = Vec::new();
        }
        DesktopAction::PresetModelToggled(model) => {
            if let Some(position) = state.preset_models.iter().position(|m| *m == model) {
                state.preset_models.remove(position);
            } else {
                state.preset_models.push(model);
            }
        }
        DesktopAction::PresetModelMenuToggled(open) => state.preset_model_menu_open = open,
        DesktopAction::UpdateStateChanged(update) => state.update = update,
        DesktopAction::UpdateOfferFound(offer) => state.last_offer = Some(offer),
        DesktopAction::AutoUpdateToggled(auto_update) => state.auto_update = auto_update,
        DesktopAction::UpdateStaged(prepared) => {
            state.prepared_update = Some(prepared);
            state.update = UpdateState::Ready {
                version: mcode_updates::current_version().to_owned(),
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

/// Maps one committed session event to its conversation projection.
#[must_use]
pub fn project_entry(
    event: &mcode_session::session::SessionEvent,
    text: String,
) -> ConversationEntry {
    use mcode_session::session::EventKind;
    ConversationEntry {
        event_id: event.event_id.as_str().to_owned(),
        kind: match event.kind {
            EventKind::Message => EntryKind::UserMessage,
            EventKind::ToolCall => EntryKind::ToolCall,
            EventKind::ToolResult => EntryKind::ToolResult,
            EventKind::Usage | EventKind::Task => EntryKind::Usage,
        },
        text,
        call_id: event.call_id.as_ref().map(|call| call.as_str().to_owned()),
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
                    text: "hello".to_owned(),
                    call_id: None,
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
            SettingsState::from_settings(&mcode_config::AppSettings::default(), 0, Vec::new());
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
            DesktopAction::SettingsProviderAdded(mcode_config::ProviderSettings {
                id: "openai-main".to_owned(),
                kind: "openai-completions".to_owned(),
                base_url: "https://api.openai.com/v1".to_owned(),
                models: vec!["gpt-x".to_owned()],
                enabled: true,
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
    fn failure_and_tabs_round_trip() {
        let mut state = WorkspaceState::default();
        reduce(&mut state, DesktopAction::Failed("boom".to_owned()));
        assert_eq!(state.error.as_deref(), Some("boom"));
        reduce(&mut state, DesktopAction::DismissError);
        assert_eq!(state.error, None);

        reduce(&mut state, DesktopAction::ShowMainView(MainView::Settings));
        assert_eq!(state.view, MainView::Settings);
        reduce(&mut state, DesktopAction::ToggleTheme);
        assert!(state.dark_theme);
        reduce(&mut state, DesktopAction::ToggleTheme);
        assert!(!state.dark_theme);
    }

    #[test]
    fn model_selection_falls_back_to_an_enabled_provider() {
        let mut state = WorkspaceState::default();
        let mut settings =
            SettingsState::from_settings(&mcode_config::AppSettings::default(), 0, Vec::new());
        settings.providers.push(mcode_config::ProviderSettings {
            id: "acme".to_owned(),
            kind: "openai-completions".to_owned(),
            base_url: "https://api.acme.dev/v1".to_owned(),
            models: vec!["m1".to_owned(), "m2".to_owned()],
            enabled: true,
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
                    text: "hello".to_owned(),
                    call_id: None,
                },
            },
        );
        let conversation = state.active.as_ref().expect("conversation");
        assert_eq!(conversation.head, "evt1-x");
        assert!(conversation.streaming.is_none());
        assert_eq!(conversation.entries.len(), 1);
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
    fn chat_deltas_are_dropped_without_a_conversation() {
        let mut state = WorkspaceState::default();
        reduce(&mut state, DesktopAction::ChatDelta("ignored".to_owned()));
        assert!(state.active.is_none());
        assert!(!state.sending);
    }
}
