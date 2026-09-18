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

use crate::view_model::{ContextTab, ConversationEntry, DesktopAction, EntryKind};
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
                .children(entries.into_iter().map(|entry| render_entry(entry, theme)))
                .when(
                    workspace
                        .vm()
                        .active
                        .as_ref()
                        .and_then(|c| c.streaming.as_ref())
                        .is_some(),
                    |this| {
                        this.child(
                            div()
                                .id("streaming-entry")
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .id("streaming-thinking")
                                        .when(!streaming_thinking(workspace).is_empty(), |this| {
                                            this.px_3().py_1().text_xs().opacity(0.6).child(
                                                format!(
                                                    "thinking: {}",
                                                    streaming_thinking(workspace)
                                                ),
                                            )
                                        })
                                        .child(div()),
                                )
                                .child(
                                    div()
                                        .id("streaming-text")
                                        .max_w(rems(40.))
                                        .px_3()
                                        .py_2()
                                        .rounded_md()
                                        .text_sm()
                                        .bg(theme.secondary)
                                        .opacity(0.9)
                                        .child(format!("{}\u{2026}", streaming_text(workspace))),
                                ),
                        )
                    },
                ),
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
    let label = match entry.kind {
        EntryKind::ToolCall => format!("\u{1f527} {}", entry.text),
        EntryKind::ToolResult => format!("\u{2705} {}", entry.text),
        EntryKind::Usage => format!("\u{1f4ca} {}", entry.text),
        _ => entry.text.clone(),
    };
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
                .child(label),
        )
}

fn streaming_text(workspace: &Workspace) -> String {
    workspace
        .vm()
        .active
        .as_ref()
        .and_then(|c| c.streaming.as_ref())
        .map(|s| s.text.as_str())
        .unwrap_or_default()
        .to_owned()
}

fn streaming_thinking(workspace: &Workspace) -> String {
    workspace
        .vm()
        .active
        .as_ref()
        .and_then(|c| c.streaming.as_ref())
        .map(|s| s.thinking.as_str())
        .unwrap_or_default()
        .to_owned()
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
                .child(context_tab_button(tab, ContextTab::Web, "Web", cx))
                .child(context_tab_button(tab, ContextTab::Changes, "Changes", cx))
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
                    ContextTab::Web => render_web(workspace, window, cx).into_any_element(),
                    ContextTab::Changes => render_changes(workspace, cx).into_any_element(),
                    ContextTab::Settings => {
                        render_settings(workspace, window, cx).into_any_element()
                    }
                }),
        )
}

/// Renders the bounded web search panel.
fn render_web(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let query_input = workspace.web_query_input(window, cx);
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
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child("Web search"),
        )
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
                        .label("Search")
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
                .id(format!("web-result-{url}"))
                .flex()
                .flex_col()
                .gap_0p5()
                .p_2()
                .rounded_md()
                .bg(cx.theme().secondary)
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(title),
                )
                .child(div().text_xs().opacity(0.7).child(snippet))
                .child(div().text_xs().opacity(0.5).child(url))
        }))
        .into_any_element()
}

/// Renders the changed-files panel fed by tool activity details.
fn render_changes(workspace: &Workspace, cx: &Context<Workspace>) -> gpui_kit::AnyElement {
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
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child("Changes"),
        )
        .when(changed.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .opacity(0.5)
                    .child("File edits and diffs from tool runs appear here"),
            )
        })
        .children(changed.into_iter().map(|text| {
            div()
                .id(format!("change-item-{}", short_id(&text)))
                .p_2()
                .rounded_md()
                .text_sm()
                .bg(cx.theme().secondary)
                .child(text)
        }))
        .into_any_element()
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
    let backend_form_element = render_backend_form(workspace, window, cx);
    let mcp_form_element = render_mcp_form(workspace, window, cx);
    let theme = cx.theme();
    let provider_rows: Vec<String> = settings
        .providers
        .iter()
        .map(|provider| provider.id.clone())
        .collect();
    let backend_rows: Vec<(String, String, bool)> = settings
        .web_backends
        .iter()
        .map(|backend| (backend.id.clone(), backend.kind.clone(), backend.enabled))
        .collect();
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
                    let key_set = settings.providers_with_keys.iter().any(|keyed| keyed == id);
                    div()
                        .id(format!("provider-row-{id}"))
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .py_1()
                        .child(div().text_sm().child(format!(
                            "{id} · {} · key {}",
                            provider_kind_of(workspace, index),
                            if key_set { "\u{2713}" } else { "\u{2717}" }
                        )))
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
                .id("settings-backends")
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().opacity(0.7).child("Web search backends"))
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
                                .py_1()
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
                                                .on_click(cx.listener(
                                                    move |workspace, _, _, cx| {
                                                        workspace.on_toggle_backend(index, cx);
                                                    },
                                                )),
                                        )
                                        .child(
                                            Button::new(format!("backend-remove-{id}"))
                                                .label("Remove")
                                                .on_click(cx.listener(
                                                    move |workspace, _, _, cx| {
                                                        workspace.on_remove_backend(index, cx);
                                                    },
                                                )),
                                        ),
                                )
                        }),
                ),
        )
        .child(backend_form_element)
        .child(
            div()
                .id("settings-mcp")
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().opacity(0.7).child("MCP servers"))
                .when(mcp_rows.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .opacity(0.5)
                            .child("No MCP servers yet"),
                    )
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
                                .py_1()
                                .child(
                                    div().text_sm().child(format!(
                                        "{id} \u{b7} {transport} \u{b7} {} \u{b7} key {}",
                                        if *enabled { "enabled" } else { "disabled" },
                                        if *keyed { "\u{2713}" } else { "-" }
                                    )),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .gap_1()
                                        .child(
                                            Button::new(format!("mcp-toggle-{id}"))
                                                .label(if *enabled { "Disable" } else { "Enable" })
                                                .on_click(cx.listener(
                                                    move |workspace, _, _, cx| {
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
                                                            DesktopAction::SettingsMcpToggled(
                                                                index, on,
                                                            ),
                                                            cx,
                                                        );
                                                    },
                                                )),
                                        )
                                        .child(
                                            Button::new(format!("mcp-tools-{id}"))
                                                .label("List tools")
                                                .on_click({
                                                    let id = id.clone();
                                                    cx.listener(
                                                        move |workspace, _, _, cx| {
                                                            workspace.on_list_mcp_tools(&id, cx);
                                                        },
                                                    )
                                                }),
                                        )
                                        .child(
                                            Button::new(format!("mcp-remove-{id}"))
                                                .label("Remove")
                                                .on_click(cx.listener(
                                                    move |workspace, _, _, cx| {
                                                        workspace.apply_action(
                                                            DesktopAction::SettingsMcpRemoved(
                                                                index,
                                                            ),
                                                            cx,
                                                        );
                                                    },
                                                )),
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
                            .child(div().text_xs().opacity(0.5).child("Built-in catalog"))
                            .children(catalog_rows.iter().map(|(id, transport)| {
                                div()
                                    .id(format!("mcp-catalog-{id}"))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div().text_sm().opacity(0.8).child(format!(
                                            "{id} \u{b7} {transport} \u{b7} paste your API key, then Add"
                                        )),
                                    )
                                    .child(
                                        Button::new(format!("mcp-add-{id}"))
                                            .label("Add")
                                            .primary()
                                            .on_click({
                                                let id = id.clone();
                                                cx.listener(
                                                move |workspace, _, window, cx| {
                                                    let server =
                                                        mcode_config::builtin_mcp_servers()
                                                            .into_iter()
                                                            .find(|server| server.id == id)
                                                            .expect("catalog entry");
                                                    let key = workspace
                                                        .mcp_key_input(window, cx)
                                                        .read(cx)
                                                        .value()
                                                        .trim()
                                                        .to_owned();
                                                    workspace.on_add_builtin_mcp(
                                                        server, &key, cx,
                                                    );
                                                },
                                            )
                                            }),
                                    )
                            })),
                    )
                }),
        )
        .child(mcp_form_element)
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
                .child(form_field("model", form.read(cx).model.clone()))
                .child(form_field(
                    "api key (stored in secrets.json)",
                    form.read(cx).api_key.clone(),
                )),
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
        .child(div().text_xs().opacity(0.7).child("Add MCP server"))
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

fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

/// Inline add-provider form state.
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

impl Render for ProviderForm {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
