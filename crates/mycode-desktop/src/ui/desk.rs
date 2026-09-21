//! The Desk theme set: NIGHT DESK + DAY DESK, taken from docs/design/demo.html.
//!
//! The demo is the visual authority: flat panels, 1px hairlines, near-zero
//! radius, dense mono ledger type — the opposite of the generic glassmorphism
//! skin. Colors apply by overriding the resolved [`Theme`] colors after every
//! `Theme::change`, so all existing render code keeps working; the reskin
//! then reads these tokens via the `desk` module instead of `skin`.

use gpui_kit::Hsla;
use gpui_kit::component::theme::{Theme, ThemeMode};

fn hex(value: u32) -> Hsla {
    gpui_kit::rgb(value).into()
}

fn hex_a(value: u32, alpha: f32) -> Hsla {
    let mut color = hex(value);
    color.a = alpha;
    color
}

/// Applies the Desk palette over the resolved theme colors. Called after
/// every [`Theme::change`] so the day/night choice in settings keeps working
/// — the desk look is a skin over both modes, not a new mode.
pub fn apply(theme: &mut Theme) {
    // The desk is flat: hairline-square radii, no drop shadows.
    theme.radius = gpui_kit::px(3.);
    theme.radius_lg = gpui_kit::px(3.);
    theme.shadow = false;
    if theme.mode == ThemeMode::Dark {
        apply_night(theme);
    } else {
        apply_day(theme);
    }
}

fn apply_night(theme: &mut Theme) {
    let bg = hex(0x0B0D10);
    let panel = hex(0x101318);
    let panel_2 = hex(0x14181F);
    let line = hex(0x22262E);
    let ink = hex(0xE8E5DC);
    let ink_dim = hex(0x9BA0AB);
    let ink_faint = hex(0x5C6270);
    let amber = hex(0xFFB224);
    let green = hex(0x3FB96B);
    let red = hex(0xF05A5A);
    let cyan = hex(0x5AC8FA);
    let violet = hex(0x9D8CFF);

    theme.background = bg;
    theme.foreground = ink;
    theme.muted = panel_2;
    theme.muted_foreground = ink_dim;
    theme.border = line;
    theme.secondary = panel_2;
    theme.secondary_foreground = ink;
    theme.secondary_hover = hex(0x1A1F27);
    theme.secondary_active = hex(0x1E242E);
    theme.accent = hex_a(0xFFB224, 0.16);
    theme.accent_foreground = amber;
    theme.caret = amber;
    theme.selection = hex_a(0xFFB224, 0.28);
    theme.primary = amber;
    theme.primary_foreground = hex(0x14100A);
    theme.primary_hover = hex(0xFFC04D);
    theme.primary_active = hex(0xE69E12);
    theme.link = cyan;
    theme.link_hover = cyan;
    theme.link_active = cyan;
    theme.info = cyan;
    theme.info_foreground = hex(0x0B0D10);
    theme.success = green;
    theme.success_foreground = hex(0x0B0D10);
    theme.warning = amber;
    theme.warning_foreground = hex(0x14100A);
    theme.danger = red;
    theme.danger_foreground = hex(0x14100A);
    theme.input = line;
    theme.ring = amber;

    theme.sidebar = panel;
    theme.sidebar_foreground = ink_dim;
    theme.sidebar_border = line;
    theme.sidebar_accent = panel_2;
    theme.sidebar_accent_foreground = ink;
    theme.sidebar_primary = amber;
    theme.sidebar_primary_foreground = hex(0x14100A);

    theme.popover = panel_2;
    theme.popover_foreground = ink;
    theme.title_bar = panel;
    theme.title_bar_border = line;
    theme.status_bar = panel;
    theme.status_bar_border = line;
    theme.tab_bar = panel;
    theme.tab_active = panel_2;
    theme.tab_active_foreground = ink;
    theme.tab_foreground = ink_dim;
    theme.colors.list = panel;
    theme.colors.list_hover = panel_2;
    theme.colors.list_even = panel;
    theme.colors.list_head = panel;
    theme.table = panel;
    theme.table_hover = panel_2;
    theme.table_even = panel;
    theme.table_head = panel;
    theme.table_head_foreground = ink_faint;
    theme.scrollbar = bg;
    theme.scrollbar_thumb = hex(0x23262E);
    theme.scrollbar_thumb_hover = hex(0x333845);
    theme.window_border = line;
    theme.overlay = hex_a(0x000000, 0.35);

    theme.green = green;
    theme.green_light = hex(0x6BCB8F);
    theme.red = red;
    theme.red_light = hex(0xF0716A);
    theme.blue = cyan;
    theme.blue_light = cyan;
    theme.yellow = amber;
    theme.yellow_light = hex(0xFFC04D);
    theme.magenta = violet;
    theme.magenta_light = violet;
    theme.cyan = cyan;
    theme.cyan_light = cyan;
}

fn apply_day(theme: &mut Theme) {
    let bg = hex(0xF4F1E8);
    let panel = hex(0xFBF9F2);
    let panel_2 = hex(0xF0ECE0);
    let line = hex(0xD9D2BE);
    let line_soft = hex(0xE6E1D1);
    let ink = hex(0x1C1810);
    let ink_dim = hex(0x57503F);
    let ink_faint = hex(0x97896F);
    let amber = hex(0xA66A00);
    let amber_deep = hex(0x8A5700);
    let green = hex(0x1E7A46);
    let red = hex(0xBF3627);
    let cyan = hex(0x0F6E8F);
    let violet = hex(0x6A56C9);

    theme.background = bg;
    theme.foreground = ink;
    theme.muted = panel_2;
    theme.muted_foreground = ink_dim;
    theme.border = line;
    theme.secondary = panel_2;
    theme.secondary_foreground = ink;
    theme.secondary_hover = line_soft;
    theme.secondary_active = hex(0xDED7C2);
    theme.accent = hex_a(0xA66A00, 0.14);
    theme.accent_foreground = amber_deep;
    theme.caret = amber;
    theme.selection = hex_a(0xA66A00, 0.22);
    theme.primary = amber_deep;
    theme.primary_foreground = hex(0xFFF6E4);
    theme.primary_hover = amber;
    theme.primary_active = hex(0x744A00);
    theme.link = cyan;
    theme.link_hover = cyan;
    theme.link_active = cyan;
    theme.info = cyan;
    theme.info_foreground = hex(0xFBF9F2);
    theme.success = green;
    theme.success_foreground = hex(0xFBF9F2);
    theme.warning = amber;
    theme.warning_foreground = hex(0xFFF6E4);
    theme.danger = red;
    theme.danger_foreground = hex(0xFBF9F2);
    theme.input = line;
    theme.ring = amber;

    theme.sidebar = panel;
    theme.sidebar_foreground = ink_dim;
    theme.sidebar_border = line;
    theme.sidebar_accent = panel_2;
    theme.sidebar_accent_foreground = ink;
    theme.sidebar_primary = amber_deep;
    theme.sidebar_primary_foreground = hex(0xFFF6E4);

    theme.popover = panel;
    theme.popover_foreground = ink;
    theme.title_bar = panel;
    theme.title_bar_border = line;
    theme.status_bar = panel;
    theme.status_bar_border = line;
    theme.tab_bar = panel;
    theme.tab_active = panel_2;
    theme.tab_active_foreground = ink;
    theme.tab_foreground = ink_dim;
    theme.colors.list = panel;
    theme.colors.list_hover = panel_2;
    theme.colors.list_even = panel;
    theme.colors.list_head = panel_2;
    theme.table = panel;
    theme.table_hover = panel_2;
    theme.table_even = panel;
    theme.table_head = panel_2;
    theme.table_head_foreground = ink_faint;
    theme.scrollbar = bg;
    theme.scrollbar_thumb = hex(0xCFC8B2);
    theme.scrollbar_thumb_hover = hex(0xB8AF94);
    theme.window_border = line;
    theme.overlay = hex_a(0x1C1810, 0.12);

    theme.green = green;
    theme.green_light = green;
    theme.red = red;
    theme.red_light = red;
    theme.blue = cyan;
    theme.blue_light = cyan;
    theme.yellow = amber;
    theme.yellow_light = amber;
    theme.magenta = violet;
    theme.magenta_light = violet;
    theme.cyan = cyan;
    theme.cyan_light = cyan;
}

/// Semantic Desk tokens for the signal colors and the faint ink level; theme
/// colors cover the rest. The CRT screen stays dark in BOTH modes: a dark
/// terminal on the bright day desk (`.term` in the demo).
pub struct Desk {
    pub amber: Hsla,
    pub green: Hsla,
    pub red: Hsla,
    pub cyan: Hsla,
    pub violet: Hsla,
    pub faint: Hsla,
    pub screen: Hsla,
    pub screen_dim: Hsla,
    pub think_bg: Hsla,
}

impl Desk {
    pub fn of(theme: &Theme) -> Self {
        if theme.mode == ThemeMode::Dark {
            Self {
                amber: hex(0xFFB224),
                green: hex(0x3FB96B),
                red: hex(0xF05A5A),
                cyan: hex(0x5AC8FA),
                violet: hex(0x9D8CFF),
                faint: hex(0x5C6270),
                screen: hex(0x08090C),
                screen_dim: hex(0x8B909B),
                think_bg: hex_a(0xFFFFFF, 0.014),
            }
        } else {
            Self {
                amber: hex(0xA66A00),
                green: hex(0x1E7A46),
                red: hex(0xBF3627),
                cyan: hex(0x0F6E8F),
                violet: hex(0x6A56C9),
                faint: hex(0x97896F),
                screen: hex(0x08090C),
                screen_dim: hex(0x8B909B),
                think_bg: hex_a(0x1C1810, 0.02),
            }
        }
    }
}
