//! Product data export/import: one JSON bundle carrying settings, UI state,
//! todos, and session ledgers between machines. Secrets never travel — the
//! vault stays local and keys must be re-entered after an import.

use std::path::{Path, PathBuf};

use mycode_config::{
    AppSettings, AuthorityRevision, HomeLayout, UiState, read_app_settings, read_ui_state,
    replace_app_settings, replace_ui_state,
};
use serde::{Deserialize, Serialize};

/// Current bundle schema version.
pub const EXPORT_FORMAT_VERSION: u32 = 1;
/// Bundle kind marker.
pub const EXPORT_KIND: &str = "mycode-export";
/// Sessions carried by one bundle.
pub const MAX_EXPORT_SESSIONS: usize = 64;
/// Files per exported session directory.
const MAX_SESSION_FILES: usize = 16;
/// Bytes per exported session.
const MAX_SESSION_BYTES: usize = 512 * 1024;
/// Largest accepted bundle file.
const MAX_BUNDLE_BYTES: u64 = 48 * 1024 * 1024;
/// Todos carried by one bundle.
const MAX_EXPORT_TODOS: usize = 256;

/// One session directory's text files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportedSession {
    /// Session id (directory name).
    pub session_id: String,
    /// File name to file body pairs.
    pub files: Vec<(String, String)>,
}

/// The full bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportBundle {
    pub format_version: u32,
    pub kind: String,
    pub exported_at_unix: u64,
    /// Strict settings document.
    pub settings: AppSettings,
    /// UI state document.
    pub ui_state: UiState,
    /// Todo documents by session id, file bodies verbatim.
    pub todos: Vec<(String, String)>,
    /// Session ledger directories.
    pub sessions: Vec<ExportedSession>,
}

/// What an export wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportSummary {
    pub sessions: usize,
    pub todos: usize,
    pub bytes: usize,
}

/// What an import applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportSummary {
    pub settings: bool,
    pub ui_state: bool,
    pub todos: usize,
    pub sessions: usize,
}

fn sessions_root(home: &HomeLayout) -> Result<PathBuf, String> {
    home.owned_join("plugins/session/data/sessions")
        .map_err(|_| "sessions directory unavailable".to_owned())
}

/// A safe bundle file name: plain, non-empty, no separators.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains(['/', '\\', ':', '\0'])
        && name != "."
        && name != ".."
        && !name.chars().any(char::is_control)
}

/// Collects the product data into one bundle.
///
/// # Errors
///
/// Returns a rendered message for unreadable or oversized inputs.
pub fn build_bundle(home: &HomeLayout) -> Result<ExportBundle, String> {
    let settings = read_app_settings(home).map_err(|error| format!("settings: {error}"))?;
    let ui_state = read_ui_state(home).map_err(|error| format!("ui state: {error}"))?;
    let mut todos = Vec::new();
    let workspace = home.root().join("workspace");
    if let Ok(entries) = std::fs::read_dir(&workspace) {
        for entry in entries.flatten() {
            if todos.len() >= MAX_EXPORT_TODOS {
                break;
            }
            let path = entry.path().join("todos.json");
            if let Ok(body) = std::fs::read_to_string(&path)
                && let Some(session_id) = entry.file_name().to_str()
                && plain_name(session_id)
            {
                todos.push((session_id.to_owned(), body));
            }
        }
    }
    let mut sessions = Vec::new();
    let root = sessions_root(home)?;
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            if sessions.len() >= MAX_EXPORT_SESSIONS {
                break;
            }
            let Some(session_id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !plain_name(&session_id) {
                continue;
            }
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let mut files = Vec::new();
            let mut budget = MAX_SESSION_BYTES;
            if let Ok(dir_entries) = std::fs::read_dir(&dir) {
                for file in dir_entries.flatten() {
                    if files.len() >= MAX_SESSION_FILES || budget == 0 {
                        break;
                    }
                    let path = file.path();
                    let Some(name) = file.file_name().to_str().map(str::to_owned) else {
                        continue;
                    };
                    if !plain_name(&name) || !path.is_file() {
                        continue;
                    }
                    if let Ok(meta) = file.metadata()
                        && meta.len() <= budget as u64
                        && let Ok(body) = std::fs::read_to_string(&path)
                    {
                        budget -= body.len();
                        files.push((name, body));
                    }
                }
            }
            if !files.is_empty() {
                sessions.push(ExportedSession { session_id, files });
            }
        }
    }
    Ok(ExportBundle {
        format_version: EXPORT_FORMAT_VERSION,
        kind: EXPORT_KIND.to_owned(),
        exported_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default(),
        settings,
        ui_state,
        todos,
        sessions,
    })
}

/// Writes one bundle to a user-chosen path.
///
/// # Errors
///
/// Returns a rendered message when the bundle cannot be serialized or
/// written.
pub fn export_to_file(home: &HomeLayout, path: &Path) -> Result<ExportSummary, String> {
    let bundle = build_bundle(home)?;
    let summary = ExportSummary {
        sessions: bundle.sessions.len(),
        todos: bundle.todos.len(),
        bytes: 0,
    };
    let mut body =
        serde_json::to_vec_pretty(&bundle).map_err(|error| format!("encode: {error}"))?;
    body.push(b'\n');
    std::fs::write(path, &body).map_err(|error| format!("write: {error}"))?;
    Ok(ExportSummary {
        bytes: body.len(),
        ..summary
    })
}

/// Applies one bundle: settings and UI state replace local values; todos and
/// sessions only fill gaps, never overwrite.
///
/// # Errors
///
/// Returns a rendered message for a malformed bundle or a failed write.
pub fn import_from_file(home: &HomeLayout, path: &Path) -> Result<ImportSummary, String> {
    let meta = std::fs::metadata(path).map_err(|error| format!("read: {error}"))?;
    if meta.len() > MAX_BUNDLE_BYTES {
        return Err("bundle exceeds the size limit".to_owned());
    }
    let body = std::fs::read(path).map_err(|error| format!("read: {error}"))?;
    let bundle: ExportBundle =
        serde_json::from_slice(&body).map_err(|error| format!("bundle: {error}"))?;
    if bundle.format_version != EXPORT_FORMAT_VERSION || bundle.kind != EXPORT_KIND {
        return Err("not a MYCode export bundle".to_owned());
    }
    bundle
        .settings
        .validate()
        .map_err(|error| format!("settings: {error}"))?;

    // Settings replace under CAS against the current revision.
    let current = read_current_revision(home)?;
    replace_app_settings(home, current, &bundle.settings)
        .map_err(|error| format!("settings: {error}"))?;
    replace_ui_state(home, &bundle.ui_state).map_err(|error| format!("ui state: {error}"))?;

    let mut todos_applied = 0usize;
    for (session_id, body) in &bundle.todos {
        if !plain_name(session_id) || body.len() > mycode_config::MAX_SETTINGS_BYTES {
            continue;
        }
        let target = home
            .root()
            .join("workspace")
            .join(session_id)
            .join("todos.json");
        if target.exists() {
            continue;
        }
        if let Some(parent) = target.parent()
            && std::fs::create_dir_all(parent).is_ok()
            && std::fs::write(&target, body).is_ok()
        {
            todos_applied += 1;
        }
    }

    let mut sessions_applied = 0usize;
    let root = sessions_root(home)?;
    for session in bundle.sessions.iter().take(MAX_EXPORT_SESSIONS) {
        if !plain_name(&session.session_id) {
            continue;
        }
        let dir = root.join(&session.session_id);
        if dir.exists() || session.files.is_empty() {
            continue;
        }
        let mut written = false;
        if std::fs::create_dir_all(&dir).is_ok() {
            written = true;
            for (name, body) in session.files.iter().take(MAX_SESSION_FILES) {
                if !plain_name(name)
                    || body.len() > MAX_SESSION_BYTES
                    || std::fs::write(dir.join(name), body).is_err()
                {
                    written = false;
                    break;
                }
            }
        }
        if written {
            sessions_applied += 1;
        } else {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    Ok(ImportSummary {
        settings: true,
        ui_state: true,
        todos: todos_applied,
        sessions: sessions_applied,
    })
}

/// Reads the current settings revision for the import's CAS write.
fn read_current_revision(home: &HomeLayout) -> Result<AuthorityRevision, String> {
    let path = home.root().join(mycode_config::SETTINGS_PATH);
    if !path.exists() {
        return Ok(AuthorityRevision::ABSENT);
    }
    let body = std::fs::read(&path).map_err(|error| format!("settings: {error}"))?;
    #[derive(serde::Deserialize)]
    struct Header {
        #[serde(rename = "formatVersion")]
        format_version: u32,
        revision: u64,
    }
    let header: Header =
        serde_json::from_slice(&body).map_err(|_| "settings: unreadable".to_owned())?;
    if header.format_version != mycode_config::SETTINGS_FORMAT_VERSION {
        return Err("settings: unknown format".to_owned());
    }
    AuthorityRevision::new(header.revision).map_err(|_| "settings: bad revision".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> (tempfile::TempDir, HomeLayout) {
        let parent = tempfile::tempdir().expect("parent");
        let home = HomeLayout::from_root(parent.path().join("home")).expect("home");
        (parent, home)
    }

    #[test]
    fn bundle_roundtrips_through_a_file() {
        let (_parent, home) = layout();
        replace_ui_state(&home, &UiState::default()).expect("ui");
        let export_path = home.root().with_extension("export.json");
        let summary = export_to_file(&home, &export_path).expect("export");
        assert_eq!(summary.sessions, 0);
        let imported = import_from_file(&home, &export_path).expect("import");
        assert!(imported.settings && imported.ui_state);
        assert_eq!(imported.sessions, 0);
        std::fs::remove_file(&export_path).ok();
    }

    #[test]
    fn foreign_bundle_is_rejected() {
        let (_parent, home) = layout();
        let path = home.root().with_extension("foreign.json");
        std::fs::write(&path, br#"{"formatVersion":1,"kind":"other"}"#).expect("write");
        assert!(import_from_file(&home, &path).is_err());
        std::fs::remove_file(&path).ok();
    }
}
