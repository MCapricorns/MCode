//! The chat column: centered conversation transcript, the pending-ask panel,
//! and the integrated composer carrying the project and model chips.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px, rems,
};

use super::{ellipsis, project_label};
use crate::view_model::{ConversationEntry, EntryKind};
use crate::workspace::Workspace;

pub(super) fn render_chat(
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let entries: Vec<ConversationEntry> = workspace
        .vm()
        .active
        .as_ref()
        .map(|conversation| conversation.entries.clone())
        .unwrap_or_default();
    let streaming = workspace
        .vm()
        .active
        .as_ref()
        .and_then(|c| c.streaming.clone());
    div()
        .id("chat")
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(
            div()
                .id("conversation")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .child(
                    div()
                        .id("conversation-inner")
                        .w_full()
                        .flex()
                        .flex_col()
                        .mx_auto()
                        .max_w(rems(46.))
                        .gap_3()
                        .py_4()
                        .px_4()
                        .when(entries.is_empty() && streaming.is_none(), |this| {
                            this.child(render_welcome(workspace, cx))
                        })
                        .children(
                            entries
                                .into_iter()
                                .map(|entry| render_entry(entry, cx.theme()).into_any_element()),
                        )
                        .when_some(streaming, |this, streaming| {
                            this.child(render_streaming_entry(streaming, cx.theme()))
                        }),
                ),
        )
        .child(render_composer(workspace, cx))
        .into_any_element()
}

/// The in-flight assistant turn: collapsed thinking plus a bubble.
fn render_streaming_entry(
    streaming: crate::view_model::StreamingReply,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .id("streaming-entry")
        .flex()
        .flex_col()
        .gap_1()
        .when(!streaming.thinking.is_empty(), |this| {
            this.child(
                div()
                    .id("streaming-thinking")
                    .text_xs()
                    .opacity(0.55)
                    .overflow_hidden()
                    .child(ellipsis(&streaming.thinking, 400)),
            )
        })
        .when(!streaming.text.is_empty(), |this| {
            this.child(
                div()
                    .id("streaming-text")
                    .text_sm()
                    .self_start()
                    .max_w(rems(40.))
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .rounded_tl(px(4.))
                    .bg(theme.secondary)
                    .child(streaming.text),
            )
        })
}

/// The welcome hero shown when no conversation has started.
fn render_welcome(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let recents = workspace.vm().recents.clone();
    div()
        .id("welcome")
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .py(px(48.))
        .child(
            div()
                .id("welcome-logo")
                .size(px(52.))
                .rounded(px(14.))
                .bg(theme.primary)
                .text_color(theme.primary_foreground)
                .flex()
                .items_center()
                .justify_center()
                .text_xl()
                .font_weight(gpui_kit::FontWeight::BOLD)
                .child("M"),
        )
        .child(
            div()
                .text_xl()
                .font_weight(gpui_kit::FontWeight::BOLD)
                .child("MCode"),
        )
        .child(
            div()
                .text_sm()
                .opacity(0.6)
                .child("A coding agent for your desktop. Pick a project or just start chatting."),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .mt_2()
                .child(
                    Button::new("welcome-open-project")
                        .icon(IconName::FolderOpen)
                        .label("Open project folder")
                        .primary()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_open_project_dialog(cx);
                        })),
                )
                .child(
                    Button::new("welcome-new-chat")
                        .icon(IconName::MessageSquare)
                        .label("Just start chatting")
                        .ghost()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_new_session(cx);
                        })),
                ),
        )
        .when(!recents.is_empty(), |this| {
            this.child(
                div()
                    .id("welcome-recents")
                    .mt_4()
                    .w(px(440.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .opacity(0.5)
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("RECENT PROJECTS"),
                    )
                    .children(recents.iter().take(5).map(|project| {
                        let project = project.clone();
                        div()
                            .id(format!("recent-{}", super::short_id(&project)))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py(px(6.))
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|this| this.bg(theme.secondary))
                            .on_click({
                                let project = project.clone();
                                cx.listener(move |workspace, _, _, cx| {
                                    workspace.on_open_recent(&project, cx);
                                })
                            })
                            .child(
                                Icon::new(IconName::Folder)
                                    .xsmall()
                                    .text_color(theme.muted_foreground),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .overflow_hidden()
                                    .child(project_label(&project)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .opacity(0.4)
                                    .overflow_hidden()
                                    .child(project.clone()),
                            )
                    })),
            )
        })
        .into_any_element()
}

/// Flat transcript entry rendering: user rows as tinted cards, assistant
/// text bare, tool activity as compact mono rows.
fn render_entry(entry: ConversationEntry, theme: &Theme) -> impl IntoElement {
    match entry.kind {
        EntryKind::UserMessage => div()
            .id(format!("entry-{}", entry.event_id))
            .flex()
            .flex_col()
            .items_end()
            .child(
                div()
                    .max_w(rems(40.))
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .rounded_tr(px(4.))
                    .text_sm()
                    .bg(theme.primary)
                    .text_color(theme.primary_foreground)
                    .child(entry.text),
            ),
        EntryKind::AssistantMessage => div()
            .id(format!("entry-{}", entry.event_id))
            .flex()
            .flex_col()
            .items_start()
            .w_full()
            .child(
                div()
                    .text_sm()
                    .w_full()
                    .py_1()
                    .overflow_hidden()
                    .child(entry.text),
            ),
        EntryKind::ToolCall => div()
            .id(format!("entry-{}", entry.event_id))
            .flex()
            .flex_col()
            .items_start()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py(px(3.))
                    .rounded_md()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .border_1()
                    .border_color(theme.border)
                    .child(Icon::new(IconName::Wrench).xsmall())
                    .child(ellipsis(&entry.text, 160)),
            ),
        EntryKind::ToolResult => {
            let failed = entry.text.starts_with("failed:");
            div()
                .id(format!("entry-{}", entry.event_id))
                .flex()
                .flex_col()
                .items_start()
                .child(
                    div()
                        .max_w(rems(42.))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .when(failed, |this| this.text_color(theme.danger))
                        .when(!failed, |this| this.opacity(0.75))
                        .child(ellipsis(&entry.text, 600)),
                )
        }
        EntryKind::Usage => div().id(format!("entry-{}", entry.event_id)),
    }
}

/// The bottom composer: a single card with the textarea on top and a chip
/// row (project, model, send) inside it — the opencode desktop shape.
fn render_composer(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let composer = workspace.composer().clone();
    let model_label: SharedString = model_picker_label(workspace.vm()).into();
    let has_session = workspace.vm().active.is_some();
    let sending = workspace.vm().sending;
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

    div().id("composer").flex().w_full().px_4().pb_4().child(
        div()
            .id("composer-card")
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .mx_auto()
            .w_full()
            .max_w(rems(46.))
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .shadow_sm()
            .child(
                div()
                    .id("composer-input")
                    .text_sm()
                    .min_h(px(56.))
                    .child(Textarea::new(&composer)),
            )
            .child(
                div()
                    .id("composer-chip-row")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .pt_1()
                    .child(
                        div()
                            .id("composer-project-chip")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .h(px(24.))
                            .rounded(px(12.))
                            .text_xs()
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|this| this.bg(theme.secondary))
                            .on_click(cx.listener(|workspace, _, _, cx| {
                                workspace.on_open_project_dialog(cx);
                            }))
                            .child(
                                Icon::new(if has_project {
                                    IconName::FolderOpen
                                } else {
                                    IconName::Folder
                                })
                                .xsmall(),
                            )
                            .child(project_chip_label),
                    )
                    .child(
                        div()
                            .id("composer-model-chip")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .h(px(24.))
                            .rounded(px(12.))
                            .text_xs()
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|this| this.bg(theme.secondary))
                            .on_click(cx.listener(|workspace, _, _, cx| {
                                let open = !workspace.vm().model_menu_open;
                                workspace.on_toggle_model_menu(open, cx);
                            }))
                            .child(Icon::new(IconName::Bot).xsmall())
                            .child(model_label),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("send")
                            .icon(IconName::ArrowUp)
                            .primary()
                            .rounded(px(20.))
                            .disabled(sending || !has_session)
                            .on_click(cx.listener(|workspace, _, window, cx| {
                                workspace.on_send(window, cx);
                            })),
                    ),
            ),
    )
}

fn model_picker_label(vm: &crate::view_model::WorkspaceState) -> String {
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
    match vm.selected_model.as_deref() {
        Some(model) => format!("{provider} \u{b7} {model}"),
        None => provider,
    }
}

/// The model picker dropdown, pinned above the composer: provider rows and
/// the selected provider's configured models.
pub(super) fn render_model_menu_layer(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let selected_provider = workspace.vm().selected_provider.clone();
    let selected_model = workspace.vm().selected_model.clone();
    let providers: Vec<(String, String)> = workspace
        .vm()
        .settings
        .as_ref()
        .map(|settings| {
            settings
                .providers
                .iter()
                .filter(|provider| provider.enabled)
                .map(|provider| {
                    let name = workspace
                        .vm()
                        .catalog
                        .as_ref()
                        .map(|catalog| catalog.display_name(&provider.id))
                        .unwrap_or_else(|| provider.id.clone());
                    (provider.id.clone(), name)
                })
                .collect()
        })
        .unwrap_or_default();
    let models: Vec<String> = selected_provider
        .as_deref()
        .and_then(|provider_id| {
            workspace
                .vm()
                .settings
                .as_ref()
                .and_then(|settings| {
                    settings
                        .providers
                        .iter()
                        .find(|provider| provider.id == *provider_id)
                })
                .map(|provider| provider.models.iter().take(64).cloned().collect::<Vec<_>>())
        })
        .unwrap_or_default();
    let providers_empty = providers.is_empty();
    let models_empty = models.is_empty();
    div()
        .id("model-menu-layer")
        .absolute()
        .inset_0()
        .child(
            div()
                .id("model-menu-backdrop")
                .absolute()
                .size_full()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_toggle_model_menu(false, cx);
                })),
        )
        .child(
            div()
                .id("model-menu")
                .absolute()
                .bottom(px(88.))
                .right(px(16.))
                .w(px(330.))
                .max_h(px(420.))
                .overflow_y_scroll()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .text_color(theme.popover_foreground)
                .shadow_lg()
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .opacity(0.6)
                        .px_2()
                        .pt_1()
                        .child("PROVIDER"),
                )
                .children(providers.into_iter().map(|(id, name)| {
                    let selected = Some(&id) == selected_provider.as_ref();
                    menu_row(
                        format!("provider-{id}"),
                        name,
                        selected,
                        cx.listener(move |workspace, _, _, cx| {
                            workspace.on_select_provider(&id, cx);
                        }),
                        cx,
                    )
                }))
                .when(providers_empty, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .opacity(0.5)
                            .px_2()
                            .child("No enabled providers — add one in Settings \u{2192} Models"),
                    )
                })
                .when(!models_empty, |this| {
                    this.child(
                        div()
                            .border_t_1()
                            .border_color(theme.border)
                            .mt_1()
                            .pt_1()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .opacity(0.6)
                                    .px_2()
                                    .pb_1()
                                    .child("MODEL"),
                            ),
                    )
                    .children(models.into_iter().map(|id| {
                        let selected = Some(&id) == selected_model.as_ref();
                        let label = id.clone();
                        menu_row(
                            format!("model-{id}"),
                            label,
                            selected,
                            cx.listener(move |workspace, _, _, cx| {
                                workspace.on_select_model(&id, cx);
                            }),
                            cx,
                        )
                    }))
                }),
        )
        .into_any_element()
}

fn menu_row(
    id: String,
    label: String,
    selected: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .py(px(5.))
        .rounded_md()
        .text_sm()
        .cursor_pointer()
        .hover(|this| this.bg(theme.secondary))
        .on_click(on_click)
        .child(div().overflow_hidden().child(label))
        .when(selected, |this| {
            this.child(
                Icon::new(IconName::Check)
                    .xsmall()
                    .text_color(theme.primary),
            )
        })
}

/// Renders the pending ask panel: one answer row per question.
pub(super) fn render_ask_panel(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let rows: Vec<(String, Vec<String>, bool)> =
        workspace.vm().pending_ask.clone().unwrap_or_default();
    let ask_input = workspace.ask_input(window, cx);
    let theme = cx.theme();
    div()
        .id("ask-panel")
        .flex()
        .w_full()
        .px_4()
        .flex_col()
        .gap_2()
        .pb_2()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(Icon::new(IconName::CircleAlert).small())
                .child("The agent needs your input"),
        )
        .children(
            rows.iter()
                .enumerate()
                .map(|(index, (question, choices, optional))| {
                    div()
                        .id(format!("ask-row-{index}"))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .child(format!("{}. {}", index + 1, question)),
                        )
                        .when(!choices.is_empty(), |this| {
                            this.child(
                                div()
                                    .id(format!("ask-choices-{index}"))
                                    .flex()
                                    .flex_row()
                                    .flex_wrap()
                                    .gap_1()
                                    .children(choices.iter().enumerate().map(
                                        |(choice_index, choice)| {
                                            let answer = choice.clone();
                                            Button::new(format!(
                                                "ask-{index}-{choice_index}-{}",
                                                super::short_id(choice)
                                            ))
                                            .label(choice.clone())
                                            .small()
                                            .outline()
                                            .on_click(
                                                cx.listener(move |workspace, _, _, cx| {
                                                    let mut answers =
                                                        vec![String::new(); index + 1];
                                                    answers[index] = answer.clone();
                                                    workspace.on_answer_ask(answers, cx);
                                                }),
                                            )
                                        },
                                    )),
                            )
                        })
                        .when(*optional, |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .opacity(0.5)
                                    .child("This question is optional"),
                            )
                        })
                }),
        )
        .child(
            div()
                .id("ask-free-row")
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id("ask-free-input")
                        .flex_1()
                        .min_w_0()
                        .h(px(30.))
                        .text_sm()
                        .child(Input::new(&ask_input)),
                )
                .child(
                    Button::new("ask-submit")
                        .label("Answer all")
                        .small()
                        .primary()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_submit_free_ask(cx);
                        })),
                ),
        )
        .border_t_1()
        .border_color(theme.border)
        .into_any_element()
}

// The form state types live with the settings view; re-exported for the
// workspace module.
