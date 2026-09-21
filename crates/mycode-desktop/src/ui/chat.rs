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
        // The Desk timeline pairs each tool call with its result (both share
        // `call_id`) so a call renders as one ledger block; unpaired entries
        // keep their flat order. Pairing is display-only: state is untouched.
        let mut elements: Vec<gpui_kit::AnyElement> = Vec::with_capacity(entries.len());
        let mut index = 0;
        while index < entries.len() {
            let entry = &entries[index];
            if entry.kind == EntryKind::UserMessage {
                elements.push(render_user_entry(entry, index > 0, index, cx));
            } else if entry.kind == EntryKind::ToolCall {
                let result = entry.call_id.as_deref().and_then(|call| {
                    entries[index + 1..].iter().find(|next| {
                        next.kind == EntryKind::ToolResult && next.call_id.as_deref() == Some(call)
                    })
                });
                elements.push(render_tool_block(entry, result, cx.theme()));
            } else if entry.kind == EntryKind::ToolResult
                && entry.call_id.is_some()
                && entries[..index]
                    .iter()
                    .any(|prev| prev.kind == EntryKind::ToolCall && prev.call_id == entry.call_id)
            {
                // Already shown inside its call's block above.
            } else {
                elements.push(render_entry(entry, cx.theme()));
            }
            index += 1;
        }
        let streaming_element = streaming
            .map(|streaming| render_streaming_entry(streaming, cx.theme()).into_any_element());
        (show_welcome, elements, streaming_element)
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

/// The in-flight assistant turn: the demo's `.think` dashed box for the
/// reasoning tail plus bare streaming text — no bubble.
fn render_streaming_entry(
    streaming: &crate::view_model::StreamingReply,
    theme: &Theme,
) -> impl IntoElement {
    let desk = super::desk::Desk::of(theme);
    div()
        .id("streaming-entry")
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
                .text_color(desk.faint)
                .child("stream"),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .when(!streaming.thinking.is_empty(), |this| {
                    this.child(
                        div()
                            .id("streaming-thinking")
                            .border_1()
                            .border_dashed()
                            .border_color(theme.border)
                            .rounded(px(3.))
                            .px_2()
                            .py(px(6.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .overflow_hidden()
                            .child(ellipsis(&streaming.thinking, 400)),
                    )
                })
                .when(!streaming.text.is_empty(), |this| {
                    this.child(
                        div()
                            .id("streaming-text")
                            .text_sm()
                            .w_full()
                            .child(streaming.text.clone()),
                    )
                }),
        )
}

/// The welcome hero shown when no conversation has started.
fn render_welcome(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let recents = workspace.vm().recents.clone();
    let desk = super::desk::Desk::of(theme);
    div()
        .id("welcome")
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .py(px(64.))
        .child(
            // The desk wordmark — plain type, no tile: MYCODE//UI in amber on
            // ink, matching the title bar brand.
            div()
                .id("welcome-title")
                .flex()
                .flex_row()
                .items_baseline()
                .gap_1()
                .text_xl()
                .font_weight(gpui_kit::FontWeight::BOLD)
                .child("MYCODE")
                .child(div().text_color(desk.amber).child("//"))
                .child("UI"),
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
                .mt_4()
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
                    .mt_6()
                    .w(px(440.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(desk.faint)
                            .font_weight(gpui_kit::FontWeight::BOLD)
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
                    })),
            )
        })
        .into_any_element()
}

/// Desk transcript entries: the demo's timeline blocks — a mono stamp gutter
/// plus a ledger row per entry. User rows are plain text with a `▸` arrow
/// (`.msg-user`), assistant text is bare (`.msg-agent`), and each tool call
/// renders as one bordered block (`.tool`) with its result inside.
fn render_entry(entry: &ConversationEntry, theme: &Theme) -> gpui_kit::AnyElement {
    match entry.kind {
        EntryKind::UserMessage => desk_block(
            entry,
            theme,
            div()
                .flex()
                .flex_row()
                .gap_2()
                .text_sm()
                .child(
                    div()
                        .text_color(super::desk::Desk::of(theme).cyan)
                        .child("▸"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(SharedString::from(&entry.text)),
                ),
        )
        .into_any_element(),
        EntryKind::AssistantMessage => desk_block(
            entry,
            theme,
            div()
                .text_sm()
                .w_full()
                .overflow_hidden()
                .child(SharedString::from(&entry.text)),
        )
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
    div()
        .id(format!("entry-{}", entry.event_id))
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
                .text_color(super::desk::Desk::of(theme).faint)
                .child(short_stamp(&entry.event_id)),
        )
        .child(div().flex_1().min_w_0().flex().flex_col().child(content))
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
    let mut block = div()
        .id(format!("entry-{}", call.event_id))
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
        .bg(theme.background)
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
        block = block.child(div().border_t_1().border_color(theme.border).child(body));
    }
    desk_block(call, theme, block).into_any_element()
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
    // Thinking effort rides the provider wire; without an enabled provider
    // the control would be dead weight in the chip row.
    let has_enabled_provider = workspace
        .vm()
        .settings
        .as_ref()
        .is_some_and(|settings| settings.providers.iter().any(|provider| provider.enabled));

    div()
        .id("composer")
        .flex()
        .w_full()
        .border_t_1()
        .border_color(theme.border)
        .bg(theme.sidebar)
        .px_4()
        .py_2()
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
                                .min_h(px(40.))
                                .child(Textarea::new(&composer)),
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
                        .when(has_enabled_provider, |this| {
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
                                    .child(Icon::new(IconName::Sparkles).xsmall().flex_shrink_0())
                                    .child(reasoning_label),
                            )
                        })
                        .child(div().flex_1().min_w_0())
                        .child(
                            Button::new("send")
                                .icon(IconName::ArrowUp)
                                .primary()
                                .rounded(px(3.))
                                .flex_shrink_0()
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
    let desk = super::desk::Desk::of(theme);
    // Desk `.msg-user`: plain left-aligned ledger row with a cyan ▸ arrow.
    // The hover edit/recall actions are unchanged — they now sit inline to
    // the right of the text instead of under a right-aligned bubble.
    let mut column = div().flex_1().min_w_0().flex().flex_col().gap_1().child(
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
    /// One reasoning-effort level.
    Reasoning {
        level: &'static str,
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
    let has_providers = !providers.is_empty();
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
    // Thinking effort only matters once a provider is configured.
    if has_providers {
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
