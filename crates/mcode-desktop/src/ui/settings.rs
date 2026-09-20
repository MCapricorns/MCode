//! The full-page settings view: a secondary left nav (General / Models /
//! MCP / Web / Data / About) and a sectioned content pane with switch rows
//! and clean forms.
//!
//! Rendering order matters: entities are created and row lists materialized
//! with `&mut Context` first, and only then is `cx.theme()` borrowed for the
//! layout pass.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px, rems,
};

use super::{ellipsis, skin};
use crate::view_model::{DesktopAction, MainView, SettingsSection, UpdateState};
use crate::workspace::Workspace;

pub(super) fn render_settings_view(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let section = workspace.vm().settings_section;
    let settings_ready = workspace.vm().settings.is_some();
    let header_meta = workspace
        .vm()
        .settings
        .clone()
        .map(|s| (s.dirty, s.saving, s.revision));
    let theme = cx.theme();
    div()
        .id("settings-view")
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(theme.background)
        .child(
            div()
                .id("settings-header")
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px_6()
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .child(
                            Button::new("settings-back")
                                .icon(IconName::ArrowLeft)
                                .label("Back")
                                .small()
                                .ghost()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_show_main_view(MainView::Chat, cx);
                                })),
                        )
                        .child(
                            div()
                                .text_lg()
                                .font_weight(gpui_kit::FontWeight::BOLD)
                                .child("Settings"),
                        ),
                )
                .child(div().flex().flex_row().items_center().gap_2().when_some(
                    header_meta,
                    |this, (dirty, saving, revision)| {
                        this.child(
                            div()
                                .text_xs()
                                .opacity(0.45)
                                .child(format!("revision {revision}")),
                        )
                        .child(
                            Button::new("settings-save")
                                .icon(IconName::Check)
                                .label("Save changes")
                                .small()
                                .primary()
                                .disabled(!dirty || saving)
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_save_settings(cx);
                                })),
                        )
                    },
                )),
        )
        .child(
            div()
                .id("settings-body")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_row()
                .child(render_settings_nav(section, cx))
                .child(
                    div()
                        .id("settings-content")
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .overflow_y_scroll()
                        .child(
                            div()
                                .id("settings-content-inner")
                                .mx_auto()
                                .max_w(rems(52.))
                                .w_full()
                                .flex()
                                .flex_col()
                                .gap_4()
                                .px_6()
                                .py_4()
                                .when(!settings_ready, |this| {
                                    this.child(
                                        div()
                                            .text_sm()
                                            .opacity(0.6)
                                            .child("Loading settings\u{2026}"),
                                    )
                                })
                                .when(settings_ready, |this| {
                                    this.child(match section {
                                        SettingsSection::General => {
                                            render_general_section(workspace, window, cx)
                                        }
                                        SettingsSection::Models => {
                                            render_models_section(workspace, window, cx)
                                        }
                                        SettingsSection::Mcp => {
                                            render_mcp_section(workspace, window, cx)
                                        }
                                        SettingsSection::Web => {
                                            render_web_section(workspace, window, cx)
                                        }
                                        SettingsSection::Data => render_data_section(workspace, cx),
                                        SettingsSection::About => {
                                            render_about_section(workspace, cx)
                                        }
                                    })
                                }),
                        ),
                ),
        )
        .into_any_element()
}

fn render_settings_nav(section: SettingsSection, cx: &mut Context<Workspace>) -> impl IntoElement {
    let theme = cx.theme();
    let sections = [
        SettingsSection::General,
        SettingsSection::Models,
        SettingsSection::Mcp,
        SettingsSection::Web,
        SettingsSection::Data,
        SettingsSection::About,
    ];
    let desk = super::desk::Desk::of(theme);
    let rows: Vec<AnyElement> = sections
        .into_iter()
        .map(|candidate| {
            let selected = candidate == section;
            let icon = candidate.icon();
            let label = candidate.label();
            div()
                .id(format!("settings-nav-{}", candidate.id()))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .py(px(6.))
                .rounded(px(3.))
                .border_l_1()
                .border_color(theme.transparent)
                .when(selected, |this| this.border_color(desk.amber))
                .text_sm()
                .cursor_pointer()
                .when(selected, |this| {
                    this.bg(theme.sidebar_accent)
                        .text_color(theme.sidebar_accent_foreground)
                })
                .when(!selected, |this| this.text_color(theme.sidebar_foreground))
                .hover(|this| this.bg(theme.sidebar_accent))
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    workspace.on_show_settings_section(candidate, cx);
                }))
                .child(Icon::new(icon).small().text_color(if selected {
                    theme.sidebar_accent_foreground
                } else {
                    theme.muted_foreground
                }))
                .child(label)
                .into_any_element()
        })
        .collect();
    div()
        .id("settings-nav")
        .w(px(196.))
        .h_full()
        .flex()
        .flex_col()
        .gap_0p5()
        .p_3()
        .flex_shrink_0()
        .border_r_1()
        .border_color(theme.border)
        .bg(theme.sidebar)
        .children(rows)
}

/// One settings card: the desk version keeps the hairline border but sits on
/// a square, flat panel with a mono caption header.
fn settings_card(
    id: &str,
    title: &str,
    hint: Option<&str>,
    theme: &Theme,
    children: Vec<AnyElement>,
) -> impl IntoElement {
    div()
        .id(format!("card-{id}"))
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded(px(3.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.sidebar)
        .child(
            div()
                .id(format!("card-{id}-header"))
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::BOLD)
                        .child(title.to_owned()),
                )
                .when_some(hint, |this, hint| {
                    this.child(div().text_xs().opacity(0.5).child(hint.to_owned()))
                }),
        )
        .child(
            div()
                .id(format!("card-{id}-body"))
                .flex()
                .flex_col()
                .gap_2()
                .children(children),
        )
}

/// A label + control row used across sections.
fn settings_row(
    id: &str,
    label: &str,
    description: Option<&str>,
    control: AnyElement,
) -> AnyElement {
    div()
        .id(format!("row-{id}"))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_4()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .min_w_0()
                .child(div().text_sm().child(label.to_owned()))
                .when_some(description, |this, description| {
                    this.child(div().text_xs().opacity(0.5).child(description.to_owned()))
                }),
        )
        .child(control)
        .into_any_element()
}

// ---- General ----

fn render_general_section(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let ua_input = workspace.settings_ua_input(window, cx);
    let dark = workspace.vm().dark_theme;
    let effective_ua = workspace
        .vm()
        .settings
        .as_ref()
        .map(|settings| settings.effective_user_agent.clone())
        .unwrap_or_default();
    let theme_row = settings_row(
        "theme",
        "Color theme",
        None,
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
                        workspace.on_select_theme(false, window, cx);
                    }))
                    .into_any_element(),
            )
            .child(
                Button::new("theme-dark")
                    .icon(IconName::Moon)
                    .label("Dark")
                    .small()
                    .when(dark, |this| this.primary())
                    .when(!dark, |this| this.ghost())
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.on_select_theme(true, window, cx);
                    }))
                    .into_any_element(),
            )
            .into_any_element(),
    );
    let ua_field = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_sm().child("HTTP User-Agent"))
        .child(div().h(px(30.)).text_sm().child(Input::new(&ua_input)))
        .child(
            div()
                .text_xs()
                .opacity(0.5)
                .child(format!("Effective: {effective_ua}")),
        )
        .into_any_element();
    let theme = cx.theme();
    settings_card(
        "appearance",
        "Appearance",
        Some("Theme applies immediately and is saved to settings right away."),
        theme,
        vec![theme_row, ua_field],
    )
    .into_any_element()
}

// ---- Models ----

fn render_models_section(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    match workspace.vm().models_subview {
        crate::view_model::ModelsSubview::List => render_models_list_page(workspace, cx),
        crate::view_model::ModelsSubview::Catalog => {
            render_models_catalog_page(workspace, window, cx)
        }
        crate::view_model::ModelsSubview::Custom => {
            render_custom_provider_page(workspace, window, cx)
        }
    }
}

/// A secondary-page header: back arrow, title, and optional hint.
fn subview_header(
    title: &str,
    hint: Option<&str>,
    on_back: impl Fn(&mut Workspace, &mut Context<Workspace>) + 'static,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    div()
        .id("subview-header")
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .pb_1()
        .child(
            Button::new("subview-back")
                .icon(IconName::ArrowLeft)
                .label("Back")
                .small()
                .ghost()
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    on_back(workspace, cx);
                })),
        )
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::BOLD)
                .child(title.to_owned()),
        )
        .when_some(hint, |this, hint| {
            this.child(div().text_xs().opacity(0.5).child(hint.to_owned()))
        })
        .into_any_element()
}

/// The Models landing page: configured provider rows plus the two add paths.
fn render_models_list_page(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let Some(settings) = workspace.vm().settings.clone() else {
        return div().into_any_element();
    };
    let catalog = workspace.vm().catalog.clone();
    let mut provider_rows: Vec<(usize, String, String, String, usize, bool, bool)> = Vec::new();
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
            provider.enabled,
        ));
    }
    let provider_row_elements: Vec<AnyElement> = provider_rows
        .into_iter()
        .map(|(index, name, kind, host, models, keyed, enabled)| {
            provider_row(index, name, kind, host, models, keyed, enabled, cx)
        })
        .collect();
    let theme = cx.theme();
    let providers_empty = provider_row_elements.is_empty();
    settings_card(
        "providers",
        "Model providers",
        Some("Pick a provider, paste its API key, done. Every listed model becomes selectable in the composer."),
        theme,
        vec![
            div()
                .when(providers_empty, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .opacity(0.5)
                            .child("No providers yet — add one below"),
                    )
                })
                .children(provider_row_elements)
                .into_any_element(),
            div()
                .id("add-provider-row")
                .flex()
                .flex_row()
                .flex_wrap()
                .gap_2()
                .pt_1()
                .child(
                    Button::new("add-from-catalog")
                        .icon(IconName::Plus)
                        .label("Add from catalog\u{2026}")
                        .small()
                        .primary()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_show_models_subview(
                                crate::view_model::ModelsSubview::Catalog,
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new("add-custom-endpoint")
                        .icon(IconName::Terminal)
                        .label("Add custom endpoint\u{2026}")
                        .small()
                        .outline()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_show_models_subview(
                                crate::view_model::ModelsSubview::Custom,
                                cx,
                            );
                        })),
                )
                .into_any_element(),
        ],
    )
    .into_any_element()
}

/// The catalog picker page: search + every provider from models.dev; picking
/// one opens the preset configuration form.
fn render_models_catalog_page(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let Some(catalog) = workspace.vm().catalog.clone() else {
        return div().into_any_element();
    };
    // &mut Context work first: input entities and nested forms.
    let preset_search = workspace.preset_search_input(window, cx);
    let active_preset = workspace.vm().active_preset.clone();
    let preset_form_element = active_preset
        .as_ref()
        .map(|preset_id| render_preset_form(workspace, preset_id, window, cx))
        .unwrap_or_else(|| div().into_any_element());

    let preset_search_text = workspace.vm().preset_search.to_lowercase();
    let mut preset_rows: Vec<(String, String, String, usize)> = Vec::new();
    for provider in &catalog.providers {
        if !preset_search_text.is_empty()
            && !provider.name.to_lowercase().contains(&preset_search_text)
            && !provider.id.contains(&preset_search_text)
        {
            continue;
        }
        preset_rows.push((
            provider.id.clone(),
            provider.name.clone(),
            provider.kind.clone(),
            provider.models.len(),
        ));
    }
    // Lazy rows via `uniform_list`: only the visible window of providers is
    // measured and painted per frame. The container gets a definite pixel
    // height (row count, capped) — the earlier virtualized attempt collapsed
    // because its container had no height bound at all.
    let preset_rows = std::rc::Rc::new(preset_rows);
    let preset_list_weak = cx.weak_entity();
    let list_height = px(
        (preset_rows.len().clamp(1, 9) as f32) * PRESET_ROW_HEIGHT.as_f32() + 2.,
    );
    let list_rows = preset_rows.clone();
    let preset_list = div()
        .id("preset-catalog-list")
        .w_full()
        .h(list_height)
        .overflow_hidden()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .child(gpui_kit::uniform_list(
            "preset-catalog-rows",
            preset_rows.len(),
            move |range, _window, cx| {
                let theme = cx.theme().clone();
                range
                    .map(|index| {
                        let (id, name, kind, models) = &list_rows[index];
                        preset_row(
                            id.clone(),
                            name.clone(),
                            kind.clone(),
                            *models,
                            &preset_list_weak,
                            &theme,
                        )
                    })
                    .collect()
            },
        )
        .h_full());

    let has_preset = workspace.vm().active_preset.is_some();
    let header = subview_header(
        if has_preset {
            "Configure provider"
        } else {
            "Add from catalog"
        },
        if has_preset {
            None
        } else {
            Some("190+ providers from models.dev")
        },
        move |workspace, cx| {
            if has_preset {
                workspace.on_close_preset(cx);
            } else {
                workspace.on_show_models_subview(crate::view_model::ModelsSubview::List, cx);
            }
        },
        cx,
    );
    let theme = cx.theme();
    div()
        .id("models-catalog-page")
        .flex()
        .flex_col()
        .gap_3()
        .child(header)
        .when(workspace.vm().active_preset.is_none(), |this| {
            this.child(
                settings_card(
                    "catalog-search",
                    "Choose a provider",
                    Some("Filter by name or id, then pick a provider to configure."),
                    theme,
                    vec![
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(div().h(px(30.)).text_sm().child(Input::new(&preset_search)))
                            .child(preset_list)
                            .into_any_element(),
                    ],
                )
                .into_any_element(),
            )
        })
        .when(workspace.vm().active_preset.is_some(), |this| {
            this.child(preset_form_element)
        })
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn provider_row(
    index: usize,
    name: String,
    kind: String,
    host: String,
    models: usize,
    keyed: bool,
    enabled: bool,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    div()
        .id(format!("provider-row-{index}"))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
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
                        .child(format!("{kind} \u{b7} {host} \u{b7} {models} model(s)")),
                ),
        )
        .child(Icon::new(IconName::KeyRound).small().text_color(if keyed {
            theme.success
        } else {
            theme.danger
        }))
        .child(
            Switch::new(format!("provider-toggle-{index}"))
                .checked(enabled)
                .on_click(cx.listener(move |workspace, checked: &bool, _, cx| {
                    if let Some(provider) = workspace
                        .vm()
                        .settings
                        .as_ref()
                        .and_then(|s| s.providers.get(index))
                        .cloned()
                    {
                        let updated = mcode_config::ProviderSettings {
                            enabled: *checked,
                            ..provider
                        };
                        workspace.apply_action(
                            DesktopAction::SettingsProviderChanged(index, updated),
                            cx,
                        );
                    }
                })),
        )
        .child(super::icon_button(
            format!("provider-remove-{index}"),
            IconName::Trash,
            cx.listener(move |workspace, _, _, cx| {
                workspace.on_remove_provider(index, cx);
            }),
            cx,
        ))
        .into_any_element()
}

/// One visible row of the virtualized preset catalog list. Fixed height via
/// `PRESET_ROW_HEIGHT` keeps `uniform_list` measurements uniform.
fn preset_row(
    id: String,
    name: String,
    kind: String,
    models: usize,
    weak: &gpui_kit::WeakEntity<Workspace>,
    theme: &Theme,
) -> AnyElement {
    let weak = weak.clone();
    div()
        .id(format!("preset-row-{id}"))
        .h(PRESET_ROW_HEIGHT)
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .rounded_md()
        // NOTE: no `overflow_hidden` on the row — inside a scroll container it
        // collapses the flex_1 name column to zero width on real windows
        // (headless layout tests do not reproduce this; verified on screen).
        .hover(|this| this.bg(theme.secondary))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_sm().overflow_hidden().child(name))
                .child(
                    div()
                        .text_xs()
                        .opacity(0.6)
                        .overflow_hidden()
                        .child(format!("{id} · {kind} · {models} models")),
                ),
        )
        .child(
            Button::new(format!("preset-add-{id}"))
                .icon(IconName::Plus)
                .label("Add")
                .small()
                .outline()
                .on_click(move |_, _, cx| {
                    let _ = weak.update(cx, |workspace, cx| {
                        workspace.on_open_preset(&id, cx);
                    });
                }),
        )
        .into_any_element()
}

/// Fixed row height for the virtualized preset catalog list.
const PRESET_ROW_HEIGHT: gpui_kit::Pixels = px(48.);

/// One visible row of the virtualized preset model checklist.
fn preset_model_row(
    model: &str,
    selected: bool,
    weak: &gpui_kit::WeakEntity<Workspace>,
    theme: &Theme,
) -> AnyElement {
    let model_id = model.to_owned();
    let weak = weak.clone();
    div()
        .id(format!("preset-model-{model}"))
        .h(px(28.))
        .w_full()
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
        .on_click(move |_, _, cx| {
            let _ = weak.update(cx, |workspace, cx| {
                workspace.apply_action(DesktopAction::PresetModelToggled(model_id.clone()), cx);
            });
        })
        .child(model.to_owned())
        .when(selected, |this| {
            this.child(
                Icon::new(IconName::Check)
                    .xsmall()
                    .text_color(theme.primary),
            )
        })
        .into_any_element()
}

/// The expanded preset form: model picker plus the API key input.
fn render_preset_form(
    workspace: &mut Workspace,
    provider_id: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let key_input = workspace.preset_key_input(window, cx);
    let Some(catalog) = workspace.vm().catalog.clone() else {
        return div().into_any_element();
    };
    let Some(preset) = catalog.provider(provider_id) else {
        return div().into_any_element();
    };
    let name = preset.name.clone();
    let models: Vec<String> = preset.models.iter().map(|model| model.id.clone()).collect();
    let checked: Vec<String> = workspace.vm().preset_models.clone();
    let selection_label = if models.is_empty() {
        "no models".to_owned()
    } else {
        format!("{} of {} models", checked.len(), models.len())
    };
    let menu_open = workspace.vm().preset_model_menu_open;
    let provider_id_owned = provider_id.to_owned();
    let theme = cx.theme();
    div()
        .id("preset-form")
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme.primary)
        .bg(theme.secondary)
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(format!("Add {name}")),
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
                        .child("Models — all are selected by default; uncheck what you do not need"),
                )
                .child(
                    Button::new("preset-model-chip")
                        .label(selection_label)
                        .small()
                        .outline()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            let open = !workspace.vm().preset_model_menu_open;
                            workspace.apply_action(DesktopAction::PresetModelMenuToggled(open), cx);
                        })),
                )
                .when(menu_open, |this| {
                    // Lazy rows via `uniform_list` under a definite pixel
                    // height — same fix as the catalog provider list: only
                    // visible models are measured and painted per frame.
                    let weak = cx.weak_entity();
                    let list_models = models.clone();
                    let list_checked = checked.clone();
                    let list_height = px(
                        (list_models.len().clamp(1, 8) as f32) * 28. + 2.,
                    );
                    this.child(
                        div()
                            .id("preset-model-list")
                            .w_full()
                            .h(list_height)
                            .overflow_hidden()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background)
                            .child(gpui_kit::uniform_list(
                                "preset-model-rows",
                                list_models.len(),
                                move |range, _window, cx| {
                                    let theme = cx.theme().clone();
                                    range
                                        .map(|index| {
                                            let model = &list_models[index];
                                            preset_model_row(
                                                model,
                                                list_checked.contains(model),
                                                &weak,
                                                &theme,
                                            )
                                        })
                                        .collect()
                                },
                            )
                            .h_full()),
                    )
                }),
        )
        .when(preset.auth == mcode_catalog::AUTH_DEVICE_CODE, |this| {
            // OAuth sign-in replaces the pasted key for this provider.
            let theme = cx.theme();
            let sign_in = workspace.vm().copilot_sign_in.clone();
            let error = workspace.vm().copilot_error.clone();
            this.child(
                div()
                    .id("preset-sign-in")
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when_some(error, |this, message| {
                        this.child(
                            div()
                                .text_xs()
                                .p_2()
                                .rounded_md()
                                .bg(theme.danger.opacity(0.12))
                                .text_color(theme.danger)
                                .child(message),
                        )
                    })
                    .when_some(sign_in, |this, sign_in| {
                        this.child(
                            div()
                                .id("preset-sign-in-code")
                                .flex()
                                .flex_col()
                                .gap_1()
                                .p_2()
                                .rounded_md()
                                .border_1()
                                .border_color(skin::glass_border(theme))
                                .bg(skin::glass(theme))
                                .child(
                                    div().text_xs().opacity(0.7).child(
                                        "Your browser opened github.com/login/device — enter this code:",
                                    ),
                                )
                                .child(
                                    div()
                                        .text_xl()
                                        .font_family(theme.mono_font_family.clone())
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .child(sign_in.user_code),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .opacity(0.5)
                                        .child(format!("waiting at {}", sign_in.verification_uri)),
                                ),
                        )
                    })
                    .when(workspace.vm().copilot_sign_in.is_none(), |this| {
                        this.child(
                            Button::new("preset-sign-in-start")
                                .icon(IconName::Github)
                                .label("Sign in with GitHub")
                                .small()
                                .primary()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_start_copilot_sign_in(cx);
                                })),
                        )
                    })
                    .child(
                        Button::new("preset-cancel-oauth")
                            .label("Close")
                            .small()
                            .ghost()
                            .on_click(cx.listener(|workspace, _, _, cx| {
                                workspace.on_close_preset(cx);
                            })),
                    ),
            )
        })
        .when(preset.auth != mcode_catalog::AUTH_DEVICE_CODE, |this| {
            this.child(
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
        })
        .into_any_element()
}

// ---- MCP ----

fn render_mcp_section(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let Some(settings) = workspace.vm().settings.clone() else {
        return div().into_any_element();
    };
    let mcp_rows: Vec<(String, String, String, bool, bool, usize)> = settings
        .mcp_servers
        .iter()
        .enumerate()
        .map(|(index, server)| {
            (
                server.id.clone(),
                server.transport.clone(),
                server
                    .endpoint
                    .clone()
                    .or_else(|| server.command.clone())
                    .unwrap_or_default(),
                server.enabled,
                settings.mcp_with_keys.iter().any(|id| id == &server.id),
                index,
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
    // &mut Context work first.
    let catalog_row_elements: Vec<AnyElement> = catalog_rows
        .iter()
        .map(|(id, transport)| builtin_catalog_row(id, transport, cx))
        .collect();
    let mcp_form_element = render_mcp_form(workspace, window, cx);
    let key_input = workspace.mcp_key_input(window, cx);
    let mcp_row_elements: Vec<AnyElement> = mcp_rows
        .into_iter()
        .map(|(id, transport, endpoint, enabled, keyed, index)| {
            mcp_row(id, transport, endpoint, enabled, keyed, index, cx)
        })
        .collect();

    let theme = cx.theme();
    let mcp_empty = mcp_row_elements.is_empty();
    let has_catalog = !catalog_row_elements.is_empty();
    div()
        .id("mcp-section")
        .flex()
        .flex_col()
        .gap_3()
        .child(settings_card(
            "mcp",
            "MCP servers",
            Some(
                "Stdio or Streamable-HTTP tool servers. Enabled servers connect at \
                 the start of the next turn and their tools join the agent's toolset.",
            ),
            theme,
            vec![
                div()
                    .when(mcp_empty, |this| {
                        this.child(div().text_xs().opacity(0.5).child("No MCP servers yet"))
                    })
                    .children(mcp_row_elements)
                    .into_any_element(),
            ],
        ))
        .when(has_catalog, |this| {
            this.child(settings_card(
                "mcp-catalog",
                "Built-in catalog",
                Some("Paste a key for the server you need, then Add."),
                theme,
                vec![
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(catalog_row_elements)
                        .child(
                            div()
                                .id("mcp-key-row")
                                .flex()
                                .flex_col()
                                .gap_1()
                                .pt_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .opacity(0.6)
                                        .child("Key for the built-in server above"),
                                )
                                .child(div().h(px(28.)).text_sm().child(Input::new(&key_input))),
                        )
                        .into_any_element(),
                ],
            ))
        })
        .child(settings_card(
            "mcp-add",
            "Add custom server",
            Some("A stdio command or an https endpoint speaking Streamable HTTP."),
            theme,
            vec![mcp_form_element],
        ))
        .into_any_element()
}

fn mcp_row(
    id: String,
    transport: String,
    endpoint: String,
    enabled: bool,
    keyed: bool,
    index: usize,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    div()
        .id(format!("mcp-row-{id}"))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
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
                        .child(id.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .opacity(0.6)
                        .overflow_hidden()
                        .child(format!(
                            "{transport} \u{b7} {endpoint} \u{b7} {}",
                            if keyed { "key stored" } else { "no key" }
                        )),
                ),
        )
        .child(
            Switch::new(format!("mcp-toggle-{id}"))
                .checked(enabled)
                .on_click(cx.listener(move |workspace, checked: &bool, _, cx| {
                    workspace.apply_action(DesktopAction::SettingsMcpToggled(index, *checked), cx);
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
        .child(super::icon_button(
            format!("mcp-remove-{id}"),
            IconName::Trash,
            cx.listener(move |workspace, _, _, cx| {
                workspace.apply_action(DesktopAction::SettingsMcpRemoved(index), cx);
            }),
            cx,
        ))
        .into_any_element()
}

fn builtin_catalog_row(id: &str, transport: &str, cx: &mut Context<Workspace>) -> AnyElement {
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
                    let id = id.to_owned();
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
        .into_any_element()
}

// ---- Web ----

fn render_web_section(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let Some(settings) = workspace.vm().settings.clone() else {
        return div().into_any_element();
    };
    let backend_form_element = render_backend_form(workspace, window, cx);
    let backend_rows: Vec<(String, String, String, bool, usize)> = settings
        .web_backends
        .iter()
        .enumerate()
        .map(|(index, backend)| {
            (
                backend.id.clone(),
                backend.kind.clone(),
                backend.endpoint.clone(),
                backend.enabled,
                index,
            )
        })
        .collect();
    let backend_row_elements: Vec<AnyElement> = backend_rows
        .into_iter()
        .map(|(id, kind, endpoint, enabled, index)| {
            backend_row(id, kind, endpoint, enabled, index, cx)
        })
        .collect();
    let theme = cx.theme();
    let backends_empty = backend_row_elements.is_empty();
    div()
        .id("web-section")
        .flex()
        .flex_col()
        .gap_3()
        .child(settings_card(
            "web",
            "Web search",
            Some(
                "Querit works out of the box: the key comes from the QUERIT_API_KEY \
                 environment variable (or the vault entry web-<id>) and \
                 https://api.querit.ai is the built-in backend whenever no custom \
                 backend below is enabled.",
            ),
            theme,
            vec![
                div()
                    .when(backends_empty, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .opacity(0.5)
                                .child("Using the built-in Querit backend"),
                        )
                    })
                    .children(backend_row_elements)
                    .into_any_element(),
            ],
        ))
        .child(settings_card(
            "web-add",
            "Add custom backend",
            Some(
                "A Querit-compatible endpoint (POST /v1/search, POST /v1/contents) \
                 overrides the built-in default while enabled.",
            ),
            theme,
            vec![backend_form_element],
        ))
        .into_any_element()
}

fn backend_row(
    id: String,
    kind: String,
    endpoint: String,
    enabled: bool,
    index: usize,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    div()
        .id(format!("backend-row-{id}"))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
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
                        .child(id.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .opacity(0.6)
                        .overflow_hidden()
                        .child(format!("{kind} \u{b7} {endpoint}")),
                ),
        )
        .child(
            Switch::new(format!("backend-toggle-{id}"))
                .checked(enabled)
                .on_click(cx.listener(move |workspace, checked: &bool, _, cx| {
                    workspace
                        .apply_action(DesktopAction::SettingsBackendToggled(index, *checked), cx);
                })),
        )
        .child(super::icon_button(
            format!("backend-remove-{id}"),
            IconName::Trash,
            cx.listener(move |workspace, _, _, cx| {
                workspace.on_remove_backend(index, cx);
            }),
            cx,
        ))
        .into_any_element()
}

// ---- Data ----

fn render_data_section(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let usage_enabled = workspace
        .vm()
        .settings
        .as_ref()
        .map(|settings| settings.usage_enabled)
        .unwrap_or(true);
    let usage_row = settings_row(
        "usage",
        "Durable usage records",
        Some("Write a Usage event per completed turn, aggregated in the Overview panel."),
        Switch::new("settings-usage-toggle")
            .checked(usage_enabled)
            .on_click(cx.listener(|workspace, checked: &bool, _, cx| {
                workspace.apply_action(DesktopAction::SettingsUsageToggled(*checked), cx);
            }))
            .into_any_element(),
    );
    let theme = cx.theme();
    let transfer_row = div()
        .id("data-transfer")
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .pt_1()
        .child(
            Button::new("data-export")
                .icon(IconName::Download)
                .label("Export data\u{2026}")
                .small()
                .outline()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_export_data(cx);
                })),
        )
        .child(
            Button::new("data-import")
                .icon(IconName::Upload)
                .label("Import data\u{2026}")
                .small()
                .outline()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_import_data(cx);
                })),
        )
        .child(
            div()
                .text_xs()
                .opacity(0.55)
                .child("Settings, todos, and sessions — API keys stay on this machine."),
        )
        .into_any_element();
    settings_card(
        "data",
        "Data",
        Some("Usage records and moving your configuration between machines."),
        theme,
        vec![usage_row, transfer_row],
    )
    .into_any_element()
}

// ---- About ----

fn render_about_section(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let vm = workspace.vm();
    let current = mcode_updates::current_version().to_owned();
    let catalog_line = match vm.catalog_fetched_at {
        0 => "bundled snapshot".to_owned(),
        seconds => {
            let date = catalog_date(seconds);
            format!("cloud catalog \u{b7} fetched {date}")
        }
    };
    let auto_update = vm.auto_update;
    let status: (String, Option<AnyElement>) = match &vm.update {
        UpdateState::Idle => ("Update checks run at startup.".to_owned(), None),
        UpdateState::Checking => ("Checking for updates\u{2026}".to_owned(), None),
        UpdateState::UpToDate => (
            format!("v{current} is the latest version."),
            Some(
                div()
                    .text_xs()
                    .text_color(cx.theme().success)
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
        UpdateState::Downloading { .. } => ("Downloading and verifying\u{2026}".to_owned(), None),
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
                    .text_color(cx.theme().danger)
                    .child(ellipsis(message, 120))
                    .into_any_element(),
            ),
        ),
    };
    let rows = vec![
        settings_row(
            "version",
            "Current version",
            None,
            div()
                .text_sm()
                .opacity(0.8)
                .child(format!("v{current}"))
                .into_any_element(),
        ),
        settings_row(
            "auto-update",
            "Automatic checks",
            Some("Check GitHub for a newer release once a day."),
            Switch::new("update-auto-toggle")
                .checked(auto_update)
                .on_click(cx.listener(|workspace, checked: &bool, _, cx| {
                    workspace.on_toggle_auto_update(*checked, cx);
                }))
                .into_any_element(),
        ),
        settings_row(
            "update-status",
            "Status",
            None,
            div()
                .text_xs()
                .opacity(0.7)
                .child(status.0)
                .into_any_element(),
        ),
        status
            .1
            .map(|node| {
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .child(node)
                    .into_any_element()
            })
            .unwrap_or_else(|| div().into_any_element()),
        settings_row(
            "catalog",
            "Provider catalog",
            None,
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
                )
                .into_any_element(),
        ),
    ];
    let theme = cx.theme();
    settings_card(
        "about",
        "About",
        Some("The app updates itself from GitHub releases."),
        theme,
        rows,
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

fn labeled_field(label: &str, input: Entity<InputState>) -> impl IntoElement {
    div()
        .id(format!("form-field-{label}"))
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_xs().opacity(0.6).child(label.to_owned()))
        .child(div().h(px(28.)).child(Input::new(&input)))
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
) -> AnyElement {
    let form = workspace.backend_form(window, cx);
    let theme = cx.theme();
    div()
        .id("backend-form")
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_md()
        .bg(theme.secondary)
        .child(div().text_xs().opacity(0.7).child("Add web search backend"))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .text_sm()
                .child(labeled_field("id", form.read(cx).id.clone()))
                .child(labeled_field("kind", form.read(cx).kind.clone()))
                .child(labeled_field("endpoint", form.read(cx).endpoint.clone())),
        )
        .child(
            Button::new("backend-add")
                .label("Add backend")
                .small()
                .outline()
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
    /// Selected transport (`http` or `stdio`).
    pub transport: String,
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
        let endpoint = make("https://mcp.example.com/mcp");
        let command = make("stdio: command (e.g. npx)");
        let api_key = make("api key (leave empty to skip)");
        cx.new(|_| Self {
            id,
            transport: "http".to_owned(),
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
) -> AnyElement {
    let form = workspace.mcp_form(window, cx);
    let transport = form.read(cx).transport.clone();
    let transport_menu_open = workspace.vm().mcp_transport_menu_open;
    let transport_field = dropdown_field(
        "mcp-transport",
        "Transport",
        Some("HTTP servers speak Streamable-HTTP; stdio servers spawn a command."),
        &transport,
        &["http", "stdio"],
        transport_menu_open,
        |workspace, open, cx| workspace.on_toggle_mcp_transport_menu(open, cx),
        |workspace, transport, cx| workspace.on_select_mcp_transport(transport, cx),
        cx,
    );
    let theme = cx.theme();
    div()
        .id("mcp-form")
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_md()
        .bg(theme.secondary)
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
                .gap_2()
                .text_sm()
                .child(labeled_field("id", form.read(cx).id.clone()))
                .child(transport_field)
                .child(labeled_field(
                    "endpoint (http)",
                    form.read(cx).endpoint.clone(),
                ))
                .child(labeled_field(
                    "command (stdio)",
                    form.read(cx).command.clone(),
                ))
                .child(labeled_field(
                    "api key (stored in secrets.json)",
                    form.read(cx).api_key.clone(),
                )),
        )
        .child(
            Button::new("mcp-add")
                .label("Add server")
                .small()
                .outline()
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_add_mcp(cx);
                })),
        )
        .into_any_element()
}

/// Inline custom-endpoint provider form state.
pub(crate) struct ProviderForm {
    /// Provider identity input.
    pub id: Entity<InputState>,
    /// Selected wire protocol (`anthropic-messages`, `openai-completions`,
    /// `openai-responses`).
    pub kind: String,
    /// Base URL input.
    pub base_url: Entity<InputState>,
    /// Default model input.
    pub model: Entity<InputState>,
    /// Optional context window override (tokens).
    pub context_limit: Entity<InputState>,
    /// Optional max output override (tokens).
    pub max_output: Entity<InputState>,
    /// API key input; stored in the secret store, never in settings.
    pub api_key: Entity<InputState>,
}

impl ProviderForm {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Entity<Self> {
        let mut make = |placeholder: &'static str| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let id = make("id, e.g. openai-main");
        let base_url = make("https://api.example.com/v1");
        let model = make("model id");
        let context_limit = make("optional, e.g. 200000");
        let max_output = make("optional, e.g. 8192");
        let api_key = make("api key (leave empty to skip)");
        cx.new(|_| Self {
            id,
            kind: "openai-completions".to_owned(),
            base_url,
            model,
            context_limit,
            max_output,
            api_key,
        })
    }
}

/// A dropdown row: label on the left, a button showing the current value on
/// the right, and an inline option list that opens below the row.
#[allow(clippy::too_many_arguments)]
fn dropdown_field(
    id: &str,
    label: &str,
    description: Option<&str>,
    current: &str,
    options: &[&'static str],
    open: bool,
    on_toggle: impl Fn(&mut Workspace, bool, &mut Context<Workspace>) + Copy + 'static,
    on_pick: impl Fn(&mut Workspace, &str, &mut Context<Workspace>) + Copy + 'static,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    div()
        .id(format!("dropdown-{id}"))
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .min_w_0()
                        .child(div().text_sm().child(label.to_owned()))
                        .when_some(description, |this, description| {
                            this.child(div().text_xs().opacity(0.5).child(description.to_owned()))
                        }),
                )
                .child(
                    Button::new(format!("dropdown-button-{id}"))
                        .label(current.to_owned())
                        .icon(IconName::ChevronDown)
                        .small()
                        .outline()
                        .on_click(cx.listener(move |workspace, _, _, cx| {
                            on_toggle(workspace, !open, cx);
                        })),
                ),
        )
        .when(open, |this| {
            this.child(
                div()
                    .id(format!("dropdown-list-{id}"))
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .p_1()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .children(options.iter().map(|option| {
                        let option = *option;
                        let selected = option == current;
                        div()
                            .id(format!("dropdown-{id}-{option}"))
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .px_2()
                            .h(px(28.))
                            .rounded_md()
                            .text_sm()
                            .cursor_pointer()
                            .hover(|this| this.bg(theme.secondary))
                            .on_click(cx.listener(move |workspace, _, _, cx| {
                                on_pick(workspace, option, cx);
                            }))
                            .child(option)
                            .when(selected, |this| {
                                this.child(
                                    Icon::new(IconName::Check)
                                        .xsmall()
                                        .text_color(theme.primary),
                                )
                            })
                    })),
            )
        })
        .into_any_element()
}

/// The custom-endpoint page: protocol dropdown, endpoint identity, and the
/// optional model parameter overrides.
fn render_custom_provider_page(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let form = workspace.provider_form(window, cx);
    let kind = form.read(cx).kind.clone();
    let kind_menu_open = workspace.vm().provider_kind_menu_open;
    let (context_input, max_output_input, base_url_input, model_input, id_input, api_key_input) = {
        let read = form.read(cx);
        (
            read.context_limit.clone(),
            read.max_output.clone(),
            read.base_url.clone(),
            read.model.clone(),
            read.id.clone(),
            read.api_key.clone(),
        )
    };
    let kind_field = dropdown_field(
        "provider-kind",
        "Protocol",
        Some("Wire protocol the endpoint speaks."),
        &kind,
        &[
            "anthropic-messages",
            "openai-completions",
            "openai-responses",
        ],
        kind_menu_open,
        |workspace, open, cx| workspace.on_toggle_provider_kind_menu(open, cx),
        |workspace, kind, cx| workspace.on_select_provider_kind(kind, cx),
        cx,
    );
    let header = subview_header(
        "Add custom endpoint",
        Some("Any endpoint speaking one of the three wire protocols."),
        |workspace, cx| {
            workspace.on_show_models_subview(crate::view_model::ModelsSubview::List, cx);
        },
        cx,
    );
    let theme = cx.theme();
    div()
        .id("custom-provider-page")
        .flex()
        .flex_col()
        .gap_3()
        .child(header)
        .child(
            settings_card(
                "custom-provider",
                "Endpoint",
                Some("The provider appears in the model picker as soon as it is added."),
                theme,
                vec![
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_sm()
                        .child(labeled_field("id", id_input))
                        .child(kind_field)
                        .child(labeled_field("base URL", base_url_input))
                        .child(labeled_field("default model", model_input))
                        .into_any_element(),
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_sm()
                        .child(labeled_field("context window (optional)", context_input))
                        .child(labeled_field(
                            "max output tokens (optional)",
                            max_output_input,
                        ))
                        .into_any_element(),
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_sm()
                        .child(labeled_field(
                            "api key (stored in secrets.json)",
                            api_key_input,
                        ))
                        .into_any_element(),
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(
                            Button::new("provider-add")
                                .icon(IconName::Check)
                                .label("Add provider")
                                .small()
                                .primary()
                                .on_click(cx.listener(|workspace, _, _, cx| {
                                    workspace.on_add_provider(cx);
                                })),
                        )
                        .into_any_element(),
                ],
            )
            .into_any_element(),
        )
        .into_any_element()
}
