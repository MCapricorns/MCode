//! Warm paper panels and a same-hue honey wash over the active theme.
//! Day stays light and night stays dark. Gradients stay inside one family
//! so the window does not read as a mint-to-violet rainbow.
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

/// Paper panel fill used by the composer card.
pub(super) fn glass(theme: &Theme) -> Hsla {
    let dark = is_dark(theme);
    let base = mix(
        theme.background,
        theme.yellow,
        if dark { 0.05 } else { 0.04 },
    );
    Hsla {
        a: if dark { 0.86 } else { 0.94 },
        ..base
    }
}

/// Sidebar / inspector fill, slightly warmer than the page.
pub(super) fn glass_sidebar(theme: &Theme) -> Hsla {
    let dark = is_dark(theme);
    let base = mix(theme.sidebar, theme.yellow, if dark { 0.05 } else { 0.03 });
    Hsla {
        a: if dark { 0.90 } else { 0.96 },
        ..base
    }
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
/// parent cannot stretch it full width.
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
            .rounded(px(2.))
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .text_color(color)
            .child(label.to_owned()),
    )
}

/// The shared floating-panel recipe: hairline glass border, near-opaque
/// popover fill, popover ink, and a soft shadow at the desk's tight radius.
pub(super) fn popover_panel(id: impl Into<gpui_kit::ElementId>, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .rounded(px(3.))
        .border_1()
        .border_color(glass_border(theme))
        .bg(popover(theme))
        .text_color(theme.popover_foreground)
        .shadow_lg()
}
