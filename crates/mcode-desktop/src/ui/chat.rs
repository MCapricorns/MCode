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

use super::{ellipsis, project_label, skin};
use crate::view_model::{ConversationEntry, EntryKind};
use crate::workspace::Workspace;

pub(super) fn render_chat(
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    // Entries render straight from state: cloning the whole transcript per
    // frame made every notify (menu toggles, stream deltas) allocate all
    // message text again. The borrow is scoped so the welcome and composer
    // builders can still take `&mut Workspace`.
    let (show_welcome, entry_elements, streaming_element) = {
        let active = workspace.vm().active.as_ref();
        let entries: &[ConversationEntry] = active
            .map(|conversation| conversation.entries.as_slice())
            .unwrap_or_default();
        let streaming = active.and_then(|c| c.streaming.as_ref());
        let show_welcome = entries.is_empty() && streaming.is_none();
        let elements: Vec<gpui_kit::AnyElement> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                if entry.kind == EntryKind::UserMessage {
                    render_user_entry(entry, index > 0, index, cx)
                } else {
                    render_entry(entry, cx.theme()).into_any_element()
                }
            })
            .collect();
        let streaming_element = streaming
            .map(|streaming| render_streaming_entry(streaming, cx.theme()).into_any_element());
        (show_welcome, elements, streaming_element)
    };
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
                        .when(show_welcome, |this| {
                            this.child(render_welcome(workspace, cx))
                        })
                        .children(entry_elements)
                        .when_some(streaming_element, |this, streaming| this.child(streaming)),
                ),
        )
        .child(render_composer(workspace, _window, cx))
        .into_any_element()
}

/// The in-flight assistant turn: collapsed thinking plus a bubble.
fn render_streaming_entry(
    streaming: &crate::view_model::StreamingReply,
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
                    .child(streaming.text.clone()),
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
                .bg(skin::accent(theme, 135.))
                .text_color(theme.primary_foreground)
                .shadow_lg()
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
                                div()
                                    .id(format!("recent-remove-{}", super::short_id(&project)))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(20.))
                                    .rounded(px(5.))
                                    .opacity(0.0)
                                    .group_hover("recent-row", |this| this.opacity(1.0))
                                    .cursor_pointer()
                                    .text_color(theme.muted_foreground)
                                    .hover(|this| this.bg(theme.secondary))
                                    .on_click({
                                        let project = project.clone();
                                        cx.listener(move |workspace, _, _, cx| {
                                            workspace.on_remove_recent(&project, cx);
                                        })
                                    })
                                    .child(Icon::new(IconName::X).xsmall()),
                            )
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
fn render_entry(entry: &ConversationEntry, theme: &Theme) -> impl IntoElement {
    match entry.kind {
        EntryKind::UserMessage => div()
            .id(format!("entry-{}", entry.event_id))
            .flex()
            .flex_col()
            .items_end()
            .child(
                div()
                    .max_w(rems(28.))
                    .px(px(10.))
                    .py(px(5.))
                    .rounded(px(12.))
                    .rounded_tr(px(4.))
                    .text_sm()
                    .bg(skin::accent(theme, 135.))
                    .text_color(theme.primary_foreground)
                    .shadow_sm()
                    .child(SharedString::from(&entry.text)),
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
                    .child(SharedString::from(&entry.text)),
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
fn render_composer(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let composer = workspace.composer().clone();
    if let Some(text) = workspace.take_composer_prefill() {
        composer.update(cx, |state, cx| state.set_value(text, window, cx));
    }
    let theme = cx.theme();
    let model_label: SharedString = model_picker_label(workspace.vm()).into();
    let reasoning_label: SharedString = reasoning_chip_label(workspace.vm()).into();
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
            .rounded(px(16.))
            .border_1()
            .border_color(skin::glass_border(theme))
            .bg(skin::glass(theme))
            .shadow_lg()
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
                    .child(
                        div()
                            .id("composer-reasoning-chip")
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
                                let current = workspace
                                    .vm()
                                    .settings
                                    .as_ref()
                                    .and_then(|settings| settings.reasoning.clone())
                                    .unwrap_or_else(|| "default".to_owned());
                                let levels = ["default", "low", "medium", "high"];
                                let index = levels
                                    .iter()
                                    .position(|level| *level == current)
                                    .unwrap_or(0);
                                let next = levels[(index + 1) % levels.len()];
                                workspace.on_select_reasoning(next, cx);
                            }))
                            .child(Icon::new(IconName::Sparkles).xsmall())
                            .child(reasoning_label),
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

/// One user bubble with hover actions: edit-and-resend (rewinds to before
/// this message and prefills the composer) and recall (drops this message
/// and everything after). The first message has no prior event to rewind
/// to, so its actions hide.
fn render_user_entry(
    entry: &ConversationEntry,
    can_rewind: bool,
    index: usize,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let mut bubble = div()
        .id(format!("entry-{}", entry.event_id))
        .flex()
        .flex_col()
        .items_end()
        .gap_1()
        .group("user-entry")
        .child(
            div()
                .max_w(rems(28.))
                .px(px(10.))
                .py(px(5.))
                .rounded(px(12.))
                .rounded_tr(px(4.))
                .text_sm()
                .bg(skin::accent(theme, 135.))
                .text_color(theme.primary_foreground)
                .shadow_sm()
                .child(SharedString::from(&entry.text)),
        );
    if can_rewind {
        bubble = bubble.child(
            div()
                .id(format!("entry-actions-{index}"))
                .flex()
                .flex_row()
                .gap_1()
                .opacity(0.0)
                .group_hover("user-entry", |this| this.opacity(1.0))
                .child(
                    div()
                        .id(format!("entry-edit-{index}"))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .h(px(22.))
                        .rounded(px(6.))
                        .text_xs()
                        .cursor_pointer()
                        .text_color(theme.muted_foreground)
                        .hover(|this| this.bg(theme.secondary))
                        .on_click(cx.listener(move |workspace, _, _, cx| {
                            workspace.on_edit_message(index, cx);
                        }))
                        .child(Icon::new(IconName::Pen).xsmall())
                        .child("edit"),
                )
                .child(
                    div()
                        .id(format!("entry-recall-{index}"))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .h(px(22.))
                        .rounded(px(6.))
                        .text_xs()
                        .cursor_pointer()
                        .text_color(theme.muted_foreground)
                        .hover(|this| this.bg(theme.secondary))
                        .on_click(cx.listener(move |workspace, _, _, cx| {
                            workspace.on_recall_message(index, cx);
                        }))
                        .child(Icon::new(IconName::RefreshCcw).xsmall())
                        .child("recall"),
                ),
        );
    }
    bubble.into_any_element()
}

/// Composer chip label for the thinking-effort cycle button.
fn reasoning_chip_label(vm: &crate::view_model::WorkspaceState) -> String {
    match vm
        .settings
        .as_ref()
        .and_then(|settings| settings.reasoning.as_deref())
    {
        Some("low") => "Thinking \u{b7} low".to_owned(),
        Some("medium") => "Thinking \u{b7} med".to_owned(),
        Some("high") => "Thinking \u{b7} high".to_owned(),
        _ => "Thinking \u{b7} auto".to_owned(),
    }
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

/// One row in the flattened model menu: section headers, providers, models,
/// and thinking levels share a fixed height so `uniform_list` only builds
/// the visible slice instead of rebuilding every row on each frame.
enum ModelMenuRow {
    /// Section label; `divider` draws the separator line above it.
    Header { label: &'static str, divider: bool },
    /// Non-interactive note row.
    Hint(&'static str),
    /// One enabled provider from settings.
    Provider {
        id: String,
        name: String,
        selected: bool,
    },
    /// One configured model of the selected provider.
    Model { id: String, selected: bool },
    /// One reasoning-effort level.
    Reasoning {
        level: &'static str,
        label: String,
        selected: bool,
    },
}

/// Fixed row height that keeps the virtualized menu uniform.
const MENU_ROW_HEIGHT: gpui_kit::Pixels = px(30.);

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
                .map(|provider| provider.models.clone())
        })
        .unwrap_or_default();
    let selected_reasoning = workspace
        .vm()
        .settings
        .as_ref()
        .and_then(|settings| settings.reasoning.clone())
        .unwrap_or_else(|| "default".to_owned());

    let mut rows: Vec<ModelMenuRow> = vec![ModelMenuRow::Header {
        label: "PROVIDER",
        divider: false,
    }];
    if providers.is_empty() {
        rows.push(ModelMenuRow::Hint(
            "No enabled providers — add one in Settings \u{2192} Models",
        ));
    } else {
        rows.extend(
            providers
                .into_iter()
                .map(|(id, name)| ModelMenuRow::Provider {
                    selected: Some(&id) == selected_provider.as_ref(),
                    id,
                    name,
                }),
        );
    }
    if !models.is_empty() {
        rows.push(ModelMenuRow::Header {
            label: "MODEL",
            divider: true,
        });
        rows.extend(models.into_iter().map(|id| ModelMenuRow::Model {
            selected: Some(&id) == selected_model.as_ref(),
            id,
        }));
    }
    rows.push(ModelMenuRow::Header {
        label: "THINKING",
        divider: true,
    });
    rows.extend(
        ["default", "low", "medium", "high"].map(|level| ModelMenuRow::Reasoning {
            selected: level == selected_reasoning,
            label: match level {
                "low" => "Low \u{b7} brief".to_owned(),
                "medium" => "Medium \u{b7} balanced".to_owned(),
                "high" => "High \u{b7} deep".to_owned(),
                other => format!("{other} \u{b7} provider default"),
            },
            level,
        }),
    );

    let rows = std::rc::Rc::new(rows);
    let weak = cx.weak_entity();
    let list = gpui_kit::uniform_list("model-menu-rows", rows.len(), move |range, _window, cx| {
        let theme = cx.theme();
        range
            .map(|index| model_menu_row(&rows[index], &weak, theme))
            .collect()
    });
    div()
        .id("model-menu-layer")
        .absolute()
        .inset_0()
        .child(
            div()
                .id("model-menu-backdrop")
                .absolute()
                .size_full()
                .bg(skin::scrim(theme))
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
                .rounded(px(14.))
                .border_1()
                .border_color(skin::glass_border(theme))
                .bg(skin::popover(theme))
                .text_color(theme.popover_foreground)
                .shadow_lg()
                .p_2()
                .child(list.w_full().max_h(px(404.))),
        )
        .into_any_element()
}

/// Renders one visible row of the virtualized model menu.
fn model_menu_row(
    row: &ModelMenuRow,
    weak: &gpui_kit::WeakEntity<Workspace>,
    theme: &Theme,
) -> gpui_kit::AnyElement {
    match row {
        ModelMenuRow::Header { label, divider } => div()
            .id(format!("model-menu-header-{label}"))
            .h(MENU_ROW_HEIGHT)
            .flex()
            .flex_row()
            .items_center()
            .px_2()
            .when(*divider, |this| {
                this.border_t_1().border_color(theme.border)
            })
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .opacity(0.6)
                    .child(*label),
            )
            .into_any_element(),
        ModelMenuRow::Hint(text) => div()
            .id("model-menu-empty")
            .h(MENU_ROW_HEIGHT)
            .flex()
            .flex_row()
            .items_center()
            .px_2()
            .child(div().text_xs().opacity(0.5).child(*text))
            .into_any_element(),
        ModelMenuRow::Provider { id, name, selected } => {
            let provider_id = id.clone();
            let weak = weak.clone();
            menu_row(
                format!("provider-{id}"),
                name.clone(),
                *selected,
                move |_, _, cx| {
                    let _ = weak.update(cx, |workspace, cx| {
                        workspace.on_select_provider(&provider_id, cx);
                    });
                },
                theme,
            )
            .into_any_element()
        }
        ModelMenuRow::Model { id, selected } => {
            let model_id = id.clone();
            let weak = weak.clone();
            menu_row(
                format!("model-{id}"),
                id.clone(),
                *selected,
                move |_, _, cx| {
                    let _ = weak.update(cx, |workspace, cx| {
                        workspace.on_select_model(&model_id, cx);
                    });
                },
                theme,
            )
            .into_any_element()
        }
        ModelMenuRow::Reasoning {
            level,
            label,
            selected,
        } => {
            let level = *level;
            let weak = weak.clone();
            menu_row(
                format!("reasoning-{level}"),
                label.clone(),
                *selected,
                move |_, _, cx| {
                    let _ = weak.update(cx, |workspace, cx| {
                        workspace.on_select_reasoning(level, cx);
                    });
                },
                theme,
            )
            .into_any_element()
        }
    }
}

fn menu_row(
    id: String,
    label: String,
    selected: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .id(id)
        .h(MENU_ROW_HEIGHT)
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
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

/// The `@` file and `/` command autocomplete popover above the composer.
pub(super) fn render_mention_layer(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let mention = workspace
        .vm()
        .mention
        .clone()
        .expect("caller checks the menu is open");
    let heading = match mention.kind {
        crate::view_model::MentionKind::File => "FILES",
        crate::view_model::MentionKind::Command => "COMMANDS",
    };
    div()
        .id("mention-layer")
        .absolute()
        .inset_0()
        .child(
            div()
                .id("mention-menu")
                .absolute()
                .bottom(px(120.))
                .left(px(16.))
                .w(px(420.))
                .max_h(px(300.))
                .overflow_y_scroll()
                .rounded(px(14.))
                .border_1()
                .border_color(skin::glass_border(theme))
                .bg(skin::popover(theme))
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
                        .child(heading),
                )
                .children(mention.items.into_iter().map(|(insert, display)| {
                    let row_id = format!("mention-{insert}");
                    menu_row(
                        row_id,
                        display,
                        false,
                        cx.listener(move |workspace, _, window, cx| {
                            workspace.on_accept_mention(insert.clone(), window, cx);
                        }),
                        cx.theme(),
                    )
                })),
        )
        .into_any_element()
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
