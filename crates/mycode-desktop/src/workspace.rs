//! The workspace window view: activity bar, sessions sidebar, chat, and a
//! full-page settings view.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Root;
use std::collections::HashMap;

use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::{App, AppContext as _, Bounds, Context, Entity, Pixels, Window, WindowBounds};
use gpui_kit::{px, size};
use mycode_app::{BranchId, HeadStamp, SessionEventId, SessionId};
use mycode_config::HomeLayout;

use crate::ui::{BackendForm, McpForm, ProviderForm};
use crate::view_model::{
    CHAT_CANCELLED, DesktopAction, MainView, SettingsState, UpdateState, WorkspaceState, reduce,
};
use mycode_app::{BridgeCommand, BridgeEvent, BridgeReply, CoreBridge};

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
    // Open as a regular window at the default bounds: the maximize-on-open
    // workaround for gpui's stale-scale sizing is retired by user request
    // (DPI edge cases accepted); the user can maximize manually.
    options.window_bounds = Some(WindowBounds::Windowed(WINDOW_BOUNDS));
    options.window_min_size = Some(size(px(960.), px(560.)));
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.title = Some("MYCode".into());
    }
    cx.open_window(options, |window, cx| {
        // Paint the Desk palette before first layout: `init` leaves the stock
        // light theme active until settings load.
        crate::ui::desk::apply(gpui_kit::component::theme::Theme::global_mut(cx));
        let workspace = Workspace::new(bridge, events, window, cx);
        cx.new(|cx| Root::new(workspace, window, cx))
    })
    .expect("open the MYCode window");
}

/// The main workspace view.
pub struct Workspace {
    vm: WorkspaceState,
    bridge: CoreBridge,
    /// Root focus so key events (Escape) reach the workspace node even when
    /// no input holds focus.
    focus_handle: gpui_kit::FocusHandle,
    composer: Entity<TextareaState>,
    ua_input: Option<Entity<InputState>>,
    ua_sync_pending: bool,
    provider_form: Option<Entity<ProviderForm>>,
    backend_form: Option<Entity<BackendForm>>,
    mcp_form: Option<Entity<McpForm>>,
    mcp_key_input: Option<Entity<InputState>>,
    mcp_json_input: Option<Entity<TextareaState>>,
    web_key_inputs: HashMap<String, Entity<InputState>>,
    preset_key_input: Option<Entity<InputState>>,
    preset_search_input: Option<Entity<InputState>>,
    ask_input: Option<Entity<InputState>>,
    pending_project: Option<String>,
    /// Draft restored by 撤回修改, applied on the next render (needs a window).
    pending_composer_prefill: Option<String>,
    /// Last `@` fragment already searched, to dedupe bridge dispatches.
    mention_query: Option<String>,
    pending_catalog_refresh: bool,
    runtime_ticks: u64,
    /// Keeps the conversation column glued to the newest entry while a turn
    /// streams; without it new content grows below the fold.
    conversation_scroll: gpui_kit::ScrollHandle,
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
                .placeholder("Message MYCode…")
                .auto_grow(1, 10)
                .submit_on_enter(true)
        });
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let workspace = cx.new(|_| Self {
            vm: WorkspaceState::default(),
            bridge,
            focus_handle,
            composer,
            ua_input: None,
            ua_sync_pending: false,
            provider_form: None,
            backend_form: None,
            mcp_form: None,
            mcp_key_input: None,
            mcp_json_input: None,
            web_key_inputs: HashMap::new(),
            preset_key_input: None,
            preset_search_input: None,
            ask_input: None,
            pending_project: None,
            pending_composer_prefill: None,
            mention_query: None,
            pending_catalog_refresh: false,
            runtime_ticks: 0,
            conversation_scroll: gpui_kit::ScrollHandle::new(),
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
            BridgeEvent::AssistantStep { session_id, entry } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::AssistantStepCommitted(entry)
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
                self.apply_action(DesktopAction::ChatDone { head, entry }, cx);
                self.pump_queued_send(cx);
                return;
            }
            BridgeEvent::ChatFailed {
                session_id,
                message,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                let cancelled = message == CHAT_CANCELLED;
                self.apply_action(DesktopAction::ChatFailed(message), cx);
                // A user interrupt frees the turn; queued follow-ups start next.
                // Provider errors keep the queue so a failed retry cannot loop.
                if cancelled {
                    self.pump_queued_send(cx);
                }
                return;
            }
            BridgeEvent::Notice {
                session_id,
                message,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::Failed(message)
            }
            BridgeEvent::UsageRecorded {
                session_id,
                provider,
                model,
                input,
                output,
                cache,
                elapsed_ms,
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
                    cache,
                    elapsed_ms,
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
            BridgeEvent::ToolProgress {
                session_id,
                call_id,
                name,
                message,
            } => {
                if !matches_active(&session_id) {
                    return;
                }
                DesktopAction::ToolProgress {
                    call_id,
                    name,
                    message,
                }
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
            BridgeEvent::CopilotSignedIn => {
                // The provider entry and key landed; refresh the settings
                // projection so the row and keyed badge appear immediately.
                self.dispatch(BridgeCommand::LoadSettings, cx);
                DesktopAction::CopilotSignInFinished(Ok(()))
            }
            BridgeEvent::CopilotSignInFailed { message } => {
                DesktopAction::CopilotSignInFinished(Err(message))
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
                self.refresh_mention_search(cx);
            }
            InputEvent::PressEnter { shift: false, .. } => {
                self.on_send(window, cx);
            }
            InputEvent::PressEnter { .. } | InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    pub(super) fn apply_action(&mut self, action: DesktopAction, cx: &mut Context<Self>) {
        let grew = matches!(
            action,
            DesktopAction::MessageSent { .. }
                | DesktopAction::TurnArmed
                | DesktopAction::ChatDelta(_)
                | DesktopAction::ChatThinkingDelta(_)
                | DesktopAction::ToolStarted { .. }
                | DesktopAction::ToolProgress { .. }
                | DesktopAction::ToolResultAppended(_)
                | DesktopAction::ChatDone { .. }
                | DesktopAction::UsageRecorded { .. }
        );
        reduce(&mut self.vm, action);
        // Transcript-growing actions keep the conversation scrolled to the
        // newest content, the way chat clients behave while streaming.
        if self.vm.view == MainView::Chat && grew {
            self.conversation_scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    /// The conversation column's scroll handle.
    pub(super) fn conversation_scroll_handle(&self) -> &gpui_kit::ScrollHandle {
        &self.conversation_scroll
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
                self.bind_unbound_to_active_project(cx);
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
                self.refresh_skills(cx);
            }
            BridgeReply::Resources(Ok(files)) => {
                self.apply_action(DesktopAction::ResourcesLoaded(files), cx);
            }
            BridgeReply::ProjectFiles(Ok(files)) => {
                self.apply_action(DesktopAction::MentionFiles(files), cx);
            }
            // A failed mention search just leaves the menu empty.
            BridgeReply::ProjectFiles(Err(_)) => {}
            BridgeReply::CopilotSignInStarted(Ok(info)) => {
                self.apply_action(
                    DesktopAction::CopilotSignInStarted(crate::view_model::CopilotSignIn {
                        user_code: info.user_code,
                        verification_uri: info.verification_uri,
                    }),
                    cx,
                );
            }
            BridgeReply::CopilotSignInStarted(Err(message)) => {
                self.apply_action(DesktopAction::CopilotSignInFinished(Err(message)), cx);
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
                // Apply the persisted theme only when it differs from the
                // live one: reloads (import, sign-in) must not clobber a
                // runtime toggle that has not been saved yet.
                let mode = if state.theme == "light" {
                    ThemeMode::Light
                } else {
                    ThemeMode::Dark
                };
                if cx.theme().mode != mode {
                    Theme::change(mode, None, cx);
                    crate::ui::desk::apply(Theme::global_mut(cx));
                    Theme::sync_base(cx);
                }
                self.ua_sync_pending = true;
                self.apply_action(DesktopAction::SettingsLoaded(state), cx);
                self.apply_runtime_shell(cx);
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
            BridgeReply::ProviderKeySaved(Ok((provider_keys, mcp_keys))) => {
                // Refresh the key markers in place: reloading settings here
                // would race the concurrently running settings save and wipe
                // the just-added provider (and any other unsaved edits).
                self.apply_action(
                    DesktopAction::ProviderKeySaved {
                        provider_keys,
                        mcp_keys,
                    },
                    cx,
                );
            }
            BridgeReply::ChatStarted(Ok(())) => {}
            // The turn unwinds over the event channel; the reply itself
            // carries no state.
            BridgeReply::ChatCancelled(_) => {}
            BridgeReply::McpTools {
                server_id,
                outcome: Ok(tools),
            } => {
                self.apply_action(DesktopAction::McpToolsListed { server_id, tools }, cx);
            }
            BridgeReply::McpTools {
                server_id,
                outcome: Err(message),
            } => {
                self.apply_action(DesktopAction::McpProbeFailed { server_id, message }, cx);
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
                self.bind_unbound_to_active_project(cx);
                self.refresh_skills(cx);
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
            self.apply_action(
                DesktopAction::Failed("the open session could not be read".to_owned()),
                cx,
            );
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
        // The bridge rebuilds the turn history from the ledger's typed
        // events, so tool_use/tool_result pairing survives replay.
        self.dispatch(
            BridgeCommand::ChatTurn {
                session,
                branch,
                expected_head,
                provider_id: provider.id.clone(),
                model,
            },
            cx,
        );
    }

    fn send(&mut self, draft: String, window: &mut Window, cx: &mut Context<Self>) {
        self.send_text(draft, true, Some(window), cx);
    }

    /// Enqueues a follow-up while a turn is running; the composer stays free
    /// for the next draft. The queue is capped so a stuck turn cannot grow
    /// without bound.
    fn enqueue_follow_up(&mut self, draft: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.vm.queued.len() >= crate::view_model::MAX_QUEUED_MESSAGES {
            return;
        }
        self.apply_action(DesktopAction::MessageQueued(draft), cx);
        self.composer
            .update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();
    }

    /// Starts the next queued follow-up once the in-flight turn is idle.
    /// Does not wipe the composer: the user may already be typing another
    /// message behind the queue.
    fn pump_queued_send(&mut self, cx: &mut Context<Self>) {
        if self.vm.sending || self.vm.queued.is_empty() {
            return;
        }
        let draft = self.vm.queued[0].clone();
        self.apply_action(DesktopAction::QueuedMessageTaken, cx);
        self.send_text(draft, false, None, cx);
    }

    fn send_text(
        &mut self,
        draft: String,
        clear_composer: bool,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if draft.trim().is_empty() {
            return;
        }
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
        self.apply_action(DesktopAction::TurnArmed, cx);
        if clear_composer {
            self.vm.composer_draft.clear();
            if let Some(window) = window {
                self.composer
                    .update(cx, |state, cx| state.set_value("", window, cx));
            }
        }
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

    /// Fires a project-file search when the active `@` fragment changed.
    fn refresh_mention_search(&mut self, cx: &mut Context<Self>) {
        let (kind, fragment) = match self.vm.mention.as_ref() {
            Some(mention) => (mention.kind, mention.fragment.clone()),
            None => {
                self.mention_query = None;
                return;
            }
        };
        if self.mention_query.as_deref() == Some(fragment.as_str()) {
            return;
        }
        self.mention_query = Some(fragment.clone());
        if kind == crate::view_model::MentionKind::Command {
            self.merge_skill_commands(&fragment, cx);
            return;
        }
        if kind != crate::view_model::MentionKind::File {
            return;
        }
        let session_id = self
            .vm
            .active
            .as_ref()
            .map(|conversation| conversation.session_id.clone());
        let Some(session_id) = session_id else {
            return;
        };
        self.dispatch(
            BridgeCommand::SearchProjectFiles {
                session_id,
                query: fragment,
            },
            cx,
        );
    }

    fn skill_roots(&self) -> (std::path::PathBuf, Option<std::path::PathBuf>) {
        let workspace = self
            .vm
            .project_dir
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
        let user_home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(std::path::PathBuf::from);
        (workspace, user_home)
    }

    fn refresh_skills(&mut self, cx: &mut Context<Self>) {
        let (workspace, user_home) = self.skill_roots();
        let skills = mycode_config::discover_skills(&workspace, user_home.as_deref())
            .into_iter()
            .map(|skill| crate::view_model::SkillEntry {
                slug: skill.slug,
                title: skill.title,
                path: skill.path.to_string_lossy().into_owned(),
                global: skill.global,
            })
            .collect();
        self.apply_action(DesktopAction::SkillsLoaded(skills), cx);
    }

    fn merge_skill_commands(&mut self, fragment: &str, cx: &mut Context<Self>) {
        let (workspace, user_home) = self.skill_roots();
        let skills = mycode_config::discover_skills(&workspace, user_home.as_deref());
        if let Some(mention) = self.vm.mention.as_mut() {
            for skill in skills {
                if !skill.slug.starts_with(fragment) {
                    continue;
                }
                let insert = format!("/{}", skill.slug);
                if mention
                    .items
                    .iter()
                    .any(|(existing, _)| existing == &insert)
                {
                    continue;
                }
                mention
                    .items
                    .push((insert, format!("/{} · {}", skill.slug, skill.title)));
            }
        }
        cx.notify();
    }

    /// Accepts one mention row: rewrites the draft (files) or runs the
    /// command (commands), then closes the menu.
    pub(super) fn on_accept_mention(
        &mut self,
        insert: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mention) = self.vm.mention.take() else {
            return;
        };
        self.mention_query = None;
        match mention.kind {
            crate::view_model::MentionKind::File => {
                let mut text = self.vm.composer_draft.clone();
                if let Some(position) = text.rfind('@') {
                    text.replace_range(position.., &format!("@{insert} "));
                }
                self.pending_composer_prefill = Some(text);
                cx.notify();
            }
            crate::view_model::MentionKind::Command => {
                if insert == "/new" {
                    self.on_new_session(cx);
                } else if insert == "/settings" {
                    self.on_show_main_view(crate::view_model::MainView::Settings, cx);
                } else if let Some(slug) = insert.strip_prefix('/') {
                    self.insert_skill_draft(slug, cx);
                }
            }
        }
    }

    pub(super) fn on_refresh_skills(&mut self, cx: &mut Context<Self>) {
        self.refresh_skills(cx);
    }

    pub(super) fn on_use_skill(&mut self, slug: &str, cx: &mut Context<Self>) {
        self.insert_skill_draft(slug, cx);
        self.on_show_main_view(crate::view_model::MainView::Chat, cx);
    }

    fn insert_skill_draft(&mut self, slug: &str, cx: &mut Context<Self>) {
        let (workspace, user_home) = self.skill_roots();
        let Some(skill) = mycode_config::discover_skills(&workspace, user_home.as_deref())
            .into_iter()
            .find(|skill| skill.slug == slug)
        else {
            return;
        };
        let path = skill.path.display();
        self.pending_composer_prefill = Some(format!(
            "/{slug}\n\nFollow the `{title}` skill. Read `{path}` and apply it before continuing.\n",
            title = skill.title
        ));
        cx.notify();
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
            Some(conversation.entries[index].text.to_string())
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

    /// Switches the sidebar's active project and binds the open chat to it.
    pub(super) fn on_switch_project(&mut self, project: Option<String>, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ProjectMenuToggled(false), cx);
        self.apply_action(DesktopAction::ActiveProjectChanged(project.clone()), cx);
        if let Some(project) = project {
            if let Some(session_id) = self.vm.active.as_ref().map(|c| c.session_id.clone()) {
                self.apply_action(
                    DesktopAction::SessionProjectBound {
                        session_id: session_id.clone(),
                        project: project.clone(),
                    },
                    cx,
                );
                self.dispatch(
                    BridgeCommand::SetProjectDir {
                        session_id,
                        path: Some(project.clone()),
                    },
                    cx,
                );
            }
            self.apply_action(DesktopAction::UnboundSessionsAssigned(project), cx);
            self.refresh_skills(cx);
        }
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
        if section == crate::view_model::SettingsSection::Skills {
            self.refresh_skills(cx);
        }
    }

    /// Switches the Models settings sub-page.
    pub(super) fn on_show_models_subview(
        &mut self,
        view: crate::view_model::ModelsSubview,
        cx: &mut Context<Self>,
    ) {
        self.apply_action(DesktopAction::ShowModelsSubview(view), cx);
    }

    pub(super) fn on_toggle_provider_kind_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ProviderKindMenuToggled(open), cx);
    }

    /// Picks the custom provider form's wire protocol.
    pub(super) fn on_select_provider_kind(&mut self, kind: &str, cx: &mut Context<Self>) {
        if let Some(form) = self.provider_form.clone() {
            form.update(cx, |form, _| form.kind = kind.to_owned());
        }
        self.apply_action(DesktopAction::ProviderKindMenuToggled(false), cx);
    }

    pub(super) fn on_toggle_mcp_transport_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::McpTransportMenuToggled(open), cx);
    }

    /// Picks the custom MCP form's transport.
    pub(super) fn on_select_mcp_transport(&mut self, transport: &str, cx: &mut Context<Self>) {
        if let Some(form) = self.mcp_form.clone() {
            form.update(cx, |form, _| form.transport = transport.to_owned());
        }
        self.apply_action(DesktopAction::McpTransportMenuToggled(false), cx);
    }

    pub(super) fn on_open_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if let Some(session_id) = SessionId::parse(session_id) {
            self.dispatch(BridgeCommand::OpenSession(session_id), cx);
        }
    }

    /// Applies and persists the light/dark theme choice: the appearance
    /// setting is marked dirty and saved immediately, mirroring the
    /// reasoning-effort flow.
    pub(super) fn on_select_theme(
        &mut self,
        dark: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.vm.dark_theme == dark {
            return;
        }
        let mode = if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        };
        Theme::change(mode, Some(window), cx);
        // The Desk palette rides on top of the resolved light/dark theme so
        // the day/night toggle keeps working: repaint + push to base layer.
        crate::ui::desk::apply(Theme::global_mut(cx));
        Theme::sync_base(cx);
        self.apply_action(DesktopAction::SettingsThemeSelected(dark), cx);
        self.on_save_settings(cx);
    }

    /// Escape dismisses the topmost floating menu first; with nothing open it
    /// leaves the settings view.
    pub(super) fn on_escape(&mut self, cx: &mut Context<Self>) {
        let mut dismissed = false;
        if self.vm.project_menu_open {
            self.apply_action(DesktopAction::ProjectMenuToggled(false), cx);
            dismissed = true;
        }
        if self.vm.model_menu_open {
            self.apply_action(DesktopAction::ModelMenuToggled(false), cx);
            dismissed = true;
        }
        if self.vm.reasoning_menu_open {
            self.apply_action(DesktopAction::ReasoningMenuToggled(false), cx);
            dismissed = true;
        }
        if self.vm.subagent_menu.is_some() {
            self.apply_action(DesktopAction::SubagentMenuToggled(None), cx);
            dismissed = true;
        }
        if self.vm.preset_model_menu_open {
            self.apply_action(DesktopAction::PresetModelMenuToggled(false), cx);
            dismissed = true;
        }
        if self.vm.provider_kind_menu_open {
            self.apply_action(DesktopAction::ProviderKindMenuToggled(false), cx);
            dismissed = true;
        }
        if self.vm.mcp_transport_menu_open {
            self.apply_action(DesktopAction::McpTransportMenuToggled(false), cx);
            dismissed = true;
        }
        if self.vm.mention.is_some() {
            self.apply_action(DesktopAction::MentionDismissed, cx);
            dismissed = true;
        }
        if !dismissed
            && self.vm.view == MainView::Settings
            && self.vm.settings_section == crate::view_model::SettingsSection::Models
            && self.vm.models_subview != crate::view_model::ModelsSubview::List
        {
            self.apply_action(
                DesktopAction::ShowModelsSubview(crate::view_model::ModelsSubview::List),
                cx,
            );
            dismissed = true;
        }
        if !dismissed && self.vm.view == MainView::Settings {
            self.apply_action(DesktopAction::ShowMainView(MainView::Chat), cx);
        }
        if !dismissed && self.vm.sending {
            self.on_cancel_chat(cx);
        }
    }

    pub(super) fn on_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.composer.read(cx).value().to_string();
        if draft != self.vm.composer_draft {
            self.apply_action(DesktopAction::ComposerChanged(draft.clone()), cx);
        }
        if self.vm.sending {
            if !draft.trim().is_empty() {
                self.enqueue_follow_up(draft, window, cx);
            }
            return;
        }
        if !draft.trim().is_empty() {
            self.send(draft, window, cx);
            return;
        }
        self.pump_queued_send(cx);
    }

    pub(super) fn on_remove_queued(&mut self, index: usize, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::QueuedMessageRemoved(index), cx);
    }

    /// Aborts the in-flight turn; the bridge answers with a `cancelled`
    /// failure event that resets the sending state.
    pub(super) fn on_cancel_chat(&mut self, cx: &mut Context<Self>) {
        let Some(conversation) = self.vm.active.as_ref() else {
            return;
        };
        if !self.vm.sending {
            return;
        }
        self.dispatch(
            BridgeCommand::CancelChat {
                session_id: conversation.session_id.clone(),
            },
            cx,
        );
    }

    /// Switches the main area between chat and settings.
    pub(super) fn on_show_main_view(&mut self, view: MainView, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ShowMainView(view), cx);
        if view == MainView::Settings {
            self.refresh_skills(cx);
        }
    }

    pub(super) fn on_toggle_model_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ModelMenuToggled(open), cx);
    }

    pub(super) fn on_toggle_reasoning_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ReasoningMenuToggled(open), cx);
    }

    pub(super) fn on_select_provider(&mut self, provider_id: &str, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ProviderSelected(provider_id.to_owned()), cx);
        if self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.dirty)
        {
            self.on_save_settings(cx);
        }
        self.persist_ui_state(cx);
    }

    pub(super) fn on_select_model(&mut self, model_id: &str, cx: &mut Context<Self>) {
        // Picking a catalog model the provider row does not carry yet appends
        // it to the row: turn resolution validates against that list, so an
        // unpersisted selection would silently fall back to the first model.
        let mut appended = false;
        if let Some(settings) = self.vm.settings.as_mut() {
            let provider = settings
                .providers
                .iter_mut()
                .find(|provider| Some(&provider.id) == self.vm.selected_provider.as_ref());
            if let Some(provider) = provider {
                let known = provider.models.iter().any(|model| model == model_id);
                if !known && provider.models.len() < mycode_config::MAX_MODELS_PER_PROVIDER {
                    provider.models.push(model_id.to_owned());
                    settings.dirty = true;
                    appended = true;
                }
            }
        }
        if appended {
            self.on_save_settings(cx);
        }
        self.apply_action(DesktopAction::ModelSelected(model_id.to_owned()), cx);
        if self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.dirty)
        {
            self.on_save_settings(cx);
        }
        self.persist_ui_state(cx);
    }

    /// Persists the requested reasoning effort through the settings doc.
    pub(super) fn on_select_reasoning(&mut self, level: &str, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ReasoningMenuToggled(false), cx);
        self.apply_action(DesktopAction::ModelMenuToggled(false), cx);
        if self
            .vm
            .settings
            .as_ref()
            .is_none_or(|settings| settings.saving)
        {
            return;
        }
        let levels = crate::view_model::selected_reasoning_levels(&self.vm);
        let Some(settings) = self.vm.settings.as_mut() else {
            return;
        };
        settings.reasoning = if level == "default" || level.is_empty() {
            None
        } else if levels.iter().any(|item| item == level) {
            Some(level.to_owned())
        } else {
            return;
        };
        settings.dirty = true;
        cx.notify();
        self.on_save_settings(cx);
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
        let suggested = format!("mycode-export-{stamp}.json");
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
            prompt: Some("Choose a MYCode export bundle".into()),
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
        self.apply_action(
            DesktopAction::SessionProjectBound {
                session_id: session_id.clone(),
                project: project.to_owned(),
            },
            cx,
        );
        self.apply_action(DesktopAction::ProjectOpened(project.to_owned()), cx);
        self.apply_action(
            DesktopAction::UnboundSessionsAssigned(project.to_owned()),
            cx,
        );
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
        self.refresh_skills(cx);
        self.persist_ui_state(cx);
    }

    /// Persists the durable UI state projection.
    fn persist_ui_state(&self, cx: &mut Context<Self>) {
        let state = mycode_config::UiState {
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
        if let Err(message) = mycode_app::apply_and_restart(&prepared) {
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

    /// Probes one MCP server as the editor currently shows it (saved or
    /// not) and marks the row as connecting until the reply lands.
    pub(super) fn on_list_mcp_tools(&mut self, server_id: &str, cx: &mut Context<Workspace>) {
        let Some(server) = self
            .vm
            .settings
            .as_ref()
            .and_then(|settings| settings.mcp_servers.iter().find(|s| s.id == server_id))
            .cloned()
        else {
            return;
        };
        self.apply_action(DesktopAction::McpProbeStarted(server_id.to_owned()), cx);
        self.dispatch(
            BridgeCommand::McpListTools {
                server: Box::new(server),
            },
            cx,
        );
    }

    pub(super) fn on_add_builtin_mcp(
        &mut self,
        server: mycode_config::McpServerSettings,
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

    pub(super) fn focus_handle(&self) -> &gpui_kit::FocusHandle {
        &self.focus_handle
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
        let kind = form.read(cx).kind.clone();
        let base_url = form.read(cx).base_url.read(cx).value().trim().to_string();
        let model = form.read(cx).model.read(cx).value().trim().to_string();
        let api_key = form.read(cx).api_key.read(cx).value().trim().to_string();
        let context_limit = parse_token_field(&form.read(cx).context_limit.read(cx).value());
        let max_output = parse_token_field(&form.read(cx).max_output.read(cx).value());
        if id.is_empty() || base_url.is_empty() || model.is_empty() {
            self.apply_action(
                DesktopAction::Failed("fill id, base URL, and model".to_owned()),
                cx,
            );
            return;
        }
        let (context_limit, max_output) = match (context_limit, max_output) {
            (Ok(a), Ok(b)) => (a, b),
            _ => {
                self.apply_action(
                    DesktopAction::Failed(
                        "context window and max output must be plain numbers".to_owned(),
                    ),
                    cx,
                );
                return;
            }
        };
        self.apply_action(
            DesktopAction::SettingsProviderAdded(mycode_config::ProviderSettings {
                id: id.clone(),
                kind,
                base_url,
                models: vec![model],
                enabled: true,
                context_limit,
                max_output,
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
        self.apply_action(
            DesktopAction::ShowModelsSubview(crate::view_model::ModelsSubview::List),
            cx,
        );
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

    /// Starts the GitHub device-flow sign-in with the checked model list.
    pub(super) fn on_start_copilot_sign_in(&mut self, cx: &mut Context<Self>) {
        let models = self.vm.preset_models.clone();
        self.dispatch(BridgeCommand::StartCopilotSignIn { models }, cx);
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
            DesktopAction::SettingsProviderAdded(mycode_config::ProviderSettings {
                id: id.clone(),
                kind: preset.kind.clone(),
                base_url: preset.base_url.clone(),
                models,
                enabled: true,
                context_limit: None,
                max_output: None,
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
            DesktopAction::SettingsBackendAdded(mycode_config::WebBackendSettings {
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
        let transport = form.read(cx).transport.clone();
        let endpoint = form.read(cx).endpoint.read(cx).value().trim().to_string();
        let command = form.read(cx).command.read(cx).value().trim().to_string();
        let env_line = form.read(cx).env.read(cx).value().trim().to_string();
        let api_key = form.read(cx).api_key.read(cx).value().trim().to_string();
        if id.is_empty() {
            self.apply_action(DesktopAction::Failed("fill id".to_owned()), cx);
            return;
        }
        if self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.mcp_servers.iter().any(|server| server.id == id))
        {
            self.apply_action(
                DesktopAction::Failed(format!("an MCP server named '{id}' already exists")),
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
                mycode_config::McpServerSettings {
                    id: id.clone(),
                    enabled: true,
                    transport,
                    command: None,
                    args: Vec::new(),
                    env: Default::default(),
                    endpoint: Some(endpoint),
                    key_header: Some("bearer".to_owned()),
                }
            }
            "stdio" => {
                // The form takes the whole line the server docs publish
                // (`npx -y @scope/server --flag`); split it here so the
                // child gets a program plus argv, not one giant program name.
                let mut words = mycode_config::split_command_line(&command).into_iter();
                let Some(program) = words.next().filter(|word| !word.is_empty()) else {
                    self.apply_action(
                        DesktopAction::Failed("stdio servers need a command".to_owned()),
                        cx,
                    );
                    return;
                };
                let env = match parse_env_line(&env_line) {
                    Ok(env) => env,
                    Err(message) => {
                        self.apply_action(DesktopAction::Failed(message), cx);
                        return;
                    }
                };
                mycode_config::McpServerSettings {
                    id: id.clone(),
                    enabled: true,
                    transport,
                    command: Some(program),
                    args: words.collect(),
                    env,
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

    pub(super) fn web_key_input(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<InputState> {
        self.web_key_inputs
            .entry(id.to_owned())
            .or_insert_with(|| {
                cx.new(|cx| {
                    InputState::new(window, cx).placeholder("paste API key (Bearer is added)")
                })
            })
            .clone()
    }

    pub(super) fn on_save_web_key(&mut self, id: &str, cx: &mut Context<Workspace>) {
        let Some(input) = self.web_key_inputs.get(id).cloned() else {
            return;
        };
        let api_key = mycode_config::normalize_api_key(&input.read(cx).value());
        self.dispatch(
            BridgeCommand::SaveProviderKey {
                provider_id: format!("web-{id}"),
                api_key,
            },
            cx,
        );
    }

    pub(super) fn mcp_json_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<TextareaState> {
        self.mcp_json_input
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .placeholder("Paste mcp.json or a Claude Desktop / Cursor config…")
                        .auto_grow(4, 16)
                })
            })
            .clone()
    }

    pub(super) fn on_import_mcp_json(&mut self, cx: &mut Context<Workspace>) {
        let Some(input) = self.mcp_json_input.clone() else {
            return;
        };
        let raw = input.read(cx).value().to_string();
        let imported = match mycode_config::parse_mcp_import(&raw) {
            Ok(imported) => imported,
            Err(message) => {
                self.apply_action(DesktopAction::Failed(message), cx);
                return;
            }
        };
        let mut existing: Vec<String> = self
            .vm
            .settings
            .as_ref()
            .map(|settings| {
                settings
                    .mcp_servers
                    .iter()
                    .map(|server| server.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        for row in imported {
            if existing.contains(&row.server.id) {
                self.apply_action(
                    DesktopAction::Failed(format!(
                        "an MCP server named '{}' already exists",
                        row.server.id
                    )),
                    cx,
                );
                continue;
            }
            let server_id = row.server.id.clone();
            existing.push(server_id.clone());
            let api_key = row.api_key.clone();
            self.apply_action(DesktopAction::SettingsMcpAdded(row.server), cx);
            if let Some(api_key) = api_key.filter(|key| !key.is_empty()) {
                self.dispatch(
                    BridgeCommand::SaveProviderKey {
                        provider_id: format!("mcp-{server_id}"),
                        api_key,
                    },
                    cx,
                );
            }
        }
    }

    pub(super) fn on_subagent_role_enabled(
        &mut self,
        role: &str,
        enabled: bool,
        cx: &mut Context<Workspace>,
    ) {
        let Some(settings) = self.vm.settings.as_ref() else {
            return;
        };
        let mut next = settings.subagents.clone();
        next.role_mut(role).enabled = enabled;
        self.apply_action(DesktopAction::SettingsSubagentsChanged(next), cx);
    }

    pub(super) fn on_toggle_subagent_menu(
        &mut self,
        role: &str,
        field: &str,
        open: bool,
        cx: &mut Context<Workspace>,
    ) {
        let next = open.then(|| (role.to_owned(), field.to_owned()));
        self.apply_action(DesktopAction::SubagentMenuToggled(next), cx);
    }

    pub(super) fn on_set_subagent_thinking(
        &mut self,
        role: &str,
        thinking: Option<String>,
        cx: &mut Context<Workspace>,
    ) {
        let Some(settings) = self.vm.settings.as_ref() else {
            return;
        };
        let mut next = settings.subagents.clone();
        let entry = next.role_mut(role);
        entry.thinking = thinking.filter(|level| level != "inherit" && level != "default");
        self.apply_action(DesktopAction::SettingsSubagentsChanged(next), cx);
        self.apply_action(DesktopAction::SubagentMenuToggled(None), cx);
    }

    pub(super) fn on_set_subagent_route(
        &mut self,
        role: &str,
        provider: Option<String>,
        model: Option<String>,
        cx: &mut Context<Workspace>,
    ) {
        let Some(settings) = self.vm.settings.as_ref() else {
            return;
        };
        let mut next = settings.subagents.clone();
        let entry = next.role_mut(role);
        if provider.as_deref() == Some("inherit") || model.as_deref() == Some("inherit") {
            entry.provider = None;
            entry.model = None;
        } else if let Some(provider) = provider {
            let model = model.or_else(|| {
                settings
                    .providers
                    .iter()
                    .find(|item| item.id == provider)
                    .and_then(|item| item.models.first().cloned())
            });
            match model {
                Some(model) => {
                    entry.provider = Some(provider);
                    entry.model = Some(model);
                }
                None => {
                    entry.provider = None;
                    entry.model = None;
                }
            }
        } else if let Some(model) = model {
            let fallback = self.vm.selected_provider.clone().or_else(|| {
                settings
                    .providers
                    .iter()
                    .find(|item| item.enabled)
                    .map(|item| item.id.clone())
            });
            match fallback {
                Some(provider) => {
                    entry.provider = Some(provider);
                    entry.model = Some(model);
                }
                None => {
                    entry.provider = None;
                    entry.model = None;
                }
            }
        }
        self.apply_action(DesktopAction::SettingsSubagentsChanged(next), cx);
        self.apply_action(DesktopAction::SubagentMenuToggled(None), cx);
    }

    pub(super) fn on_detect_shell(&mut self, cx: &mut Context<Self>) {
        let Some(detected) = mycode_tools::detect_default_shell() else {
            self.apply_action(
                DesktopAction::Failed(
                    "No usable shell was found. Browse to pwsh, powershell, cmd, or bash."
                        .to_owned(),
                ),
                cx,
            );
            return;
        };
        self.set_shell_preference(
            detected.kind.as_str(),
            &detected.program.to_string_lossy(),
            "auto",
            cx,
        );
        self.on_save_settings(cx);
    }

    pub(super) fn on_browse_shell(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a shell executable".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.first() else {
                return;
            };
            let program = path.to_string_lossy().into_owned();
            let kind = mycode_tools::ShellKind::from_program(path);
            let _ = this.update(cx, |workspace, cx| {
                workspace.set_shell_preference(kind.as_str(), &program, "user", cx);
                workspace.on_save_settings(cx);
            });
        })
        .detach();
    }

    pub(super) fn on_set_shell_kind(&mut self, kind: &str, cx: &mut Context<Self>) {
        let Some(settings) = self.vm.settings.as_ref() else {
            return;
        };
        let current = settings.tools.shell.clone().unwrap_or_default();
        if current.kind == kind && !current.program.is_empty() {
            return;
        }
        if let Some(detected) = mycode_tools::detect_default_shell()
            .filter(|detected| detected.kind.as_str() == kind)
        {
            self.set_shell_preference(kind, &detected.program.to_string_lossy(), "user", cx);
        } else if !current.program.is_empty()
            && mycode_tools::ShellKind::from_program(std::path::Path::new(&current.program)).as_str()
                == kind
        {
            self.set_shell_preference(kind, &current.program, "user", cx);
        } else {
            self.apply_action(
                DesktopAction::Failed(format!(
                    "No {kind} executable was found. Use Browse to pick one."
                )),
                cx,
            );
        }
        self.on_save_settings(cx);
    }

    fn set_shell_preference(&mut self, kind: &str, program: &str, source: &str, cx: &mut Context<Self>) {
        let Some(settings) = self.vm.settings.as_ref() else {
            return;
        };
        let mut tools = settings.tools.clone();
        tools.shell = Some(mycode_config::ShellSettings {
            kind: kind.to_owned(),
            program: program.to_owned(),
            source: source.to_owned(),
        });
        self.apply_action(DesktopAction::SettingsToolsChanged(tools), cx);
        self.apply_runtime_shell(cx);
    }

    fn apply_runtime_shell(&mut self, _cx: &mut Context<Self>) {
        let shell = self.vm.settings.as_ref().and_then(|settings| {
            let configured = settings.tools.shell.as_ref()?;
            let program = configured.program.trim();
            if program.is_empty() {
                return None;
            }
            let kind = mycode_tools::ShellKind::parse(&configured.kind)
                .unwrap_or_else(|| mycode_tools::ShellKind::from_program(std::path::Path::new(program)));
            Some(mycode_tools::DetectedShell {
                kind,
                program: std::path::PathBuf::from(program),
            })
        });
        mycode_tools::set_runtime_shell(shell);
    }

    fn bind_unbound_to_active_project(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.vm.project_dir.clone() else {
            return;
        };
        let before = self.vm.session_projects.len();
        self.apply_action(DesktopAction::UnboundSessionsAssigned(project), cx);
        if self.vm.session_projects.len() != before {
            self.persist_ui_state(cx);
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
        let revision = mycode_config::AuthorityRevision::new(settings.revision)
            .unwrap_or(mycode_config::AuthorityRevision::ABSENT);
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

/// Parses one optional token-count field: empty keeps `None`, a plain number
/// overrides; anything else is an error.
fn parse_token_field(raw: &str) -> Result<Option<u64>, ()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed.parse::<u64>().map(Some).map_err(|_| ())
}

/// Parses the stdio form's environment line: `KEY=VALUE` pairs separated by
/// commas or whitespace, values optionally quoted. Empty input is no env.
fn parse_env_line(line: &str) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut env = std::collections::BTreeMap::new();
    for word in mycode_config::split_command_line(&line.replace(',', " ")) {
        let Some((key, value)) = word.split_once('=') else {
            return Err(format!("env entry '{word}' must look like KEY=VALUE"));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err("env entry has an empty variable name".to_owned());
        }
        env.insert(key.to_owned(), value.to_owned());
    }
    if env.len() > mycode_config::MAX_MCP_ENV_VARS {
        return Err(format!(
            "at most {} env entries per server",
            mycode_config::MAX_MCP_ENV_VARS
        ));
    }
    Ok(env)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_lines_parse_pairs_and_reject_malformed_entries() {
        let env = parse_env_line("A=1, B=two").expect("pairs");
        assert_eq!(env.get("A").map(String::as_str), Some("1"));
        assert_eq!(env.get("B").map(String::as_str), Some("two"));
        assert!(parse_env_line("").expect("empty").is_empty());
        assert!(parse_env_line("NOEQUALS").is_err());
        assert!(parse_env_line("=x").is_err());
        let env = parse_env_line("PATH_X=\"C:\\Program Files\\x\" DEBUG=1").expect("quoted");
        assert_eq!(
            env.get("PATH_X").map(String::as_str),
            Some("C:\\Program Files\\x")
        );
        assert_eq!(env.get("DEBUG").map(String::as_str), Some("1"));
    }
}
