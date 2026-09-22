//! Right inspector: session facts, token bars, last-turn ledger, task queue,
//! and prompt resources — the inspector column.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::theme::Theme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, Div, InteractiveElement, IntoElement, ParentElement, Stateful,
    StatefulInteractiveElement, Styled, Window, div, px,
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
        .border_color(super::skin::glass_border(theme))
        .bg(super::skin::glass_sidebar(theme))
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
        .when(
            vm.selected_model.is_some() || !vm.usage_totals.is_empty() || vm.live_turn.is_some(),
            |this| {
                let selected = vm.selected_model.as_deref();
                let rows: Vec<_> = vm
                    .usage_totals
                    .iter()
                    .filter(|row| {
                        selected.is_none_or(|model| {
                            crate::view_model::usage_key_matches(&row.key, model)
                        })
                    })
                    .collect();
                let live = vm
                    .live_turn
                    .as_ref()
                    .filter(|turn| selected.is_none_or(|model| turn.model == model));
                let mut children = Vec::new();
                if let Some(model) = selected {
                    children.push(
                        div()
                            .text_sm()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(desk.amber)
                            .whitespace_normal()
                            .child(model.to_owned())
                            .into_any_element(),
                    );
                }
                if let Some(turn) = live {
                    children.push(bar_row(
                        "in",
                        turn.input,
                        turn.input.saturating_add(turn.output).max(1),
                        desk.cyan,
                        &format!("{} live", super::compact_count(turn.input)),
                        theme,
                    ));
                    children.push(bar_row(
                        "out",
                        turn.output,
                        turn.input.saturating_add(turn.output).max(1),
                        desk.amber,
                        &format!("{} live", super::compact_count(turn.output)),
                        theme,
                    ));
                } else if rows.is_empty() {
                    children.push(
                        div()
                            .text_xs()
                            .opacity(0.5)
                            .child("No usage for this model yet")
                            .into_any_element(),
                    );
                }
                children.extend(
                    rows.iter()
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
                                    &super::compact_count(row.input),
                                    theme,
                                ))
                                .child(bar_row(
                                    "out",
                                    row.output,
                                    row.input.saturating_add(row.output).max(1),
                                    desk.amber,
                                    &super::compact_count(row.output),
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
                        .collect::<Vec<_>>(),
                );
                this.child(insp_sec(
                    "usage",
                    Some(("TOKEN USAGE".to_owned(), None)),
                    children,
                    theme,
                ))
            },
        )
        .when(
            vm.last_turn.as_ref().is_some_and(|turn| {
                vm.selected_model
                    .as_deref()
                    .is_none_or(|model| turn.model == model)
            }),
            |this| {
                let turn = vm.last_turn.as_ref().expect("checked");
                let shown_model = vm.selected_model.as_deref().unwrap_or(turn.model.as_str());
                let catalog_context = vm.catalog.as_ref().and_then(|catalog| {
                    let provider_id = vm.selected_provider.as_deref()?;
                    let provider = catalog.provider(provider_id)?;
                    provider
                        .models
                        .iter()
                        .find(|model| model.id == shown_model)
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
                    let percent =
                        (turn.input as f64 / context_window as f64 * 1000.0).round() / 10.0;
                    children.push(bar_row(
                        "context",
                        turn.input,
                        context_window,
                        desk.cyan,
                        &format!(
                            "{} / {} ({percent}%)",
                            super::compact_count(turn.input),
                            super::compact_count(context_window)
                        ),
                        theme,
                    ));
                } else {
                    children.push(kv_row(
                        "last-turn-Context",
                        "CONTEXT",
                        &super::compact_count(turn.input),
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
                        &format!("{share}% of {}", super::compact_count(turn.input)),
                        theme,
                    ));
                }
                children.push(bar_row(
                    "output",
                    turn.output,
                    turn.input.max(turn.output).max(1),
                    desk.amber,
                    &super::compact_count(turn.output),
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
            },
        )
        .when(vm.sending, |this| {
            let status = vm
                .active
                .as_ref()
                .and_then(|conversation| conversation.streaming.as_ref())
                .map(|streaming| streaming.status.as_str())
                .filter(|status| !status.is_empty())
                .unwrap_or("Waiting for the model");
            this.child(insp_sec(
                "live",
                Some(("LIVE TURN".to_owned(), None)),
                {
                    let mut rows = vec![kv_row("live-status", "STATUS", status, theme)];
                    if let Some(turn) = vm.live_turn.as_ref().filter(|turn| {
                        vm.selected_model
                            .as_deref()
                            .is_none_or(|model| turn.model == model)
                    }) {
                        rows.push(kv_row(
                            "live-in",
                            "IN",
                            &super::compact_count(turn.input),
                            theme,
                        ));
                        rows.push(kv_row(
                            "live-out",
                            "OUT",
                            &super::compact_count(turn.output),
                            theme,
                        ));
                    }
                    rows
                },
                theme,
            ))
        })
        .when(
            crate::view_model::task_surface_visible(vm) && !vm.live_jobs.is_empty(),
            |this| {
                let running = vm.live_jobs.iter().filter(|job| !job.done).count();
                let summary = if running == 0 {
                    format!("{} done", vm.live_jobs.len())
                } else {
                    format!("{running} running")
                };
                this.child(insp_sec(
                    "subagents",
                    Some(("SUBAGENTS".to_owned(), Some(summary))),
                    vm.live_jobs
                        .iter()
                        .enumerate()
                        .map(|(index, job)| {
                            let (lamp, status, color) = if job.done {
                                (desk.green, "DONE", desk.green)
                            } else {
                                (desk.amber, "RUN", desk.amber)
                            };
                            let role = if job.role.is_empty() {
                                "task".to_owned()
                            } else {
                                job.role.clone()
                            };
                            let call_id = job.call_id.clone();
                            rail_card(
                                format!("subagent-{index}"),
                                lamp,
                                role,
                                job.label.clone(),
                                job.step.clone(),
                                status,
                                color,
                                job.done,
                                theme,
                            )
                            .cursor_pointer()
                            .on_click(cx.listener(move |workspace, _, _, cx| {
                                workspace.on_open_subagent(&call_id, cx);
                            }))
                            .into_any_element()
                        })
                        .collect(),
                    theme,
                ))
            },
        )
        .when(
            crate::view_model::task_surface_visible(vm) && !vm.todo_rows.is_empty(),
            |this| {
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
                            rail_card(
                                format!("todo-{index}"),
                                lamp,
                                String::new(),
                                content.clone(),
                                String::new(),
                                label,
                                color,
                                status == "done",
                                theme,
                            )
                            .into_any_element()
                        })
                        .collect(),
                    theme,
                ))
            },
        )
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
                            .min_w_0()
                            .overflow_hidden()
                            .px_2()
                            .py(px(8.))
                            .rounded(super::skin::radius_control())
                            .bg(super::skin::frost(theme))
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_sm()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(desk.amber)
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(name.clone()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_xs()
                                    .text_color(desk.faint)
                                    .whitespace_nowrap()
                                    .text_ellipsis_start()
                                    .child(path.clone()),
                            )
                            .into_any_element()
                    })
                    .collect()
            },
            theme,
        ))
}

/// One inspector row: a status chip stays on the first line, and the
/// brief and step truncate instead of wrapping into the chip.
#[allow(clippy::too_many_arguments)]
fn rail_card(
    id: impl Into<gpui_kit::ElementId>,
    lamp: gpui_kit::Hsla,
    kicker: String,
    title: String,
    detail: String,
    status: &str,
    status_color: gpui_kit::Hsla,
    dim: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let desk = super::desk::Desk::of(theme);
    div()
        .id(id)
        .flex()
        .flex_col()
        .gap_0p5()
        .min_w_0()
        .overflow_hidden()
        .px_2()
        .py(px(8.))
        .rounded(super::skin::radius_control())
        .bg(super::skin::frost_card(theme))
        .border_1()
        .border_color(super::skin::glass_border(theme))
        .when(dim, |this| this.opacity(0.55))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(super::lamp(lamp))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(desk.amber)
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .when(!kicker.is_empty(), |this| this.child(kicker)),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .px_1()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(status_color)
                        .child(status.to_owned()),
                ),
        )
        .when(!title.is_empty(), |this| {
            this.child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .pl(px(15.))
                    .text_sm()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(title),
            )
        })
        .when(!detail.is_empty(), |this| {
            this.child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .pl(px(15.))
                    .text_xs()
                    .text_color(desk.faint)
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(detail),
            )
        })
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
                .overflow_hidden()
                .text_sm()
                .font_family(theme.mono_font_family.clone())
                .whitespace_nowrap()
                .text_ellipsis()
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

fn window_label(text: &str, theme: &Theme) -> impl IntoElement {
    div()
        .text_xs()
        .font_family(theme.mono_font_family.clone())
        .text_color(theme.muted_foreground)
        .child(text.to_owned())
}

fn window_body(text: String) -> impl IntoElement {
    div().text_sm().whitespace_normal().child(text)
}

/// Small window for one inspector subagent so its progress is readable.
pub(super) fn render_subagent_window(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let call_id = workspace.vm().subagent_window.clone().unwrap_or_default();
    let job = workspace
        .vm()
        .live_jobs
        .iter()
        .find(|job| job.call_id == call_id)
        .cloned();
    let title = job.as_ref().map(|job| {
        if job.role.is_empty() {
            if job.label.is_empty() {
                "Subagent".to_owned()
            } else {
                job.label.clone()
            }
        } else if job.label.is_empty() {
            job.role.clone()
        } else {
            format!("{} \u{b7} {}", job.role, job.label)
        }
    });
    let goal = job
        .as_ref()
        .map(|job| job.label.clone())
        .unwrap_or_default();
    let prompt = job
        .as_ref()
        .map(|job| job.prompt.clone())
        .unwrap_or_default();
    let path = job.as_ref().map(|job| job.path.clone()).unwrap_or_default();
    let now = job.as_ref().map(|job| job.step.clone()).unwrap_or_default();
    let log = job.map(|job| job.log).unwrap_or_default();
    let last = log.len().saturating_sub(1);
    div()
        .id("subagent-window-layer")
        .absolute()
        .top(px(88.))
        .right(px(336.))
        .w(px(360.))
        .max_h(px(420.))
        .flex()
        .flex_col()
        .rounded(px(12.))
        .border_1()
        .border_color(super::skin::glass_border(theme))
        .bg(super::skin::popover(theme))
        .shadow_lg()
        .overflow_hidden()
        .occlude()
        .child(
            div()
                .px_3()
                .py_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .truncate()
                        .child(title.unwrap_or_else(|| "Subagent".to_owned())),
                )
                .child(
                    div()
                        .id("subagent-window-close")
                        .px_2()
                        .py(px(2.))
                        .rounded(px(6.))
                        .text_xs()
                        .cursor_pointer()
                        .text_color(theme.muted_foreground)
                        .hover(|this| this.bg(theme.secondary))
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            cx.stop_propagation();
                            workspace.on_open_subagent("", cx);
                        }))
                        .child("Close"),
                ),
        )
        .child(
            div()
                .id("subagent-window-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .child(window_label("GOAL", theme))
                .child(window_body(if goal.is_empty() {
                    "No short goal yet.".to_owned()
                } else {
                    goal
                }))
                .child(window_label("TASK", theme))
                .child(
                    div()
                        .text_sm()
                        .whitespace_normal()
                        .child(if prompt.is_empty() {
                            "Waiting for the task brief.".to_owned()
                        } else {
                            prompt
                        }),
                )
                .when(!path.is_empty(), |this| {
                    this.child(window_label("PATH", theme))
                        .child(window_body(path))
                })
                .child(window_label(&format!("NOW  {now}"), theme))
                .children(log.into_iter().enumerate().map(|(index, line)| {
                    let current = index == last;
                    div()
                        .id(format!("subagent-log-{index}"))
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(if current {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        })
                        .child(line)
                })),
        )
        .into_any_element()
}
