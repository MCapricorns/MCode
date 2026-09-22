//! Reducer internals: the pure transition functions behind
//! [`super::reduce`]. Only this file mutates [`WorkspaceState`]; the parent
//! module declares the state, action, and projection types.
use mycode_app::{CHAT_CANCELLED, MAX_STREAMING_CHARS, StreamingReply};

use super::{
    ActiveConversation, COMPOSER_COMMANDS, ComposerMention, ConversationEntry, DesktopAction,
    EntryKind, LiveJob, MentionKind, SettingsState, TRANSCRIPT_PAGE, TurnStats, UpdateState,
    UsageTotal, WorkspaceState,
};

/// Applies one action to the state.
pub fn reduce(state: &mut WorkspaceState, action: DesktopAction) {
    let touches_providers = matches!(
        action,
        DesktopAction::SettingsLoaded(_)
            | DesktopAction::SettingsProviderAdded(_)
            | DesktopAction::SettingsProviderRemoved(_)
            | DesktopAction::SettingsProviderToggled(_, _)
            | DesktopAction::SettingsSaved(_)
            | DesktopAction::UiStateLoaded { .. }
    );
    match action {
        DesktopAction::SessionsLoaded(mut sessions) => {
            let active_id = state.active.as_ref().map(|c| c.session_id.as_str());
            for session in &mut sessions {
                session.active = active_id == Some(session.session_id.as_str());
            }
            state.sessions = sessions;
        }
        DesktopAction::SessionCreated(mut summary) => {
            state
                .sessions
                .retain(|s| s.session_id != summary.session_id);
            summary.active = true;
            state.sessions.insert(0, summary.clone());
            state.active = Some(ActiveConversation {
                session_id: summary.session_id,
                branch_id: summary.root_branch_id,
                head: "empty".to_owned(),
                entries: Vec::new(),
                streaming: None,
            });
            state.live_jobs.clear();
            state.subagent_window = None;
            state.transcript_extra = 0;
        }
        DesktopAction::SessionDeleted => {
            state.active = None;
            state.sending = false;
            state.queued.clear();
            state.live_jobs.clear();
            state.subagent_window = None;
            state.pending_ask = None;
            state.error = None;
            state.transcript_extra = 0;
        }
        DesktopAction::ConversationOpened(conversation) => {
            let switched = state
                .active
                .as_ref()
                .map(|active| active.session_id.as_str())
                != Some(conversation.session_id.as_str());
            if switched {
                // Queue, tasks, and asks belong to the previous session.
                state.queued.clear();
                state.live_jobs.clear();
                state.subagent_window = None;
                state.todo_rows.clear();
                state.pending_ask = None;
                state.sending = false;
                state.transcript_extra = 0;
            }
            let session_id = conversation.session_id.clone();
            state.active = Some(conversation);
            for session in &mut state.sessions {
                session.active = session.session_id == session_id;
            }
        }
        DesktopAction::ComposerChanged(text) => {
            let bounded: String = text.chars().take(super::MAX_COMPOSER_CHARS).collect();
            state.mention = parse_mention(&bounded);
            state.composer_draft = bounded;
        }
        DesktopAction::MentionFiles(files) => {
            if let Some(mention) = state.mention.as_mut()
                && mention.kind == MentionKind::File
            {
                mention.items = files
                    .into_iter()
                    .map(|path| {
                        let display = path.clone();
                        (path, display)
                    })
                    .collect();
            }
        }
        DesktopAction::CopilotSignInStarted(info) => {
            state.copilot_sign_in = Some(info);
            state.copilot_error = None;
        }
        DesktopAction::CopilotSignInFinished(outcome) => {
            state.copilot_sign_in = None;
            state.copilot_error = outcome.err();
        }
        DesktopAction::MessageQueued(text) => {
            let bounded: String = text.chars().take(super::MAX_COMPOSER_CHARS).collect();
            if !bounded.trim().is_empty() && state.queued.len() < super::MAX_QUEUED_MESSAGES {
                state.queued.push(bounded);
            }
            state.composer_draft.clear();
            state.mention = None;
        }
        DesktopAction::QueuedMessageRemoved(index) => {
            if index < state.queued.len() {
                state.queued.remove(index);
            }
        }
        DesktopAction::QueuedMessageTaken => {
            if !state.queued.is_empty() {
                state.queued.remove(0);
            }
        }
        DesktopAction::QueuedMessagePromoted(index) => {
            if index < state.queued.len() {
                let item = state.queued.remove(index);
                state.queued.insert(0, item);
            }
        }
        DesktopAction::SubagentWindowChanged(call_id) => {
            state.subagent_window = call_id;
        }
        DesktopAction::MessageSent { head, entry } => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.head = head;
                conversation.entries.push(entry);
            }
            state.composer_draft.clear();
            state.mention = None;
        }
        DesktopAction::TurnArmed => {
            state.sending = true;
            state.error = None;
            set_streaming_status(state, "Waiting for the model");
        }
        DesktopAction::ChatDelta(delta) => {
            append_streaming(state, false, delta);
        }
        DesktopAction::ToolStarted { call_id, name } => {
            set_streaming_status(state, &format!("Running {name}"));
            if name == "task" {
                upsert_live_job(state, &call_id, "", "starting", false);
            }
            if let Some(conversation) = state.active.as_mut() {
                conversation.entries.push(ConversationEntry {
                    event_id: format!("call-{call_id}"),
                    kind: EntryKind::ToolCall,
                    text: name.into(),
                    call_id: Some(call_id),
                    thinking: String::new(),
                });
            }
        }
        DesktopAction::ToolProgress {
            call_id,
            name,
            message,
        } => {
            let status = if name.is_empty() {
                message.clone()
            } else {
                format!("{name}: {message}")
            };
            set_streaming_status(state, &status);
            apply_live_job_progress(state, &call_id, &name, &message);
        }
        DesktopAction::ToolResultAppended(entry) => {
            if let Some(call_id) = entry.call_id.as_deref() {
                finish_live_job(state, call_id);
            }
            if let Some(conversation) = state.active.as_mut() {
                conversation.entries.push(entry);
            }
        }
        DesktopAction::ResourcesLoaded(files) => state.resources = files,
        DesktopAction::SkillsLoaded(skills) => state.skills = skills,
        DesktopAction::UsageRecorded {
            provider,
            model,
            input,
            output,
            cache,
            elapsed_ms,
            entry,
        } => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.entries.push(entry);
            }
            state.last_turn = Some(TurnStats {
                model: model.clone(),
                input,
                output,
                cache,
                elapsed_ms,
            });
            let key = format!("{provider}/{model}");
            let row = match state.usage_totals.iter_mut().find(|row| row.key == key) {
                Some(row) => row,
                None => {
                    state.usage_totals.push(UsageTotal {
                        key,
                        ..UsageTotal::default()
                    });
                    state.usage_totals.last_mut().expect("just pushed")
                }
            };
            row.input = row.input.saturating_add(input);
            row.output = row.output.saturating_add(output);
            row.cache = row.cache.saturating_add(cache.unwrap_or_default());
            row.requests = row.requests.saturating_add(1);
        }
        DesktopAction::TodoUpdated(tasks) => {
            state.todo_rows = tasks
                .into_iter()
                .filter(|(_, status)| status != "done")
                .collect();
        }
        DesktopAction::AskRequested(rows) => {
            state.ask_answers = vec![String::new(); rows.len()];
            state.pending_ask = Some(rows);
        }
        DesktopAction::AskChoicePicked { index, answer } => {
            if let Some(slot) = state.ask_answers.get_mut(index) {
                *slot = answer;
            }
        }
        DesktopAction::AskAnswered => {
            state.pending_ask = None;
            state.ask_answers.clear();
        }
        DesktopAction::TranscriptRevealMore => {
            state.transcript_extra = state.transcript_extra.saturating_add(TRANSCRIPT_PAGE);
        }
        DesktopAction::ChatThinkingDelta(delta) => {
            append_streaming(state, true, delta);
        }
        DesktopAction::AssistantStepCommitted(entry) => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.entries.push(entry);
            }
            set_streaming_status(state, "Waiting for the next step");
            if let Some(conversation) = state.active.as_mut()
                && let Some(streaming) = conversation.streaming.as_mut()
            {
                streaming.text.clear();
                streaming.thinking.clear();
            }
        }
        DesktopAction::ChatDone { head, entry } => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.head = head;
                // A turn that ended on a committed tool step reports that
                // step again as its last message; it is already listed.
                let already_listed = conversation
                    .entries
                    .iter()
                    .any(|existing| existing.event_id == entry.event_id);
                if !already_listed {
                    conversation.entries.push(entry);
                }
                conversation.streaming = None;
            }
            state.live_jobs.clear();
            state.sending = false;
        }
        DesktopAction::ChatFailed(message) => {
            if let Some(conversation) = state.active.as_mut() {
                conversation.streaming = None;
            }
            // A user-initiated cancel resets the turn without an error
            // banner; the sentinel travels as the failure message.
            if message != CHAT_CANCELLED {
                state.error = Some(message);
            }
            state.live_jobs.clear();
            state.sending = false;
        }
        DesktopAction::Failed(message) => {
            state.error = Some(message);
            state.sending = false;
        }
        DesktopAction::SettingsLoaded(settings) => {
            let dark = settings.theme != "light";
            state.settings = Some(settings);
            state.dark_theme = dark;
        }
        DesktopAction::SettingsThemeSelected(dark) => {
            if let Some(settings) = state.settings.as_mut() {
                settings.theme = if dark { "dark" } else { "light" }.to_owned();
                settings.dirty = true;
            }
            state.dark_theme = dark;
        }
        DesktopAction::MentionDismissed => state.mention = None,
        DesktopAction::SettingsUserAgentChanged(user_agent) => {
            edit_settings(state, |settings| {
                settings.user_agent = user_agent;
                true
            });
        }
        DesktopAction::SettingsProviderToggled(index, enabled) => {
            edit_settings(state, |settings| {
                settings.providers.get_mut(index).is_some_and(|provider| {
                    provider.enabled = enabled;
                    true
                })
            });
        }
        DesktopAction::SettingsProviderAdded(provider) => {
            edit_settings(state, |settings| {
                if settings.providers.len() < mycode_config::MAX_PROVIDERS {
                    settings.providers.push(provider);
                    true
                } else {
                    false
                }
            });
        }
        DesktopAction::SettingsProviderRemoved(index) => {
            edit_settings(state, |settings| {
                if index < settings.providers.len() {
                    settings.providers.remove(index);
                    true
                } else {
                    false
                }
            });
        }
        DesktopAction::SettingsBackendAdded(backend) => {
            edit_settings(state, |settings| {
                if settings.web_backends.len() < mycode_config::MAX_WEB_BACKENDS {
                    settings.web_backends.push(backend);
                    true
                } else {
                    false
                }
            });
        }
        DesktopAction::SettingsBackendRemoved(index) => {
            edit_settings(state, |settings| {
                if index < settings.web_backends.len() {
                    settings.web_backends.remove(index);
                    true
                } else {
                    false
                }
            });
        }
        DesktopAction::SettingsUsageToggled(enabled) => {
            edit_settings(state, |settings| {
                settings.usage_enabled = enabled;
                true
            });
        }
        DesktopAction::SettingsBackendToggled(index, enabled) => {
            edit_settings(state, |settings| {
                if index >= settings.web_backends.len() {
                    return false;
                }
                if enabled {
                    for (slot, backend) in settings.web_backends.iter_mut().enumerate() {
                        backend.enabled = slot == index;
                    }
                } else {
                    settings.web_backends[index].enabled = false;
                }
                true
            });
        }
        DesktopAction::SettingsSubagentsChanged(subagents) => {
            edit_settings(state, |settings| {
                settings.subagents = subagents;
                true
            });
        }
        DesktopAction::SettingsToolsChanged(tools) => {
            edit_settings(state, |settings| {
                settings.tools = tools;
                true
            });
        }
        DesktopAction::SubagentMenuToggled(menu) => state.subagent_menu = menu,
        DesktopAction::UnboundSessionsAssigned(project) => {
            assign_unbound_sessions(state, &project);
        }
        DesktopAction::SettingsSaved(revision) => {
            if let Some(settings) = state.settings.as_mut() {
                settings.revision = revision;
                settings.saving = false;
                settings.dirty = false;
                settings.effective_user_agent = settings.to_settings().effective_user_agent();
            }
        }
        DesktopAction::ProviderKeySaved {
            provider_keys,
            mcp_keys,
        } => {
            if let Some(settings) = state.settings.as_mut() {
                settings.providers_with_keys = provider_keys;
                settings.mcp_with_keys = mcp_keys;
            }
        }
        DesktopAction::SettingsMcpAdded(server) => {
            edit_settings(state, |settings| {
                if settings.mcp_servers.len() < mycode_config::MAX_MCP_SERVERS {
                    settings.mcp_servers.push(server);
                    true
                } else {
                    false
                }
            });
        }
        DesktopAction::SettingsMcpRemoved(index) => {
            if let Some(settings) = state.settings.as_mut()
                && index < settings.mcp_servers.len()
            {
                let removed = settings.mcp_servers.remove(index);
                settings.dirty = true;
                // A stale listing for a deleted row must not resurface if a
                // server with the same id is added again later.
                state.mcp_tools.retain(|(id, _)| *id != removed.id);
                state.mcp_probing.retain(|id| *id != removed.id);
            }
        }
        DesktopAction::SettingsMcpToggled(index, enabled) => {
            edit_settings(state, |settings| {
                settings.mcp_servers.get_mut(index).is_some_and(|server| {
                    server.enabled = enabled;
                    true
                })
            });
        }
        DesktopAction::McpProbeStarted(server_id) => {
            if !state.mcp_probing.contains(&server_id) {
                state.mcp_probing.push(server_id);
            }
        }
        DesktopAction::McpToolsListed { server_id, tools } => {
            state.mcp_probing.retain(|id| *id != server_id);
            if let Some(entry) = state.mcp_tools.iter_mut().find(|(id, _)| *id == server_id) {
                entry.1 = tools;
            } else {
                state.mcp_tools.push((server_id, tools));
            }
        }
        DesktopAction::McpProbeFailed { server_id, message } => {
            state.mcp_probing.retain(|id| *id != server_id);
            state.mcp_tools.retain(|(id, _)| *id != server_id);
            state.error = Some(message);
        }
        DesktopAction::DismissError => state.error = None,
        DesktopAction::ShowMainView(view) => {
            state.view = view;
            // Crossing views closes every floating menu so no stale layer
            // renders above the destination view.
            close_floating_menus(state);
        }
        DesktopAction::ShowSettingsSection(section) => state.settings_section = section,
        DesktopAction::ProjectMenuToggled(open) => state.project_menu_open = open,
        DesktopAction::CatalogLoaded {
            document,
            fetched_at,
        } => {
            state.catalog = Some(document);
            state.catalog_fetched_at = fetched_at;
        }
        DesktopAction::UiStateLoaded {
            recents,
            last_project,
            auto_update,
            selected_provider,
            selected_model,
            session_projects,
        } => {
            state.recents = recents;
            state.project_dir = last_project.filter(|path| !path.trim().is_empty());
            state.session_projects = session_projects;
            state.auto_update = auto_update;
            if selected_provider.is_some() {
                state.selected_provider = selected_provider;
                state.selected_model = selected_model;
            }
            ensure_model_selection(state);
        }
        DesktopAction::ProjectOpened(project) => {
            state.project_dir = Some(project.clone());
            state.recents.retain(|existing| existing != &project);
            state.recents.insert(0, project);
            state.recents.truncate(mycode_config::MAX_RECENT_PROJECTS);
        }
        DesktopAction::SessionProjectBound {
            session_id,
            project,
        } => {
            state
                .session_projects
                .retain(|(existing, _)| existing != &session_id);
            state
                .session_projects
                .insert(0, (session_id, project.clone()));
            state
                .session_projects
                .truncate(mycode_config::MAX_SESSION_PROJECTS);
        }
        DesktopAction::ActiveProjectChanged(project) => {
            state.project_dir = project;
            if !super::task_surface_visible(state) {
                state.subagent_window = None;
            }
        }
        DesktopAction::RecentRemoved(project) => {
            state
                .recents
                .retain(|existing| !super::same_project_path(existing, &project));
            if state
                .project_dir
                .as_ref()
                .is_some_and(|current| super::same_project_path(current, &project))
            {
                state.project_dir = None;
            }
        }
        DesktopAction::ProviderSelected(provider) => {
            state.selected_provider = Some(provider.clone());
            state.selected_model = state
                .catalog
                .as_ref()
                .and_then(|catalog| catalog.provider(&provider))
                .and_then(|provider| provider.models.first())
                .map(|model| model.id.clone())
                .or_else(|| {
                    state
                        .settings
                        .as_ref()
                        .and_then(|settings| settings.providers.iter().find(|p| p.id == provider))
                        .and_then(|provider| provider.models.first().cloned())
                });
            state.model_menu_open = false;
            if !selected_model_supports_reasoning(state) {
                state.reasoning_menu_open = false;
            }
            clamp_reasoning_to_catalog(state);
        }
        DesktopAction::ModelSelected(model) => {
            // Picking a catalog model the provider row does not carry yet
            // appends it to the row: turn resolution validates against that
            // list, so an unpersisted selection would silently fall back to
            // the first model.
            let provider_id = state.selected_provider.clone();
            let mut capped = false;
            if let Some(settings) = state.settings.as_mut()
                && let Some(provider) = provider_id
                    .as_deref()
                    .and_then(|id| settings.providers.iter_mut().find(|p| p.id == id))
            {
                let known = provider.models.contains(&model);
                if !known {
                    if provider.models.len() < mycode_config::MAX_MODELS_PER_PROVIDER {
                        provider.models.push(model.clone());
                        settings.dirty = true;
                    } else {
                        capped = true;
                    }
                }
            }
            if capped {
                // The row is at the settings cap: storing the pick would
                // leave a selection the next turn silently drops, so refuse
                // the pick and say why.
                state.error = Some(format!(
                    "this provider is at its {}-model limit \u{2014} remove one in \
                     Settings \u{2192} Models before switching to an unlisted model",
                    mycode_config::MAX_MODELS_PER_PROVIDER
                ));
                return;
            }
            state.selected_model = Some(model);
            state.model_menu_open = false;
            if !selected_model_supports_reasoning(state) {
                state.reasoning_menu_open = false;
            }
            clamp_reasoning_to_catalog(state);
        }
        DesktopAction::ModelMenuToggled(open) => {
            state.model_menu_open = open;
            if open {
                state.reasoning_menu_open = false;
            }
        }
        DesktopAction::ReasoningMenuToggled(open) => {
            state.reasoning_menu_open = open;
            if open {
                state.model_menu_open = false;
            }
        }
        DesktopAction::SettingsReasoningChanged(level) => {
            state.reasoning_menu_open = false;
            state.model_menu_open = false;
            if state
                .settings
                .as_ref()
                .is_none_or(|settings| settings.saving)
            {
                return;
            }
            let picked: Option<String> = if level == "default" || level.is_empty() {
                None
            } else if selected_reasoning_levels(state).contains(&level) {
                Some(level.clone())
            } else {
                // Levels the catalog does not advertise are refused.
                return;
            };
            edit_settings(state, |settings| {
                settings.reasoning = picked;
                true
            });
        }
        DesktopAction::PresetSearchChanged(text) => state.preset_search = text,
        DesktopAction::ActivePresetChanged(preset) => {
            state.active_preset = preset.clone();
            state.preset_model_menu_open = false;
            // Opening a provider pre-checks its model list, strongest first,
            // so a long catalog does not bury o3 / gpt-5 under the cap.
            state.preset_models = preset
                .as_ref()
                .and_then(|id| {
                    state
                        .catalog
                        .as_ref()
                        .and_then(|catalog| catalog.provider(id))
                })
                .map(|provider| {
                    super::rank_model_ids(provider.models.iter().map(|model| model.id.clone()))
                        .into_iter()
                        .take(mycode_config::MAX_MODELS_PER_PROVIDER)
                        .collect()
                })
                .unwrap_or_default();
        }
        DesktopAction::PresetModelToggled(model) => {
            if let Some(position) = state.preset_models.iter().position(|m| *m == model) {
                state.preset_models.remove(position);
            } else {
                state.preset_models.push(model);
            }
        }
        DesktopAction::PresetModelMenuToggled(open) => state.preset_model_menu_open = open,
        DesktopAction::ShowModelsSubview(view) => {
            state.models_subview = view;
            state.active_preset = None;
            state.preset_model_menu_open = false;
            state.provider_kind_menu_open = false;
            state.mcp_transport_menu_open = false;
        }
        DesktopAction::ProviderKindMenuToggled(open) => state.provider_kind_menu_open = open,
        DesktopAction::McpTransportMenuToggled(open) => state.mcp_transport_menu_open = open,
        DesktopAction::ShellKindMenuToggled(open) => state.shell_kind_menu_open = open,
        DesktopAction::UpdateStateChanged(update) => state.update = update,
        DesktopAction::UpdateOfferFound(offer) => state.last_offer = Some(offer),
        DesktopAction::AutoUpdateToggled(auto_update) => state.auto_update = auto_update,
        DesktopAction::UpdateStaged(prepared) => {
            state.prepared_update = Some(prepared);
            state.update = UpdateState::Ready {
                version: mycode_app::current_version().to_owned(),
            };
        }
    }
    if touches_providers {
        ensure_model_selection(state);
    }
}

/// Applies one edit to the settings projection and marks the document dirty
/// when the edit reports it changed something. Collapses the
/// borrow-guard-mark boilerplate the settings arms share.
fn edit_settings(state: &mut WorkspaceState, edit: impl FnOnce(&mut SettingsState) -> bool) {
    if let Some(settings) = state.settings.as_mut()
        && edit(settings)
    {
        settings.dirty = true;
    }
}

/// Closes every floating menu layer, whatever view it belongs to. Returns
/// whether anything was open, so Escape can tell a dismissal from a no-op.
pub(crate) fn close_floating_menus(state: &mut WorkspaceState) -> bool {
    let was_open = state.project_menu_open
        || state.model_menu_open
        || state.reasoning_menu_open
        || state.subagent_menu.is_some()
        || state.preset_model_menu_open
        || state.provider_kind_menu_open
        || state.mcp_transport_menu_open
        || state.shell_kind_menu_open
        || state.mention.is_some();
    state.project_menu_open = false;
    state.model_menu_open = false;
    state.reasoning_menu_open = false;
    state.subagent_menu = None;
    state.preset_model_menu_open = false;
    state.provider_kind_menu_open = false;
    state.mcp_transport_menu_open = false;
    state.shell_kind_menu_open = false;
    state.mention = None;
    was_open
}

fn assign_unbound_sessions(state: &mut WorkspaceState, project: &str) {
    let bound: std::collections::HashSet<String> = state
        .session_projects
        .iter()
        .map(|(id, _)| id.clone())
        .collect();
    // Collect the new bindings in sidebar order (newest first), then splice
    // the block in at the front so the most-recent-first invariant survives.
    let mut added: Vec<(String, String)> = state
        .sessions
        .iter()
        .filter(|session| !bound.contains(&session.session_id))
        .map(|session| (session.session_id.clone(), project.to_owned()))
        .collect();
    if let Some(active) = state.active.as_ref()
        && !bound.contains(&active.session_id)
        && !added.iter().any(|(id, _)| *id == active.session_id)
    {
        // The open conversation is the most recently used session of all.
        added.insert(0, (active.session_id.clone(), project.to_owned()));
    }
    for pair in added.into_iter().rev() {
        state.session_projects.insert(0, pair);
    }
    state
        .session_projects
        .truncate(mycode_config::MAX_SESSION_PROJECTS);
}

/// Keeps the model picker on a provider/model that actually exists.
fn ensure_model_selection(state: &mut WorkspaceState) {
    let Some(settings) = state.settings.as_ref() else {
        return;
    };
    let selected_valid = state
        .selected_provider
        .as_ref()
        .and_then(|provider_id| {
            settings
                .providers
                .iter()
                .find(|provider| &provider.id == provider_id)
        })
        .is_some_and(|provider| {
            provider.enabled
                && state
                    .selected_model
                    .as_ref()
                    .is_some_and(|model| provider.models.contains(model))
        });
    if selected_valid {
        return;
    }
    let fallback = settings
        .providers
        .iter()
        .find(|provider| provider.enabled && !provider.models.is_empty());
    state.selected_provider = fallback.map(|provider| provider.id.clone());
    state.selected_model = fallback.and_then(|provider| provider.models.first().cloned());
}

/// Whether the selected catalog model advertises reasoning.
///
/// Unknown catalog rows keep the control visible so custom providers are
/// not locked out; the menu itself is built from `reasoning_options`.
#[must_use]
pub fn selected_model_supports_reasoning(state: &WorkspaceState) -> bool {
    let Some(catalog) = state.catalog.as_ref() else {
        return true;
    };
    let Some(provider) = state
        .selected_provider
        .as_deref()
        .and_then(|id| catalog.provider(id))
    else {
        return true;
    };
    match state.selected_model.as_deref() {
        Some(model_id) => provider
            .models
            .iter()
            .find(|model| model.id == model_id)
            .map(|model| model.reasoning)
            .unwrap_or(true),
        None => provider.models.iter().any(|model| model.reasoning),
    }
}

/// Thinking choices advertised for one catalog model.
///
/// Missing catalog rows yield only `default` so the UI never invents
/// low/medium/high.
#[must_use]
pub fn reasoning_levels_for(
    state: &WorkspaceState,
    provider_id: Option<&str>,
    model_id: Option<&str>,
) -> Vec<String> {
    let Some(catalog) = state.catalog.as_ref() else {
        return vec!["default".to_owned()];
    };
    let (Some(provider_id), Some(model_id)) = (provider_id, model_id) else {
        return vec!["default".to_owned()];
    };
    match catalog.model(provider_id, model_id) {
        Some(model) => model.reasoning_levels(),
        None => vec!["default".to_owned()],
    }
}

/// Thinking choices for the composer chip's selected model.
#[must_use]
pub fn selected_reasoning_levels(state: &WorkspaceState) -> Vec<String> {
    reasoning_levels_for(
        state,
        state.selected_provider.as_deref(),
        state.selected_model.as_deref(),
    )
}

fn clamp_reasoning_to_catalog(state: &mut WorkspaceState) {
    let levels = selected_reasoning_levels(state);
    let Some(settings) = state.settings.as_mut() else {
        return;
    };
    let Some(current) = settings.reasoning.as_deref() else {
        return;
    };
    if !levels.iter().any(|level| level == current) {
        settings.reasoning = None;
        settings.dirty = true;
    }
}

/// Parses the composer draft into an active mention, if any: a trailing
/// `@fragment` token selects files; a leading `/name` (still the whole
/// draft) selects commands.
fn parse_mention(text: &str) -> Option<ComposerMention> {
    if text.ends_with(char::is_whitespace) || text.is_empty() {
        return None;
    }
    let token = text.split_whitespace().last().unwrap_or_default();
    if let Some(fragment) = token.strip_prefix('@')
        && !fragment.contains('@')
    {
        return Some(ComposerMention {
            kind: MentionKind::File,
            fragment: fragment.to_owned(),
            items: Vec::new(),
        });
    }
    if let Some(fragment) = text.strip_prefix('/')
        && !fragment.contains(char::is_whitespace)
    {
        let items = COMPOSER_COMMANDS
            .iter()
            .filter(|(name, _)| name[1..].starts_with(fragment))
            .map(|(name, label)| ((*name).to_owned(), format!("{name} \u{b7} {label}")))
            .collect();
        return Some(ComposerMention {
            kind: MentionKind::Command,
            fragment: fragment.to_owned(),
            items,
        });
    }
    None
}

fn parse_task_progress(message: &str) -> Option<(&str, &str, &str)> {
    let mut parts = message.splitn(4, '|');
    if parts.next()? != "task" {
        return None;
    }
    Some((parts.next()?, parts.next()?, parts.next().unwrap_or("")))
}

fn upsert_live_job(state: &mut WorkspaceState, call_id: &str, role: &str, step: &str, done: bool) {
    if let Some(job) = state
        .live_jobs
        .iter_mut()
        .find(|job| !call_id.is_empty() && job.call_id == call_id)
    {
        if !role.is_empty() {
            job.role = role.to_owned();
        }
        if !step.is_empty() {
            push_job_step(job, step);
        }
        job.done = done;
        return;
    }
    if let Some(job) = state.live_jobs.iter_mut().rev().find(|job| !job.done)
        && (call_id.is_empty() || job.call_id.is_empty())
    {
        if !call_id.is_empty() {
            job.call_id = call_id.to_owned();
        }
        if !role.is_empty() {
            job.role = role.to_owned();
        }
        if !step.is_empty() {
            push_job_step(job, step);
        }
        job.done = done;
        return;
    }
    let step = if step.is_empty() {
        "starting".to_owned()
    } else {
        step.to_owned()
    };
    state.live_jobs.push(LiveJob {
        call_id: call_id.to_owned(),
        role: role.to_owned(),
        label: String::new(),
        log: vec![step.clone()],
        step,
        done,
    });
}

fn push_job_step(job: &mut super::LiveJob, step: &str) {
    job.step = step.to_owned();
    if job.log.last().is_none_or(|last| last != step) {
        job.log.push(step.to_owned());
        if job.log.len() > 48 {
            job.log.remove(0);
        }
    }
}

fn apply_live_job_progress(state: &mut WorkspaceState, call_id: &str, name: &str, message: &str) {
    if name != "task" && !message.starts_with("task|") {
        return;
    }
    if let Some((role, phase, detail)) = parse_task_progress(message) {
        match phase {
            "queued" => {
                upsert_live_job(state, call_id, role, "queued", false);
                if let Some(job) = live_job_mut(state, call_id) {
                    job.label = detail.to_owned();
                }
            }
            "done" => upsert_live_job(state, call_id, role, "done", true),
            "tool" | "step" => upsert_live_job(state, call_id, role, detail, false),
            other => upsert_live_job(state, call_id, role, other, false),
        }
        return;
    }
    upsert_live_job(state, call_id, "", message, false);
}

fn live_job_mut<'a>(state: &'a mut WorkspaceState, call_id: &str) -> Option<&'a mut LiveJob> {
    if call_id.is_empty() {
        return state.live_jobs.iter_mut().rev().find(|job| !job.done);
    }
    state
        .live_jobs
        .iter_mut()
        .find(|job| job.call_id == call_id)
}

fn finish_live_job(state: &mut WorkspaceState, call_id: &str) {
    if let Some(job) = live_job_mut(state, call_id) {
        job.done = true;
        if job.step.is_empty() || job.step == "starting" || job.step == "queued" {
            job.step = "done".to_owned();
        }
    }
}

/// Ensures a live streaming bubble exists and updates its status line.
fn set_streaming_status(state: &mut WorkspaceState, status: &str) {
    if let Some(conversation) = state.active.as_mut() {
        let streaming = conversation
            .streaming
            .get_or_insert_with(StreamingReply::default);
        streaming.status = status.to_owned();
    }
}

/// Buffers one streaming fragment into the active conversation.
fn append_streaming(state: &mut WorkspaceState, thinking: bool, delta: String) {
    if delta.is_empty() {
        return;
    }
    if let Some(conversation) = state.active.as_mut() {
        let streaming = conversation
            .streaming
            .get_or_insert_with(StreamingReply::default);
        streaming.status = if thinking {
            "Thinking".to_owned()
        } else {
            "Writing".to_owned()
        };
        let buffer = if thinking {
            &mut streaming.thinking
        } else {
            &mut streaming.text
        };
        // Take the remaining room once instead of re-counting the buffer per
        // pushed character; the buffer can grow to 256 KiB, which made the
        // per-char check quadratic over a long turn.
        let room = MAX_STREAMING_CHARS.saturating_sub(buffer.chars().count());
        buffer.extend(delta.chars().take(room));
    }
}
