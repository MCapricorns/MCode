//! Right context panel: Overview / Web / Changes.
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px,
};

use super::{ellipsis, short_id};
use crate::view_model::{ContextTab, EntryKind};
use crate::workspace::Workspace;

pub(super) fn render_context_panel(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let tab = workspace.vm().context_tab;
    div()
        .id("context-panel")
        .w(px(312.))
        .h_full()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .border_l_1()
        .border_color(theme.border)
        .bg(theme.sidebar)
        .child(
            div()
                .id("context-tabs")
                .flex()
                .flex_row()
                .gap_1()
                .p_2()
                .child(context_tab_button(
                    tab,
                    ContextTab::Overview,
                    "Overview",
                    cx,
                ))
                .child(context_tab_button(tab, ContextTab::Web, "Web", cx))
                .child(context_tab_button(tab, ContextTab::Changes, "Changes", cx)),
        )
        .child(
            div()
                .id("context-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_3()
                .pb_3()
                .child(match tab {
                    ContextTab::Overview => render_overview(workspace, cx).into_any_element(),
                    ContextTab::Web => render_web(workspace, window, cx).into_any_element(),
                    ContextTab::Changes => render_changes(workspace, cx).into_any_element(),
                }),
        )
}

fn context_tab_button(
    current: ContextTab,
    tab: ContextTab,
    label: &'static str,
    cx: &mut Context<Workspace>,
) -> Button {
    let selected = current == tab;
    Button::new(format!("context-tab-{label}"))
        .label(label)
        .small()
        .when(selected, |this| this.primary())
        .when(!selected, |this| this.ghost())
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.on_show_tab(tab, cx);
        }))
}

/// Renders the bounded web search panel.
fn render_web(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let query_input = workspace.web_query_input(window, cx);
    let theme = cx.theme();
    let results: Vec<(String, String, String)> = workspace
        .vm()
        .web_results
        .iter()
        .map(|result| {
            (
                result.url.clone(),
                result.title.clone(),
                result.snippet.clone(),
            )
        })
        .collect();
    div()
        .id("web-panel")
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .id("web-query-row")
                .flex()
                .flex_row()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h(px(30.))
                        .child(Input::new(&query_input)),
                )
                .child(
                    Button::new("web-search-run")
                        .icon(IconName::Search)
                        .small()
                        .primary()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_web_search_run(cx);
                        })),
                ),
        )
        .when(results.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .opacity(0.5)
                    .child("No results yet — enable a backend in Settings \u{2192} Web search"),
            )
        })
        .children(results.into_iter().map(|(url, title, snippet)| {
            div()
                .id(format!("web-result-{}", short_id(&url)))
                .flex()
                .flex_col()
                .gap_0p5()
                .p_2()
                .rounded_md()
                .bg(theme.background)
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(title),
                )
                .child(div().text_xs().opacity(0.7).child(ellipsis(&snippet, 200)))
                .child(div().text_xs().opacity(0.5).child(ellipsis(&url, 80)))
        }))
        .into_any_element()
}

/// Renders the changed-files panel fed by tool activity details.
fn render_changes(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let changed: Vec<String> = workspace
        .vm()
        .active
        .as_ref()
        .map(|conversation| {
            conversation
                .entries
                .iter()
                .filter(|entry| matches!(entry.kind, EntryKind::ToolCall | EntryKind::ToolResult))
                .map(|entry| entry.text.clone())
                .collect()
        })
        .unwrap_or_default();
    div()
        .id("changes-panel")
        .flex()
        .flex_col()
        .gap_2()
        .child(
            Button::new("changes-rollback")
                .icon(IconName::RotateCcw)
                .label("Rollback")
                .small()
                .ghost()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_rollback(cx);
                })),
        )
        .when(changed.is_empty(), |this| {
            this.child(div().text_xs().opacity(0.5).child(
                "File edits from tool runs appear here; Rollback restores every snapshotted file.",
            ))
        })
        .children(changed.into_iter().map(|text| {
            div()
                .id(format!("change-item-{}", short_id(&text)))
                .p_2()
                .rounded_md()
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .bg(theme.background)
                .child(ellipsis(&text, 240))
        }))
        .into_any_element()
}

fn render_overview(workspace: &Workspace, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let vm = workspace.vm();
    let rows: Vec<(String, String)> = match vm.active.as_ref() {
        Some(conversation) => vec![
            ("Session".to_owned(), short_id(&conversation.session_id)),
            ("Branch".to_owned(), short_id(&conversation.branch_id)),
            ("Head".to_owned(), short_id(&conversation.head)),
            (
                "Messages".to_owned(),
                conversation.entries.len().to_string(),
            ),
        ],
        None => vec![("Sessions".to_owned(), vm.sessions.len().to_string())],
    };
    div()
        .id("overview")
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child("Session overview"),
        )
        .children(rows.into_iter().map(|(label, value)| {
            div()
                .id(format!("overview-row-{label}"))
                .flex()
                .flex_row()
                .justify_between()
                .gap_2()
                .text_sm()
                .child(div().opacity(0.6).child(label))
                .child(div().child(value))
        }))
        .when(!vm.usage_totals.is_empty(), |this| {
            this.child(
                div()
                    .id("overview-usage")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .mt_1()
                    .child(div().text_xs().opacity(0.6).child("Token usage"))
                    .children(vm.usage_totals.iter().enumerate().map(
                        |(index, (key, input, output, requests))| {
                            div()
                                .id(format!("usage-{index}"))
                                .flex()
                                .flex_row()
                                .justify_between()
                                .text_sm()
                                .child(div().child(key.clone()))
                                .child(
                                    div().text_xs().opacity(0.7).child(format!(
                                        "{input} in / {output} out ({requests} turns)"
                                    )),
                                )
                        },
                    )),
            )
        })
        .when(!vm.todo_rows.is_empty(), |this| {
            this.child(
                div()
                    .id("overview-todo")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .mt_1()
                    .child(div().text_xs().opacity(0.6).child("Tasks"))
                    .children(
                        vm.todo_rows
                            .iter()
                            .enumerate()
                            .map(|(index, (content, status))| {
                                let mark = match status.as_str() {
                                    "done" => "\u{2705}",
                                    "in progress" => "\u{25b6}",
                                    _ => "\u{2b1c}",
                                };
                                div()
                                    .id(format!("todo-{index}"))
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .text_sm()
                                    .child(div().child(mark))
                                    .child(
                                        div()
                                            .flex_1()
                                            .when(status == "done", |this| this.opacity(0.5))
                                            .child(content.clone()),
                                    )
                                    .child(div().text_xs().opacity(0.5).child(status.clone()))
                            }),
                    ),
            )
        })
        .child(
            div()
                .id("overview-resources")
                .flex()
                .flex_col()
                .gap_1()
                .mt_2()
                .child(div().text_xs().opacity(0.6).child("Prompt resources"))
                .when(vm.resources.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .opacity(0.5)
                            .child("No AGENTS.md / MCODE.md found in the project"),
                    )
                })
                .children(vm.resources.iter().map(|(name, path)| {
                    div()
                        .id(format!("resource-{name}"))
                        .flex()
                        .flex_col()
                        .p_2()
                        .rounded_md()
                        .bg(theme.background)
                        .child(div().text_sm().child(name.clone()))
                        .child(div().text_xs().opacity(0.6).child(ellipsis(path, 60)))
                })),
        )
}
