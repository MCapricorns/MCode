//! Day and night palettes: frosted glass over tinted surfaces, not a gray CRT.
//!
//! Colors apply by overriding the resolved [`Theme`] after every
//! `Theme::change`. Semantic lamps (mint, violet, sky, peach) live on
//! [`Desk`]; chrome reads glass fills from `skin`.

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
    theme.radius = gpui_kit::px(12.);
    theme.radius_lg = gpui_kit::px(16.);
    theme.shadow = true;
    if theme.mode == ThemeMode::Dark {
        apply_night(theme);
    } else {
        apply_day(theme);
    }
}

fn apply_night(theme: &mut Theme) {
    let bg = hex(0x0C1220);
    let panel = hex(0x141A2C);
    let panel_2 = hex(0x1A2236);
    let line = hex(0x2A3550);
    let ink = hex(0xE8EEF8);
    let ink_dim = hex(0x9AA8C7);
    let ink_faint = hex(0x6B7896);
    let amber = hex(0xF5C16A);
    let green = hex(0x7DCEA0);
    let red = hex(0xF07178);
    let cyan = hex(0x7DD3FC);
    let violet = hex(0xC4B5FD);

    theme.background = bg;
    theme.foreground = ink;
    theme.muted = panel_2;
    theme.muted_foreground = ink_dim;
    theme.border = line;
    theme.secondary = panel_2;
    theme.secondary_foreground = ink;
    theme.secondary_hover = hex(0x1A1F27);
    theme.secondary_active = hex(0x1E242E);
    theme.accent = hex_a(0x7DD3FC, 0.18);
    theme.accent_foreground = cyan;
    theme.caret = cyan;
    theme.selection = hex_a(0xC4B5FD, 0.28);
    theme.primary = cyan;
    theme.primary_foreground = hex(0x0C1220);
    theme.primary_hover = hex(0xA5E6FF);
    theme.primary_active = hex(0x38BDF8);
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
    theme.ring = cyan;

    theme.sidebar = panel;
    theme.sidebar_foreground = ink_dim;
    theme.sidebar_border = line;
    theme.sidebar_accent = panel_2;
    theme.sidebar_accent_foreground = ink;
    theme.sidebar_primary = cyan;
    theme.sidebar_primary_foreground = hex(0x0C1220);

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
    let bg = hex(0xF3F7FB);
    let panel = hex(0xFBFDFF);
    let panel_2 = hex(0xEEF4F8);
    let line = hex(0xD5E0EC);
    let line_soft = hex(0xE4EDF4);
    let ink = hex(0x1A2332);
    let ink_dim = hex(0x5A6B80);
    let ink_faint = hex(0x8A9BB0);
    let amber = hex(0xC47A12);
    let green = hex(0x2F9A64);
    let red = hex(0xD4524A);
    let cyan = hex(0x0284C7);
    let violet = hex(0x7C5CBF);

    theme.background = bg;
    theme.foreground = ink;
    theme.muted = panel_2;
    theme.muted_foreground = ink_dim;
    theme.border = line;
    theme.secondary = panel_2;
    theme.secondary_foreground = ink;
    theme.secondary_hover = line_soft;
    theme.secondary_active = hex(0xD5E4F0);
    theme.accent = hex_a(0x0284C7, 0.12);
    theme.accent_foreground = cyan;
    theme.caret = cyan;
    theme.selection = hex_a(0x7C5CBF, 0.18);
    theme.primary = cyan;
    theme.primary_foreground = hex(0xFBFDFF);
    theme.primary_hover = hex(0x0EA5E9);
    theme.primary_active = hex(0x0369A1);
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
    theme.ring = cyan;

    theme.sidebar = panel;
    theme.sidebar_foreground = ink_dim;
    theme.sidebar_border = line;
    theme.sidebar_accent = panel_2;
    theme.sidebar_accent_foreground = ink;
    theme.sidebar_primary = cyan;
    theme.sidebar_primary_foreground = hex(0xFBFDFF);

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
/// colors cover the rest. Day mode keeps tool output on a light surface so
/// the window is not a dark CRT on a light desk.
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
                amber: hex(0xF5C16A),
                green: hex(0x7DCEA0),
                red: hex(0xF07178),
                cyan: hex(0x7DD3FC),
                violet: hex(0xC4B5FD),
                faint: hex(0x6B7896),
                screen: hex(0x0A101C),
                screen_dim: hex(0x9AA8C7),
                think_bg: hex_a(0xC4B5FD, 0.08),
            }
        } else {
            Self {
                amber: hex(0xC47A12),
                green: hex(0x2F9A64),
                red: hex(0xD4524A),
                cyan: hex(0x0284C7),
                violet: hex(0x7C5CBF),
                faint: hex(0x8A9BB0),
                screen: hex(0xF3F7FB),
                screen_dim: hex(0x5A6B80),
                think_bg: hex_a(0x7C5CBF, 0.08),
            }
        }
    }
}
