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
use gpui_kit::component::input::{Input, InputState, Textarea};
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
    let nav = render_settings_nav(workspace, section, cx).into_any_element();
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
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
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(gpui_kit::FontWeight::BOLD)
                                        .child("Settings"),
                                )
                                .child(div().text_sm().text_color(desk.amber).child("//"))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child(section.label()),
                                ),
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
                .child(nav)
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
                                        SettingsSection::Agents => {
                                            render_agents_section(workspace, cx)
                                        }
                                        SettingsSection::Skills => {
                                            render_skills_section(workspace, cx)
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

/// What the nav shows beside each section: a count or a status lamp.
#[derive(Clone, Copy, Default)]
struct NavBadge {
    /// Right-aligned mono figure (configured providers, enabled servers).
    count: Option<usize>,
    /// Attention lamp (an update is waiting, a provider has no key).
    lamp: Option<gpui_kit::Hsla>,
}

/// Per-section badges derived from live state.
fn nav_badges(workspace: &Workspace, cx: &Context<Workspace>) -> Vec<(SettingsSection, NavBadge)> {
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    let vm = workspace.vm();
    let settings = vm.settings.as_ref();
    let providers = settings.map(|s| s.providers.iter().filter(|p| p.enabled).count());
    let servers = settings.map(|s| s.mcp_servers.iter().filter(|m| m.enabled).count());
    let backends = settings.map(|s| s.web_backends.iter().filter(|b| b.enabled).count());
    let missing_key = settings.is_some_and(|s| {
        s.providers
            .iter()
            .any(|p| p.enabled && !s.providers_with_keys.contains(&p.id))
    });
    let update_lamp = match vm.update {
        UpdateState::Available { .. } | UpdateState::Ready { .. } => Some(desk.amber),
        UpdateState::Failed(_) => Some(desk.red),
        _ => None,
    };
    vec![
        (SettingsSection::General, NavBadge::default()),
        (
            SettingsSection::Models,
            NavBadge {
                count: providers,
                lamp: missing_key.then_some(desk.red),
            },
        ),
        (
            SettingsSection::Agents,
            NavBadge {
                count: settings.map(|s| {
                    mycode_config::builtin_roles()
                        .roles
                        .iter()
                        .filter(|role| s.subagents.is_enabled(&role.name))
                        .count()
                }),
                lamp: None,
            },
        ),
        (
            SettingsSection::Skills,
            NavBadge {
                count: Some(vm.skills.len()),
                lamp: None,
            },
        ),
        (
            SettingsSection::Mcp,
            NavBadge {
                count: servers,
                lamp: None,
            },
        ),
        (
            SettingsSection::Web,
            NavBadge {
                count: backends,
                lamp: None,
            },
        ),
        (SettingsSection::Data, NavBadge::default()),
        (
            SettingsSection::About,
            NavBadge {
                count: None,
                lamp: update_lamp,
            },
        ),
    ]
}

/// The settings secondary menu in the Desk look: a mono "SETTINGS" pane
/// head, grouped rows (WORKSPACE / CONNECT / SYSTEM) with an index, icon,
/// label and hint, live count badges, and an amber left rail on the
/// selected row.
fn render_settings_nav(
    workspace: &Workspace,
    section: SettingsSection,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let badges = nav_badges(workspace, cx);
    let dirty = workspace.vm().settings.as_ref().is_some_and(|s| s.dirty);
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    let mut index = 0usize;
    let mut groups: Vec<AnyElement> = Vec::new();
    for (group, members) in SettingsSection::GROUPS {
        let mut rows: Vec<AnyElement> = Vec::new();
        for candidate in members.iter().copied() {
            index += 1;
            let badge = badges
                .iter()
                .find(|(s, _)| *s == candidate)
                .map(|(_, badge)| *badge)
                .unwrap_or_default();
            rows.push(nav_row(
                candidate,
                candidate == section,
                index,
                badge,
                theme,
                &desk,
                cx,
            ));
        }
        groups.push(
            div()
                .id(format!("settings-nav-group-{group}"))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .px_2()
                        .pt(px(8.))
                        .pb(px(2.))
                        .text_xs()
                        .text_color(desk.faint)
                        .child(*group),
                )
                .children(rows)
                .into_any_element(),
        );
    }
    div()
        .id("settings-nav")
        .w(px(212.))
        .h_full()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .border_r_1()
        .border_color(theme.border)
        .bg(theme.sidebar)
        .child(super::sidebar::pane_head(
            "SETTINGS",
            Some(&format!("{index:02}")),
            desk.faint,
            theme,
        ))
        .child(
            div()
                .id("settings-nav-groups")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .px_2()
                .pb_2()
                .children(groups),
        )
        .child(
            div()
                .id("settings-nav-footer")
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(theme.border)
                .text_xs()
                .text_color(desk.faint)
                .child(format!("v{}", env!("CARGO_PKG_VERSION")))
                .when(dirty, |this| {
                    this.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .text_color(desk.amber)
                            .child(super::lamp(desk.amber))
                            .child("UNSAVED"),
                    )
                }),
        )
}

/// One nav row: index, icon, label + hint, and the badge column.
#[allow(clippy::too_many_arguments)]
fn nav_row(
    candidate: SettingsSection,
    selected: bool,
    index: usize,
    badge: NavBadge,
    theme: &Theme,
    desk: &super::desk::Desk,
    cx: &Context<Workspace>,
) -> AnyElement {
    let ink = if selected {
        theme.sidebar_accent_foreground
    } else {
        theme.sidebar_foreground
    };
    div()
        .id(format!("settings-nav-{}", candidate.id()))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .pl(px(6.))
        .pr_2()
        .py(px(4.))
        .rounded(px(3.))
        .border_l_2()
        .border_color(theme.transparent)
        .cursor_pointer()
        .when(selected, |this| {
            this.bg(theme.sidebar_accent).border_color(desk.amber)
        })
        .hover(|this| this.bg(theme.sidebar_accent))
        .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.on_show_settings_section(candidate, cx);
        }))
        .child(
            div()
                .w(px(16.))
                .text_xs()
                .text_color(if selected { desk.amber } else { desk.faint })
                .child(format!("{index:02}")),
        )
        .child(
            Icon::new(candidate.icon())
                .with_size(px(14.))
                .text_color(if selected {
                    desk.amber
                } else {
                    theme.muted_foreground
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_sm()
                        .text_color(ink)
                        .when(selected, |this| {
                            this.font_weight(gpui_kit::FontWeight::MEDIUM)
                        })
                        .child(candidate.label()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(desk.faint)
                        .overflow_hidden()
                        .child(candidate.hint()),
                ),
        )
        .when_some(badge.lamp, |this, color| this.child(super::lamp(color)))
        .when_some(badge.count, |this, count| {
            this.child(
                div()
                    .min_w(px(18.))
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(3.))
                    .border_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(if count > 0 { ink } else { desk.faint })
                    .child(count.to_string()),
            )
        })
        .into_any_element()
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
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(super::desk::Desk::of(theme).faint)
                        .child(title.to_owned()),
                )
                .when_some(hint, |this, hint| {
                    this.child(
                        div()
                            .text_xs()
                            .opacity(0.5)
                            .whitespace_normal()
                            .child(hint.to_owned()),
                    )
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
        .items_start()
        .justify_between()
        .gap_4()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().whitespace_normal().child(label.to_owned()))
                .when_some(description, |this, description| {
                    this.child(
                        div()
                            .text_xs()
                            .opacity(0.5)
                            .whitespace_normal()
                            .child(description.to_owned()),
                    )
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
                        let updated = mycode_config::ProviderSettings {
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
        .when(preset.auth == mycode_providers::catalog::AUTH_DEVICE_CODE, |this| {
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
        .when(preset.auth != mycode_providers::catalog::AUTH_DEVICE_CODE, |this| {
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
    let listed_tools = workspace.vm().mcp_tools.clone();
    let probing = workspace.vm().mcp_probing.clone();
    let mcp_rows: Vec<McpRow> = settings
        .mcp_servers
        .iter()
        .enumerate()
        .map(|(index, server)| {
            let target = match server.transport.as_str() {
                "stdio" => {
                    let mut line = server.command.clone().unwrap_or_default();
                    for arg in &server.args {
                        line.push(' ');
                        line.push_str(arg);
                    }
                    line
                }
                _ => server.endpoint.clone().unwrap_or_default(),
            };
            McpRow {
                id: server.id.clone(),
                transport: server.transport.clone(),
                target,
                enabled: server.enabled,
                keyed: settings.mcp_with_keys.iter().any(|id| id == &server.id),
                index,
                tools: listed_tools
                    .iter()
                    .find(|(id, _)| *id == server.id)
                    .map(|(_, tools)| tools.clone()),
                probing: probing.contains(&server.id),
            }
        })
        .collect();
    let builtin_servers = mycode_config::builtin_mcp_servers();
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
    let json_input = workspace.mcp_json_input(window, cx);
    let mcp_row_elements: Vec<AnyElement> =
        mcp_rows.into_iter().map(|row| mcp_row(row, cx)).collect();

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
                "Stdio or Streamable-HTTP tool servers. Enabled servers connect once, \
                 stay connected across turns, and their tools join the agent's toolset. \
                 Use Probe to connect now and see the tools a server offers.",
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
            "mcp-import",
            "Import JSON",
            Some(
                "Paste a Claude Desktop / Cursor mcp.json, a servers map, or one server object. \
                 Authorization headers are stored as the API key; Bearer is added on the wire.",
            ),
            theme,
            vec![
                div()
                    .min_h(px(96.))
                    .w_full()
                    .child(Textarea::new(&json_input))
                    .into_any_element(),
                Button::new("mcp-import-json")
                    .label("Import pasted JSON")
                    .small()
                    .outline()
                    .on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_import_mcp_json(cx);
                    }))
                    .into_any_element(),
            ],
        ))
        .child(settings_card(
            "mcp-add",
            "Add custom server",
            Some("A stdio command or an https endpoint speaking Streamable HTTP."),
            theme,
            vec![mcp_form_element],
        ))
        .into_any_element()
}

/// One configured MCP server as the settings row shows it.
struct McpRow {
    id: String,
    transport: String,
    /// Endpoint URL or the full stdio command line.
    target: String,
    enabled: bool,
    keyed: bool,
    index: usize,
    /// Tool names from the last probe, when one succeeded.
    tools: Option<Vec<String>>,
    probing: bool,
}

fn mcp_row(row: McpRow, cx: &Context<Workspace>) -> AnyElement {
    let McpRow {
        id,
        transport,
        target,
        enabled,
        keyed,
        index,
        tools,
        probing,
    } = row;
    let theme = cx.theme();
    let desk = super::desk::Desk::of(theme);
    let key_note = match (transport.as_str(), keyed) {
        ("http", true) => " \u{b7} key stored",
        ("http", false) => " \u{b7} no key",
        _ => "",
    };
    let tool_chips: Vec<AnyElement> = tools
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|tool| {
            div()
                .px(px(6.))
                .py(px(1.))
                .rounded(px(3.))
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .child(tool.clone())
                .into_any_element()
        })
        .collect();
    let tools_summary: Option<String> = tools.as_ref().map(|tools| {
        if tools.is_empty() {
            "connected \u{b7} no tools advertised".to_owned()
        } else {
            format!("connected \u{b7} {} tool(s)", tools.len())
        }
    });
    div()
        .id(format!("mcp-row-{id}"))
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(super::lamp(if tools.is_some() {
                    desk.green
                } else if enabled {
                    desk.amber
                } else {
                    desk.faint
                }))
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
                                .whitespace_normal()
                                .child(format!("{transport} \u{b7} {target}{key_note}")),
                        )
                        .when_some(tools_summary, |this, summary| {
                            this.child(div().text_xs().text_color(desk.green).child(summary))
                        }),
                )
                .child(
                    Switch::new(format!("mcp-toggle-{id}"))
                        .checked(enabled)
                        .on_click(cx.listener(move |workspace, checked: &bool, _, cx| {
                            workspace.apply_action(
                                DesktopAction::SettingsMcpToggled(index, *checked),
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new(format!("mcp-tools-{id}"))
                        .label(if probing {
                            "Connecting\u{2026}"
                        } else {
                            "Probe"
                        })
                        .small()
                        .ghost()
                        .disabled(probing)
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
                )),
        )
        .when(!tool_chips.is_empty(), |this| {
            this.child(
                div()
                    .id(format!("mcp-tools-list-{id}"))
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_1()
                    .pl(px(19.))
                    .children(tool_chips),
            )
        })
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
                        let server = mycode_config::builtin_mcp_servers()
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

// ---- Agents ----

fn render_agents_section(workspace: &Workspace, cx: &Context<Workspace>) -> AnyElement {
    let Some(settings) = workspace.vm().settings.clone() else {
        return div().into_any_element();
    };
    let catalog = mycode_config::builtin_roles();
    let providers: Vec<(String, Vec<String>)> = settings
        .providers
        .iter()
        .filter(|provider| provider.enabled)
        .map(|provider| (provider.id.clone(), provider.models.clone()))
        .collect();
    let max_concurrent = settings.subagents.max_concurrent;
    let role_cards: Vec<AnyElement> = catalog
        .roles
        .iter()
        .map(|role| {
            let entry = settings.subagents.role(&role.name);
            let enabled = entry.is_none_or(|item| item.enabled);
            let thinking = entry
                .and_then(|item| item.thinking.clone())
                .unwrap_or_else(|| "inherit".to_owned());
            let provider = entry
                .and_then(|item| item.provider.clone())
                .unwrap_or_else(|| "inherit".to_owned());
            let model = entry
                .and_then(|item| item.model.clone())
                .unwrap_or_else(|| "inherit".to_owned());
            agent_role_card(
                role.name.clone(),
                role.description.clone(),
                role.isolation.as_str(),
                role.origin.as_str(),
                enabled,
                thinking,
                provider,
                model,
                providers.clone(),
                cx,
            )
        })
        .collect();
    let theme = cx.theme();
    div()
        .id("agents-section")
        .flex()
        .flex_col()
        .gap_3()
        .child(settings_card(
            "agents-capacity",
            "Delegation",
            Some(
                "The parent model may hand work to these roles. Inherit uses the \
                 session provider and the role's own thinking level. 0 concurrent \
                 slots means automatic capacity.",
            ),
            theme,
            vec![settings_row(
                "agents-concurrent",
                "Max concurrent",
                Some("0 = automatic"),
                Button::new("agents-concurrent-cycle")
                    .label(if max_concurrent == 0 {
                        "auto".to_owned()
                    } else {
                        max_concurrent.to_string()
                    })
                    .small()
                    .outline()
                    .on_click(cx.listener(move |workspace, _, _, cx| {
                        let Some(settings) = workspace.vm().settings.clone() else {
                            return;
                        };
                        let mut next = settings.subagents;
                        next.max_concurrent = if next.max_concurrent >= mycode_config::MAX_SUBAGENT_CONCURRENCY
                        {
                            0
                        } else {
                            next.max_concurrent + 1
                        };
                        workspace.apply_action(DesktopAction::SettingsSubagentsChanged(next), cx);
                    }))
                    .into_any_element(),
            )],
        ))
        .child(settings_card(
            "agents-roles",
            "Roles",
            Some("Scout is read-only. Artisan writes in a worktree. Steward cleans up. Sentinel reviews."),
            theme,
            role_cards,
        ))
        .into_any_element()
}

fn render_skills_section(workspace: &Workspace, cx: &Context<Workspace>) -> AnyElement {
    let theme = cx.theme();
    let skills = workspace.vm().skills.clone();
    let rows: Vec<AnyElement> = if skills.is_empty() {
        vec![
            div()
                .text_xs()
                .opacity(0.5)
                .whitespace_normal()
                .child(
                    "No skills yet. Add SKILL.md under the project .agents/skills/ \
                     folder or ~/.agents/skills/.",
                )
                .into_any_element(),
        ]
    } else {
        skills
            .into_iter()
            .map(|skill| {
                let slug = skill.slug.clone();
                let scope = if skill.global { "user" } else { "workspace" };
                div()
                    .id(format!("skill-row-{}", skill.slug))
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .p_3()
                    .rounded(px(3.))
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(format!("/{}", skill.slug)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .opacity(0.7)
                                    .whitespace_normal()
                                    .child(skill.title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .opacity(0.45)
                                    .whitespace_normal()
                                    .child(format!("{scope} · {}", skill.path)),
                            ),
                    )
                    .child(
                        Button::new(format!("skill-use-{}", skill.slug))
                            .label("Use")
                            .small()
                            .outline()
                            .on_click(cx.listener(move |workspace, _, _, cx| {
                                workspace.on_use_skill(&slug, cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect()
    };
    div()
        .id("skills-section")
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    Button::new("skills-refresh")
                        .icon(IconName::RefreshCw)
                        .label("Refresh")
                        .small()
                        .outline()
                        .on_click(cx.listener(|workspace, _, _, cx| {
                            workspace.on_refresh_skills(cx);
                        })),
                ),
        )
        .child(settings_card(
            "skills-catalog",
            "Slash commands",
            Some(
                "Type / in the composer to insert a skill. Workspace \
                 .agents/skills win over the same slug in ~/.agents.",
            ),
            theme,
            rows,
        ))
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn agent_role_card(
    name: String,
    description: String,
    isolation: &'static str,
    origin: &'static str,
    enabled: bool,
    thinking: String,
    provider: String,
    model: String,
    providers: Vec<(String, Vec<String>)>,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    let role = name.clone();
    let thinking_label = thinking.clone();
    let provider_label = provider.clone();
    let model_label = model.clone();
    let provider_ids: Vec<String> = providers.iter().map(|(id, _)| id.clone()).collect();
    let models_for_provider: Vec<String> = providers
        .iter()
        .find(|(id, _)| *id == provider)
        .map(|(_, models)| models.clone())
        .unwrap_or_default();
    div()
        .id(format!("agent-role-{name}"))
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(px(3.))
        .border_1()
        .border_color(theme.border)
        .child(
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .child(name.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .opacity(0.6)
                                .whitespace_normal()
                                .child(description),
                        )
                        .child(
                            div()
                                .text_xs()
                                .opacity(0.45)
                                .child(format!("{origin} \u{b7} {isolation}")),
                        ),
                )
                .child(
                    Switch::new(format!("agent-enabled-{name}"))
                        .checked(enabled)
                        .on_click({
                            let role = role.clone();
                            cx.listener(move |workspace, checked: &bool, _, cx| {
                                workspace.on_subagent_role_enabled(&role, *checked, cx);
                            })
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap_2()
                .child(
                    Button::new(format!("agent-thinking-{name}"))
                        .label(format!("think: {thinking_label}"))
                        .small()
                        .outline()
                        .on_click({
                            let role = role.clone();
                            cx.listener(move |workspace, _, _, cx| {
                                workspace.on_cycle_subagent_thinking(&role, cx);
                            })
                        }),
                )
                .child(
                    Button::new(format!("agent-provider-{name}"))
                        .label(format!("provider: {provider_label}"))
                        .small()
                        .outline()
                        .on_click({
                            let role = role.clone();
                            let ids = provider_ids.clone();
                            cx.listener(move |workspace, _, _, cx| {
                                workspace.on_cycle_subagent_provider(&role, &ids, cx);
                            })
                        }),
                )
                .child(
                    Button::new(format!("agent-model-{name}"))
                        .label(format!("model: {model_label}"))
                        .small()
                        .outline()
                        .on_click({
                            let role = role.clone();
                            let models = models_for_provider.clone();
                            cx.listener(move |workspace, _, _, cx| {
                                workspace.on_cycle_subagent_model(&role, &models, cx);
                            })
                        }),
                ),
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
    let keyed = settings.providers_with_keys.clone();
    let builtin_ids: Vec<String> = mycode_config::builtin_web_backends()
        .into_iter()
        .map(|backend| backend.id)
        .collect();
    let vendor_rows: Vec<AnyElement> = settings
        .web_backends
        .iter()
        .enumerate()
        .filter(|(_, backend)| builtin_ids.contains(&backend.id))
        .map(|(index, backend)| {
            let key_input = workspace.web_key_input(&backend.id, window, cx);
            vendor_backend_row(
                VendorBackend {
                    id: backend.id.clone(),
                    kind: backend.kind.clone(),
                    endpoint: backend.endpoint.clone(),
                    enabled: backend.enabled,
                    keyed: keyed.iter().any(|id| id == &format!("web-{}", backend.id)),
                    index,
                },
                key_input,
                cx,
            )
        })
        .collect();
    let custom_rows: Vec<AnyElement> = settings
        .web_backends
        .iter()
        .enumerate()
        .filter(|(_, backend)| !builtin_ids.contains(&backend.id))
        .map(|(index, backend)| {
            backend_row(
                backend.id.clone(),
                backend.kind.clone(),
                backend.endpoint.clone(),
                backend.enabled,
                index,
                cx,
            )
        })
        .collect();
    let backend_form_element = render_backend_form(workspace, window, cx);
    let theme = cx.theme();
    let custom_empty = custom_rows.is_empty();
    div()
        .id("web-section")
        .flex()
        .flex_col()
        .gap_3()
        .child(settings_card(
            "web",
            "Web search",
            Some(
                "Querit and AnySearch are ready: paste the API key and turn one on. \
                 Authorization is sent as Bearer automatically — do not type Bearer yourself. \
                 Environment variables QUERIT_API_KEY / ANYSEARCH_API_KEY also work.",
            ),
            theme,
            vendor_rows,
        ))
        .child(settings_card(
            "web-custom",
            "Custom backends",
            Some(
                "A Querit-compatible endpoint (POST /v1/search, POST /v1/contents) \
                 or another AnySearch-compatible host.",
            ),
            theme,
            vec![
                div()
                    .when(custom_empty, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .opacity(0.5)
                                .whitespace_normal()
                                .child("No custom backends"),
                        )
                    })
                    .children(custom_rows)
                    .into_any_element(),
                backend_form_element,
            ],
        ))
        .into_any_element()
}

struct VendorBackend {
    id: String,
    kind: String,
    endpoint: String,
    enabled: bool,
    keyed: bool,
    index: usize,
}

fn vendor_backend_row(
    backend: VendorBackend,
    key_input: Entity<InputState>,
    cx: &Context<Workspace>,
) -> AnyElement {
    let VendorBackend {
        id,
        kind,
        endpoint,
        enabled,
        keyed,
        index,
    } = backend;
    let theme = cx.theme();
    let title = match kind.as_str() {
        "querit" => "Querit",
        "anysearch" => "AnySearch",
        other => other,
    };
    div()
        .id(format!("vendor-backend-{id}"))
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(px(3.))
        .border_1()
        .border_color(theme.border)
        .child(
            div()
                .w_full()
                .flex()
                .flex_row()
                .items_start()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .child(title.to_owned()),
                                )
                                .child(Icon::new(IconName::KeyRound).small().text_color(
                                    if keyed {
                                        theme.foreground
                                    } else {
                                        theme.muted_foreground
                                    },
                                )),
                        )
                        .child(
                            div()
                                .text_xs()
                                .opacity(0.6)
                                .whitespace_normal()
                                .child(endpoint),
                        ),
                )
                .child(
                    Switch::new(format!("backend-toggle-{id}"))
                        .checked(enabled)
                        .on_click(cx.listener(move |workspace, checked: &bool, _, cx| {
                            workspace.apply_action(
                                DesktopAction::SettingsBackendToggled(index, *checked),
                                cx,
                            );
                        })),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h(px(28.))
                        .child(Input::new(&key_input)),
                )
                .child(
                    Button::new(format!("web-key-save-{id}"))
                        .label("Save key")
                        .small()
                        .outline()
                        .on_click({
                            let id = id.clone();
                            cx.listener(move |workspace, _, _, cx| {
                                workspace.on_save_web_key(&id, cx);
                            })
                        }),
                ),
        )
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
        .items_start()
        .gap_3()
        .p_2()
        .rounded(px(3.))
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
                        .whitespace_normal()
                        .child(id.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .opacity(0.6)
                        .whitespace_normal()
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
    let current = mycode_app::current_version().to_owned();
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
        let kind = make("querit | anysearch | custom");
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
    /// Full command line input (stdio transport): program plus arguments.
    pub command: Entity<InputState>,
    /// Extra environment variables (stdio transport), `KEY=VALUE` pairs.
    pub env: Entity<InputState>,
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
        let command = make("npx -y @modelcontextprotocol/server-filesystem C:\\projects");
        let env = make("KEY=value, OTHER=value (optional)");
        let api_key = make("api key (leave empty to skip)");
        cx.new(|_| Self {
            id,
            transport: "http".to_owned(),
            endpoint,
            command,
            env,
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
    let is_http = transport == "http";
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
                // Only the fields the chosen transport uses are shown; the
                // old form listed every field and users typed the full
                // command line into a single "command" that never resolved.
                .when(is_http, |this| {
                    this.child(labeled_field(
                        "endpoint (https URL)",
                        form.read(cx).endpoint.clone(),
                    ))
                    .child(labeled_field(
                        "api key (stored in the vault, sent as Bearer)",
                        form.read(cx).api_key.clone(),
                    ))
                })
                .when(!is_http, |this| {
                    this.child(labeled_field(
                        "command line (program and arguments, quotes allowed)",
                        form.read(cx).command.clone(),
                    ))
                    .child(labeled_field(
                        "environment (KEY=VALUE pairs, optional)",
                        form.read(cx).env.clone(),
                    ))
                }),
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
