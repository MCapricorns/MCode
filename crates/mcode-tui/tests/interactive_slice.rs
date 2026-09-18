use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Mutex, MutexGuard};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use mcode_render::{RenderBlock, display_width};
use mcode_tui::{
    Action, AppView, ColorCapability, DEFAULT_SCROLLBACK_BLOCKS, Effect, InputOutcome,
    Invalidation, LineEditor, MaterializeBudget, TerminalCapabilities, TerminalGuard, Viewport,
    materialize, restore_on_abnormal_exit,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

fn guard_test_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn default_view() -> AppView {
    AppView::new(
        Viewport::new(80, 24),
        TerminalCapabilities::new(ColorCapability::TrueColor, true),
    )
}

fn synthetic_history(block_count: usize) -> Vec<RenderBlock> {
    (1..=block_count)
        .map(|index| RenderBlock::Text(format!("history-{index:02}")))
        .collect()
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

#[test]
fn mocked_guard_restores_on_drop() {
    let _lock = guard_test_lock();
    let probe = {
        let (guard, probe) = TerminalGuard::new_mocked().expect("mock guard");
        assert!(!guard.is_restored());
        assert!(probe.is_raw_mode());
        assert!(probe.is_alternate_screen());
        assert!(probe.is_cursor_hidden());
        drop(guard);
        probe
    };
    assert!(probe.is_restored());
    assert_eq!(probe.restore_count(), 1);
    assert!(!probe.is_raw_mode());
    assert!(!probe.is_alternate_screen());
    assert!(!probe.is_cursor_hidden());
}

#[test]
fn mocked_guard_restores_once_after_panic_unwind() {
    let _lock = guard_test_lock();
    let (guard, probe) = TerminalGuard::new_mocked().expect("mock guard");
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _guard = guard;
        panic!("terminal guard unwind");
    }));
    assert!(result.is_err());
    assert!(probe.is_restored());
    assert_eq!(probe.restore_count(), 1);
    assert!(!probe.is_raw_mode());
}

#[test]
fn abnormal_exit_restores_a_forgotten_guard_on_windows_path() {
    let _lock = guard_test_lock();
    let (guard, probe) = TerminalGuard::new_mocked().expect("mock guard");
    std::mem::forget(guard);
    assert!(!probe.is_restored());
    restore_on_abnormal_exit();
    assert!(probe.is_restored());
    assert_eq!(probe.restore_count(), 1);
    restore_on_abnormal_exit();
    assert_eq!(probe.restore_count(), 1);
}

#[test]
fn resize_updates_viewport() {
    let mut view = default_view();
    assert!(
        view.dispatch(Action::Resize(Viewport::new(80, 24)))
            .is_empty()
    );

    assert_eq!(
        view.dispatch(Action::Resize(Viewport::new(0, 0))),
        vec![Effect::Redraw(Invalidation::Layout)]
    );
    assert_eq!(view.state().viewport(), Viewport::new(0, 0));
}

#[test]
fn unicode_width_and_grapheme_backspace_use_render_helpers() {
    let mut editor = LineEditor::new();
    assert!(editor.insert('Ａ'));
    assert_eq!(editor.display_width(), 2);
    assert_eq!(display_width(editor.as_str()), 2);

    let scientist = "\u{1F469}\u{200D}\u{1F52C}";
    assert!(editor.paste(scientist));
    assert!(editor.as_str().ends_with(scientist));
    assert!(editor.backspace());
    assert_eq!(editor.as_str(), "Ａ");

    let mut view = default_view();
    assert!(
        view.handle_input(&Event::Key(KeyEvent::new(
            KeyCode::Char('Ａ'),
            KeyModifiers::NONE,
        )))
        .is_handled()
    );
    assert_eq!(display_width(view.state().input()), 2);
    view.dispatch(Action::Paste(scientist.into()));
    view.dispatch(Action::Backspace);
    assert_eq!(view.state().input(), "Ａ");
}

#[test]
fn paste_event_fills_the_multiline_editor() {
    let mut view = default_view();
    let pasted = "hello\n世界";
    assert_eq!(
        view.handle_input(&Event::Paste(pasted.into())),
        InputOutcome::Handled(vec![Effect::Redraw(Invalidation::Content)])
    );
    assert_eq!(view.state().input(), pasted);
    assert!(view.state().input().contains('\n'));

    view.dispatch(Action::InsertNewline);
    assert!(view.state().input().ends_with('\n'));

    let shift_enter = view.handle_input(&Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::SHIFT,
    )));
    assert!(shift_enter.is_handled());
    assert!(view.state().input().ends_with("\n\n"));
}

#[test]
fn zero_viewport_budget_does_not_walk_history() {
    let blocks = (0..5_000)
        .map(|index| RenderBlock::Text(format!("block-{index}")))
        .collect::<Vec<_>>();

    let zero_width = materialize(&blocks, MaterializeBudget::new(0, 24, 0));
    assert!(zero_width.lines().is_empty());
    assert_eq!(zero_width.blocks_examined(), 0);

    let zero_height = materialize(&blocks, MaterializeBudget::new(80, 0, 0));
    assert!(zero_height.lines().is_empty());
    assert_eq!(zero_height.blocks_examined(), 0);

    let zero_viewport = materialize(
        &blocks,
        MaterializeBudget::from_viewport(Viewport::new(0, 0), 0),
    );
    assert_eq!(zero_viewport.blocks_examined(), 0);

    let visible = materialize(&blocks, MaterializeBudget::new(80, 10, 0));
    assert_eq!(visible.lines().len(), 10);
    assert!(visible.blocks_examined() <= 20);
    assert!(visible.blocks_examined() > 0);
    assert_eq!(
        visible.lines().last().map(|line| line.text()),
        Some("block-4999")
    );
}

#[test]
fn scroll_actions_move_and_clamp_synthetic_history() {
    let mut view = default_view();
    view.dispatch(Action::ReplaceBlocks(synthetic_history(40)));

    assert_eq!(
        view.dispatch(Action::ScrollBy(5)),
        vec![Effect::Redraw(Invalidation::Content)]
    );
    assert_eq!(view.state().scroll_offset(), 5);
    assert_eq!(
        view.dispatch(Action::ScrollBy(-2)),
        vec![Effect::Redraw(Invalidation::Content)]
    );
    assert_eq!(view.state().scroll_offset(), 3);

    let oldest_offset = materialize(
        view.state().blocks(),
        MaterializeBudget::new(78, 14, usize::MAX),
    )
    .offset();
    assert_eq!(oldest_offset, 65);
    assert_eq!(
        view.dispatch(Action::ScrollBy(i32::MAX)),
        vec![Effect::Redraw(Invalidation::Content)]
    );
    assert_eq!(view.state().scroll_offset(), oldest_offset);
    assert!(view.dispatch(Action::ScrollBy(1)).is_empty());

    assert_eq!(
        view.dispatch(Action::ScrollBy(i32::MIN)),
        vec![Effect::Redraw(Invalidation::Content)]
    );
    assert_eq!(view.state().scroll_offset(), 0);
}

#[test]
fn scroll_offset_tracks_layout_changes_and_resets_on_replace() {
    let mut view = default_view();
    view.dispatch(Action::ReplaceBlocks(synthetic_history(40)));
    view.dispatch(Action::ReplaceInput("a\nb\nc\nd".into()));
    view.dispatch(Action::ScrollBy(i32::MAX));

    let multiline_oldest_offset = materialize(
        view.state().blocks(),
        MaterializeBudget::new(78, 11, usize::MAX),
    )
    .offset();
    assert_eq!(view.state().scroll_offset(), multiline_oldest_offset);

    view.dispatch(Action::ReplaceInput("one line".into()));
    let single_line_oldest_offset = materialize(
        view.state().blocks(),
        MaterializeBudget::new(78, 14, usize::MAX),
    )
    .offset();
    assert!(single_line_oldest_offset < multiline_oldest_offset);
    assert_eq!(view.state().scroll_offset(), single_line_oldest_offset);

    let parked = view.state().scroll_offset();
    view.dispatch(Action::Resize(Viewport::new(1, 2)));
    assert_eq!(view.state().scroll_offset(), parked);
    assert!(view.dispatch(Action::ScrollBy(1)).is_empty());
    view.dispatch(Action::Resize(Viewport::new(80, 24)));
    assert_eq!(view.state().scroll_offset(), parked);

    let same_blocks = view.state().blocks().to_vec();
    assert_eq!(
        view.dispatch(Action::ReplaceBlocks(same_blocks)),
        vec![Effect::Redraw(Invalidation::Content)]
    );
    assert_eq!(view.state().scroll_offset(), 0);

    view.dispatch(Action::ScrollBy(i32::MAX));
    let mut changed_blocks = view.state().blocks().to_vec();
    changed_blocks.push(RenderBlock::Text("fresh".into()));
    view.dispatch(Action::ReplaceBlocks(changed_blocks));
    assert_eq!(view.state().scroll_offset(), 0);
    assert_eq!(
        view.state().blocks().last(),
        Some(&RenderBlock::Text("fresh".into()))
    );
}

#[test]
fn replace_blocks_is_capacity_bounded_and_draw_survives_zero_inner_width() {
    let mut view = default_view();
    let blocks = (0..2_000)
        .map(|index| RenderBlock::Text(format!("{index}")))
        .collect::<Vec<_>>();
    view.dispatch(Action::ReplaceBlocks(blocks));
    assert_eq!(view.state().blocks().len(), DEFAULT_SCROLLBACK_BLOCKS);
    assert_eq!(
        view.state().blocks().last(),
        Some(&RenderBlock::Text("1999".into()))
    );

    let many = (0..5_000)
        .map(|index| RenderBlock::Text(format!("row-{index}")))
        .collect::<Vec<_>>();
    let mut narrow = AppView::new(
        Viewport::new(1, 24),
        TerminalCapabilities::from_detection(ColorCapability::TrueColor, false, false),
    );
    narrow.dispatch(Action::ReplaceBlocks(many));
    let backend = TestBackend::new(1, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal must initialize");
    terminal
        .draw(|frame| narrow.draw(frame))
        .expect("zero inner width must not panic");
}

#[test]
fn status_is_pure_data() {
    let mut view = default_view();
    assert_eq!(view.state().status_surface().message(), "Ready");
    view.dispatch(Action::SetStatus("Indexing".into()));
    assert_eq!(view.state().status(), "Indexing");
}

#[test]
fn from_detection_drives_unicode_and_color_readability() {
    let ascii = TerminalCapabilities::from_detection(ColorCapability::TrueColor, false, false);
    assert!(ascii.supports_color());
    assert!(!ascii.supports_unicode());

    let backend = TestBackend::new(40, 16);
    let mut terminal = Terminal::new(backend).expect("test terminal must initialize");
    let mut view = AppView::new(Viewport::new(40, 16), ascii);
    view.dispatch(Action::ReplaceBlocks(vec![RenderBlock::Text(
        "\u{2764}\u{fe0f} Ａ".into(),
    )]));
    view.dispatch(Action::ReplaceInput("é".into()));
    terminal
        .draw(|frame| view.draw(frame))
        .expect("ASCII fallback must render");
    let screen = buffer_text(terminal.backend().buffer());
    assert!(screen.is_ascii(), "unicode leaked into {screen:?}");
    assert!(!screen.contains('\u{2026}'));

    let no_color = TerminalCapabilities::from_detection(ColorCapability::TrueColor, true, true);
    assert!(!no_color.supports_color());
    assert!(no_color.supports_unicode());
    let buffer = {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal must initialize");
        let mut view = AppView::new(Viewport::new(80, 24), no_color);
        terminal
            .draw(|frame| view.draw(frame))
            .expect("no-color unicode view must render");
        terminal.backend().buffer().clone()
    };
    assert_eq!(buffer[(0, 0)].bg, ratatui::style::Color::Reset);
}

#[test]
fn second_guard_is_rejected_without_restoring_the_first() {
    let _lock = guard_test_lock();
    let (first, probe) = TerminalGuard::new_mocked().expect("first guard");
    let second = TerminalGuard::new_mocked();
    assert!(second.is_err(), "nested enter must fail");
    assert!(!probe.is_restored());
    assert!(probe.is_raw_mode());
    assert!(probe.is_bracketed_paste());
    drop(first);
    assert!(probe.is_restored());
    let (third, _) = TerminalGuard::new_mocked().expect("guard after restore");
    drop(third);
}

#[test]
fn input_window_keeps_caret_visible_on_long_paste() {
    let mut view = default_view();
    let pasted = format!(
        "{}
{}",
        "line\n".repeat(8).trim_end(),
        "宽".repeat(80)
    );
    view.dispatch(Action::Paste(pasted));
    let backend = TestBackend::new(40, 16);
    let mut terminal = Terminal::new(backend).expect("test terminal must initialize");
    terminal
        .draw(|frame| view.draw(frame))
        .expect("caret window must render");
    let screen = buffer_text(terminal.backend().buffer());
    assert!(screen.contains('宽'), "caret line missing from {screen:?}");
}
