//! Frosted translucent panels and gradient accents over the active theme.
//! Day and night each stay on their own surface: light stays light, dark
//! stays dark, with a mint-to-violet ambient wash behind the glass.
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::{
    Background, Div, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Stateful,
    Styled as _, black, div, linear_color_stop, linear_gradient, px,
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

/// Rotates a color's hue for gradient endpoints.
fn hue_shift(color: Hsla, degrees: f32) -> Hsla {
    Hsla {
        h: color.h + degrees,
        ..color
    }
}

/// Ambient gradient behind the window: sky easing toward mint and violet.
pub(super) fn ambient(theme: &Theme) -> Background {
    let dark = is_dark(theme);
    let start = mix(theme.background, theme.cyan, if dark { 0.16 } else { 0.10 });
    let end = mix(
        theme.background,
        theme.magenta,
        if dark { 0.18 } else { 0.08 },
    );
    linear_gradient(
        152.,
        linear_color_stop(start, 0.),
        linear_color_stop(end, 1.),
    )
}

/// Frosted panel fill used by the composer card.
pub(super) fn glass(theme: &Theme) -> Hsla {
    let dark = is_dark(theme);
    let base = mix(theme.background, theme.cyan, if dark { 0.10 } else { 0.06 });
    Hsla {
        a: if dark { 0.62 } else { 0.78 },
        ..base
    }
}

/// Frosted sidebar / inspector fill, slightly violet-tinted.
pub(super) fn glass_sidebar(theme: &Theme) -> Hsla {
    let dark = is_dark(theme);
    let base = mix(theme.sidebar, theme.magenta, if dark { 0.12 } else { 0.06 });
    Hsla {
        a: if dark { 0.52 } else { 0.72 },
        ..base
    }
}

/// Border tone matching the frosted panels.
pub(super) fn glass_border(theme: &Theme) -> Hsla {
    let dark = is_dark(theme);
    let base = mix(theme.border, theme.cyan, if dark { 0.22 } else { 0.12 });
    Hsla {
        a: if dark { 0.55 } else { 0.60 },
        ..base
    }
}

/// Near-opaque frosted popover fill.
pub(super) fn popover(theme: &Theme) -> Hsla {
    let base = mix(theme.popover, theme.magenta, 0.06);
    Hsla { a: 0.94, ..base }
}

/// Scrim drawn over the app behind an open menu layer.
pub(super) fn scrim(theme: &Theme) -> Hsla {
    Hsla {
        a: if is_dark(theme) { 0.35 } else { 0.10 },
        ..black()
    }
}

/// Brand gradient for welcome and hero accents: mint → sky → violet.
pub(super) fn accent(theme: &Theme, angle: f32) -> Background {
    linear_gradient(
        angle,
        linear_color_stop(theme.green, 0.),
        linear_color_stop(hue_shift(theme.magenta, 12.), 1.),
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
