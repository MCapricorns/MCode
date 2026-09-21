//! Per-session file checkpoints for tool-driven edits.
//!
//! Before a mutating tool (write/edit) touches a file, the host snapshots the
//! current bytes into `<home>/checkpoints/<session-id>/`. Snapshots are
//! content-addressed (blake3), bounded per session, and recorded in an
//! append-only JSONL manifest. Rolling back restores every snapshotted path
//! to its earliest snapshot — the state from before the session touched it.
//!
//! Bounds: 256 snapshots per session, 8 MiB per file. Checkpointing never
//! blocks a tool run on failure: callers treat errors as skip-checkpoint.

use std::path::{Path, PathBuf};

use blake3::Hasher;
use serde::{Deserialize, Serialize};

use crate::{ConfigError, HomeLayout};

/// Maximum snapshots retained per session.
pub(crate) const MAX_SNAPSHOTS_PER_SESSION: usize = 256;
/// Maximum snapshotted file size in bytes.
pub(crate) const MAX_SNAPSHOT_FILE_BYTES: usize = 8 * 1024 * 1024;
/// Exact manifest file name.
pub const MANIFEST_NAME: &str = "manifest.jsonl";

/// One recorded snapshot in a session manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckpointEntry {
    /// Monotonic sequence number within the session.
    pub seq: u64,
    /// Absolute workspace path of the snapshotted file.
    pub path: String,
    /// blake3 digest of the snapshotted bytes.
    pub digest: String,
    /// Snapshotted byte length.
    pub bytes: u64,
}

/// Snapshots one file before mutation.
///
/// Missing files are not snapshotted (creation is not rollback-able through
/// this store) and `Ok(None)` reports that.
///
/// # Errors
///
/// Returns [`ConfigError`] for oversized files, snapshot IO failures, or a
/// corrupted manifest.
pub fn checkpoint_file(
    home: &HomeLayout,
    session_id: &str,
    path: &Path,
) -> Result<Option<CheckpointEntry>, ConfigError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        // A file the tool is about to create is not snapshotted.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ConfigError::authority_rejection()),
    };
    if bytes.len() > MAX_SNAPSHOT_FILE_BYTES {
        return Err(ConfigError::new(crate::ConfigErrorKind::Oversized));
    }
    let mut hasher = Hasher::new();
    hasher.update(&bytes);
    let digest = hasher.finalize().to_hex().to_string();

    let dir = checkpoint_dir(home, session_id);
    std::fs::create_dir_all(&dir).map_err(|_| ConfigError::authority_rejection())?;
    let manifest_path = dir.join(MANIFEST_NAME);
    let entries = read_manifest(&manifest_path)?;
    if entries.len() >= MAX_SNAPSHOTS_PER_SESSION {
        return Err(ConfigError::new(crate::ConfigErrorKind::CheckpointLimit));
    }
    // One snapshot per (path, digest): identical content is never re-stored.
    if entries
        .iter()
        .any(|entry| entry.path == path.as_os_str().to_string_lossy() && entry.digest == digest)
    {
        return Ok(None);
    }
    let seq = entries.last().map(|entry| entry.seq + 1).unwrap_or(1);
    let blob = format!("{seq}-{digest}.bin");
    std::fs::write(dir.join(&blob), &bytes).map_err(|_| ConfigError::authority_rejection())?;
    let entry = CheckpointEntry {
        seq,
        path: path.as_os_str().to_string_lossy().into_owned(),
        digest,
        bytes: bytes.len() as u64,
    };
    append_manifest(&manifest_path, &entry)?;
    Ok(Some(entry))
}

/// Lists one session's snapshots in manifest order.
///
/// # Errors
///
/// Returns [`ConfigError`] for IO or manifest corruption.
pub(crate) fn list_checkpoints(
    home: &HomeLayout,
    session_id: &str,
) -> Result<Vec<CheckpointEntry>, ConfigError> {
    read_manifest(&checkpoint_dir(home, session_id).join(MANIFEST_NAME))
}

/// Restores every snapshotted path to its earliest snapshot.
///
/// Returns the restored absolute paths in restore order. Files that never
/// had a snapshot (new files created during the session) are left alone.
///
/// # Errors
///
/// Returns [`ConfigError`] for IO or manifest corruption; partial restores
/// stop at the first failure.
pub fn rollback_session(home: &HomeLayout, session_id: &str) -> Result<Vec<String>, ConfigError> {
    let entries = list_checkpoints(home, session_id)?;
    let mut earliest: std::collections::HashMap<String, &CheckpointEntry> =
        std::collections::HashMap::new();
    for entry in &entries {
        earliest.entry(entry.path.clone()).or_insert(entry);
    }
    let dir = checkpoint_dir(home, session_id);
    let mut restored = Vec::new();
    for entry in earliest.values() {
        let blob = format!("{}-{}.bin", entry.seq, entry.digest);
        let bytes =
            std::fs::read(dir.join(blob)).map_err(|_| ConfigError::authority_rejection())?;
        std::fs::write(&entry.path, bytes).map_err(|_| ConfigError::authority_rejection())?;
        restored.push(entry.path.clone());
    }
    Ok(restored)
}

fn checkpoint_dir(home: &HomeLayout, session_id: &str) -> PathBuf {
    home.root().join("checkpoints").join(session_id)
}

fn read_manifest(path: &Path) -> Result<Vec<CheckpointEntry>, ConfigError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(Vec::new()),
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| ConfigError::authority_rejection())?;
    let mut entries = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: CheckpointEntry =
            serde_json::from_str(line).map_err(|_| ConfigError::authority_rejection())?;
        entries.push(entry);
    }
    Ok(entries)
}

fn append_manifest(path: &Path, entry: &CheckpointEntry) -> Result<(), ConfigError> {
    use std::io::Write as _;
    let line = serde_json::to_string(entry).map_err(|_| ConfigError::authority_rejection())?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|_| ConfigError::authority_rejection())?;
    writeln!(file, "{line}").map_err(|_| ConfigError::authority_rejection())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> (tempfile::TempDir, HomeLayout) {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        (parent, layout)
    }

    #[test]
    fn checkpoint_snapshot_rollback_roundtrip() {
        let (_parent, home) = layout();
        let workspace = tempfile::tempdir().expect("workspace");
        let file = workspace.path().join("code.rs");
        std::fs::write(&file, "fn a() {}").expect("seed");

        // First edit: snapshot the original.
        let entry = checkpoint_file(&home, "ses1-x", &file)
            .expect("checkpoint")
            .expect("snapshot exists");
        assert_eq!(entry.seq, 1);

        // Identical content is not re-snapshotted.
        assert!(
            checkpoint_file(&home, "ses1-x", &file)
                .expect("checkpoint")
                .is_none()
        );

        // Second edit: snapshot the intermediate state.
        std::fs::write(&file, "fn a() { b(); }").expect("edit");
        let entry = checkpoint_file(&home, "ses1-x", &file)
            .expect("checkpoint")
            .expect("snapshot exists");
        assert_eq!(entry.seq, 2);

        // Overwrite with something unrecoverable, then roll back.
        std::fs::write(&file, "totally destroyed").expect("destroy");
        let restored = rollback_session(&home, "ses1-x").expect("rollback");
        assert_eq!(
            restored,
            vec![file.as_os_str().to_string_lossy().into_owned()]
        );
        assert_eq!(
            std::fs::read_to_string(&file).expect("read"),
            "fn a() {}",
            "rollback restores the earliest snapshot"
        );

        let manifest = list_checkpoints(&home, "ses1-x").expect("manifest");
        assert_eq!(manifest.len(), 2);
    }

    #[test]
    fn missing_files_and_oversized_files_are_rejected_or_skipped() {
        let (_parent, home) = layout();
        let workspace = tempfile::tempdir().expect("workspace");
        let absent = workspace.path().join("absent.txt");
        assert!(
            checkpoint_file(&home, "ses1-x", &absent)
                .expect("missing file skips")
                .is_none()
        );
        let big = workspace.path().join("big.bin");
        std::fs::write(&big, vec![0_u8; MAX_SNAPSHOT_FILE_BYTES + 1]).expect("seed");
        assert!(checkpoint_file(&home, "ses1-x", &big).is_err(), "oversized");
    }
}
