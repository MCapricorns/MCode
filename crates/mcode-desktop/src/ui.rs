//! GPUI rendering for the workspace: activity bar, sessions sidebar, chat
//! with a model picker and project chip, a welcome view, and full-page
//! settings driven by the provider catalog.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::TitleBar;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea};
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, ClickEvent, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px, rems,
};

use crate::view_model::{
    ContextTab, ConversationEntry, DesktopAction, EntryKind, MainView, SessionSummary, UpdateState,
};
use crate::workspace::Workspace;

/// Renders the whole window chrome and content.
pub fn render_root(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id("workspace")
        .relative()
        .flex()
        .flex_col()
        .size_full()
        .bg(theme.background)
        .text_color(theme.foreground)
        .child(render_title_bar(workspace, cx))
        .when(workspace.vm().error.is_some(), |this| {
            this.child(render_error_banner(workspace, cx))
        })
        .when(workspace.vm().pending_ask.is_some(), |this| {
            this.child(render_ask_panel(workspace, window, cx))
        })
        .child(
            div()
                .id("columns")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_row()
                .child(render_activity_bar(workspace, cx))
                .when(workspace.vm().view == MainView::Chat, |this| {
                    this.child(render_sessions_sidebar(workspace, cx))
                })
                .child(match workspace.vm().view {
                    MainView::Chat => render_chat(workspace, window, cx).into_any_element(),
                    MainView::Settings => {
                        render_settings_view(workspace, window, cx).into_any_element()
                    }
                })
                .when(workspace.vm().view == MainView::Chat, |this| {
                    this.child(render_context_panel(workspace, window, cx))
                }),
        )
        .when(workspace.vm().model_menu_open, |this| {
            this.child(render_model_menu_layer(workspace, cx))
        })
}

fn render_title_bar(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let subtitle: SharedString = match workspace.vm().view {
        MainView::Chat => workspace
            .vm()
            .project_dir
            .as_deref()
            .map(project_label)
            .unwrap_or_else(|| "MCode".to_owned())
            .into(),
        MainView::Settings => "Settings".into(),
    };
    let update_label: Option<SharedString> = match &workspace.vm().update {
        UpdateState::Available { version, .. } => Some(format!("Update to v{version}").into()),
        _ => None,
    };
    TitleBar::new()
        .child(
            div()
                .id("title-row")
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .w_full()
                .px_3()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::BOLD)
                                .child("MCode"),
                        )
                        .child(div().text_xs().opacity(0.6).child(subtitle)),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .when_some(update_label, |this, label| {
                            this.child(
                                Button::new("title-update")
                                    .label(label)
                                    .small()
                                    .warning()
                                    .on_click(cx.listener(|workspace, _, _, cx| {
                                        workspace.on_show_main_view(MainView::Settings, cx);
                                    })),
                            )
                        })
                        .child(
                            div()
                                .text_xs()
                                .opacity(0.4)
                                .child(format!("v{}", mcode_updates::current_version())),
                        ),
                ),
        )
        .border_b_1()
        .border_color(theme.border)
}

fn render_error_banner(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let message = workspace.vm().error.clone().unwrap_or_default();
    div()
        .id("error-banner")
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px_3()
        .py_1()
        .bg(theme.danger)
        .text_color(theme.danger_foreground)
        .text_xs()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(Icon::new(IconName::CircleAlert).with_size(px(14.)))
                .child(message),
        )
        .child(
            Button::new("dismiss-error")
                .icon(IconName::X)
                .small()
                .ghost()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_dismiss_error(cx);
                })),
        )
}

/// Renders the pending ask panel: one answer row per question.
fn render_ask_panel(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let rows: Vec<(String, Vec<String>, bool)> =
        workspace.vm().pending_ask.clone().unwrap_or_default();
    div()
        .id("ask-panel")
        .border_b_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(Icon::new(IconName::CircleAlert).with_size(px(16.)))
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
                            let buttons: Vec<(usize, String, String)> = choices
                                .iter()
                                .enumerate()
                                .map(|(choice_index, choice)| {
                                    (choice_index, choice.clone(), choice.clone())
                                })
                                .collect();
                            this.child(
                                div()
                                    .id(format!("ask-choices-{index}"))
                                    .flex()
                                    .flex_row()
                                    .flex_wrap()
                                    .gap_1()
                                    .children(buttons.into_iter().map(
                                        |(choice_index, label, answer)| {
                                            Button::new(format!(
                                                "ask-{index}-{choice_index}-{label}"
                                            ))
                                            .label(label)
                                            .small()
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
                        .child(Input::new(&workspace.ask_input(window, cx))),
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
        .into_any_element()
}

/// Narrow icon rail on the far left: views on top, controls at the bottom.
fn render_activity_bar(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let view = workspace.vm().view;
    div()
        .id("activity-bar")
        .w(px(46.))
        .h_full()
        .flex()
        .flex_col()
        .items_center()
        .py_2()
        .gap_1()
        .bg(theme.sidebar)
        .border_r_1()
        .border_color(theme.sidebar_border)
        .child(
            div()
                .id("activity-logo")
                .size(px(28.))
                .rounded_md()
                .bg(theme.primary)
                .text_color(theme.primary_foreground)
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::BOLD)
                .child("M"),
        )
        .child(rail_button(
            "rail-chat",
            IconName::MessageSquare,
            view == MainView::Chat,
            cx.listener(|workspace, _, _, cx| {
                workspace.on_show_main_view(MainView::Chat, cx);
            }),
            cx,
        ))
        .child(rail_button(
            "rail-settings",
            IconName::Settings,
            view == MainView::Settings,
            cx.listener(|workspace, _, _, cx| {
                workspace.on_show_main_view(MainView::Settings, cx);
            }),
            cx,
        ))
        .child(div().id("activity-spacer").flex_1().min_h_0())
        .child(rail_button(
            "rail-theme",
            if workspace.vm().dark_theme {
                IconName::Sun
            } else {
                IconName::Moon
            },
            false,
            cx.listener(|workspace, _, window, cx| {
                workspace.on_toggle_theme(window, cx);
            }),
            cx,
        ))
}

fn rail_button(
    id: &'static str,
    icon: IconName,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id(id)
        .size(px(32.))
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(active, |this| this.bg(theme.sidebar_accent))
        .hover(|this| this.bg(theme.sidebar_accent))
        .text_color(if active {
            theme.sidebar_accent_foreground
        } else {
            theme.muted_foreground
        })
        .child(Icon::new(icon).with_size(px(17.)))
        .on_click(on_click)
}

fn render_sessions_sidebar(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let sessions: Vec<SessionSummary> = workspace.vm().sessions.clone();
    div()
        .id("sidebar")
        .w(px(232.))
        .h_full()
        .flex()
        .flex_col()
        .bg(theme.sidebar)
        .border_r_1()
        .border_color(theme.sidebar_border)
        .child(
            div()
                .id("sidebar-header")
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px_3()
                .py_2()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(theme.sidebar_foreground)
                        .child("Chats"),
                )
                .child(
                    Button::new("new-session")
                        .icon(IconName::Plus)
                        .small()
                        .primary()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_new_session(cx);
                        })),
                ),
        )
        .child(
            div()
                .id("session-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .px_2()
                .pb_2()
                .gap(px(2.))
                .when(sessions.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .opacity(0.5)
                            .px_2()
                            .py_1()
                            .child("No chats yet"),
                    )
                })
                .children(sessions.into_iter().map(|summary| {
                    let active =
                        workspace.vm().active.as_ref().is_some_and(|conversation| {
                            conversation.session_id == summary.session_id
                        });
                    div()
                        .id(format!("session-row-{}", summary.session_id))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py(px(6.))
                        .rounded_md()
                        .cursor_pointer()
                        .when(active, |this| this.bg(theme.sidebar_accent))
                        .hover(|this| this.bg(theme.sidebar_accent))
                        .text_color(if active {
                            theme.sidebar_accent_foreground
                        } else {
                            theme.sidebar_foreground
                        })
                        .on_click({
                            let session_id = summary.session_id.clone();
                            cx.listener(move |workspace, _, _, cx| {
                                workspace.on_open_session(&session_id, cx);
                            })
                        })
                        .child(
                            Icon::new(IconName::MessageSquare)
                                .with_size(px(14.))
                                .text_color(theme.muted_foreground),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .overflow_hidden()
                                .child(short_id(&summary.session_id)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .opacity(0.5)
                                .child(summary.event_count.to_string()),
                        )
                })),
        )
}

fn render_chat(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let composer = workspace.composer().clone();
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
        .and_then(|c| c.streaming.as_ref())
        .cloned();
    div()
        .id("chat")
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(render_chat_top_bar(workspace, window, cx))
        .child(
            div()
                .id("conversation")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .when(entries.is_empty() && streaming.is_none(), |this| {
                    this.child(render_welcome(workspace, cx))
                })
                .children(
                    entries
                        .into_iter()
                        .map(|entry| render_entry(entry, cx.theme()).into_any_element()),
                )
                .when_some(streaming, |this, streaming| {
                    this.child(
                        div()
                            .id("streaming-entry")
                            .flex()
                            .flex_col()
                            .gap_1()
                            .when(!streaming.thinking.is_empty(), |this| {
                                this.child(
                                    div()
                                        .id("streaming-thinking")
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_1()
                                        .text_xs()
                                        .opacity(0.6)
                                        .child(format!("thinking… {}", streaming.thinking)),
                                )
                            })
                            .when(!streaming.text.is_empty(), |this| {
                                let theme = cx.theme();
                                this.child(
                                    div()
                                        .id("streaming-text")
                                        .self_start()
                                        .max_w(rems(42.))
                                        .px_3()
                                        .py_2()
                                        .rounded_lg()
                                        .rounded_tl(px(4.))
                                        .text_sm()
                                        .bg(theme.secondary)
                                        .child(streaming.text),
                                )
                            }),
                    )
                }),
        )
        .child(
            div()
                .id("composer")
                .border_t_1()
                .border_color(cx.theme().border)
                .p_3()
                .child(
                    div()
                        .id("composer-card")
                        .flex()
                        .flex_row()
                        .items_end()
                        .gap_2()
                        .p_2()
                        .rounded_xl()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().background)
                        .child(
                            div()
                                .id("composer-input")
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .child(Textarea::new(&composer)),
                        )
                        .child(
                            Button::new("send")
                                .icon(IconName::SendHorizontal)
                                .primary()
                                .rounded(px(20.))
                                .disabled(workspace.vm().sending || workspace.vm().active.is_none())
                                .on_click(cx.listener(|workspace, _, window, cx| {
                                    workspace.on_send(window, cx);
                                })),
                        ),
                ),
        )
        .into_any_element()
}

/// Chat top bar: project chip plus the model picker.
fn render_chat_top_bar(
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let project_label: SharedString = workspace
        .vm()
        .project_dir
        .as_deref()
        .map(project_label)
        .unwrap_or_else(|| "Open project".to_owned())
        .into();
    let model_label: SharedString = model_picker_label(workspace.vm()).into();
    div()
        .id("chat-top-bar")
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(theme.border)
        .child(
            Button::new("project-chip")
                .icon(IconName::FolderOpen)
                .label(project_label)
                .small()
                .ghost()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_open_project_dialog(cx);
                })),
        )
        .child(div().flex_1().min_w_0())
        .child(
            Button::new("model-chip")
                .icon(IconName::Bot)
                .label(model_label)
                .small()
                .ghost()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    let open = !workspace.vm().model_menu_open;
                    workspace.on_toggle_model_menu(open, cx);
                })),
        )
}

/// The model picker dropdown, pinned under the top bar: a full-window click
/// catcher plus the provider and model lists.
fn render_model_menu_layer(
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
    let models: Vec<(String, String)> = selected_provider
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
                .map(|provider| {
                    provider
                        .models
                        .iter()
                        .take(64)
                        .map(|model| (model.clone(), model.clone()))
                        .collect::<Vec<_>>()
                })
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
                .top(px(76.))
                .right(px(12.))
                .w(px(320.))
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
                            .child("No enabled providers — add one in Settings"),
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
                    .children(models.into_iter().map(|(id, name)| {
                        let selected = Some(&id) == selected_model.as_ref();
                        menu_row(
                            format!("model-{id}"),
                            name,
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
            this.child(Icon::new(IconName::Check).with_size(px(14.)))
        })
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
        Some(model) => format!("{provider} · {model}"),
        None => provider,
    }
}

/// The model picker dropdown: provider rows plus the selected provider's
/// models.
/// The welcome hero shown when no conversation has started.
fn render_welcome(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let recents = workspace.vm().recents.clone();
    div()
        .id("welcome")
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .child(
            div()
                .id("welcome-logo")
                .size(px(52.))
                .rounded_xl()
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
                    .w(px(420.))
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
                            .id(format!("recent-{}", short_id(&project)))
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
                                    .with_size(px(14.))
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

fn render_entry(entry: ConversationEntry, theme: &Theme) -> impl IntoElement {
    match entry.kind {
        EntryKind::UserMessage => div()
            .id(format!("entry-{}", entry.event_id))
            .flex()
            .flex_col()
            .items_end()
            .child(
                div()
                    .max_w(rems(42.))
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
            .child(
                div()
                    .max_w(rems(42.))
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .rounded_tl(px(4.))
                    .text_sm()
                    .bg(theme.secondary)
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
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::Wrench).with_size(px(12.)))
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
                        .when(failed, |this| this.text_color(theme.danger))
                        .when(!failed, |this| this.opacity(0.85))
                        .child(ellipsis(&entry.text, 600)),
                )
        }
        EntryKind::Usage => div()
            .id(format!("entry-{}", entry.event_id))
            .flex()
            .flex_col()
            .items_end()
            .child(
                div()
                    .text_xs()
                    .opacity(0.5)
                    .child(ellipsis(&entry.text, 120)),
            ),
    }
}

/// Right context panel: Overview / Web / Changes.
fn render_context_panel(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let tab = workspace.vm().context_tab;
    div()
        .id("context-panel")
        .w(px(320.))
        .h_full()
        .flex()
        .flex_col()
        .border_l_1()
        .border_color(theme.border)
        .bg(theme.sidebar)
        .child(
            div()
                .id("context-tabs")
                .flex()
                .flex_row()
                .gap_1()
                .p_2()
                .child(context_tab_button(
                    tab,
                    ContextTab::Overview,
                    "Overview",
                    cx,
                ))
                .child(context_tab_button(tab, ContextTab::Web, "Web", cx))
                .child(context_tab_button(tab, ContextTab::Changes, "Changes", cx)),
        )
        .child(
            div()
                .id("context-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_3()
                .pb_3()
                .child(match tab {
                    ContextTab::Overview => render_overview(workspace, cx).into_any_element(),
                    ContextTab::Web => render_web(workspace, window, cx).into_any_element(),
                    ContextTab::Changes => render_changes(workspace, cx).into_any_element(),
                }),
        )
}

fn context_tab_button(
    current: ContextTab,
    tab: ContextTab,
    label: &'static str,
    cx: &mut Context<Workspace>,
) -> Button {
    let selected = current == tab;
    Button::new(format!("context-tab-{label}"))
        .label(label)
        .small()
        .when(selected, |this| this.primary())
        .when(!selected, |this| this.ghost())
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.on_show_tab(tab, cx);
        }))
}

/// Renders the bounded web search panel.
fn render_web(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let query_input = workspace.web_query_input(window, cx);
    let theme = cx.theme();
    let results: Vec<(String, String, String)> = workspace
        .vm()
        .web_results
        .iter()
        .map(|result| {
            (
                result.url.clone(),
                result.title.clone(),
                result.snippet.clone(),
            )
        })
        .collect();
    div()
        .id("web-panel")
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .id("web-query-row")
                .flex()
                .flex_row()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h(px(30.))
                        .child(Input::new(&query_input)),
                )
                .child(
                    Button::new("web-search-run")
                        .icon(IconName::Search)
                        .small()
                        .primary()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_web_search_run(cx);
                        })),
                ),
        )
        .when(results.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .opacity(0.5)
                    .child("No results yet — enable a backend in Settings"),
            )
        })
        .children(results.into_iter().map(|(url, title, snippet)| {
            div()
                .id(format!("web-result-{}", short_id(&url)))
                .flex()
                .flex_col()
                .gap_0p5()
                .p_2()
                .rounded_md()
                .bg(theme.background)
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(title),
                )
                .child(div().text_xs().opacity(0.7).child(ellipsis(&snippet, 200)))
                .child(div().text_xs().opacity(0.5).child(ellipsis(&url, 80)))
        }))
        .into_any_element()
}

/// Renders the changed-files panel fed by tool activity details.
fn render_changes(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let changed: Vec<String> = workspace
        .vm()
        .active
        .as_ref()
        .map(|conversation| {
            conversation
                .entries
                .iter()
                .filter(|entry| matches!(entry.kind, EntryKind::ToolCall | EntryKind::ToolResult))
                .map(|entry| entry.text.clone())
                .collect()
        })
        .unwrap_or_default();
    div()
        .id("changes-panel")
        .flex()
        .flex_col()
        .gap_2()
        .child(
            Button::new("changes-rollback")
                .icon(IconName::RotateCcw)
                .label("Rollback")
                .small()
                .ghost()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_rollback(cx);
                })),
        )
        .when(changed.is_empty(), |this| {
            this.child(div().text_xs().opacity(0.5).child(
                "File edits from tool runs appear here; Rollback restores every snapshotted file.",
            ))
        })
        .children(changed.into_iter().map(|text| {
            div()
                .id(format!("change-item-{}", short_id(&text)))
                .p_2()
                .rounded_md()
                .text_xs()
                .bg(theme.background)
                .child(ellipsis(&text, 240))
        }))
        .into_any_element()
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

// ---- settings view ----

fn render_settings_view(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let Some(settings) = workspace.vm().settings.clone() else {
        return div()
            .id("settings-loading")
            .text_sm()
            .opacity(0.6)
            .p_6()
            .child("Loading settings…")
            .into_any_element();
    };
    let ua_input = workspace.settings_ua_input(window, cx);
    let provider_form_element = render_provider_form(workspace, window, cx);
    let backend_form_element = render_backend_form(workspace, window, cx);
    let mcp_form_element = render_mcp_form(workspace, window, cx);
    let preset_search = workspace.preset_search_input(window, cx);
    div()
        .id("settings-view")
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(cx.theme().background)
        .child(
            div()
                .id("settings-header")
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px_6()
                .py_3()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui_kit::FontWeight::BOLD)
                        .child("Settings"),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .opacity(0.5)
                                .child(format!("revision {}", settings.revision)),
                        )
                        .child(
                            Button::new("settings-save")
                                .icon(IconName::Check)
                                .label("Save changes")
                                .small()
                                .primary()
                                .disabled(!settings.dirty || settings.saving)
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_save_settings(cx);
                                })),
                        ),
                ),
        )
        .child(
            div()
                .id("settings-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_6()
                .py_4()
                .max_w(rems(56.))
                .flex()
                .flex_col()
                .gap_4()
                .child(render_appearance_section(workspace, ua_input, cx))
                .child(render_providers_section(
                    workspace,
                    settings.clone(),
                    preset_search,
                    provider_form_element,
                    window,
                    cx,
                ))
                .child(render_web_section(
                    workspace,
                    settings.clone(),
                    backend_form_element,
                    cx,
                ))
                .child(render_mcp_section(
                    workspace,
                    settings.clone(),
                    mcp_form_element,
                    window,
                    cx,
                ))
                .child(render_usage_section(workspace, settings.clone(), cx))
                .child(render_updates_section(workspace, cx)),
        )
        .into_any_element()
}

fn section_header(title: &str, hint: Option<&str>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(title.to_owned()),
        )
        .when_some(hint, |this, hint| {
            this.child(div().text_xs().opacity(0.5).child(hint.to_owned()))
        })
}

fn render_appearance_section(
    workspace: &mut Workspace,
    ua_input: Entity<InputState>,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let dark = workspace.vm().dark_theme;
    let effective_ua = workspace
        .vm()
        .settings
        .as_ref()
        .map(|settings| settings.effective_user_agent.clone())
        .unwrap_or_default();
    div()
        .id("settings-appearance")
        .flex()
        .flex_col()
        .gap_2()
        .child(section_header(
            "Appearance",
            Some("Theme and request identity"),
        ))
        .child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .child(
                    Button::new("theme-light")
                        .icon(IconName::Sun)
                        .label("Light")
                        .small()
                        .when(!dark, |this| this.primary())
                        .when(dark, |this| this.ghost())
                        .on_click(cx.listener(|workspace, _, window, cx| {
                            if workspace.vm().dark_theme {
                                workspace.on_toggle_theme(window, cx);
                            }
                        })),
                )
                .child(
                    Button::new("theme-dark")
                        .icon(IconName::Moon)
                        .label("Dark")
                        .small()
                        .when(dark, |this| this.primary())
                        .when(!dark, |this| this.ghost())
                        .on_click(cx.listener(|workspace, _, window, cx| {
                            if !workspace.vm().dark_theme {
                                workspace.on_toggle_theme(window, cx);
                            }
                        })),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().opacity(0.7).child("HTTP User-Agent"))
                .child(div().h(px(30.)).text_sm().child(Input::new(&ua_input)))
                .child(
                    div()
                        .text_xs()
                        .opacity(0.5)
                        .child(format!("Effective: {effective_ua}")),
                ),
        )
        .child(
            div()
                .p_2()
                .rounded_md()
                .bg(theme.secondary)
                .text_xs()
                .opacity(0.7)
                .child("Leave the User-Agent empty to reuse the pi agent identity."),
        )
        .into_any_element()
}

fn render_providers_section(
    workspace: &mut Workspace,
    settings: crate::view_model::SettingsState,
    preset_search: Entity<InputState>,
    provider_form_element: gpui_kit::AnyElement,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let catalog = workspace.vm().catalog.clone();
    let active_preset = workspace.vm().active_preset.clone();
    let preset_search_text = workspace.vm().preset_search.to_lowercase();
    let mut provider_rows: Vec<(usize, String, String, String, usize, bool)> = Vec::new();
    for (index, provider) in settings.providers.iter().enumerate() {
        let name = catalog
            .as_ref()
            .map(|catalog| catalog.display_name(&provider.id))
            .unwrap_or_else(|| provider.id.clone());
        let host = provider
            .base_url
            .strip_prefix("https://")
            .unwrap_or(&provider.base_url)
            .split('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        let keyed = settings
            .providers_with_keys
            .iter()
            .any(|id| id == &provider.id);
        provider_rows.push((
            index,
            name,
            provider.kind.clone(),
            host,
            provider.models.len(),
            keyed,
        ));
    }
    let mut preset_rows: Vec<(String, String, String, usize)> = Vec::new();
    if let Some(catalog) = catalog.as_ref() {
        for provider in &catalog.providers {
            if !preset_search_text.is_empty()
                && !provider.name.to_lowercase().contains(&preset_search_text)
                && !provider.id.contains(&preset_search_text)
            {
                continue;
            }
            if preset_rows.len() >= 60 {
                break;
            }
            preset_rows.push((
                provider.id.clone(),
                provider.name.clone(),
                provider.kind.clone(),
                provider.models.len(),
            ));
        }
    }
    div()
        .id("settings-providers")
        .flex()
        .flex_col()
        .gap_2()
        .child(section_header(
            "Model providers",
            Some("Pick a provider, paste its API key, done. The catalog updates automatically from models.dev."),
        ))
        .when(provider_rows.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .opacity(0.5)
                    .child("No providers yet — add one from the catalog below"),
            )
        })
        .children(provider_rows.into_iter().map(
            |(index, name, kind, host, models, keyed)| {
                div()
                    .id(format!("provider-row-{index}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .overflow_hidden()
                                    .child(name),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .opacity(0.6)
                                    .child(format!("{kind} · {host} · {models} model(s)")),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .child(
                                Icon::new(IconName::KeyRound)
                                    .with_size(px(14.))
                                    .text_color(if keyed {
                                        theme.success
                                    } else {
                                        theme.muted_foreground
                                    }),
                            )
                            .child(
                                Button::new(format!("provider-toggle-{index}"))
                                    .label(if workspace.vm().settings.as_ref().and_then(|s| s.providers.get(index)).map(|p| p.enabled).unwrap_or(false) { "On" } else { "Off" })
                                    .small()
                                    .ghost()
                                    .on_click(cx.listener(move |workspace, _, _, cx| {
                                        if let Some(provider) = workspace
                                            .vm()
                                            .settings
                                            .as_ref()
                                            .and_then(|s| s.providers.get(index))
                                            .cloned()
                                        {
                                            let updated = mcode_config::ProviderSettings {
                                                enabled: !provider.enabled,
                                                ..provider
                                            };
                                            workspace.apply_action(
                                                DesktopAction::SettingsProviderChanged(
                                                    index, updated,
                                                ),
                                                cx,
                                            );
                                        }
                                    })),
                            )
                            .child(
                                Button::new(format!("provider-remove-{index}"))
                                    .icon(IconName::Trash)
                                    .small()
                                    .ghost()
                                    .on_click(cx.listener(move |workspace, _, _, cx| {
                                        workspace.on_remove_provider(index, cx);
                                    })),
                            ),
                    )
            },
        ))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .rounded_md()
                .bg(theme.secondary)
                .child(div().text_xs().opacity(0.7).child("Add from catalog"))
                .child(
                    div()
                        .h(px(30.))
                        .text_sm()
                        .child(Input::new(&preset_search)),
                )
                .child(
                    div()
                        .id("preset-catalog-list")
                        .max_h(px(220.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(preset_rows.into_iter().map(
                            |(id, name, kind, models)| {
                                div()
                                    .id(format!("preset-row-{id}"))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .px_2()
                                    .py(px(5.))
                                    .rounded_md()
                                    .when(
                                        active_preset.as_deref() == Some(id.as_str()),
                                        |this| this.bg(theme.background),
                                    )
                                    .hover(|this| this.bg(theme.background))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .overflow_hidden()
                                                    .child(name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .opacity(0.6)
                                                    .child(format!("{id} · {kind} · {models} models")),
                                            ),
                                    )
                                    .child(
                                        Button::new(format!("preset-add-{id}"))
                                            .icon(IconName::Plus)
                                            .label("Add")
                                            .small()
                                            .on_click(cx.listener(move |workspace, _, _, cx| {
                                                workspace.on_open_preset(&id, cx);
                                            })),
                                    )
                            },
                        )),
                ),
        )
        .when_some(active_preset, |this, preset_id| {
            this.child(render_preset_form(workspace, &preset_id, window, cx))
        })
        .child(provider_form_element)
        .into_any_element()
}

/// The expanded preset form: model picker plus the API key input.
fn render_preset_form(
    workspace: &mut Workspace,
    provider_id: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let key_input = workspace.preset_key_input(window, cx);
    let theme = cx.theme();
    let Some(catalog) = workspace.vm().catalog.clone() else {
        return div().into_any_element();
    };
    let Some(preset) = catalog.provider(provider_id) else {
        return div().into_any_element();
    };
    let name = preset.name.clone();
    let models: Vec<String> = preset
        .models
        .iter()
        .take(64)
        .map(|model| model.id.clone())
        .collect();
    let chosen = workspace
        .vm()
        .preset_model
        .clone()
        .unwrap_or_else(|| models.first().cloned().unwrap_or_default());
    let menu_open = workspace.vm().preset_model_menu_open;
    let provider_id_owned = provider_id.to_owned();
    div()
        .id("preset-form")
        .flex()
        .flex_col()
        .gap_2()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(format!("Add {name}")),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().opacity(0.6).child("Default model"))
                .child(
                    Button::new("preset-model-chip")
                        .label(chosen.clone())
                        .small()
                        .ghost()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            let open = !workspace.vm().preset_model_menu_open;
                            workspace.apply_action(DesktopAction::PresetModelMenuToggled(open), cx);
                        })),
                )
                .when(menu_open, |this| {
                    this.child(
                        div()
                            .id("preset-model-list")
                            .max_h(px(180.))
                            .overflow_y_scroll()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .p_1()
                            .flex()
                            .flex_col()
                            .gap_px()
                            .children(models.into_iter().map(|model| {
                                let model_for_click = model.clone();
                                div()
                                    .id(format!("preset-model-{model}"))
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .text_sm()
                                    .cursor_pointer()
                                    .hover(|this| this.bg(theme.secondary))
                                    .on_click(cx.listener(move |workspace, _, _, cx| {
                                        workspace.apply_action(
                                            DesktopAction::PresetModelChanged(
                                                model_for_click.clone(),
                                            ),
                                            cx,
                                        );
                                        workspace.apply_action(
                                            DesktopAction::PresetModelMenuToggled(false),
                                            cx,
                                        );
                                    }))
                                    .child(model)
                            })),
                    )
                }),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .opacity(0.6)
                        .child("API key (stored in the secret vault)"),
                )
                .child(div().h(px(28.)).text_sm().child(Input::new(&key_input))),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(
                    Button::new("preset-confirm")
                        .icon(IconName::Check)
                        .label("Add provider")
                        .small()
                        .primary()
                        .on_click(cx.listener(move |workspace, _, _, cx| {
                            let provider_id = provider_id_owned.clone();
                            workspace.on_add_preset(&provider_id, cx);
                        })),
                )
                .child(
                    Button::new("preset-cancel")
                        .label("Cancel")
                        .small()
                        .ghost()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_close_preset(cx);
                        })),
                ),
        )
        .into_any_element()
}

fn render_web_section(
    _workspace: &mut Workspace,
    settings: crate::view_model::SettingsState,
    backend_form_element: gpui_kit::AnyElement,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let backend_rows: Vec<(String, String, bool)> = settings
        .web_backends
        .iter()
        .map(|backend| (backend.id.clone(), backend.kind.clone(), backend.enabled))
        .collect();
    div()
        .id("settings-web")
        .flex()
        .flex_col()
        .gap_2()
        .child(section_header(
            "Web search",
            Some("One enabled backend powers the Web panel."),
        ))
        .when(backend_rows.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .opacity(0.5)
                    .child("No backends yet — add one below"),
            )
        })
        .children(
            backend_rows
                .iter()
                .enumerate()
                .map(|(index, (id, kind, enabled))| {
                    div()
                        .id(format!("backend-row-{id}"))
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .p_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .child(div().text_sm().child(format!(
                            "{id} · {kind} · {}",
                            if *enabled { "enabled" } else { "disabled" }
                        )))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_1()
                                .child(
                                    Button::new(format!("backend-toggle-{id}"))
                                        .label(if *enabled { "Disable" } else { "Enable" })
                                        .small()
                                        .ghost()
                                        .on_click(cx.listener(move |workspace, _, _, cx| {
                                            workspace.on_toggle_backend(index, cx);
                                        })),
                                )
                                .child(
                                    Button::new(format!("backend-remove-{id}"))
                                        .icon(IconName::Trash)
                                        .small()
                                        .ghost()
                                        .on_click(cx.listener(move |workspace, _, _, cx| {
                                            workspace.on_remove_backend(index, cx);
                                        })),
                                ),
                        )
                }),
        )
        .child(backend_form_element)
        .into_any_element()
}

fn render_mcp_section(
    workspace: &mut Workspace,
    settings: crate::view_model::SettingsState,
    mcp_form_element: gpui_kit::AnyElement,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let mcp_rows: Vec<(String, String, bool, bool)> = settings
        .mcp_servers
        .iter()
        .map(|server| {
            (
                server.id.clone(),
                server.transport.clone(),
                server.enabled,
                settings.mcp_with_keys.iter().any(|id| id == &server.id),
            )
        })
        .collect();
    let builtin_servers = mcode_config::builtin_mcp_servers();
    let catalog_rows: Vec<(String, String)> = builtin_servers
        .iter()
        .filter(|server| {
            !settings
                .mcp_servers
                .iter()
                .any(|configured| configured.id == server.id)
        })
        .map(|server| (server.id.clone(), server.transport.clone()))
        .collect();
    div()
        .id("settings-mcp")
        .flex()
        .flex_col()
        .gap_2()
        .child(section_header(
            "MCP servers",
            Some("Stdio or Streamable-HTTP tool servers."),
        ))
        .when(mcp_rows.is_empty(), |this| {
            this.child(div().text_xs().opacity(0.5).child("No MCP servers yet"))
        })
        .children(
            mcp_rows
                .iter()
                .enumerate()
                .map(|(index, (id, transport, enabled, keyed))| {
                    div()
                        .id(format!("mcp-row-{id}"))
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .p_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .child(div().text_sm().child(format!(
                            "{id} \u{b7} {transport} \u{b7} {} \u{b7} key {}",
                            if *enabled { "enabled" } else { "disabled" },
                            if *keyed { "\u{2713}" } else { "-" }
                        )))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_1()
                                .child(
                                    Button::new(format!("mcp-toggle-{id}"))
                                        .label(if *enabled { "Disable" } else { "Enable" })
                                        .small()
                                        .ghost()
                                        .on_click(cx.listener(move |workspace, _, _, cx| {
                                            let on = !workspace
                                                .vm()
                                                .settings
                                                .as_ref()
                                                .and_then(|settings| {
                                                    settings.mcp_servers.get(index)
                                                })
                                                .map(|server| server.enabled)
                                                .unwrap_or(false);
                                            workspace.apply_action(
                                                DesktopAction::SettingsMcpToggled(index, on),
                                                cx,
                                            );
                                        })),
                                )
                                .child(
                                    Button::new(format!("mcp-tools-{id}"))
                                        .label("List tools")
                                        .small()
                                        .ghost()
                                        .on_click({
                                            let id = id.clone();
                                            cx.listener(move |workspace, _, _, cx| {
                                                workspace.on_list_mcp_tools(&id, cx);
                                            })
                                        }),
                                )
                                .child(
                                    Button::new(format!("mcp-remove-{id}"))
                                        .icon(IconName::Trash)
                                        .small()
                                        .ghost()
                                        .on_click(cx.listener(move |workspace, _, _, cx| {
                                            workspace.apply_action(
                                                DesktopAction::SettingsMcpRemoved(index),
                                                cx,
                                            );
                                        })),
                                ),
                        )
                }),
        )
        .when(!catalog_rows.is_empty(), |this| {
            this.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded_md()
                    .bg(theme.secondary)
                    .child(
                        div()
                            .text_xs()
                            .opacity(0.7)
                            .child("Built-in catalog — paste a key, then Add"),
                    )
                    .children(catalog_rows.iter().map(|(id, transport)| {
                        div()
                            .id(format!("mcp-catalog-{id}"))
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .opacity(0.8)
                                    .child(format!("{id} \u{b7} {transport}")),
                            )
                            .child(
                                Button::new(format!("mcp-add-{id}"))
                                    .label("Add")
                                    .small()
                                    .primary()
                                    .on_click({
                                        let id = id.clone();
                                        cx.listener(move |workspace, _, window, cx| {
                                            let server = mcode_config::builtin_mcp_servers()
                                                .into_iter()
                                                .find(|server| server.id == id)
                                                .expect("catalog entry");
                                            let key = workspace
                                                .mcp_key_input(window, cx)
                                                .read(cx)
                                                .value()
                                                .trim()
                                                .to_owned();
                                            workspace.on_add_builtin_mcp(server, &key, cx);
                                        })
                                    }),
                            )
                    })),
            )
        })
        .child(mcp_key_row(workspace, window, cx))
        .child(mcp_form_element)
        .into_any_element()
}

fn mcp_key_row(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let key_input = workspace.mcp_key_input(window, cx);
    div()
        .id("mcp-key-row")
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .opacity(0.6)
                .child("Key for the built-in catalog server above"),
        )
        .child(div().h(px(28.)).text_sm().child(Input::new(&key_input)))
        .into_any_element()
}

fn render_usage_section(
    _workspace: &mut Workspace,
    settings: crate::view_model::SettingsState,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    div()
        .id("settings-usage")
        .flex()
        .flex_col()
        .gap_2()
        .child(section_header("Usage", None))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_2()
                .child(div().text_sm().child("Durable usage records").opacity(0.8))
                .child(
                    Button::new("settings-usage-toggle")
                        .label(if settings.usage_enabled {
                            "Turn off"
                        } else {
                            "Turn on"
                        })
                        .small()
                        .ghost()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            let enabled = !workspace
                                .vm()
                                .settings
                                .as_ref()
                                .map(|settings| settings.usage_enabled)
                                .unwrap_or(true);
                            workspace
                                .apply_action(DesktopAction::SettingsUsageToggled(enabled), cx);
                        })),
                ),
        )
        .into_any_element()
}

fn render_updates_section(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let vm = workspace.vm();
    let current = mcode_updates::current_version().to_owned();
    let catalog_line = match vm.catalog_fetched_at {
        0 => "bundled snapshot".to_owned(),
        seconds => {
            let date = catalog_date(seconds);
            format!("cloud catalog · fetched {date}")
        }
    };
    let status: (String, Option<gpui_kit::AnyElement>) = match &vm.update {
        UpdateState::Idle => ("Update checks run at startup.".to_owned(), None),
        UpdateState::Checking => ("Checking for updates…".to_owned(), None),
        UpdateState::UpToDate => (
            format!("v{current} is the latest version."),
            Some(
                div()
                    .text_xs()
                    .text_color(theme.success)
                    .child("up to date")
                    .into_any_element(),
            ),
        ),
        UpdateState::Available { version, .. } => (
            format!("v{version} is available."),
            Some(
                Button::new("update-download")
                    .icon(IconName::Download)
                    .label("Download & install")
                    .small()
                    .primary()
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_download_update(cx);
                    }))
                    .into_any_element(),
            ),
        ),
        UpdateState::Downloading { .. } => ("Downloading and verifying…".to_owned(), None),
        UpdateState::Ready { version } => (
            format!("v{version} is staged."),
            Some(
                Button::new("update-restart")
                    .icon(IconName::RefreshCw)
                    .label("Restart to install")
                    .small()
                    .primary()
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_install_update(cx);
                    }))
                    .into_any_element(),
            ),
        ),
        UpdateState::Failed(message) => (
            message.clone(),
            Some(
                div()
                    .text_xs()
                    .text_color(theme.danger)
                    .child(ellipsis(message, 120))
                    .into_any_element(),
            ),
        ),
    };
    div()
        .id("settings-updates")
        .flex()
        .flex_col()
        .gap_2()
        .child(section_header(
            "Updates & catalog",
            Some("The app updates itself from GitHub releases."),
        ))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .child(format!("Current version: v{current}")),
                )
                .child(
                    Button::new("update-check")
                        .icon(IconName::Search)
                        .label("Check now")
                        .small()
                        .ghost()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_check_update(cx);
                        })),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_2()
                .child(div().text_sm().child("Automatic checks").opacity(0.8))
                .child(
                    Button::new("update-auto-toggle")
                        .label(if vm.auto_update { "On" } else { "Off" })
                        .small()
                        .ghost()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            let enabled = !workspace.vm().auto_update;
                            workspace.on_toggle_auto_update(enabled, cx);
                        })),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_2()
                .child(div().text_sm().child("Status").opacity(0.8))
                .child(div().text_xs().opacity(0.7).child(status.0)),
        )
        .when_some(status.1, |this, node| {
            this.child(div().flex().flex_row().justify_end().child(node))
        })
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_2()
                .child(div().text_sm().child("Provider catalog").opacity(0.8))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(div().text_xs().opacity(0.6).child(catalog_line))
                        .child(
                            Button::new("catalog-refresh")
                                .icon(IconName::RefreshCw)
                                .label("Refresh")
                                .small()
                                .ghost()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_refresh_catalog(cx);
                                })),
                        ),
                ),
        )
        .into_any_element()
}

fn catalog_date(seconds: u64) -> String {
    // Bounded ISO-ish date from unix seconds without a chrono dependency.
    let days = seconds / 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---- shared form widgets ----

/// Inline add-backend form state.
pub(crate) struct BackendForm {
    /// Backend identity input.
    pub id: Entity<InputState>,
    /// Backend kind input (`querit` or `custom`).
    pub kind: Entity<InputState>,
    /// HTTPS endpoint input.
    pub endpoint: Entity<InputState>,
}

impl BackendForm {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Entity<Self> {
        let mut make = |placeholder: &'static str| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let id = make("id, e.g. querit-main");
        let kind = make("querit | custom");
        let endpoint = make("https://search.example.com");
        cx.new(|_| Self { id, kind, endpoint })
    }
}

fn render_backend_form(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let form = workspace.backend_form(window, cx);
    div()
        .id("backend-form")
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_md()
        .bg(cx.theme().secondary)
        .child(div().text_xs().opacity(0.7).child("Add web search backend"))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .text_sm()
                .child(form_field("id", form.read(cx).id.clone()))
                .child(form_field("kind", form.read(cx).kind.clone()))
                .child(form_field("endpoint", form.read(cx).endpoint.clone())),
        )
        .child(
            Button::new("backend-add")
                .label("Add backend")
                .small()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_add_backend(cx);
                })),
        )
        .into_any_element()
}

/// Inline add-MCP-server form state (http or stdio).
pub(crate) struct McpForm {
    /// Server identity input.
    pub id: Entity<InputState>,
    /// Transport input (`http` or `stdio`).
    pub transport: Entity<InputState>,
    /// HTTP endpoint input (http transport).
    pub endpoint: Entity<InputState>,
    /// Command input (stdio transport).
    pub command: Entity<InputState>,
    /// API key input, stored in the secret store.
    pub api_key: Entity<InputState>,
}

impl McpForm {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Entity<Self> {
        let mut make = |placeholder: &'static str| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let id = make("id, e.g. my-mcp");
        let transport = make("http | stdio");
        let endpoint = make("https://mcp.example.com/mcp");
        let command = make("stdio: command (e.g. npx)");
        let api_key = make("api key (leave empty to skip)");
        cx.new(|_| Self {
            id,
            transport,
            endpoint,
            command,
            api_key,
        })
    }
}

fn render_mcp_form(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let form = workspace.mcp_form(window, cx);
    div()
        .id("mcp-form")
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_md()
        .bg(cx.theme().secondary)
        .child(
            div()
                .text_xs()
                .opacity(0.7)
                .child("Add a custom MCP server"),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .text_sm()
                .child(form_field("id", form.read(cx).id.clone()))
                .child(form_field("transport", form.read(cx).transport.clone()))
                .child(form_field(
                    "endpoint (http)",
                    form.read(cx).endpoint.clone(),
                ))
                .child(form_field("command (stdio)", form.read(cx).command.clone()))
                .child(form_field(
                    "api key (stored in secrets.json)",
                    form.read(cx).api_key.clone(),
                )),
        )
        .child(
            Button::new("mcp-add")
                .label("Add server")
                .small()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_add_mcp(cx);
                })),
        )
        .into_any_element()
}

fn form_field(label: &str, input: Entity<InputState>) -> impl IntoElement {
    div()
        .id(format!("form-field-{label}"))
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_xs().opacity(0.6).child(label.to_owned()))
        .child(div().h(px(28.)).child(Input::new(&input)))
}

/// Inline custom-endpoint provider form state.
pub(crate) struct ProviderForm {
    /// Provider identity input.
    pub id: Entity<InputState>,
    /// Wire-protocol kind input.
    pub kind: Entity<InputState>,
    /// Base URL input.
    pub base_url: Entity<InputState>,
    /// Default model input.
    pub model: Entity<InputState>,
    /// API key input; stored in the secret store, never in settings.
    pub api_key: Entity<InputState>,
}

impl ProviderForm {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Entity<Self> {
        let mut make = |placeholder: &'static str| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let id = make("id, e.g. openai-main");
        let kind = make("anthropic-messages | openai-completions | openai-responses");
        let base_url = make("https://api.example.com/v1");
        let model = make("model id");
        let api_key = make("api key (leave empty to skip)");
        cx.new(|_| Self {
            id,
            kind,
            base_url,
            model,
            api_key,
        })
    }
}

fn render_provider_form(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let form = workspace.provider_form(window, cx);
    div()
        .id("provider-form")
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_md()
        .bg(cx.theme().secondary)
        .child(div().text_xs().opacity(0.7).child("Add a custom endpoint"))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .text_sm()
                .child(form_field("id", form.read(cx).id.clone()))
                .child(form_field("kind", form.read(cx).kind.clone()))
                .child(form_field("base URL", form.read(cx).base_url.clone()))
                .child(form_field("model", form.read(cx).model.clone()))
                .child(form_field(
                    "api key (stored in secrets.json)",
                    form.read(cx).api_key.clone(),
                )),
        )
        .child(
            Button::new("provider-add")
                .label("Add provider")
                .small()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_add_provider(cx);
                })),
        )
        .into_any_element()
}

// ---- helpers ----

fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

fn ellipsis(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_owned()
    } else {
        let head: String = text.chars().take(max_chars).collect();
        format!("{head}\u{2026}")
    }
}

fn project_label(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}
