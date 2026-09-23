//! Settings editor, floating menus, and the settings sub-pages.
use super::*;

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

    reduce(
        &mut state,
        DesktopAction::ShowWebSubview(crate::view_model::WebSubview::Custom),
    );
    assert_eq!(state.web_subview, crate::view_model::WebSubview::Custom);
    state.mcp_transport_menu_open = true;
    reduce(
        &mut state,
        DesktopAction::ShowMcpSubview(crate::view_model::McpSubview::Json),
    );
    assert_eq!(state.mcp_subview, crate::view_model::McpSubview::Json);
    assert!(!state.mcp_transport_menu_open);

    reduce(&mut state, DesktopAction::ProviderKindMenuToggled(true));
    assert!(state.provider_kind_menu_open);
    reduce(&mut state, DesktopAction::McpTransportMenuToggled(true));
    assert!(state.mcp_transport_menu_open);
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
