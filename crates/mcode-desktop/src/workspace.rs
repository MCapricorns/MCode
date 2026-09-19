//! The workspace window view: activity bar, sessions sidebar, chat, and a
//! full-page settings view.
use gpui_kit::component::Root;
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::{App, AppContext as _, Bounds, Context, Entity, Pixels, Window, WindowBounds};
use gpui_kit::{px, size};
use mcode_config::HomeLayout;
use mcode_session::session::{BranchId, HeadStamp, SessionEventId, SessionId};

use crate::bridge::{BridgeCommand, BridgeEvent, BridgeReply, CoreBridge};
use crate::ui::{BackendForm, McpForm, ProviderForm};
use crate::view_model::{
    DesktopAction, MainView, SettingsState, UpdateState, WorkspaceState, reduce,
};

/// Window chrome bounds for the first window.
const WINDOW_BOUNDS: Bounds<Pixels> = Bounds {
    origin: gpui_kit::point(px(120.), px(80.)),
    size: size(px(1280.), px(840.)),
};

/// Poll cadence for streaming chat events from the core thread.
const EVENT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Re-run the automatic update check after one day of runtime.
const UPDATE_RECHECK_TICKS: u64 = 24 * 60 * 60 * 1000 / EVENT_POLL_INTERVAL.as_millis() as u64;

/// Opens the main window over one owned home.
///
/// # Panics
///
/// Panics when the window cannot open; the process has no useful headless
/// fallback by design.
pub fn open_window(home: HomeLayout, cx: &mut App) {
    let (bridge, events) = CoreBridge::start(home);
    // The custom titlebar owns dragging and window controls, so the system
    // titlebar is hidden (`appears_transparent`).
    let mut options = gpui_kit::component::TitleBar::window_options();
    options.window_bounds = Some(WindowBounds::Windowed(WINDOW_BOUNDS));
    options.window_min_size = Some(size(px(960.), px(560.)));
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.title = Some("MCode".into());
    }
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
    preset_key_input: Option<Entity<InputState>>,
    preset_search_input: Option<Entity<InputState>>,
    ask_input: Option<Entity<InputState>>,
    pending_project: Option<String>,
    /// Draft restored by 撤回修改, applied on the next render (needs a window).
    pending_composer_prefill: Option<String>,
    pending_catalog_refresh: bool,
    runtime_ticks: u64,
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
            preset_key_input: None,
            preset_search_input: None,
            ask_input: None,
            pending_project: None,
            pending_composer_prefill: None,
            pending_catalog_refresh: false,
            runtime_ticks: 0,
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
            workspace.dispatch(BridgeCommand::LoadUiState, cx);
            workspace.dispatch(BridgeCommand::GetCatalog, cx);
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
                let _ = this.update(cx, |workspace, cx| workspace.on_runtime_tick(cx));
            }
        })
        .detach();
    }

    /// Periodic background work: the daily update re-check.
    fn on_runtime_tick(&mut self, cx: &mut Context<Self>) {
        self.runtime_ticks += 1;
        if self.runtime_ticks.is_multiple_of(UPDATE_RECHECK_TICKS) && self.vm.auto_update {
            self.on_check_update(cx);
        }
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
                // Refresh the sidebar so the session title picks up the turn.
                self.dispatch(BridgeCommand::ListSessions, cx);
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
            BridgeEvent::UsageRecorded {
                session_id,
                provider,
                model,
                input,
                output,
                entry,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::UsageRecorded {
                    provider,
                    model,
                    input,
                    output,
                    entry,
                }
            }
            BridgeEvent::TodoUpdated { session_id, tasks } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::TodoUpdated(tasks)
            }
            BridgeEvent::AskRequested {
                session_id,
                questions,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::AskRequested(questions)
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
            BridgeEvent::CatalogUpdated { .. } => {
                self.dispatch(BridgeCommand::GetCatalog, cx);
                return;
            }
            BridgeEvent::UpdateAvailable { offer } => {
                let version = offer.version.clone();
                let notes_url = offer.notes_url.clone();
                self.apply_action(DesktopAction::UpdateOfferFound(offer), cx);
                DesktopAction::UpdateStateChanged(UpdateState::Available { version, notes_url })
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
                if let Some(project) = self.pending_project.take() {
                    self.bind_project(&project, cx);
                }
                self.dispatch(BridgeCommand::ListSessions, cx);
            }
            BridgeReply::Conversation(Ok(conversation)) => {
                let session_id = conversation.session_id.clone();
                self.apply_action(DesktopAction::ConversationOpened(conversation), cx);
                self.dispatch(BridgeCommand::ListResources { session_id }, cx);
            }
            BridgeReply::Resources(Ok(files)) => {
                self.apply_action(DesktopAction::ResourcesLoaded(files), cx);
            }
            BridgeReply::AskAnswered(Ok(())) => {}
            BridgeReply::Sent(Ok((head, entry))) => {
                self.apply_action(DesktopAction::MessageSent { head, entry }, cx);
                self.begin_chat_turn(cx);
            }
            BridgeReply::Settings(Ok((settings, revision, provider_keys, mcp_keys))) => {
                let revision = revision.get();
                let mut state = SettingsState::from_settings(&settings, revision, provider_keys);
                state.mcp_with_keys = mcp_keys;
                // Apply the persisted theme once at startup; later changes go
                // through on_toggle_theme.
                let mode = if state.theme == "light" {
                    ThemeMode::Light
                } else {
                    ThemeMode::Dark
                };
                Theme::change(mode, None, cx);
                self.ua_sync_pending = true;
                self.apply_action(DesktopAction::SettingsLoaded(state), cx);
            }
            BridgeReply::Exported(Ok(_summary)) => {}
            BridgeReply::Exported(Err(message)) => {
                self.apply_action(
                    DesktopAction::Failed(format!("export failed: {message}")),
                    cx,
                );
            }
            BridgeReply::Imported(Ok(summary)) => {
                // Reload everything the bundle may have replaced.
                self.dispatch(BridgeCommand::LoadSettings, cx);
                self.dispatch(BridgeCommand::LoadUiState, cx);
                self.dispatch(BridgeCommand::ListSessions, cx);
                if summary.sessions > 0 {
                    self.apply_action(
                        DesktopAction::Failed(format!(
                            "imported {} new session(s); restart to see restored history",
                            summary.sessions
                        )),
                        cx,
                    );
                }
            }
            BridgeReply::Imported(Err(message)) => {
                self.apply_action(
                    DesktopAction::Failed(format!("import failed: {message}")),
                    cx,
                );
            }
            BridgeReply::Recalled(Ok((conversation, edit))) => {
                let prefill = edit.clone();
                self.apply_action(
                    DesktopAction::ConversationOpened((*conversation).clone()),
                    cx,
                );
                if prefill.is_some() {
                    self.pending_composer_prefill = prefill;
                }
                cx.notify();
            }
            BridgeReply::SessionDeleted(Ok(())) => {
                self.apply_action(DesktopAction::SessionDeleted, cx);
                self.dispatch(BridgeCommand::ListSessions, cx);
            }
            BridgeReply::SettingsSaved(Ok(revision)) => {
                self.apply_action(DesktopAction::SettingsSaved(revision.get()), cx);
            }
            BridgeReply::ProviderKeySaved(Ok(())) => {
                self.dispatch(BridgeCommand::LoadSettings, cx);
            }
            BridgeReply::ChatStarted(Ok(())) => {}
            // Web search is a model tool now; UI-initiated replies are ignored.
            BridgeReply::WebSearched(_) => {}
            BridgeReply::McpTools(Ok((server_id, tools))) => {
                self.apply_action(DesktopAction::McpToolsListed { server_id, tools }, cx);
            }
            BridgeReply::RolledBack(Ok(restored)) => {
                let message = if restored.is_empty() {
                    "nothing to roll back".to_owned()
                } else {
                    format!("restored {} file(s)", restored.len())
                };
                self.apply_action(DesktopAction::Failed(message), cx);
            }
            BridgeReply::Catalog(Ok(info)) => {
                self.pending_catalog_refresh = false;
                self.apply_action(
                    DesktopAction::CatalogLoaded {
                        document: info.document,
                        fetched_at: info.fetched_at,
                    },
                    cx,
                );
            }
            BridgeReply::Catalog(Err(message)) => {
                if self.pending_catalog_refresh {
                    self.pending_catalog_refresh = false;
                    self.apply_action(DesktopAction::Failed(message), cx);
                }
            }
            BridgeReply::UiState(Ok(ui_state)) => {
                self.apply_action(
                    DesktopAction::UiStateLoaded {
                        recents: ui_state.recent_projects,
                        last_project: ui_state.last_project,
                        auto_update: ui_state.auto_update,
                        selected_provider: ui_state.selected_provider,
                        selected_model: ui_state.selected_model,
                        session_projects: ui_state.session_projects,
                    },
                    cx,
                );
            }
            BridgeReply::UiState(Err(_)) => {}
            BridgeReply::UiStateSaved(Ok(())) => {}
            BridgeReply::UiStateSaved(Err(message)) => {
                self.apply_action(DesktopAction::Failed(message), cx);
            }
            BridgeReply::ProjectSet(Ok(())) => {}
            BridgeReply::ProjectSet(Err(message)) => {
                self.apply_action(DesktopAction::Failed(message), cx);
            }
            BridgeReply::UpdateChecked(Ok(None)) => {
                self.apply_action(DesktopAction::UpdateStateChanged(UpdateState::UpToDate), cx);
            }
            BridgeReply::UpdateChecked(Ok(Some(offer))) => {
                self.apply_action(
                    DesktopAction::UpdateStateChanged(UpdateState::Available {
                        version: offer.version,
                        notes_url: offer.notes_url,
                    }),
                    cx,
                );
            }
            BridgeReply::UpdateChecked(Err(message)) => {
                self.apply_action(
                    DesktopAction::UpdateStateChanged(UpdateState::Failed(message)),
                    cx,
                );
            }
            BridgeReply::UpdateDownloaded(Ok(prepared)) => {
                self.apply_action(DesktopAction::UpdateStaged(prepared), cx);
            }
            BridgeReply::UpdateDownloaded(Err(message)) => {
                self.apply_action(
                    DesktopAction::UpdateStateChanged(UpdateState::Failed(message)),
                    cx,
                );
            }
            BridgeReply::Sessions(Err(message))
            | BridgeReply::Created(Err(message))
            | BridgeReply::Conversation(Err(message))
            | BridgeReply::Sent(Err(message))
            | BridgeReply::Settings(Err(message))
            | BridgeReply::SettingsSaved(Err(message))
            | BridgeReply::ProviderKeySaved(Err(message))
            | BridgeReply::ChatStarted(Err(message))
            | BridgeReply::McpTools(Err(message))
            | BridgeReply::RolledBack(Err(message))
            | BridgeReply::Recalled(Err(message))
            | BridgeReply::SessionDeleted(Err(message))
            | BridgeReply::Resources(Err(message))
            | BridgeReply::AskAnswered(Err(message)) => {
                self.apply_action(DesktopAction::Failed(message), cx);
            }
        }
    }

    /// Starts one model turn over the active conversation using the picked
    /// provider/model, falling back to the first enabled provider.
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
        let selected_provider = self.vm.selected_provider.clone();
        let selected_model = self.vm.selected_model.clone();
        let provider = settings
            .providers
            .iter()
            .find(|provider| provider.enabled && Some(&provider.id) == selected_provider.as_ref())
            .or_else(|| settings.providers.iter().find(|provider| provider.enabled));
        let Some(provider) = provider else {
            self.apply_action(
                DesktopAction::Failed(
                    "no enabled provider — add one with its API key in Settings".to_owned(),
                ),
                cx,
            );
            return;
        };
        let model = selected_model
            .filter(|model| provider.models.contains(model))
            .or_else(|| provider.models.first().cloned());
        let Some(model) = model else {
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
                model,
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

    /// Rewinds to just before the user message at `index` and prefills the
    /// composer with its text (修改). The first message has no prior event to
    /// rewind to and is ignored.
    pub(super) fn on_edit_message(&mut self, index: usize, cx: &mut Context<Self>) {
        self.recall_at(index, true, cx);
    }

    /// Rewinds to just before the user message at `index` (撤回).
    pub(super) fn on_recall_message(&mut self, index: usize, cx: &mut Context<Self>) {
        self.recall_at(index, false, cx);
    }

    fn recall_at(&mut self, index: usize, edit: bool, cx: &mut Context<Self>) {
        if self.vm.sending {
            return;
        }
        let Some(conversation) = self.vm.active.clone() else {
            return;
        };
        if index == 0 || index >= conversation.entries.len() {
            return;
        }
        if conversation.entries[index].kind != crate::view_model::EntryKind::UserMessage {
            return;
        }
        let Some(session) = SessionId::parse(&conversation.session_id) else {
            return;
        };
        let Some(branch) = BranchId::parse(&conversation.branch_id) else {
            return;
        };
        let expected_head = parse_head(&conversation.head);
        let to_event = conversation.entries[index - 1].event_id.clone();
        let edit = if edit {
            Some(conversation.entries[index].text.clone())
        } else {
            None
        };
        self.dispatch(
            BridgeCommand::RecallMessage {
                session,
                branch,
                expected_head,
                to_event,
                edit,
            },
            cx,
        );
    }

    /// Deletes one session's durable data and drops it if active.
    pub(super) fn on_delete_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self
            .vm
            .active
            .as_ref()
            .is_some_and(|conversation| conversation.session_id == session_id)
        {
            self.apply_action(DesktopAction::SessionDeleted, cx);
        }
        self.dispatch(
            BridgeCommand::DeleteSession {
                session_id: session_id.to_owned(),
            },
            cx,
        );
    }

    /// Removes one directory from the remembered projects list.
    pub(super) fn on_remove_recent(&mut self, project: &str, cx: &mut Context<Self>) {
        self.dispatch(
            BridgeCommand::RemoveRecent {
                project: project.to_owned(),
            },
            cx,
        );
        self.apply_action(
            DesktopAction::ActiveProjectChanged(
                self.vm
                    .project_dir
                    .clone()
                    .filter(|current| current != project),
            ),
            cx,
        );
    }

    /// Takes the composer prefill restored by edit-and-resend.
    pub(super) fn take_composer_prefill(&mut self) -> Option<String> {
        self.pending_composer_prefill.take()
    }

    pub(super) fn on_new_session(&mut self, cx: &mut Context<Self>) {
        // New chats inherit the active project so the sidebar grouping and
        // the tool working directory follow the project switcher.
        if self.vm.project_dir.is_some() {
            self.pending_project = self.vm.project_dir.clone();
        }
        self.dispatch(BridgeCommand::CreateSession, cx);
    }

    /// Switches the sidebar's active project filter (no session rebinding).
    pub(super) fn on_switch_project(&mut self, project: Option<String>, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ProjectMenuToggled(false), cx);
        self.apply_action(DesktopAction::ActiveProjectChanged(project), cx);
        self.persist_ui_state(cx);
    }

    pub(super) fn on_toggle_project_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ProjectMenuToggled(open), cx);
    }

    pub(super) fn on_show_settings_section(
        &mut self,
        section: crate::view_model::SettingsSection,
        cx: &mut Context<Self>,
    ) {
        self.apply_action(DesktopAction::ShowSettingsSection(section), cx);
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

    /// Switches the main area between chat and settings.
    pub(super) fn on_show_main_view(&mut self, view: MainView, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ShowMainView(view), cx);
    }

    pub(super) fn on_toggle_model_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ModelMenuToggled(open), cx);
    }

    pub(super) fn on_select_provider(&mut self, provider_id: &str, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ProviderSelected(provider_id.to_owned()), cx);
        self.persist_ui_state(cx);
    }

    pub(super) fn on_select_model(&mut self, model_id: &str, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ModelSelected(model_id.to_owned()), cx);
        self.persist_ui_state(cx);
    }

    // ---- project selection ----

    /// Opens the native folder picker and binds the chosen directory.
    /// Exports product data (settings, UI state, todos, session ledgers) to
    /// a file chosen in a save dialog. Secrets never travel.
    pub(super) fn on_export_data(&mut self, cx: &mut Context<Self>) {
        let directory = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        let suggested = format!("mcode-export-{stamp}.json");
        let receiver = cx.prompt_for_new_path(&directory, Some(&suggested));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = receiver.await else {
                return;
            };
            let _ = this.update(cx, |workspace, cx| {
                workspace.dispatch(BridgeCommand::ExportData { path }, cx);
            });
        })
        .detach();
    }

    /// Applies one export bundle chosen in an open dialog. Existing todos and
    /// sessions are never overwritten.
    pub(super) fn on_import_data(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose an MCode export bundle".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.first().cloned() else {
                return;
            };
            let _ = this.update(cx, |workspace, cx| {
                workspace.dispatch(BridgeCommand::ImportData { path }, cx);
            });
        })
        .detach();
    }

    pub(super) fn on_open_project_dialog(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose a project folder for the agent".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(directory) = paths.first() else {
                return;
            };
            let project = directory.to_string_lossy().into_owned();
            let _ = this.update(cx, |workspace, cx| workspace.bind_project(&project, cx));
        })
        .detach();
    }

    /// Opens one of the remembered recent projects.
    pub(super) fn on_open_recent(&mut self, project: &str, cx: &mut Context<Self>) {
        self.bind_project(project, cx);
    }

    /// Binds the project to the active session, creating one when needed.
    fn bind_project(&mut self, project: &str, cx: &mut Context<Self>) {
        if self.vm.active.is_none() {
            self.pending_project = Some(project.to_owned());
            self.dispatch(BridgeCommand::CreateSession, cx);
            return;
        }
        let session_id = self
            .vm
            .active
            .as_ref()
            .map(|conversation| conversation.session_id.clone())
            .expect("active session");
        self.apply_action(DesktopAction::ProjectOpened(project.to_owned()), cx);
        self.dispatch(
            BridgeCommand::SetProjectDir {
                session_id: session_id.clone(),
                path: Some(project.to_owned()),
            },
            cx,
        );
        self.dispatch(
            BridgeCommand::ListResources {
                session_id: session_id.clone(),
            },
            cx,
        );
        self.persist_ui_state(cx);
    }

    /// Persists the durable UI state projection.
    fn persist_ui_state(&self, cx: &mut Context<Self>) {
        let state = mcode_config::UiState {
            recent_projects: self.vm.recents.clone(),
            last_project: self.vm.project_dir.clone(),
            auto_update: self.vm.auto_update,
            selected_provider: self.vm.selected_provider.clone(),
            selected_model: self.vm.selected_model.clone(),
            session_projects: self.vm.session_projects.clone(),
        };
        self.dispatch(BridgeCommand::SaveUiState { state }, cx);
    }

    // ---- catalog ----

    pub(super) fn on_refresh_catalog(&mut self, cx: &mut Context<Self>) {
        self.pending_catalog_refresh = true;
        self.dispatch(BridgeCommand::RefreshCatalog, cx);
    }

    // ---- updates ----

    pub(super) fn on_check_update(&mut self, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::UpdateStateChanged(UpdateState::Checking), cx);
        self.dispatch(BridgeCommand::CheckUpdate, cx);
    }

    pub(super) fn on_download_update(&mut self, cx: &mut Context<Self>) {
        let Some(offer) = self.vm.last_offer.clone() else {
            return;
        };
        self.apply_action(
            DesktopAction::UpdateStateChanged(UpdateState::Downloading {
                version: offer.version.clone(),
            }),
            cx,
        );
        self.dispatch(BridgeCommand::DownloadUpdate { offer }, cx);
    }

    /// Installs the staged update: arm the detached swap, then exit.
    pub(super) fn on_install_update(&mut self, cx: &mut Context<Self>) {
        let Some(prepared) = self.vm.prepared_update.clone() else {
            return;
        };
        if let Err(message) = mcode_updates::apply_and_restart(&prepared) {
            self.apply_action(DesktopAction::Failed(message), cx);
            return;
        }
        cx.quit();
    }

    pub(super) fn on_toggle_auto_update(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::AutoUpdateToggled(enabled), cx);
        self.persist_ui_state(cx);
    }

    // ---- ask / tools ----

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

    pub(super) fn ask_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<InputState> {
        self.ask_input
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder("Type a free-text answer for every question (separate with |)")
                })
            })
            .clone()
    }

    pub(super) fn on_submit_free_ask(&mut self, cx: &mut Context<Workspace>) {
        let Some(input) = self.ask_input.clone() else {
            return;
        };
        let Some(rows) = self.vm.pending_ask.clone() else {
            return;
        };
        let raw = input.read(cx).value().to_string();
        let parts: Vec<String> = raw.split('|').map(|part| part.trim().to_owned()).collect();
        let answers: Vec<String> = rows
            .iter()
            .enumerate()
            .map(|(index, (_, _, _))| parts.get(index).cloned().unwrap_or_default())
            .collect();
        self.on_answer_ask(answers, cx);
    }

    pub(super) fn on_answer_ask(&mut self, answers: Vec<String>, cx: &mut Context<Workspace>) {
        let Some(conversation) = self.vm.active.clone() else {
            return;
        };
        self.apply_action(DesktopAction::AskAnswered, cx);
        self.dispatch(
            BridgeCommand::AskAnswer {
                session_id: conversation.session_id,
                answers,
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

    // ---- provider presets from the catalog ----

    /// The filter input for the provider preset picker.
    pub(super) fn preset_search_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<InputState> {
        if self.preset_search_input.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter providers…"));
            cx.subscribe_in(&input, window, |workspace, entity, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = entity.read(cx).value().to_string();
                    workspace.apply_action(DesktopAction::PresetSearchChanged(text), cx);
                }
            })
            .detach();
            self.preset_search_input = Some(input);
        }
        self.preset_search_input
            .clone()
            .expect("preset search input")
    }

    pub(super) fn on_open_preset(&mut self, provider_id: &str, cx: &mut Context<Self>) {
        self.preset_key_input = None;
        self.apply_action(
            DesktopAction::ActivePresetChanged(Some(provider_id.to_owned())),
            cx,
        );
    }

    pub(super) fn on_close_preset(&mut self, cx: &mut Context<Self>) {
        self.preset_key_input = None;
        self.apply_action(DesktopAction::ActivePresetChanged(None), cx);
    }

    pub(super) fn preset_key_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<InputState> {
        self.preset_key_input
            .get_or_insert_with(|| {
                cx.new(|cx| InputState::new(window, cx).placeholder("paste API key here"))
            })
            .clone()
    }

    /// Adds one catalog preset: provider entry plus stored key, then saves.
    pub(super) fn on_add_preset(&mut self, provider_id: &str, cx: &mut Context<Self>) {
        let Some(catalog) = self.vm.catalog.clone() else {
            return;
        };
        let Some(preset) = catalog.provider(provider_id) else {
            return;
        };
        // Checked models in catalog order; an empty selection falls back to
        // the catalog's first model.
        let checked = self.vm.preset_models.clone();
        let mut models: Vec<String> = preset
            .models
            .iter()
            .filter(|entry| checked.contains(&entry.id))
            .map(|entry| entry.id.clone())
            .collect();
        if models.is_empty() {
            let Some(first) = preset.models.first() else {
                return;
            };
            models.push(first.id.clone());
        }
        let model = models[0].clone();
        // Unique id: the catalog spelling, suffixed when already configured.
        let mut id = preset.id.clone();
        let mut suffix = 2;
        while self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.providers.iter().any(|p| p.id == id))
        {
            id = format!("{}-{suffix}", preset.id);
            suffix += 1;
        }
        let api_key = self
            .preset_key_input
            .clone()
            .map(|input| input.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        // Drop the input entity so the pasted key never lingers on screen.
        self.preset_key_input = None;
        self.apply_action(
            DesktopAction::SettingsProviderAdded(mcode_config::ProviderSettings {
                id: id.clone(),
                kind: preset.kind.clone(),
                base_url: preset.base_url.clone(),
                models,
                enabled: true,
            }),
            cx,
        );
        if !api_key.is_empty() {
            self.dispatch(
                BridgeCommand::SaveProviderKey {
                    provider_id: id.clone(),
                    api_key,
                },
                cx,
            );
        }
        self.apply_action(DesktopAction::ActivePresetChanged(None), cx);
        self.apply_action(DesktopAction::ProviderSelected(id.clone()), cx);
        self.apply_action(DesktopAction::ModelSelected(model), cx);
        self.on_save_settings(cx);
        self.persist_ui_state(cx);
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
