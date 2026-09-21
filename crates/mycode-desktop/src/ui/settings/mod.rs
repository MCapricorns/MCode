//! The full-page settings view: a secondary left nav (General / Models /
//! Agents / Skills / MCP / Web / Data / About) and a sectioned content pane
//! with switch rows and clean forms.
//!
//! Rendering order matters: entities are created and row lists materialized
//! with `&mut Context` first, and only then is `cx.theme()` borrowed for the
//! layout pass.
mod about;
mod agents;
mod data;
mod general;
mod mcp;
mod models;
mod skills;
mod web;
mod widgets;

pub(crate) use mcp::{McpForm, build_mcp_server};
pub(crate) use models::ProviderForm;
pub(crate) use web::BackendForm;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px, rems,
};

use crate::view_model::{MainView, SettingsSection, UpdateState};
use crate::workspace::Workspace;

pub(super) fn render_settings_view(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let section = workspace.vm().settings_section;
    let settings_ready = workspace.vm().settings.is_some();
    let header_meta = workspace
        .vm()
        .settings
        .clone()
        .map(|s| (s.dirty, s.saving, s.revision));
    let nav = render_settings_nav(workspace, section, cx).into_any_element();
    let theme = cx.theme();
    let desk = crate::ui::desk::Desk::of(theme);
    div()
        .id("settings-view")
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(theme.background)
        .child(
            div()
                .id("settings-header")
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px_6()
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .child(
                            Button::new("settings-back")
                                .icon(IconName::ArrowLeft)
                                .label("Back")
                                .small()
                                .ghost()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_show_main_view(MainView::Chat, cx);
                                })),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(gpui_kit::FontWeight::BOLD)
                                        .child("Settings"),
                                )
                                .child(div().text_sm().text_color(desk.amber).child("//"))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child(section.label()),
                                ),
                        ),
                )
                .child(div().flex().flex_row().items_center().gap_2().when_some(
                    header_meta,
                    |this, (dirty, saving, revision)| {
                        this.child(
                            div()
                                .text_xs()
                                .opacity(0.45)
                                .child(format!("revision {revision}")),
                        )
                        .child(
                            Button::new("settings-save")
                                .icon(IconName::Check)
                                .label("Save changes")
                                .small()
                                .primary()
                                .disabled(!dirty || saving)
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_save_settings(cx);
                                })),
                        )
                    },
                )),
        )
        .child(
            div()
                .id("settings-body")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_row()
                .child(nav)
                .child(
                    div()
                        .id("settings-content")
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .overflow_y_scroll()
                        .child(
                            div()
                                .id("settings-content-inner")
                                .mx_auto()
                                .max_w(rems(52.))
                                .w_full()
                                .flex()
                                .flex_col()
                                .gap_4()
                                .px_6()
                                .py_4()
                                .when(!settings_ready, |this| {
                                    this.child(
                                        div()
                                            .text_sm()
                                            .opacity(0.6)
                                            .child("Loading settings\u{2026}"),
                                    )
                                })
                                .when(settings_ready, |this| {
                                    this.child(match section {
                                        SettingsSection::General => {
                                            general::render_general_section(workspace, window, cx)
                                        }
                                        SettingsSection::Models => {
                                            models::render_models_section(workspace, window, cx)
                                        }
                                        SettingsSection::Agents => {
                                            agents::render_agents_section(workspace, cx)
                                        }
                                        SettingsSection::Skills => {
                                            skills::render_skills_section(workspace, cx)
                                        }
                                        SettingsSection::Mcp => {
                                            mcp::render_mcp_section(workspace, window, cx)
                                        }
                                        SettingsSection::Web => {
                                            web::render_web_section(workspace, window, cx)
                                        }
                                        SettingsSection::Data => {
                                            data::render_data_section(workspace, cx)
                                        }
                                        SettingsSection::About => {
                                            about::render_about_section(workspace, cx)
                                        }
                                    })
                                }),
                        ),
                ),
        )
        .into_any_element()
}

/// What the nav shows beside each section: a count or a status lamp.
#[derive(Clone, Copy, Default)]
struct NavBadge {
    /// Right-aligned mono figure (configured providers, enabled servers).
    count: Option<usize>,
    /// Attention lamp (an update is waiting, a provider has no key).
    lamp: Option<gpui_kit::Hsla>,
}

/// Per-section badges derived from live state.
fn nav_badges(workspace: &Workspace, cx: &Context<Workspace>) -> Vec<(SettingsSection, NavBadge)> {
    let theme = cx.theme();
    let desk = crate::ui::desk::Desk::of(theme);
    let vm = workspace.vm();
    let settings = vm.settings.as_ref();
    let providers = settings.map(|s| s.providers.iter().filter(|p| p.enabled).count());
    let servers = settings.map(|s| s.mcp_servers.iter().filter(|m| m.enabled).count());
    let backends = settings.map(|s| s.web_backends.iter().filter(|b| b.enabled).count());
    let missing_key = settings.is_some_and(|s| {
        s.providers
            .iter()
            .any(|p| p.enabled && !s.providers_with_keys.contains(&p.id))
    });
    let update_lamp = match vm.update {
        UpdateState::Available { .. } | UpdateState::Ready { .. } => Some(desk.amber),
        UpdateState::Failed(_) => Some(desk.red),
        _ => None,
    };
    vec![
        (SettingsSection::General, NavBadge::default()),
        (
            SettingsSection::Models,
            NavBadge {
                count: providers,
                lamp: missing_key.then_some(desk.red),
            },
        ),
        (
            SettingsSection::Agents,
            NavBadge {
                count: settings.map(|s| {
                    mycode_config::builtin_roles()
                        .roles
                        .iter()
                        .filter(|role| s.subagents.is_enabled(&role.name))
                        .count()
                }),
                lamp: None,
            },
        ),
        (
            SettingsSection::Skills,
            NavBadge {
                count: Some(vm.skills.len()),
                lamp: None,
            },
        ),
        (
            SettingsSection::Mcp,
            NavBadge {
                count: servers,
                lamp: None,
            },
        ),
        (
            SettingsSection::Web,
            NavBadge {
                count: backends,
                lamp: None,
            },
        ),
        (SettingsSection::Data, NavBadge::default()),
        (
            SettingsSection::About,
            NavBadge {
                count: None,
                lamp: update_lamp,
            },
        ),
    ]
}

/// The settings secondary menu in the Desk look: a mono "SETTINGS" pane
/// head, grouped rows (WORKSPACE / CONNECT / SYSTEM) with an index, icon,
/// label and hint, live count badges, and an amber left rail on the
/// selected row.
fn render_settings_nav(
    workspace: &Workspace,
    section: SettingsSection,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let badges = nav_badges(workspace, cx);
    let dirty = workspace.vm().settings.as_ref().is_some_and(|s| s.dirty);
    let theme = cx.theme();
    let desk = crate::ui::desk::Desk::of(theme);
    let mut index = 0usize;
    let mut groups: Vec<AnyElement> = Vec::new();
    for (group, members) in SettingsSection::GROUPS {
        let mut rows: Vec<AnyElement> = Vec::new();
        for candidate in members.iter().copied() {
            index += 1;
            let badge = badges
                .iter()
                .find(|(s, _)| *s == candidate)
                .map(|(_, badge)| *badge)
                .unwrap_or_default();
            rows.push(nav_row(
                candidate,
                candidate == section,
                index,
                badge,
                theme,
                &desk,
                cx,
            ));
        }
        groups.push(
            div()
                .id(format!("settings-nav-group-{group}"))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .px_2()
                        .pt(px(8.))
                        .pb(px(2.))
                        .text_xs()
                        .text_color(desk.faint)
                        .child(*group),
                )
                .children(rows)
                .into_any_element(),
        );
    }
    div()
        .id("settings-nav")
        .w(px(212.))
        .h_full()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .border_r_1()
        .border_color(theme.border)
        .bg(theme.sidebar)
        .child(crate::ui::sidebar::pane_head(
            "SETTINGS",
            Some(&format!("{index:02}")),
            desk.faint,
            theme,
        ))
        .child(
            div()
                .id("settings-nav-groups")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .px_2()
                .pb_2()
                .children(groups),
        )
        .child(
            div()
                .id("settings-nav-footer")
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(theme.border)
                .text_xs()
                .text_color(desk.faint)
                .child(format!("v{}", env!("CARGO_PKG_VERSION")))
                .when(dirty, |this| {
                    this.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .text_color(desk.amber)
                            .child(crate::ui::lamp(desk.amber))
                            .child("UNSAVED"),
                    )
                }),
        )
}

/// One nav row: index, icon, label + hint, and the badge column.
#[allow(clippy::too_many_arguments)]
fn nav_row(
    candidate: SettingsSection,
    selected: bool,
    index: usize,
    badge: NavBadge,
    theme: &Theme,
    desk: &crate::ui::desk::Desk,
    cx: &Context<Workspace>,
) -> AnyElement {
    let ink = if selected {
        theme.sidebar_accent_foreground
    } else {
        theme.sidebar_foreground
    };
    div()
        .id(format!("settings-nav-{}", candidate.id()))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .pl(px(6.))
        .pr_2()
        .py(px(4.))
        .rounded(px(3.))
        .border_l_2()
        .border_color(theme.transparent)
        .cursor_pointer()
        .when(selected, |this| {
            this.bg(theme.sidebar_accent).border_color(desk.amber)
        })
        .hover(|this| this.bg(theme.sidebar_accent))
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.on_show_settings_section(candidate, cx);
        }))
        .child(
            div()
                .w(px(16.))
                .text_xs()
                .text_color(if selected { desk.amber } else { desk.faint })
                .child(format!("{index:02}")),
        )
        .child(
            Icon::new(candidate.icon())
                .with_size(px(14.))
                .text_color(if selected {
                    desk.amber
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
                        .text_color(ink)
                        .when(selected, |this| {
                            this.font_weight(gpui_kit::FontWeight::MEDIUM)
                        })
                        .child(candidate.label()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(desk.faint)
                        .overflow_hidden()
                        .child(candidate.hint()),
                ),
        )
        .when_some(badge.lamp, |this, color| this.child(crate::ui::lamp(color)))
        .when_some(badge.count, |this, count| {
            this.child(
                div()
                    .min_w(px(18.))
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(3.))
                    .border_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(if count > 0 { ink } else { desk.faint })
                    .child(count.to_string()),
            )
        })
        .into_any_element()
}

/// A secondary-page header: back arrow, title, and optional hint.
pub(super) fn subview_header(
    title: &str,
    hint: Option<&str>,
    on_back: impl Fn(&mut Workspace, &mut Context<Workspace>) + 'static,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    div()
        .id("subview-header")
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .pb_1()
        .child(
            Button::new("subview-back")
                .icon(IconName::ArrowLeft)
                .label("Back")
                .small()
                .ghost()
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    on_back(workspace, cx);
                })),
        )
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::BOLD)
                .child(title.to_owned()),
        )
        .when_some(hint, |this, hint| {
            this.child(div().text_xs().opacity(0.5).child(hint.to_owned()))
        })
        .into_any_element()
}
