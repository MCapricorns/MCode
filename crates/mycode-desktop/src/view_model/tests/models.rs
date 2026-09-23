//! Model picker, reasoning menus, and the preset form.
use super::*;

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
