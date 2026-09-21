//! The Agents settings page: delegation capacity and one card per subagent
//! role with its thinking/provider/model dropdowns.
use gpui_kit::component::button::Button;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled, div, px,
};

use super::widgets::{dropdown_field, row_header, settings_card, settings_row};
use crate::view_model::DesktopAction;
use crate::workspace::Workspace;

pub(super) fn render_agents_section(workspace: &Workspace, cx: &Context<Workspace>) -> AnyElement {
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
                workspace,
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
                        next.max_concurrent =
                            if next.max_concurrent >= mycode_config::MAX_SUBAGENT_CONCURRENCY {
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

#[allow(clippy::too_many_arguments)]
fn agent_role_card(
    workspace: &Workspace,
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
    let theme: &Theme = cx.theme();
    let role = name.clone();
    let thinking_label = thinking.clone();
    let provider_label = provider.clone();
    let model_label = model.clone();
    let mut provider_ids: Vec<String> = vec!["inherit".to_owned()];
    provider_ids.extend(providers.iter().map(|(id, _)| id.clone()));
    let mut models_for_provider: Vec<String> = vec!["inherit".to_owned()];
    models_for_provider.extend(
        providers
            .iter()
            .find(|(id, _)| *id == provider)
            .map(|(_, models)| models.clone())
            .unwrap_or_default(),
    );
    let thinking_options = {
        let mut levels = vec!["inherit".to_owned()];
        levels.extend(crate::view_model::reasoning_levels_for(
            workspace.vm(),
            (provider != "inherit").then_some(provider.as_str()),
            (model != "inherit").then_some(model.as_str()),
        ));
        levels
    };
    let open_field = workspace
        .vm()
        .subagent_menu
        .as_ref()
        .filter(|(open_role, _)| open_role == &name)
        .map(|(_, field)| field.as_str());
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
                    row_header(&name, description).child(
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
                .flex_col()
                .gap_2()
                .child(agent_choice_dropdown(
                    &role,
                    "thinking",
                    "Thinking",
                    &thinking_label,
                    &thinking_options,
                    open_field == Some("thinking"),
                    cx,
                ))
                .child(agent_choice_dropdown(
                    &role,
                    "provider",
                    "Provider",
                    &provider_label,
                    &provider_ids,
                    open_field == Some("provider"),
                    cx,
                ))
                .child(agent_choice_dropdown(
                    &role,
                    "model",
                    "Model",
                    &model_label,
                    &models_for_provider,
                    open_field == Some("model"),
                    cx,
                )),
        )
        .into_any_element()
}

/// One Agents-page dropdown over the shared [`dropdown_field`] widget: the
/// open flag lives in the shared subagent menu state and the pick routes to
/// the role's route/thinking handler.
#[allow(clippy::too_many_arguments)]
fn agent_choice_dropdown(
    role: &str,
    field: &str,
    label: &str,
    current: &str,
    options: &[String],
    open: bool,
    cx: &Context<Workspace>,
) -> AnyElement {
    let toggle_role = role.to_owned();
    let toggle_field = field.to_owned();
    dropdown_field(
        &format!("agent-{role}-{field}"),
        label,
        None,
        current,
        options,
        open,
        move |workspace, open, cx| {
            workspace.on_toggle_subagent_menu(&toggle_role, &toggle_field, open, cx)
        },
        {
            let role = role.to_owned();
            let field = field.to_owned();
            move |workspace, pick, cx| match field.as_str() {
                "thinking" => workspace.on_set_subagent_thinking(&role, Some(pick.to_owned()), cx),
                "provider" => {
                    workspace.on_set_subagent_route(&role, Some(pick.to_owned()), None, cx)
                }
                _ => workspace.on_set_subagent_route(&role, None, Some(pick.to_owned()), cx),
            }
        },
        cx,
    )
}
