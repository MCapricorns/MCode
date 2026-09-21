//! Right context panel: one Overview column (session facts, token usage,
//! tasks, prompt resources). Web search is a model tool, not a panel;
//! rollback is not a dedicated view.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::theme::Theme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div,
};

use super::{short_id, skin};
use crate::view_model::cache_percent;
use crate::workspace::Workspace;

pub(super) fn render_context_panel(
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    div()
        .id("context-panel")
        .w(gpui_kit::px(320.))
        .h_full()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .border_l_1()
        .border_color(skin::glass_border(theme))
        .bg(skin::glass_sidebar(theme))
        .child(super::sidebar::pane_head(
            "INSPECTOR",
            None,
            desk.faint,
            theme,
        ))
        .child(
            div()
                .id("context-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_3()
                .pt_2()
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
        .child(section_caption("SESSION", theme))
        .children(
            rows.into_iter().map(|(label, value)| {
                kv_row(&format!("overview-row-{label}"), &label, &value, theme)
            }),
        )
        .when(!vm.usage_totals.is_empty(), |this| {
            this.child(
                div()
                    .id("overview-usage")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .mt_1()
                    .child(section_caption("TOKEN USAGE", theme))
                    .children(vm.usage_totals.iter().enumerate().map(|(index, row)| {
                        let mut figures = format!(
                            "{} in / {} out \u{b7} {} turns",
                            compact(row.input),
                            compact(row.output),
                            row.requests
                        );
                        if let Some(share) = cache_percent(row.cache, row.input) {
                            figures.push_str(&format!(" \u{b7} {share}% cached"));
                        }
                        div()
                            .id(format!("usage-{index}"))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .child(row.key.clone()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .opacity(0.7)
                                    .child(figures),
                            )
                    })),
            )
        })
        .when(vm.last_turn.is_some(), |this| {
            let turn = vm.last_turn.as_ref().expect("checked");
            let mut rows: Vec<(String, String)> = Vec::new();
            let catalog_context = vm.catalog.as_ref().and_then(|catalog| {
                let provider_id = vm.selected_provider.as_deref()?;
                let provider = catalog.provider(provider_id)?;
                provider
                    .models
                    .iter()
                    .find(|model| model.id == turn.model)
                    .map(|model| model.context)
                    .filter(|context| *context > 0)
            });
            // A provider-level override (custom endpoints) wins over the
            // catalog value.
            let override_context = vm
                .settings
                .as_ref()
                .and_then(|settings| {
                    settings.providers.iter().find(|provider| {
                        Some(provider.id.as_str()) == vm.selected_provider.as_deref()
                    })
                })
                .and_then(|provider| provider.context_limit)
                .filter(|context| *context > 0);
            let context_window = override_context.or(catalog_context);
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
                && let Some(share) = cache_percent(cache, turn.input)
            {
                rows.push((
                    "Cache".to_owned(),
                    format!("{share}% of {}", compact(turn.input)),
                ));
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
                    .child(section_caption("LAST TURN", theme))
                    .children(rows.into_iter().map(|(label, value)| {
                        kv_row(&format!("last-turn-{label}"), &label, &value, theme)
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
                    .child(section_caption("TASKS", theme))
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
                                    .child(div().flex_shrink_0().child(mark))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .when(status == "done", |this| this.opacity(0.5))
                                            .child(content.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_xs()
                                            .opacity(0.5)
                                            .child(status.clone()),
                                    )
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
                .child(section_caption("PROMPT RESOURCES", theme))
                .when(vm.resources.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .opacity(0.5)
                            .child("No AGENTS.md / MYCODE.md found in the project"),
                    )
                })
                .children(vm.resources.iter().map(|(name, path)| {
                    div()
                        .id(format!("resource-{name}"))
                        .flex()
                        .flex_col()
                        .p_2()
                        .rounded_md()
                        .bg(theme.secondary.opacity(0.4))
                        .child(div().text_sm().truncate().child(name.clone()))
                        // Absolute paths are long: clip the head so the file
                        // name stays visible in a 320px column.
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis_start()
                                .text_xs()
                                .opacity(0.6)
                                .child(path.clone()),
                        )
                })),
        )
}

/// The demo's `.pane-head` caption for one inspector section.
fn section_caption(label: &'static str, theme: &Theme) -> impl IntoElement {
    div()
        .id(format!("insp-cap-{label}"))
        .text_xs()
        .text_color(super::desk::Desk::of(theme).faint)
        .font_weight(gpui_kit::FontWeight::BOLD)
        .child(label)
}

/// One `.kv` row: mono faint label left, value right.
fn kv_row(id: &str, label: &str, value: &str, theme: &Theme) -> gpui_kit::AnyElement {
    let desk = super::desk::Desk::of(theme);
    div()
        .id(id.to_owned())
        .flex()
        .flex_row()
        .justify_between()
        .gap_2()
        .text_sm()
        .child(
            div()
                .flex_shrink_0()
                .text_color(desk.faint)
                .child(label.to_owned()),
        )
        .child(div().min_w_0().truncate().child(value.to_owned()))
        .into_any_element()
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
