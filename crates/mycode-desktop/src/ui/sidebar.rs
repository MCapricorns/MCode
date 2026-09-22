//! Workspace sidebar: folder roots on top, sessions underneath.
//!
//! A workspace can hold several directories at once (a web app and its API,
//! for example). The open chat keeps one working directory. The other roots
//! stay available as absolute paths. Sessions are a flat list, not a second
//! copy of every folder the user has ever opened.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::Button;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};

use super::{element_id, project_label, skin};
use crate::view_model::{MainView, SessionSummary};
use crate::workspace::Workspace;

/// Sidebar width.
const WIDTH: gpui_kit::Pixels = px(260.);

pub(super) fn render_sidebar(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let roots = workspace.vm().workspace_roots.clone();
    let cwd = workspace.vm().project_dir.clone();
    let sessions = workspace.vm().sessions.clone();
    let bindings = workspace.vm().session_projects.clone();
    let view = workspace.vm().view;
    let session_count = sessions.len();
    let roots_view = workspace_roots(cx, &roots, &sessions, &bindings, cwd.as_deref());
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);

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
            "Workspace",
            Some(&session_count.to_string()),
            desk.faint,
            theme,
        ))
        .child(
            div().px_2().pt_1().child(
                Button::new("new-chat")
                    .icon(IconName::Plus)
                    .label("New session")
                    .small()
                    .outline()
                    .w_full()
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_new_session(cx);
                    })),
            ),
        )
        .child(roots_view)
        .child(render_sidebar_footer(workspace, view, cx))
}

fn workspace_roots(
    cx: &mut Context<Workspace>,
    roots: &[String],
    sessions: &[SessionSummary],
    bindings: &[(String, String)],
    cwd: Option<&str>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let groups: Vec<(String, Vec<SessionSummary>)> = roots
        .iter()
        .map(|root| {
            let rows = sessions
                .iter()
                .filter(|session| {
                    bindings.iter().any(|(id, path)| {
                        id == &session.session_id
                            && crate::view_model::same_project_path(path, root)
                    })
                })
                .cloned()
                .collect();
            (root.clone(), rows)
        })
        .collect();
    let other: Vec<SessionSummary> = sessions
        .iter()
        .filter(|session| {
            !bindings.iter().any(|(id, path)| {
                id == &session.session_id
                    && roots
                        .iter()
                        .any(|root| crate::view_model::same_project_path(path, root))
            })
        })
        .cloned()
        .collect();
    div()
        .id("workspace-roots")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(2.))
        .px_2()
        .py_2()
        .when(roots.is_empty(), |this| {
            this.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Add the folders this workspace should see. Each chat keeps its own folder."),
            )
        })
        .children(groups.into_iter().enumerate().map(|(index, (root, rows))| {
            let root_for_rows = root.clone();
            div()
                .id(format!("workspace-group-{index}"))
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(root_row(index, &root, cwd, cx))
                .children(rows.into_iter().map(|summary| {
                    div().pl_3().child(session_row(
                        &summary,
                        Some(root_for_rows.as_str()),
                        Some(root_for_rows.as_str()),
                        cx,
                    ))
                }))
        }))
        .when(!other.is_empty(), |this| {
            this.child(
                div()
                    .px_2()
                    .pt_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Other"),
            )
            .children(other.iter().map(|summary| {
                let project = bindings
                    .iter()
                    .find_map(|(id, path)| (id == &summary.session_id).then_some(path.as_str()));
                session_row(summary, project, cwd, cx)
            }))
        })
        .child(
            div()
                .id("workspace-add")
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .h(px(28.))
                .rounded(skin::radius_control())
                .text_xs()
                .text_color(theme.muted_foreground)
                .cursor_pointer()
                .hover(|this| {
                    this.bg(skin::frost_hover(theme))
                        .text_color(theme.foreground)
                })
                .on_click(cx.listener(|workspace, _, _, cx| {
                    let open = !workspace.vm().project_menu_open;
                    workspace.on_toggle_project_menu(open, cx);
                }))
                .child(Icon::new(IconName::Plus).xsmall().flex_shrink_0())
                .child("Add folder"),
        )
}

fn root_row(
    index: usize,
    root: &str,
    cwd: Option<&str>,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let is_cwd = cwd.is_some_and(|current| crate::view_model::same_project_path(current, root));
    let path = root.to_owned();
    let label: SharedString = project_label(root).into();
    div()
        .id(format!("workspace-root-{}", element_id(root)))
        .group("workspace-root")
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .h(px(32.))
        .rounded(skin::radius_control())
        .when(is_cwd, |this| this.bg(skin::frost_accent(theme)))
        .cursor_pointer()
        .hover(|this| this.bg(skin::frost_hover(theme)))
        .on_click({
            let path = path.clone();
            cx.listener(move |workspace, _, _, cx| {
                workspace.on_open_recent(&path, cx);
            })
        })
        .child(
            Icon::new(IconName::Folder)
                .xsmall()
                .flex_shrink_0()
                .text_color(if is_cwd {
                    theme.primary
                } else {
                    theme.muted_foreground
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_sm()
                        .truncate()
                        .text_color(theme.foreground)
                        .child(label),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis_start()
                        .child(path.clone()),
                ),
        )
        .when(!is_cwd, |this| {
            let remove = path.clone();
            this.child(
                div()
                    .id(format!("workspace-root-remove-{index}"))
                    .flex_shrink_0()
                    .px_1()
                    .cursor_pointer()
                    .text_color(theme.muted_foreground)
                    .hover(|row| row.text_color(theme.danger))
                    .on_click(cx.listener(move |workspace, _, _, cx| {
                        cx.stop_propagation();
                        workspace.on_remove_workspace_root(&remove, cx);
                    }))
                    .child(Icon::new(IconName::X).xsmall()),
            )
        })
}

fn session_row(
    summary: &SessionSummary,
    project: Option<&str>,
    cwd: Option<&str>,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    let is_open = summary.active;
    let title: SharedString = if !summary.title.is_empty() {
        summary.title.clone().into()
    } else if let Some(project) = project {
        project_label(project).into()
    } else {
        "New session".into()
    };
    let elsewhere = project.is_some_and(|path| {
        cwd.is_none_or(|current| !crate::view_model::same_project_path(current, path))
    });
    let folder: Option<SharedString> = elsewhere
        .then(|| project.map(project_label))
        .flatten()
        .map(Into::into);
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
        .rounded(skin::radius_control())
        .when(is_open, |this| this.bg(skin::frost_accent(theme)))
        .cursor_pointer()
        .hover(|this| this.bg(skin::frost_hover(theme)))
        .text_color(if is_open {
            theme.foreground
        } else {
            theme.sidebar_foreground
        })
        .on_click({
            let session_id = session_id.clone();
            cx.listener(move |workspace, _, _, cx| {
                workspace.on_open_session(&session_id, cx);
            })
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_sm().truncate().child(title))
                .when_some(folder, |this, folder| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(folder),
                    )
                }),
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
        .child(super::hover_delete_button(
            format!("session-delete-{}", summary.session_id),
            IconName::Trash,
            "session-row",
            {
                let session_id = session_id.clone();
                cx.listener(move |workspace, _, _, cx| {
                    workspace.on_delete_session(&session_id, cx);
                })
            },
            cx,
        ))
}

/// Section caption used by the sidebar and the model inspector.
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
        .child(
            div().flex().flex_row().items_center().gap_2().child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("v{}", mycode_app::current_version())),
            ),
        )
}

/// Recent folders plus browse, anchored under the workspace add control.
pub(super) fn render_project_menu_layer(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let recents = workspace.vm().recents.clone();
    let roots = workspace.vm().workspace_roots.clone();
    div()
        .id("project-menu-layer")
        .absolute()
        .inset_0()
        .child(
            div()
                .id("project-menu-backdrop")
                .absolute()
                .size_full()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_toggle_project_menu(false, cx);
                })),
        )
        .child(
            skin::popover_panel("project-menu", theme)
                .absolute()
                .top(px(78.))
                .left(px(8.))
                .w(px(244.))
                .max_h(px(360.))
                .overflow_y_scroll()
                .p_1()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(menu_row(
                    "project-menu-browse",
                    "Browse…",
                    false,
                    cx.listener(|workspace, _, _, cx| {
                        workspace.on_toggle_project_menu(false, cx);
                        workspace.on_open_project_dialog(cx);
                    }),
                    theme,
                ))
                .when(!recents.is_empty(), |this| {
                    this.child(
                        div()
                            .px_2()
                            .pt_2()
                            .pb(px(2.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Recent"),
                    )
                })
                .children(recents.iter().enumerate().map(|(index, project)| {
                    let path = project.clone();
                    let already = roots
                        .iter()
                        .any(|root| crate::view_model::same_project_path(root, project));
                    div()
                        .id(format!("project-menu-recent-{index}"))
                        .h(px(30.))
                        .px_2()
                        .flex()
                        .flex_row()
                        .items_center()
                        .rounded(skin::radius_control())
                        .text_sm()
                        .when(!already, |row| {
                            row.cursor_pointer()
                                .text_color(theme.foreground)
                                .hover(|row| row.bg(skin::frost_hover(theme)))
                                .on_click(cx.listener(move |workspace, _, _, cx| {
                                    workspace.on_toggle_project_menu(false, cx);
                                    workspace.on_add_workspace_root(&path, cx);
                                }))
                        })
                        .when(already, |row| row.text_color(theme.muted_foreground))
                        .child(div().min_w_0().truncate().child(project_label(project)))
                })),
        )
        .into_any_element()
}

fn menu_row(
    id: impl Into<gpui_kit::ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    on_click: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(30.))
        .px_2()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .rounded(skin::radius_control())
        .text_sm()
        .cursor_pointer()
        .text_color(theme.foreground)
        .when(selected, |this| this.bg(skin::frost_accent(theme)))
        .hover(|this| this.bg(skin::frost_hover(theme)))
        .on_click(on_click)
        .child(div().min_w_0().truncate().child(label.into()))
}
