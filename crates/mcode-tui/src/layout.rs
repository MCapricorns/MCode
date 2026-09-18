//! Shared geometry for rendering and transcript navigation.
//!
//! The renderer and scroll actions consume the same calculated transcript
//! region so wrapping, page steps, and offset clamping stay aligned.
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::Block;

use crate::logo::LOGO_ROWS;
use crate::state::{AppState, Viewport};

/// Maximum visible rows inside the multiline input panel.
const MAX_INPUT_INNER_LINES: usize = 6;

/// Rectangles occupied by the complete application view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ViewLayout {
    pub(crate) logo: Rect,
    pub(crate) body: Rect,
    pub(crate) transcript: Rect,
    pub(crate) input: Rect,
    pub(crate) status: Rect,
}

/// Calculates view geometry for `state` inside `area`.
#[must_use]
pub(crate) fn view_layout(area: Rect, state: &AppState) -> ViewLayout {
    let status_height = u16::from(area.height > 0);
    let extra_input_lines = if state.interaction().is_some() || state.input().is_empty() {
        0
    } else {
        state
            .input()
            .split('\n')
            .count()
            .saturating_sub(1)
            .min(MAX_INPUT_INNER_LINES.saturating_sub(1))
    };
    let input_height = if area.height >= 4 {
        3_u16
            .saturating_add(u16::try_from(extra_input_lines).unwrap_or(0))
            .min(area.height.saturating_sub(status_height.saturating_add(1)))
    } else {
        area.height.saturating_sub(status_height)
    };
    let logo_height = if state.interaction().is_some() {
        0
    } else {
        let logo_budget = area.height.saturating_sub(status_height + input_height + 1);
        LOGO_ROWS.min(logo_budget)
    };

    let [logo, body, input, status] = Layout::vertical([
        Constraint::Length(logo_height),
        Constraint::Fill(1),
        Constraint::Length(input_height),
        Constraint::Length(status_height),
    ])
    .areas(area);
    let transcript = Block::bordered().inner(body);

    ViewLayout {
        logo,
        body,
        transcript,
        input,
        status,
    }
}

/// Returns the exact transcript dimensions used for `state`.
#[must_use]
pub(crate) fn transcript_viewport(state: &AppState) -> Viewport {
    let viewport = state.viewport();
    let area = Rect::new(0, 0, viewport.width, viewport.height);
    let transcript = view_layout(area, state).transcript;
    Viewport::new(transcript.width, transcript.height)
}
