//! The composer card: textarea, chip row (project, model, thinking), the
//! follow-up queue, and the send/stop controls.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::ui::{desk::Desk, ellipsis, project_label, skin};
use crate::view_model::WorkspaceState;
use crate::view_model::{selected_model_supports_reasoning, selected_reasoning_level};
use crate::workspace::Workspace;

/// The bottom composer: a single card with the textarea on top and a chip
/// row (project, model, send) inside it — the opencode desktop shape.
pub(super) fn render_composer(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let composer = workspace.composer().clone();
    if let Some(text) = workspace.take_composer_prefill() {
        composer.update(cx, |state, cx| state.set_value(text, window, cx));
    }
    let model_label: SharedString = model_picker_label(workspace.vm()).into();
    let has_session = workspace.vm().active.is_some();
    let sending = workspace.vm().sending;
    let has_draft = !workspace.vm().composer_draft.trim().is_empty();
    let queued = workspace.vm().queued.clone();
    let has_queue = !queued.is_empty();
    let queue_panel = if has_queue {
        Some(render_queued_followups(queued, cx).into_any_element())
    } else {
        None
    };
    let theme = cx.theme();
    let session_project = workspace
        .vm()
        .active
        .as_ref()
        .and_then(|conversation| {
            workspace
                .vm()
                .session_projects
                .iter()
                .find(|(id, _)| *id == conversation.session_id)
                .map(|(_, project)| project.clone())
        })
        .or_else(|| workspace.vm().project_dir.clone());
    let project_chip_label: SharedString = session_project
        .as_deref()
        .map(project_label)
        .unwrap_or_else(|| "Set folder".to_owned())
        .into();
    let has_project = session_project.is_some();
    let project_icon = if has_project {
        IconName::FolderOpen
    } else {
        IconName::Folder
    };

    div()
        .id("composer")
        .flex()
        .flex_col()
        .w_full()
        .border_t_1()
        .border_color(skin::glass_border(theme))
        .bg(skin::glass(theme))
        .px_4()
        .py_2()
        .when_some(queue_panel, |this, queue| this.child(queue))
        .child(
            div()
                .id("composer-card")
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .w_full()
                .rounded(px(3.))
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .child(
                    div()
                        .id("composer-input")
                        .flex()
                        .flex_row()
                        .gap_2()
                        .w_full()
                        .min_w_0()
                        .overflow_hidden()
                        .text_sm()
                        .child(
                            // The demo's amber `▸` prompt glyph.
                            div()
                                .pt(px(3.))
                                .text_color(Desk::of(theme).amber)
                                .child("▸"),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .min_h(px(40.))
                                .child(Textarea::new(&composer).appearance(false).bordered(false)),
                        ),
                )
                .child(
                    div()
                        .id("composer-chip-row")
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .pt_1()
                        .child(composer_chip(
                            "project",
                            project_icon,
                            project_chip_label,
                            true,
                            |workspace, _window, cx| {
                                workspace.on_open_project_dialog(cx);
                            },
                            cx,
                        ))
                        .child(composer_chip(
                            "model",
                            IconName::Bot,
                            model_label,
                            // `provider · model · thinking` is the longest chip
                            // label; clip it so it never displaces send.
                            true,
                            |workspace, _window, cx| {
                                let open = !workspace.vm().model_menu_open;
                                workspace.on_toggle_model_menu(open, cx);
                            },
                            cx,
                        ))
                        .child(div().flex_1().min_w_0())
                        .when(sending && has_draft, |this| {
                            this.child(
                                Button::new("queue")
                                    .icon(IconName::List)
                                    .label("Queue")
                                    .primary()
                                    .rounded(px(3.))
                                    .flex_shrink_0()
                                    .on_click(cx.listener(|workspace, _, window, cx| {
                                        workspace.on_send(window, cx);
                                    })),
                            )
                        })
                        .when(!sending, |this| {
                            this.child(
                                Button::new("send")
                                    .icon(IconName::ArrowUp)
                                    .primary()
                                    .rounded(px(3.))
                                    .flex_shrink_0()
                                    .disabled(!has_session || (!has_draft && !has_queue))
                                    .on_click(cx.listener(|workspace, _, window, cx| {
                                        workspace.on_send(window, cx);
                                    })),
                            )
                        })
                        .when(sending, |this| {
                            this.child(
                                Button::new("stop")
                                    .icon(IconName::X)
                                    .danger()
                                    .rounded(px(3.))
                                    .flex_shrink_0()
                                    .on_click(cx.listener(|workspace, _, _, cx| {
                                        workspace.on_cancel_chat(cx);
                                    })),
                            )
                        }),
                ),
        )
}

/// One composer chip: icon + label in a bordered pill that opens its menu.
/// `truncate` lets long labels (the model chip) clip instead of displacing
/// the send button.
fn composer_chip(
    id: &str,
    icon: IconName,
    label: SharedString,
    truncate: bool,
    on_click: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id(format!("composer-{id}-chip"))
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .when(truncate, |this| this.min_w_0())
        .when(!truncate, |this| this.flex_shrink_0())
        .px_2()
        .h(px(24.))
        .rounded(px(3.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.sidebar)
        .text_xs()
        .cursor_pointer()
        .text_color(theme.muted_foreground)
        .hover(|this| this.bg(theme.secondary))
        .on_click(cx.listener(move |workspace, _, window, cx| {
            on_click(workspace, window, cx);
        }))
        .child(Icon::new(icon).xsmall().flex_shrink_0())
        .child(
            div()
                .when(truncate, |this| this.min_w_0().truncate())
                .child(label),
        )
}

/// Follow-ups waiting behind the in-flight turn; each row can be dismissed.
fn render_queued_followups(items: Vec<String>, cx: &mut Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let desk = Desk::of(theme);
    div()
        .id("composer-queue")
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .pb_1()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(crate::ui::lamp(desk.amber))
                .child(
                    div()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(desk.faint)
                        .child(format!("QUEUED  {}", items.len())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Sends when this turn ends"),
                )
                .child(
                    div()
                        .id("queued-interrupt")
                        .flex_shrink_0()
                        .px_2()
                        .py(px(2.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.secondary_active)
                        .text_xs()
                        .text_color(theme.foreground)
                        .cursor_pointer()
                        .hover(|this| this.bg(theme.secondary_hover))
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_interrupt_queued(0, cx);
                        }))
                        .child("Interrupt & send"),
                ),
        )
        .children(items.into_iter().enumerate().map(|(index, text)| {
            let preview: SharedString = ellipsis(&text, 72).into();
            div()
                .id(SharedString::from(format!("queued-{index}")))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .min_w_0()
                .h(px(22.))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .truncate()
                        .child(preview),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("queued-remove-{index}")))
                        .cursor_pointer()
                        .flex_shrink_0()
                        .on_click(cx.listener(move |workspace, _, _, cx| {
                            workspace.on_remove_queued(index, cx);
                        }))
                        .child(Icon::new(IconName::X).xsmall()),
                )
        }))
}

/// Composer chip label for the thinking-effort submenu.
fn model_picker_label(vm: &WorkspaceState) -> String {
    let provider = vm
        .selected_provider
        .as_deref()
        .map(|id| {
            vm.catalog
                .as_ref()
                .map(|catalog| catalog.display_name(id))
                .unwrap_or_else(|| id.to_owned())
        })
        .unwrap_or_else(|| "Model".to_owned());
    let base = match vm.selected_model.as_deref() {
        Some(model) => format!("{provider} \u{b7} {model}"),
        None => provider,
    };
    if selected_model_supports_reasoning(vm) {
        format!("{base} \u{b7} {}", selected_reasoning_level(vm))
    } else {
        base
    }
}
