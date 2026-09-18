//! The workspace window view: fixed Cursor-style three-column layout.
use gpui_kit::component::Root;
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::{App, AppContext as _, Bounds, Context, Entity, Pixels, Window, WindowBounds};
use gpui_kit::{px, size};
use mcode_config::HomeLayout;
use mcode_session::session::{BranchId, HeadStamp, SessionEventId, SessionId};

use crate::bridge::{BridgeCommand, BridgeEvent, BridgeReply, CoreBridge};
use crate::ui::{BackendForm, McpForm, ProviderForm};
use crate::view_model::{ContextTab, DesktopAction, SettingsState, WorkspaceState, reduce};

/// Window chrome bounds for the first window.
const WINDOW_BOUNDS: Bounds<Pixels> = Bounds {
    origin: gpui_kit::point(px(120.), px(80.)),
    size: size(px(1280.), px(840.)),
};

/// Poll cadence for streaming chat events from the core thread.
const EVENT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Opens the main window over one owned home.
///
/// # Panics
///
/// Panics when the window cannot open; the process has no useful headless
/// fallback by design.
pub fn open_window(home: HomeLayout, cx: &mut App) {
    let (bridge, events) = CoreBridge::start(home);
    let options = gpui_kit::WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(WINDOW_BOUNDS)),
        titlebar: Some(gpui_kit::TitlebarOptions {
            title: Some("MCode".into()),
            ..Default::default()
        }),
        window_min_size: Some(size(px(960.), px(560.))),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        let workspace = Workspace::new(bridge, events, window, cx);
        cx.new(|cx| Root::new(workspace, window, cx))
    })
    .expect("open the MCode window");
}

/// The main workspace view.
pub struct Workspace {
    vm: WorkspaceState,
    bridge: CoreBridge,
    composer: Entity<TextareaState>,
    ua_input: Option<Entity<InputState>>,
    ua_sync_pending: bool,
    provider_form: Option<Entity<ProviderForm>>,
    backend_form: Option<Entity<BackendForm>>,
    mcp_form: Option<Entity<McpForm>>,
    mcp_key_input: Option<Entity<InputState>>,
    web_query_input: Option<Entity<InputState>>,
}

impl Workspace {
    /// Builds the workspace and issues the initial core loads.
    pub fn new(
        bridge: CoreBridge,
        events: std::sync::mpsc::Receiver<BridgeEvent>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Message MCode…  (Enter to send, Shift+Enter for a new line)")
                .auto_grow(1, 10)
        });
        let workspace = cx.new(|_| Self {
            vm: WorkspaceState::default(),
            bridge,
            composer,
            ua_input: None,
            ua_sync_pending: false,
            provider_form: None,
            backend_form: None,
            mcp_form: None,
            mcp_key_input: None,
            web_query_input: None,
        });
        workspace.update(cx, |workspace, cx| {
            let composer = workspace.composer.clone();
            cx.subscribe_in(&composer, window, |workspace, _, event, window, cx| {
                workspace.on_composer_event(event, window, cx);
            })
            .detach();
            workspace.spawn_event_pump(events, cx);
            workspace.dispatch(BridgeCommand::ListSessions, cx);
            workspace.dispatch(BridgeCommand::LoadSettings, cx);
        });
        workspace
    }

    /// Polls the core event channel and folds streaming events into state.
    fn spawn_event_pump(
        &self,
        events: std::sync::mpsc::Receiver<BridgeEvent>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(EVENT_POLL_INTERVAL).await;
                loop {
                    match events.try_recv() {
                        Ok(event) => {
                            if this
                                .update(cx, |workspace, cx| workspace.apply_event(event, cx))
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                    }
                }
            }
        })
        .detach();
    }

    fn apply_event(&mut self, event: BridgeEvent, cx: &mut Context<Self>) {
        let active_session = self.vm.active.as_ref().map(|c| c.session_id.clone());
        let matches_active = |session_id: &str| active_session.as_deref() == Some(session_id);
        let action = match event {
            BridgeEvent::ChatText { session_id, delta } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::ChatDelta(delta)
            }
            BridgeEvent::ChatThinking { session_id, delta } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::ChatThinkingDelta(delta)
            }
            BridgeEvent::ChatDone {
                session_id,
                head,
                entry,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::ChatDone { head, entry }
            }
            BridgeEvent::ChatFailed {
                session_id,
                message,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::ChatFailed(message)
            }
            BridgeEvent::ToolStarted {
                session_id,
                call_id,
                name,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::ToolStarted { call_id, name }
            }
            BridgeEvent::ToolCompleted { session_id, entry } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::ToolResultAppended(entry)
            }
        };
        self.apply_action(action, cx);
    }

    fn on_composer_event(
        &mut self,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let text = self.composer.read(cx).value().to_string();
                self.apply_action(DesktopAction::ComposerChanged(text), cx);
            }
            InputEvent::PressEnter { shift: false, .. } => {
                let draft = self.vm.composer_draft.clone();
                if !draft.trim().is_empty() && !self.vm.sending {
                    self.send(draft, window, cx);
                }
            }
            InputEvent::PressEnter { .. } | InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    pub(super) fn apply_action(&mut self, action: DesktopAction, cx: &mut Context<Self>) {
        reduce(&mut self.vm, action);
        cx.notify();
    }

    fn dispatch(&self, command: BridgeCommand, cx: &mut Context<Self>) {
        let request = self.bridge.request(command);
        cx.spawn(async move |this, cx| {
            let reply = request.await;
            let _ = this.update(cx, |workspace, cx| workspace.apply_reply(reply, cx));
        })
        .detach();
    }

    fn apply_reply(&mut self, reply: BridgeReply, cx: &mut Context<Self>) {
        match reply {
            BridgeReply::Sessions(Ok(sessions)) => {
                self.apply_action(DesktopAction::SessionsLoaded(sessions), cx);
            }
            BridgeReply::Created(Ok(summary)) => {
                let session_id = SessionId::parse(&summary.session_id).expect("core session id");
                self.apply_action(DesktopAction::SessionCreated(summary), cx);
                self.dispatch(BridgeCommand::OpenSession(session_id), cx);
            }
            BridgeReply::Conversation(Ok(conversation)) => {
                self.apply_action(DesktopAction::ConversationOpened(conversation), cx);
            }
            BridgeReply::Sent(Ok((head, entry))) => {
                self.apply_action(DesktopAction::MessageSent { head, entry }, cx);
                self.begin_chat_turn(cx);
            }
            BridgeReply::Settings(Ok((settings, revision, provider_keys, mcp_keys))) => {
                let revision = revision.get();
                let mut state = SettingsState::from_settings(&settings, revision, provider_keys);
                state.mcp_with_keys = mcp_keys;
                self.ua_sync_pending = true;
                self.apply_action(DesktopAction::SettingsLoaded(state), cx);
            }
            BridgeReply::SettingsSaved(Ok(revision)) => {
                self.apply_action(DesktopAction::SettingsSaved(revision.get()), cx);
            }
            BridgeReply::ProviderKeySaved(Ok(())) => {
                self.dispatch(BridgeCommand::LoadSettings, cx);
            }
            BridgeReply::ChatStarted(Ok(())) => {}
            BridgeReply::WebSearched(Ok(results)) => {
                self.apply_action(DesktopAction::WebSearched(results), cx);
            }
            BridgeReply::McpTools(Ok((server_id, tools))) => {
                self.apply_action(DesktopAction::McpToolsListed { server_id, tools }, cx);
            }
            BridgeReply::Sessions(Err(message))
            | BridgeReply::Created(Err(message))
            | BridgeReply::Conversation(Err(message))
            | BridgeReply::Sent(Err(message))
            | BridgeReply::Settings(Err(message))
            | BridgeReply::SettingsSaved(Err(message))
            | BridgeReply::ProviderKeySaved(Err(message))
            | BridgeReply::ChatStarted(Err(message))
            | BridgeReply::WebSearched(Err(message))
            | BridgeReply::McpTools(Err(message)) => {
                self.apply_action(DesktopAction::Failed(message), cx);
            }
        }
    }

    /// Starts one model turn over the active conversation using the first
    /// enabled configured provider.
    fn begin_chat_turn(&mut self, cx: &mut Context<Self>) {
        let Some(conversation) = self.vm.active.clone() else {
            self.apply_action(DesktopAction::Failed("no open session".to_owned()), cx);
            return;
        };
        let (Some(session), Some(branch)) = (
            SessionId::parse(&conversation.session_id),
            BranchId::parse(&conversation.branch_id),
        ) else {
            return;
        };
        let expected_head = parse_head(&conversation.head);
        let Some(settings) = self.vm.settings.as_ref() else {
            self.apply_action(
                DesktopAction::Failed("settings are still loading".to_owned()),
                cx,
            );
            return;
        };
        let Some(provider) = settings.providers.iter().find(|p| p.enabled) else {
            self.apply_action(
                DesktopAction::Failed(
                    "no enabled provider — add one with its API key in Settings".to_owned(),
                ),
                cx,
            );
            return;
        };
        let Some(model) = provider.models.first() else {
            self.apply_action(
                DesktopAction::Failed("the provider has no models configured".to_owned()),
                cx,
            );
            return;
        };
        let history: Vec<mcode_core::Message> = conversation
            .entries
            .iter()
            .map(|entry| match entry.kind {
                crate::view_model::EntryKind::UserMessage => {
                    mcode_core::Message::User(mcode_core::UserMessage::text(entry.text.clone()))
                }
                _ => mcode_core::Message::Assistant(mcode_core::AssistantMessage {
                    blocks: vec![mcode_core::ContentBlock::Text(mcode_core::TextBlock::new(
                        entry.text.clone(),
                    ))],
                    usage: None,
                    stop_reason: mcode_core::StopReason::Stop,
                }),
            })
            .collect();
        self.dispatch(
            BridgeCommand::ChatTurn {
                session,
                branch,
                expected_head,
                provider_id: provider.id.clone(),
                model: model.clone(),
                history,
            },
            cx,
        );
    }

    fn send(&mut self, draft: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conversation) = self.vm.active.clone() else {
            return;
        };
        let (Some(session), Some(branch)) = (
            SessionId::parse(&conversation.session_id),
            BranchId::parse(&conversation.branch_id),
        ) else {
            return;
        };
        let expected_head = parse_head(&conversation.head);
        self.vm.sending = true;
        self.vm.composer_draft.clear();
        self.composer
            .update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();
        self.dispatch(
            BridgeCommand::SendMessage {
                session,
                branch,
                expected_head,
                text: draft,
            },
            cx,
        );
    }

    pub(super) fn on_new_session(&mut self, cx: &mut Context<Self>) {
        self.dispatch(BridgeCommand::CreateSession, cx);
    }

    pub(super) fn on_open_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if let Some(session_id) = SessionId::parse(session_id) {
            self.dispatch(BridgeCommand::OpenSession(session_id), cx);
        }
    }

    pub(super) fn on_toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = if self.vm.dark_theme {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        Theme::change(next, Some(window), cx);
        self.apply_action(DesktopAction::ToggleTheme, cx);
    }

    pub(super) fn on_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.vm.composer_draft.clone();
        if !draft.trim().is_empty() && !self.vm.sending {
            self.send(draft, window, cx);
        }
    }

    pub(super) fn on_show_tab(&mut self, tab: ContextTab, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ShowContextTab(tab), cx);
    }

    pub(super) fn on_list_mcp_tools(&mut self, server_id: &str, cx: &mut Context<Workspace>) {
        self.dispatch(
            BridgeCommand::McpListTools {
                server_id: server_id.to_owned(),
            },
            cx,
        );
    }

    pub(super) fn on_add_builtin_mcp(
        &mut self,
        server: mcode_config::McpServerSettings,
        api_key: &str,
        cx: &mut Context<Workspace>,
    ) {
        let server_id = server.id.clone();
        self.apply_action(DesktopAction::SettingsMcpAdded(server), cx);
        if !api_key.is_empty() {
            self.dispatch(
                BridgeCommand::SaveProviderKey {
                    provider_id: format!("mcp-{server_id}"),
                    api_key: api_key.to_owned(),
                },
                cx,
            );
        }
    }

    pub(super) fn on_web_search(&mut self, query: &str, cx: &mut Context<Workspace>) {
        if query.trim().is_empty() {
            return;
        }
        self.dispatch(
            BridgeCommand::WebSearch {
                query: query.trim().to_owned(),
            },
            cx,
        );
    }

    pub(super) fn on_dismiss_error(&mut self, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::DismissError, cx);
    }

    pub(super) fn vm(&self) -> &WorkspaceState {
        &self.vm
    }

    pub(super) fn composer(&self) -> &Entity<TextareaState> {
        &self.composer
    }

    pub(super) fn refresh_sessions(&mut self, cx: &mut Context<Self>) {
        self.dispatch(BridgeCommand::ListSessions, cx);
    }

    pub(super) fn settings_ua_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if self.ua_input.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("pi agent default"));
            cx.subscribe_in(&input, window, |workspace, entity, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = entity.read(cx).value().to_string();
                    workspace.apply_action(DesktopAction::SettingsUserAgentChanged(text), cx);
                }
            })
            .detach();
            self.ua_input = Some(input);
        }
        let input = self.ua_input.clone().expect("ua input");
        if self.ua_sync_pending {
            let target = self
                .vm
                .settings
                .as_ref()
                .map(|settings| settings.user_agent.clone())
                .unwrap_or_default();
            let current = input.read(cx).value().to_string();
            if current != target {
                input.update(cx, |state, cx| state.set_value(target, window, cx));
            }
            self.ua_sync_pending = false;
        }
        input
    }

    pub(super) fn web_query_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<InputState> {
        self.web_query_input
            .get_or_insert_with(|| {
                cx.new(|cx| InputState::new(window, cx).placeholder("Search the web…"))
            })
            .clone()
    }

    pub(super) fn on_web_search_run(&mut self, cx: &mut Context<Workspace>) {
        let Some(input) = self.web_query_input.clone() else {
            return;
        };
        let query = input.read(cx).value().trim().to_owned();
        if query.is_empty() {
            return;
        }
        self.on_web_search(&query, cx);
    }

    pub(super) fn provider_form(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ProviderForm> {
        self.provider_form
            .get_or_insert_with(|| ProviderForm::new(window, cx))
            .clone()
    }

    pub(super) fn on_add_provider(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.provider_form.clone() else {
            return;
        };
        let id = form.read(cx).id.read(cx).value().trim().to_string();
        let kind = form.read(cx).kind.read(cx).value().trim().to_string();
        let base_url = form.read(cx).base_url.read(cx).value().trim().to_string();
        let model = form.read(cx).model.read(cx).value().trim().to_string();
        let api_key = form.read(cx).api_key.read(cx).value().trim().to_string();
        if id.is_empty() || kind.is_empty() || base_url.is_empty() || model.is_empty() {
            self.apply_action(
                DesktopAction::Failed("fill id, kind, base URL, and model".to_owned()),
                cx,
            );
            return;
        }
        self.apply_action(
            DesktopAction::SettingsProviderAdded(mcode_config::ProviderSettings {
                id: id.clone(),
                kind,
                base_url,
                models: vec![model],
                enabled: true,
            }),
            cx,
        );
        if !api_key.is_empty() {
            self.dispatch(
                BridgeCommand::SaveProviderKey {
                    provider_id: id,
                    api_key,
                },
                cx,
            );
        }
    }

    pub(super) fn on_remove_provider(&mut self, index: usize, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::SettingsProviderRemoved(index), cx);
    }

    pub(super) fn backend_form(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<BackendForm> {
        self.backend_form
            .get_or_insert_with(|| BackendForm::new(window, cx))
            .clone()
    }

    pub(super) fn on_add_backend(&mut self, cx: &mut Context<Workspace>) {
        let Some(form) = self.backend_form.clone() else {
            return;
        };
        let id = form.read(cx).id.read(cx).value().trim().to_string();
        let kind = form.read(cx).kind.read(cx).value().trim().to_string();
        let endpoint = form.read(cx).endpoint.read(cx).value().trim().to_string();
        if id.is_empty() || kind.is_empty() || endpoint.is_empty() {
            self.apply_action(
                DesktopAction::Failed("fill id, kind, and endpoint".to_owned()),
                cx,
            );
            return;
        }
        self.apply_action(
            DesktopAction::SettingsBackendAdded(mcode_config::WebBackendSettings {
                id,
                kind,
                endpoint,
                enabled: false,
            }),
            cx,
        );
    }

    pub(super) fn on_remove_backend(&mut self, index: usize, cx: &mut Context<Workspace>) {
        self.apply_action(DesktopAction::SettingsBackendRemoved(index), cx);
    }

    pub(super) fn on_toggle_backend(&mut self, index: usize, cx: &mut Context<Workspace>) {
        let enabled = self
            .vm
            .settings
            .as_ref()
            .and_then(|settings| settings.web_backends.get(index))
            .map(|backend| !backend.enabled)
            .unwrap_or(false);
        self.apply_action(DesktopAction::SettingsBackendToggled(index, enabled), cx);
    }

    pub(super) fn mcp_form(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<McpForm> {
        self.mcp_form
            .get_or_insert_with(|| McpForm::new(window, cx))
            .clone()
    }

    pub(super) fn mcp_key_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<InputState> {
        self.mcp_key_input
            .get_or_insert_with(|| {
                cx.new(|cx| InputState::new(window, cx).placeholder("paste API key here"))
            })
            .clone()
    }

    pub(super) fn on_add_mcp(&mut self, cx: &mut Context<Workspace>) {
        let Some(form) = self.mcp_form.clone() else {
            return;
        };
        let id = form.read(cx).id.read(cx).value().trim().to_string();
        let transport = form.read(cx).transport.read(cx).value().trim().to_string();
        let endpoint = form.read(cx).endpoint.read(cx).value().trim().to_string();
        let command = form.read(cx).command.read(cx).value().trim().to_string();
        let api_key = form.read(cx).api_key.read(cx).value().trim().to_string();
        if id.is_empty() || transport.is_empty() {
            self.apply_action(
                DesktopAction::Failed("fill id and transport".to_owned()),
                cx,
            );
            return;
        }
        let server = match transport.as_str() {
            "http" => {
                if endpoint.is_empty() {
                    self.apply_action(
                        DesktopAction::Failed("http servers need an endpoint".to_owned()),
                        cx,
                    );
                    return;
                }
                mcode_config::McpServerSettings {
                    id: id.clone(),
                    enabled: false,
                    transport,
                    command: None,
                    args: Vec::new(),
                    endpoint: Some(endpoint),
                    key_header: Some("bearer".to_owned()),
                }
            }
            "stdio" => {
                if command.is_empty() {
                    self.apply_action(
                        DesktopAction::Failed("stdio servers need a command".to_owned()),
                        cx,
                    );
                    return;
                }
                mcode_config::McpServerSettings {
                    id: id.clone(),
                    enabled: false,
                    transport,
                    command: Some(command),
                    args: Vec::new(),
                    endpoint: None,
                    key_header: None,
                }
            }
            _ => {
                self.apply_action(
                    DesktopAction::Failed("transport must be http or stdio".to_owned()),
                    cx,
                );
                return;
            }
        };
        self.apply_action(DesktopAction::SettingsMcpAdded(server), cx);
        if !api_key.is_empty() {
            self.dispatch(
                BridgeCommand::SaveProviderKey {
                    provider_id: format!("mcp-{id}"),
                    api_key,
                },
                cx,
            );
        }
    }

    pub(super) fn on_save_settings(&mut self, cx: &mut Context<Self>) {
        let Some(settings) = self.vm.settings.clone() else {
            return;
        };
        if settings.saving || !settings.dirty {
            return;
        }
        let document = settings.to_settings();
        if let Err(message) = document.validate() {
            self.apply_action(
                DesktopAction::Failed(format!("invalid settings: {message}")),
                cx,
            );
            return;
        }
        self.vm.settings.as_mut().expect("settings").saving = true;
        let revision = mcode_config::AuthorityRevision::new(settings.revision)
            .unwrap_or(mcode_config::AuthorityRevision::ABSENT);
        cx.notify();
        self.dispatch(
            BridgeCommand::SaveSettings {
                expected_revision: revision,
                settings: document,
            },
            cx,
        );
    }
}

fn parse_head(spelling: &str) -> HeadStamp {
    if spelling == "empty" {
        HeadStamp::Empty
    } else {
        SessionEventId::parse(spelling)
            .map(HeadStamp::Event)
            .unwrap_or(HeadStamp::Empty)
    }
}

/// Renders the whole window; split across the `ui` submodule.
impl Workspace {
    pub(super) fn render_root(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        crate::ui::render_root(self, window, cx)
    }
}

impl gpui_kit::Render for Workspace {
    fn render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        self.render_root(window, cx)
    }
}
