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

use crate::ui::skin::popover_panel;
use crate::view_model::{MentionKind, selected_reasoning_level};
use crate::workspace::Workspace;

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
    // Providers offer far more models than the menu can show. Rank the
    // strongest first (o3, gpt-5, opus) so the cap does not hide them, and
    // keep configured ids that the catalog does not list.
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
            crate::view_model::rank_model_ids(all)
                .into_iter()
                .take(mycode_config::MAX_MODELS_PER_PROVIDER)
                .collect()
        })
        .unwrap_or(configured_models);
    let suggested = crate::view_model::suggested_model_ids(models.iter().cloned());
    let rest: Vec<String> = models
        .iter()
        .filter(|id| !suggested.iter().any(|picked| picked == *id))
        .cloned()
        .collect();

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
    if !suggested.is_empty() {
        rows.push(ModelMenuRow::Header {
            label: "SUGGESTED",
            divider: true,
        });
        rows.extend(suggested.into_iter().map(|id| ModelMenuRow::Model {
            selected: Some(&id) == selected_model.as_ref(),
            id,
        }));
    }
    if !rest.is_empty() {
        rows.push(ModelMenuRow::Header {
            label: "MODEL",
            divider: true,
        });
        rows.extend(rest.into_iter().map(|id| ModelMenuRow::Model {
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
            popover_panel("model-menu", theme)
                .w_full()
                .max_h(px(420.))
                .overflow_y_scroll()
                .p_2()
                .flex()
                .flex_col()
                .children(rows.iter().map(|row| model_menu_row(row, &weak, theme))),
        )
        .into_any_element()
}

/// Thinking-effort submenu, docked like the model picker so a click picks
/// one level instead of cycling the chip.
pub(super) fn render_reasoning_menu(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let selected = selected_reasoning_level(workspace.vm());
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
            popover_panel("reasoning-menu", theme)
                .w_full()
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
