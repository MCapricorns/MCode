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
use crate::view_model::{
    ConversationEntry, EntryKind, selected_model_supports_reasoning, transcript_start,
};
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
    let sending = workspace.vm().sending;
    let extra = workspace.vm().transcript_extra;
    let (show_welcome, hidden, entry_elements, streaming_element) = {
        let active = workspace.vm().active.as_ref();
        let entries: &[ConversationEntry] = active
            .map(|conversation| conversation.entries.as_slice())
            .unwrap_or_default();
        let streaming = active.and_then(|c| c.streaming.as_ref());
        let show_welcome = entries.is_empty() && streaming.is_none() && !sending;
        let items = collect_transcript_items(entries);
        let start = transcript_start(items.len(), extra);
        let hidden = start;
        let mut elements: Vec<gpui_kit::AnyElement> = Vec::with_capacity(items.len() - start);
        for item in items.into_iter().skip(start) {
            match item {
                TranscriptItem::User { entry, index } => {
                    elements.push(render_user_entry(entry, index > 0, index, cx));
                }
                TranscriptItem::Tool { call, result } => {
                    elements.push(render_tool_block(call, result, cx.theme()));
                }
                TranscriptItem::Entry(entry) => {
                    elements.push(render_entry(entry, cx.theme()));
                }
            }
        }
        let streaming_element = streaming
            .map(|streaming| render_streaming_entry(streaming, cx.theme()).into_any_element())
            .or_else(|| {
                sending.then(|| {
                    render_streaming_entry(
                        &crate::view_model::StreamingReply {
                            status: "Waiting for the model".to_owned(),
                            ..crate::view_model::StreamingReply::default()
                        },
                        cx.theme(),
                    )
                    .into_any_element()
                })
            });
        (show_welcome, hidden, elements, streaming_element)
    };
    let scroll_handle = workspace.conversation_scroll_handle().clone();
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
                .track_scroll(&scroll_handle)
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
                        .when(hidden > 0, |this| this.child(render_fold_chip(hidden, cx)))
                        .children(entry_elements)
                        .when_some(streaming_element, |this, streaming| this.child(streaming)),
                ),
        )
        // The model menu docks in-flow right above the composer: an
        // absolutely positioned overlay landed outside the visible window on
        // mis-scaled displays, and a docked panel cannot be clipped away.
        .when(workspace.vm().model_menu_open, |this| {
            this.child(render_model_menu(workspace, cx))
        })
        .when(workspace.vm().reasoning_menu_open, |this| {
            this.child(render_reasoning_menu(workspace, cx))
        })
        .when(
            workspace
                .vm()
                .mention
                .as_ref()
                .is_some_and(|mention| !mention.items.is_empty()),
            |this| this.child(render_mention_layer(workspace, cx)),
        )
        .child(render_composer(workspace, _window, cx))
        .into_any_element()
}

/// The in-flight assistant turn: a live status line, then thinking and text.
fn render_streaming_entry(
    streaming: &crate::view_model::StreamingReply,
    theme: &Theme,
) -> impl IntoElement {
    let desk = super::desk::Desk::of(theme);
    let status = if streaming.status.is_empty() {
        "Working".to_owned()
    } else {
        streaming.status.clone()
    };
    desk_shell(
        "live".to_owned(),
        theme,
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(ledger_tag(
                &format!("WORKING · {status}"),
                desk.amber,
                desk.amber.opacity(0.45),
                theme,
            ))
            .when(!streaming.thinking.is_empty(), |this| {
                this.child(thinking_box(
                    "streaming-thinking".into(),
                    &streaming.thinking,
                    theme,
                ))
            })
            .when(!streaming.text.is_empty(), |this| {
                this.child(ledger_tag(
                    "AGENT",
                    desk.green,
                    desk.green.opacity(0.35),
                    theme,
                ))
                .child(agent_text(streaming.text.clone(), theme))
            })
            .when(
                streaming.thinking.is_empty() && streaming.text.is_empty(),
                |this| {
                    this.child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(status),
                    )
                },
            ),
    )
}

/// The welcome hero shown when no conversation has started.
fn render_welcome(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let recents = workspace.vm().recents.clone();
    let settings = workspace.vm().settings.clone();
    let desk = super::desk::Desk::of(theme);
    let agents_ready = settings.as_ref().is_none_or(|settings| {
        settings.subagents.roles.is_empty()
            || settings.subagents.roles.iter().any(|role| role.enabled)
    });
    let mcp_ready = settings
        .as_ref()
        .is_some_and(|settings| settings.mcp_servers.iter().any(|server| server.enabled));
    let web_ready = settings
        .as_ref()
        .is_some_and(|settings| settings.web_backends.iter().any(|backend| backend.enabled));
    let skills_ready = !workspace.vm().skills.is_empty();
    div()
        .id("welcome")
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .py(px(56.))
        .child(
            div()
                .id("welcome-title")
                .flex()
                .flex_row()
                .items_baseline()
                .gap_1()
                .text_xl()
                .font_weight(gpui_kit::FontWeight::BOLD)
                .child("MYCODE"),
        )
        .child(
            div()
                .id("welcome-accent")
                .w(px(72.))
                .h(px(3.))
                .rounded(px(2.))
                .bg(super::skin::accent(theme, 90.)),
        )
        .child(
            div()
                .id("welcome-tagline")
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("A coding agent for your desktop."),
        )
        .child(
            div()
                .id("welcome-chips")
                .flex()
                .flex_row()
                .gap_1()
                .child(capability_chip("AGENTS", agents_ready, desk.violet, theme))
                .child(capability_chip("SKILLS", skills_ready, desk.amber, theme))
                .child(capability_chip("MCP", mcp_ready, desk.amber, theme))
                .child(capability_chip("WEB", web_ready, desk.cyan, theme))
                .child(capability_chip("FILES", true, desk.green, theme)),
        )
        .child(
            div()
                .id("welcome-hint")
                .text_xs()
                .text_color(desk.faint)
                .max_w(rems(38.))
                .flex()
                .flex_col()
                .items_center()
                .gap_0()
                .child("Open a project folder so the agent can read and edit files,")
                .child("or just start chatting — tools stay available everywhere."),
        )
        .child(
            div()
                .id("welcome-actions")
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
                    .w(px(460.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded(px(3.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.sidebar)
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(desk.faint)
                            .px_1()
                            .pb(px(4.))
                            .child("RECENT PROJECTS"),
                    )
                    .children(recents.iter().take(5).map(|project| {
                        let project = project.clone();
                        div()
                            .id(format!("recent-{}", super::short_id(&project)))
                            .group("recent-row")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py(px(7.))
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|this| this.bg(theme.secondary))
                            .on_click({
                                let project = project.clone();
                                cx.listener(move |workspace, _, _, cx| {
                                    workspace.on_open_recent(&project, cx);
                                })
                            })
                            .child(super::lamp(desk.faint))
                            .child(
                                Icon::new(IconName::Folder)
                                    .xsmall()
                                    .flex_shrink_0()
                                    .text_color(theme.muted_foreground),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .child(project_label(&project)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .opacity(0.4)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis_start()
                                    .child(project.clone()),
                            )
                            .child(
                                div()
                                    .id(format!("recent-remove-{}", super::short_id(&project)))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(20.))
                                    .rounded(px(2.))
                                    .flex_shrink_0()
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
                    })),
            )
        })
        .into_any_element()
}

fn capability_chip(
    label: &str,
    ready: bool,
    color: gpui_kit::Hsla,
    theme: &Theme,
) -> impl IntoElement {
    let desk = super::desk::Desk::of(theme);
    div().flex().flex_row().child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px(px(7.))
            .py(px(2.))
            .border_1()
            .border_color(if ready {
                color.opacity(0.4)
            } else {
                theme.border
            })
            .rounded(px(2.))
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .text_color(if ready { color } else { desk.faint })
            .child(label.to_owned()),
    )
}

enum TranscriptItem<'a> {
    User {
        entry: &'a ConversationEntry,
        index: usize,
    },
    Tool {
        call: &'a ConversationEntry,
        result: Option<&'a ConversationEntry>,
    },
    Entry(&'a ConversationEntry),
}

fn collect_transcript_items(entries: &[ConversationEntry]) -> Vec<TranscriptItem<'_>> {
    let mut items = Vec::with_capacity(entries.len());
    let mut index = 0;
    while index < entries.len() {
        let entry = &entries[index];
        if entry.kind == EntryKind::UserMessage {
            items.push(TranscriptItem::User { entry, index });
        } else if entry.kind == EntryKind::ToolCall {
            let result = entry.call_id.as_deref().and_then(|call| {
                entries[index + 1..].iter().find(|next| {
                    next.kind == EntryKind::ToolResult && next.call_id.as_deref() == Some(call)
                })
            });
            items.push(TranscriptItem::Tool {
                call: entry,
                result,
            });
        } else if entry.kind == EntryKind::ToolResult
            && entry.call_id.is_some()
            && entries[..index]
                .iter()
                .any(|prev| prev.kind == EntryKind::ToolCall && prev.call_id == entry.call_id)
        {
            // Already shown inside its call's block above.
        } else {
            items.push(TranscriptItem::Entry(entry));
        }
        index += 1;
    }
    items
}

fn render_fold_chip(hidden: usize, cx: &Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    div()
        .id("transcript-fold")
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap_2()
        .py(px(8.))
        .rounded(px(12.))
        .border_1()
        .border_color(skin::glass_border(theme))
        .bg(skin::glass(theme))
        .cursor_pointer()
        .on_click(cx.listener(|workspace, _, _, cx| {
            workspace.on_reveal_transcript(cx);
        }))
        .child(super::lamp(desk.violet))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("{hidden} earlier messages")),
        )
}

/// Desk transcript entries: the demo's timeline blocks — a mono stamp gutter
/// plus a ledger row per entry. User rows are plain text with a `▸` arrow
/// (`.msg-user`), assistant text is bare (`.msg-agent`), and each tool call
/// renders as one bordered block (`.tool`) with its result inside.
fn render_entry(entry: &ConversationEntry, theme: &Theme) -> gpui_kit::AnyElement {
    match entry.kind {
        EntryKind::UserMessage => desk_block(entry, theme, {
            let desk = super::desk::Desk::of(theme);
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(ledger_tag("YOU", desk.cyan, desk.cyan.opacity(0.35), theme))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .text_sm()
                        .child(div().text_color(desk.cyan).child("▸"))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_normal()
                                .child(SharedString::from(&entry.text)),
                        ),
                )
        })
        .into_any_element(),
        EntryKind::AssistantMessage => desk_block(entry, theme, {
            let desk = super::desk::Desk::of(theme);
            div()
                .flex()
                .flex_col()
                .gap_2()
                .when(!entry.thinking.is_empty(), |this| {
                    this.child(ledger_tag("THINKING", desk.faint, theme.border, theme))
                        .child(thinking_box(
                            format!("thinking-{}", entry.event_id).into(),
                            &entry.thinking,
                            theme,
                        ))
                })
                .child(ledger_tag(
                    "AGENT",
                    desk.green,
                    desk.green.opacity(0.35),
                    theme,
                ))
                .child(agent_text(SharedString::from(&entry.text), theme))
        })
        .into_any_element(),
        EntryKind::ToolCall => render_tool_block(entry, None, theme),
        EntryKind::ToolResult => {
            let desk = super::desk::Desk::of(theme);
            let failed = entry.text.starts_with("failed:");
            desk_block(
                entry,
                theme,
                div()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(if failed {
                        desk.red
                    } else {
                        theme.muted_foreground
                    })
                    .child(ellipsis(&entry.text, 600)),
            )
            .into_any_element()
        }
        EntryKind::Usage => div()
            .id(format!("entry-{}", entry.event_id))
            .into_any_element(),
    }
}

/// The timeline block shell: stamp gutter + content, matching the demo's
/// `.block` (left stamp, hover anchor omitted — no interaction change).
fn desk_block(
    entry: &ConversationEntry,
    theme: &Theme,
    content: impl IntoElement,
) -> impl IntoElement {
    desk_shell(
        short_stamp(&entry.event_id),
        theme,
        div()
            .id(format!("entry-{}", entry.event_id))
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(content),
    )
}

fn desk_shell(stamp: String, theme: &Theme, content: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .gap_3()
        .w_full()
        .child(
            div()
                .w(px(52.))
                .flex_shrink_0()
                .pt(px(3.))
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .text_color(super::desk::Desk::of(theme).faint)
                .child(stamp),
        )
        .child(content)
}

fn ledger_tag(
    label: &str,
    color: gpui_kit::Hsla,
    border: gpui_kit::Hsla,
    theme: &Theme,
) -> impl IntoElement {
    // The chip sits in a row so a flex-col parent cannot stretch it full width.
    div().flex().flex_row().child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .px(px(7.))
            .py(px(2.))
            .border_1()
            .border_color(border)
            .rounded(px(2.))
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .text_color(color)
            .child(label.to_owned()),
    )
}

fn thinking_box(id: SharedString, text: &str, theme: &Theme) -> impl IntoElement {
    let desk = super::desk::Desk::of(theme);
    div()
        .id(id)
        .w_full()
        .border_1()
        .border_dashed()
        .border_color(theme.border)
        .rounded(px(3.))
        .px_3()
        .py_2()
        .bg(desk.think_bg)
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .text_color(desk.faint)
                .child(div().text_color(desk.amber).child("▸"))
                .child("REASONING"),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child(text.to_owned()),
        )
}

fn agent_text(text: impl Into<SharedString>, theme: &Theme) -> impl IntoElement {
    div()
        .w_full()
        .rounded(px(3.))
        .px_3()
        .py_2()
        .bg(theme.secondary)
        .text_sm()
        .whitespace_normal()
        .child(text.into())
}

/// Stable short stamp for the gutter: the entry id is a ledger identity, not
/// a clock time, so show its tail (mirrors `short_id`, 8 chars, mono).
fn short_stamp(event_id: &str) -> String {
    let tail: String = event_id
        .chars()
        .rev()
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    tail
}

/// One tool ledger block: header row (lamp + name + target) with the result
/// rendered inside as diff-green/red or CRT terminal text. Failures paint the
/// border red; a pending call (no result yet) shows the amber lamp.
fn render_tool_block(
    call: &ConversationEntry,
    result: Option<&ConversationEntry>,
    theme: &Theme,
) -> gpui_kit::AnyElement {
    let desk = super::desk::Desk::of(theme);
    let failed = result.is_some_and(|r| r.text.starts_with("failed:"));
    let waiting = result.is_none();
    let lamp_color = if failed {
        desk.red
    } else if waiting {
        desk.amber
    } else {
        desk.green
    };
    let mut card = div()
        .flex()
        .flex_col()
        .w_full()
        .rounded(px(3.))
        .border_1()
        .border_color(if failed {
            desk.red.opacity(0.4)
        } else {
            theme.border
        })
        .bg(theme.sidebar)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .py(px(7.))
                .text_xs()
                .child(super::lamp(lamp_color))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.foreground)
                        .child(call.text.to_string()),
                ),
        );
    if let Some(result) = result {
        let body = result_body(result, theme, &desk);
        card = card.child(div().border_t_1().border_color(theme.border).child(body));
    }
    desk_block(
        call,
        theme,
        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .child(ledger_tag(
                "TOOL",
                desk.amber,
                desk.amber.opacity(0.3),
                theme,
            ))
            .child(card),
    )
    .into_any_element()
}

/// The tool result body: diff-style green/red lines stay colored text on the
/// panel; longer output renders as a dark CRT strip (`.term`) in both modes.
fn result_body(
    result: &ConversationEntry,
    theme: &Theme,
    desk: &super::desk::Desk,
) -> impl IntoElement {
    let text = result.text.to_string();
    let failed = text.starts_with("failed:");
    let lines: Vec<&str> = text.lines().collect();
    let looks_diff = lines
        .iter()
        .any(|line| line.starts_with('+') || line.starts_with('-') || line.starts_with("@@"));
    if looks_diff {
        div()
            .flex()
            .flex_col()
            .py_1()
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .children(lines.iter().take(40).map(|line| {
                let (color, bg) = if line.starts_with('+') {
                    (desk.green, desk.green.opacity(0.07))
                } else if line.starts_with('-') {
                    (desk.red, desk.red.opacity(0.06))
                } else {
                    (theme.muted_foreground, theme.transparent)
                };
                div()
                    .px_2()
                    .text_color(color)
                    .bg(bg)
                    .child(line.to_string())
            }))
            .into_any_element()
    } else if failed || text.len() > 300 || lines.len() > 6 {
        div()
            .px_2()
            .py_1()
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .bg(desk.screen)
            .text_color(if failed { desk.red } else { desk.screen_dim })
            .child(ellipsis(&text, 2000))
            .into_any_element()
    } else {
        div()
            .px_2()
            .py_1()
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .text_color(theme.muted_foreground)
            .child(ellipsis(&text, 600))
            .into_any_element()
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
    let model_label: SharedString = model_picker_label(workspace.vm()).into();
    let reasoning_label: SharedString = reasoning_chip_label(workspace.vm()).into();
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
    // Thinking effort rides the provider wire; hide the chip when no
    // provider is enabled or the catalog model does not reason.
    let show_reasoning_chip = workspace
        .vm()
        .settings
        .as_ref()
        .is_some_and(|settings| settings.providers.iter().any(|provider| provider.enabled))
        && selected_model_supports_reasoning(workspace.vm());

    div()
        .id("composer")
        .flex()
        .flex_col()
        .w_full()
        .border_t_1()
        .border_color(super::skin::glass_border(theme))
        .bg(super::skin::glass(theme))
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
                .mx_auto()
                .w_full()
                .max_w(rems(46.))
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
                                .text_color(super::desk::Desk::of(theme).amber)
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
                        .flex_wrap()
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
                                .min_w_0()
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
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_open_project_dialog(cx);
                                }))
                                .child(
                                    Icon::new(if has_project {
                                        IconName::FolderOpen
                                    } else {
                                        IconName::Folder
                                    })
                                    .xsmall()
                                    .flex_shrink_0(),
                                )
                                .child(div().min_w_0().truncate().child(project_chip_label)),
                        )
                        .child(
                            div()
                                .id("composer-model-chip")
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1()
                                .min_w_0()
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
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    let open = !workspace.vm().model_menu_open;
                                    workspace.on_toggle_model_menu(open, cx);
                                }))
                                .child(Icon::new(IconName::Bot).xsmall().flex_shrink_0())
                                // `provider · model` is the longest chip label
                                // and grows with the catalog; clip it here so
                                // it never displaces the send button.
                                .child(div().min_w_0().truncate().child(model_label)),
                        )
                        .when(show_reasoning_chip, |this| {
                            this.child(
                                div()
                                    .id("composer-reasoning-chip")
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_1()
                                    .flex_shrink_0()
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
                                    .on_click(cx.listener(|workspace, _, _, cx| {
                                        let open = !workspace.vm().reasoning_menu_open;
                                        workspace.on_toggle_reasoning_menu(open, cx);
                                    }))
                                    .child(Icon::new(IconName::Sparkles).xsmall().flex_shrink_0())
                                    .child(reasoning_label),
                            )
                        })
                        .child(div().flex_1().min_w_0())
                        .child(
                            div()
                                .text_xs()
                                .flex_shrink_0()
                                .text_color(theme.muted_foreground)
                                .child("Enter to send"),
                        )
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

/// Follow-ups waiting behind the in-flight turn; each row can be dismissed.
fn render_queued_followups(items: Vec<String>, cx: &mut Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    div()
        .id("composer-queue")
        .flex()
        .flex_col()
        .gap_1()
        .mx_auto()
        .w_full()
        .max_w(rems(46.))
        .pb_1()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(super::lamp(desk.amber))
                .child(
                    div()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(desk.faint)
                        .child(format!("QUEUED  {}", items.len())),
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

/// One user bubble with hover actions: edit-and-resend (rewinds to before)
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
    let desk = super::desk::Desk::of(theme);
    // Desk `.msg-user`: plain left-aligned ledger row with a cyan ▸ arrow.
    // The hover edit/recall actions are unchanged — they now sit inline to
    // the right of the text instead of under a right-aligned bubble.
    let mut column = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(ledger_tag("YOU", desk.cyan, desk.cyan.opacity(0.35), theme))
        .child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .w_full()
                .text_sm()
                .child(div().text_color(desk.cyan).child("▸"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_normal()
                        .child(SharedString::from(&entry.text)),
                ),
        );
    if can_rewind {
        column = column.child(
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
    div()
        .id(format!("entry-{}", entry.event_id))
        .flex()
        .flex_row()
        .gap_3()
        .w_full()
        .group("user-entry")
        .child(
            div()
                .w(px(52.))
                .flex_shrink_0()
                .pt(px(3.))
                .text_xs()
                .text_color(desk.faint)
                .child(short_stamp(&entry.event_id)),
        )
        .child(column)
        .into_any_element()
}

/// Composer chip label for the thinking-effort submenu.
fn reasoning_chip_label(vm: &crate::view_model::WorkspaceState) -> String {
    let selected = vm
        .settings
        .as_ref()
        .and_then(|settings| settings.reasoning.as_deref())
        .filter(|level| {
            crate::view_model::selected_reasoning_levels(vm)
                .iter()
                .any(|item| item == level)
        })
        .unwrap_or("default");
    format!("Thinking \u{b7} {selected}")
}

fn reasoning_row_label(level: &str) -> String {
    match level {
        "default" => "Default \u{b7} provider".to_owned(),
        "off" => "Off".to_owned(),
        "on" => "On".to_owned(),
        "minimal" => "Minimal".to_owned(),
        "low" => "Low \u{b7} brief".to_owned(),
        "medium" => "Medium \u{b7} balanced".to_owned(),
        "high" => "High \u{b7} deep".to_owned(),
        "xhigh" => "Extra high".to_owned(),
        "max" => "Max".to_owned(),
        other => other.to_owned(),
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
/// and thinking levels share a fixed height so the panel reads as one grid.
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
    /// One reasoning-effort level advertised by the selected catalog model.
    Reasoning {
        level: String,
        label: String,
        selected: bool,
    },
}

/// Fixed row height that keeps the menu rows visually uniform.
const MENU_ROW_HEIGHT: gpui_kit::Pixels = px(30.);

/// The model picker panel, docked in-flow above the composer: provider rows
/// and the selected provider's configured models. Plain rows in a bounded
/// scroll area — a virtualized list collapsed to a sliver inside a
/// height-less container, hiding the menu behind the window edge.
pub(super) fn render_model_menu(
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
    let configured_models: Vec<String> = selected_provider
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
    // Providers offer far more models than the configured subset; the
    // catalog list is the menu, with any configured-but-uncataloged ids
    // appended so nothing the user saved disappears.
    let models: Vec<String> = selected_provider
        .as_deref()
        .and_then(|provider_id| {
            workspace
                .vm()
                .catalog
                .as_ref()
                .and_then(|catalog| catalog.provider(provider_id))
        })
        .map(|provider| {
            let mut all: Vec<String> = provider
                .models
                .iter()
                .map(|model| model.id.clone())
                .collect();
            for model in &configured_models {
                if !all.contains(model) {
                    all.push(model.clone());
                }
            }
            all.truncate(mycode_config::MAX_MODELS_PER_PROVIDER);
            all
        })
        .unwrap_or(configured_models);

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

    let weak = cx.weak_entity();
    div()
        .id("model-menu-layer")
        .w_full()
        .px_4()
        .pb_1()
        .child(
            div()
                .id("model-menu")
                .mx_auto()
                .w_full()
                .max_w(rems(46.))
                .rounded(px(3.))
                .border_1()
                .border_color(skin::glass_border(theme))
                .bg(skin::popover(theme))
                .text_color(theme.popover_foreground)
                .shadow_lg()
                .p_2()
                .max_h(px(420.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(rows.iter().map(|row| model_menu_row(row, &weak, theme))),
        )
        .into_any_element()
}

/// Thinking-effort submenu, docked like the model picker so a click picks
/// one level instead of cycling the chip.
fn render_reasoning_menu(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let selected = workspace
        .vm()
        .settings
        .as_ref()
        .and_then(|settings| settings.reasoning.clone())
        .filter(|level| {
            crate::view_model::selected_reasoning_levels(workspace.vm())
                .iter()
                .any(|item| item == level)
        })
        .unwrap_or_else(|| "default".to_owned());
    let levels = crate::view_model::selected_reasoning_levels(workspace.vm());
    let rows: Vec<ModelMenuRow> = std::iter::once(ModelMenuRow::Header {
        label: "THINKING",
        divider: false,
    })
    .chain(levels.into_iter().map(|level| ModelMenuRow::Reasoning {
        selected: level == selected,
        label: reasoning_row_label(&level),
        level,
    }))
    .collect();

    let weak = cx.weak_entity();
    div()
        .id("reasoning-menu-layer")
        .w_full()
        .px_4()
        .pb_1()
        .child(
            div()
                .id("reasoning-menu")
                .mx_auto()
                .w_full()
                .max_w(rems(46.))
                .rounded(px(3.))
                .border_1()
                .border_color(skin::glass_border(theme))
                .bg(skin::popover(theme))
                .text_color(theme.popover_foreground)
                .shadow_lg()
                .p_2()
                .flex()
                .flex_col()
                .children(rows.iter().map(|row| model_menu_row(row, &weak, theme))),
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
            let level = level.clone();
            let weak = weak.clone();
            menu_row(
                format!("reasoning-{level}"),
                label.clone(),
                *selected,
                move |_, _, cx| {
                    let picked = level.clone();
                    let _ = weak.update(cx, |workspace, cx| {
                        workspace.on_select_reasoning(&picked, cx);
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
        .child(div().min_w_0().truncate().child(label))
        .when(selected, |this| {
            this.child(
                Icon::new(IconName::Check)
                    .xsmall()
                    .flex_shrink_0()
                    .text_color(theme.primary),
            )
        })
}

/// The `@` file and `/` command autocomplete panel, docked in-flow above
/// the composer like the model menu.
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
        .w_full()
        .px_4()
        .pb_1()
        .child(
            div()
                .id("mention-menu")
                .mx_auto()
                .w_full()
                .max_w(rems(46.))
                .max_h(px(300.))
                .overflow_y_scroll()
                .rounded(px(3.))
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

/// Floating ask card: questions stay on top of the transcript until answered.
pub(super) fn render_ask_panel(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let rows: Vec<(String, Vec<String>, bool)> =
        workspace.vm().pending_ask.clone().unwrap_or_default();
    let picks = workspace.vm().ask_answers.clone();
    let single = rows.len() == 1;
    let ask_input = workspace.ask_input(window, cx);
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    div()
        .id("ask-layer")
        .absolute()
        .inset_0()
        .flex()
        .items_start()
        .justify_center()
        .pt(px(72.))
        .px_4()
        .child(
            div()
                .id("ask-scrim")
                .absolute()
                .inset_0()
                .bg(skin::scrim(theme)),
        )
        .child(
            div()
                .id("ask-card")
                .relative()
                .w_full()
                .max_w(px(460.))
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
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
                        .gap_2()
                        .child(super::lamp(desk.violet))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child("The agent needs your input"),
                        ),
                )
                .children(
                    rows.iter()
                        .enumerate()
                        .map(|(index, (question, choices, optional))| {
                            let picked = picks.get(index).cloned().unwrap_or_default();
                            div()
                                .id(format!("ask-row-{index}"))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(div().text_sm().child(format!(
                                    "{}. {}",
                                    index + 1,
                                    question
                                )))
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
                                                    let selected = picked == *choice;
                                                    Button::new(format!(
                                                        "ask-{index}-{choice_index}-{}",
                                                        super::short_id(choice)
                                                    ))
                                                    .label(choice.clone())
                                                    .small()
                                                    .when(selected, |this| this.primary())
                                                    .when(!selected, |this| this.outline())
                                                    .on_click(cx.listener(
                                                        move |workspace, _, _, cx| {
                                                            workspace.on_pick_ask_choice(
                                                                index,
                                                                answer.clone(),
                                                                single,
                                                                cx,
                                                            );
                                                        },
                                                    ))
                                                },
                                            )),
                                    )
                                })
                                .when(*optional, |this| {
                                    this.child(div().text_xs().opacity(0.5).child("Optional"))
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
                                .h(px(32.))
                                .text_sm()
                                .child(Input::new(&ask_input)),
                        )
                        .child(
                            Button::new("ask-submit")
                                .label("Answer")
                                .small()
                                .primary()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_submit_free_ask(cx);
                                })),
                        ),
                ),
        )
        .into_any_element()
}

// The form state types live with the settings view; re-exported for the
// workspace module.
