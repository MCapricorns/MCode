//! Pure desktop view-model: state, actions, and reducer.
//!
//! This module has no GPUI dependency. The render layer turns
//! [`WorkspaceState`] into elements and feeds [`DesktopAction`]s back, so the
//! product behavior stays testable without a GPU or window.
/// One sidebar session row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    /// Session identity spelling (`ses1-…`).
    pub session_id: String,
    /// Root branch identity spelling.
    pub root_branch_id: String,
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
    pub providers: Vec<mcode_config::ProviderSettings>,
    /// Configured search backends.
    pub web_backends: Vec<mcode_config::WebBackendSettings>,
    /// MCP servers.
    pub mcp_servers: Vec<mcode_config::McpServerSettings>,
    /// Appearance theme: `light` or `dark`.
    pub theme: String,
    /// A save is in flight.
    pub saving: bool,
    /// Unsaved local edits exist.
    pub dirty: bool,
}

impl SettingsState {
    /// Projects one settings document plus revision.
    #[must_use]
    pub fn from_settings(settings: &mcode_config::AppSettings, revision: u64) -> Self {
        Self {
            revision,
            user_agent: settings.user_agent.clone(),
            effective_user_agent: settings.effective_user_agent(),
            providers: settings.providers.clone(),
            web_backends: settings.web.backends.clone(),
            mcp_servers: settings.mcp_servers.clone(),
            theme: settings.appearance.theme.clone(),
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
            usage: mcode_config::UsageSettings { enabled: true },
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
    /// Visual settings.
    Settings,
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
    /// The editable settings projection.
    pub settings: Option<SettingsState>,
    /// True when the window uses the dark theme.
    pub dark_theme: bool,
    /// Last terminal error surfaced to the user.
    pub error: Option<String>,
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
    /// Settings were persisted under CAS; carries the new revision.
    SettingsSaved(u64),
    /// Toggle light/dark theme.
    ToggleTheme,
    /// Clear the surfaced error.
    DismissError,
}

/// Maximum composer text before the send is rejected locally.
pub const MAX_COMPOSER_CHARS: usize = 64 * 1024;

/// Applies one action to the state.
pub fn reduce(state: &mut WorkspaceState, action: DesktopAction) {
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
        DesktopAction::SettingsSaved(revision) => {
            if let Some(settings) = state.settings.as_mut() {
                settings.revision = revision;
                settings.saving = false;
                settings.dirty = false;
                settings.effective_user_agent = settings.to_settings().effective_user_agent();
            }
        }
        DesktopAction::ToggleTheme => state.dark_theme = !state.dark_theme,
        DesktopAction::DismissError => state.error = None,
    }
}

/// Maps one committed session event to its conversation projection.
#[must_use]
pub fn project_entry(
    event: &mcode_plugin_host::session::SessionEvent,
    text: String,
) -> ConversationEntry {
    use mcode_plugin_host::session::EventKind;
    ConversationEntry {
        event_id: event.event_id.as_str().to_owned(),
        kind: match event.kind {
            EventKind::Message => EntryKind::UserMessage,
            EventKind::ToolCall => EntryKind::ToolCall,
            EventKind::ToolResult => EntryKind::ToolResult,
            EventKind::Usage => EntryKind::Usage,
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
        let settings = SettingsState::from_settings(&mcode_config::AppSettings::default(), 0);
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
                kind: "openai".to_owned(),
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

        reduce(
            &mut state,
            DesktopAction::ShowContextTab(ContextTab::Settings),
        );
        assert_eq!(state.context_tab, ContextTab::Settings);
        reduce(&mut state, DesktopAction::ToggleTheme);
        assert!(state.dark_theme);
        reduce(&mut state, DesktopAction::ToggleTheme);
        assert!(!state.dark_theme);
    }
}
