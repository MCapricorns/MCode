//! Solid panels over a visible page gradient.
//!
//! The window background is the gradient. Rails, dialogs, chips, and the
//! composer are opaque so a label never disappears into the wash.
use gpui_kit::component::theme::Theme;
use gpui_kit::{
    Background, Div, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Stateful,
    Styled as _, div, linear_color_stop, linear_gradient, px,
};

/// Page gradient. The two stops are far enough apart to read in the chat
/// column, and close enough that body text stays legible on both.
pub(super) fn ambient(theme: &Theme) -> Background {
    linear_gradient(
        155.,
        linear_color_stop(theme.background, 0.),
        linear_color_stop(theme.status_bar, 1.),
    )
}

/// Title bar and composer strip.
pub(super) fn glass(theme: &Theme) -> Hsla {
    theme.title_bar
}

/// Sidebar and inspector.
pub(super) fn glass_sidebar(theme: &Theme) -> Hsla {
    theme.sidebar
}

/// Quiet control fill.
pub(super) fn frost(theme: &Theme) -> Hsla {
    theme.secondary
}

/// Dialog and form card fill.
pub(super) fn frost_card(theme: &Theme) -> Hsla {
    theme.popover
}

/// Toast fill. Opaque so one line of ink stays readable.
pub(super) fn toast_fill(theme: &Theme) -> Hsla {
    theme.popover
}

/// Hover fill.
pub(super) fn frost_hover(theme: &Theme) -> Hsla {
    theme.secondary_hover
}

/// Selected-row fill. Opaque tint, not a translucent accent.
pub(super) fn frost_accent(theme: &Theme) -> Hsla {
    theme.accent
}

/// Card corner radius.
pub(super) fn radius_card() -> gpui_kit::Pixels {
    px(12.)
}

/// Button and chip corner radius.
pub(super) fn radius_control() -> gpui_kit::Pixels {
    px(8.)
}

/// Matched pair of welcome / picker actions.
pub(super) fn glass_button(
    id: impl Into<gpui_kit::ElementId>,
    emphasized: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let fill = if emphasized {
        theme.primary
    } else {
        theme.secondary
    };
    let ink = if emphasized {
        theme.primary_foreground
    } else {
        theme.foreground
    };
    let hover = if emphasized {
        theme.primary
    } else {
        theme.secondary_hover
    };
    let border = if emphasized {
        theme.primary
    } else {
        theme.border
    };
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap_2()
        .h(px(36.))
        .px(px(14.))
        .rounded(radius_control())
        .border_1()
        .border_color(border)
        .bg(fill)
        .text_color(ink)
        .text_sm()
        .cursor_pointer()
        .hover(move |style| style.bg(hover).border_color(border))
}

/// Hairline border.
pub(super) fn glass_border(theme: &Theme) -> Hsla {
    theme.border
}

/// Menu fill. Opaque.
pub(super) fn popover(theme: &Theme) -> Hsla {
    theme.popover
}

/// Scrim drawn over the app behind an open menu layer.
pub(super) fn scrim(theme: &Theme) -> Hsla {
    theme.overlay
}

/// A small mono tag chip.
pub(super) fn mono_chip(label: &str, color: Hsla, border: Hsla, theme: &Theme) -> impl IntoElement {
    div().flex().flex_row().child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .px(px(7.))
            .py(px(2.))
            .border_1()
            .border_color(border)
            .bg(theme.secondary)
            .rounded(radius_control())
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .text_color(color)
            .child(label.to_owned()),
    )
}

/// The shared floating-panel recipe.
pub(super) fn popover_panel(id: impl Into<gpui_kit::ElementId>, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .rounded(radius_card())
        .border_1()
        .border_color(glass_border(theme))
        .bg(popover(theme))
        .text_color(theme.popover_foreground)
        .shadow_lg()
}
