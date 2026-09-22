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
    let thinking_levels = crate::view_model::selected_reasoning_levels(workspace.vm());
    let thinking_selected = selected_reasoning_level(workspace.vm());
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
    let suggested: Vec<(String, String)> = {
        let flat: Vec<(String, String)> = grouped
            .iter()
            .flat_map(|(id, _, models)| models.iter().map(|model| (id.clone(), model.clone())))
            .collect();
        let ranked =
            crate::view_model::suggested_model_ids(flat.iter().map(|(_, model)| model.clone()));
        ranked
            .into_iter()
            .filter_map(|model| {
                flat.iter()
                    .find(|(_, id)| *id == model)
                    .map(|(provider, model)| (provider.clone(), model.clone()))
            })
            .collect()
    };

    let mut rows = Vec::new();
    if grouped.is_empty() {
        rows.push(ModelMenuRow::Hint(
            "No enabled providers — add one in Settings \u{2192} Models",
        ));
    }
    if !suggested.is_empty() {
        rows.push(ModelMenuRow::Header {
            label: "SUGGESTED".to_owned(),
            divider: false,
        });
        rows.extend(suggested.iter().map(|(provider, id)| ModelMenuRow::Model {
            provider: provider.clone(),
            selected: selected_provider.as_deref() == Some(provider.as_str())
                && selected_model.as_deref() == Some(id.as_str()),
            id: id.clone(),
        }));
    }
    for (provider, name, models) in &grouped {
        rows.push(ModelMenuRow::Header {
            label: name.clone(),
            divider: true,
        });
        let shown: Vec<&String> = models
            .iter()
            .filter(|id| {
                !suggested
                    .iter()
                    .any(|(sug_provider, sug_id)| sug_provider == provider && sug_id == *id)
            })
            .collect();
        rows.extend(shown.into_iter().map(|id| ModelMenuRow::Model {
            provider: provider.clone(),
            selected: selected_provider.as_deref() == Some(provider.as_str())
                && selected_model.as_deref() == Some(id.as_str()),
            id: id.clone(),
        }));
    }

    let weak = cx.weak_entity();
    div()
        .id("model-menu-layer")
        .w_full()
        .px_4()
        .pb_1()
        .child(
            popover_panel("model-menu", theme)
                .w_full()
                .max_h(px(420.))
                .overflow_y_scroll()
                .p_2()
                .flex()
                .flex_col()
                .when(!thinking_levels.is_empty(), |this| {
                    this.child(thinking_strip(
                        &thinking_levels,
                        thinking_selected,
                        &weak,
                        theme,
                    ))
                })
                .children(rows.iter().map(|row| model_menu_row(row, &weak, theme))),
        )
        .into_any_element()
}

/// Thinking levels inside the model panel, so effort is not a second dropdown.
fn thinking_strip(
    levels: &[String],
    selected: &str,
    weak: &gpui_kit::WeakEntity<Workspace>,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .id("model-thinking-strip")
        .flex()
        .flex_row()
        .flex_wrap()
        .gap_1()
        .pb_2()
        .child(
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .opacity(0.6)
                .pr_1()
                .child("THINKING"),
        )
        .children(levels.iter().map(|level| {
            let picked = level.clone();
            let weak = weak.clone();
            let on = level == selected;
            div()
                .id(format!("thinking-chip-{level}"))
                .px_2()
                .h(px(24.))
                .flex()
                .items_center()
                .rounded(skin::radius_control())
                .text_xs()
                .cursor_pointer()
                .border_1()
                .border_color(if on {
                    theme.yellow.opacity(0.55)
                } else {
                    skin::glass_border(theme)
                })
                .bg(if on {
                    skin::frost_accent(theme)
                } else {
                    skin::frost(theme)
                })
                .hover(|this| this.bg(skin::frost_hover(theme)))
                .on_click(move |_, _, cx| {
                    let picked = picked.clone();
                    let _ = weak.update(cx, |workspace, cx| {
                        workspace.on_select_reasoning(&picked, cx);
                    });
                })
                .child(reasoning_row_label(level))
        }))
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
            selected,
        } => {
            let model_id = id.clone();
            let provider_id = provider.clone();
            let weak = weak.clone();
            menu_row(
                format!("model-{provider}-{id}"),
                id.clone(),
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
        MentionKind::File => "FILES",
        MentionKind::Command => "COMMANDS",
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
