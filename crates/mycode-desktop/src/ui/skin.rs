//! Flat fills and borders for the Desk look (docs/design/demo.html): opaque
//! panel surfaces on hairlines — the old glassmorphism helpers (translucent
//! glass, gradient ambient, brand gradients) are gone with the redesign.
//! Only what the surviving callsites use remains.
use gpui_kit::Hsla;
use gpui_kit::component::theme::{Theme, ThemeMode};

fn is_dark(theme: &Theme) -> bool {
    theme.mode == ThemeMode::Dark
}

/// Flat card fill matching `.tool`/`.composer` panels.
pub(super) fn glass(theme: &Theme) -> Hsla {
    theme.muted
}

/// Flat rail fill matching `.rail`/`.insp` panels.
pub(super) fn glass_sidebar(theme: &Theme) -> Hsla {
    theme.sidebar
}

/// Hairline border for panels and cards.
pub(super) fn glass_border(theme: &Theme) -> Hsla {
    theme.border
}

/// Floating menu fill: the second panel tone, opaque so overlapping rows stay
/// readable without a blur trick.
pub(super) fn popover(theme: &Theme) -> Hsla {
    theme.popover
}

/// Scrim drawn over the app behind an open menu layer.
pub(super) fn scrim(theme: &Theme) -> Hsla {
    Hsla {
        a: if is_dark(theme) { 0.35 } else { 0.12 },
        ..gpui_kit::black()
    }
}
