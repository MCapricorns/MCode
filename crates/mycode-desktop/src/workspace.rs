//! The workspace window view: title bar, tape strip, sessions sidebar, chat
//! column, right inspector, and the full-page settings view.
use std::collections::HashMap;

use gpui_kit::component::Root;
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::{App, AppContext as _, Bounds, Context, Entity, Pixels, Window, WindowBounds};
use gpui_kit::{px, size};
use mycode_app::{HeadStamp, SessionEventId, SessionId};
use mycode_config::HomeLayout;

use crate::ui::{BackendForm, McpForm, ProviderForm};
use crate::view_model::{DesktopAction, MainView, WorkspaceState, reduce};
use mycode_app::{BridgeCommand, BridgeEvent, CoreBridge};

mod bridge;
mod projects;
mod settings_editor;
mod updates_data;

/// Window chrome bounds for the first window.
const WINDOW_BOUNDS: Bounds<Pixels> = Bounds {
    origin: gpui_kit::point(px(80.), px(48.)),
    size: size(px(1520.), px(960.)),
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
    /// In-app folder browser. `None` while the native dialog is not used.
    pub(crate) project_picker: Option<crate::ui::project_picker::ProjectPicker>,
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
            project_picker: None,
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

    pub(crate) fn apply_action(&mut self, action: DesktopAction, cx: &mut Context<Self>) {
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
    pub(crate) fn conversation_scroll_handle(&self) -> &gpui_kit::ScrollHandle {
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

    // ---- thin nav / toggle dispatchers ----

    pub(crate) fn on_show_settings_section(
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
    pub(crate) fn on_show_models_subview(
        &mut self,
        view: crate::view_model::ModelsSubview,
        cx: &mut Context<Self>,
    ) {
        self.apply_action(DesktopAction::ShowModelsSubview(view), cx);
    }

    pub(crate) fn on_toggle_provider_kind_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ProviderKindMenuToggled(open), cx);
    }

    /// Picks the custom provider form's wire protocol.
    pub(crate) fn on_select_provider_kind(&mut self, kind: &str, cx: &mut Context<Self>) {
        if let Some(form) = self.provider_form.clone() {
            form.update(cx, |form, _| form.kind = kind.to_owned());
        }
        self.apply_action(DesktopAction::ProviderKindMenuToggled(false), cx);
    }

    pub(crate) fn on_toggle_mcp_transport_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::McpTransportMenuToggled(open), cx);
    }

    /// Picks the custom MCP form's transport.
    pub(crate) fn on_select_mcp_transport(&mut self, transport: &str, cx: &mut Context<Self>) {
        if let Some(form) = self.mcp_form.clone() {
            form.update(cx, |form, _| form.transport = transport.to_owned());
        }
        self.apply_action(DesktopAction::McpTransportMenuToggled(false), cx);
    }

    pub(crate) fn on_open_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        self.follow_session_project(session_id, cx);
        if let Some(session_id) = SessionId::parse(session_id) {
            self.dispatch(BridgeCommand::OpenSession(session_id), cx);
        }
    }

    /// Applies and persists the light/dark theme choice: the appearance
    /// setting is marked dirty and saved immediately, mirroring the
    /// reasoning-effort flow.
    pub(crate) fn on_select_theme(
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

    pub(crate) fn on_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(crate) fn on_remove_queued(&mut self, index: usize, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.apply_action(DesktopAction::QueuedMessageRemoved(index), cx);
    }

    /// Stops the in-flight turn and sends one queued follow-up immediately.
    pub(crate) fn on_open_subagent(&mut self, call_id: &str, cx: &mut Context<Self>) {
        let next = if call_id.is_empty() || self.vm.subagent_window.as_deref() == Some(call_id) {
            None
        } else {
            Some(call_id.to_owned())
        };
        self.apply_action(DesktopAction::SubagentWindowChanged(next), cx);
    }

    pub(crate) fn on_interrupt_queued(&mut self, index: usize, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.apply_action(DesktopAction::QueuedMessagePromoted(index), cx);
        if self.vm.sending {
            self.on_cancel_chat(cx);
            return;
        }
        self.pump_queued_send(cx);
    }

    /// Aborts the in-flight turn; the bridge answers with a `cancelled`
    /// failure event that resets the sending state.
    pub(crate) fn on_cancel_chat(&mut self, cx: &mut Context<Self>) {
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
    pub(crate) fn on_show_main_view(&mut self, view: MainView, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ShowMainView(view), cx);
        if view == MainView::Settings {
            self.refresh_skills(cx);
        }
    }

    pub(crate) fn on_toggle_model_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ModelMenuToggled(open), cx);
    }

    pub(crate) fn on_toggle_shell_kind_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ShellKindMenuToggled(open), cx);
    }

    pub(crate) fn on_select_provider(&mut self, provider_id: &str, cx: &mut Context<Self>) {
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

    /// Picks one model of the selected provider. The reducer appends an
    /// unknown model to the provider row (so the next turn can use it) or
    /// refuses the pick at the per-provider cap.
    pub(crate) fn on_select_model_on(
        &mut self,
        provider_id: &str,
        model_id: &str,
        cx: &mut Context<Self>,
    ) {
        if self.vm.selected_provider.as_deref() != Some(provider_id) {
            self.on_select_provider(provider_id, cx);
        }
        self.on_select_model(model_id, cx);
    }

    pub(crate) fn on_select_model(&mut self, model_id: &str, cx: &mut Context<Self>) {
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
    pub(crate) fn on_select_reasoning(&mut self, level: &str, cx: &mut Context<Self>) {
        self.apply_action(
            DesktopAction::SettingsReasoningChanged(level.to_owned()),
            cx,
        );
        self.on_save_settings(cx);
    }

    pub(crate) fn on_reveal_transcript(&mut self, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::TranscriptRevealMore, cx);
    }

    pub(crate) fn on_dismiss_error(&mut self, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::DismissError, cx);
    }

    // ---- accessors for the render layer ----

    pub(crate) fn vm(&self) -> &WorkspaceState {
        &self.vm
    }

    pub(crate) fn focus_handle(&self) -> &gpui_kit::FocusHandle {
        &self.focus_handle
    }

    pub(crate) fn composer(&self) -> &Entity<TextareaState> {
        &self.composer
    }

    /// Takes the composer prefill restored by edit-and-resend.
    pub(crate) fn take_composer_prefill(&mut self) -> Option<String> {
        self.pending_composer_prefill.take()
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
pub(crate) fn parse_env_line(
    line: &str,
) -> Result<std::collections::BTreeMap<String, String>, String> {
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
    pub(crate) fn render_root(
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
