//! Handle-relative directory walk for grep/find.
//!
//! Enumeration and ignore-file reads use retained directory handles.
//! The visitor receives that same parent handle plus the listed name so
//! later parent-relative no-follow opens never rebuild a path string or
//! re-walk ancestors from the selected target root. Nested git roots drop
//! outer Gitignore/GitExclude layers; `.ignore` layers stay. Ignore state is
//! a persistent `Arc` linked list so adding a layer is O(1) and ancestor
//! frames keep the previous head. Listings are buffered up to a width cap,
//! decorated once with the lossy rendered component key, sorted with the
//! original `OsString` as the complete tie-break, and visited best-first by the
//! full rendered path. Resolution and walk share one [`WalkLimiter`],
//! including the handle budget.
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use tokio_util::sync::CancellationToken;

use super::{
    EntryKind, HandleLease, IoErrors, MAX_GIT_PARENT_HOPS, NameMatch, ParentDirectory,
    PathOrderKey, ResolvedRoot, WalkLimiter, child_name_in_parent, files_same_identity,
    is_hidden_skip, lossy_component, open_child_file, open_directory_nofollow,
    open_parent_directory, to_posix,
};

/// Buffer for one `NtQueryDirectoryFile(ReturnSingleEntry = true)` result.
///
/// 64 KiB is well above the Windows component limit while keeping one fixed,
/// bounded allocation per live directory listing.
#[cfg(windows)]
const DIR_LIST_SINGLE_U64S: usize = 8192;

/// Hard cap on bytes loaded from one `.ignore`, `.gitignore`, or git
/// exclude file. Real ignore files are tiny; a larger value would let a
/// huge or sparse ignore pin unbounded memory after the search deadline
/// has already cancelled the worker token. Oversized files fail closed.
pub(crate) const IGNORE_FILE_MAX_BYTES: usize = 1024 * 1024;

/// Ignore-file read size. Checking cancel and the deadline between these
/// kernel reads is what stops `spawn_blocking` from running `read_to_end`
/// after the outer timeout has fired. 8 KiB keeps the check frequent
/// without a syscall per byte.
const IGNORE_READ_CHUNK: usize = 8 * 1024;

/// Visits every non-hidden, non-ignored file and directory under the
/// retained target handle.
///
/// The walk never re-opens `root.root` by path. Directory listings and
/// ignore files are read through handle-relative opens. The visitor is
/// given the retained parent directory handle and the exact listed name
/// so a later open cannot re-parse the relative path or re-walk names
/// from the target root. Frontier directories are ordered by the next
/// child's full rendered path, using the same lossy component key as the
/// listing sort, so a match-count stop is the globally smallest rendered
/// top-N. Other budgets (time, entries, handles) can still stop earlier.
/// Live directory handles are charged to the shared invocation limiter.
/// Exhausted and empty directory frames drop immediately so only the
/// best-first frontier retains charged walk handles.
pub(crate) fn walk_retained_tree(
    root: &ResolvedRoot,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
    io_errors: &IoErrors,
    mut visit: impl FnMut(&Path, &OsStr, EntryKind, &File) -> ignore::WalkState,
) -> io::Result<()> {
    let _seams = super::bind_current_limiter(&root.limiter);
    if root.is_file() {
        return Ok(());
    }
    let target = match root.target.file.try_clone() {
        Ok(file) => file,
        Err(error) => {
            io_errors.record(".", &error);
            return Ok(());
        }
    };
    let Ok(lease) = root.limiter.lease() else {
        return Ok(());
    };
    let listing = match collect_listing(&target, limiter, cancel) {
        Ok(listing) => listing,
        Err(error) => {
            io_errors.record(".", &error);
            return Ok(());
        }
    };
    if listing.is_empty() {
        return Ok(());
    }
    let mut frames: Vec<Option<WalkFrame>> = vec![Some(WalkFrame {
        dir: target,
        rel: PathBuf::new(),
        ignores: root.ignores.clone(),
        listing,
        next: 0,
        _lease: lease,
    })];
    let mut heap = BinaryHeap::new();
    if let Some(path) = peek_child_path(frames[0].as_ref()) {
        heap.push(Reverse((path, 0usize)));
    }
    while let Some(Reverse((_, frame_idx))) = heap.pop() {
        if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
            return Ok(());
        }
        let Some(frame) = frames.get_mut(frame_idx).and_then(Option::as_mut) else {
            continue;
        };
        if frame.next >= frame.listing.len() {
            frames[frame_idx] = None;
            continue;
        }
        let entry = frame.listing[frame.next].clone();
        frame.next += 1;
        if entry.skip || is_dot(&entry.name) || name_is_hidden(&entry.name) || entry.hidden_attr {
            requeue_or_release(&mut frames, &mut heap, frame_idx);
            continue;
        }
        let parent_rel = frame.rel.clone();
        let parent_ignores = frame.ignores.clone();
        let parent_dir = match frame.dir.try_clone() {
            Ok(file) => file,
            Err(error) => {
                io_errors.record(&rel_label(&frame.rel), &error);
                requeue_or_release(&mut frames, &mut heap, frame_idx);
                continue;
            }
        };
        requeue_or_release(&mut frames, &mut heap, frame_idx);
        let kind = match entry.kind {
            Some(kind) => kind,
            None => match probe_kind(&parent_dir, &entry.name) {
                Ok(Some(kind)) => kind,
                Ok(None) => continue,
                Err(error) => {
                    let child = join_rel(&parent_rel, &entry.name);
                    io_errors.record(&to_posix(&child), &error);
                    continue;
                }
            },
        };
        let child_rel = join_rel(&parent_rel, &entry.name);
        if parent_ignores.is_ignored(
            &root.target_relative,
            &child_rel,
            kind == EntryKind::Directory,
        ) {
            continue;
        }
        if matches!(
            visit(&child_rel, &entry.name, kind, &parent_dir),
            ignore::WalkState::Quit
        ) {
            return Ok(());
        }
        if kind != EntryKind::Directory {
            continue;
        }
        if child_rel.components().count() >= limiter.max_walk_depth() {
            limiter.stop("walk depth limit reached");
            return Ok(());
        }
        let mut child_ignores = parent_ignores;
        let allowed_rel = join_rel(&root.target_relative, &child_rel);
        let child_dir = match root.open_descended_dir(&parent_dir, &entry.name) {
            Ok(child_dir) => child_dir,
            Err(error) if is_hidden_skip(&error) => continue,
            Err(error) => {
                io_errors.record(&to_posix(&child_rel), &error);
                continue;
            }
        };
        if let Err(error) = child_ignores.ingest(&child_dir, &allowed_rel, limiter, cancel) {
            return Err(io::Error::other(format!(
                "search ignore files cannot be loaded at {}: {error}",
                to_posix(&child_rel)
            )));
        }
        let Ok(child_lease) = root.limiter.lease() else {
            return Ok(());
        };
        let listing = match collect_listing(&child_dir, limiter, cancel) {
            Ok(listing) => listing,
            Err(error) => {
                io_errors.record(&to_posix(&child_rel), &error);
                continue;
            }
        };
        if listing.is_empty() {
            continue;
        }
        let child_idx = frames.len();
        frames.push(Some(WalkFrame {
            dir: child_dir,
            rel: child_rel,
            ignores: child_ignores,
            listing,
            next: 0,
            _lease: child_lease,
        }));
        if let Some(path) = peek_child_path(frames[child_idx].as_ref()) {
            heap.push(Reverse((path, child_idx)));
        }
    }
    Ok(())
}

fn requeue_or_release(
    frames: &mut [Option<WalkFrame>],
    heap: &mut BinaryHeap<Reverse<(PathOrderKey, usize)>>,
    frame_idx: usize,
) {
    let done = match frames.get(frame_idx).and_then(Option::as_ref) {
        Some(frame) => frame.next >= frame.listing.len(),
        None => true,
    };
    if done {
        if let Some(slot) = frames.get_mut(frame_idx) {
            *slot = None;
        }
        return;
    }
    if let Some(path) = peek_child_path(frames[frame_idx].as_ref()) {
        heap.push(Reverse((path, frame_idx)));
    }
}

fn collect_listing(
    dir: &File,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<Vec<ListedName>> {
    let mut listing = DirListing::new();
    let mut names = Vec::new();
    loop {
        match listing.next(dir, limiter, cancel)? {
            Some(entry) => names.push(entry),
            None => return Ok(names),
        }
    }
}

fn sort_listing(entries: Vec<ListedName>) -> Vec<ListedName> {
    let mut decorated: Vec<_> = entries
        .into_iter()
        .map(|entry| {
            (
                (lossy_component(&entry.name).into_owned(), entry.name),
                (entry.kind, entry.skip, entry.hidden_attr),
            )
        })
        .collect();
    decorated.sort_by(|left, right| left.0.cmp(&right.0));
    decorated
        .into_iter()
        .map(|((_, name), (kind, skip, hidden_attr))| ListedName {
            name,
            kind,
            skip,
            hidden_attr,
        })
        .collect()
}

fn peek_child_path(frame: Option<&WalkFrame>) -> Option<PathOrderKey> {
    let frame = frame?;
    let entry = frame.listing.get(frame.next)?;
    Some(PathOrderKey::from_path(&join_rel(&frame.rel, &entry.name)))
}

struct WalkFrame {
    dir: File,
    rel: PathBuf,
    ignores: IgnoreStack,
    listing: Vec<ListedName>,
    next: usize,
    _lease: HandleLease,
}

/// Ignore-file class used by [`ignore::WalkBuilder`] precedence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IgnoreFileKind {
    /// `.ignore`, highest ignore-file precedence.
    Ignore,
    /// `.gitignore`, applied only after a `.git` ancestor is seen.
    Gitignore,
    /// `.git/info/exclude`.
    GitExclude,
}

#[derive(Clone, Debug)]
struct IgnoreLayer {
    base: PathBuf,
    /// Path from this ignore file's directory to the allowed root.
    ///
    /// Empty for layers at or below the session cwd. Ancestor Git layers
    /// prepend this so `repo/.gitignore` matches `subdir/file` when cwd
    /// is `repo/subdir`.
    ancestor_prefix: PathBuf,
    kind: IgnoreFileKind,
    matcher: Gitignore,
}

/// Persistent ignore-layer node. Pushing a layer is an `Arc` allocation of
/// one node; ancestors keep the previous head without copying the chain.
#[derive(Clone, Debug)]
struct IgnoreNode {
    layer: IgnoreLayer,
    parent: Option<Arc<IgnoreNode>>,
}

/// Shared ignore-layer chain. Cloning a frame is two `Arc` bumps.
#[derive(Clone, Debug, Default)]
pub(super) struct IgnoreStack {
    git_enabled: bool,
    ignore_head: Option<Arc<IgnoreNode>>,
    git_head: Option<Arc<IgnoreNode>>,
}

impl IgnoreStack {
    /// Discovers the Git boundary above `allowed` and seeds ancestor layers.
    ///
    /// Walks parent directories through handle-relative `..` until a `.git`
    /// entry, the filesystem root, or [`MAX_GIT_PARENT_HOPS`]. A `.git` file
    /// is parsed for `gitdir:` and `commondir`. Failure to establish a
    /// boundary after a `.git` entry is seen, or after a parent hop is
    /// refused, is terminating.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when a parent cannot be opened, a `.git` file
    /// cannot be parsed, or `commondir` / `info/exclude` cannot be loaded.
    pub(super) fn seed_git_boundary(
        &mut self,
        allowed: &File,
        limiter: &WalkLimiter,
        cancel: &CancellationToken,
    ) -> io::Result<()> {
        if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
            return Err(stopped_error(limiter));
        }
        match probe_git_entry(allowed) {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Err(error),
        }

        let mut current = allowed.try_clone()?;
        let mut names_to_cwd: Vec<OsString> = Vec::new();
        let mut hops = 0usize;
        loop {
            hops += 1;
            if hops > MAX_GIT_PARENT_HOPS {
                return Err(io::Error::other(
                    "search ignore files cannot be loaded: git boundary is too deep",
                ));
            }
            let parent = match open_parent_directory(&current)? {
                ParentDirectory::FilesystemRoot => return Ok(()),
                ParentDirectory::Parent(parent) => parent,
            };
            if files_same_identity(&parent, &current)? {
                return Ok(());
            }
            let child_name = child_name_in_parent(&parent, &current, limiter, cancel)?;
            names_to_cwd.insert(0, child_name);
            match probe_git_entry(&parent) {
                Ok(Some(kind)) => {
                    apply_git_root(self, &parent, kind, &names_to_cwd, limiter, cancel)?;
                    return load_ancestor_gitignores(self, &parent, &names_to_cwd, limiter, cancel);
                }
                Ok(None) => {}
                Err(error) if is_not_found(&error) => {}
                Err(error) => return Err(error),
            }
            current = parent;
        }
    }

    pub(super) fn ingest(
        &mut self,
        dir: &File,
        allowed_rel: &Path,
        limiter: &WalkLimiter,
        cancel: &CancellationToken,
    ) -> io::Result<()> {
        if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
            return Err(stopped_error(limiter));
        }
        match probe_git_entry(dir) {
            Ok(Some(kind)) => {
                if self.git_enabled {
                    // Nested git root: WalkBuilder stops outer Gitignore and git
                    // exclude at this boundary. `.ignore` layers keep stacking.
                    self.git_head = None;
                }
                apply_git_root(self, dir, kind, &[], limiter, cancel)?;
            }
            Ok(None) => {}
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Err(error),
        }
        if self.git_enabled
            && let Some(text) = read_child_text(dir, ".gitignore", limiter, cancel)?
        {
            self.push_layer(
                allowed_rel.to_path_buf(),
                PathBuf::new(),
                &text,
                IgnoreFileKind::Gitignore,
                limiter,
            )?;
        }
        if let Some(text) = read_child_text(dir, ".ignore", limiter, cancel)? {
            self.push_layer(
                allowed_rel.to_path_buf(),
                PathBuf::new(),
                &text,
                IgnoreFileKind::Ignore,
                limiter,
            )?;
        }
        Ok(())
    }

    fn push_layer(
        &mut self,
        base: PathBuf,
        ancestor_prefix: PathBuf,
        text: &str,
        kind: IgnoreFileKind,
        limiter: &WalkLimiter,
    ) -> io::Result<()> {
        limiter.try_reserve_ignore_layer()?;
        let mut builder = GitignoreBuilder::new(".");
        for (index, line) in text.lines().enumerate() {
            let line = if index == 0 {
                line.trim_start_matches('\u{feff}')
            } else {
                line
            };
            limiter.try_reserve_ignore_rule()?;
            builder.add_line(None, line).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid ignore rule on line {}: {error}", index + 1),
                )
            })?;
        }
        let gitignore = builder.build().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("ignore rules cannot be compiled: {error}"),
            )
        })?;
        if gitignore.is_empty() {
            limiter.release_ignore_layer();
            return Ok(());
        }
        let node = Arc::new(IgnoreNode {
            layer: IgnoreLayer {
                base,
                ancestor_prefix,
                kind,
                matcher: gitignore,
            },
            parent: match kind {
                IgnoreFileKind::Ignore => self.ignore_head.clone(),
                IgnoreFileKind::Gitignore | IgnoreFileKind::GitExclude => self.git_head.clone(),
            },
        });
        match kind {
            IgnoreFileKind::Ignore => self.ignore_head = Some(node),
            IgnoreFileKind::Gitignore | IgnoreFileKind::GitExclude => self.git_head = Some(node),
        }
        Ok(())
    }

    fn is_ignored(&self, target_relative: &Path, walk_rel: &Path, is_dir: bool) -> bool {
        let allowed_rel = join_rel(target_relative, walk_rel);
        // Closest match wins within a kind; `.ignore` outranks `.gitignore`,
        // which outranks git exclude. A descendant whitelist therefore cannot
        // override a higher-precedence ancestor rule.
        layer_match(
            self.ignore_head.as_ref(),
            IgnoreFileKind::Ignore,
            &allowed_rel,
            is_dir,
        )
        .or(layer_match(
            self.git_head.as_ref(),
            IgnoreFileKind::Gitignore,
            &allowed_rel,
            is_dir,
        ))
        .or(layer_match(
            self.git_head.as_ref(),
            IgnoreFileKind::GitExclude,
            &allowed_rel,
            is_dir,
        ))
        .is_ignore()
    }
}

/// Hidden or ignore-excluded relative to the allowed root.
///
/// The empty path (the session cwd / selected root with no suffix) is never
/// skipped. Each on-disk component is checked so an explicit file target
/// and a Windows alias of that target use the same rule as walker children.
pub(super) fn relative_is_skipped(ignores: &IgnoreStack, relative: &Path, is_dir: bool) -> bool {
    if relative.as_os_str().is_empty() {
        return false;
    }
    let mut acc = PathBuf::new();
    let components: Vec<_> = relative.components().collect();
    for (index, component) in components.iter().enumerate() {
        acc.push(component.as_os_str());
        let last = index + 1 == components.len();
        if name_is_hidden(component.as_os_str()) {
            return true;
        }
        if ignores.is_ignored(Path::new(""), &acc, if last { is_dir } else { true }) {
            return true;
        }
    }
    false
}

fn layer_match(
    head: Option<&Arc<IgnoreNode>>,
    kind: IgnoreFileKind,
    allowed_rel: &Path,
    is_dir: bool,
) -> ignore::Match<()> {
    let mut node = head;
    while let Some(current) = node {
        if current.layer.kind == kind {
            let prefixed;
            let suffix = if !current.layer.ancestor_prefix.as_os_str().is_empty() {
                prefixed = current.layer.ancestor_prefix.join(allowed_rel);
                prefixed.as_path()
            } else if current.layer.base.as_os_str().is_empty() {
                allowed_rel
            } else {
                match allowed_rel.strip_prefix(&current.layer.base) {
                    Ok(suffix) => suffix,
                    Err(_) => {
                        node = current.parent.as_ref();
                        continue;
                    }
                }
            };
            match current.layer.matcher.matched(suffix, is_dir) {
                ignore::Match::Ignore(_) => return ignore::Match::Ignore(()),
                ignore::Match::Whitelist(_) => return ignore::Match::Whitelist(()),
                ignore::Match::None => {}
            }
        }
        node = current.parent.as_ref();
    }
    ignore::Match::None
}

#[derive(Clone, Copy, Debug)]
enum GitEntryKind {
    Directory,
    File,
}

fn probe_git_entry(dir: &File) -> io::Result<Option<GitEntryKind>> {
    match open_child_file(dir, OsStr::new(".git"), None, NameMatch::Exact) {
        Ok(git) => {
            let metadata = git.metadata()?;
            if metadata.is_dir() {
                Ok(Some(GitEntryKind::Directory))
            } else if metadata.is_file() {
                Ok(Some(GitEntryKind::File))
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "search ignore files cannot be loaded: .git is not a file or directory",
                ))
            }
        }
        Err(error) if is_not_found(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn apply_git_root(
    stack: &mut IgnoreStack,
    worktree: &File,
    kind: GitEntryKind,
    names_to_cwd: &[OsString],
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<()> {
    stack.git_enabled = true;
    let git_dir = match kind {
        GitEntryKind::Directory => open_child_file(
            worktree,
            OsStr::new(".git"),
            Some(EntryKind::Directory),
            NameMatch::Exact,
        )?,
        GitEntryKind::File => open_gitdir_from_file(worktree, limiter, cancel)?,
    };
    let common = resolve_common_dir(&git_dir, limiter, cancel)?;
    ingest_exclude_from_common(stack, &common, names_to_cwd, limiter, cancel)
}

fn open_gitdir_from_file(
    worktree: &File,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<File> {
    let Some(text) = read_child_text(worktree, ".git", limiter, cancel)? else {
        return Err(io::Error::other(
            "search ignore files cannot be loaded: .git file is empty",
        ));
    };
    let gitdir = parse_gitdir_file(&text)?;
    open_git_metadata_dir(worktree, &gitdir)
}

fn parse_gitdir_file(text: &str) -> io::Result<PathBuf> {
    for (index, line) in text.lines().enumerate() {
        let line = if index == 0 {
            line.trim_start_matches('\u{feff}').trim()
        } else {
            line.trim()
        };
        let Some(path) = line.strip_prefix("gitdir:") else {
            continue;
        };
        let path = path.trim();
        if path.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "search ignore files cannot be loaded: gitdir path is empty",
            ));
        }
        return Ok(PathBuf::from(path));
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "search ignore files cannot be loaded: .git file has no gitdir",
    ))
}

fn resolve_common_dir(
    git_dir: &File,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<File> {
    match read_child_text(git_dir, "commondir", limiter, cancel) {
        Ok(Some(text)) => {
            let path = text.lines().next().unwrap_or("").trim();
            if path.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "search ignore files cannot be loaded: commondir is empty",
                ));
            }
            open_git_metadata_dir(git_dir, Path::new(path))
        }
        Ok(None) => git_dir.try_clone(),
        Err(error) => Err(error),
    }
}

fn open_git_metadata_dir(base: &File, path: &Path) -> io::Result<File> {
    if path.is_absolute() {
        return open_directory_nofollow(path);
    }
    let mut current = base.try_clone()?;
    let mut hops = 0usize;
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                hops += 1;
                if hops > MAX_GIT_PARENT_HOPS {
                    return Err(io::Error::other(
                        "search ignore files cannot be loaded: gitdir path is too deep",
                    ));
                }
                current = match open_parent_directory(&current)? {
                    ParentDirectory::Parent(parent) => parent,
                    ParentDirectory::FilesystemRoot => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "search ignore files cannot be loaded: gitdir path escaped past filesystem root",
                        ));
                    }
                };
            }
            Component::Normal(name) => {
                current =
                    open_child_file(&current, name, Some(EntryKind::Directory), NameMatch::Exact)?;
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "search ignore files cannot be loaded: relative gitdir has a root component",
                ));
            }
        }
    }
    Ok(current)
}

fn ingest_exclude_from_common(
    stack: &mut IgnoreStack,
    common: &File,
    names_to_cwd: &[OsString],
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<()> {
    let info = match open_child_file(
        common,
        OsStr::new("info"),
        Some(EntryKind::Directory),
        NameMatch::Exact,
    ) {
        Ok(info) => info,
        Err(error) if is_not_found(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    if let Some(text) = read_child_text(&info, "exclude", limiter, cancel)? {
        stack.push_layer(
            PathBuf::new(),
            names_to_path(names_to_cwd),
            &text,
            IgnoreFileKind::GitExclude,
            limiter,
        )?;
    }
    Ok(())
}

fn load_ancestor_gitignores(
    stack: &mut IgnoreStack,
    git_root: &File,
    names_to_cwd: &[OsString],
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<()> {
    if let Some(text) = read_child_text(git_root, ".gitignore", limiter, cancel)? {
        stack.push_layer(
            PathBuf::new(),
            names_to_path(names_to_cwd),
            &text,
            IgnoreFileKind::Gitignore,
            limiter,
        )?;
    }
    let mut current = git_root.try_clone()?;
    for (index, name) in names_to_cwd.iter().enumerate() {
        if index + 1 == names_to_cwd.len() {
            break;
        }
        current = open_child_file(&current, name, Some(EntryKind::Directory), NameMatch::Exact)?;
        if let Some(text) = read_child_text(&current, ".gitignore", limiter, cancel)? {
            stack.push_layer(
                PathBuf::new(),
                names_to_path(&names_to_cwd[index + 1..]),
                &text,
                IgnoreFileKind::Gitignore,
                limiter,
            )?;
        }
    }
    Ok(())
}

fn names_to_path(names: &[OsString]) -> PathBuf {
    let mut path = PathBuf::new();
    for name in names {
        path.push(name);
    }
    path
}

fn is_not_found(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
}

#[expect(dead_code, reason = "kept for Git file-vs-directory probes")]
fn is_not_a_directory(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::NotADirectory {
        return true;
    }
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ENOTDIR)
    }
    #[cfg(windows)]
    {
        error
            .raw_os_error()
            .is_some_and(|code| code as u32 == windows_sys::Win32::Foundation::ERROR_DIRECTORY)
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

fn read_child_text(
    parent: &File,
    name: &str,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<Option<String>> {
    let mut file = match open_child_file(
        parent,
        OsStr::new(name),
        Some(EntryKind::File),
        NameMatch::Exact,
    ) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
        return Err(stopped_error(limiter));
    }
    // Grow from empty. Never reserve `metadata.len()` bytes: a sparse file
    // reports a huge logical size and that reservation is the unbounded
    // allocation this cap exists to prevent. Do not fail closed on logical
    // size alone: a growing file can report a fitting size then exceed it.
    // Each kernel read requests at most remaining stored capacity plus one
    // probe byte so actual I/O cannot pass the declared cap by a chunk.
    let mut buf = Vec::new();
    let mut chunk = [0u8; IGNORE_READ_CHUNK];
    loop {
        if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
            return Err(stopped_error(limiter));
        }
        let room = IGNORE_FILE_MAX_BYTES.saturating_sub(buf.len());
        let want = room.saturating_add(1).min(IGNORE_READ_CHUNK);
        super::wait_for_worker_readable(&file)?;
        let read = file.read(&mut chunk[..want])?;
        if read == 0 {
            break;
        }
        limiter.add_ignore_read(read);
        if read > room {
            return Err(ignore_too_large());
        }
        limiter.add_ignore_stored(read)?;
        buf.extend_from_slice(&chunk[..read]);
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|_| io::Error::other("ignore file is not valid UTF-8"))
}

fn ignore_too_large() -> io::Error {
    io::Error::other("ignore file exceeds size limit")
}

fn stopped_error(limiter: &WalkLimiter) -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        limiter.stopped_reason().unwrap_or("search stopped"),
    )
}

fn rel_label(rel: &Path) -> String {
    if rel.as_os_str().is_empty() {
        ".".to_owned()
    } else {
        to_posix(rel)
    }
}

fn join_rel(base: &Path, name: impl AsRef<Path>) -> PathBuf {
    let name = name.as_ref();
    if base.as_os_str().is_empty() {
        name.to_path_buf()
    } else if name.as_os_str().is_empty() {
        base.to_path_buf()
    } else {
        base.join(name)
    }
}

fn is_dot(name: &OsStr) -> bool {
    name == "." || name == ".."
}

pub(super) fn name_is_hidden(name: &OsStr) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        name.as_bytes().first() == Some(&b'.')
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        name.encode_wide().next() == Some(u16::from(b'.'))
    }
    #[cfg(not(any(unix, windows)))]
    {
        name.to_string_lossy().starts_with('.')
    }
}

#[derive(Clone)]
struct ListedName {
    name: OsString,
    kind: Option<EntryKind>,
    skip: bool,
    hidden_attr: bool,
}

/// Per-directory listing. Names are collected up to the width cap, then
/// sorted so match-budget truncation is a deterministic global prefix.
struct DirListing {
    pending: Vec<ListedName>,
    next_index: usize,
    loaded: bool,
    #[cfg(unix)]
    dirp: Option<UnixDirOwner>,
    #[cfg(windows)]
    words: Vec<u64>,
    #[cfg(windows)]
    restart: bool,
    #[cfg(windows)]
    exhausted: bool,
    #[cfg(not(any(unix, windows)))]
    _unsupported: (),
}

impl DirListing {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            next_index: 0,
            loaded: false,
            #[cfg(unix)]
            dirp: None,
            #[cfg(windows)]
            words: Vec::new(),
            #[cfg(windows)]
            restart: true,
            #[cfg(windows)]
            exhausted: false,
            #[cfg(not(any(unix, windows)))]
            _unsupported: (),
        }
    }

    fn next(
        &mut self,
        dir: &File,
        limiter: &WalkLimiter,
        cancel: &CancellationToken,
    ) -> io::Result<Option<ListedName>> {
        if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
            return Ok(None);
        }
        if !self.loaded {
            self.load(dir, limiter, cancel)?;
        }
        if self.next_index >= self.pending.len() {
            return Ok(None);
        }
        let entry = self.pending[self.next_index].clone();
        self.next_index += 1;
        Ok(Some(entry))
    }

    fn load(
        &mut self,
        dir: &File,
        limiter: &WalkLimiter,
        cancel: &CancellationToken,
    ) -> io::Result<()> {
        let mut entries = Vec::new();
        let width = limiter.max_dir_width();
        loop {
            if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
                self.loaded = true;
                return Ok(());
            }
            match self.next_platform(dir, limiter, cancel)? {
                None => break,
                Some(entry) => {
                    if entries.len() >= width {
                        limiter.stop("directory width limit reached");
                        return Err(io::Error::other("directory width limit reached"));
                    }
                    entries.push(entry);
                }
            }
        }
        #[cfg(test)]
        if limiter.reverse_dir_enum() {
            // Opposite OS order only. The rendered-key sort below still runs.
            entries.reverse();
        }
        #[cfg(test)]
        limiter.record_listing_key_allocations(entries.len());
        self.pending = sort_listing(entries);
        self.next_index = 0;
        self.loaded = true;
        Ok(())
    }

    #[cfg(unix)]
    fn next_platform(
        &mut self,
        dir: &File,
        limiter: &WalkLimiter,
        cancel: &CancellationToken,
    ) -> io::Result<Option<ListedName>> {
        use std::ffi::CStr;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::io::IntoRawFd;

        if self.dirp.is_none() {
            let cloned = dir.try_clone()?;
            let fd = cloned.into_raw_fd();
            // SAFETY: `fd` is exclusively owned. `fdopendir` either takes it
            // or we close it on the failure path below.
            let dirp = unsafe { libc::fdopendir(fd) };
            if dirp.is_null() {
                let error = io::Error::last_os_error();
                // SAFETY: `fdopendir` failed, so this process still owns `fd`.
                unsafe {
                    libc::close(fd);
                }
                return Err(error);
            }
            self.dirp = Some(UnixDirOwner(dirp));
        }
        let owner = self.dirp.as_ref().expect("unix listing started");
        loop {
            if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
                return Ok(None);
            }
            if !limiter.try_reserve_entry() {
                return Err(io::Error::other("walk entry limit reached"));
            }
            super::unix_clear_errno();
            #[cfg(test)]
            limiter.record_entry_access();
            // SAFETY: `owner.0` is a live `DIR*`. A non-null `dirent` is valid
            // until the next `readdir`/`closedir` on this stream.
            let entry = unsafe { libc::readdir(owner.0) };
            if entry.is_null() {
                limiter.release_entry();
                let error = io::Error::last_os_error();
                if error.raw_os_error().unwrap_or(0) == 0 {
                    return Ok(None);
                }
                return Err(error);
            }
            // SAFETY: `d_name` is a NUL-terminated component from `readdir`.
            let c_name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            let name = OsStr::from_bytes(c_name.to_bytes());
            if is_dot(name) {
                limiter.release_entry();
                continue;
            }
            let file_type = unsafe { (*entry).d_type };
            let (kind, skip) = match file_type {
                libc::DT_DIR => (Some(EntryKind::Directory), false),
                libc::DT_REG => (Some(EntryKind::File), false),
                libc::DT_LNK => (None, true),
                libc::DT_UNKNOWN => (None, false),
                _ => (None, true),
            };
            return Ok(Some(ListedName {
                name: name.to_os_string(),
                kind,
                skip,
                hidden_attr: false,
            }));
        }
    }

    #[cfg(windows)]
    fn next_platform(
        &mut self,
        dir: &File,
        limiter: &WalkLimiter,
        cancel: &CancellationToken,
    ) -> io::Result<Option<ListedName>> {
        use std::os::windows::io::AsRawHandle;
        use std::ptr::{null, null_mut};
        use windows_sys::Wdk::Storage::FileSystem::{
            FileFullDirectoryInformation, NtQueryDirectoryFile,
        };
        use windows_sys::Win32::Foundation::{
            RtlNtStatusToDosError, STATUS_NO_MORE_FILES, STATUS_SUCCESS,
        };
        use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

        if self.exhausted {
            return Ok(None);
        }
        loop {
            if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
                return Ok(None);
            }
            if !limiter.try_reserve_entry() {
                return Err(io::Error::other("walk entry limit reached"));
            }
            if self.words.is_empty() {
                self.words.resize(DIR_LIST_SINGLE_U64S, 0);
            }
            let byte_len = u32::try_from(self.words.len().saturating_mul(8)).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "directory listing buffer is too large",
                )
            })?;
            let mut io_status = IO_STATUS_BLOCK::default();
            #[cfg(test)]
            limiter.record_entry_access();
            // SAFETY: `dir` is a live synchronous directory handle; `words`
            // is aligned writable storage; null event/APC/name pointers select
            // a synchronous unfiltered query. `ReturnSingleEntry` prevents the
            // kernel from materializing more names than the one reservation.
            let status = unsafe {
                NtQueryDirectoryFile(
                    dir.as_raw_handle(),
                    null_mut(),
                    None,
                    null(),
                    &mut io_status,
                    self.words.as_mut_ptr().cast(),
                    byte_len,
                    FileFullDirectoryInformation,
                    true,
                    null(),
                    self.restart,
                )
            };
            if status == STATUS_NO_MORE_FILES {
                limiter.release_entry();
                self.exhausted = true;
                return Ok(None);
            }
            if status != STATUS_SUCCESS {
                limiter.release_entry();
                // `NtQueryDirectoryFile` returns NTSTATUS and does not define
                // `GetLastError`; convert the returned value directly.
                let code = unsafe { RtlNtStatusToDosError(status) };
                return Err(io::Error::from_raw_os_error(code as i32));
            }
            self.restart = false;
            let used = io_status.Information;
            if used == 0 || used > self.words.len().saturating_mul(8) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "directory query returned an invalid byte count",
                ));
            }
            let (entry, next_offset) =
                parse_one_full_dir_info(self.words.as_ptr().cast(), used, 0)?;
            if next_offset != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "single-entry directory query returned multiple records",
                ));
            }
            if let Some(entry) = entry {
                return Ok(Some(entry));
            }
            limiter.release_entry();
        }
    }

    #[cfg(not(any(unix, windows)))]
    fn next_platform(
        &mut self,
        _dir: &File,
        limiter: &WalkLimiter,
        cancel: &CancellationToken,
    ) -> io::Result<Option<ListedName>> {
        let _ = (limiter, cancel);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative directory listing is not implemented on this platform",
        ))
    }
}

#[cfg(unix)]
struct UnixDirOwner(*mut libc::DIR);

#[cfg(unix)]
impl Drop for UnixDirOwner {
    fn drop(&mut self) {
        // SAFETY: `dirp` is exclusively owned and not yet closed.
        unsafe {
            libc::closedir(self.0);
        }
    }
}

#[cfg(unix)]
fn probe_kind(parent: &File, name: &OsStr) -> io::Result<Option<EntryKind>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::AsRawFd;

    let c_name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path component contains NUL"))?;
    // SAFETY: `stat` is an out-parameter written by `fstatat`.
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    // SAFETY: `parent` is a live directory fd, `c_name` is NUL-terminated,
    // and `AT_SYMLINK_NOFOLLOW` inspects the named child itself.
    let status = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            c_name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    let format = stat.st_mode & libc::S_IFMT;
    if format == libc::S_IFLNK {
        Ok(None)
    } else if format == libc::S_IFDIR {
        Ok(Some(EntryKind::Directory))
    } else if format == libc::S_IFREG {
        Ok(Some(EntryKind::File))
    } else {
        Ok(None)
    }
}

#[cfg(windows)]
fn parse_one_full_dir_info(
    base: *const u8,
    cap: usize,
    offset: usize,
) -> io::Result<(Option<ListedName>, usize)> {
    use std::mem::offset_of;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FULL_DIR_INFO,
    };

    let header_size = offset_of!(FILE_FULL_DIR_INFO, FileName);
    if offset.saturating_add(header_size) > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated directory listing",
        ));
    }
    // SAFETY: `offset + header` is inside `cap`, so the fixed prefix
    // of `FILE_FULL_DIR_INFO` can be read.
    let info = unsafe { &*base.add(offset).cast::<FILE_FULL_DIR_INFO>() };
    let name_bytes = info.FileNameLength as usize;
    if !name_bytes.is_multiple_of(2)
        || offset
            .saturating_add(header_size)
            .saturating_add(name_bytes)
            > cap
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory entry name is truncated",
        ));
    }
    let name_units = name_bytes / 2;
    // SAFETY: `FileName` is `name_units` UTF-16 code units inside the
    // same listing buffer already bounds-checked above.
    let name = unsafe { std::slice::from_raw_parts(info.FileName.as_ptr(), name_units) };
    let name = OsString::from_wide(name);
    let entry = if is_dot(&name) {
        None
    } else {
        let attributes = info.FileAttributes;
        let reparse = attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
        let directory = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        Some(ListedName {
            name,
            kind: if reparse {
                None
            } else if directory {
                Some(EntryKind::Directory)
            } else {
                Some(EntryKind::File)
            },
            skip: reparse,
            hidden_attr: attributes & FILE_ATTRIBUTE_HIDDEN != 0,
        })
    };
    Ok((entry, info.NextEntryOffset as usize))
}

#[cfg(windows)]
fn probe_kind(_parent: &File, _name: &OsStr) -> io::Result<Option<EntryKind>> {
    Ok(None)
}

#[cfg(not(any(unix, windows)))]
fn probe_kind(_parent: &File, _name: &OsStr) -> io::Result<Option<EntryKind>> {
    Ok(None)
}

#[cfg(test)]
pub(super) fn read_ignore_file_for_test(
    parent: &File,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<Option<String>> {
    read_child_text(parent, ".ignore", limiter, cancel)
}

#[cfg(test)]
#[path = "fs_walk_tests.rs"]
mod tests;
