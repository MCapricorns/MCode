//! Ratatui adapter for the pure MCode view state.
//!
//! Rendering consumes [`mcode_render::RenderBlock`] through its bounded plain
//! fallback. When Unicode is disabled, borders and all visible text degrade to
//! ASCII. The adapter performs no input, terminal setup, session calls, or
//! other effects, which keeps `TestBackend` tests deterministic.
use crossterm::event::{Event, KeyEvent};
use mcode_render::{
    MAX_PLAIN_WIDTH, RenderBlock, display_width, next_grapheme_boundary, sanitize_terminal_text,
    truncate_display_width,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::actions::{Action, ActionId, ActionRegistry, KeyPattern};
use crate::hints;
use crate::interaction::{INTERACTION_MAX_BODY_COLUMNS, INTERACTION_MAX_TITLE_COLUMNS, option_key};
use crate::labels::{HELP_TITLE, INPUT_TITLE, INTERACTION_CANCEL, TRANSCRIPT_TITLE, UNBOUND_LABEL};
use crate::layout::view_layout;
use crate::logo::{TerminalLogo, terminal_logo};
use crate::scrollback::{MaterializeBudget, materialize};
use crate::state::AppState;
use crate::terminal::{ColorCapability, TerminalCapabilities, rgb_to_ansi256};
use crate::theme::{Rgb, SemanticToken, Theme};

/// ASCII border used when the host prohibits Unicode terminal glyphs.
const ASCII_BORDER_SET: border::Set = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// Maps one semantic sRGB color to the selected terminal color depth.
#[must_use]
pub fn terminal_color(color: Rgb, capability: ColorCapability) -> Color {
    match capability {
        ColorCapability::NoColor => Color::Reset,
        ColorCapability::Basic => nearest_basic_color(color),
        ColorCapability::Ansi256 => Color::Indexed(rgb_to_ansi256(color)),
        ColorCapability::TrueColor => Color::Rgb(color.red, color.green, color.blue),
    }
}

/// Builds a foreground style for one semantic token.
#[must_use]
pub fn token_style(
    theme: &Theme,
    token: SemanticToken,
    capabilities: TerminalCapabilities,
) -> Style {
    if capabilities.supports_color() {
        Style::default().fg(terminal_color(theme.color(token), capabilities.color()))
    } else {
        Style::default()
    }
}

/// Draws the complete foundation view into a Ratatui frame.
///
/// The layout contains a responsive logo, render-block viewport, status bar,
/// and input placeholder or buffer. Key hints come from `registry`, the same
/// bindings that dispatch input. Small areas are clipped by Ratatui without
/// panicking.
pub fn draw(
    frame: &mut Frame<'_>,
    state: &AppState,
    registry: &ActionRegistry,
    theme: &Theme,
    capabilities: TerminalCapabilities,
) {
    let area = frame.area();
    if area.is_empty() {
        return;
    }

    frame.render_widget(
        Block::new().style(background_style(
            theme,
            SemanticToken::Background,
            capabilities,
        )),
        area,
    );

    let logo = terminal_logo(area.width, capabilities);
    debug_assert_eq!(logo.lines().len(), usize::from(crate::logo::LOGO_ROWS));
    let layout = view_layout(area, state);

    render_logo(frame, layout.logo, &logo, theme, capabilities);
    render_body(
        frame,
        layout.body,
        layout.transcript,
        state,
        registry,
        theme,
        capabilities,
    );
    render_input(frame, layout.input, state, theme, capabilities);
    render_status(frame, layout.status, state, registry, theme, capabilities);
}

fn render_logo(
    frame: &mut Frame<'_>,
    area: Rect,
    logo: &TerminalLogo,
    theme: &Theme,
    capabilities: TerminalCapabilities,
) {
    let lines = logo
        .lines()
        .iter()
        .take(usize::from(area.height))
        .map(|line| {
            Line::from(
                line.spans()
                    .iter()
                    .map(|span| {
                        let mut style = token_style(theme, span.token(), capabilities);
                        if span.is_bold() {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                        Span::styled(span.text().to_owned(), style)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_body(
    frame: &mut Frame<'_>,
    area: Rect,
    inner: Rect,
    state: &AppState,
    registry: &ActionRegistry,
    theme: &Theme,
    capabilities: TerminalCapabilities,
) {
    if area.is_empty() {
        return;
    }

    let panel = bordered_block(capabilities)
        .title(TRANSCRIPT_TITLE)
        .style(panel_style(theme, capabilities));
    frame.render_widget(panel, area);

    let lines = if state.interaction().is_some() {
        interaction_lines(theme, capabilities, registry, state, inner)
    } else if state.is_help_visible() {
        help_lines(theme, capabilities, registry, state, inner.width.into())
    } else {
        block_lines(state, inner, theme, capabilities)
    };
    frame.render_widget(Paragraph::new(lines), inner);
}

fn help_lines(
    theme: &Theme,
    capabilities: TerminalCapabilities,
    registry: &ActionRegistry,
    state: &AppState,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::styled(
            HELP_TITLE,
            token_style(theme, SemanticToken::Accent, capabilities).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ];
    // Dynamic key labels can contain any bound character, so every entry
    // passes the same sanitize-and-ASCII-degrade path as other text to keep
    // the full-buffer ASCII contract when Unicode is disabled.
    lines.extend(hints::help_lines(registry, state).into_iter().map(|entry| {
        Line::styled(
            bounded_terminal_line(entry, width, capabilities),
            token_style(theme, SemanticToken::TextPrimary, capabilities),
        )
    }));
    lines
}

fn interaction_lines(
    theme: &Theme,
    capabilities: TerminalCapabilities,
    registry: &ActionRegistry,
    state: &AppState,
    area: Rect,
) -> Vec<Line<'static>> {
    let Some(prompt) = state.interaction() else {
        return Vec::new();
    };
    let height = usize::from(area.height);
    let has_body = !prompt.body_lines().is_empty();
    let required_body_rows = usize::from(has_body);
    let minimum_rows = 1_usize
        .saturating_add(required_body_rows)
        .saturating_add(prompt.options().len());
    if height < minimum_rows {
        return Vec::new();
    }

    let cancel_keys = hints::key_label(registry, state, ActionId::CancelInteraction);
    let mut remaining_rows = height - minimum_rows;
    let show_cancel = cancel_keys.is_some() && remaining_rows > 0;
    remaining_rows = remaining_rows.saturating_sub(usize::from(show_cancel));
    let gap_before_body = has_body && remaining_rows > 0;
    remaining_rows = remaining_rows.saturating_sub(usize::from(gap_before_body));
    let gap_after_body = remaining_rows > 0;
    remaining_rows = remaining_rows.saturating_sub(usize::from(gap_after_body));
    let body_rows = required_body_rows.saturating_add(
        remaining_rows.min(prompt.body_lines().len().saturating_sub(required_body_rows)),
    );

    let width = usize::from(area.width);
    let title_width = width.min(INTERACTION_MAX_TITLE_COLUMNS);
    let body_width = width.min(INTERACTION_MAX_BODY_COLUMNS);
    let mut lines = vec![Line::styled(
        bounded_terminal_line(prompt.title(), title_width, capabilities),
        token_style(theme, SemanticToken::Warning, capabilities).add_modifier(Modifier::BOLD),
    )];
    if gap_before_body {
        lines.push(Line::raw(""));
    }
    lines.extend(prompt.body_lines().iter().take(body_rows).map(|body| {
        Line::styled(
            bounded_terminal_line(body, body_width, capabilities),
            token_style(theme, SemanticToken::TextPrimary, capabilities),
        )
    }));
    if gap_after_body {
        lines.push(Line::raw(""));
    }
    for (index, option) in prompt.options().iter().enumerate() {
        let Some(key) = option_key(index) else {
            continue;
        };
        let text = match interaction_option_key_label(registry, state, key) {
            Some(keys) => format!("{keys} {}", option.label()),
            None => format!("({UNBOUND_LABEL}) {}", option.label()),
        };
        lines.push(Line::styled(
            bounded_terminal_line(text, width, capabilities),
            token_style(theme, SemanticToken::Accent, capabilities),
        ));
    }
    if show_cancel && let Some(keys) = cancel_keys {
        let text = format!("{keys} {INTERACTION_CANCEL}");
        lines.push(Line::styled(
            bounded_terminal_line(text, width, capabilities),
            token_style(theme, SemanticToken::Accent, capabilities),
        ));
    }
    debug_assert!(lines.len() <= height);
    lines
}

fn interaction_option_key_label(
    registry: &ActionRegistry,
    state: &AppState,
    option_key: char,
) -> Option<String> {
    registry.bindings().iter().find_map(|binding| {
        let KeyPattern::Exact { code, modifiers } = binding.pattern() else {
            return None;
        };
        let event = Event::Key(KeyEvent::new(*code, *modifiers));
        match registry.action_for_event(&event, state) {
            Some(Action::SelectInteractionOption(selected)) if selected == option_key => {
                hints::pattern_label(binding.pattern())
            }
            _ => None,
        }
    })
}

fn block_lines(
    state: &AppState,
    area: Rect,
    theme: &Theme,
    capabilities: TerminalCapabilities,
) -> Vec<Line<'static>> {
    let width = usize::from(area.width);
    let view = materialize(
        state.blocks(),
        MaterializeBudget::new(width, usize::from(area.height), state.scroll_offset()),
    );
    view.lines()
        .iter()
        .map(|line| {
            let default_token = block_token(line.block());
            let token = match line.block() {
                RenderBlock::Diff(_) if line.text().starts_with('+') => SemanticToken::DiffAdded,
                RenderBlock::Diff(_) if line.text().starts_with('-') => SemanticToken::DiffRemoved,
                _ => default_token,
            };
            Line::styled(
                bounded_terminal_line(line.text(), width, capabilities),
                token_style(theme, token, capabilities),
            )
        })
        .collect()
}

fn block_token(block: &RenderBlock) -> SemanticToken {
    match block {
        RenderBlock::Text(_) | RenderBlock::Table(_) | RenderBlock::Tree(_) => {
            SemanticToken::TextPrimary
        }
        RenderBlock::Markdown(_) => SemanticToken::MarkdownCode,
        RenderBlock::Diff(_) => SemanticToken::DiffContext,
        RenderBlock::Progress(_) => SemanticToken::ProgressFill,
        RenderBlock::Error(_) => SemanticToken::Error,
        RenderBlock::Widget(_) => SemanticToken::ToolOutput,
        _ => SemanticToken::TextPrimary,
    }
}

fn render_input(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    capabilities: TerminalCapabilities,
) {
    if area.is_empty() {
        return;
    }

    let panel = bordered_block(capabilities)
        .title(INPUT_TITLE)
        .style(input_panel_style(theme, capabilities));
    let inner = panel.inner(area);
    frame.render_widget(panel, area);

    let width = usize::from(inner.width);
    let height = usize::from(inner.height).max(1);
    let lines = if state.input().is_empty() {
        vec![Line::styled(
            bounded_terminal_line(state.input_placeholder(), width, capabilities),
            token_style(theme, SemanticToken::TextDim, capabilities),
        )]
    } else {
        let (caret_line, caret_col) = state.editor().caret_line_column();
        let v_off = caret_line.saturating_sub(height.saturating_sub(1));
        let h_off = caret_col.saturating_sub(width.saturating_sub(1));
        state
            .input()
            .split('\n')
            .skip(v_off)
            .take(height)
            .map(|line| {
                Line::styled(
                    bounded_terminal_line(skip_display_cols(line, h_off), width, capabilities),
                    token_style(theme, SemanticToken::InputText, capabilities),
                )
            })
            .collect()
    };
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_status(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    registry: &ActionRegistry,
    theme: &Theme,
    capabilities: TerminalCapabilities,
) {
    if area.is_empty() {
        return;
    }

    let key_hints = hints::status_key_hints(registry, state);
    let status = if key_hints.is_empty() {
        format!(" {}", state.status())
    } else {
        format!(" {}  {key_hints}", state.status())
    };
    let status = bounded_terminal_line(status, area.width.into(), capabilities);
    frame.render_widget(
        Paragraph::new(status).style(foreground_background_style(
            theme,
            SemanticToken::StatusText,
            SemanticToken::StatusBackground,
            capabilities,
        )),
        area,
    );
}

fn bordered_block(capabilities: TerminalCapabilities) -> Block<'static> {
    let block = Block::bordered();
    if capabilities.supports_unicode() {
        block
    } else {
        block.border_set(ASCII_BORDER_SET)
    }
}

fn skip_display_cols(text: &str, skip: usize) -> String {
    if skip == 0 {
        return text.to_owned();
    }
    let mut index = 0_usize;
    let mut width = 0_usize;
    while index < text.len() && width < skip {
        let next = next_grapheme_boundary(text, index);
        width = width.saturating_add(display_width(&text[index..next]));
        index = next;
    }
    text[index..].to_owned()
}

fn bounded_terminal_line(
    text: impl AsRef<str>,
    width: usize,
    capabilities: TerminalCapabilities,
) -> String {
    let clean = sanitize_terminal_text(text);
    let line = clean.lines().next().unwrap_or_default();
    if capabilities.supports_unicode() {
        truncate_display_width(line, width)
    } else {
        let ascii = line
            .chars()
            .map(|character| match character {
                '…' => '.',
                _ if character.is_ascii() => character,
                _ => '?',
            })
            .collect::<String>();
        truncate_ascii_line(&ascii, width.min(MAX_PLAIN_WIDTH))
    }
}

fn truncate_ascii_line(text: &str, width: usize) -> String {
    if text.len() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }

    let mut rendered = String::with_capacity(width);
    rendered.push_str(&text[..width - 1]);
    rendered.push('.');
    rendered
}

fn panel_style(theme: &Theme, capabilities: TerminalCapabilities) -> Style {
    foreground_background_style(
        theme,
        SemanticToken::Border,
        SemanticToken::Surface,
        capabilities,
    )
}

fn input_panel_style(theme: &Theme, capabilities: TerminalCapabilities) -> Style {
    foreground_background_style(
        theme,
        SemanticToken::BorderFocus,
        SemanticToken::InputBackground,
        capabilities,
    )
}

fn background_style(
    theme: &Theme,
    token: SemanticToken,
    capabilities: TerminalCapabilities,
) -> Style {
    if capabilities.supports_color() {
        Style::default().bg(terminal_color(theme.color(token), capabilities.color()))
    } else {
        Style::default()
    }
}

fn foreground_background_style(
    theme: &Theme,
    foreground: SemanticToken,
    background: SemanticToken,
    capabilities: TerminalCapabilities,
) -> Style {
    if capabilities.supports_color() {
        Style::default()
            .fg(terminal_color(
                theme.color(foreground),
                capabilities.color(),
            ))
            .bg(terminal_color(
                theme.color(background),
                capabilities.color(),
            ))
    } else {
        Style::default()
    }
}

fn nearest_basic_color(color: Rgb) -> Color {
    const BASIC: [(Rgb, Color); 16] = [
        (Rgb::new(0x00, 0x00, 0x00), Color::Black),
        (Rgb::new(0x80, 0x00, 0x00), Color::Red),
        (Rgb::new(0x00, 0x80, 0x00), Color::Green),
        (Rgb::new(0x80, 0x80, 0x00), Color::Yellow),
        (Rgb::new(0x00, 0x00, 0x80), Color::Blue),
        (Rgb::new(0x80, 0x00, 0x80), Color::Magenta),
        (Rgb::new(0x00, 0x80, 0x80), Color::Cyan),
        (Rgb::new(0xc0, 0xc0, 0xc0), Color::Gray),
        (Rgb::new(0x80, 0x80, 0x80), Color::DarkGray),
        (Rgb::new(0xff, 0x00, 0x00), Color::LightRed),
        (Rgb::new(0x00, 0xff, 0x00), Color::LightGreen),
        (Rgb::new(0xff, 0xff, 0x00), Color::LightYellow),
        (Rgb::new(0x00, 0x00, 0xff), Color::LightBlue),
        (Rgb::new(0xff, 0x00, 0xff), Color::LightMagenta),
        (Rgb::new(0x00, 0xff, 0xff), Color::LightCyan),
        (Rgb::new(0xff, 0xff, 0xff), Color::White),
    ];

    BASIC
        .iter()
        .min_by_key(|(candidate, _)| color_distance(color, *candidate))
        .map_or(Color::White, |(_, mapped)| *mapped)
}

fn color_distance(first: Rgb, second: Rgb) -> i32 {
    let red = i32::from(first.red) - i32::from(second.red);
    let green = i32::from(first.green) - i32::from(second.green);
    let blue = i32::from(first.blue) - i32::from(second.blue);
    let first_chroma = i32::from(first.red.max(first.green).max(first.blue))
        - i32::from(first.red.min(first.green).min(first.blue));
    let second_chroma = i32::from(second.red.max(second.green).max(second.blue))
        - i32::from(second.red.min(second.green).min(second.blue));
    let chroma = first_chroma - second_chroma;
    let red_green = (i32::from(first.red) - i32::from(first.green))
        - (i32::from(second.red) - i32::from(second.green));
    let green_blue = (i32::from(first.green) - i32::from(first.blue))
        - (i32::from(second.green) - i32::from(second.blue));
    let blue_red = (i32::from(first.blue) - i32::from(first.red))
        - (i32::from(second.blue) - i32::from(second.red));
    red * red
        + green * green
        + blue * blue
        + 2 * chroma * chroma
        + 2 * (red_green * red_green + green_blue * green_blue + blue_red * blue_red)
}
