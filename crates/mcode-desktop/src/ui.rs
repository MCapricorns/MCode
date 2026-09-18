//! GPUI rendering for the workspace: Cursor-style three-column layout.

use gpui_kit::component::TitleBar;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea};
use gpui_kit::component::{ActiveTheme as _, Disableable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Window, div,
};
use gpui_kit::{px, rems};

use crate::view_model::{ContextTab, ConversationEntry, EntryKind};
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
        .child(
            div()
                .id("columns")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_row()
                .child(render_sidebar(workspace, cx))
                .child(render_chat(workspace, cx))
                .child(render_context_panel(workspace, window, cx)),
        )
}

fn render_title_bar(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let session_title: SharedString = workspace
        .vm()
        .active
        .as_ref()
        .map(|conversation| format!("Session {}…", short_id(&conversation.session_id)).into())
        .unwrap_or_else(|| "MCode".into());
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
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::BOLD)
                                .child("MCode"),
                        )
                        .child(div().text_xs().opacity(0.6).child(session_title)),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("theme-toggle")
                                .label(if workspace.vm().dark_theme {
                                    "Light"
                                } else {
                                    "Dark"
                                })
                                .on_click(cx.listener(|workspace, _, window, cx| {
                                    workspace.on_toggle_theme(window, cx);
                                })),
                        )
                        .child(Button::new("open-settings").label("Settings").on_click(
                            cx.listener(|workspace, _, _, cx| {
                                workspace.on_show_tab(ContextTab::Settings, cx);
                            }),
                        )),
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
        .child(div().child(message))
        .child(
            Button::new("dismiss-error")
                .label("Dismiss")
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_dismiss_error(cx);
                })),
        )
}

fn render_sidebar(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let sessions = workspace
        .vm()
        .sessions
        .iter()
        .map(|summary| summary.session_id.clone())
        .collect::<Vec<_>>();
    div()
        .id("sidebar")
        .w(px(264.))
        .h_full()
        .flex()
        .flex_col()
        .border_r_1()
        .border_color(theme.border)
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
                        .child("Sessions"),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(Button::new("refresh-sessions").label("Refresh").on_click(
                            cx.listener(|workspace, _, _, cx| {
                                workspace.refresh_sessions(cx);
                            }),
                        ))
                        .child(Button::new("new-session").label("New").primary().on_click(
                            cx.listener(|workspace, _, _, cx| {
                                workspace.on_new_session(cx);
                            }),
                        )),
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
                .py_1()
                .children(sessions.into_iter().map(|session_id| {
                    let active = workspace
                        .vm()
                        .active
                        .as_ref()
                        .is_some_and(|conversation| conversation.session_id == session_id);
                    div()
                        .id(format!("session-row-{session_id}"))
                        .px_3()
                        .py_2()
                        .cursor_pointer()
                        .rounded_sm()
                        .when(active, |this| this.bg(theme.secondary))
                        .hover(|this| this.bg(theme.secondary_hover))
                        .on_click({
                            let session_id = session_id.clone();
                            cx.listener(move |workspace, _, _, cx| {
                                workspace.on_open_session(&session_id, cx);
                            })
                        })
                        .child(div().text_sm().child(format!(
                            "{}…  ·  {}",
                            short_id(&session_id),
                            "chat"
                        )))
                })),
        )
}

fn render_chat(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let composer = workspace.composer().clone();
    let entries: Vec<ConversationEntry> = workspace
        .vm()
        .active
        .as_ref()
        .map(|conversation| conversation.entries.clone())
        .unwrap_or_default();
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
                .gap_2()
                .p_4()
                .when(entries.is_empty(), |this| {
                    this.child(
                        div()
                            .id("conversation-empty")
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_sm()
                            .opacity(0.5)
                            .child("No messages yet — provider replies arrive with T11"),
                    )
                })
                .children(entries.into_iter().map(|entry| render_entry(entry, theme))),
        )
        .child(
            div()
                .id("composer")
                .border_t_1()
                .border_color(theme.border)
                .p_3()
                .flex()
                .flex_row()
                .items_end()
                .gap_2()
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
                        .label("Send")
                        .primary()
                        .disabled(workspace.vm().sending || workspace.vm().active.is_none())
                        .on_click(cx.listener(|workspace, _, window, cx| {
                            workspace.on_send(window, cx);
                        })),
                ),
        )
}

fn render_entry(entry: ConversationEntry, theme: &gpui_kit::component::Theme) -> impl IntoElement {
    let mine = entry.kind == EntryKind::UserMessage;
    div()
        .id(format!("entry-{}", entry.event_id))
        .flex()
        .flex_col()
        .when(mine, |this| this.items_end())
        .child(
            div()
                .max_w(rems(40.))
                .px_3()
                .py_2()
                .rounded_md()
                .text_sm()
                .when(mine, |this| {
                    this.bg(theme.primary).text_color(theme.primary_foreground)
                })
                .when(!mine, |this| this.bg(theme.secondary))
                .child(entry.text.clone()),
        )
}

fn render_context_panel(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let tab = workspace.vm().context_tab;
    div()
        .id("context-panel")
        .w(px(340.))
        .h_full()
        .flex()
        .flex_col()
        .border_l_1()
        .border_color(theme.border)
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
                .child(context_tab_button(
                    tab,
                    ContextTab::Settings,
                    "Settings",
                    cx,
                )),
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
                    ContextTab::Settings => {
                        render_settings(workspace, window, cx).into_any_element()
                    }
                }),
        )
}

fn context_tab_button(
    current: ContextTab,
    tab: ContextTab,
    label: &str,
    cx: &mut Context<Workspace>,
) -> Button {
    let selected = current == tab;
    Button::new(format!("context-tab-{label}"))
        .label(label)
        .when(selected, |this| this.primary())
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.on_show_tab(tab, cx);
        }))
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
        .child(
            div()
                .mt_2()
                .p_2()
                .rounded_md()
                .bg(theme.secondary)
                .text_xs()
                .opacity(0.7)
                .child("Files, diffs, todo, and usage land here with T13–T15"),
        )
}

fn render_settings(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let Some(settings) = workspace.vm().settings.clone() else {
        return div()
            .id("settings-loading")
            .text_sm()
            .opacity(0.6)
            .child("Loading settings…");
    };
    let ua_input = workspace.settings_ua_input(window, cx);
    let provider_form_element = render_provider_form(workspace, window, cx);
    let theme = cx.theme();
    let provider_rows: Vec<String> = settings
        .providers
        .iter()
        .map(|provider| provider.id.clone())
        .collect();
    div()
        .id("settings")
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child("Settings"),
        )
        .child(
            div()
                .id("settings-ua")
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().opacity(0.7).child("User-Agent"))
                .child(div().h(px(30.)).text_sm().child(Input::new(&ua_input)))
                .child(
                    div()
                        .text_xs()
                        .opacity(0.5)
                        .child(format!("Effective: {}", settings.effective_user_agent)),
                ),
        )
        .child(
            div()
                .id("settings-providers")
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().opacity(0.7).child("Providers"))
                .when(provider_rows.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .opacity(0.5)
                            .child("No providers yet — add one below"),
                    )
                })
                .children(provider_rows.iter().enumerate().map(|(index, id)| {
                    div()
                        .id(format!("provider-row-{id}"))
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .py_1()
                        .child(
                            div()
                                .text_sm()
                                .child(format!("{id} · {}", provider_kind_of(workspace, index))),
                        )
                        .child(
                            Button::new(format!("provider-remove-{id}"))
                                .label("Remove")
                                .on_click(cx.listener(move |workspace, _, _, cx| {
                                    workspace.on_remove_provider(index, cx);
                                })),
                        )
                })),
        )
        .child(provider_form_element)
        .child(
            div()
                .id("settings-footer")
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    Button::new("settings-save")
                        .label("Save settings")
                        .primary()
                        .disabled(!settings.dirty || settings.saving)
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_save_settings(cx);
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .opacity(0.5)
                        .child(format!("Revision {}", settings.revision)),
                ),
        )
        .child(
            div()
                .p_2()
                .rounded_md()
                .bg(theme.secondary)
                .text_xs()
                .opacity(0.7)
                .child("Search backends and MCP servers become editable here with T13/T14"),
        )
}

fn provider_kind_of(workspace: &Workspace, index: usize) -> String {
    workspace
        .vm()
        .settings
        .as_ref()
        .and_then(|settings| settings.providers.get(index))
        .map(|provider| provider.kind.clone())
        .unwrap_or_default()
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
        .child(div().text_xs().opacity(0.7).child("Add provider"))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .text_sm()
                .child(form_field("id", form.read(cx).id.clone()))
                .child(form_field("kind", form.read(cx).kind.clone()))
                .child(form_field("base URL", form.read(cx).base_url.clone()))
                .child(form_field("model", form.read(cx).model.clone())),
        )
        .child(
            Button::new("provider-add")
                .label("Add")
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_add_provider(cx);
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

fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

/// Inline add-provider form state.
pub(crate) struct ProviderForm {
    /// Provider identity input.
    pub id: Entity<InputState>,
    /// Adapter kind input.
    pub kind: Entity<InputState>,
    /// Base URL input.
    pub base_url: Entity<InputState>,
    /// Default model input.
    pub model: Entity<InputState>,
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
        cx.new(|_| Self {
            id,
            kind,
            base_url,
            model,
        })
    }
}

impl Render for ProviderForm {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
