//! GPUI rendering for the workspace, in the opencode-desktop spirit: one
//! project-centric sidebar, a centered conversation with an integrated
//! composer, and a full-page settings view with its own secondary nav.
mod chat;
mod context;
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

use crate::view_model::{MainView, UpdateState};
use crate::workspace::Workspace;

pub(crate) use settings::{BackendForm, McpForm, ProviderForm};

/// Renders the whole window chrome and content.
pub fn render_root(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme = cx.theme();
    let ambient = skin::ambient(theme);
    let focus_handle = workspace.focus_handle().clone();
    div()
        .id("workspace")
        .relative()
        .flex()
        .flex_col()
        .size_full()
        .bg(theme.background)
        .text_color(theme.foreground)
        // Root focus plus the key listener below keep Escape alive even when
        // no input holds focus: bubbled key events reach this node from any
        // focused descendant, and from itself via the startup focus.
        .track_focus(&focus_handle)
        .on_key_down(cx.listener(|workspace, event: &KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" {
                workspace.on_escape(cx);
            }
        }))
        .child(render_title_bar(workspace, cx))
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
                .flex()
                .flex_row()
                .bg(ambient)
                .when(
                    workspace.vm().view == crate::view_model::MainView::Chat,
                    |this| this.child(sidebar::render_sidebar(workspace, cx)),
                )
                .child(match workspace.vm().view {
                    MainView::Chat => chat::render_chat(workspace, window, cx).into_any_element(),
                    MainView::Settings => {
                        settings::render_settings_view(workspace, window, cx).into_any_element()
                    }
                })
                .when(
                    workspace.vm().view == MainView::Chat && workspace.vm().active.is_some(),
                    |this| this.child(context::render_context_panel(workspace, window, cx)),
                ),
        )
        .when(workspace.vm().project_menu_open, |this| {
            this.child(sidebar::render_project_menu_layer(workspace, cx))
        })
        .when(workspace.vm().model_menu_open, |this| {
            this.child(chat::render_model_menu_layer(workspace, cx))
        })
        .when(
            workspace
                .vm()
                .mention
                .as_ref()
                .is_some_and(|mention| !mention.items.is_empty()),
            |this| this.child(chat::render_mention_layer(workspace, cx)),
        )
}

fn render_title_bar(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
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
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .size(px(18.))
                        .rounded(px(5.))
                        .bg(theme.primary)
                        .text_color(theme.primary_foreground)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::BOLD)
                        .child("M"),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child("MCode"),
                )
                .child(div().text_xs().opacity(0.5).child(subtitle)),
        )
        .child(div().flex().flex_row().items_center().gap_2().when_some(
            update_label,
            |this, label| {
                this.child(
                    Button::new("title-update")
                        .label(label)
                        .small()
                        .warning()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_show_main_view(MainView::Settings, cx);
                        })),
                )
            },
        ))
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
        .rounded(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(theme.muted_foreground)
        .hover(|this| this.bg(theme.secondary))
        .child(Icon::new(icon).with_size(px(14.)))
        .on_click(on_click)
}

// ---- shared helpers ----

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
