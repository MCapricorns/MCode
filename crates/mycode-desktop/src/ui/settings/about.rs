//! The About settings page: version, self-update status, and the provider
//! catalog snapshot date.
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::{AnyElement, Context, IntoElement, ParentElement, Styled, div};

use super::widgets::{settings_card, settings_row};
use crate::ui::ellipsis;
use crate::view_model::UpdateState;
use crate::workspace::Workspace;

pub(super) fn render_about_section(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let vm = workspace.vm();
    let current = mycode_app::current_version().to_owned();
    let catalog_line = match vm.catalog_fetched_at {
        0 => "bundled snapshot".to_owned(),
        seconds => {
            let date = catalog_date(seconds);
            format!("cloud catalog \u{b7} fetched {date}")
        }
    };
    let auto_update = vm.auto_update;
    let status: (String, Option<AnyElement>) = match &vm.update {
        UpdateState::Idle => ("Update checks run at startup.".to_owned(), None),
        UpdateState::Checking => ("Checking for updates\u{2026}".to_owned(), None),
        UpdateState::UpToDate => (
            format!("v{current} is the latest version."),
            Some(
                div()
                    .text_xs()
                    .text_color(cx.theme().success)
                    .child("up to date")
                    .into_any_element(),
            ),
        ),
        UpdateState::Available { version, .. } => (
            format!("v{version} is available."),
            Some(
                Button::new("update-download")
                    .icon(IconName::Download)
                    .label("Download & install")
                    .small()
                    .primary()
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_download_update(cx);
                    }))
                    .into_any_element(),
            ),
        ),
        UpdateState::Downloading { .. } => ("Downloading and verifying\u{2026}".to_owned(), None),
        UpdateState::Ready { version } => (
            format!("v{version} is staged."),
            Some(
                Button::new("update-restart")
                    .icon(IconName::RefreshCw)
                    .label("Restart to install")
                    .small()
                    .primary()
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_install_update(cx);
                    }))
                    .into_any_element(),
            ),
        ),
        UpdateState::Failed(message) => (
            message.clone(),
            Some(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(ellipsis(message, 120))
                    .into_any_element(),
            ),
        ),
    };
    let rows = vec![
        settings_row(
            "version",
            "Current version",
            None,
            div()
                .text_sm()
                .opacity(0.8)
                .child(format!("v{current}"))
                .into_any_element(),
        ),
        settings_row(
            "auto-update",
            "Automatic checks",
            Some("Check GitHub for a newer release once a day."),
            Switch::new("update-auto-toggle")
                .checked(auto_update)
                .on_click(cx.listener(|workspace, checked: &bool, _, cx| {
                    workspace.on_toggle_auto_update(*checked, cx);
                }))
                .into_any_element(),
        ),
        settings_row(
            "update-status",
            "Status",
            None,
            div()
                .text_xs()
                .opacity(0.7)
                .child(status.0)
                .into_any_element(),
        ),
        status
            .1
            .map(|node| {
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .child(node)
                    .into_any_element()
            })
            .unwrap_or_else(|| div().into_any_element()),
        settings_row(
            "catalog",
            "Provider catalog",
            None,
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(div().text_xs().opacity(0.6).child(catalog_line))
                .child(
                    Button::new("catalog-refresh")
                        .icon(IconName::RefreshCw)
                        .label("Refresh")
                        .small()
                        .ghost()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_refresh_catalog(cx);
                        })),
                )
                .into_any_element(),
        ),
    ];
    let theme = cx.theme();
    settings_card(
        "about",
        "About",
        Some("The app updates itself from GitHub releases."),
        theme,
        rows,
    )
    .into_any_element()
}

fn catalog_date(seconds: u64) -> String {
    // Bounded ISO-ish date from unix seconds without a chrono dependency.
    let days = seconds / 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
