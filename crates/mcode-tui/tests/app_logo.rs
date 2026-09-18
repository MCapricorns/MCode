use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use mcode_render::{Progress, RenderBlock};
use mcode_tui::render::terminal_color;
use mcode_tui::{
    Action, ActionBinding, ActionId, ActionRegistry, AppState, AppView, ColorCapability, Effect,
    InputOutcome, Invalidation, KeyPattern, LogoVariant, Rgb, TerminalCapabilities, ThemeSelection,
    Viewport, When, reduce, terminal_logo,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;

const SIZES: [(u16, u16); 4] = [(100, 30), (80, 24), (60, 20), (30, 12)];

#[test]
fn logo_is_responsive_and_strictly_width_bounded() {
    let color = TerminalCapabilities::new(ColorCapability::TrueColor, true);
    for width in [100, 80] {
        let logo = terminal_logo(width, color);
        assert_eq!(logo.variant(), LogoVariant::Wide);
        assert!(
            logo.lines()
                .iter()
                .all(|line| line.display_width() <= usize::from(width))
        );
        assert!(
            logo.lines()
                .iter()
                .any(|line| line.to_plain_text().contains("TERMINAL CODE AGENT"))
        );
    }

    for width in [60, 30, 20] {
        let logo = terminal_logo(width, color);
        assert_eq!(logo.variant(), LogoVariant::Compact);
        assert!(
            logo.lines()
                .iter()
                .all(|line| line.display_width() <= usize::from(width))
        );
    }
}

#[test]
fn no_color_and_non_unicode_logos_are_ascii() {
    for capabilities in [
        TerminalCapabilities::new(ColorCapability::NoColor, true),
        TerminalCapabilities::new(ColorCapability::TrueColor, false),
    ] {
        for width in [100, 30, 20] {
            let logo = terminal_logo(width, capabilities);
            assert_eq!(logo.variant(), LogoVariant::Ascii);
            for line in logo.lines() {
                let plain = line.to_plain_text();
                assert!(plain.is_ascii(), "ASCII fallback contained {plain:?}");
                assert!(line.display_width() <= usize::from(width));
            }
        }
    }
}

#[test]
fn logo_lockups_have_stable_plain_text() {
    let color = TerminalCapabilities::new(ColorCapability::TrueColor, true);
    let no_color = TerminalCapabilities::new(ColorCapability::NoColor, true);

    assert_eq!(
        terminal_logo(100, color).lines()[0].to_plain_text(),
        "╭────────────────╮   M C O D E"
    );
    assert_eq!(
        terminal_logo(60, color).lines()[0].to_plain_text(),
        "╭─ MCODE ────────╮"
    );
    assert_eq!(
        terminal_logo(100, no_color).lines()[0].to_plain_text(),
        "+-- MCODE --------+"
    );
}

#[test]
fn reducer_is_pure_and_emits_data_only_effects() {
    let original = AppState::new(Viewport::new(80, 24));
    let inserted = reduce(&original, Action::Insert('a'));
    assert_eq!(original.input(), "");
    assert_eq!(inserted.state().input(), "a");
    assert_eq!(inserted.effects(), &[Effect::Redraw(Invalidation::Content)]);

    let submitted = reduce(inserted.state(), Action::Submit);
    assert_eq!(submitted.state().input(), "");
    assert_eq!(
        submitted.effects(),
        &[
            Effect::SubmitInput("a".into()),
            Effect::Redraw(Invalidation::Content),
        ]
    );
    assert_eq!(inserted.state().input(), "a");
}

#[test]
fn resize_and_theme_invalidation_only_fire_on_change() {
    let capabilities = TerminalCapabilities::new(ColorCapability::TrueColor, true);
    let mut view = AppView::new(Viewport::new(80, 24), capabilities);
    assert_eq!(view.take_invalidation(), Some(Invalidation::Layout));

    assert!(
        view.dispatch(Action::Resize(Viewport::new(80, 24)))
            .is_empty()
    );
    assert_eq!(view.take_invalidation(), None);

    assert_eq!(
        view.dispatch(Action::Resize(Viewport::new(100, 30))),
        vec![Effect::Redraw(Invalidation::Layout)]
    );
    assert_eq!(view.take_invalidation(), Some(Invalidation::Layout));

    assert_eq!(
        view.dispatch(Action::SelectTheme(ThemeSelection::Light)),
        vec![Effect::Redraw(Invalidation::Theme)]
    );
    assert_eq!(view.theme_resolution().theme().name(), "mcode-light");
    assert_eq!(view.take_invalidation(), Some(Invalidation::Theme));

    assert!(
        view.dispatch(Action::SelectTheme(ThemeSelection::Light))
            .is_empty()
    );
    assert_eq!(view.take_invalidation(), None);
}

#[test]
fn crossterm_input_is_translated_without_running_effects() {
    let capabilities = TerminalCapabilities::new(ColorCapability::TrueColor, true);
    let mut view = AppView::new(Viewport::new(80, 24), capabilities);
    let _ = view.take_invalidation();

    let typed = view.handle_input(&Event::Key(KeyEvent::new(
        KeyCode::Char('x'),
        KeyModifiers::NONE,
    )));
    assert!(typed.is_handled());
    assert_eq!(view.state().input(), "x");
    assert_eq!(
        typed,
        InputOutcome::Handled(vec![Effect::Redraw(Invalidation::Content)])
    );

    let ignored = view.handle_input(&Event::FocusGained);
    assert_eq!(ignored, InputOutcome::Ignored);

    let quit = view.handle_input(&Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )));
    assert_eq!(quit, InputOutcome::Handled(vec![Effect::RequestQuit]));
}

#[test]
fn injected_action_registry_honors_when_predicates() {
    let capabilities = TerminalCapabilities::new(ColorCapability::TrueColor, true);
    let registry = ActionRegistry::new().with_binding(
        ActionBinding::new(
            KeyPattern::exact(KeyCode::Esc, KeyModifiers::NONE),
            ActionId::Quit,
        )
        .when(When::HELP_VISIBLE),
    );
    let mut view = AppView::new(Viewport::new(80, 24), capabilities).with_action_registry(registry);
    let escape = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert_eq!(view.action_registry().bindings().len(), 1);
    assert_eq!(view.handle_input(&escape), InputOutcome::Ignored);

    view.dispatch(Action::ToggleHelp);
    assert_eq!(
        view.handle_input(&escape),
        InputOutcome::Handled(vec![Effect::RequestQuit])
    );
    assert_eq!(
        view.handle_input(&Event::Key(KeyEvent::new(
            KeyCode::F(1),
            KeyModifiers::NONE,
        ))),
        InputOutcome::Ignored
    );
}

#[test]
fn test_backend_renders_required_foundation_at_fixed_sizes() {
    let variants = [
        (
            ThemeSelection::Dark,
            TerminalCapabilities::new(ColorCapability::TrueColor, true),
        ),
        (
            ThemeSelection::Light,
            TerminalCapabilities::new(ColorCapability::TrueColor, true),
        ),
        (
            ThemeSelection::Dark,
            TerminalCapabilities::new(ColorCapability::NoColor, true),
        ),
    ];

    for (selection, capabilities) in variants {
        for (width, height) in SIZES {
            let buffer = render_case(width, height, selection.clone(), capabilities);
            let screen = buffer_text(&buffer);
            if width >= 70 {
                assert!(screen.contains("TERMINAL CODE AGENT"), "{width}x{height}");
            } else {
                assert!(screen.contains("MCODE"), "{width}x{height}");
            }
            if height >= 20 {
                assert!(screen.contains("Conversation"), "{width}x{height}");
                assert!(screen.contains("Build plan ready."), "{width}x{height}");
            }
            assert!(screen.contains("Ask MCode"), "{width}x{height}");
            assert!(screen.contains("Ready"), "{width}x{height}");
            assert_eq!(buffer.area.width, width);
            assert_eq!(buffer.area.height, height);
        }
    }
}

#[test]
fn non_unicode_capability_keeps_the_complete_buffer_ascii() {
    let width = 30;
    let height = 12;
    let capabilities = TerminalCapabilities::new(ColorCapability::TrueColor, false);
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal must initialize");
    let mut view = AppView::new(Viewport::new(width, height), capabilities);
    view.dispatch(Action::ReplaceBlocks(vec![RenderBlock::Text(
        "❤️".repeat(100),
    )]));
    view.dispatch(Action::ReplaceInput("é".repeat(100)));
    view.dispatch(Action::SetStatus("状态".repeat(100)));

    terminal
        .draw(|frame| view.draw(frame))
        .expect("non-Unicode view must render");
    let screen = buffer_text(terminal.backend().buffer());

    assert!(screen.is_ascii(), "non-Unicode buffer contained {screen:?}");
    assert!(!screen.contains('…'));
    assert!(screen.contains('+'));
    assert!(screen.contains('|'));
    assert!(screen.contains('.'));
}

#[test]
fn dark_light_and_no_color_buffers_use_expected_background_modes() {
    let dark = render_case(
        80,
        24,
        ThemeSelection::Dark,
        TerminalCapabilities::new(ColorCapability::TrueColor, true),
    );
    let light = render_case(
        80,
        24,
        ThemeSelection::Light,
        TerminalCapabilities::new(ColorCapability::TrueColor, true),
    );
    let plain = render_case(
        80,
        24,
        ThemeSelection::Dark,
        TerminalCapabilities::new(ColorCapability::NoColor, true),
    );

    assert_eq!(dark[(0, 0)].bg, Color::Rgb(0x10, 0x12, 0x11));
    assert_eq!(light[(0, 0)].bg, Color::Rgb(0xf7, 0xf7, 0xf2));
    assert_eq!(plain[(0, 0)].bg, Color::Reset);
    assert!(
        buffer_text(&plain)
            .lines()
            .next()
            .unwrap_or_default()
            .is_ascii()
    );
}

#[test]
fn color_mapping_covers_every_terminal_depth() {
    let accent = Rgb::new(0x44, 0xdf, 0x6c);
    assert_eq!(
        terminal_color(accent, ColorCapability::NoColor),
        Color::Reset
    );
    assert!(matches!(
        terminal_color(accent, ColorCapability::Basic),
        Color::Green | Color::LightGreen
    ));
    assert!(matches!(
        terminal_color(accent, ColorCapability::Ansi256),
        Color::Indexed(_)
    ));
    assert_eq!(
        terminal_color(accent, ColorCapability::TrueColor),
        Color::Rgb(0x44, 0xdf, 0x6c)
    );
}

fn render_case(
    width: u16,
    height: u16,
    selection: ThemeSelection,
    capabilities: TerminalCapabilities,
) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal must initialize");
    let mut view =
        AppView::new(Viewport::new(width, height), capabilities).with_theme_selection(selection);
    view.dispatch(Action::ReplaceBlocks(vec![
        RenderBlock::Text("Build plan ready.".into()),
        RenderBlock::Markdown("`cargo test` is next.".into()),
        RenderBlock::Progress(Progress::running("Checking workspace", 2, Some(4))),
    ]));

    terminal
        .draw(|frame| view.draw(frame))
        .expect("foundation view must render");
    terminal.backend().buffer().clone()
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
