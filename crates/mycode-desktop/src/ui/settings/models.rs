//! The Models settings pages: the configured provider list, the models.dev
//! catalog picker with its preset form, and the custom-endpoint form.
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use super::widgets::{dropdown_field, labeled_field, row_header, settings_card};
use crate::ui::skin;
use crate::view_model::DesktopAction;
use crate::workspace::Workspace;

pub(super) fn render_models_section(
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
    let list_height = px((preset_rows.len().clamp(1, 9) as f32) * PRESET_ROW_HEIGHT.as_f32() + 2.);
    let list_rows = preset_rows.clone();
    let preset_list = div()
        .id("preset-catalog-list")
        .w_full()
        .h(list_height)
        .overflow_hidden()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .child(
            gpui_kit::uniform_list(
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
            .h_full(),
        );

    let has_preset = workspace.vm().active_preset.is_some();
    let header = super::subview_header(
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
        .child(row_header(
            &name,
            format!("{kind} \u{b7} {host} \u{b7} {models} model(s)"),
        ))
        .child(Icon::new(IconName::KeyRound).small().text_color(if keyed {
            theme.success
        } else {
            theme.danger
        }))
        .child(
            Switch::new(format!("provider-toggle-{index}"))
                .checked(enabled)
                .on_click(cx.listener(move |workspace, checked: &bool, _, cx| {
                    workspace
                        .apply_action(DesktopAction::SettingsProviderToggled(index, *checked), cx);
                })),
        )
        .child(crate::ui::icon_button(
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
        .child(row_header(
            &name,
            format!("{id} · {kind} · {models} models"),
        ))
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
                    div().text_xs().opacity(0.6).child(
                        "Models — all are selected by default; uncheck what you do not need",
                    ),
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
                    let list_height = px((list_models.len().clamp(1, 8) as f32) * 28. + 2.);
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
                            .child(
                                gpui_kit::uniform_list(
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
                                .h_full(),
                            ),
                    )
                }),
        )
        .when(
            mycode_providers::catalog::uses_oauth_login(&preset.auth),
            |this| {
                let theme = cx.theme();
                let sign_in = workspace.vm().copilot_sign_in.clone();
                let error = workspace.vm().copilot_error.clone();
                let sign_label = match preset.id.as_str() {
                    "xai" => "Sign in with SuperGrok / X",
                    "openai-codex" => "Sign in with ChatGPT",
                    _ => "Sign in with GitHub",
                };
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
                                    .child(div().text_xs().opacity(0.7).child(format!(
                                        "Open {} and enter this code:",
                                        sign_in.verification_uri
                                    )))
                                    .child(
                                        div()
                                            .text_xl()
                                            .font_family(theme.mono_font_family.clone())
                                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                            .child(sign_in.user_code),
                                    ),
                            )
                        })
                        .when(workspace.vm().copilot_sign_in.is_none(), |this| {
                            this.child(
                                Button::new("preset-sign-in-start")
                                    .label(sign_label)
                                    .small()
                                    .primary()
                                    .on_click(cx.listener(|workspace, _, _, cx| {
                                        workspace.on_start_oauth_sign_in(cx);
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
            },
        )
        .when(
            preset.auth != mycode_providers::catalog::AUTH_DEVICE_CODE,
            |this| {
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
            },
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
    let kind_options = [
        "anthropic-messages",
        "openai-completions",
        "openai-responses",
    ]
    .iter()
    .map(|kind| (*kind).to_owned())
    .collect::<Vec<_>>();
    let kind_field = dropdown_field(
        "provider-kind",
        "Protocol",
        Some("Wire protocol the endpoint speaks."),
        &kind,
        &kind_options,
        kind_menu_open,
        |workspace, open, cx| workspace.on_toggle_provider_kind_menu(open, cx),
        |workspace, kind, cx| workspace.on_select_provider_kind(kind, cx),
        cx,
    );
    let header = super::subview_header(
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
