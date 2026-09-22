//! Project- and session-shaped workspace handlers: the mention/skill
//! pipeline, message recall, session switching, and the project bindings
//! that drive the sidebar grouping.
use gpui_kit::{Context, Window};

use mycode_app::{BranchId, BridgeCommand, SessionId};

use crate::view_model::DesktopAction;
use crate::workspace::Workspace;

impl Workspace {
    /// Fires a project-file search when the active `@` fragment changed.
    pub(super) fn refresh_mention_search(&mut self, cx: &mut Context<Self>) {
        let (kind, fragment) = match self.vm.mention.as_ref() {
            Some(mention) => (mention.kind, mention.fragment.clone()),
            None => {
                self.mention_query = None;
                return;
            }
        };
        if self.mention_query.as_deref() == Some(fragment.as_str()) {
            return;
        }
        self.mention_query = Some(fragment.clone());
        if kind == crate::view_model::MentionKind::Command {
            self.merge_skill_commands(&fragment, cx);
            return;
        }
        if kind != crate::view_model::MentionKind::File {
            return;
        }
        let session_id = self
            .vm
            .active
            .as_ref()
            .map(|conversation| conversation.session_id.clone());
        let Some(session_id) = session_id else {
            return;
        };
        self.dispatch(
            BridgeCommand::SearchProjectFiles {
                session_id,
                query: fragment,
            },
            cx,
        );
    }

    fn skill_roots(&self) -> (std::path::PathBuf, Option<std::path::PathBuf>) {
        let workspace = self
            .vm
            .project_dir
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
        let user_home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(std::path::PathBuf::from);
        (workspace, user_home)
    }

    pub(super) fn refresh_skills(&mut self, cx: &mut Context<Self>) {
        let (workspace, user_home) = self.skill_roots();
        let skills = mycode_config::discover_skills(&workspace, user_home.as_deref())
            .into_iter()
            .map(|skill| crate::view_model::SkillEntry {
                slug: skill.slug,
                title: skill.title,
                path: skill.path.to_string_lossy().into_owned(),
                global: skill.global,
            })
            .collect();
        self.apply_action(DesktopAction::SkillsLoaded(skills), cx);
    }

    fn merge_skill_commands(&mut self, fragment: &str, cx: &mut Context<Self>) {
        let (workspace, user_home) = self.skill_roots();
        let skills = mycode_config::discover_skills(&workspace, user_home.as_deref());
        if let Some(mention) = self.vm.mention.as_mut() {
            for skill in skills {
                if !skill.slug.starts_with(fragment) {
                    continue;
                }
                let insert = format!("/{}", skill.slug);
                if mention
                    .items
                    .iter()
                    .any(|(existing, _)| existing == &insert)
                {
                    continue;
                }
                mention
                    .items
                    .push((insert, format!("/{} · {}", skill.slug, skill.title)));
            }
        }
        cx.notify();
    }

    /// Accepts one mention row: rewrites the draft (files) or runs the
    /// command (commands), then closes the menu.
    pub(crate) fn on_accept_mention(
        &mut self,
        insert: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mention) = self.vm.mention.take() else {
            return;
        };
        self.mention_query = None;
        match mention.kind {
            crate::view_model::MentionKind::File => {
                let mut text = self.vm.composer_draft.clone();
                if let Some(position) = text.rfind('@') {
                    text.replace_range(position.., &format!("@{insert} "));
                }
                self.pending_composer_prefill = Some(text);
                cx.notify();
            }
            crate::view_model::MentionKind::Command => {
                if insert == "/new" {
                    self.on_new_session(cx);
                } else if insert == "/settings" {
                    self.on_show_main_view(crate::view_model::MainView::Settings, cx);
                } else if let Some(slug) = insert.strip_prefix('/') {
                    self.insert_skill_draft(slug, cx);
                }
            }
        }
    }

    pub(crate) fn on_refresh_skills(&mut self, cx: &mut Context<Self>) {
        self.refresh_skills(cx);
    }

    pub(crate) fn on_use_skill(&mut self, slug: &str, cx: &mut Context<Self>) {
        self.insert_skill_draft(slug, cx);
        self.on_show_main_view(crate::view_model::MainView::Chat, cx);
    }

    fn insert_skill_draft(&mut self, slug: &str, cx: &mut Context<Self>) {
        let (workspace, user_home) = self.skill_roots();
        let Some(skill) = mycode_config::discover_skills(&workspace, user_home.as_deref())
            .into_iter()
            .find(|skill| skill.slug == slug)
        else {
            return;
        };
        let path = skill.path.display();
        self.pending_composer_prefill = Some(format!(
            "/{slug}\n\nFollow the `{title}` skill. Read `{path}` and apply it before continuing.\n",
            title = skill.title
        ));
        cx.notify();
    }

    /// Rewinds to just before the user message at `index` and prefills the
    /// composer with its text (修改). The first message has no prior event to
    /// rewind to and is ignored.
    pub(crate) fn on_edit_message(&mut self, index: usize, cx: &mut Context<Self>) {
        self.recall_at(index, true, cx);
    }

    /// Rewinds to just before the user message at `index` (撤回).
    pub(crate) fn on_recall_message(&mut self, index: usize, cx: &mut Context<Self>) {
        self.recall_at(index, false, cx);
    }

    fn recall_at(&mut self, index: usize, edit: bool, cx: &mut Context<Self>) {
        if self.vm.sending {
            return;
        }
        let Some(conversation) = self.vm.active.clone() else {
            return;
        };
        if index == 0 || index >= conversation.entries.len() {
            return;
        }
        if conversation.entries[index].kind != crate::view_model::EntryKind::UserMessage {
            return;
        }
        let Some(session) = SessionId::parse(&conversation.session_id) else {
            return;
        };
        let Some(branch) = BranchId::parse(&conversation.branch_id) else {
            return;
        };
        let expected_head = crate::workspace::parse_head(&conversation.head);
        let to_event = conversation.entries[index - 1].event_id.clone();
        let edit = if edit {
            Some(conversation.entries[index].text.to_string())
        } else {
            None
        };
        self.dispatch(
            BridgeCommand::RecallMessage {
                session,
                branch,
                expected_head,
                to_event,
                edit,
            },
            cx,
        );
    }

    /// Deletes one session's durable data and drops it if active.
    pub(crate) fn on_delete_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self
            .vm
            .active
            .as_ref()
            .is_some_and(|conversation| conversation.session_id == session_id)
        {
            self.apply_action(DesktopAction::SessionDeleted, cx);
        }
        self.dispatch(
            BridgeCommand::DeleteSession {
                session_id: session_id.to_owned(),
            },
            cx,
        );
    }

    /// Removes one directory from the remembered projects list.
    pub(crate) fn on_remove_recent(&mut self, project: &str, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.dispatch(
            BridgeCommand::RemoveRecent {
                project: project.to_owned(),
            },
            cx,
        );
        self.apply_action(DesktopAction::RecentRemoved(project.to_owned()), cx);
    }

    pub(crate) fn on_new_session(&mut self, cx: &mut Context<Self>) {
        // New chats inherit the active project so the sidebar grouping and
        // the tool working directory follow the project switcher.
        if self.vm.project_dir.is_some() {
            self.pending_project = self.vm.project_dir.clone();
        }
        self.dispatch(BridgeCommand::CreateSession, cx);
    }

    /// Filters the sidebar to one project. Chats stay on the project they
    /// were opened in; switching must not drag every session into This Project.
    pub(crate) fn on_switch_project(&mut self, project: Option<String>, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ProjectMenuToggled(false), cx);
        self.apply_action(DesktopAction::ActiveProjectChanged(project.clone()), cx);
        if project.is_some() {
            self.refresh_skills(cx);
        }
        self.persist_ui_state(cx);
    }

    pub(crate) fn on_toggle_project_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ProjectMenuToggled(open), cx);
    }

    // ---- project selection ----

    /// Opens the in-app folder browser and binds the chosen directory.
    pub(crate) fn on_open_project_dialog(&mut self, cx: &mut Context<Self>) {
        self.project_picker = Some(crate::ui::project_picker::ProjectPicker::open());
        cx.notify();
    }

    pub(crate) fn on_picker_cancel(&mut self, cx: &mut Context<Self>) {
        self.project_picker = None;
        cx.notify();
    }

    pub(crate) fn on_picker_enter(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.project_picker = Some(crate::ui::project_picker::browse(Some(path)));
        cx.notify();
    }

    pub(crate) fn on_picker_up(&mut self, cx: &mut Context<Self>) {
        let Some(current) = self
            .project_picker
            .as_ref()
            .and_then(|picker| picker.current.clone())
        else {
            return;
        };
        self.project_picker = Some(crate::ui::project_picker::browse(
            crate::ui::project_picker::parent_folder(&current),
        ));
        cx.notify();
    }

    pub(crate) fn on_picker_home(&mut self, cx: &mut Context<Self>) {
        self.project_picker = Some(crate::ui::project_picker::ProjectPicker::home());
        cx.notify();
    }

    /// Binds the first dropped directory, or the parent of a dropped file.
    pub(crate) fn on_drop_project(&mut self, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        let folder = paths
            .iter()
            .find(|path| path.is_dir())
            .cloned()
            .or_else(|| {
                paths
                    .first()
                    .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
            });
        let Some(folder) = folder else {
            return;
        };
        if !folder.is_dir() {
            return;
        }
        self.project_picker = None;
        self.bind_project(&folder.to_string_lossy(), cx);
    }

    pub(crate) fn on_picker_roots(&mut self, cx: &mut Context<Self>) {
        self.project_picker = Some(crate::ui::project_picker::browse(None));
        cx.notify();
    }

    pub(crate) fn on_picker_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self
            .project_picker
            .as_ref()
            .and_then(|picker| picker.current.clone())
        else {
            return;
        };
        if !path.is_dir() {
            return;
        }
        self.project_picker = None;
        self.bind_project(&path.to_string_lossy(), cx);
    }

    /// Opens one of the remembered recent projects.
    pub(crate) fn on_open_recent(&mut self, project: &str, cx: &mut Context<Self>) {
        self.bind_project(project, cx);
    }

    /// Binds the project to the active session, creating one when needed.
    pub(super) fn bind_project(&mut self, project: &str, cx: &mut Context<Self>) {
        if self.vm.active.is_none() {
            self.pending_project = Some(project.to_owned());
            self.dispatch(BridgeCommand::CreateSession, cx);
            return;
        }
        let session_id = self
            .vm
            .active
            .as_ref()
            .map(|conversation| conversation.session_id.clone())
            .expect("active session");
        self.apply_action(
            DesktopAction::SessionProjectBound {
                session_id: session_id.clone(),
                project: project.to_owned(),
            },
            cx,
        );
        self.apply_action(DesktopAction::ProjectOpened(project.to_owned()), cx);
        self.apply_action(
            DesktopAction::UnboundSessionsAssigned(project.to_owned()),
            cx,
        );
        self.dispatch(
            BridgeCommand::SetProjectDir {
                session_id: session_id.clone(),
                path: Some(project.to_owned()),
            },
            cx,
        );
        self.dispatch(
            BridgeCommand::ListResources {
                session_id: session_id.clone(),
            },
            cx,
        );
        self.refresh_skills(cx);
        self.persist_ui_state(cx);
    }

    /// Persists the durable UI state projection.
    pub(super) fn persist_ui_state(&self, cx: &mut Context<Self>) {
        let state = mycode_config::UiState {
            recent_projects: self.vm.recents.clone(),
            last_project: self.vm.project_dir.clone(),
            auto_update: self.vm.auto_update,
            selected_provider: self.vm.selected_provider.clone(),
            selected_model: self.vm.selected_model.clone(),
            session_projects: self.vm.session_projects.clone(),
        };
        self.dispatch(BridgeCommand::SaveUiState { state }, cx);
    }
}
