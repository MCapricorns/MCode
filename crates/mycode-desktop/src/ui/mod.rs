//! GPUI rendering for the workspace in the Desk look (docs/design/demo.html):
//! a flat trading-desk ledger — hairline panels, near-zero radius, dense mono
//! type — over the existing project/sidebar/conversation/settings structure.
//! Functionality is unchanged; only the visual skin moves.
mod chat;
mod context;
pub(crate) mod desk;
mod settings;
mod sidebar;
mod skin;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::TitleBar;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    ClickEvent, Context, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::view_model::{MainView, UpdateState, cache_percent};
use crate::workspace::Workspace;

pub(crate) use settings::{BackendForm, McpForm, ProviderForm};

/// How much of the desk chrome fits the current window width.
///
/// The sidebar and the inspector are fixed-width columns, so in a narrow
/// window they squeeze the transcript down to nothing. The sidebar stays
/// mounted because it owns the project picker, the session list, and the
/// Settings entry; the inspector is withdrawn instead, and the figures worth
/// keeping (token ledger, cache share) move onto the always-visible tape.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct DeskLayout {
    /// Whether the right inspector column is mounted.
    inspector: bool,
    /// Whether the tape strip has room for its secondary figures.
    wide_tape: bool,
}

impl DeskLayout {
    /// Width below which the inspector is withdrawn: both rails plus a
    /// readable transcript need this much room.
    const INSPECTOR_MIN: f32 = 1180.;
    /// Width below which the tape keeps only its lamps and headline counts.
    const WIDE_TAPE_MIN: f32 = 1040.;

    fn of(window: &Window) -> Self {
        let width = f32::from(window.viewport_size().width);
        Self {
            inspector: width >= Self::INSPECTOR_MIN,
            wide_tape: width >= Self::WIDE_TAPE_MIN,
        }
    }
}

/// Renders the whole window chrome and content.
pub fn render_root(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let layout = DeskLayout::of(window);
    let theme = cx.theme().clone();
    let mono = theme.mono_font_family.clone();
    let bg = theme.background;
    let fg = theme.foreground;
    let focus_handle = workspace.focus_handle().clone();
    div()
        .id("workspace")
        .relative()
        .flex()
        .flex_col()
        .size_full()
        .bg(bg)
        .text_color(fg)
        // Root focus plus the key listener below keep Escape alive even when
        // no input holds focus: bubbled key events reach this node from any
        // focused descendant, and from itself via the startup focus.
        .track_focus(&focus_handle)
        .on_key_down(cx.listener(|workspace, event: &KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" {
                workspace.on_escape(cx);
            }
        }))
        .font_family(mono)
        .child(render_title_bar(workspace, cx))
        .child(render_tape(workspace, layout, cx))
        .when(workspace.vm().error.is_some(), |this| {
            this.child(render_error_banner(workspace, cx))
        })
        .when(workspace.vm().pending_ask.is_some(), |this| {
            this.child(chat::render_ask_panel(workspace, window, cx))
        })
        .child(
            div()
                .id("body")
                .flex_1()
                .min_h_0()
                .min_w_0()
                .overflow_hidden()
                .bg(bg)
                .flex()
                .flex_row()
                .when(workspace.vm().view == MainView::Chat, |this| {
                    this.child(sidebar::render_sidebar(workspace, cx))
                })
                .child(match workspace.vm().view {
                    MainView::Chat => chat::render_chat(workspace, window, cx).into_any_element(),
                    MainView::Settings => {
                        settings::render_settings_view(workspace, window, cx).into_any_element()
                    }
                })
                .when(
                    layout.inspector
                        && workspace.vm().view == MainView::Chat
                        && workspace.vm().active.is_some(),
                    |this| this.child(context::render_context_panel(workspace, window, cx)),
                ),
        )
        .when(workspace.vm().project_menu_open, |this| {
            this.child(sidebar::render_project_menu_layer(workspace, cx))
        })
}

fn render_title_bar(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let desk = desk::Desk::of(theme);
    let subtitle: SharedString = match workspace.vm().view {
        MainView::Chat => workspace
            .vm()
            .project_dir
            .as_deref()
            .map(project_label)
            .unwrap_or_else(|| "no project".to_owned())
            .into(),
        MainView::Settings => "Settings".into(),
    };
    let update_label: Option<SharedString> = match &workspace.vm().update {
        UpdateState::Available { version, .. } => Some(format!("v{version} available").into()),
        _ => None,
    };
    TitleBar::new()
        .child(
            div()
                .id("title-row")
                .flex()
                .flex_row()
                .items_center()
                .gap_5()
                .min_w_0()
                .flex_1()
                .overflow_hidden()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .flex_shrink_0()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::BOLD)
                                .child("MYCODE"),
                        )
                        .child(div().text_sm().text_color(desk.amber).child("//"))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::BOLD)
                                .child("UI"),
                        ),
                )
                // The project name is unbounded user data: truncate it in the
                // layout instead of letting it paint over the controls right.
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(subtitle),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_4()
                .flex_shrink_0()
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
                        .id("day-night-toggle")
                        .flex()
                        .flex_row()
                        .items_center()
                        .border_1()
                        .border_color(theme.border)
                        .rounded(px(3.))
                        .overflow_hidden()
                        .text_xs()
                        .child(theme_toggle_seg(
                            "DAY",
                            !workspace.vm().dark_theme,
                            &desk,
                            theme,
                            cx,
                        ))
                        .child(theme_toggle_seg(
                            "NIGHT",
                            workspace.vm().dark_theme,
                            &desk,
                            theme,
                            cx,
                        )),
                ),
        )
        .border_b_1()
        .border_color(theme.border)
}

/// One half of the demo's DAY/NIGHT segmented toggle; the active half paints
/// amber. Same `on_select_theme` path as the footer icon it replaces.
fn theme_toggle_seg(
    label: &'static str,
    on: bool,
    desk: &desk::Desk,
    theme: &gpui_kit::component::theme::Theme,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    div()
        .id(format!("theme-{label}"))
        .px_2()
        .py(px(2.))
        .cursor_pointer()
        .when(on, |this| {
            this.bg(desk.amber).text_color(theme.primary_foreground)
        })
        .when(!on, |this| this.text_color(theme.muted_foreground))
        .child(label)
        .on_click(cx.listener(move |workspace, _, window, cx| {
            workspace.on_select_theme(label == "NIGHT", window, cx);
        }))
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
        .gap_2()
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
                .min_w_0()
                .child(Icon::new(IconName::CircleAlert).with_size(px(14.)))
                .child(div().overflow_hidden().child(message)),
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

// ---- tape strip + shared helpers ----

/// The TAPE strip under the title bar: session lamps, the token ledger with
/// its cache share, and the active project — the demo's ticker row rendered
/// from live state instead of animated sample text.
///
/// The strip is one fixed-height row, so every cell is shrink-proof and the
/// row clips: a long project name must not paint over the figures. Under
/// width pressure the secondary figures drop out rather than collide.
fn render_tape(
    workspace: &mut Workspace,
    layout: DeskLayout,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let desk = desk::Desk::of(theme);
    let vm = workspace.vm();
    let sessions = vm.sessions.len();
    let open = vm.sessions.iter().filter(|s| s.active).count();
    let (input, output, cache) = vm.usage_totals.iter().fold(
        (0u64, 0u64, 0u64),
        |(input, output, cache), row| {
            (
                input + row.input,
                output + row.output,
                cache + row.cache,
            )
        },
    );
    let cache_share = cache_percent(cache, input);
    let project: SharedString = vm
        .project_dir
        .as_deref()
        .map(project_label)
        .unwrap_or_else(|| "no project".to_owned())
        .into();
    div()
        .id("tape")
        .flex()
        .flex_row()
        .items_center()
        .gap_5()
        .px_3()
        .h(px(26.))
        .flex_shrink_0()
        .overflow_hidden()
        .border_b_1()
        .border_color(theme.border)
        .bg(theme.background)
        .text_xs()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .flex_shrink_0()
                .text_color(desk.amber)
                .font_weight(gpui_kit::FontWeight::BOLD)
                .child("TAPE"),
        )
        .child(tape_stat("OPEN", &open.to_string(), desk.green, theme))
        .when(layout.wide_tape, |this| {
            this.child(tape_stat(
                "CHATS",
                &sessions.to_string(),
                theme.foreground,
                theme,
            ))
        })
        .child(tape_stat(
            "IN",
            &compact_count(input),
            theme.foreground,
            theme,
        ))
        .child(tape_stat(
            "OUT",
            &compact_count(output),
            theme.foreground,
            theme,
        ))
        // Cache share rides the tape because the inspector that also reports
        // it is withdrawn in a narrow window.
        .when_some(cache_share, |this, share| {
            this.child(tape_stat("CACHE", &format!("{share}%"), desk.cyan, theme))
        })
        .when(vm.sending, |this| {
            this.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .flex_shrink_0()
                    .text_color(desk.amber)
                    .child(lamp(desk.amber))
                    .child("STREAMING"),
            )
        })
        .child(div().flex_1().min_w_0())
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(project),
        )
}

fn tape_stat(
    label: &str,
    value: &str,
    color: gpui_kit::Hsla,
    theme: &gpui_kit::component::theme::Theme,
) -> impl IntoElement {
    div()
        .id(format!("tape-{label}"))
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .flex_shrink_0()
        .text_color(theme.muted_foreground)
        .child(label.to_owned())
        .child(div().text_color(color).child(value.to_owned()))
}

/// Compact token-count spelling: 12.3k / 1.2M (mirrors context.rs).
fn compact_count(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

/// A 7px status lamp: the demo's `.lamp` dot.
pub(super) fn lamp(color: gpui_kit::Hsla) -> impl IntoElement {
    div().size(px(7.)).rounded_full().bg(color)
}

pub(super) fn icon_button(
    id: impl Into<gpui_kit::ElementId>,
    icon: IconName,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id(id)
        .size(px(26.))
        .rounded(px(3.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(theme.muted_foreground)
        .hover(|this| this.bg(theme.secondary))
        .child(Icon::new(icon).with_size(px(14.)))
        .on_click(on_click)
}

pub(super) fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

pub(super) fn ellipsis(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_owned()
    } else {
        let head: String = text.chars().take(max_chars).collect();
        format!("{head}\u{2026}")
    }
}

pub(super) fn project_label(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}
