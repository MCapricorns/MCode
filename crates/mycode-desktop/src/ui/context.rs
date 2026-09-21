//! Right inspector: session facts, token bars, last-turn ledger, task queue,
//! and prompt resources — the demo.html `.insp` column.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::theme::Theme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px,
};

use super::short_id;
use crate::view_model::cache_percent;
use crate::workspace::Workspace;

pub(super) fn render_context_panel(
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    let active_id = workspace
        .vm()
        .active
        .as_ref()
        .map(|conversation| short_id(&conversation.session_id));
    div()
        .id("context-panel")
        .w(px(320.))
        .h_full()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .border_l_1()
        .border_color(theme.border)
        .bg(theme.sidebar)
        .child(super::sidebar::pane_head(
            "INSPECTOR",
            active_id.as_deref(),
            desk.faint,
            theme,
        ))
        .child(
            div()
                .id("context-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(render_overview(workspace, cx)),
        )
}

fn render_overview(workspace: &Workspace, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    let vm = workspace.vm();
    let session_rows: Vec<(String, String)> = match vm.active.as_ref() {
        Some(conversation) => vec![
            ("SESSION".to_owned(), short_id(&conversation.session_id)),
            ("BRANCH".to_owned(), short_id(&conversation.branch_id)),
            ("HEAD".to_owned(), short_id(&conversation.head)),
            (
                "MESSAGES".to_owned(),
                conversation.entries.len().to_string(),
            ),
        ],
        None => vec![("SESSIONS".to_owned(), vm.sessions.len().to_string())],
    };
    div()
        .id("overview")
        .flex()
        .flex_col()
        .child(insp_sec(
            "session",
            Some(("SESSION".to_owned(), None)),
            session_rows
                .into_iter()
                .map(|(label, value)| {
                    kv_row(&format!("overview-row-{label}"), &label, &value, theme)
                })
                .collect(),
            theme,
        ))
        .when(!vm.usage_totals.is_empty(), |this| {
            this.child(insp_sec(
                "usage",
                Some(("TOKEN USAGE".to_owned(), None)),
                vm.usage_totals
                    .iter()
                    .enumerate()
                    .map(|(index, row)| {
                        let share = cache_percent(row.cache, row.input);
                        div()
                            .id(format!("usage-{index}"))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(desk.amber)
                                    .whitespace_normal()
                                    .child(row.key.clone()),
                            )
                            .child(bar_row(
                                "in",
                                row.input,
                                row.input.saturating_add(row.output).max(1),
                                desk.cyan,
                                &compact(row.input),
                                theme,
                            ))
                            .child(bar_row(
                                "out",
                                row.output,
                                row.input.saturating_add(row.output).max(1),
                                desk.amber,
                                &compact(row.output),
                                theme,
                            ))
                            .when_some(share, |this, share| {
                                this.child(bar_row(
                                    "cache",
                                    share,
                                    100,
                                    desk.green,
                                    &format!("{share}% · {} turns", row.requests),
                                    theme,
                                ))
                            })
                            .into_any_element()
                    })
                    .collect(),
                theme,
            ))
        })
        .when(vm.last_turn.is_some(), |this| {
            let turn = vm.last_turn.as_ref().expect("checked");
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
            let context_window = override_context.or(catalog_context).unwrap_or(0);
            let mut children = vec![
                div()
                    .text_sm()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(desk.amber)
                    .whitespace_normal()
                    .child(turn.model.clone())
                    .into_any_element(),
            ];
            if context_window > 0 {
                let percent = (turn.input as f64 / context_window as f64 * 1000.0).round() / 10.0;
                children.push(bar_row(
                    "context",
                    turn.input,
                    context_window,
                    desk.cyan,
                    &format!(
                        "{} / {} ({percent}%)",
                        compact(turn.input),
                        compact(context_window)
                    ),
                    theme,
                ));
            } else {
                children.push(kv_row(
                    "last-turn-Context",
                    "CONTEXT",
                    &compact(turn.input),
                    theme,
                ));
            }
            if let Some(cache) = turn.cache
                && let Some(share) = cache_percent(cache, turn.input)
            {
                children.push(bar_row(
                    "cache",
                    share,
                    100,
                    desk.green,
                    &format!("{share}% of {}", compact(turn.input)),
                    theme,
                ));
            }
            children.push(bar_row(
                "output",
                turn.output,
                turn.input.max(turn.output).max(1),
                desk.amber,
                &compact(turn.output),
                theme,
            ));
            if turn.elapsed_ms > 0 {
                let per_second = turn.output as f64 / (turn.elapsed_ms as f64 / 1000.0);
                children.push(kv_row(
                    "last-turn-Speed",
                    "SPEED",
                    &format!("{per_second:.1} tok/s · {}s", turn.elapsed_ms / 1000),
                    theme,
                ));
            }
            this.child(insp_sec(
                "last-turn",
                Some(("LAST TURN".to_owned(), None)),
                children,
                theme,
            ))
        })
        .when(!vm.todo_rows.is_empty(), |this| {
            this.child(insp_sec(
                "tasks",
                Some(("SESSION QUEUE".to_owned(), None)),
                vm.todo_rows
                    .iter()
                    .enumerate()
                    .map(|(index, (content, status))| {
                        let (lamp, label, color) = match status.as_str() {
                            "done" => (desk.green, "DONE", desk.green),
                            "in progress" => (desk.amber, "RUN", desk.amber),
                            _ => (desk.faint, "QUEUE", desk.faint),
                        };
                        div()
                            .id(format!("todo-{index}"))
                            .flex()
                            .flex_row()
                            .items_start()
                            .gap_2()
                            .py(px(6.))
                            .border_b_1()
                            .border_dashed()
                            .border_color(theme.border)
                            .child(super::lamp(lamp))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .whitespace_normal()
                                    .when(status == "done", |this| this.opacity(0.5))
                                    .child(content.clone()),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(color)
                                    .child(label),
                            )
                            .into_any_element()
                    })
                    .collect(),
                theme,
            ))
        })
        .child(insp_sec(
            "resources",
            Some(("PROMPT RESOURCES".to_owned(), None)),
            if vm.resources.is_empty() {
                vec![
                    div()
                        .text_xs()
                        .text_color(desk.faint)
                        .whitespace_normal()
                        .child("No AGENTS.md / MYCODE.md in this project")
                        .into_any_element(),
                ]
            } else {
                vm.resources
                    .iter()
                    .map(|(name, path)| {
                        div()
                            .id(format!("resource-{name}"))
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .py(px(6.))
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .text_sm()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(desk.amber)
                                    .whitespace_normal()
                                    .child(name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(desk.faint)
                                    .whitespace_normal()
                                    .child(path.clone()),
                            )
                            .into_any_element()
                    })
                    .collect()
            },
            theme,
        ))
}

fn insp_sec(
    id: &str,
    title: Option<(String, Option<String>)>,
    children: Vec<gpui_kit::AnyElement>,
    theme: &Theme,
) -> gpui_kit::AnyElement {
    let desk = super::desk::Desk::of(theme);
    div()
        .id(format!("insp-sec-{id}"))
        .flex()
        .flex_col()
        .gap_2()
        .px_3()
        .py_3()
        .border_b_1()
        .border_color(theme.border)
        .when_some(title, |this, (caption, sub)| {
            this.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .font_weight(gpui_kit::FontWeight::BOLD)
                            .text_color(desk.faint)
                            .child(caption),
                    )
                    .when_some(sub, |this, sub| {
                        this.child(div().text_xs().text_color(desk.faint).child(sub))
                    }),
            )
        })
        .children(children)
        .into_any_element()
}

fn kv_row(id: &str, label: &str, value: &str, theme: &Theme) -> gpui_kit::AnyElement {
    let desk = super::desk::Desk::of(theme);
    div()
        .id(id.to_owned())
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .child(
            div()
                .w(px(78.))
                .flex_shrink_0()
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .text_color(desk.faint)
                .child(label.to_owned()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .font_family(theme.mono_font_family.clone())
                .whitespace_normal()
                .child(value.to_owned()),
        )
        .into_any_element()
}

fn bar_row(
    label: &str,
    value: u64,
    total: u64,
    color: gpui_kit::Hsla,
    figure: &str,
    theme: &Theme,
) -> gpui_kit::AnyElement {
    let desk = super::desk::Desk::of(theme);
    let fill = if total == 0 {
        0.0
    } else {
        (value as f32 / total as f32).clamp(0.04, 1.0)
    };
    div()
        .id(format!("bar-{label}"))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(52.))
                .flex_shrink_0()
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .text_color(desk.faint)
                .child(label.to_owned()),
        )
        .child(
            div()
                .flex_1()
                .h(px(5.))
                .rounded(px(1.))
                .bg(theme.border)
                .child(
                    div()
                        .h_full()
                        .w(px((148.0 * fill).round()))
                        .rounded(px(1.))
                        .bg(color),
                ),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .text_color(desk.faint)
                .child(figure.to_owned()),
        )
        .into_any_element()
}

fn compact(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}
