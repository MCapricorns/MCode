//! The welcome hero shown while the conversation is empty.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, px, rems,
};

use crate::ui::skin::mono_chip;
use crate::ui::{desk::Desk, element_id, hover_delete_button, lamp, project_label, skin};
use crate::workspace::Workspace;

pub(super) fn render_welcome(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let recents = workspace.vm().recents.clone();
    let settings = workspace.vm().settings.clone();
    let desk = Desk::of(theme);
    let agents_ready = settings.as_ref().is_none_or(|settings| {
        settings.subagents.roles.is_empty()
            || settings.subagents.roles.iter().any(|role| role.enabled)
    });
    let mcp_ready = settings
        .as_ref()
        .is_some_and(|settings| settings.mcp_servers.iter().any(|server| server.enabled));
    let web_ready = settings
        .as_ref()
        .is_some_and(|settings| settings.web_backends.iter().any(|backend| backend.enabled));
    let skills_ready = !workspace.vm().skills.is_empty();
    div()
        .id("welcome")
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .py(px(56.))
        .child(
            div()
                .id("welcome-title")
                .flex()
                .flex_row()
                .items_baseline()
                .gap_1()
                .text_xl()
                .font_weight(gpui_kit::FontWeight::BOLD)
                .child("MYCode Harness"),
        )
        .child(
            div()
                .id("welcome-accent")
                .w(px(72.))
                .h(px(3.))
                .rounded(px(2.))
                .bg(skin::accent(theme, 90.)),
        )
        .child(
            div()
                .id("welcome-tagline")
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("A coding agent for your desktop."),
        )
        .child(
            div()
                .id("welcome-chips")
                .flex()
                .flex_row()
                .gap_1()
                .child(capability_chip("AGENTS", agents_ready, desk.violet, theme))
                .child(capability_chip("SKILLS", skills_ready, desk.amber, theme))
                .child(capability_chip("MCP", mcp_ready, desk.amber, theme))
                .child(capability_chip("WEB", web_ready, desk.cyan, theme))
                .child(capability_chip("FILES", true, desk.green, theme)),
        )
        .child(
            div()
                .id("welcome-hint")
                .text_xs()
                .text_color(desk.faint)
                .max_w(rems(38.))
                .flex()
                .flex_col()
                .items_center()
                .gap_0()
                .child("Open a project folder so the agent can read and edit files,")
                .child("or just start chatting — tools stay available everywhere."),
        )
        .child(
            div()
                .id("welcome-actions")
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .mt_3()
                .child(
                    welcome_action(
                        "welcome-open-project",
                        IconName::FolderOpen,
                        "Open project folder",
                        true,
                        theme,
                    )
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_open_project_dialog(cx);
                    })),
                )
                .child(
                    welcome_action(
                        "welcome-new-chat",
                        IconName::MessageSquare,
                        "Just start chatting",
                        false,
                        theme,
                    )
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_new_session(cx);
                    })),
                ),
        )
        .when(!recents.is_empty(), |this| {
            this.child(
                div()
                    .id("welcome-recents")
                    .mt_4()
                    .w(px(460.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded(skin::radius_card())
                    .border_1()
                    .border_color(skin::glass_border(theme))
                    .bg(skin::frost_card(theme))
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(desk.faint)
                            .px_1()
                            .pb(px(4.))
                            .child("RECENT PROJECTS"),
                    )
                    .children(recents.iter().take(5).map(|project| {
                        let project = project.clone();
                        div()
                            .id(format!("recent-{}", element_id(&project)))
                            .group("recent-row")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py(px(7.))
                            .rounded(skin::radius_control())
                            .cursor_pointer()
                            .hover(|this| this.bg(skin::frost_hover(theme)))
                            .on_click({
                                let project = project.clone();
                                cx.listener(move |workspace, _, _, cx| {
                                    workspace.on_open_recent(&project, cx);
                                })
                            })
                            .child(lamp(desk.faint))
                            .child(
                                Icon::new(IconName::Folder)
                                    .xsmall()
                                    .flex_shrink_0()
                                    .text_color(theme.muted_foreground),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .child(project_label(&project)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .opacity(0.4)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis_start()
                                    .child(project.clone()),
                            )
                            .child(hover_delete_button(
                                format!("recent-remove-{}", element_id(&project)),
                                IconName::X,
                                "recent-row",
                                {
                                    let project = project.clone();
                                    cx.listener(move |workspace, _, _, cx| {
                                        workspace.on_remove_recent(&project, cx);
                                    })
                                },
                                cx,
                            ))
                    })),
            )
        })
        .into_any_element()
}

fn welcome_action(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    emphasized: bool,
    theme: &Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let ink = if emphasized {
        theme.foreground
    } else {
        theme.muted_foreground
    };
    skin::glass_button(id, emphasized, theme)
        .min_w(px(176.))
        .text_color(ink)
        .child(Icon::new(icon).with_size(px(15.)).text_color(ink))
        .child(label)
}

fn capability_chip(
    label: &str,
    ready: bool,
    color: gpui_kit::Hsla,
    theme: &Theme,
) -> impl IntoElement {
    let desk = Desk::of(theme);
    mono_chip(
        label,
        if ready { color } else { desk.faint },
        if ready {
            color.opacity(0.4)
        } else {
            theme.border
        },
        theme,
    )
}
