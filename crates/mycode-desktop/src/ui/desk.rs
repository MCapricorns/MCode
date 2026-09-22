//! Day and night palettes: warm paper and ink, one honey accent.
//!
//! Colors apply by overriding the resolved [`Theme`] after every
//! `Theme::change`, including the button tokens GPUI actually paints.
//! Signal lamps stay muted and are not washed across every surface.

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
/// every [`Theme::change`] so the day/night choice in settings keeps working.
pub fn apply(theme: &mut Theme) {
    theme.radius = gpui_kit::px(8.);
    theme.radius_lg = gpui_kit::px(12.);
    theme.shadow = true;
    if theme.mode == ThemeMode::Dark {
        apply_night(theme);
    } else {
        apply_day(theme);
    }
    sync_controls(theme);
}

fn apply_night(theme: &mut Theme) {
    let bg = hex(0x110F0D);
    let panel = hex(0x1A1714);
    let panel_2 = hex(0x24201C);
    let line = hex(0x3C342C);
    let ink = hex(0xF6F1EA);
    let ink_dim = hex(0xC4B8AA);
    let ink_faint = hex(0x8A7D70);
    let honey = hex(0xE0B15A);
    let honey_deep = hex(0xC48A3A);
    let honey_ink = hex(0x1A140C);
    let sage = hex(0x8FBF9F);
    let coral = hex(0xE08B7A);
    let dust = hex(0x8FB4C4);
    let lilac = hex(0xC6B4D4);

    theme.background = bg;
    theme.foreground = ink;
    theme.muted = panel_2;
    theme.muted_foreground = ink_dim;
    theme.border = line;
    theme.secondary = panel_2;
    theme.secondary_foreground = ink;
    theme.secondary_hover = hex(0x2C2722);
    theme.secondary_active = hex(0x342E28);
    theme.accent = hex_a(0xE0B15A, 0.16);
    theme.accent_foreground = honey;
    theme.caret = honey;
    theme.selection = hex_a(0xE0B15A, 0.28);
    theme.primary = honey;
    theme.primary_foreground = honey_ink;
    theme.primary_hover = hex(0xE8C27A);
    theme.primary_active = honey_deep;
    theme.link = honey;
    theme.link_hover = hex(0xE8C27A);
    theme.link_active = honey_deep;
    theme.info = dust;
    theme.info_foreground = honey_ink;
    theme.success = sage;
    theme.success_foreground = honey_ink;
    theme.warning = honey;
    theme.warning_foreground = honey_ink;
    theme.danger = coral;
    theme.danger_foreground = honey_ink;
    theme.input = line;
    theme.ring = honey;

    theme.sidebar = panel;
    theme.sidebar_foreground = ink_dim;
    theme.sidebar_border = line;
    theme.sidebar_accent = panel_2;
    theme.sidebar_accent_foreground = ink;
    theme.sidebar_primary = honey;
    theme.sidebar_primary_foreground = honey_ink;

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
    theme.scrollbar_thumb = hex(0x3C342C);
    theme.scrollbar_thumb_hover = hex(0x52483E);
    theme.window_border = line;
    theme.overlay = hex_a(0x110F0D, 0.55);

    theme.green = sage;
    theme.green_light = hex(0xA8D4B6);
    theme.red = coral;
    theme.red_light = hex(0xE8A89C);
    theme.blue = dust;
    theme.blue_light = hex(0xB3CDD8);
    theme.yellow = honey;
    theme.yellow_light = hex(0xE8C27A);
    theme.magenta = lilac;
    theme.magenta_light = hex(0xD8CCE2);
    theme.cyan = dust;
    theme.cyan_light = hex(0xB3CDD8);
}

fn apply_day(theme: &mut Theme) {
    let bg = hex(0xF6F1E8);
    let panel = hex(0xFFFCF8);
    let panel_2 = hex(0xF3ECE3);
    let line = hex(0xE4D9CC);
    let line_soft = hex(0xEFE6DA);
    let ink = hex(0x1C1712);
    let ink_dim = hex(0x6A5E52);
    let ink_faint = hex(0x9C8E7E);
    let honey = hex(0xA15C28);
    let honey_deep = hex(0x7C4318);
    let cream = hex(0xFFF8F0);
    let sage = hex(0x2F7D52);
    let coral = hex(0xC4543E);
    let dust = hex(0x3D6E86);
    let lilac = hex(0x6E5688);

    theme.background = bg;
    theme.foreground = ink;
    theme.muted = panel_2;
    theme.muted_foreground = ink_dim;
    theme.border = line;
    theme.secondary = panel;
    theme.secondary_foreground = ink;
    theme.secondary_hover = line_soft;
    theme.secondary_active = line;
    theme.accent = hex_a(0xA15C28, 0.12);
    theme.accent_foreground = honey;
    theme.caret = honey;
    theme.selection = hex_a(0xA15C28, 0.18);
    theme.primary = honey;
    theme.primary_foreground = cream;
    theme.primary_hover = hex(0xB56A32);
    theme.primary_active = honey_deep;
    theme.link = honey;
    theme.link_hover = honey_deep;
    theme.link_active = honey_deep;
    theme.info = dust;
    theme.info_foreground = cream;
    theme.success = sage;
    theme.success_foreground = cream;
    theme.warning = honey;
    theme.warning_foreground = cream;
    theme.danger = coral;
    theme.danger_foreground = cream;
    theme.input = line;
    theme.ring = honey;

    theme.sidebar = panel;
    theme.sidebar_foreground = ink_dim;
    theme.sidebar_border = line;
    theme.sidebar_accent = panel_2;
    theme.sidebar_accent_foreground = ink;
    theme.sidebar_primary = honey;
    theme.sidebar_primary_foreground = cream;

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
    theme.scrollbar_thumb = hex(0xD9CDBE);
    theme.scrollbar_thumb_hover = hex(0xC4B5A2);
    theme.window_border = line;
    theme.overlay = hex_a(0x1C1712, 0.16);

    theme.green = sage;
    theme.green_light = sage;
    theme.red = coral;
    theme.red_light = coral;
    theme.blue = dust;
    theme.blue_light = dust;
    theme.yellow = honey;
    theme.yellow_light = hex(0xC4844A);
    theme.magenta = lilac;
    theme.magenta_light = lilac;
    theme.cyan = dust;
    theme.cyan_light = dust;
}

/// Copies the desk fills into the token slots kit buttons actually read.
///
/// `Theme::change` resets `tokens.button_primary` to the stock light-theme
/// near-black. Painting only `theme.primary` leaves that black button behind.
fn sync_controls(theme: &mut Theme) {
    theme.button_primary = theme.primary;
    theme.button_primary_hover = theme.primary_hover;
    theme.button_primary_active = theme.primary_active;
    theme.button_primary_foreground = theme.primary_foreground;
    theme.tokens.button_primary = theme.primary.into();
    theme.tokens.button_primary_hover = theme.primary_hover.into();
    theme.tokens.button_primary_active = theme.primary_active.into();
    theme.tokens.button_primary_foreground = theme.primary_foreground.into();

    theme.button = theme.secondary;
    theme.button_hover = theme.secondary_hover;
    theme.button_active = theme.secondary_active;
    theme.button_foreground = theme.foreground;
    theme.tokens.button = theme.secondary.into();
    theme.tokens.button_hover = theme.secondary_hover.into();
    theme.tokens.button_active = theme.secondary_active.into();
    theme.tokens.button_foreground = theme.foreground.into();

    theme.tokens.primary = theme.primary.into();
    theme.tokens.primary_hover = theme.primary_hover.into();
    theme.tokens.primary_active = theme.primary_active.into();
    theme.tokens.primary_foreground = theme.primary_foreground.into();
    theme.tokens.secondary = theme.secondary.into();
    theme.tokens.secondary_foreground = theme.foreground.into();
}

/// Semantic Desk tokens for the signal colors and the faint ink level.
pub struct Desk {
    pub amber: Hsla,
    pub green: Hsla,
    pub red: Hsla,
    pub cyan: Hsla,
    pub violet: Hsla,
    pub faint: Hsla,
    pub screen: Hsla,
    pub screen_dim: Hsla,
}

impl Desk {
    pub fn of(theme: &Theme) -> Self {
        if theme.mode == ThemeMode::Dark {
            Self {
                amber: hex(0xE0B15A),
                green: hex(0x8FBF9F),
                red: hex(0xE08B7A),
                cyan: hex(0x8FB4C4),
                violet: hex(0xC6B4D4),
                faint: hex(0x8A7D70),
                screen: hex(0x161310),
                screen_dim: hex(0xC4B8AA),
            }
        } else {
            Self {
                amber: hex(0xA15C28),
                green: hex(0x2F7D52),
                red: hex(0xC4543E),
                cyan: hex(0x3D6E86),
                violet: hex(0x6E5688),
                faint: hex(0x9C8E7E),
                screen: hex(0xFFFCF8),
                screen_dim: hex(0x6A5E52),
            }
        }
    }
}
