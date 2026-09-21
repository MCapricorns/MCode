//! The project-centric sidebar: project switcher, new-chat button, and
//! sessions grouped by project (the opencode desktop shape).
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::Button;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use super::{project_label, short_id, skin};
use crate::view_model::{MainView, SessionSummary};
use crate::workspace::Workspace;

/// Sidebar width.
const WIDTH: gpui_kit::Pixels = px(248.);

pub(super) fn render_sidebar(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    let active_project = workspace.vm().project_dir.clone();
    let sessions = workspace.vm().sessions.clone();
    let session_count = sessions.len();
    let bindings = workspace.vm().session_projects.clone();
    let view = workspace.vm().view;

    // Group sessions by their bound project.
    let mut current: Vec<SessionSummary> = Vec::new();
    let mut others: Vec<(String, Vec<SessionSummary>)> = Vec::new();
    let mut unbound: Vec<SessionSummary> = Vec::new();
    for session in sessions {
        let project = bindings
            .iter()
            .find(|(id, _)| *id == session.session_id)
            .map(|(_, project)| project.clone());
        match project.as_deref() {
            Some(project) if active_project.as_deref() == Some(project) => current.push(session),
            Some(project) => match others.iter_mut().find(|(key, _)| key == project) {
                Some((_, rows)) => rows.push(session),
                None => others.push((project.to_owned(), vec![session])),
            },
            None => unbound.push(session),
        }
    }

    let project_name: SharedString = active_project
        .as_deref()
        .map(project_label)
        .unwrap_or_else(|| "Choose a project".to_owned())
        .into();
    let has_project = active_project.is_some();
    let current_empty = current.is_empty();

    div()
        .id("sidebar")
        .w(WIDTH)
        .h_full()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .bg(skin::glass_sidebar(theme))
        .border_r_1()
        .border_color(skin::glass_border(theme))
        .child(pane_head(
            "PROJECTS",
            Some(&session_count.to_string()),
            desk.faint,
            theme,
        ))
        .child(
            div()
                .id("sidebar-header")
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .child(
                    div()
                        .id("project-pill")
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py(px(7.))
                        .rounded(px(3.))
                        .border_1()
                        .border_color(theme.border)
                        .cursor_pointer()
                        .hover(|this| this.bg(theme.secondary))
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            let open = !workspace.vm().project_menu_open;
                            workspace.on_toggle_project_menu(open, cx);
                        }))
                        .child(
                            Icon::new(if has_project {
                                IconName::FolderOpen
                            } else {
                                IconName::Folder
                            })
                            .with_size(px(15.))
                            .flex_shrink_0()
                            .text_color(theme.muted_foreground),
                        )
                        .child(
                            div()
                                .id("project-pill-label")
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .truncate()
                                        .child(project_name),
                                )
                                .when(has_project, |this| {
                                    let full = active_project.clone().unwrap_or_default();
                                    // A path is unbounded: let the layout clip
                                    // it from the left so the leaf stays
                                    // readable in a 248px rail.
                                    this.child(
                                        div()
                                            .text_xs()
                                            .opacity(0.45)
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis_start()
                                            .child(full),
                                    )
                                }),
                        )
                        .child(
                            Icon::new(IconName::ChevronDown)
                                .with_size(px(14.))
                                .flex_shrink_0()
                                .text_color(theme.muted_foreground),
                        ),
                )
                .child(
                    Button::new("new-chat")
                        .icon(IconName::Plus)
                        .label("New chat")
                        .small()
                        .outline()
                        .w_full()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_new_session(cx);
                        })),
                ),
        )
        .child(
            div()
                .id("session-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .px_2()
                .pb_2()
                .gap(px(2.))
                .child(group_label(
                    "group-current",
                    if has_project { "THIS PROJECT" } else { "CHATS" },
                    cx.theme(),
                ))
                .when(has_project && current_empty, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .opacity(0.5)
                            .px_2()
                            .py_1()
                            .child("No chats in this project yet"),
                    )
                })
                .children(current.iter().map(|summary| session_row(summary, cx)))
                .when(!unbound.is_empty(), |this| {
                    this.child(group_label("group-unbound", "NO PROJECT", cx.theme()))
                        .children(unbound.iter().map(|summary| session_row(summary, cx)))
                })
                .children(others.iter().map(|(project, rows)| {
                    div()
                        .id(format!("other-group-{}", short_id(project)))
                        .child(switch_group_header(project, cx))
                        .children(rows.iter().map(|summary| session_row(summary, cx)))
                })),
        )
        .child(render_sidebar_footer(workspace, view, cx))
}

/// One clickable session row: the demo's `.sess` — status lamp, title, meta
/// count; the open session paints an amber left rail (`.sess.active`).
fn session_row(summary: &SessionSummary, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    let is_open = summary.active;
    let title: SharedString = if summary.title.is_empty() {
        short_id(&summary.session_id).into()
    } else {
        summary.title.clone().into()
    };
    let session_id = summary.session_id.clone();
    div()
        .id(format!("session-row-{}", summary.session_id))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .group("session-row")
        .px_2()
        .py(px(6.))
        .rounded(px(3.))
        .border_l_1()
        .border_color(theme.transparent)
        .when(is_open, |this| {
            this.bg(theme.sidebar_accent).border_color(desk.amber)
        })
        .cursor_pointer()
        .hover(|this| this.bg(theme.sidebar_accent))
        .text_color(if is_open {
            theme.sidebar_accent_foreground
        } else {
            theme.sidebar_foreground
        })
        .on_click({
            let session_id = session_id.clone();
            cx.listener(move |workspace, _, _, cx| {
                workspace.on_open_session(&session_id, cx);
            })
        })
        .child(super::lamp(if is_open { desk.green } else { desk.faint }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_sm().truncate().child(title)),
        )
        .when(summary.event_count > 0, |this| {
            this.child(
                div()
                    .text_xs()
                    .flex_shrink_0()
                    .text_color(desk.faint)
                    .child(summary.event_count.to_string()),
            )
        })
        .child(
            div()
                .id(format!("session-delete-{}", summary.session_id))
                .flex()
                .items_center()
                .justify_center()
                .size(px(20.))
                .rounded(px(2.))
                .flex_shrink_0()
                .opacity(0.0)
                .group_hover("session-row", |this| this.opacity(1.0))
                .cursor_pointer()
                .text_color(theme.muted_foreground)
                .hover(|this| this.bg(theme.sidebar_accent))
                .on_click({
                    let session_id = session_id.clone();
                    cx.listener(move |workspace, _, _, cx| {
                        workspace.on_delete_session(&session_id, cx);
                    })
                })
                .child(Icon::new(IconName::Trash).xsmall()),
        )
}

/// Collapsed header for another project's session group; clicking switches
/// the active project.
fn switch_group_header(project: &str, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let project = project.to_owned();
    div()
        .id(format!("group-header-{}", short_id(&project)))
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_2()
        .py(px(5.))
        .rounded(px(7.))
        .cursor_pointer()
        .hover(|this| this.bg(theme.secondary))
        .on_click({
            let project = project.clone();
            cx.listener(move |workspace, _, _, cx| {
                workspace.on_switch_project(Some(project.clone()), cx);
            })
        })
        .child(
            Icon::new(IconName::ChevronRight)
                .xsmall()
                .flex_shrink_0()
                .text_color(theme.muted_foreground),
        )
        .child(
            div()
                .min_w_0()
                .text_xs()
                .opacity(0.7)
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .truncate()
                .child(project_label(&project)),
        )
}

fn group_label(id: &'static str, label: &'static str, theme: &Theme) -> impl IntoElement {
    let desk = super::desk::Desk::of(theme);
    pane_head(label, None, desk.faint, theme).id(id)
}

/// The demo's `.pane-head`: a letterspaced mono caption with an optional
/// count, sitting on a soft bottom hairline.
pub(super) fn pane_head(
    label: &str,
    count: Option<&str>,
    color: gpui_kit::Hsla,
    theme: &Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(format!("pane-head-{label}"))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px_3()
        .pt(px(10.))
        .pb(px(8.))
        .border_b_1()
        .border_color(theme.border)
        .text_xs()
        .text_color(color)
        .child(div().min_w_0().truncate().child(label.to_owned()))
        .when_some(count.map(str::to_owned), |this, count| {
            this.child(div().flex_shrink_0().child(count))
        })
}

fn render_sidebar_footer(
    _workspace: &mut Workspace,
    view: MainView,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id("sidebar-footer")
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(theme.sidebar_border)
        .child(
            div()
                .id("footer-settings")
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .text_color(if view == MainView::Settings {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .hover(|this| this.text_color(theme.foreground))
                .on_click(cx.listener(|workspace, _, _, cx| {
                    let next = if workspace.vm().view == MainView::Settings {
                        MainView::Chat
                    } else {
                        MainView::Settings
                    };
                    workspace.on_show_main_view(next, cx);
                }))
                .child(Icon::new(IconName::Settings).small())
                .child(div().text_xs().child(if view == MainView::Settings {
                    "Back to chat"
                } else {
                    "Settings"
                })),
        )
        // Day/night lives in the title bar's DAY|NIGHT toggle now; the footer
        // keeps only the version stamp (the demo's `.rail-foot`).
        .child(
            div().flex().flex_row().items_center().gap_2().child(
                div()
                    .text_xs()
                    .opacity(0.4)
                    .child(format!("v{}", mycode_updates::current_version())),
            ),
        )
}

/// Full-window backdrop plus the project dropdown pinned under the pill.
pub(super) fn render_project_menu_layer(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let recents = workspace.vm().recents.clone();
    let active = workspace.vm().project_dir.clone();
    let has_active = active.is_some();
    div()
        .id("project-menu-layer")
        .absolute()
        .inset_0()
        .child(
            div()
                .id("project-menu-backdrop")
                .absolute()
                .size_full()
                .bg(skin::scrim(theme))
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_toggle_project_menu(false, cx);
                })),
        )
        .child(
            div()
                .id("project-menu")
                .absolute()
                .top(px(40.))
                .left(px(8.))
                .w(px(244.))
                .max_h(px(430.))
                .overflow_y_scroll()
                .rounded(px(3.))
                .border_1()
                .border_color(skin::glass_border(theme))
                .bg(skin::popover(theme))
                .text_color(theme.popover_foreground)
                .shadow_lg()
                .p_1()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .opacity(0.55)
                        .px_2()
                        .pt_1()
                        .pb(px(2.))
                        .child("RECENT PROJECTS"),
                )
                .when(recents.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .opacity(0.5)
                            .px_2()
                            .py_1()
                            .child("Nothing yet — pick a folder below"),
                    )
                })
                .children(recents.iter().take(10).map(|project| {
                    let selected = active.as_deref() == Some(project.as_str());
                    project_menu_row(project_label(project), project, selected, cx)
                }))
                .child(div().h(px(1.)).mx_2().my_1().bg(theme.border))
                .child(menu_action_row(
                    "project-menu-open",
                    IconName::FolderPlus,
                    "Choose a folder\u{2026}",
                    cx.listener(|workspace, _, _, cx| {
                        workspace.on_open_project_dialog(cx);
                    }),
                    cx,
                ))
                .when(has_active, |this| {
                    this.child(menu_action_row(
                        "project-menu-clear",
                        IconName::List,
                        "All chats (no filter)",
                        cx.listener(|workspace, _, _, cx| {
                            workspace.on_switch_project(None, cx);
                        }),
                        cx,
                    ))
                }),
        )
        .into_any_element()
}

fn project_menu_row(
    label: String,
    path: &str,
    selected: bool,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let path = path.to_owned();
    div()
        .id(format!("project-recent-{}", short_id(path.as_str())))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .py(px(6.))
        .rounded_md()
        .cursor_pointer()
        .when(selected, |this| this.bg(theme.secondary))
        .hover(|this| this.bg(theme.secondary))
        .on_click({
            let path = path.clone();
            cx.listener(move |workspace, _, _, cx| {
                workspace.on_switch_project(Some(path.clone()), cx);
            })
        })
        .child(
            Icon::new(IconName::Folder)
                .small()
                .flex_shrink_0()
                .text_color(theme.muted_foreground),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_sm().truncate().child(label))
                .child(
                    div()
                        .text_xs()
                        .opacity(0.45)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis_start()
                        .child(path),
                ),
        )
        .when(selected, |this| {
            this.child(
                Icon::new(IconName::Check)
                    .xsmall()
                    .flex_shrink_0()
                    .text_color(theme.primary),
            )
        })
}

fn menu_action_row(
    id: &'static str,
    icon: IconName,
    label: &str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .py(px(6.))
        .rounded_md()
        .cursor_pointer()
        .hover(|this| this.bg(theme.secondary))
        .on_click(on_click)
        .child(Icon::new(icon).small().text_color(theme.muted_foreground))
        .child(div().text_sm().child(label.to_owned()))
}
