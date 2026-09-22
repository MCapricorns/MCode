//! Reducer and projection tests, kept beside the declarations they exercise.
use super::reduce::{selected_model_supports_reasoning, selected_reasoning_levels};
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
fn switching_models_filters_inspector_usage() {
    let mut state = WorkspaceState {
        selected_model: Some("o3".to_owned()),
        usage_totals: vec![UsageTotal {
            key: "openai/gpt-4.1".to_owned(),
            input: 9,
            output: 1,
            requests: 1,
            ..UsageTotal::default()
        }],
        last_turn: Some(TurnStats {
            model: "gpt-4.1".to_owned(),
            input: 9,
            output: 1,
            cache: None,
            elapsed_ms: 1,
        }),
        ..WorkspaceState::default()
    };
    assert!(!usage_key_matches("openai/gpt-4.1", "o3"));
    reduce(
        &mut state,
        DesktopAction::UsageSnapshot {
            model: "o3".to_owned(),
            input: 20,
            output: 3,
            cache: None,
            elapsed_ms: 4,
        },
    );
    assert_eq!(state.live_turn.as_ref().map(|turn| turn.input), Some(20));
    assert_eq!(
        parse_usage_text("o3: 20 in / 3 out"),
        Some(("o3".to_owned(), 20, 3))
    );
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
fn transcript_start_keeps_a_fixed_tail() {
    assert_eq!(transcript_start(10, 0), 0);
    assert_eq!(transcript_start(TRANSCRIPT_TAIL, 0), 0);
    assert_eq!(transcript_start(TRANSCRIPT_TAIL + 7, 0), 7);
    assert_eq!(transcript_start(TRANSCRIPT_TAIL + 7, TRANSCRIPT_PAGE), 0);
}

#[test]
fn revealing_earlier_messages_resets_when_the_session_changes() {
    let mut state = WorkspaceState::default();
    reduce(&mut state, DesktopAction::TranscriptRevealMore);
    assert_eq!(state.transcript_extra, TRANSCRIPT_PAGE);
    reduce(
        &mut state,
        DesktopAction::SessionCreated(summary("ses1-a", 0)),
    );
    assert_eq!(state.transcript_extra, 0);
    reduce(&mut state, DesktopAction::TranscriptRevealMore);
    reduce(
        &mut state,
        DesktopAction::ConversationOpened(ActiveConversation {
            session_id: "ses1-b".to_owned(),
            branch_id: "br-b".to_owned(),
            head: "empty".to_owned(),
            entries: Vec::new(),
            streaming: None,
        }),
    );
    assert_eq!(state.transcript_extra, 0);
}

#[test]
fn todo_updates_drop_completed_rows() {
    let mut state = WorkspaceState::default();
    reduce(
        &mut state,
        DesktopAction::TodoUpdated(vec![
            ("plan".to_owned(), "in progress".to_owned()),
            ("ship".to_owned(), "done".to_owned()),
            ("review".to_owned(), "pending".to_owned()),
        ]),
    );
    assert_eq!(
        state.todo_rows,
        vec![
            ("plan".to_owned(), "in progress".to_owned()),
            ("review".to_owned(), "pending".to_owned()),
        ]
    );
    reduce(
        &mut state,
        DesktopAction::TodoUpdated(vec![("plan".to_owned(), "done".to_owned())]),
    );
    assert!(state.todo_rows.is_empty());
}

#[test]
fn ask_picks_accumulate_until_submit() {
    let mut state = WorkspaceState::default();
    reduce(
        &mut state,
        DesktopAction::AskRequested(vec![
            ("one?".to_owned(), vec!["a".to_owned()], false),
            ("two?".to_owned(), Vec::new(), true),
        ]),
    );
    assert_eq!(state.ask_answers, vec!["", ""]);
    reduce(
        &mut state,
        DesktopAction::AskChoicePicked {
            index: 0,
            answer: "a".to_owned(),
        },
    );
    assert_eq!(state.ask_answers[0], "a");
    reduce(&mut state, DesktopAction::AskAnswered);
    assert!(state.pending_ask.is_none());
    assert!(state.ask_answers.is_empty());
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
        DesktopAction::CopilotSignInFinished(Err("the request was denied on GitHub".to_owned())),
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
    assert_eq!(state.live_jobs[0].step, "running read");
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
    let mut state = WorkspaceState {
        sessions: vec![summary("ses-a", 0), summary("ses-b", 0)],
        ..WorkspaceState::default()
    };
    let project = if cfg!(windows) {
        r"D:\my_private_pro\MCode"
    } else {
        "/work/MCode"
    };
    let same_project = if cfg!(windows) {
        r"D:\my_private_pro\Mcode"
    } else {
        "/work/MCode"
    };
    reduce(&mut state, DesktopAction::ProjectOpened(project.to_owned()));
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
            project: project.to_owned(),
        },
    );
    let grouped = group_sessions(&state.sessions, &state.session_projects, Some(same_project));
    assert_eq!(grouped.current.len(), 1);
    assert_eq!(grouped.current[0].session_id, "ses-a");
    assert_eq!(grouped.unbound.len(), 1);
}

#[test]
fn unbound_sessions_are_assigned_to_the_active_project() {
    let mut state = WorkspaceState {
        sessions: vec![summary("ses-a", 0), summary("ses-b", 0)],
        ..WorkspaceState::default()
    };
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
    let mut settings =
        SettingsState::from_settings(&mycode_config::AppSettings::default(), 0, Vec::new());
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
