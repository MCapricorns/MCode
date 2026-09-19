//! Right context panel: one Overview column (session facts, token usage,
//! tasks, prompt resources). Web search is a model tool, not a panel;
//! rollback is not a dedicated view.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div,
};

use super::{ellipsis, short_id};
use crate::workspace::Workspace;

pub(super) fn render_context_panel(
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id("context-panel")
        .w(gpui_kit::px(312.))
        .h_full()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .border_l_1()
        .border_color(theme.border)
        .bg(theme.sidebar)
        .child(
            div()
                .id("context-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_3()
                .pt_3()
                .pb_3()
                .child(render_overview(workspace, cx)),
        )
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
        .when(vm.last_turn.is_some(), |this| {
            let turn = vm.last_turn.as_ref().expect("checked");
            let mut rows: Vec<(String, String)> = Vec::new();
            let context_window = vm.catalog.as_ref().and_then(|catalog| {
                let provider_id = vm.selected_provider.as_deref()?;
                let provider = catalog.provider(provider_id)?;
                provider
                    .models
                    .iter()
                    .find(|model| model.id == turn.model)
                    .map(|model| model.context)
                    .filter(|context| *context > 0)
            });
            match context_window {
                Some(window) => {
                    let percent = (turn.input as f64 / window as f64 * 1000.0).round() / 10.0;
                    rows.push((
                        "Context".to_owned(),
                        format!("{} / {} ({percent}%)", compact(turn.input), compact(window)),
                    ));
                }
                None => rows.push(("Context".to_owned(), compact(turn.input))),
            }
            if let Some(cache) = turn.cache
                && turn.input > 0
            {
                rows.push(("Cache".to_owned(), format!("{}%", cache * 100 / turn.input)));
            }
            if turn.elapsed_ms > 0 {
                let per_second = turn.output as f64 / (turn.elapsed_ms as f64 / 1000.0);
                rows.push((
                    "Speed".to_owned(),
                    format!("{per_second:.1} tok/s over {} s", turn.elapsed_ms / 1000),
                ));
            }
            rows.push(("Output".to_owned(), compact(turn.output)));
            this.child(
                div()
                    .id("overview-last-turn")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .mt_1()
                    .child(div().text_xs().opacity(0.6).child("Last turn"))
                    .children(rows.into_iter().map(|(label, value)| {
                        div()
                            .id(format!("last-turn-{label}"))
                            .flex()
                            .flex_row()
                            .justify_between()
                            .gap_2()
                            .text_sm()
                            .child(div().opacity(0.6).child(label))
                            .child(div().child(value))
                    })),
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

/// Compact token-count spelling: 12.3k / 1.2M.
fn compact(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}
