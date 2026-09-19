//! Host-owned shared file capability and kernel.
//!
//! Paths are anchored at an already-opened session cwd directory handle. User
//! components are opened no-follow, one name at a time. Absolute arguments are
//! accepted only when they are lexically and handle-proven inside that cwd.
//! Hidden names are readable and writable; Search ignore policy is not applied.
//! Prepared handles are host-owned and are never re-exported to WASM.
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, ErrorKind};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use tokio_util::sync::CancellationToken;

use crate::builtin::fs_search::{
    SEARCH_TIME_LIMIT, lexical_normalize, posix_relative_key, resolve_relative_argument,
    run_blocking, run_blocking_until, strip_verbatim_prefix, validate_component_name,
};
use crate::builtin::process::ExecutionLease;
use crate::tool::ToolError;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
use windows as sys;

/// Access requested when binding a local file capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileAccess {
    /// Existing regular file opened for content read.
    ExistingContent,
    /// Existing regular file, or a missing leaf under a retained parent.
    ExistingOrMissing,
}

/// Opaque versioned revision token. The encoding is intentionally not
/// documented beyond the `mcode-rev1-` prefix so callers treat it as a cookie.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileRevision(String);

impl FileRevision {
    /// Returns the opaque token string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Test-only constructor for redaction assertions.
    #[cfg(test)]
    pub(crate) fn from_debug_token(token: &str) -> Self {
        Self(token.to_owned())
    }
}

impl std::fmt::Display for FileRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Outcome of a kernel read.
pub struct FileRead {
    /// Windowed UTF-8 text with BOM stripped. A truncation notice may be
    /// appended, and a `[revision ...]` line is always appended.
    pub displayed: String,
    /// True when the tool-level line or byte cap cut the window.
    pub truncated: bool,
    /// Line count of the full decoded file (BOM stripped).
    pub total_lines: usize,
    /// Lines included in `displayed` before any truncation notice.
    pub returned_lines: usize,
    /// Opaque revision covering identity, size, mtime, and raw-byte hash.
    pub revision: FileRevision,
    /// Cwd-relative on-disk spelling used as the path key.
    pub path_key: String,
}

/// Outcome of a kernel write.
#[derive(Debug)]
pub struct FileWrite {
    /// Number of UTF-8 bytes written.
    pub bytes_written: usize,
    /// Opaque revision of the published file.
    pub revision: FileRevision,
    /// True when an existing hardlinked directory entry was replaced by a new inode.
    pub detached_hardlink: bool,
    /// Cwd-relative on-disk spelling used as the path key.
    pub path_key: String,
}

impl std::fmt::Debug for FileRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileRead")
            .field("displayed", &"<redacted>")
            .field("truncated", &self.truncated)
            .field("total_lines", &self.total_lines)
            .field("returned_lines", &self.returned_lines)
            .field("revision", &self.revision)
            .field("path_key", &self.path_key)
            .finish()
    }
}

/// Full-file UTF-8 snapshot for atomic edit. The text includes a leading
/// UTF-8 BOM when the on-disk bytes had one.
pub struct FileSnapshot {
    /// Complete UTF-8 file text, including a leading BOM if present.
    pub text: String,
    /// Opaque revision covering identity, size, mtime, and raw-byte hash.
    pub revision: FileRevision,
    /// Cwd-relative on-disk spelling used as the path key.
    pub path_key: String,
}

impl std::fmt::Debug for FileSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileSnapshot")
            .field("text", &"<redacted>")
            .field("revision", &self.revision)
            .field("path_key", &self.path_key)
            .finish()
    }
}

/// Maximum bytes actually read from one file. Larger metadata sizes fail closed.
pub const MAX_READ_SCAN_BYTES: u64 = 32 * 1024 * 1024;
/// Maximum UTF-8 bytes accepted by one write.
pub const MAX_WRITE_BYTES: usize = 8 * 1024 * 1024;
/// Display line cap retained from the previous read tool.
pub const MAX_LINES: usize = 2000;
/// Display byte cap retained from the previous read tool.
pub const MAX_BYTES: usize = 50 * 1024;
/// Directory-entry names examined while proving unique spelling.
pub(super) const MAX_DIR_WIDTH: usize = 16_384;
/// Write/read chunk size. Cancel is checked between chunks.
pub(super) const WRITE_CHUNK: usize = 64 * 1024;
const MAX_WALK_DEPTH: usize = 256;
const TEMP_ATTEMPTS: usize = 8;
const REVISION_DOMAIN: &str = "mcode-tools file-revision v1";

/// How a child is opened from a retained parent handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChildOpen {
    Directory,
    ExistingFile,
    Probe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FileKind {
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(super) struct FileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    volume: u64,
    #[cfg(windows)]
    file_id: [u8; 16],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FileMeta {
    identity: FileIdentity,
    kind: FileKind,
    size: u64,
    mtime_secs: i64,
    mtime_nsecs: u32,
    nlink: u64,
    unix_mode: u32,
    unix_uid: u32,
    unix_gid: u32,
    #[cfg(windows)]
    windows_attributes: u32,
}

pub(super) struct OpenedChild {
    file: File,
    meta: FileMeta,
    /// Windows temp creation only: duplicate handle that already holds
    /// `DELETE`, moved into the [`TempName`] guard so fail-safe cleanup
    /// survives a later restrictive DACL copied from the source.
    #[cfg(windows)]
    delete_handle: Option<File>,
}

enum PreparedInner {
    Existing {
        parent: File,
        file: File,
        name: OsString,
        meta: FileMeta,
        key: String,
    },
    Missing {
        parent: File,
        remaining: Vec<OsString>,
        key: String,
        parent_identity: FileIdentity,
    },
}

/// Handle-backed file target bound during dispatch preparation.
///
/// A value exists only as a ready retained capability. Dispatch binds
/// [`PreparedFile::key`] and execution takes the inner handles once.
pub struct PreparedFile {
    key: String,
    access: FileAccess,
    inner: Mutex<Option<PreparedInner>>,
}

impl std::fmt::Debug for PreparedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedFile")
            .field("key", &self.key)
            .field("access", &self.access)
            .finish_non_exhaustive()
    }
}

impl PreparedFile {
    /// Returns the cwd-relative on-disk spelling bound to this capability.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Access mode retained by this capability.
    #[must_use]
    pub fn access(&self) -> FileAccess {
        self.access
    }

    fn take_inner(&self) -> Option<PreparedInner> {
        self.inner.lock().ok().and_then(|mut guard| guard.take())
    }
}

pub(super) fn check_cancel(cancel: &CancellationToken) -> io::Result<()> {
    if cancel.is_cancelled() {
        Err(io::Error::new(
            ErrorKind::Interrupted,
            "file operation cancelled before completion",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn map_not_found(error: io::Error) -> io::Error {
    if error.kind() == ErrorKind::NotFound {
        error
    } else if error.raw_os_error() == Some(2) {
        io::Error::new(ErrorKind::NotFound, error)
    } else {
        error
    }
}

fn map_open_error(raw: &str, error: io::Error) -> ToolError {
    if error.kind() == ErrorKind::Interrupted {
        return ToolError::Execution(error.to_string());
    }
    if matches!(
        error.kind(),
        ErrorKind::InvalidInput | ErrorKind::InvalidData
    ) {
        ToolError::InvalidArgs(format!(
            "path escapes the session cwd, crosses a link, or is not a regular file: {raw}"
        ))
    } else if error.kind() == ErrorKind::NotFound {
        ToolError::Execution(format!(
            "file does not exist or is inaccessible: {raw}: {error}"
        ))
    } else {
        ToolError::Execution(format!("file path is inaccessible: {raw}: {error}"))
    }
}

fn absolute_cwd(cwd: &Path) -> Result<PathBuf, ToolError> {
    let cwd = strip_verbatim_prefix(cwd);
    let absolute = if cwd.is_absolute() {
        lexical_normalize(&cwd)
    } else {
        let process_cwd = std::env::current_dir().map_err(|error| {
            ToolError::Execution(format!("process cwd is not accessible: {error}"))
        })?;
        lexical_normalize(&process_cwd.join(&cwd))
    };
    if !absolute.is_absolute()
        || absolute
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(ToolError::InvalidArgs(format!(
            "session cwd is not an absolute resolvable path: {}",
            cwd.display()
        )));
    }
    Ok(absolute)
}

/// Resolves `cwd` and `path` once for retained-capability execution.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] or [`ToolError::InvalidArgs`] when the
/// target cannot be bound, including missing read targets, cancellation, and
/// symlink/reparse/device/ADS/escape failures.
pub fn prepare_file(
    cwd: &Path,
    path: &str,
    cancel: &CancellationToken,
    access: FileAccess,
) -> Result<PreparedFile, ToolError> {
    check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
    let absolute = absolute_cwd(cwd)?;
    let allowed = sys::open_allowed_root(&absolute)
        .map_err(|error| ToolError::Execution(format!("session cwd is not accessible: {error}")))?;
    let relative = resolve_relative_argument(&absolute, None, path)
        .map_err(|()| ToolError::InvalidArgs(format!("path escapes the session cwd: {path}")))?;
    let components: Vec<OsString> = relative
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name.to_os_string()),
            _ => Err(ToolError::InvalidArgs(format!(
                "path escapes the session cwd: {path}"
            ))),
        })
        .collect::<Result<_, _>>()?;
    if components.len() > MAX_WALK_DEPTH {
        return Err(ToolError::InvalidArgs(
            "path exceeds the maximum component depth".to_owned(),
        ));
    }
    if components.is_empty() {
        return Err(ToolError::InvalidArgs(
            "path must name a file inside the session cwd".to_owned(),
        ));
    }
    walk_prepare(allowed, components, path, cancel, access)
}

fn walk_prepare(
    allowed: File,
    components: Vec<OsString>,
    raw: &str,
    cancel: &CancellationToken,
    access: FileAccess,
) -> Result<PreparedFile, ToolError> {
    let mut parent = allowed;
    let mut proven = PathBuf::new();
    let last = components.len() - 1;
    for (index, name) in components.iter().enumerate() {
        check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
        validate_component_name(name).map_err(|error| map_open_error(raw, error))?;
        let is_last = index == last;
        let how = if is_last {
            ChildOpen::Probe
        } else {
            ChildOpen::Directory
        };
        match sys::open_child(&parent, name, how) {
            Ok(opened) => {
                if !is_last {
                    if opened.meta.kind != FileKind::Directory {
                        return Err(ToolError::InvalidArgs(format!(
                            "path escapes the session cwd, crosses a link, or is not a regular file: {raw}"
                        )));
                    }
                    let exact = sys::unique_component_name(&parent, opened.meta.identity, cancel)
                        .map_err(|error| map_open_error(raw, error))?;
                    proven.push(exact);
                    parent = opened.file;
                    continue;
                }
                if opened.meta.kind != FileKind::File {
                    return Err(ToolError::InvalidArgs(format!(
                        "path is not a regular file: {raw}"
                    )));
                }
                let exact = sys::unique_component_name(&parent, opened.meta.identity, cancel)
                    .map_err(|error| map_open_error(raw, error))?;
                proven.push(&exact);
                let key = posix_relative_key(&proven);
                return Ok(PreparedFile {
                    key: key.clone(),
                    access,
                    inner: Mutex::new(Some(PreparedInner::Existing {
                        parent,
                        file: opened.file,
                        name: exact,
                        meta: opened.meta,
                        key,
                    })),
                });
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                if access != FileAccess::ExistingOrMissing {
                    return Err(map_open_error(raw, error));
                }
                let remaining = components[index..].to_vec();
                let mut key_path = proven;
                for part in &remaining {
                    key_path.push(part);
                }
                let parent_meta =
                    sys::current_meta(&parent).map_err(|error| map_open_error(raw, error))?;
                let key = posix_relative_key(&key_path);
                return Ok(PreparedFile {
                    key: key.clone(),
                    access,
                    inner: Mutex::new(Some(PreparedInner::Missing {
                        parent,
                        remaining,
                        key,
                        parent_identity: parent_meta.identity,
                    })),
                });
            }
            Err(error) => return Err(map_open_error(raw, error)),
        }
    }
    Err(ToolError::InvalidArgs(format!(
        "path must name a file inside the session cwd: {raw}"
    )))
}

/// [`prepare_file`] on the cancellable supervisor thread.
///
/// # Errors
///
/// Same as [`prepare_file`], plus cancellation and deadline expiry.
pub async fn prepare_file_async(
    cwd: PathBuf,
    path: String,
    cancel: CancellationToken,
    access: FileAccess,
) -> Result<PreparedFile, ToolError> {
    run_blocking(
        "file dispatch preflight",
        &cancel,
        SEARCH_TIME_LIMIT,
        move |worker_cancel| prepare_file(&cwd, &path, &worker_cancel, access),
    )
    .await
}

fn bind_prepared(
    prepared: Option<&PreparedFile>,
    cwd: &Path,
    path: &str,
    cancel: &CancellationToken,
    access: FileAccess,
) -> Result<PreparedInner, ToolError> {
    if let Some(prepared) = prepared {
        if prepared.access() != access {
            return Err(ToolError::Execution(format!(
                "prepared file access mismatch: prepared {:?}, requested {:?}",
                prepared.access(),
                access
            )));
        }
        let Some(inner) = prepared.take_inner() else {
            return Err(ToolError::Execution(
                "prepared file capability is missing or was already consumed".to_owned(),
            ));
        };
        let key = match &inner {
            PreparedInner::Existing { key, .. } | PreparedInner::Missing { key, .. } => {
                key.as_str()
            }
        };
        if key != prepared.key() {
            return Err(ToolError::Execution(
                "prepared file capability does not match its path key".to_owned(),
            ));
        }
        return Ok(inner);
    }
    let prepared = prepare_file(cwd, path, cancel, access)?;
    prepared.take_inner().ok_or_else(|| {
        ToolError::Execution(
            "prepared file capability is missing or was already consumed".to_owned(),
        )
    })
}

fn content_hash(raw: &[u8]) -> [u8; 32] {
    *blake3::hash(raw).as_bytes()
}

fn revision_token(meta: &FileMeta, raw_hash: &[u8; 32]) -> FileRevision {
    let mut hasher = blake3::Hasher::new_derive_key(REVISION_DOMAIN);
    hasher.update(b"v1\0");
    #[cfg(unix)]
    {
        hasher.update(&meta.identity.device.to_le_bytes());
        hasher.update(&meta.identity.inode.to_le_bytes());
    }
    #[cfg(windows)]
    {
        hasher.update(&meta.identity.volume.to_le_bytes());
        hasher.update(&meta.identity.file_id);
    }
    hasher.update(&meta.size.to_le_bytes());
    hasher.update(&meta.mtime_secs.to_le_bytes());
    hasher.update(&meta.mtime_nsecs.to_le_bytes());
    hasher.update(raw_hash);
    FileRevision(format!("mcode-rev1-{}", hasher.finalize().to_hex()))
}

fn reject_encoding(raw: &[u8]) -> Result<&str, ToolError> {
    if raw.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) || raw.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        return Err(ToolError::Execution(
            "UTF-32 encoded files are not supported".to_owned(),
        ));
    }
    if raw.starts_with(&[0xFE, 0xFF]) || raw.starts_with(&[0xFF, 0xFE]) {
        return Err(ToolError::Execution(
            "UTF-16 encoded files are not supported".to_owned(),
        ));
    }
    std::str::from_utf8(raw).map_err(|_| ToolError::Execution("file is not valid UTF-8".to_owned()))
}

/// Selects the `[start, end)` line window under the [`MAX_LINES`] cap.
///
/// `offset`/`limit` are user-controlled, so all arithmetic saturates: the
/// returned invariant is `start <= capped_end <= end <= total_lines`, and
/// `selected` never exceeds [`MAX_LINES`] entries.
fn window_text(
    displayed: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> (String, usize, usize, bool) {
    let total_lines = displayed.lines().count();
    let start = offset.unwrap_or(1).saturating_sub(1).min(total_lines);
    let end = match limit {
        Some(limit) => start.saturating_add(limit).min(total_lines),
        None => total_lines,
    };
    let capped_end = end.min(start.saturating_add(MAX_LINES));
    let selected: Vec<&str> = displayed
        .lines()
        .skip(start)
        .take(capped_end - start)
        .collect();
    let text = selected.join("\n");
    let (mut text, byte_truncated) = crate::builtin::truncate_bytes(&text, MAX_BYTES);
    let truncated = byte_truncated || capped_end < end;
    if truncated {
        text.push_str(&format!(
            "\n[output truncated: showing lines {}-{} of {total_lines}; re-invoke with offset/limit to read more]",
            start + 1,
            start + selected.len(),
        ));
    }
    (text, total_lines, selected.len(), truncated)
}

fn append_revision(body: &str, revision: &FileRevision) -> String {
    if body.is_empty() {
        format!("[revision {revision}]")
    } else {
        format!("{body}\n[revision {revision}]")
    }
}

struct RawFile {
    raw: Vec<u8>,
    key: String,
    revision: FileRevision,
}

fn read_raw_file(
    prepared: Option<&PreparedFile>,
    cwd: &Path,
    path: &str,
    cancel: &CancellationToken,
) -> Result<RawFile, ToolError> {
    check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
    let inner = bind_prepared(prepared, cwd, path, cancel, FileAccess::ExistingContent)?;
    let PreparedInner::Existing {
        parent,
        mut file,
        name,
        meta,
        key,
    } = inner
    else {
        return Err(ToolError::Execution(format!(
            "file does not exist or is inaccessible: {path}"
        )));
    };
    if meta.size > MAX_READ_SCAN_BYTES {
        return Err(ToolError::Execution(
            "file exceeds the read size limit".to_owned(),
        ));
    }
    let listed = sys::open_child(&parent, &name, ChildOpen::ExistingFile)
        .map_err(|error| map_open_error(path, error))?;
    if listed.meta.identity != meta.identity {
        return Err(ToolError::Execution(
            "file identity changed before read".to_owned(),
        ));
    }
    drop(listed);
    let before = sys::current_meta(&file).map_err(|error| map_open_error(path, error))?;
    if before.identity != meta.identity
        || before.size != meta.size
        || before.mtime_secs != meta.mtime_secs
        || before.mtime_nsecs != meta.mtime_nsecs
    {
        return Err(ToolError::Execution(
            "file identity changed before read".to_owned(),
        ));
    }
    let raw = sys::read_exact_capped(&mut file, before.size, MAX_READ_SCAN_BYTES, cancel).map_err(
        |error| {
            if error.kind() == ErrorKind::Interrupted {
                ToolError::Execution(error.to_string())
            } else {
                ToolError::Execution(format!("failed to read {path}: {error}"))
            }
        },
    )?;
    let after = sys::current_meta(&file).map_err(|error| map_open_error(path, error))?;
    if after.identity != before.identity
        || after.size != before.size
        || after.mtime_secs != before.mtime_secs
        || after.mtime_nsecs != before.mtime_nsecs
        || after.size != raw.len() as u64
    {
        return Err(ToolError::Execution(
            "file identity or size changed during read".to_owned(),
        ));
    }
    let revision = revision_token(&after, &content_hash(&raw));
    Ok(RawFile { raw, key, revision })
}

/// Reads a prepared (or internally prepared) UTF-8 file.
///
/// # Errors
///
/// Returns [`ToolError`] when the capability is missing, encoding is rejected,
/// the file exceeds [`MAX_READ_SCAN_BYTES`], identity changes, or the call is
/// cancelled. Cancel and timeout never return a partial window.
pub fn read_file(
    prepared: Option<&PreparedFile>,
    cwd: &Path,
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    cancel: &CancellationToken,
) -> Result<FileRead, ToolError> {
    let raw = read_raw_file(prepared, cwd, path, cancel)?;
    let text = reject_encoding(&raw.raw)?;
    let displayed = text.strip_prefix('\u{feff}').unwrap_or(text);
    let (window, total_lines, returned_lines, truncated) = window_text(displayed, offset, limit);
    Ok(FileRead {
        displayed: append_revision(&window, &raw.revision),
        truncated,
        total_lines,
        returned_lines,
        revision: raw.revision,
        path_key: raw.key,
    })
}

/// Reads a prepared (or internally prepared) UTF-8 file as a full snapshot.
///
/// Unlike [`read_file`], this does not window or strip a UTF-8 BOM. Cancel
/// and timeout never return a partial snapshot.
///
/// # Errors
///
/// Same as [`read_file`].
pub fn read_file_snapshot(
    prepared: Option<&PreparedFile>,
    cwd: &Path,
    path: &str,
    cancel: &CancellationToken,
) -> Result<FileSnapshot, ToolError> {
    let raw = read_raw_file(prepared, cwd, path, cancel)?;
    let text = reject_encoding(&raw.raw)?.to_owned();
    Ok(FileSnapshot {
        text,
        revision: raw.revision,
        path_key: raw.key,
    })
}

/// Snapshot-reads on the cancellable supervisor.
///
/// # Errors
///
/// Same as [`read_file_snapshot`].
pub async fn read_file_snapshot_async(
    prepared: Option<std::sync::Arc<PreparedFile>>,
    cwd: PathBuf,
    path: String,
    cancel: CancellationToken,
) -> Result<FileSnapshot, ToolError> {
    let deadline = Instant::now() + SEARCH_TIME_LIMIT;
    run_blocking_until("file snapshot", &cancel, deadline, move |worker_cancel| {
        read_file_snapshot(prepared.as_deref(), &cwd, &path, &worker_cancel)
    })
    .await
}

/// Reads on the cancellable supervisor. Cancel/timeout do not return partial data.
///
/// # Errors
///
/// Same as [`read_file`].
pub async fn read_file_async(
    prepared: Option<std::sync::Arc<PreparedFile>>,
    cwd: PathBuf,
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
    cancel: CancellationToken,
) -> Result<FileRead, ToolError> {
    let deadline = Instant::now() + SEARCH_TIME_LIMIT;
    run_blocking_until("file read", &cancel, deadline, move |worker_cancel| {
        read_file(
            prepared.as_deref(),
            &cwd,
            &path,
            offset,
            limit,
            &worker_cancel,
        )
    })
    .await
}

static WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(test)]
type TempLinksHook = std::sync::Arc<dyn Fn() + Send + Sync>;

/// Test-only observer invoked after payload and probe names are linked.
#[cfg(test)]
static TEMP_LINKS_HOOK: Mutex<Option<TempLinksHook>> = Mutex::new(None);

/// Test-only observer invoked at the final pre-publish cancel gate.
///
/// The hook receives the path key of the target being written so a test can
/// act only on its own write while other tests run concurrently
/// in the same process.
#[cfg(test)]
static PRE_PUBLISH_HOOK: Mutex<Option<PublishHook>> = Mutex::new(None);

/// Test-only observer invoked after publish, before verification.
///
/// The hook receives the path key of the published target so a test can act
/// only on its own write while other tests run concurrently.
#[cfg(test)]
static POST_PUBLISH_HOOK: Mutex<Option<PublishHook>> = Mutex::new(None);

#[cfg(test)]
type PublishHook = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

/// Clears the installed test hook when its fixture leaves scope.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct TempLinksHookGuard;

#[cfg(test)]
impl Drop for TempLinksHookGuard {
    fn drop(&mut self) {
        if let Ok(mut hook) = TEMP_LINKS_HOOK.lock() {
            *hook = None;
        }
    }
}

/// Installs one test observer for the pre-write linked-name boundary.
#[cfg(test)]
pub(crate) fn install_temp_links_hook(hook: TempLinksHook) -> TempLinksHookGuard {
    let mut slot = TEMP_LINKS_HOOK
        .lock()
        .expect("temporary link test hook lock must not be poisoned");
    assert!(slot.replace(hook).is_none(), "test hook already installed");
    TempLinksHookGuard
}

#[cfg(test)]
fn run_temp_links_hook() {
    let hook = TEMP_LINKS_HOOK
        .lock()
        .expect("temporary link test hook lock must not be poisoned")
        .clone();
    if let Some(hook) = hook {
        hook();
    }
}

/// Clears the installed pre-publish test hook when its fixture leaves scope.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct PrePublishHookGuard;

#[cfg(test)]
impl Drop for PrePublishHookGuard {
    fn drop(&mut self) {
        if let Ok(mut hook) = PRE_PUBLISH_HOOK.lock() {
            *hook = None;
        }
    }
}

/// Serializes tests that install the process-global pre-publish hook.
#[cfg(test)]
pub(crate) fn serialize_pre_publish_tests() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Installs one test observer for the final pre-publish cancel gate.
#[cfg(test)]
pub(crate) fn install_pre_publish_hook(hook: PublishHook) -> PrePublishHookGuard {
    let mut slot = PRE_PUBLISH_HOOK
        .lock()
        .expect("pre-publish test hook lock must not be poisoned");
    assert!(slot.replace(hook).is_none(), "test hook already installed");
    PrePublishHookGuard
}

#[cfg(test)]
fn run_pre_publish_hook(key: &str) {
    let hook = PRE_PUBLISH_HOOK
        .lock()
        .expect("pre-publish test hook lock must not be poisoned")
        .clone();
    if let Some(hook) = hook {
        hook(key);
    }
}

/// Clears the installed post-publish test hook when its fixture leaves scope.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct PostPublishHookGuard;

#[cfg(test)]
impl Drop for PostPublishHookGuard {
    fn drop(&mut self) {
        if let Ok(mut hook) = POST_PUBLISH_HOOK.lock() {
            *hook = None;
        }
    }
}

/// Installs one test observer for the post-publish verification boundary.
#[cfg(test)]
pub(crate) fn install_post_publish_hook(hook: PublishHook) -> PostPublishHookGuard {
    let mut slot = POST_PUBLISH_HOOK
        .lock()
        .expect("post-publish test hook lock must not be poisoned");
    assert!(slot.replace(hook).is_none(), "test hook already installed");
    PostPublishHookGuard
}

#[cfg(test)]
fn run_post_publish_hook(key: &str) {
    let hook = POST_PUBLISH_HOOK
        .lock()
        .expect("post-publish test hook lock must not be poisoned")
        .clone();
    if let Some(hook) = hook {
        hook(key);
    }
}

/// Test-only unlink fault: `unlink_child` fails for a matching parent
/// directory identity (optionally one exact name) instead of calling the
/// kernel. The fault is keyed to a single directory so concurrently
/// running tests that write elsewhere are unaffected.
#[cfg(all(test, unix))]
struct UnlinkFault {
    dir: FileIdentity,
    name: Option<OsString>,
}

#[cfg(all(test, unix))]
static UNLINK_FAULT: Mutex<Option<UnlinkFault>> = Mutex::new(None);

/// Serializes unlink-fault fixtures: the injected fault is process-global, so
/// a second concurrent install would trip the misuse assert instead of
/// testing its own cleanup path.
#[cfg(all(test, unix))]
static UNLINK_FAULT_SERIAL: Mutex<()> = Mutex::new(());

/// Clears the installed unlink fault when its fixture leaves scope.
#[cfg(all(test, unix))]
pub(crate) struct UnlinkFaultGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
}

#[cfg(all(test, unix))]
impl Drop for UnlinkFaultGuard {
    fn drop(&mut self) {
        *lock_unlink_fault() = None;
    }
}

/// Locks the injected-fault slot, recovering from a poisoned mutex: the slot
/// holds one `Option`, so a panic that left it poisoned must not turn every
/// later unlink into a second panic.
#[cfg(all(test, unix))]
fn lock_unlink_fault() -> std::sync::MutexGuard<'static, Option<UnlinkFault>> {
    UNLINK_FAULT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Installs a test fault that fails unlinks under `dir` (all names, or one
/// exact `name`), so cleanup failure handling can be exercised
/// deterministically.
///
/// # Errors
///
/// Returns an I/O error when `dir` cannot be opened or stat.
#[cfg(all(test, unix))]
pub(crate) fn install_unlink_fault_under(
    dir: &Path,
    name: Option<&OsStr>,
) -> io::Result<UnlinkFaultGuard> {
    let serial = UNLINK_FAULT_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = sys::open_allowed_root(dir)?;
    let meta = sys::current_meta(&root)?;
    let mut fault = lock_unlink_fault();
    assert!(fault.is_none(), "an unlink fault is already installed");
    *fault = Some(UnlinkFault {
        dir: meta.identity,
        name: name.map(OsStr::to_os_string),
    });
    drop(fault);
    Ok(UnlinkFaultGuard { _serial: serial })
}

/// Returns the injected failure when `parent`/`name` match the fault.
#[cfg(all(test, unix))]
fn unlink_fault(parent: &File, name: &OsStr) -> Option<io::Error> {
    let fault = lock_unlink_fault();
    let fault = fault.as_ref()?;
    if fault.name.as_ref().is_some_and(|want| want != name) {
        return None;
    }
    let meta = sys::current_meta(parent).ok()?;
    (meta.identity == fault.dir).then(|| io::Error::other("injected mcode unlink failure"))
}

/// Test-only Windows delete fault: `mark_delete` fails for temps created
/// under one parent directory identity so concurrently running tests that
/// write elsewhere are unaffected.
#[cfg(all(test, windows))]
struct DeleteFault {
    dir: FileIdentity,
    temps: Vec<FileIdentity>,
}

#[cfg(all(test, windows))]
static DELETE_FAULT: Mutex<Option<DeleteFault>> = Mutex::new(None);

/// Clears the installed delete fault when its fixture leaves scope.
#[cfg(all(test, windows))]
#[derive(Debug)]
pub(crate) struct DeleteFaultGuard;

#[cfg(all(test, windows))]
impl Drop for DeleteFaultGuard {
    fn drop(&mut self) {
        if let Ok(mut fault) = DELETE_FAULT.lock() {
            *fault = None;
        }
    }
}

/// Installs a test fault that fails `mark_delete` for temps created under
/// `dir`.
///
/// # Errors
///
/// Returns an I/O error when `dir` cannot be opened or stat.
#[cfg(all(test, windows))]
pub(crate) fn install_delete_fault_under(dir: &Path) -> io::Result<DeleteFaultGuard> {
    let root = sys::open_allowed_root(dir)?;
    let meta = sys::current_meta(&root)?;
    let mut fault = DELETE_FAULT
        .lock()
        .expect("delete fault lock must not be poisoned");
    assert!(fault.is_none(), "a delete fault is already installed");
    *fault = Some(DeleteFault {
        dir: meta.identity,
        temps: Vec::new(),
    });
    Ok(DeleteFaultGuard)
}

/// Records a just-created temp so a matching delete fault can target it.
#[cfg(all(test, windows))]
fn note_delete_fault_temp(parent: &File, file: &File) {
    let Ok(mut fault) = DELETE_FAULT.lock() else {
        return;
    };
    let Some(fault) = fault.as_mut() else {
        return;
    };
    let Ok(parent_meta) = sys::current_meta(parent) else {
        return;
    };
    if parent_meta.identity != fault.dir {
        return;
    }
    if let Ok(meta) = sys::current_meta(file) {
        fault.temps.push(meta.identity);
    }
}

/// Returns the injected failure when `file` was created under the faulted
/// directory.
#[cfg(all(test, windows))]
fn delete_fault(file: &File) -> Option<io::Error> {
    let fault = DELETE_FAULT
        .lock()
        .expect("delete fault lock must not be poisoned");
    let fault = fault.as_ref()?;
    let meta = sys::current_meta(file).ok()?;
    fault
        .temps
        .contains(&meta.identity)
        .then(|| io::Error::other("injected mcode delete failure"))
}

fn write_lock() -> &'static Mutex<()> {
    WRITE_LOCK.get_or_init(|| Mutex::new(()))
}

fn temp_name() -> OsString {
    OsString::from(format!(
        "mcode-write-{}.tmp",
        uuid::Uuid::new_v4().as_simple()
    ))
}

struct TempName {
    parent: File,
    name: OsString,
    persist: bool,
    /// Never-written probe name (Unix mode probe / Windows inherited-DACL
    /// probe). Success paths remove it explicitly and fallibly before
    /// reporting success; [`Drop`] is only the unwind fallback.
    probe: Option<OsString>,
    /// Windows-only creation-time handle for the security probe. Copy and
    /// delete run through this handle so a restrictive inherited DACL cannot
    /// force a by-name reopen.
    #[cfg(windows)]
    probe_file: Option<File>,
    /// Windows-only creation-time duplicate holding `DELETE`; see [`Drop`].
    #[cfg(windows)]
    delete_handle: Option<File>,
}

impl TempName {
    /// Unlinks the never-written probe, disarming its cleanup only after
    /// the unlink succeeds.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the probe unlink fails.
    fn remove_probe(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        {
            let Some(handle) = self.probe_file.as_ref() else {
                self.probe = None;
                return Ok(());
            };
            match sys::mark_delete(handle) {
                Ok(()) => {
                    self.probe_file = None;
                    self.probe = None;
                    Ok(())
                }
                Err(error) => Err(error),
            }
        }
        #[cfg(not(windows))]
        {
            let Some(probe) = self.probe.as_ref() else {
                return Ok(());
            };
            match sys::unlink_child(&self.parent, probe) {
                Ok(()) => {
                    self.probe = None;
                    Ok(())
                }
                Err(error) => Err(error),
            }
        }
    }

    /// Removes unpublished probe/payload names and reports cleanup errors.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when a mandatory unlink or disposition fails.
    fn cleanup(&mut self) -> io::Result<()> {
        let mut probe_error = None;
        if self.probe.is_some()
            && let Err(error) = self.remove_probe()
        {
            probe_error = Some(error);
        }
        if self.persist {
            return match probe_error {
                Some(error) => Err(error),
                None => Ok(()),
            };
        }
        let unpublished = {
            #[cfg(windows)]
            {
                if let Some(handle) = self.delete_handle.as_ref() {
                    match sys::mark_delete(handle) {
                        Ok(()) => {
                            self.delete_handle = None;
                            Ok(())
                        }
                        Err(error) => Err(error),
                    }
                } else {
                    sys::unlink_child(&self.parent, &self.name)
                }
            }
            #[cfg(not(windows))]
            {
                sys::unlink_child(&self.parent, &self.name)
            }
        };
        match unpublished {
            Ok(()) => {
                self.persist = true;
                match probe_error {
                    Some(error) => Err(error),
                    None => Ok(()),
                }
            }
            Err(error) => {
                let message = match probe_error {
                    Some(probe) => {
                        format!("{probe}; failed to remove rejected temporary file: {error}")
                    }
                    None => format!("failed to remove rejected temporary file: {error}"),
                };
                Err(io::Error::new(error.kind(), message))
            }
        }
    }
}

fn fold_cleanup_error(primary: ToolError, cleanup: io::Result<()>) -> ToolError {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => ToolError::Execution(format!("{primary}; {cleanup}")),
    }
}

fn complete_temp<T>(mut temp: TempName, result: Result<T, ToolError>) -> Result<T, ToolError> {
    match result {
        Ok(value) => Ok(value),
        Err(primary) => Err(fold_cleanup_error(primary, temp.cleanup())),
    }
}

impl Drop for TempName {
    fn drop(&mut self) {
        // `complete_temp` reports cleanup failures. This is only a
        // panic/unwind fallback, where Drop cannot return another error.
        let _ = self.cleanup();
    }
}

/// Writes `content` through a prepared capability.
///
/// Missing targets are create-only. Existing targets require `expected_revision`
/// or `overwrite`. Both together are rejected. This is process-local compare-
/// and-swap; cooperating writers in this process are serialized on a global
/// write mutex. Advisory locks are not claimed as protection against foreign
/// writers. The payload inode stays private from creation through the
/// publish rename; final modes are restored afterwards through the retained
/// temp handle, so a mode-restoration failure is reported after the
/// replacement already happened. A final cancel gate runs immediately before
/// the irreversible publish rename, and the published name is verified to
/// still be the just-published inode carrying exactly the written content.
/// Mandatory cleanups (the never-written mode probe, the post-`linkat` temp
/// name) must succeed before success is reported, so residue is never
/// silently presented as a successful write.
///
/// # Errors
///
/// Returns [`ToolError`] on permission-style path failures, stale revisions,
/// cancellation (including the final pre-publish gate), publish failure,
/// verified-cleanup failure, or post-publish verification failure. Failure
/// before publish leaves the original file untouched.
pub fn write_file(
    prepared: Option<&PreparedFile>,
    cwd: &Path,
    path: &str,
    content: &str,
    expected_revision: Option<&str>,
    overwrite: bool,
    cancel: &CancellationToken,
) -> Result<FileWrite, ToolError> {
    if content.len() > MAX_WRITE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "write content exceeds {MAX_WRITE_BYTES} bytes"
        )));
    }
    if expected_revision.is_some() && overwrite {
        return Err(ToolError::InvalidArgs(
            "expected_revision and overwrite cannot both be set".to_owned(),
        ));
    }
    check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
    let inner = bind_prepared(prepared, cwd, path, cancel, FileAccess::ExistingOrMissing)?;
    let _guard = write_lock()
        .lock()
        .map_err(|_| ToolError::Execution("file write lock poisoned".to_owned()))?;
    check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
    match inner {
        PreparedInner::Existing {
            parent,
            file,
            name,
            meta,
            key,
        } => write_existing(
            parent,
            file,
            name,
            meta,
            key,
            content,
            expected_revision,
            overwrite,
            cancel,
        ),
        PreparedInner::Missing {
            parent,
            remaining,
            key,
            parent_identity,
        } => {
            if expected_revision.is_some() {
                return Err(ToolError::InvalidArgs(
                    "expected_revision cannot be used when the target does not exist".to_owned(),
                ));
            }
            write_missing(parent, remaining, key, parent_identity, content, cancel)
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "existing write needs parent, target, spelling, metadata, and CAS inputs together"
)]
fn write_existing(
    parent: File,
    existing: File,
    name: OsString,
    prepared_meta: FileMeta,
    key: String,
    content: &str,
    expected_revision: Option<&str>,
    overwrite: bool,
    cancel: &CancellationToken,
) -> Result<FileWrite, ToolError> {
    let listed = sys::open_child(&parent, &name, ChildOpen::ExistingFile).map_err(|error| {
        ToolError::Execution(format!("file identity changed before write: {error}"))
    })?;
    if listed.meta.identity != prepared_meta.identity {
        return Err(ToolError::Execution(
            "file identity changed before write".to_owned(),
        ));
    }
    drop(listed);
    let live = sys::current_meta(&existing)
        .map_err(|error| ToolError::Execution(format!("failed to stat {key}: {error}")))?;
    if live.identity != prepared_meta.identity {
        return Err(ToolError::Execution(
            "file identity changed before write".to_owned(),
        ));
    }
    if !overwrite {
        let expected = expected_revision.ok_or_else(|| {
            ToolError::InvalidArgs(
                "refusing to overwrite an existing file without expected_revision or overwrite=true"
                    .to_owned(),
            )
        })?;
        let mut reader = existing
            .try_clone()
            .map_err(|error| ToolError::Execution(format!("failed to reopen {key}: {error}")))?;
        let raw = sys::read_exact_capped(&mut reader, live.size, MAX_READ_SCAN_BYTES, cancel)
            .map_err(|error| ToolError::Execution(format!("failed to hash {key}: {error}")))?;
        let current = revision_token(&live, &content_hash(&raw));
        if current.as_str() != expected {
            return Err(ToolError::Execution(
                "stale expected_revision; re-read the file and retry or pass overwrite=true"
                    .to_owned(),
            ));
        }
    }
    let detached_hardlink = live.nlink > 1;
    if detached_hardlink {
        let proven =
            sys::unique_component_name(&parent, live.identity, cancel).map_err(|error| {
                ToolError::Execution(format!("cannot prove hardlink path: {error}"))
            })?;
        if proven != name {
            return Err(ToolError::Execution(
                "hardlink path cannot be uniquely proven".to_owned(),
            ));
        }
    }
    let (created, mut temp) = create_temp_in(&parent, cancel)?;
    #[cfg_attr(
        not(windows),
        expect(unused_mut, reason = "Windows drops the source handle before publish")
    )]
    let mut existing = Some(existing);
    let result = (|| {
        let temp_identity = created.meta.identity;
        let expected_hash = content_hash(content.as_bytes());
        #[cfg(test)]
        run_temp_links_hook();
        let temp_file = write_temp(created.file, content.as_bytes(), cancel)?;
        // Windows copies the source DACL and attributes (including a possible
        // read-only bit) onto the temp before publish so cleanup mirrors the
        // source; Unix keeps the payload private through the rename and
        // restores the source mode/owner afterwards on the retained handle.
        #[cfg(windows)]
        {
            preserve_existing(
                &live,
                existing.as_ref().expect("existing handle"),
                &temp_file,
            )?;
            drop(existing.take());
        }
        // All irreversible pre-publish work is complete. The final cancel gate
        // runs immediately before the rename so a cancelled or timed-out call
        // can never publish while its supervisor reports cancellation.
        #[cfg(test)]
        run_pre_publish_hook(&key);
        check_cancel(cancel).map_err(|error| {
            ToolError::Execution(format!("cancelled before publishing {key}: {error}"))
        })?;
        sys::publish_replace(&parent, &temp_file, &temp.name, &name)
            .map_err(|error| ToolError::Execution(format!("failed to publish {key}: {error}")))?;
        temp.persist = true;
        #[cfg(unix)]
        preserve_existing(
            &live,
            existing.as_ref().expect("existing handle"),
            &temp_file,
        )?;
        sys::sync_parent(&parent).map_err(|error| {
            ToolError::Execution(format!("failed to sync parent of {key}: {error}"))
        })?;
        close_share_denying_handles(&mut temp, temp_file);
        #[cfg(test)]
        run_post_publish_hook(&key);
        finish_write(
            &parent,
            &name,
            temp_identity,
            expected_hash,
            content.len(),
            key,
            detached_hardlink,
            cancel,
        )
    })();
    complete_temp(temp, result)
}

fn write_missing(
    mut parent: File,
    remaining: Vec<OsString>,
    key: String,
    parent_identity: FileIdentity,
    content: &str,
    cancel: &CancellationToken,
) -> Result<FileWrite, ToolError> {
    let live_parent = sys::current_meta(&parent).map_err(|error| {
        ToolError::Execution(format!("failed to stat parent of {key}: {error}"))
    })?;
    if live_parent.identity != parent_identity {
        return Err(ToolError::Execution(
            "parent directory identity changed before write".to_owned(),
        ));
    }
    if remaining.is_empty() {
        return Err(ToolError::InvalidArgs(
            "path must name a file inside the session cwd".to_owned(),
        ));
    }
    let dest = remaining.last().cloned().expect("remaining is non-empty");
    for dir_name in &remaining[..remaining.len() - 1] {
        check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
        validate_component_name(dir_name)
            .map_err(|error| ToolError::InvalidArgs(error.to_string()))?;
        let opened = sys::ensure_directory(&parent, dir_name).map_err(|error| {
            ToolError::Execution(format!("failed to create directory: {error}"))
        })?;
        parent = opened.file;
    }
    validate_component_name(&dest).map_err(|error| ToolError::InvalidArgs(error.to_string()))?;
    if sys::open_child(&parent, &dest, ChildOpen::Probe).is_ok() {
        return Err(ToolError::Execution(
            "refusing to overwrite a file that appeared after create-only prepare".to_owned(),
        ));
    }
    let (created, mut temp) = create_temp_in(&parent, cancel)?;
    let result = (|| {
        let temp_identity = created.meta.identity;
        let expected_hash = content_hash(content.as_bytes());
        // The payload inode stays private from creation through the rename; a
        // separate never-written probe learns the umask/default-ACL effective
        // mode and is unlinked by the guard on every path.
        #[cfg(unix)]
        let new_file_mode = attach_mode_probe(&parent, &mut temp, cancel)?;
        #[cfg(windows)]
        attach_security_probe(&parent, &mut temp, cancel)?;
        // The deterministic test observer opens every inode visible to a
        // foreign reader before the payload receives any content.
        #[cfg(test)]
        run_temp_links_hook();
        let temp_file = write_temp(created.file, content.as_bytes(), cancel)?;
        // All pre-publish work is complete; the final cancel gate runs
        // immediately before the irreversible publish rename.
        #[cfg(test)]
        run_pre_publish_hook(&key);
        check_cancel(cancel).map_err(|error| {
            ToolError::Execution(format!("cancelled before publishing {key}: {error}"))
        })?;
        sys::publish_create_only(&parent, &temp_file, &temp.name, &dest)
            .map_err(|error| ToolError::Execution(format!("failed to create {key}: {error}")))?;
        temp.persist = true;
        #[cfg(unix)]
        sys::apply_new_file_mode(&temp_file, new_file_mode).map_err(|error| {
            ToolError::Execution(format!("failed to set new file mode: {error}"))
        })?;
        #[cfg(windows)]
        apply_probed_security(&temp, &temp_file)?;
        sys::sync_parent(&parent).map_err(|error| {
            ToolError::Execution(format!("failed to sync parent of {key}: {error}"))
        })?;
        temp.remove_probe().map_err(|error| {
            ToolError::Execution(format!(
                "failed to remove the mode probe after publishing {key}: {error}"
            ))
        })?;
        close_share_denying_handles(&mut temp, temp_file);
        #[cfg(test)]
        run_post_publish_hook(&key);
        finish_write(
            &parent,
            &dest,
            temp_identity,
            expected_hash,
            content.len(),
            key,
            false,
            cancel,
        )
    })();
    complete_temp(temp, result)
}

fn create_temp_in(
    parent: &File,
    cancel: &CancellationToken,
) -> Result<(OpenedChild, TempName), ToolError> {
    // Clone before any named create. Once `sys::create_temp` succeeds, the
    // guard can be assembled without another fallible operation.
    let retained_parent = parent.try_clone().map_err(|error| {
        ToolError::Execution(format!("failed to retain parent handle: {error}"))
    })?;
    for _ in 0..TEMP_ATTEMPTS {
        check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
        let name = temp_name();
        match sys::create_temp(parent, &name) {
            Ok(opened) => {
                // The Windows DELETE duplicate moves into the guard; the
                // writer keeps the file handle and creation metadata.
                return Ok((
                    OpenedChild {
                        file: opened.file,
                        meta: opened.meta,
                        #[cfg(windows)]
                        delete_handle: None,
                    },
                    TempName {
                        parent: retained_parent,
                        name,
                        persist: false,
                        probe: None,
                        #[cfg(windows)]
                        probe_file: None,
                        #[cfg(windows)]
                        delete_handle: opened.delete_handle,
                    },
                ));
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(ToolError::Execution(format!(
                    "failed to create temporary file: {error}"
                )));
            }
        }
    }
    Err(ToolError::Execution(
        "failed to create an exclusive temporary file".to_owned(),
    ))
}

/// Writes `bytes` to the private temp file, flushes, and synchronizes it.
///
/// Mode transitions are deliberately not performed here: the payload inode
/// stays private (`0600` on Unix) from creation until after the publish
/// rename, so no window exposes written content at the temp name. Callers
/// restore final modes on the retained handle after a successful publish.
///
/// # Errors
///
/// Returns [`ToolError`] when writing, flushing, syncing, or a cancel check
/// fails.
fn write_temp(mut file: File, bytes: &[u8], cancel: &CancellationToken) -> Result<File, ToolError> {
    sys::write_all_sync(&mut file, bytes, cancel).map_err(|error| {
        if error.kind() == ErrorKind::Interrupted {
            ToolError::Execution(error.to_string())
        } else {
            ToolError::Execution(format!("failed to write temporary file: {error}"))
        }
    })?;
    Ok(file)
}

/// Creates the never-written `0666` mode probe next to the payload temp.
///
/// The kernel applies the process umask and any parent default ACL to the
/// probe, so its recorded mode is exactly what a plain `0666` create in
/// that directory yields; it is applied to the published payload through
/// its retained handle after the rename. The probe never receives payload
/// bytes, so a foreign observer holding it can only ever read an empty
/// file. The linked name is recorded on `temp`; success paths remove it
/// explicitly and report a failed removal, while [`TempName::drop`] is the
/// best-effort fallback for failure paths.
///
/// # Errors
///
/// Returns [`ToolError`] when the exclusive probe create keeps colliding,
/// fails, or the call is cancelled.
#[cfg(unix)]
fn attach_mode_probe(
    parent: &File,
    temp: &mut TempName,
    cancel: &CancellationToken,
) -> Result<u32, ToolError> {
    for _ in 0..TEMP_ATTEMPTS {
        check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
        let name = temp_name();
        match sys::create_mode_probe(parent, &name) {
            Ok(mode) => {
                temp.probe = Some(name);
                return Ok(mode);
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(ToolError::Execution(format!(
                    "failed to probe the effective new file mode: {error}"
                )));
            }
        }
    }
    Err(ToolError::Execution(
        "failed to create an exclusive mode probe file".to_owned(),
    ))
}

/// Creates a never-written probe that inherits the parent directory DACL.
///
/// # Errors
///
/// Returns [`ToolError`] when exclusive create keeps colliding, fails, or
/// the call is cancelled.
#[cfg(windows)]
fn attach_security_probe(
    parent: &File,
    temp: &mut TempName,
    cancel: &CancellationToken,
) -> Result<(), ToolError> {
    for _ in 0..TEMP_ATTEMPTS {
        check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
        let name = temp_name();
        match windows::create_security_probe(parent, &name) {
            Ok(file) => {
                temp.probe = Some(name);
                temp.probe_file = Some(file);
                return Ok(());
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(ToolError::Execution(format!(
                    "failed to probe the inherited file security: {error}"
                )));
            }
        }
    }
    Err(ToolError::Execution(
        "failed to create an exclusive security probe file".to_owned(),
    ))
}

/// Copies the probe's inherited DACL/owner onto the just-published payload.
#[cfg(windows)]
fn apply_probed_security(temp: &TempName, dst: &File) -> Result<(), ToolError> {
    let Some(probe) = temp.probe_file.as_ref() else {
        return Ok(());
    };
    let meta = windows::current_meta(probe).map_err(|error| {
        ToolError::Execution(format!("failed to stat the security probe: {error}"))
    })?;
    windows::copy_safe_mode(&meta, probe, dst).map_err(|error| {
        ToolError::Execution(format!(
            "failed to restore inherited file security: {error}"
        ))
    })
}

/// Closes handles whose share mode would block post-publish reopen-by-name.
fn close_share_denying_handles(temp: &mut TempName, temp_file: File) {
    #[cfg(windows)]
    drop(temp.delete_handle.take());
    #[cfg(not(windows))]
    let _ = temp;
    drop(temp_file);
}

/// Copies the preserved permission state of an existing target onto `dst`.
///
/// Windows runs this before publish (the temp then mirrors the source's
/// DACL/attributes, which cleanup must survive); Unix runs it after publish
/// through the retained temp handle so the payload is never exposed with
/// the source's readable mode before the rename.
///
/// # Errors
///
/// Returns [`ToolError`] when the mode/owner or DACL/attribute copy fails.
fn preserve_existing(meta: &FileMeta, src: &File, dst: &File) -> Result<(), ToolError> {
    #[cfg(unix)]
    {
        let _ = src;
        unix::copy_safe_mode(meta, dst)
            .map_err(|error| ToolError::Execution(format!("failed to preserve file mode: {error}")))
    }
    #[cfg(windows)]
    {
        windows::copy_safe_mode(meta, src, dst).map_err(|error| {
            ToolError::Execution(format!("failed to preserve DACL or attributes: {error}"))
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (meta, src, dst);
        Err(ToolError::Execution(
            "file write is not implemented on this platform".to_owned(),
        ))
    }
}

/// Verifies the just-published target and derives its revision.
///
/// The reopened name must still resolve to the published temp inode
/// (`published_identity`, recorded at temp creation) and its content must
/// hash to `expected_hash`, so a foreign replacement of the published
/// name — even with same-length content — is reported as a failure instead
/// of generating a revision for content this write never wrote.
///
/// # Errors
///
/// Returns [`ToolError`] when the call is cancelled, the published name
/// cannot be reopened, the reopened inode is not the published temp, or
/// its size or content hash does not match the written content.
#[expect(
    clippy::too_many_arguments,
    reason = "verification needs the parent, published name and identity, expected hash and size, key, and CAS result together"
)]
fn finish_write(
    parent: &File,
    name: &OsStr,
    published_identity: FileIdentity,
    expected_hash: [u8; 32],
    bytes_written: usize,
    key: String,
    detached_hardlink: bool,
    cancel: &CancellationToken,
) -> Result<FileWrite, ToolError> {
    check_cancel(cancel).map_err(|error| ToolError::Execution(error.to_string()))?;
    let mut published = sys::open_child(parent, name, ChildOpen::ExistingFile)
        .map_err(|error| ToolError::Execution(format!("failed to reopen {key}: {error}")))?;
    if published.meta.identity != published_identity {
        return Err(ToolError::Execution(
            "published file was replaced before verification".to_owned(),
        ));
    }
    let raw = sys::read_exact_capped(
        &mut published.file,
        published.meta.size,
        MAX_READ_SCAN_BYTES.max(bytes_written as u64),
        cancel,
    )
    .map_err(|error| ToolError::Execution(format!("failed to hash written file: {error}")))?;
    if raw.len() != bytes_written {
        return Err(ToolError::Execution(
            "published file size does not match written content".to_owned(),
        ));
    }
    if content_hash(&raw) != expected_hash {
        return Err(ToolError::Execution(
            "published file content does not match written content".to_owned(),
        ));
    }
    let revision = revision_token(&published.meta, &content_hash(&raw));
    Ok(FileWrite {
        bytes_written,
        revision,
        detached_hardlink,
        path_key: key,
    })
}

/// Writes on the cancellable supervisor while holding `lease`.
///
/// Publication always takes a real [`ExecutionLease`]. Dropping the caller
/// future does not release `lease` while the worker may still publish.
///
/// # Errors
///
/// Same as [`write_file`].
#[expect(
    clippy::too_many_arguments,
    reason = "lease rides with the existing write arguments onto one worker"
)]
pub(crate) async fn write_file_with_lease(
    prepared: Option<std::sync::Arc<PreparedFile>>,
    cwd: PathBuf,
    path: String,
    content: String,
    expected_revision: Option<String>,
    overwrite: bool,
    lease: ExecutionLease,
    cancel: CancellationToken,
) -> Result<FileWrite, ToolError> {
    let deadline = Instant::now() + SEARCH_TIME_LIMIT;
    run_blocking_until("file write", &cancel, deadline, move |worker_cancel| {
        let _lease = lease;
        write_file(
            prepared.as_deref(),
            &cwd,
            &path,
            &content,
            expected_revision.as_deref(),
            overwrite,
            &worker_cancel,
        )
    })
    .await
}

#[cfg(not(any(unix, windows)))]
mod sys {
    use super::*;
    pub(super) fn open_allowed_root(_: &Path) -> io::Result<File> {
        Err(io::Error::new(
            ErrorKind::Unsupported,
            "handle-relative file IO is not implemented on this platform",
        ))
    }
    pub(super) fn open_child(_: &File, _: &OsStr, _: ChildOpen) -> io::Result<OpenedChild> {
        Err(io::Error::new(
            ErrorKind::Unsupported,
            "handle-relative file IO is not implemented on this platform",
        ))
    }
    pub(super) fn unique_component_name(
        _: &File,
        _: FileIdentity,
        _: &CancellationToken,
    ) -> io::Result<OsString> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn current_meta(_: &File) -> io::Result<FileMeta> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn read_exact_capped(
        _: &mut File,
        _: u64,
        _: u64,
        _: &CancellationToken,
    ) -> io::Result<Vec<u8>> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn create_temp(_: &File, _: &OsStr) -> io::Result<OpenedChild> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn write_all_sync(_: &mut File, _: &[u8], _: &CancellationToken) -> io::Result<()> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn unlink_child(_: &File, _: &OsStr) -> io::Result<()> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn publish_replace(_: &File, _: &File, _: &OsStr, _: &OsStr) -> io::Result<()> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn publish_create_only(_: &File, _: &File, _: &OsStr, _: &OsStr) -> io::Result<()> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn sync_parent(_: &File) -> io::Result<()> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
    pub(super) fn ensure_directory(_: &File, _: &OsStr) -> io::Result<OpenedChild> {
        Err(io::Error::new(ErrorKind::Unsupported, "unsupported"))
    }
}
