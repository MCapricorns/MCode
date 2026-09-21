//! Floating session task list. Completed items are dropped before they
//! reach this surface, so the card only shows work still open.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, px,
};

use super::{DeskLayout, desk, lamp, skin};
use crate::workspace::Workspace;

/// Compact glass card pinned above the composer.
pub(super) fn render_todo_float(
    workspace: &Workspace,
    layout: DeskLayout,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let desk = desk::Desk::of(theme);
    let rows = workspace.vm().todo_rows.clone();
    let right = if layout.inspector { px(336.) } else { px(16.) };
    div()
        .id("todo-float")
        .absolute()
        .bottom(px(92.))
        .right(right)
        .w(px(280.))
        .max_h(px(280.))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(px(14.))
        .border_1()
        .border_color(skin::glass_border(theme))
        .bg(skin::popover(theme))
        .shadow_lg()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(lamp(desk.cyan))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child("TASKS"),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(rows.len().to_string()),
                ),
        )
        .children(
            rows.into_iter()
                .enumerate()
                .map(|(index, (content, status))| {
                    let (color, tag) = if status == "in progress" {
                        (desk.amber, "RUN")
                    } else {
                        (desk.cyan, "OPEN")
                    };
                    div()
                        .id(format!("todo-float-{index}"))
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap_2()
                        .py(px(6.))
                        .child(lamp(color))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(div().text_xs().text_color(color).child(tag))
                                .child(div().text_sm().whitespace_normal().child(content)),
                        )
                }),
        )
}
