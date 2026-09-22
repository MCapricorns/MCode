//! Right panel: the model this session is using, and its token counts.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::theme::Theme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px,
};

use crate::view_model::cache_percent;
use crate::workspace::Workspace;

pub(super) fn render_context_panel(
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id("context-panel")
        .w(px(260.))
        .h_full()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .border_l_1()
        .border_color(super::skin::glass_border(theme))
        .bg(super::skin::glass_sidebar(theme))
        .child(
            div()
                .id("context-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_3()
                .py_3()
                .flex()
                .flex_col()
                .gap_4()
                .child(render_changes(workspace, cx))
                .child(render_model_usage(workspace, cx)),
        )
}

fn render_changes(workspace: &Workspace, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let git = workspace.git();
    let selected = workspace.git_diff_path();
    div()
        .id("changes")
        .flex()
        .flex_col()
        .gap_2()
        .child(div().text_sm().child("Changes"))
        .when(!git.branch.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(git.branch.clone()),
            )
        })
        .when_some(git.note.clone(), |this, note| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(note),
            )
        })
        .when(git.note.is_none() && git.files.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Working tree clean"),
            )
        })
        .children(git.files.iter().enumerate().map(|(index, file)| {
            let path = file.path.clone();
            let open = selected == Some(file.path.as_str());
            div()
                .id(format!("git-file-{index}"))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .h(px(28.))
                .rounded(px(6.))
                .cursor_pointer()
                .when(open, |row| row.bg(theme.accent))
                .hover(|row| row.bg(theme.secondary_hover))
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    workspace.on_select_git_file(&path);
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(18.))
                        .text_xs()
                        .text_color(theme.primary)
                        .child(file.status.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .truncate()
                        .child(file.path.clone()),
                )
        }))
        .when(selected.is_some(), |this| {
            this.child(
                div()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .whitespace_normal()
                    .text_color(theme.muted_foreground)
                    .child(workspace.git_diff().to_owned()),
            )
        })
}

fn render_model_usage(workspace: &Workspace, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let vm = workspace.vm();
    let model = vm
        .selected_model
        .clone()
        .or_else(|| vm.last_turn.as_ref().map(|turn| turn.model.clone()))
        .or_else(|| vm.live_turn.as_ref().map(|turn| turn.model.clone()));
    let provider = vm.selected_provider.clone().unwrap_or_default();
    let thinking = crate::view_model::selected_reasoning_level(vm);
    let context_window = model_context_window(vm);
    let usage = vm.usage_totals.iter().find(|row| {
        vm.selected_model
            .as_deref()
            .is_none_or(|selected| crate::view_model::usage_key_matches(&row.key, selected))
    });
    let live = vm.live_turn.as_ref().filter(|turn| {
        vm.selected_model
            .as_deref()
            .is_none_or(|selected| turn.model == selected)
    });
    let last = vm.last_turn.as_ref().filter(|turn| {
        vm.selected_model
            .as_deref()
            .is_none_or(|selected| turn.model == selected)
    });

    div()
        .id("model-usage")
        .flex()
        .flex_col()
        .gap_3()
        .child(div().text_sm().child("Model"))
        .child(
            div()
                .text_sm()
                .whitespace_normal()
                .child(model.unwrap_or_else(|| "No model selected".to_owned())),
        )
        .when(!provider.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(provider),
            )
        })
        .child(stat_line("Thinking", thinking, theme))
        .when(context_window > 0, |this| {
            let used = live
                .map(|turn| turn.input)
                .or_else(|| last.map(|turn| turn.input))
                .unwrap_or(0);
            this.child(bar_row(
                "context",
                used,
                context_window,
                theme.cyan,
                &format!(
                    "{} / {}",
                    super::compact_count(used),
                    super::compact_count(context_window)
                ),
                theme,
            ))
        })
        .when_some(usage, |this, row| {
            let share = cache_percent(row.cache, row.input);
            this.child(stat_line("Input", &super::compact_count(row.input), theme))
                .child(stat_line(
                    "Output",
                    &super::compact_count(row.output),
                    theme,
                ))
                .when_some(share, |this, share| {
                    this.child(stat_line("Cache", &format!("{share}%"), theme))
                })
                .child(stat_line("Turns", &row.requests.to_string(), theme))
        })
        .when_some(live, |this, turn| {
            this.child(stat_line(
                "Live",
                &format!(
                    "{} in · {} out",
                    super::compact_count(turn.input),
                    super::compact_count(turn.output)
                ),
                theme,
            ))
        })
        .when(usage.is_none() && live.is_none(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("No usage in this session yet."),
            )
        })
}

fn model_context_window(vm: &crate::view_model::WorkspaceState) -> u64 {
    let shown = vm.selected_model.as_deref();
    let catalog_context = vm.catalog.as_ref().and_then(|catalog| {
        let provider_id = vm.selected_provider.as_deref()?;
        let provider = catalog.provider(provider_id)?;
        provider
            .models
            .iter()
            .find(|model| shown.is_none_or(|id| model.id == id))
            .map(|model| model.context)
            .filter(|context| *context > 0)
    });
    let override_context = vm.settings.as_ref().and_then(|settings| {
        settings
            .providers
            .iter()
            .find(|provider| Some(provider.id.as_str()) == vm.selected_provider.as_deref())
            .and_then(|provider| provider.context_limit)
            .filter(|context| *context > 0)
    });
    override_context.or(catalog_context).unwrap_or(0)
}

fn stat_line(label: &str, value: &str, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label.to_owned()),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.foreground)
                .child(value.to_owned()),
        )
}

fn bar_row(
    label: &str,
    value: u64,
    total: u64,
    color: gpui_kit::Hsla,
    figure: &str,
    theme: &Theme,
) -> impl IntoElement {
    let fill = if total == 0 {
        0.0
    } else {
        (value as f32 / total as f32).clamp(0.04, 1.0)
    };
    div()
        .id(format!("bar-{label}"))
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_row()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Context"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(figure.to_owned()),
                ),
        )
        .child(
            div()
                .h(px(4.))
                .w_full()
                .rounded_full()
                .bg(theme.border)
                .child(
                    div()
                        .h_full()
                        .w(px((200.0 * fill).round()))
                        .rounded_full()
                        .bg(color),
                ),
        )
}

fn window_label(text: &str, theme: &Theme) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(text.to_owned())
}

fn window_body(text: String) -> impl IntoElement {
    div().text_sm().whitespace_normal().child(text)
}

/// Small window for one subagent so its progress is readable.
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
            format!("{} · {}", job.role, job.label)
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
        .top(px(72.))
        .right(px(276.))
        .w(px(340.))
        .max_h(px(420.))
        .flex()
        .flex_col()
        .rounded(px(12.))
        .border_1()
        .border_color(super::skin::glass_border(theme))
        .bg(theme.popover)
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
                        .hover(|this| this.bg(theme.secondary_hover))
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
                .child(window_label("Goal", theme))
                .child(window_body(if goal.is_empty() {
                    "No short goal yet.".to_owned()
                } else {
                    goal
                }))
                .child(window_label("Task", theme))
                .child(window_body(if prompt.is_empty() {
                    "Waiting for the task brief.".to_owned()
                } else {
                    prompt
                }))
                .when(!path.is_empty(), |this| {
                    this.child(window_label("Path", theme))
                        .child(window_body(path))
                })
                .child(window_label(&format!("Now  {now}"), theme))
                .children(log.into_iter().enumerate().map(|(index, line)| {
                    let current = index == last;
                    div()
                        .id(format!("subagent-log-{index}"))
                        .text_xs()
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
