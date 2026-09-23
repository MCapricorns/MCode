//! Durable UI state for the desktop: advisory, disposable, never a source
//! of truth for credentials or product behavior.
//!
//! Holds the recent project list, the last opened project, the update
//! preference, and the last selected provider/model so the desktop reopens
//! where the user left off. Missing or invalid documents reset to defaults.
use serde::{Deserialize, Serialize};

use crate::ConfigError;
use crate::error::ConfigErrorKind;
use crate::secure_fs::owned_file::{locked_update_owned_file, read_owned_file};

/// UI state path below the owned home.
pub const UI_STATE_PATH: &str = "ui.json";
/// Maximum encoded UI state size.
pub const MAX_UI_STATE_BYTES: usize = 64 * 1024;
/// UI state format version.
pub const UI_STATE_FORMAT_VERSION: u32 = 1;
/// UI state kind tag.
pub const UI_STATE_KIND: &str = "mycode-ui-state";
/// Maximum remembered recent projects.
pub const MAX_RECENT_PROJECTS: usize = 16;
/// Maximum folders in one workspace.
pub const MAX_WORKSPACE_ROOTS: usize = 8;
/// Maximum remembered session-to-project bindings.
pub const MAX_SESSION_PROJECTS: usize = 256;
/// Maximum length of one remembered session id.
const MAX_SESSION_ID_BYTES: usize = 64;
/// Maximum length of one remembered project path.
const MAX_PROJECT_PATH_BYTES: usize = 1024;

/// Durable desktop UI state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UiState {
    /// Recent project directories, most recent first.
    pub recent_projects: Vec<String>,
    /// Last opened project directory.
    pub last_project: Option<String>,
    /// Check GitHub releases for updates automatically.
    pub auto_update: bool,
    /// Last selected provider id in the model picker.
    pub selected_provider: Option<String>,
    /// Last selected model id.
    pub selected_model: Option<String>,
    /// Session-to-project bindings (session id, project path), most recent
    /// first. Advisory: restores each chat's tool working directory.
    pub session_projects: Vec<(String, String)>,
    /// Folders currently in the workspace, most recently added first.
    /// The open chat still has one cwd; the other roots are extra tool roots.
    #[serde(default)]
    pub workspace_roots: Vec<String>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            recent_projects: Vec::new(),
            last_project: None,
            auto_update: true,
            selected_provider: None,
            selected_model: None,
            session_projects: Vec::new(),
            workspace_roots: Vec::new(),
        }
    }
}

impl UiState {
    /// Records one project directory as the most recent.
    ///
    /// Invalid, duplicate, or overflowing entries are dropped silently; the
    /// state is advisory and must never fail product flows.
    pub fn touch_project(&mut self, project: &str) {
        let Some(project) = valid_project_path(project) else {
            return;
        };
        self.recent_projects.retain(|existing| existing != &project);
        self.recent_projects.insert(0, project.clone());
        self.recent_projects.truncate(MAX_RECENT_PROJECTS);
        self.last_project = Some(project);
    }

    /// Binds one session to a project directory (upsert, most recent first).
    ///
    /// Invalid ids or paths are dropped silently, like `touch_project`.
    /// Drops one directory from the remembered projects (and last-project
    /// pin when it matches).
    pub fn remove_recent(&mut self, project: &str) {
        self.recent_projects.retain(|existing| existing != project);
        if self.last_project.as_deref() == Some(project) {
            self.last_project = self.recent_projects.first().cloned();
        }
    }

    /// Upserts one session's project binding at the front of the list.
    pub fn set_session_project(&mut self, session_id: &str, project: &str) {
        let Some(project) = valid_project_path(project) else {
            return;
        };
        if !valid_session_id(session_id) {
            return;
        }
        let session_id = session_id.to_owned();
        self.session_projects
            .retain(|(existing, _)| *existing != session_id);
        self.session_projects.insert(0, (session_id, project));
        self.session_projects.truncate(MAX_SESSION_PROJECTS);
    }

    /// The project bound to one session, when remembered.
    #[must_use]
    pub fn project_for_session(&self, session_id: &str) -> Option<&str> {
        self.session_projects
            .iter()
            .find(|(existing, _)| existing == session_id)
            .map(|(_, project)| project.as_str())
    }

    /// Validates the document.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] for any bound
    /// violation.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = || ConfigError::authority_rejection();
        if self.recent_projects.len() > MAX_RECENT_PROJECTS {
            return Err(invalid());
        }
        for project in &self.recent_projects {
            if valid_project_path(project).is_none() {
                return Err(invalid());
            }
        }
        if let Some(project) = &self.last_project
            && valid_project_path(project).is_none()
        {
            return Err(invalid());
        }
        if self.session_projects.len() > MAX_SESSION_PROJECTS {
            return Err(invalid());
        }
        for (session_id, project) in &self.session_projects {
            if !valid_session_id(session_id) || valid_project_path(project).is_none() {
                return Err(invalid());
            }
        }
        if self.workspace_roots.len() > MAX_WORKSPACE_ROOTS {
            return Err(invalid());
        }
        for project in &self.workspace_roots {
            if valid_project_path(project).is_none() {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

fn valid_project_path(value: &str) -> Option<String> {
    if value.is_empty()
        || value.len() > MAX_PROJECT_PATH_BYTES
        || value.chars().any(char::is_control)
        || !std::path::Path::new(value).is_absolute()
    {
        return None;
    }
    Some(value.to_owned())
}

fn valid_session_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_SESSION_ID_BYTES && !value.chars().any(char::is_control)
}

/// Reads the UI state; a missing document yields the defaults.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security or oversized content.
pub fn read_ui_state(home: &crate::HomeLayout) -> Result<UiState, ConfigError> {
    let bytes = read_owned_file(home, UI_STATE_PATH, MAX_UI_STATE_BYTES)?;
    let Some(bytes) = bytes else {
        return Ok(UiState::default());
    };
    let (state, migrated) = decode_ui_state(bytes.as_slice())?;
    if migrated {
        let _ = replace_ui_state(home, &state);
    }
    Ok(state)
}

/// Replaces the UI state under the owned-file lock (no revision CAS; the
/// state is advisory and last-writer-wins).
///
/// # Errors
///
/// Returns [`ConfigError`] for validation or transaction failures.
pub fn replace_ui_state(home: &crate::HomeLayout, state: &UiState) -> Result<(), ConfigError> {
    state.validate()?;
    let bytes = replace_bytes(state)?;
    locked_update_owned_file(home, UI_STATE_PATH, MAX_UI_STATE_BYTES, |_| Ok(bytes))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SerializedUiState {
    format_version: u32,
    kind: String,
    #[serde(flatten)]
    state: UiState,
}

fn replace_bytes(state: &UiState) -> Result<Vec<u8>, ConfigError> {
    let document = SerializedUiState {
        format_version: UI_STATE_FORMAT_VERSION,
        kind: UI_STATE_KIND.to_owned(),
        state: state.clone(),
    };
    let mut bytes = serde_json::to_vec_pretty(&document)
        .map_err(|_| ConfigError::new(ConfigErrorKind::Serialization))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn decode_ui_state(bytes: &[u8]) -> Result<(UiState, bool), ConfigError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Envelope {
        format_version: u32,
        kind: String,
        #[serde(flatten)]
        state: UiState,
    }
    let decoded = crate::json_recover::decode_json::<Envelope>(bytes)?;
    let envelope = decoded.value;
    if envelope.format_version != UI_STATE_FORMAT_VERSION || envelope.kind != UI_STATE_KIND {
        return Err(ConfigError::authority_rejection()
            .with_detail("ui.json: formatVersion or kind does not match this build"));
    }
    envelope.state.validate()?;
    Ok((envelope.state, decoded.migrated))
}
