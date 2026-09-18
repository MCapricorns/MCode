use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use mcode_tui::{
    Action, ActionBinding, ActionId, ActionRegistry, AppView, ColorCapability, Effect,
    INTERACTION_MAX_BODY_COLUMNS, INTERACTION_MAX_BODY_LINES, INTERACTION_MAX_OPTION_ID_CHARS,
    INTERACTION_MAX_OPTIONS, INTERACTION_MAX_REQUEST_ID_CHARS, INTERACTION_MAX_TITLE_COLUMNS,
    InputOutcome, InteractionOption, InteractionPrompt, InteractionResponse, Invalidation,
    KeyPattern, TerminalCapabilities, Viewport, When,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

fn default_view() -> AppView {
    AppView::new(
        Viewport::new(80, 24),
        TerminalCapabilities::new(ColorCapability::TrueColor, true),
    )
}

fn option(id: &str, label: &str) -> InteractionOption {
    InteractionOption::new(id, label).expect("test option identifiers must be valid")
}

fn prompt(
    request_id: impl Into<String>,
    title: impl Into<String>,
    body: impl Into<String>,
    options: impl IntoIterator<Item = InteractionOption>,
) -> InteractionPrompt {
    InteractionPrompt::new(request_id, title, body, options)
        .expect("test request identifiers must be valid")
}

fn sample_prompt(request_id: &str) -> InteractionPrompt {
    prompt(
        request_id,
        "Continue?",
        "Run the next step",
        [option("yes", "Continue"), option("no", "Skip")],
    )
}

fn buffer_text(buffer: &Buffer) -> String {
    let mut output = String::new();
    for y in buffer.area.y..buffer.area.bottom() {
        for x in buffer.area.x..buffer.area.right() {
            output.push_str(buffer[(x, y)].symbol());
        }
        output.push('\n');
    }
    output
}

fn render_view(view: &mut AppView, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal must initialize");
    terminal
        .draw(|frame| view.draw(frame))
        .expect("interaction view must render");
    buffer_text(terminal.backend().buffer())
}

#[test]
fn present_select_and_escape_cancel_the_active_modal() {
    let mut view = default_view();
    assert_eq!(
        view.dispatch(Action::PresentInteraction(sample_prompt("req-1"))),
        vec![Effect::Redraw(Invalidation::Layout)]
    );
    assert_eq!(
        view.state()
            .interaction()
            .map(InteractionPrompt::request_id),
        Some("req-1")
    );

    let selected = view.handle_input(&Event::Key(KeyEvent::new(
        KeyCode::Char('1'),
        KeyModifiers::NONE,
    )));
    assert_eq!(
        selected,
        InputOutcome::Handled(vec![
            Effect::InteractionResolved {
                request_id: "req-1".into(),
                response: InteractionResponse::Selected("yes".into()),
            },
            Effect::Redraw(Invalidation::Layout),
        ])
    );
    assert!(view.state().interaction().is_none());

    view.dispatch(Action::PresentInteraction(sample_prompt("req-2")));
    let cancelled = view.handle_input(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert_eq!(
        cancelled,
        InputOutcome::Handled(vec![
            Effect::InteractionResolved {
                request_id: "req-2".into(),
                response: InteractionResponse::Cancelled,
            },
            Effect::Redraw(Invalidation::Layout),
        ])
    );
    assert!(view.state().interaction().is_none());
}

#[test]
fn quit_cancels_the_active_modal_before_requesting_shutdown() {
    let mut view = default_view();
    view.dispatch(Action::PresentInteraction(sample_prompt("req-quit")));

    let outcome = view.handle_input(&Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )));

    assert_eq!(
        outcome,
        InputOutcome::Handled(vec![
            Effect::InteractionResolved {
                request_id: "req-quit".into(),
                response: InteractionResponse::Cancelled,
            },
            Effect::Redraw(Invalidation::Layout),
            Effect::RequestQuit,
        ])
    );
    assert!(view.state().interaction().is_none());
}

#[test]
fn second_request_is_cancelled_without_replacing_the_first() {
    let mut view = default_view();
    view.dispatch(Action::PresentInteraction(sample_prompt("req-a")));
    let effects = view.dispatch(Action::PresentInteraction(sample_prompt("req-b")));
    assert_eq!(
        view.state()
            .interaction()
            .map(InteractionPrompt::request_id),
        Some("req-a")
    );
    assert_eq!(
        effects,
        vec![Effect::InteractionResolved {
            request_id: "req-b".into(),
            response: InteractionResponse::Cancelled,
        }]
    );
}

#[test]
fn resolution_is_emitted_exactly_once() {
    let mut view = default_view();
    view.dispatch(Action::PresentInteraction(sample_prompt("req-once")));
    let first = view.dispatch(Action::SelectInteractionOption('1'));
    assert_eq!(
        first,
        vec![
            Effect::InteractionResolved {
                request_id: "req-once".into(),
                response: InteractionResponse::Selected("yes".into()),
            },
            Effect::Redraw(Invalidation::Layout),
        ]
    );
    assert!(
        view.dispatch(Action::SelectInteractionOption('1'))
            .is_empty()
    );
    assert!(view.dispatch(Action::CancelInteraction).is_empty());
    assert!(
        view.dispatch(Action::SelectInteractionOption('2'))
            .is_empty()
    );
}

#[test]
fn paste_and_text_reach_the_composer_only_when_the_modal_is_hidden() {
    let mut view = default_view();
    view.dispatch(Action::Paste("before".into()));
    view.dispatch(Action::PresentInteraction(sample_prompt("req-focus")));

    assert_eq!(
        view.handle_input(&Event::Paste("secret\nlines".into())),
        InputOutcome::Ignored
    );
    assert_eq!(
        view.handle_input(&Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        ))),
        InputOutcome::Ignored
    );
    assert_eq!(view.state().input(), "before");
    assert!(view.state().interaction().is_some());

    view.dispatch(Action::CancelInteraction);
    assert_eq!(
        view.handle_input(&Event::Paste("after".into())),
        InputOutcome::Handled(vec![Effect::Redraw(Invalidation::Content)])
    );
    assert_eq!(view.state().input(), "beforeafter");
}

#[test]
fn tiny_viewport_cancels_instead_of_capturing_hidden_input() {
    let mut tiny = AppView::new(Viewport::new(10, 4), TerminalCapabilities::default());
    let effects = tiny.dispatch(Action::PresentInteraction(sample_prompt("req-tiny")));
    assert!(tiny.state().interaction().is_none());
    assert_eq!(
        effects,
        vec![Effect::InteractionResolved {
            request_id: "req-tiny".into(),
            response: InteractionResponse::Cancelled,
        }]
    );
    assert_eq!(
        tiny.handle_input(&Event::Paste("captured".into())),
        InputOutcome::Handled(vec![Effect::Redraw(Invalidation::Content)])
    );
    assert_eq!(tiny.state().input(), "captured");

    let mut view = default_view();
    view.dispatch(Action::Resize(Viewport::new(80, 8)));
    let effects = view.dispatch(Action::PresentInteraction(sample_prompt("req-small")));
    assert!(view.state().interaction().is_none());
    assert_eq!(
        effects,
        vec![Effect::InteractionResolved {
            request_id: "req-small".into(),
            response: InteractionResponse::Cancelled,
        }]
    );

    let mut resized = default_view();
    resized.dispatch(Action::PresentInteraction(sample_prompt("req-resize")));
    assert_eq!(
        resized.dispatch(Action::Resize(Viewport::new(80, 8))),
        vec![
            Effect::Redraw(Invalidation::Layout),
            Effect::InteractionResolved {
                request_id: "req-resize".into(),
                response: InteractionResponse::Cancelled,
            },
        ]
    );
    assert!(resized.state().interaction().is_none());
}

#[test]
fn ascii_fallback_keeps_the_modal_buffer_ascii() {
    let capabilities = TerminalCapabilities::new(ColorCapability::TrueColor, false);
    let mut view = AppView::new(Viewport::new(80, 24), capabilities);
    view.dispatch(Action::PresentInteraction(prompt(
        "req-ascii",
        "许可♥",
        "请确认\u{2764}",
        [option("yes", "继续é")],
    )));
    let screen = render_view(&mut view, 80, 24);
    assert!(screen.is_ascii(), "unicode leaked into {screen:?}");
    assert!(!screen.contains('\u{2026}'));
    assert!(screen.contains('?'));
    assert!(!screen.contains("Permission required"));
    assert!(!screen.contains("Allow once"));
    assert!(!screen.contains("Always allow"));
}

#[test]
fn option_and_key_conflicts_are_deterministic() {
    let packed_prompt = prompt(
        "req-keys",
        "Choose",
        "Pick one",
        [
            option("keep", "First"),
            option("keep", "Duplicate id"),
            option("empty-label", ""),
            option("space-label", "   "),
            option("tab-label", "\t"),
            option("zero-width-label", "\u{200b}"),
            option("mixed-zero-width-label", "\u{200b} "),
            option("combining-space-label", "\u{0301} "),
            option(
                "long-space-label",
                &" ".repeat(INTERACTION_MAX_BODY_COLUMNS + 8),
            ),
            option(
                "long-zero-width-label",
                &"\u{200b}".repeat(INTERACTION_MAX_BODY_COLUMNS + 8),
            ),
            option("second", "Second"),
            option("third", "Third"),
            option("fourth", "Fourth"),
            option("fifth", "Fifth"),
            option("sixth", "Sixth"),
            option("seventh", "Seventh"),
            option("eighth", "Eighth"),
            option("ninth", "Ninth"),
            option("tenth", "Dropped"),
        ],
    );
    assert!(InteractionOption::new("", "Missing id").is_err());
    let ids = packed_prompt
        .options()
        .iter()
        .map(InteractionOption::id)
        .collect::<Vec<_>>();
    assert_eq!(packed_prompt.options().len(), INTERACTION_MAX_OPTIONS);
    assert_eq!(
        ids,
        [
            "keep", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth"
        ]
    );
    let keyed_ids = ['1', '2', '3', '4', '5', '6', '7', '8', '9']
        .into_iter()
        .map(|key| {
            packed_prompt
                .option_for_key(key)
                .map(InteractionOption::id)
                .expect("stored option key must resolve")
        })
        .collect::<Vec<_>>();
    assert_eq!(keyed_ids, ids);
    assert!(packed_prompt.option_for_key('0').is_none());

    let mut view = default_view();
    view.dispatch(Action::PresentInteraction(sample_prompt("req-miss")));
    assert!(
        view.dispatch(Action::SelectInteractionOption('3'))
            .is_empty()
    );
    assert!(
        view.dispatch(Action::SelectInteractionOption('0'))
            .is_empty()
    );
    assert_eq!(
        view.state()
            .interaction()
            .map(InteractionPrompt::request_id),
        Some("req-miss")
    );

    let mut shadowed = ActionRegistry::default();
    shadowed.register(
        ActionBinding::new(
            KeyPattern::exact(KeyCode::Char('1'), KeyModifiers::NONE),
            ActionId::Quit,
        )
        .when(When::INTERACTION_VISIBLE),
    );
    let mut view = default_view().with_action_registry(shadowed.clone());
    view.dispatch(Action::PresentInteraction(sample_prompt("req-shadow")));
    let screen = render_view(&mut view, 80, 24);
    assert!(!screen.contains("1 Continue"));
    assert!(screen.contains("(unbound) Continue"));
    assert!(screen.contains("2 Skip"));

    let mut narrow = AppView::new(
        Viewport::new(24, 24),
        TerminalCapabilities::new(ColorCapability::TrueColor, true),
    )
    .with_action_registry(shadowed);
    narrow.dispatch(Action::PresentInteraction(prompt(
        "req-long-unbound",
        "Choose",
        "Pick one",
        [option("long", &"L".repeat(INTERACTION_MAX_BODY_COLUMNS))],
    )));
    let narrow_screen = render_view(&mut narrow, 24, 24);
    assert!(narrow_screen.contains("(unbound)"));

    assert_eq!(
        view.handle_input(&Event::Key(KeyEvent::new(
            KeyCode::Char('1'),
            KeyModifiers::NONE,
        ))),
        InputOutcome::Handled(vec![
            Effect::InteractionResolved {
                request_id: "req-shadow".into(),
                response: InteractionResponse::Cancelled,
            },
            Effect::Redraw(Invalidation::Layout),
            Effect::RequestQuit,
        ])
    );
    assert!(view.state().interaction().is_none());
}

#[test]
fn modified_digit_binding_matches_rendering_and_dispatch() {
    let registry = ActionRegistry::new().with_binding(
        ActionBinding::new(
            KeyPattern::exact(KeyCode::Char('1'), KeyModifiers::SHIFT),
            ActionId::SelectInteractionOption,
        )
        .when(When::INTERACTION_VISIBLE),
    );
    let mut view = default_view().with_action_registry(registry);
    view.dispatch(Action::PresentInteraction(sample_prompt("req-shift")));

    let screen = render_view(&mut view, 80, 24);
    assert!(screen.contains("Shift+1 Continue"));
    assert!(screen.contains("(unbound) Skip"));
    assert_eq!(
        view.handle_input(&Event::Key(KeyEvent::new(
            KeyCode::Char('1'),
            KeyModifiers::SHIFT,
        ))),
        InputOutcome::Handled(vec![
            Effect::InteractionResolved {
                request_id: "req-shift".into(),
                response: InteractionResponse::Selected("yes".into()),
            },
            Effect::Redraw(Invalidation::Layout),
        ])
    );
}

#[test]
fn identifiers_are_validated_without_rewriting() {
    let request_id = "r".repeat(INTERACTION_MAX_REQUEST_ID_CHARS);
    let option_id = format!("选{}", "x".repeat(INTERACTION_MAX_OPTION_ID_CHARS - 1));
    let prompt = prompt(
        request_id.clone(),
        "Title",
        "Body",
        [option(&option_id, "Continue")],
    );
    assert_eq!(prompt.request_id(), request_id);
    assert_eq!(prompt.options()[0].id(), option_id);
    let mut round_trip = default_view();
    round_trip.dispatch(Action::PresentInteraction(prompt));
    assert!(
        round_trip
            .dispatch(Action::SelectInteractionOption('1'))
            .contains(&Effect::InteractionResolved {
                request_id: request_id.clone(),
                response: InteractionResponse::Selected(option_id.clone()),
            })
    );

    let long_request = "r".repeat(INTERACTION_MAX_REQUEST_ID_CHARS + 1);
    let long_option = "o".repeat(INTERACTION_MAX_OPTION_ID_CHARS + 1);
    assert!(InteractionPrompt::new(long_request, "Title", "Body", []).is_err());
    assert!(InteractionPrompt::new("bad\nrequest", "Title", "Body", []).is_err());
    assert!(InteractionOption::new(long_option, "Continue").is_err());
    assert!(InteractionOption::new("bad\toption", "Continue").is_err());

    let prefix = "p".repeat(INTERACTION_MAX_REQUEST_ID_CHARS - 1);
    let first_id = format!("{prefix}a");
    let second_id = format!("{prefix}b");
    let mut view = default_view();
    view.dispatch(Action::PresentInteraction(sample_prompt(&first_id)));
    assert_eq!(
        view.dispatch(Action::PresentInteraction(sample_prompt(&second_id))),
        vec![Effect::InteractionResolved {
            request_id: second_id,
            response: InteractionResponse::Cancelled,
        }]
    );
    assert_eq!(
        view.state()
            .interaction()
            .map(InteractionPrompt::request_id),
        Some(first_id.as_str())
    );
}

#[test]
fn display_inputs_are_truncated_and_control_sequences_stripped() {
    let long_title = "T".repeat(INTERACTION_MAX_TITLE_COLUMNS + 8);
    let long_line = "B".repeat(INTERACTION_MAX_BODY_COLUMNS + 8);
    let extra_lines = (0..INTERACTION_MAX_BODY_LINES + 4)
        .map(|index| format!("line-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let bounded_prompt = prompt(
        "req-bounds",
        format!("\u{1b}[31m{long_title}"),
        format!("\u{1b}[0m{long_line}\n{extra_lines}"),
        [option("yes", "Label")],
    );

    assert!(bounded_prompt.title().chars().count() <= INTERACTION_MAX_TITLE_COLUMNS + 1);
    assert!(!bounded_prompt.title().contains('\u{1b}'));
    assert_eq!(
        bounded_prompt.body_lines().len(),
        INTERACTION_MAX_BODY_LINES
    );
    assert!(!bounded_prompt.body_lines()[0].contains('\u{1b}'));

    let invisible_titles = [
        "\u{200b} ".to_owned(),
        "\u{0301} ".to_owned(),
        " ".repeat(INTERACTION_MAX_TITLE_COLUMNS + 8),
        "\u{200b}".repeat(INTERACTION_MAX_TITLE_COLUMNS + 8),
    ];
    for (index, title) in invisible_titles.into_iter().enumerate() {
        let fallback = prompt(
            format!("req-title-{index}"),
            title,
            "Body",
            [option("yes", "Yes")],
        );
        assert_eq!(fallback.title(), "Request");
    }

    let empty = prompt("req-empty", "Title", "Body", []);
    let mut view = default_view();
    let effects = view.dispatch(Action::PresentInteraction(empty));
    assert!(view.state().interaction().is_none());
    assert_eq!(
        effects,
        vec![Effect::InteractionResolved {
            request_id: "req-empty".into(),
            response: InteractionResponse::Cancelled,
        }]
    );
}

#[test]
fn same_request_id_can_refresh_without_a_second_resolution() {
    let mut view = default_view();
    view.dispatch(Action::PresentInteraction(sample_prompt("req-refresh")));
    let updated = prompt(
        "req-refresh",
        "Updated",
        "New body",
        [option("yes", "Continue")],
    );
    assert_eq!(
        view.dispatch(Action::PresentInteraction(updated)),
        vec![Effect::Redraw(Invalidation::Content)]
    );
    assert_eq!(
        view.state().interaction().map(InteractionPrompt::title),
        Some("Updated")
    );
    assert!(
        view.dispatch(Action::PresentInteraction(prompt(
            "req-refresh",
            "Updated",
            "New body",
            [option("yes", "Continue")],
        )))
        .is_empty()
    );

    let effects = view.dispatch(Action::PresentInteraction(prompt(
        "req-refresh",
        "Gone",
        "No options",
        [],
    )));
    assert!(view.state().interaction().is_none());
    assert_eq!(
        effects,
        vec![
            Effect::InteractionResolved {
                request_id: "req-refresh".into(),
                response: InteractionResponse::Cancelled,
            },
            Effect::Redraw(Invalidation::Layout),
        ]
    );
    assert!(view.dispatch(Action::CancelInteraction).is_empty());
}

#[test]
fn renderer_reserves_every_option_row_at_layout_boundaries() {
    let body = (1..=INTERACTION_MAX_BODY_LINES)
        .map(|index| format!("body-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let options = (1..=INTERACTION_MAX_OPTIONS)
        .map(|index| {
            InteractionOption::new(format!("option-{index}"), format!("Choice {index}"))
                .expect("generated option identifiers must be valid")
        })
        .collect::<Vec<_>>();
    let mut full = default_view();
    full.dispatch(Action::PresentInteraction(prompt(
        "req-full", "Choose", body, options,
    )));
    let screen = render_view(&mut full, 80, 24);
    for index in 1..=INTERACTION_MAX_OPTIONS {
        assert!(
            screen.contains(&format!("{index} Choice {index}")),
            "option {index} was clipped from {screen:?}"
        );
    }

    let mut multiline = AppView::new(
        Viewport::new(80, 10),
        TerminalCapabilities::new(ColorCapability::TrueColor, true),
    );
    multiline.dispatch(Action::ReplaceInput(
        "one\ntwo\nthree\nfour\nfive\nsix".into(),
    ));
    assert_eq!(
        multiline.dispatch(Action::PresentInteraction(sample_prompt("req-multiline"))),
        vec![Effect::Redraw(Invalidation::Layout)]
    );
    assert!(multiline.state().interaction().is_some());
    let screen = render_view(&mut multiline, 80, 10);
    assert!(screen.contains("1 Continue"));
    assert!(screen.contains("2 Skip"));
}

#[test]
fn rendered_modal_omits_grant_labels_and_shows_generic_options() {
    let mut view = default_view();
    view.dispatch(Action::PresentInteraction(sample_prompt("req-draw")));
    let screen = render_view(&mut view, 80, 24);
    assert!(screen.contains("Continue?"));
    assert!(screen.contains("Run the next step"));
    assert!(screen.contains("1 Continue"));
    assert!(screen.contains("2 Skip"));
    assert!(screen.contains("Esc Cancel"));
    assert!(!screen.contains("Permission required"));
    assert!(!screen.contains("Allow once"));
    assert!(!screen.contains("Allow for this session"));
    assert!(!screen.contains("Always allow"));
}
