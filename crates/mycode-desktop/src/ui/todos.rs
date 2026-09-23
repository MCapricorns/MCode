//! Session todos, shown as a quiet checklist above the composer.
//! Completed items are dropped before they reach this surface.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{Context, InteractiveElement, IntoElement, ParentElement, Styled, div, px};

use crate::workspace::Workspace;

/// Checklist in the chat column, above the composer. Completed items never arrive.
pub(super) fn render_todo_inline(
    workspace: &Workspace,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let rows = workspace.vm().todo_rows.clone();
    div()
        .id("todo-inline")
        .px_4()
        .pt_2()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("todos  {}", rows.len())),
        )
        .children(
            rows.into_iter()
                .enumerate()
                .map(|(index, (content, status))| {
                    let active = status == "in progress";
                    div()
                        .id(format!("todo-inline-{index}"))
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap_2()
                        .child(todo_mark(active, theme))
                        .child(
                            div()
                                .text_sm()
                                .whitespace_normal()
                                .text_color(if active {
                                    theme.foreground
                                } else {
                                    theme.muted_foreground
                                })
                                .child(content),
                        )
                }),
        )
}

fn todo_mark(active: bool, theme: &gpui_kit::component::theme::Theme) -> impl IntoElement {
    div()
        .mt(px(3.))
        .size(px(14.))
        .flex_shrink_0()
        .rounded_full()
        .border_1()
        .border_color(if active { theme.primary } else { theme.border })
        .when(active, |mark| mark.bg(theme.primary))
}
