//! Shared visual skin: frosted translucent panels and gradient accents
//! layered over the active theme, tuned for both light and dark appearances.
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::{Background, Hsla, black, linear_color_stop, linear_gradient};

fn is_dark(theme: &Theme) -> bool {
    theme.mode == ThemeMode::Dark
}

/// Linear interpolation between two colors in HSL space; `t` 0 keeps `from`.
pub(super) fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    Hsla {
        h: from.h + (to.h - from.h) * t,
        s: from.s + (to.s - from.s) * t,
        l: from.l + (to.l - from.l) * t,
        a: from.a + (to.a - from.a) * t,
    }
}

/// Rotates a color's hue for gradient endpoints.
pub(super) fn hue_shift(color: Hsla, degrees: f32) -> Hsla {
    Hsla {
        h: color.h + degrees,
        ..color
    }
}

/// The ambient gradient painted behind the whole window content: the theme
/// background easing toward a brand tint.
pub(super) fn ambient(theme: &Theme) -> Background {
    let dark = is_dark(theme);
    let tint = mix(
        theme.background,
        theme.primary,
        if dark { 0.22 } else { 0.12 },
    );
    linear_gradient(
        160.,
        linear_color_stop(theme.background, 0.),
        linear_color_stop(tint, 1.),
    )
}

/// Frosted panel fill: a translucent surface that lets the ambient gradient
/// bleed through, used by the composer card and floating menus.
pub(super) fn glass(theme: &Theme) -> Hsla {
    let dark = is_dark(theme);
    let base = mix(
        theme.background,
        theme.foreground,
        if dark { 0.05 } else { 0.03 },
    );
    Hsla {
        a: if dark { 0.70 } else { 0.80 },
        ..base
    }
}

/// Frosted sidebar fill, slightly tinted toward the brand color.
pub(super) fn glass_sidebar(theme: &Theme) -> Hsla {
    let dark = is_dark(theme);
    let base = mix(theme.sidebar, theme.primary, if dark { 0.12 } else { 0.05 });
    Hsla {
        a: if dark { 0.55 } else { 0.72 },
        ..base
    }
}

/// Border tone matching the frosted panels.
pub(super) fn glass_border(theme: &Theme) -> Hsla {
    let dark = is_dark(theme);
    let base = mix(
        theme.border,
        theme.foreground,
        if dark { 0.12 } else { 0.05 },
    );
    Hsla {
        a: if dark { 0.5 } else { 0.65 },
        ..base
    }
}

/// Near-opaque frosted popover fill that keeps menu text readable while the
/// scrimmed app shows through at the edges.
pub(super) fn popover(theme: &Theme) -> Hsla {
    let base = mix(theme.popover, theme.background, 0.2);
    Hsla { a: 0.94, ..base }
}

/// Scrim drawn over the app behind an open menu layer.
pub(super) fn scrim(theme: &Theme) -> Hsla {
    let dark = is_dark(theme);
    Hsla {
        a: if dark { 0.35 } else { 0.12 },
        ..black()
    }
}

/// Brand gradient for the user bubble, welcome logo, and hero accents.
pub(super) fn accent(theme: &Theme, angle: f32) -> Background {
    linear_gradient(
        angle,
        linear_color_stop(theme.primary, 0.),
        linear_color_stop(hue_shift(theme.primary, 45.), 1.),
    )
}
