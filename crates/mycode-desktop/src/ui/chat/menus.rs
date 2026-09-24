//! The docked composer menus: the model picker, the thinking-effort submenu,
//! and the `@`/`/` mention autocomplete.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    ClickEvent, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::i18n::t;
use crate::ui::skin::{self, popover_panel};
use crate::view_model::{MentionKind, selected_reasoning_level};
use crate::workspace::Workspace;

/// One row in the flattened model menu: section headers, providers, models,
/// and thinking levels share a fixed height so the panel reads as one grid.
enum ModelMenuRow {
    /// Section label; `divider` draws the separator line above it.
    Header { label: String, divider: bool },
    /// Non-interactive note row.
    Hint(&'static str),
    /// One configured model, with the provider it belongs to.
    Model {
        provider: String,
        id: String,
        detail: Option<String>,
        selected: bool,
    },
    /// Switches the open list to that provider. Does not save a selection.
    Provider {
        id: String,
        name: String,
        current: bool,
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
    let grouped: Vec<(String, String, Vec<String>)> = workspace
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
                    let catalog_ids = workspace
                        .vm()
                        .catalog
                        .as_ref()
                        .and_then(|catalog| catalog.provider(&provider.id))
                        .map(|entry| {
                            entry
                                .models
                                .iter()
                                .map(|model| model.id.clone())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let mut all = catalog_ids;
                    for model in &provider.models {
                        if !all.contains(model) {
                            all.push(model.clone());
                        }
                    }
                    let models = crate::view_model::rank_model_ids(all)
                        .into_iter()
                        .take(mycode_config::MAX_MODELS_PER_PROVIDER)
                        .collect();
                    (provider.id.clone(), name, models)
                })
                .collect()
        })
        .unwrap_or_default();
    let browse = workspace
        .vm()
        .model_menu_browse
        .clone()
        .or_else(|| selected_provider.clone());
    let mut rows = Vec::new();
    if grouped.is_empty() {
        rows.push(ModelMenuRow::Hint(t(
            "No enabled providers. Add one in Settings \u{b7} Models.",
            "没有启用的服务商。请在 设置 \u{b7} 模型 中添加。",
        )));
    }
    if let Some((provider, _name, models)) = grouped
        .iter()
        .find(|(id, _, _)| browse.as_deref() == Some(id.as_str()))
    {
        rows.push(ModelMenuRow::Header {
            label: t("Models", "模型").to_owned(),
            divider: false,
        });
        rows.extend(models.iter().map(|id| ModelMenuRow::Model {
            provider: provider.clone(),
            selected: selected_provider.as_deref() == Some(provider.as_str())
                && selected_model.as_deref() == Some(id.as_str()),
            id: id.clone(),
            detail: None,
        }));
    }
    let others: Vec<_> = grouped
        .iter()
        .filter(|(id, _, _)| browse.as_deref() != Some(id.as_str()))
        .collect();
    if !others.is_empty() {
        rows.push(ModelMenuRow::Header {
            label: t("Switch provider", "切换服务商").to_owned(),
            divider: true,
        });
        rows.extend(others.iter().map(|(id, name, _)| ModelMenuRow::Provider {
            id: id.clone(),
            name: name.clone(),
            current: false,
        }));
    }

    let weak = cx.weak_entity();
    div()
        .id("model-menu-layer")
        .w_full()
        .px_4()
        .pb_1()
        .flex()
        .flex_row()
        .justify_end()
        .child(
            popover_panel("model-menu", theme)
                .w(px(280.))
                .flex_none()
                .max_h(px(360.))
                .overflow_y_scroll()
                .p_1()
                .flex()
                .flex_col()
                .children(rows.iter().map(|row| model_menu_row(row, &weak, theme))),
        )
        .into_any_element()
}

/// Thinking effort as its own short list, opened from the composer button.
pub(super) fn render_thinking_menu(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let levels = crate::view_model::selected_reasoning_levels(workspace.vm());
    let selected = selected_reasoning_level(workspace.vm()).to_owned();
    let weak = cx.weak_entity();
    div()
        .id("thinking-menu-layer")
        .w_full()
        .px_4()
        .pb_1()
        .flex()
        .flex_row()
        .justify_end()
        .child(
            popover_panel("thinking-menu", theme)
                .w(px(220.))
                .flex_none()
                .p_1()
                .flex()
                .flex_col()
                .children(levels.iter().map(|level| {
                    let picked = level.clone();
                    let weak = weak.clone();
                    let on = level == &selected;
                    menu_row(
                        format!("thinking-row-{level}"),
                        reasoning_row_label(level),
                        on,
                        move |_, _, cx| {
                            let picked = picked.clone();
                            let _ = weak.update(cx, |workspace, cx| {
                                workspace.on_select_reasoning(&picked, cx);
                            });
                        },
                        theme,
                    )
                })),
        )
        .into_any_element()
}

/// Renders one visible row of the model menu.
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
                    .child(label.clone()),
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
        ModelMenuRow::Model {
            provider,
            id,
            detail,
            selected,
        } => {
            let model_id = id.clone();
            let provider_id = provider.clone();
            let weak = weak.clone();
            let label = match detail {
                Some(name) => format!("{id}  {name}"),
                None => id.clone(),
            };
            menu_row(
                format!("model-{provider}-{id}"),
                label,
                *selected,
                move |_, _, cx| {
                    let _ = weak.update(cx, |workspace, cx| {
                        workspace.on_select_model_on(&provider_id, &model_id, cx);
                    });
                },
                theme,
            )
            .into_any_element()
        }
        ModelMenuRow::Provider { id, name, current } => {
            let provider_id = id.clone();
            let weak = weak.clone();
            menu_row(
                format!("model-provider-{id}"),
                name.clone(),
                *current,
                move |_, _, cx| {
                    let _ = weak.update(cx, |workspace, cx| {
                        workspace.on_browse_model_provider(&provider_id, cx);
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
        .rounded(skin::radius_control())
        .text_sm()
        .cursor_pointer()
        .hover(|this| this.bg(skin::frost_hover(theme)))
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

fn reasoning_row_label(level: &str) -> String {
    match level {
        "default" => t("Default \u{b7} provider", "默认 \u{b7} 跟随服务商").to_owned(),
        "off" => t("Off", "关闭").to_owned(),
        "on" => t("On", "开启").to_owned(),
        "minimal" => t("Minimal", "极简").to_owned(),
        "low" => t("Low \u{b7} brief", "低 \u{b7} 简短").to_owned(),
        "medium" => t("Medium \u{b7} balanced", "中 \u{b7} 均衡").to_owned(),
        "high" => t("High \u{b7} deep", "高 \u{b7} 深入").to_owned(),
        "xhigh" => t("Extra high", "超高").to_owned(),
        "max" => t("Max", "最高").to_owned(),
        other => other.to_owned(),
    }
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
        MentionKind::File => t("FILES", "文件"),
        MentionKind::Command => t("COMMANDS", "命令"),
    };
    div()
        .id("mention-layer")
        .w_full()
        .px_4()
        .pb_1()
        .child(
            popover_panel("mention-menu", theme)
                .w_full()
                .max_h(px(300.))
                .overflow_y_scroll()
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
