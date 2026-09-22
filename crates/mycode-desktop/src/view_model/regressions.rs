//! Regression proofs for the reducer fixes: one test per fixed bug, named
//! after the behavior it pins.
use mycode_app::MAX_STREAMING_CHARS;

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

fn provider(id: &str, model: &str) -> mycode_config::ProviderSettings {
    mycode_config::ProviderSettings {
        id: id.to_owned(),
        kind: "openai-completions".to_owned(),
        base_url: format!("https://api.{id}.dev/v1"),
        models: vec![model.to_owned()],
        enabled: true,
        context_limit: None,
        max_output: None,
    }
}

/// Disabling the selected provider through its settings Switch used to skip
/// `ensure_model_selection`, leaving the picker pointed at a dead provider.
#[test]
fn disabling_the_selected_provider_re_ensures_the_selection() {
    let mut state = WorkspaceState::default();
    let mut settings =
        SettingsState::from_settings(&mycode_config::AppSettings::default(), 0, Vec::new());
    settings.providers.push(provider("acme", "m1"));
    settings.providers.push(provider("other", "o1"));
    reduce(&mut state, DesktopAction::SettingsLoaded(settings));
    assert_eq!(state.selected_provider.as_deref(), Some("acme"));
    assert_eq!(state.selected_model.as_deref(), Some("m1"));

    reduce(&mut state, DesktopAction::SettingsProviderToggled(0, false));
    assert_eq!(state.selected_provider.as_deref(), Some("other"));
    assert_eq!(state.selected_model.as_deref(), Some("o1"));
}

/// Picking a catalog model the provider row does not carry while the row sits
/// at the per-provider cap used to store an unusable selection silently.
#[test]
fn selecting_a_model_at_the_provider_cap_is_refused() {
    let mut state = WorkspaceState::default();
    let mut settings =
        SettingsState::from_settings(&mycode_config::AppSettings::default(), 0, Vec::new());
    let full: Vec<String> = (0..mycode_config::MAX_MODELS_PER_PROVIDER)
        .map(|index| format!("m{index}"))
        .collect();
    settings.providers.push(mycode_config::ProviderSettings {
        models: full,
        ..provider("acme", "m0")
    });
    reduce(&mut state, DesktopAction::SettingsLoaded(settings));
    assert_eq!(state.selected_model.as_deref(), Some("m0"));

    reduce(
        &mut state,
        DesktopAction::ModelSelected("brand-new-model".to_owned()),
    );
    assert_eq!(state.selected_model.as_deref(), Some("m0"), "pick refused");
    assert!(
        state
            .error
            .as_deref()
            .is_some_and(|message| message.contains("limit")),
        "the refusal surfaces a reason: {:?}",
        state.error
    );
    let settings = state.settings.as_ref().expect("settings");
    assert_eq!(
        settings.providers[0].models.len(),
        mycode_config::MAX_MODELS_PER_PROVIDER,
        "the row is not grown past the cap"
    );

    // An already-listed model still selects cleanly at the cap.
    state.error = None;
    reduce(&mut state, DesktopAction::ModelSelected("m1".to_owned()));
    assert_eq!(state.selected_model.as_deref(), Some("m1"));
    assert_eq!(state.error, None);

    // Below the cap, an unknown model joins the row and selects.
    let mut state = WorkspaceState::default();
    let mut settings =
        SettingsState::from_settings(&mycode_config::AppSettings::default(), 0, Vec::new());
    settings.providers.push(provider("acme", "m1"));
    reduce(&mut state, DesktopAction::SettingsLoaded(settings));
    reduce(&mut state, DesktopAction::ModelSelected("m2".to_owned()));
    assert_eq!(state.selected_model.as_deref(), Some("m2"));
    assert!(
        state.settings.as_ref().expect("settings").providers[0]
            .models
            .contains(&"m2".to_owned()),
        "an unknown model under the cap joins the row"
    );
}

/// The bulk assignment used to reverse its binding block relative to the
/// documented most-recent-first invariant.
#[test]
fn unbound_sessions_bind_most_recent_first() {
    let mut state = WorkspaceState {
        sessions: vec![
            summary("ses-new", 3),
            summary("ses-mid", 2),
            summary("ses-old", 1),
        ],
        ..WorkspaceState::default()
    };
    reduce(
        &mut state,
        DesktopAction::UnboundSessionsAssigned(r"D:\proj".to_owned()),
    );
    assert_eq!(
        state.session_projects,
        vec![
            ("ses-new".to_owned(), r"D:\proj".to_owned()),
            ("ses-mid".to_owned(), r"D:\proj".to_owned()),
            ("ses-old".to_owned(), r"D:\proj".to_owned()),
        ]
    );

    // A later assignment only tops up rows that are still unbound.
    state.sessions.push(summary("ses-late", 4));
    reduce(
        &mut state,
        DesktopAction::UnboundSessionsAssigned(r"D:\proj".to_owned()),
    );
    assert_eq!(
        state.session_projects[0],
        ("ses-late".to_owned(), r"D:\proj".to_owned())
    );
    assert_eq!(state.session_projects.len(), 4);
}

/// `MessageSent` used to leave a typed `@`/`/` mention mounted over an empty
/// draft, unlike its sibling `MessageQueued`.
#[test]
fn message_send_clears_the_mention_layer() {
    let mut state = WorkspaceState::default();
    reduce(
        &mut state,
        DesktopAction::SessionCreated(summary("ses1-a", 0)),
    );
    reduce(
        &mut state,
        DesktopAction::ComposerChanged("@src".to_owned()),
    );
    assert!(state.mention.is_some());
    reduce(
        &mut state,
        DesktopAction::MessageSent {
            head: "evt1-1".to_owned(),
            entry: ConversationEntry {
                event_id: "evt1-1".to_owned(),
                kind: EntryKind::UserMessage,
                text: "@src".into(),
                call_id: None,
                thinking: String::new(),
            },
        },
    );
    assert!(state.mention.is_none(), "a sent draft closes the menu");
}

/// The streaming buffer is bounded by characters, and a full buffer ignores
/// later deltas without re-counting per character.
#[test]
fn streaming_buffers_stop_at_the_character_cap() {
    let mut state = opened_conversation();
    let big = "x".repeat(MAX_STREAMING_CHARS + 1_000);
    reduce(&mut state, DesktopAction::ChatDelta(big));
    let streaming = state
        .active
        .as_ref()
        .expect("conversation")
        .streaming
        .as_ref()
        .expect("streaming");
    assert_eq!(streaming.text.chars().count(), MAX_STREAMING_CHARS);

    // A later delta on a full buffer adds nothing.
    reduce(&mut state, DesktopAction::ChatDelta("tail".to_owned()));
    assert_eq!(
        state
            .active
            .as_ref()
            .and_then(|conversation| conversation.streaming.as_ref())
            .map(|streaming| streaming.text.chars().count())
            .unwrap_or_default(),
        MAX_STREAMING_CHARS
    );
}

/// The thinking-effort pick is a reducer transition, clamped to what the
/// catalog advertises.
#[test]
fn reasoning_picks_route_through_the_reducer() {
    let mut state = WorkspaceState {
        selected_provider: Some("acme".to_owned()),
        selected_model: Some("efforts".to_owned()),
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
            models: vec![mycode_providers::catalog::CatalogModel {
                id: "efforts".to_owned(),
                reasoning: true,
                reasoning_efforts: vec!["low".to_owned(), "high".to_owned()],
                ..mycode_providers::catalog::CatalogModel::default()
            }],
        });
    state.catalog = Some(std::sync::Arc::new(document));
    let mut settings =
        SettingsState::from_settings(&mycode_config::AppSettings::default(), 0, Vec::new());
    // The provider row must exist and stay enabled, or the reducer's
    // selection guard resets the picker before the pick is validated.
    settings.providers.push(provider("acme", "efforts"));
    reduce(&mut state, DesktopAction::SettingsLoaded(settings));

    reduce(
        &mut state,
        DesktopAction::SettingsReasoningChanged("high".to_owned()),
    );
    assert_eq!(
        state
            .settings
            .as_ref()
            .and_then(|settings| settings.reasoning.clone())
            .as_deref(),
        Some("high")
    );
    assert!(!state.reasoning_menu_open);
    assert!(!state.model_menu_open);

    // "default" clears the override; an unadvertised level is ignored.
    reduce(
        &mut state,
        DesktopAction::SettingsReasoningChanged("default".to_owned()),
    );
    assert_eq!(
        state
            .settings
            .as_ref()
            .and_then(|settings| settings.reasoning.clone()),
        None
    );
    reduce(
        &mut state,
        DesktopAction::SettingsReasoningChanged("ultra".to_owned()),
    );
    assert_eq!(
        state
            .settings
            .as_ref()
            .and_then(|settings| settings.reasoning.clone()),
        None,
        "levels the catalog does not offer are refused"
    );
}

#[test]
fn task_cards_hide_when_the_open_chat_is_not_in_this_project() {
    let mut state = WorkspaceState {
        project_dir: Some(r"D:\work\alpha".to_owned()),
        session_projects: vec![("ses-1".to_owned(), r"D:\work\beta".to_owned())],
        active: Some(ActiveConversation {
            session_id: "ses-1".to_owned(),
            branch_id: "branch".to_owned(),
            head: "empty".to_owned(),
            entries: Vec::new(),
            streaming: None,
        }),
        todo_rows: vec![("plan".to_owned(), "pending".to_owned())],
        ..WorkspaceState::default()
    };
    assert!(!super::task_surface_visible(&state));

    state.project_dir = Some(r"D:\work\beta".to_owned());
    assert!(super::task_surface_visible(&state));
}

#[test]
fn removing_a_recent_project_drops_it_from_the_list() {
    let mut state = WorkspaceState {
        recents: vec![r"D:\work\alpha".to_owned(), r"D:\work\beta".to_owned()],
        project_dir: Some(r"D:\work\alpha".to_owned()),
        ..WorkspaceState::default()
    };
    reduce(
        &mut state,
        DesktopAction::RecentRemoved(r"d:/work/alpha".to_owned()),
    );
    assert_eq!(state.recents, vec![r"D:\work\beta".to_owned()]);
    assert_eq!(state.project_dir, None);
}

#[test]
fn suggested_models_prefer_o3_over_compact_ids() {
    let suggested = super::suggested_model_ids([
        "gpt-4o-mini".to_owned(),
        "o3".to_owned(),
        "gpt-5-nano".to_owned(),
        "o3-pro".to_owned(),
        "claude-haiku".to_owned(),
    ]);
    assert_eq!(suggested, vec!["o3-pro".to_owned(), "o3".to_owned()]);
}

/// The shell-kind dropdown owns its own open flag instead of borrowing the
/// Agents page's subagent menu state.
#[test]
fn shell_kind_menu_toggles_independently() {
    let mut state = WorkspaceState::default();
    reduce(&mut state, DesktopAction::ShellKindMenuToggled(true));
    assert!(state.shell_kind_menu_open);
    assert_eq!(state.subagent_menu, None);
    reduce(
        &mut state,
        DesktopAction::SubagentMenuToggled(Some(("scout".to_owned(), "model".to_owned()))),
    );
    assert!(state.shell_kind_menu_open, "the two menus are independent");
    reduce(&mut state, DesktopAction::ShellKindMenuToggled(false));
    assert!(!state.shell_kind_menu_open);
    assert!(state.subagent_menu.is_some());
}
