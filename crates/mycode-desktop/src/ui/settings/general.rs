//! The General settings page: theme, request identity, and the platform
//! shell preference.
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled, Window, div, px,
};

use super::widgets::{dropdown_field, settings_card, settings_row};
use crate::workspace::Workspace;

pub(super) fn render_general_section(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let ua_input = workspace.settings_ua_input(window, cx);
    let dark = workspace.vm().dark_theme;
    let effective_ua = workspace
        .vm()
        .settings
        .as_ref()
        .map(|settings| settings.effective_user_agent.clone())
        .unwrap_or_default();
    let theme_row = settings_row(
        "theme",
        "Color theme",
        None,
        div()
            .flex()
            .flex_row()
            .gap_1()
            .child(
                Button::new("theme-light")
                    .icon(IconName::Sun)
                    .label("Light")
                    .small()
                    .when(!dark, |this| this.primary())
                    .when(dark, |this| this.ghost())
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.on_select_theme(false, window, cx);
                    }))
                    .into_any_element(),
            )
            .child(
                Button::new("theme-dark")
                    .icon(IconName::Moon)
                    .label("Dark")
                    .small()
                    .when(dark, |this| this.primary())
                    .when(!dark, |this| this.ghost())
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.on_select_theme(true, window, cx);
                    }))
                    .into_any_element(),
            )
            .into_any_element(),
    );
    let ua_field = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_sm().child("HTTP User-Agent"))
        .child(div().h(px(30.)).text_sm().child(Input::new(&ua_input)))
        .child(
            div()
                .text_xs()
                .opacity(0.5)
                .child(format!("Effective: {effective_ua}")),
        )
        .into_any_element();
    let shell = workspace
        .vm()
        .settings
        .as_ref()
        .and_then(|settings| settings.tools.shell.clone());
    let shell_kind = shell
        .as_ref()
        .map(|item| item.kind.clone())
        .unwrap_or_else(|| "auto".to_owned());
    let shell_program = shell
        .as_ref()
        .map(|item| item.program.clone())
        .unwrap_or_default();
    let shell_source = shell
        .as_ref()
        .map(|item| {
            if item.source.is_empty() {
                "auto".to_owned()
            } else {
                item.source.clone()
            }
        })
        .unwrap_or_else(|| "auto".to_owned());
    let shell_status = if shell_program.is_empty() {
        "No shell found. Detect one or browse to pwsh, powershell, cmd, or bash.".to_owned()
    } else {
        format!("{shell_kind} · {shell_program} ({shell_source})")
    };
    let shell_kind_open = workspace.vm().shell_kind_menu_open;
    let shell_options = ["pwsh", "powershell", "cmd", "bash"]
        .iter()
        .map(|kind| (*kind).to_owned())
        .collect::<Vec<_>>();
    let theme = cx.theme();
    div()
        .id("general-section")
        .flex()
        .flex_col()
        .gap_3()
        .child(settings_card(
            "appearance",
            "Appearance",
            Some("Theme applies immediately and is saved to settings right away."),
            theme,
            vec![theme_row, ua_field],
        ))
        .child(settings_card(
            "shell",
            "Shell",
            Some(
                "First launch detects pwsh, then Windows PowerShell, then cmd or Git bash. \
                 Override it here if detection misses your install.",
            ),
            theme,
            vec![
                settings_row(
                    "shell-current",
                    "Current program",
                    Some(shell_status.as_str()),
                    div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .child(
                            Button::new("shell-detect")
                                .label("Detect")
                                .small()
                                .outline()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_detect_shell(cx);
                                })),
                        )
                        .child(
                            Button::new("shell-browse")
                                .label("Browse")
                                .small()
                                .outline()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_browse_shell(cx);
                                })),
                        )
                        .into_any_element(),
                ),
                dropdown_field(
                    "shell-kind",
                    "Kind",
                    Some("Used to build the launch line"),
                    &shell_kind,
                    &shell_options,
                    shell_kind_open,
                    |workspace, open, cx| workspace.on_toggle_shell_kind_menu(open, cx),
                    |workspace, kind, cx| {
                        workspace.on_set_shell_kind(kind, cx);
                        workspace.on_toggle_shell_kind_menu(false, cx);
                    },
                    cx,
                ),
            ],
        ))
        .into_any_element()
}
