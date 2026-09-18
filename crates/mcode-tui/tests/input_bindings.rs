use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MediaKeyCode};
use mcode_tui::{
    Action, ActionBinding, ActionId, ActionRegistry, AppState, AppView, BackgroundClass,
    ColorCapability, Effect, InputOutcome, Invalidation, KeyPattern, TerminalCapabilities,
    ThemeSelection, Viewport, When, help_lines, pattern_label, reduce, status_key_hints,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

/// Windows terminals report AltGr as `CONTROL | ALT`.
const ALT_GR: KeyModifiers = KeyModifiers::CONTROL.union(KeyModifiers::ALT);

fn char_event(character: char, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(character), modifiers))
}

fn view_with(registry: ActionRegistry) -> AppView {
    AppView::new(Viewport::new(80, 24), TerminalCapabilities::default())
        .with_action_registry(registry)
}

#[test]
fn altgr_printable_characters_are_inserted_by_default() {
    let mut view = view_with(ActionRegistry::default());
    let inputs = [
        ('@', ALT_GR),
        ('€', ALT_GR),
        ('{', ALT_GR),
        ('}', ALT_GR),
        ('Q', ALT_GR.union(KeyModifiers::SHIFT)),
        ('a', KeyModifiers::NONE),
        ('A', KeyModifiers::SHIFT),
    ];

    for (character, modifiers) in inputs {
        let outcome = view.handle_input(&char_event(character, modifiers));
        assert_eq!(
            outcome,
            InputOutcome::Handled(vec![Effect::Redraw(Invalidation::Content)]),
            "character {character:?} with {modifiers:?} was not ordinary text"
        );
        assert!(view.state().input().ends_with(character));
    }
    assert_eq!(view.state().input(), "@€{}QaA");
}

#[test]
fn command_modifier_combinations_are_not_text_input() {
    let mut view = view_with(ActionRegistry::default());

    for modifiers in [
        KeyModifiers::CONTROL,
        KeyModifiers::ALT,
        KeyModifiers::SUPER,
        KeyModifiers::META,
        KeyModifiers::CONTROL.union(KeyModifiers::SHIFT),
    ] {
        assert_eq!(
            view.handle_input(&char_event('x', modifiers)),
            InputOutcome::Ignored,
            "character with {modifiers:?} was treated as text"
        );
    }
    assert_eq!(
        view.handle_input(&char_event('\u{1}', KeyModifiers::NONE)),
        InputOutcome::Ignored,
        "control characters must not be treated as text"
    );
    assert_eq!(view.state().input(), "");
}

#[test]
fn explicit_ctrl_alt_binding_wins_over_text_fallback() {
    // The exact command binding is registered before the text fallback to
    // prove precedence is structural rather than registration order.
    let mut registry = ActionRegistry::new();
    registry.register(ActionBinding::new(
        KeyPattern::exact(KeyCode::Char('q'), ALT_GR),
        ActionId::Quit,
    ));
    registry.register(ActionBinding::new(
        KeyPattern::text(),
        ActionId::InsertCharacter,
    ));
    let mut view = view_with(registry);

    assert_eq!(
        view.handle_input(&char_event('q', ALT_GR)),
        InputOutcome::Handled(vec![Effect::RequestQuit])
    );
    // Extra SHIFT does not partially match the exact binding; the event
    // remains ordinary AltGr text instead.
    assert_eq!(
        view.handle_input(&char_event('q', ALT_GR.union(KeyModifiers::SHIFT))),
        InputOutcome::Handled(vec![Effect::Redraw(Invalidation::Content)])
    );

    // Other AltGr keys on the same layout keep typing as text.
    assert!(view.handle_input(&char_event('@', ALT_GR)).is_handled());
    assert_eq!(view.state().input(), "q@");
}

#[test]
fn default_registry_keeps_builtin_commands_dispatchable() {
    let mut view = view_with(ActionRegistry::default());

    assert_eq!(
        view.handle_input(&Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ))),
        InputOutcome::Handled(vec![Effect::RequestQuit])
    );
    assert_eq!(
        view.handle_input(&Event::Key(KeyEvent::new(
            KeyCode::F(1),
            KeyModifiers::NONE
        ))),
        InputOutcome::Handled(vec![Effect::Redraw(Invalidation::Content)])
    );
}

#[test]
fn default_registry_hints_show_real_bindings() {
    let registry = ActionRegistry::default();

    let state = AppState::default();
    assert_eq!(
        status_key_hints(&registry, &state),
        "F1 Help | Enter Send | Ctrl+C Quit"
    );
    assert_eq!(
        help_lines(&registry, &state),
        vec![
            "Enter     Send input",
            "Backspace Delete previous character",
            "Ctrl+C    Quit",
            "F1        Close help",
        ]
    );
}

#[test]
fn empty_registry_hides_builtin_key_hints() {
    let registry = ActionRegistry::new();

    let state = AppState::default();
    assert_eq!(status_key_hints(&registry, &state), "");
    assert_eq!(
        help_lines(&registry, &state),
        vec![
            "Send input (unbound)",
            "Delete previous character (unbound)",
            "Quit (unbound)",
            "Close help (unbound)",
        ]
    );
}

#[test]
fn rebound_registry_hints_show_new_bindings() {
    let registry = ActionRegistry::new()
        .with_binding(ActionBinding::new(
            KeyPattern::exact(KeyCode::F(10), KeyModifiers::NONE),
            ActionId::ToggleHelp,
        ))
        .with_binding(ActionBinding::new(
            KeyPattern::exact(KeyCode::Char('s'), KeyModifiers::CONTROL),
            ActionId::Submit,
        ))
        .with_binding(ActionBinding::new(
            KeyPattern::exact(KeyCode::Esc, KeyModifiers::NONE),
            ActionId::Quit,
        ));

    let state = AppState::default();

    assert_eq!(
        status_key_hints(&registry, &state),
        "F10 Help | Ctrl+S Send | Esc Quit"
    );
    assert_eq!(
        help_lines(&registry, &state),
        vec![
            "Ctrl+S    Send input",
            "Delete previous character (unbound)",
            "Esc       Quit",
            "F10       Close help",
        ]
    );
}

#[test]
fn rendered_hints_follow_the_injected_registry() {
    // Empty registry: no built-in keys may be advertised anywhere.
    let empty = render_screen(ActionRegistry::new(), TerminalCapabilities::default());
    assert!(empty.contains("Ready"), "status text was lost");
    for stale in ["F1 Help", "Enter Send", "Ctrl+C", "F1 "] {
        assert!(
            !empty.contains(stale),
            "empty registry advertised {stale:?}"
        );
    }
    assert!(empty.contains("(unbound)"));

    // Rebound registry: the status bar shows the new quit binding.
    let rebound = render_screen(
        ActionRegistry::new().with_binding(ActionBinding::new(
            KeyPattern::exact(KeyCode::Esc, KeyModifiers::NONE),
            ActionId::Quit,
        )),
        TerminalCapabilities::default(),
    );
    assert!(rebound.contains("Esc Quit"));
    assert!(!rebound.contains("Ctrl+C"));
}

#[test]
fn shadowed_bindings_are_not_advertised() {
    // The later Enter registration claims the key, so Enter->Submit is dead.
    let mut registry = ActionRegistry::new();
    registry.register(ActionBinding::new(
        KeyPattern::exact(KeyCode::Enter, KeyModifiers::NONE),
        ActionId::Submit,
    ));
    registry.register(ActionBinding::new(
        KeyPattern::exact(KeyCode::Enter, KeyModifiers::NONE),
        ActionId::Quit,
    ));
    let state = AppState::default();

    assert_eq!(status_key_hints(&registry, &state), "Enter Quit");
    assert_eq!(
        help_lines(&registry, &state),
        vec![
            "Send input (unbound)",
            "Delete previous character (unbound)",
            "Enter     Quit",
            "Close help (unbound)",
        ]
    );

    // Dispatch confirms the hint: pressing Enter quits rather than submits.
    let mut view = view_with(registry);
    assert_eq!(
        view.handle_input(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE
        ))),
        InputOutcome::Handled(vec![Effect::RequestQuit])
    );
}

#[test]
fn inactive_conditional_bindings_are_not_advertised() {
    let registry = ActionRegistry::new().with_binding(
        ActionBinding::new(
            KeyPattern::exact(KeyCode::Esc, KeyModifiers::NONE),
            ActionId::Quit,
        )
        .when(When::HELP_VISIBLE),
    );

    // Help hidden: Esc is inactive, so no key hint may be advertised.
    assert_eq!(status_key_hints(&registry, &AppState::default()), "");
    // Help visible: Esc is live and advertised.
    let help_visible = reduce(&AppState::default(), Action::ToggleHelp)
        .into_parts()
        .0;
    assert_eq!(status_key_hints(&registry, &help_visible), "Esc Quit");
}

#[test]
fn pattern_labels_render_every_modifier_and_key_variant() {
    let label = |code, modifiers| pattern_label(&KeyPattern::exact(code, modifiers));

    assert_eq!(
        label(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        ),
        Some("Ctrl+Shift+C".into())
    );
    assert_eq!(
        label(KeyCode::Char('k'), KeyModifiers::SUPER),
        Some("Super+K".into())
    );
    assert_eq!(
        label(KeyCode::Char('k'), KeyModifiers::HYPER),
        Some("Hyper+K".into())
    );
    assert_eq!(
        label(KeyCode::Char('k'), KeyModifiers::META),
        Some("Meta+K".into())
    );
    assert_eq!(
        label(KeyCode::CapsLock, KeyModifiers::NONE),
        Some("CapsLock".into())
    );
    assert_eq!(
        label(KeyCode::KeypadBegin, KeyModifiers::NONE),
        Some("KeypadBegin".into())
    );
    assert_eq!(
        label(KeyCode::Null, KeyModifiers::NONE),
        Some("Null".into())
    );
    assert_eq!(
        label(KeyCode::Media(MediaKeyCode::Play), KeyModifiers::NONE),
        Some("Media(Play)".into())
    );
    assert_eq!(pattern_label(&KeyPattern::text()), None);
}

#[test]
fn replacing_the_registry_requests_a_redraw() {
    let mut view = view_with(ActionRegistry::new());
    assert_eq!(view.take_invalidation(), Some(Invalidation::Layout));
    assert_eq!(view.invalidation(), None);

    view.set_action_registry(ActionRegistry::default());
    assert_eq!(view.invalidation(), Some(Invalidation::Content));
}

#[test]
fn detected_background_change_requests_a_redraw_under_explicit_theme() {
    // A custom When predicate may read the detected background, so live
    // bindings and their hints change even though the explicit theme
    // selection keeps theme resolution fixed.
    fn background_is_light(state: &AppState) -> bool {
        state.detected_background() == Some(BackgroundClass::Light)
    }
    let registry = ActionRegistry::new().with_binding(
        ActionBinding::new(
            KeyPattern::exact(KeyCode::Esc, KeyModifiers::NONE),
            ActionId::Quit,
        )
        .when(When::new("background_light", background_is_light)),
    );

    let mut view = view_with(registry);
    view.dispatch(Action::SelectTheme(ThemeSelection::Dark));
    assert_eq!(view.take_invalidation(), Some(Invalidation::Theme));
    assert_eq!(view.invalidation(), None);

    // Before detection the conditional binding is not live, so no key is
    // advertised.
    assert_eq!(status_key_hints(view.action_registry(), view.state()), "");

    // Detection must invalidate the view even though the explicit theme
    // resolution does not change: hosts repainting from invalidation would
    // otherwise keep showing the stale hint.
    assert_eq!(
        view.dispatch(Action::DetectBackground(Some(BackgroundClass::Light))),
        vec![Effect::Redraw(Invalidation::Content)]
    );
    assert_eq!(view.invalidation(), Some(Invalidation::Content));
    assert_eq!(
        status_key_hints(view.action_registry(), view.state()),
        "Esc Quit"
    );

    // An unchanged detection stays effect-free.
    assert_eq!(view.take_invalidation(), Some(Invalidation::Content));
    assert!(
        view.dispatch(Action::DetectBackground(Some(BackgroundClass::Light)))
            .is_empty()
    );
    assert_eq!(view.invalidation(), None);
}

#[test]
fn automatic_theme_detection_repaints_the_theme() {
    let mut view = view_with(ActionRegistry::default());
    assert_eq!(view.take_invalidation(), Some(Invalidation::Layout));

    assert_eq!(
        view.dispatch(Action::DetectBackground(Some(BackgroundClass::Light))),
        vec![Effect::Redraw(Invalidation::Theme)]
    );
    assert_eq!(view.take_invalidation(), Some(Invalidation::Theme));
}

#[test]
fn help_panel_keeps_ascii_contract_for_non_ascii_keys() {
    let registry = ActionRegistry::new().with_binding(ActionBinding::new(
        KeyPattern::exact(KeyCode::Char('é'), KeyModifiers::NONE),
        ActionId::Submit,
    ));
    let ascii_only = TerminalCapabilities::new(ColorCapability::NoColor, false);

    let text = render_screen(registry, ascii_only);
    assert!(
        text.is_ascii(),
        "non-ASCII key label leaked into the buffer"
    );
    assert!(text.contains("? Send"), "degraded key label was lost");
}

fn render_screen(registry: ActionRegistry, capabilities: TerminalCapabilities) -> String {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal must initialize");
    let mut view = AppView::new(Viewport::new(80, 24), capabilities).with_action_registry(registry);
    view.dispatch(Action::ToggleHelp);
    terminal
        .draw(|frame| view.draw(frame))
        .expect("view must render");

    let mut text = String::new();
    let buffer: &Buffer = terminal.backend().buffer();
    for y in buffer.area.y..buffer.area.bottom() {
        for x in buffer.area.x..buffer.area.right() {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}
