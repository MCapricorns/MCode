//! Frosted panels and a same-hue honey wash over the active theme.
//! Day stays light and night stays dark. Every resting surface uses one
//! translucent recipe so a control is never a solid slab beside a bare one.
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::{
    Background, Div, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Stateful,
    Styled as _, div, linear_color_stop, linear_gradient, px,
};

fn is_dark(theme: &Theme) -> bool {
    theme.mode == ThemeMode::Dark
}

/// Linear interpolation between two colors in HSL space; `t` 0 keeps `from`.
fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    Hsla {
        h: from.h + (to.h - from.h) * t,
        s: from.s + (to.s - from.s) * t,
        l: from.l + (to.l - from.l) * t,
        a: from.a + (to.a - from.a) * t,
    }
}

/// Ambient wash behind the window: a whisper of honey, same hue both ends.
pub(super) fn ambient(theme: &Theme) -> Background {
    let dark = is_dark(theme);
    let start = mix(
        theme.background,
        theme.yellow,
        if dark { 0.07 } else { 0.10 },
    );
    let end = mix(
        theme.background,
        theme.secondary,
        if dark { 0.40 } else { 0.55 },
    );
    linear_gradient(
        168.,
        linear_color_stop(start, 0.),
        linear_color_stop(end, 1.),
    )
}

/// Chrome wash (composer strip, tape). The ambient gradient shows through.
pub(super) fn glass(theme: &Theme) -> Hsla {
    let dark = is_dark(theme);
    let base = mix(
        theme.background,
        theme.yellow,
        if dark { 0.08 } else { 0.06 },
    );
    Hsla {
        a: if dark { 0.58 } else { 0.64 },
        ..base
    }
}

/// Sidebar / inspector veil, a little denser than the page wash.
pub(super) fn glass_sidebar(theme: &Theme) -> Hsla {
    let dark = is_dark(theme);
    let base = mix(theme.sidebar, theme.yellow, if dark { 0.08 } else { 0.05 });
    Hsla {
        a: if dark { 0.62 } else { 0.70 },
        ..base
    }
}

/// Shared card and quiet-button fill. Stays in the page hue so a short
/// label does not sit in an opaque white or gray slab.
pub(super) fn frost(theme: &Theme) -> Hsla {
    let dark = is_dark(theme);
    let base = mix(
        theme.background,
        theme.secondary,
        if dark { 0.40 } else { 0.50 },
    );
    Hsla {
        a: if dark { 0.46 } else { 0.55 },
        ..base
    }
}

/// Denser glass for dialogs and form cards, still not a solid panel.
pub(super) fn frost_card(theme: &Theme) -> Hsla {
    let dark = is_dark(theme);
    let base = mix(
        theme.background,
        theme.secondary,
        if dark { 0.62 } else { 0.78 },
    );
    Hsla {
        a: if dark { 0.78 } else { 0.82 },
        ..base
    }
}

/// Hover veil. Denser than [`frost`] so it reads on both the page and a card.
pub(super) fn frost_hover(theme: &Theme) -> Hsla {
    let dark = is_dark(theme);
    let base = mix(
        theme.secondary,
        theme.yellow,
        if dark { 0.10 } else { 0.08 },
    );
    Hsla {
        a: if dark { 0.58 } else { 0.68 },
        ..base
    }
}

/// Honey-tinted glass for the emphasized action in a pair.
pub(super) fn frost_accent(theme: &Theme) -> Hsla {
    let dark = is_dark(theme);
    let base = mix(
        theme.background,
        theme.yellow,
        if dark { 0.42 } else { 0.28 },
    );
    Hsla {
        a: if dark { 0.34 } else { 0.28 },
        ..base
    }
}

/// Pressed / hovered emphasized action.
pub(super) fn frost_accent_hover(theme: &Theme) -> Hsla {
    let base = frost_accent(theme);
    Hsla {
        a: (base.a + 0.14).min(0.62),
        ..base
    }
}

/// Card corner radius.
pub(super) fn radius_card() -> gpui_kit::Pixels {
    px(12.)
}

/// Button and chip corner radius.
pub(super) fn radius_control() -> gpui_kit::Pixels {
    px(10.)
}

/// Matched pair of welcome / picker actions. Both share height, radius,
/// padding, and ink. `emphasized` is a honey wash, not a solid fill.
pub(super) fn glass_button(
    id: impl Into<gpui_kit::ElementId>,
    emphasized: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let fill = if emphasized {
        frost_accent(theme)
    } else {
        frost(theme)
    };
    let hover = if emphasized {
        frost_accent_hover(theme)
    } else {
        frost_hover(theme)
    };
    let border = if emphasized {
        theme
            .yellow
            .opacity(if is_dark(theme) { 0.55 } else { 0.42 })
    } else {
        glass_border(theme)
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
        .text_color(theme.foreground)
        .text_sm()
        .cursor_pointer()
        .hover(move |style| style.bg(hover).border_color(border))
}

/// Hairline border, warmed a little so it does not read as cold gray.
pub(super) fn glass_border(theme: &Theme) -> Hsla {
    let dark = is_dark(theme);
    let base = mix(theme.border, theme.yellow, if dark { 0.18 } else { 0.12 });
    Hsla {
        a: if dark { 0.70 } else { 0.85 },
        ..base
    }
}

/// Near-opaque popover fill.
pub(super) fn popover(theme: &Theme) -> Hsla {
    let base = mix(theme.popover, theme.yellow, 0.04);
    Hsla { a: 0.97, ..base }
}

/// Scrim drawn over the app behind an open menu layer.
pub(super) fn scrim(theme: &Theme) -> Hsla {
    if is_dark(theme) {
        Hsla {
            a: 0.55,
            ..theme.background
        }
    } else {
        Hsla {
            a: 0.22,
            ..theme.foreground
        }
    }
}

/// Short same-hue accent for the welcome mark: honey into a deeper honey.
pub(super) fn accent(theme: &Theme, angle: f32) -> Background {
    let deep = mix(theme.yellow, theme.red, 0.28);
    linear_gradient(
        angle,
        linear_color_stop(theme.yellow, 0.),
        linear_color_stop(deep, 1.),
    )
}

/// A small mono tag chip: the bordered, letterspaced desk label used for
/// ledger tags and capability chips. The chip sits in a row so a flex-col
/// parent cannot stretch it full width. The wash matches the border hue so
/// a tag is never a hollow outline next to a filled card.
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
            .bg(color.opacity(0.12))
            .rounded(radius_control())
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .text_color(color)
            .child(label.to_owned()),
    )
}

/// The shared floating-panel recipe: hairline glass border, popover fill,
/// popover ink, and a soft shadow.
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
