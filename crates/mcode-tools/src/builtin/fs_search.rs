//! Shared secure filesystem-search machinery.
//!
//! `grep` and `find` resolve the session cwd once, retain stable root
//! handles, normalize output paths, enforce shared limits, and bridge
//! blocking walkers onto a dedicated OS thread. Enumeration reads directory
//! entries and ignore files through those retained handles, never by
//! re-resolving the root path name. Names are never trusted as the object
//! to read or report. User-typed components are rewritten to the unique
//! on-disk directory-entry spelling proven by parent-handle identity;
//! zero or several matches fail closed so a case-insensitive alias cannot
//! keep a lower-case path or ignore key. Grep opens each matching
//! file once for content read through the retained parent handle. Find
//! binds a metadata-only capability so an unreadable file is still
//! discovered; directories and grep request content access, and a
//! metadata-then-content pair must share identity. Windows hidden bits are
//! re-read from the opened handle before read, confirm, descent, and
//! reporting. Ignore parse/build/load failures are terminating; ordinary
//! per-path I/O is a model-visible incomplete lower bound. Unix uses
//! root-relative `openat` calls with no-follow traversal. Windows opens
//! every component relative to retained directory handles with `NtOpenFile`,
//! rejects reparse points, and validates final Unicode handle paths against
//! the retained allowed-root handle.
//!
//! Linux opens each child with `openat2(RESOLVE_BENEATH | RESOLVE_NO_XDEV |
//! RESOLVE_NO_SYMLINKS)` so bind mounts cannot be crossed, including at
//! find confirmation. Find confirmation on Linux/Android uses `O_PATH` so a
//! mode-`000` name can be reported without content-read permission; other
//! Unix confirms with no-follow metadata (`fstatat`) and the same `st_dev` /
//! type / `nlink` checks, and fails closed when that proof is unavailable.
//! Other Unix descent still uses `openat(O_NOFOLLOW)` plus `st_dev`; that is
//! the mount identity on Darwin/BSD, which have no Linux-style same-`st_dev`
//! bind mounts. Platforms without handle-relative open fail closed. Regular
//! files with a link count other than one are refused so a cwd-visible
//! hardlink cannot expose an inode that also lives outside the allowed root.
//! Directory listings are collected up to a width cap, decorated once with
//! the lossy rendered component key the frontier and top-N heaps use, sorted
//! with the original `OsString` as the complete tie-break, and visited best-first
//! by full rendered path within the invocation depth, entry, handle, ignore-byte,
//! and ignore-rule limits.
//!
//! Everything stays in-process (handle-relative walk plus ripgrep's
//! search core); no external `rg` or `fd` executable is used.
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::tool::ToolError;

#[path = "fs_walk.rs"]
mod walk;
#[cfg(test)]
pub(crate) use walk::IGNORE_FILE_MAX_BYTES;
pub(crate) use walk::walk_retained_tree;

/// Configured wall-clock deadline before supervised cancellation begins.
pub(crate) const SEARCH_TIME_LIMIT: Duration = Duration::from_secs(60);

/// Pause between cancel interrupts while a worker is still joining.
///
/// Unix pollable reads also have a per-worker wake socket, so they do not
/// depend on `SIGURG`. The signal and Windows `CancelSynchronousIo` are
/// retried for syscalls already in the kernel. Uninterruptible kernel `D`
/// state (some NFS waits) can still outlive this loop.
const INTERRUPT_RETRY: Duration = Duration::from_millis(10);

/// Maximum bytes actually read by one grep invocation.
pub(crate) const SCAN_BYTES_CAP: u64 = 512 * 1024 * 1024;

/// Ceiling on stored results regardless of caller-provided caps.
pub(crate) const STORED_CEILING: usize = 10_000;

/// Bytes of a matching line retained in output.
pub(crate) const MAX_LINE_BYTES: usize = 500;

/// Total rendered output bytes before lines are omitted.
pub(crate) const OUTPUT_BYTES_CAP: usize = 100 * 1024;

/// Maximum committed-plus-provisional match callbacks per grep invocation.
pub(crate) const COUNT_BUDGET: u64 = 100_000;

/// Per-file heap ceiling for the grep searcher's line buffer.
pub(crate) const LINE_HEAP_LIMIT: usize = 10 * 1024 * 1024;

/// Maximum per-path I/O error strings retained in result details.
pub(crate) const IO_ERROR_SAMPLES: usize = 5;

/// Directory nesting cap relative to the selected target.
///
/// Deep enough for real trees; a larger value would let a cyclic or
/// hostile layout pin handles along the DFS spine until the wall clock
/// expires.
pub(crate) const MAX_WALK_DEPTH: usize = 256;

/// Names examined in one grep/find invocation, including skipped ones.
///
/// Bounds work on a very wide repository before match/time caps fire.
pub(crate) const MAX_WALK_ENTRIES: u64 = 100_000;

/// Maximum directory-entry names buffered for sorted traversal.
///
/// One directory wider than this is treated as hostile and fails closed
/// rather than streaming an unbounded, non-deterministic listing.
pub(crate) const MAX_DIR_WIDTH: usize = 16_384;

/// Total bytes loaded from ignore files in one invocation.
///
/// Per-file cap is [`walk::IGNORE_FILE_MAX_BYTES`]; this bounds the sum
/// across the tree so many small ignore files cannot pin memory.
pub(crate) const MAX_IGNORE_TOTAL_BYTES: u64 = 4 * 1024 * 1024;

/// Maximum compiled ignore layers retained for one invocation.
pub(crate) const MAX_IGNORE_LAYERS: usize = 1_024;

/// Maximum ignore rules (accepted lines) compiled for one invocation.
pub(crate) const MAX_IGNORE_RULES: usize = 16_384;

/// Charged live directory/file handles for one invocation.
///
/// Covers the allowed root, selected target, and every best-first frontier
/// directory. Exhausted and empty directory frames release their handle
/// charge immediately; only the live frontier retains charged walk handles.
/// Equal to twice [`MAX_WALK_DEPTH`] so a full-depth spine plus the two
/// retained roots still fail closed before `RLIMIT_NOFILE`. This is a
/// limiter budget, not a kernel-enforced exact handle cap: short-lived
/// descriptors used inside one open or stat are not charged.
pub(crate) const MAX_OPEN_HANDLES: u64 = 512;

/// Maximum bytes in a grep pattern or find/include/exclude glob.
///
/// The regex crate's nest limit is not a heap bound; capping the concrete
/// pattern is what keeps compile memory proportional and fail-closed.
pub(crate) const MAX_PATTERN_BYTES: usize = 16 * 1024;

/// In-memory grep/find result heap cap (interned paths plus stored lines).
///
/// Output is already cut at [`OUTPUT_BYTES_CAP`]. This separate heap bound
/// stops 10_000 long handle-relative paths from pinning a gigabyte before
/// the renderer truncates. Tests inject a smaller value.
pub(crate) const MAX_RESULT_STORE_BYTES: usize = 4 * 1024 * 1024;

/// Parent directories examined when locating a Git boundary above cwd.
///
/// Same numeric ceiling as [`MAX_WALK_DEPTH`]: deep enough for real
/// monorepos, and a larger value would let a hostile layout walk to `/`.
pub(crate) const MAX_GIT_PARENT_HOPS: usize = 256;

/// Approximate compiled-NFA ceiling passed to grep-regex (default ~10 MiB).
pub(crate) const REGEX_SIZE_LIMIT: usize = 1024 * 1024;

/// Per-thread DFA cache ceiling passed to grep-regex (default ~10 MiB).
pub(crate) const REGEX_DFA_SIZE_LIMIT: usize = 1024 * 1024;

/// Access requested when opening a child from a retained parent handle.
///
/// Find binds [`SearchAccess::Metadata`] so a mode-`000` or
/// `FILE_READ_ATTRIBUTES`-only file can still be discovered. Grep and
/// directory listing bind [`SearchAccess::Content`]. A directory that was
/// first opened for metadata is reopened for content only when the two
/// identities match. Metadata handles are never used to read file bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchAccess {
    /// Read file bytes or list a directory (grep, ignore files, descent).
    Content,
    /// Type, identity, and attribute checks only (find files).
    Metadata,
}

/// Global caps for one tool invocation.
#[derive(Clone, Debug)]
pub(crate) struct Limits {
    /// Wall-clock budget for the whole walk and search.
    pub time_limit: Duration,
    /// Maximum bytes actually read from opened files.
    pub scan_bytes: u64,
    /// Stored-result ceiling.
    pub stored_ceiling: usize,
    /// Per-line display truncation in bytes.
    pub line_bytes: usize,
    /// Search-engine line-buffer heap ceiling per file.
    pub line_heap: usize,
    /// Total output byte cap.
    pub output_bytes: usize,
    /// Maximum committed-plus-provisional match callbacks.
    pub count_budget: u64,
    /// Maximum directory nesting relative to the selected target.
    pub max_walk_depth: usize,
    /// Maximum directory entries examined in one invocation.
    pub max_walk_entries: u64,
    /// Maximum names buffered from one directory.
    pub max_dir_width: usize,
    /// Total ignore-file bytes loaded in one invocation.
    pub max_ignore_bytes: u64,
    /// Maximum compiled ignore layers in one invocation.
    pub max_ignore_layers: usize,
    /// Maximum ignore rules compiled in one invocation.
    pub max_ignore_rules: usize,
    /// Maximum live handles retained for one invocation.
    pub max_open_handles: u64,
    /// Cumulative bytes stored in the grep/find result heap.
    pub max_result_bytes: usize,
    /// Shared wall-clock deadline for the limiter and the outer timer.
    ///
    /// When `None`, [`WalkLimiter::new`] uses `Instant::now() + time_limit`.
    /// Execution and preflight compute one `Instant` and pass it here so a
    /// worker cannot publish `stopped_early` while the outer timer is still
    /// sleeping.
    pub deadline: Option<Instant>,
    /// Per-call resolve counter. Tests inject this instead of a process static.
    #[cfg(test)]
    pub resolve_count: Option<Arc<AtomicU64>>,
    /// Reverse the raw OS listing before the deterministic sort.
    #[cfg(test)]
    pub reverse_dir_enum: bool,
    /// Forces platform identity queries to fail closed.
    #[cfg(test)]
    pub force_identity_error: bool,
    /// Force Windows hidden-attribute queries to fail.
    #[cfg(test)]
    pub force_hidden_error: bool,
    /// Injected child-open fault. Called with the component name.
    #[cfg(test)]
    pub open_fault: Option<OpenFault>,
    /// Replaces the opened child's device/volume after a real open.
    #[cfg(test)]
    pub child_device_override: Option<ChildDeviceOverride>,
    /// Observes access/flags at the open boundary and may fail the call.
    #[cfg(test)]
    pub access_gate: Option<AccessGate>,
    /// Runs after a Windows parent path snapshot and before the no-follow open.
    #[cfg(all(test, windows))]
    pub parent_discovery_hook: Option<ParentDiscoveryHook>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            time_limit: SEARCH_TIME_LIMIT,
            scan_bytes: SCAN_BYTES_CAP,
            stored_ceiling: STORED_CEILING,
            line_bytes: MAX_LINE_BYTES,
            line_heap: LINE_HEAP_LIMIT,
            output_bytes: OUTPUT_BYTES_CAP,
            count_budget: COUNT_BUDGET,
            max_walk_depth: MAX_WALK_DEPTH,
            max_walk_entries: MAX_WALK_ENTRIES,
            max_dir_width: MAX_DIR_WIDTH,
            max_ignore_bytes: MAX_IGNORE_TOTAL_BYTES,
            max_ignore_layers: MAX_IGNORE_LAYERS,
            max_ignore_rules: MAX_IGNORE_RULES,
            max_open_handles: MAX_OPEN_HANDLES,
            max_result_bytes: MAX_RESULT_STORE_BYTES,
            deadline: None,
            #[cfg(test)]
            resolve_count: None,
            #[cfg(test)]
            reverse_dir_enum: false,
            #[cfg(test)]
            force_identity_error: false,
            #[cfg(test)]
            force_hidden_error: false,
            #[cfg(test)]
            open_fault: None,
            #[cfg(test)]
            child_device_override: None,
            #[cfg(test)]
            access_gate: None,
            #[cfg(all(test, windows))]
            parent_discovery_hook: None,
        }
    }
}

/// Test-only child-open fault injected through [`Limits`].
#[cfg(test)]
pub(crate) type OpenFaultFn = Arc<dyn Fn(&OsStr) -> io::Result<()> + Send + Sync>;

/// Test-only child-open fault injected through [`Limits`].
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct OpenFault(pub OpenFaultFn);

#[cfg(test)]
impl std::fmt::Debug for OpenFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpenFault")
    }
}

/// Test-only replacement for a child's device/volume after a successful open.
#[cfg(test)]
pub(crate) type ChildDeviceOverrideFn = Arc<dyn Fn(&OsStr) -> Option<u64> + Send + Sync>;

/// Test-only replacement for a child's device/volume after a successful open.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct ChildDeviceOverride(pub ChildDeviceOverrideFn);

#[cfg(test)]
impl std::fmt::Debug for ChildDeviceOverride {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChildDeviceOverride")
    }
}

/// Access mode and platform flags observed at a real open boundary.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct ObservedOpen {
    /// Content versus metadata capability requested by the caller.
    pub access: SearchAccess,
    /// Unix `openat`/`openat2` flags, including `O_PATH` when used.
    #[cfg(unix)]
    pub flags: libc::c_int,
    /// Linux/Android `openat2` resolution policy passed to the syscall.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub resolve: u64,
    /// Windows `NtOpenFile` desired access mask.
    #[cfg(windows)]
    pub desired_access: u32,
    /// Windows `NtOpenFile` create options, including reparse bits.
    #[cfg(windows)]
    pub options: u32,
}

/// Test-only gate invoked after flags/access are chosen and before/at open.
#[cfg(test)]
pub(crate) type AccessGateFn = Arc<dyn Fn(&OsStr, ObservedOpen) -> io::Result<()> + Send + Sync>;

/// Test-only gate invoked after flags/access are chosen and before/at open.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct AccessGate(pub AccessGateFn);

#[cfg(test)]
impl std::fmt::Debug for AccessGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessGate")
    }
}

/// Test-only hook after a parent path is snapshotted from a handle.
#[cfg(all(test, windows))]
pub(crate) type ParentDiscoveryHookFn =
    Arc<dyn Fn(&Path) -> io::Result<Option<PathBuf>> + Send + Sync>;

/// Test-only hook after a parent path is snapshotted from a handle.
#[cfg(all(test, windows))]
#[derive(Clone)]
pub(crate) struct ParentDiscoveryHook(pub ParentDiscoveryHookFn);

#[cfg(all(test, windows))]
impl std::fmt::Debug for ParentDiscoveryHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ParentDiscoveryHook")
    }
}

/// Result of attempting an atomic scan-byte reservation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScanReservation {
    /// The caller may issue one read of at most this many bytes.
    Granted(usize),
    /// Another reader temporarily owns the remaining capacity.
    Pending,
    /// The cap consists entirely of settled, actually-read bytes.
    Exhausted,
}

#[derive(Default)]
struct ScanBudget {
    /// Actual bytes plus outstanding read reservations.
    claimed: u64,
    /// Outstanding reservations, used to distinguish temporary pressure
    /// from a settled exhausted budget.
    inflight: u64,
}

/// Cooperative stop state shared by every walker thread.
pub(crate) struct WalkLimiter {
    /// Set when walkers should quit as soon as possible.
    pub quit: AtomicBool,
    stop_reason: Mutex<Option<&'static str>>,
    deadline: Mutex<Instant>,
    /// One lock publishes claimed and in-flight bytes as a single state.
    scan_budget: Mutex<ScanBudget>,
    max_walk_depth: usize,
    max_walk_entries: u64,
    max_dir_width: usize,
    max_ignore_bytes: u64,
    max_ignore_layers: usize,
    max_ignore_rules: usize,
    entries: AtomicU64,
    ignore_bytes: AtomicU64,
    ignore_layers: AtomicU64,
    ignore_rules: AtomicU64,
    max_open_handles: u64,
    handles: AtomicU64,
    #[cfg(test)]
    peak_handles: AtomicU64,
    max_result_bytes: u64,
    result_bytes: AtomicU64,
    result_store_truncated: AtomicBool,
    /// Bytes actually read from ignore files, including the one-byte probe
    /// that proves an oversized file. Tested separately from stored bytes.
    #[cfg(test)]
    ignore_read_bytes: AtomicU64,
    #[cfg(test)]
    reverse_dir_enum: AtomicBool,
    #[cfg(test)]
    force_identity_error: AtomicBool,
    #[cfg(test)]
    force_hidden_error: AtomicBool,
    #[cfg(test)]
    open_fault: Mutex<Option<OpenFault>>,
    #[cfg(test)]
    child_device_override: Mutex<Option<ChildDeviceOverride>>,
    #[cfg(test)]
    access_gate: Mutex<Option<AccessGate>>,
    #[cfg(all(test, windows))]
    parent_discovery_hook: Mutex<Option<ParentDiscoveryHook>>,
    #[cfg(test)]
    entry_accesses: AtomicU64,
    #[cfg(test)]
    listing_key_allocations: AtomicU64,
}

impl WalkLimiter {
    /// Creates a limiter from `limits`.
    pub fn new(limits: &Limits) -> Self {
        Self {
            quit: AtomicBool::new(false),
            stop_reason: Mutex::new(None),
            deadline: Mutex::new(
                limits
                    .deadline
                    .unwrap_or_else(|| Instant::now() + limits.time_limit),
            ),
            scan_budget: Mutex::new(ScanBudget::default()),
            max_walk_depth: limits.max_walk_depth,
            max_walk_entries: limits.max_walk_entries,
            max_dir_width: limits.max_dir_width,
            max_ignore_bytes: limits.max_ignore_bytes,
            max_ignore_layers: limits.max_ignore_layers,
            max_ignore_rules: limits.max_ignore_rules,
            entries: AtomicU64::new(0),
            ignore_bytes: AtomicU64::new(0),
            ignore_layers: AtomicU64::new(0),
            ignore_rules: AtomicU64::new(0),
            max_open_handles: limits.max_open_handles,
            handles: AtomicU64::new(0),
            #[cfg(test)]
            peak_handles: AtomicU64::new(0),
            max_result_bytes: u64::try_from(limits.max_result_bytes).unwrap_or(u64::MAX),
            result_bytes: AtomicU64::new(0),
            result_store_truncated: AtomicBool::new(false),
            #[cfg(test)]
            ignore_read_bytes: AtomicU64::new(0),
            #[cfg(test)]
            reverse_dir_enum: AtomicBool::new(limits.reverse_dir_enum),
            #[cfg(test)]
            force_identity_error: AtomicBool::new(limits.force_identity_error),
            #[cfg(test)]
            force_hidden_error: AtomicBool::new(limits.force_hidden_error),
            #[cfg(test)]
            open_fault: Mutex::new(limits.open_fault.clone()),
            #[cfg(test)]
            child_device_override: Mutex::new(limits.child_device_override.clone()),
            #[cfg(test)]
            access_gate: Mutex::new(limits.access_gate.clone()),
            #[cfg(all(test, windows))]
            parent_discovery_hook: Mutex::new(limits.parent_discovery_hook.clone()),
            #[cfg(test)]
            entry_accesses: AtomicU64::new(0),
            #[cfg(test)]
            listing_key_allocations: AtomicU64::new(0),
        }
    }

    /// Maximum DFS depth including the selected target directory.
    pub fn max_walk_depth(&self) -> usize {
        self.max_walk_depth
    }

    /// Maximum names collected from one directory before fail-closed.
    pub fn max_dir_width(&self) -> usize {
        self.max_dir_width
    }

    /// Atomically reserves one examined name before it is opened or statted.
    ///
    /// Returns `false` and stops when the invocation entry cap is already
    /// exhausted, so the caller must not access that name.
    pub fn try_reserve_entry(&self) -> bool {
        let previous = self.entries.fetch_add(1, Ordering::AcqRel);
        if previous >= self.max_walk_entries {
            self.entries.fetch_sub(1, Ordering::AcqRel);
            self.stop("walk entry limit reached");
            return false;
        }
        true
    }

    /// Credits a reservation that did not materialize an examined name.
    pub fn release_entry(&self) {
        self.entries.fetch_sub(1, Ordering::AcqRel);
    }

    /// Examined-name count charged to this invocation (tests).
    #[cfg(test)]
    pub fn walk_entries(&self) -> u64 {
        self.entries.load(Ordering::Acquire)
    }

    /// Records one materialized directory-entry access (tests).
    #[cfg(test)]
    pub fn record_entry_access(&self) {
        self.entry_accesses.fetch_add(1, Ordering::AcqRel);
    }

    /// Materialized directory-entry accesses (tests).
    #[cfg(test)]
    pub fn entry_accesses(&self) -> u64 {
        self.entry_accesses.load(Ordering::Acquire)
    }

    /// Records one rendered-key allocation per name in a completed listing.
    #[cfg(test)]
    pub fn record_listing_key_allocations(&self, count: usize) {
        self.listing_key_allocations
            .fetch_add(u64::try_from(count).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    /// Rendered-key allocations made by completed listing sorts (tests).
    #[cfg(test)]
    pub fn listing_key_allocations(&self) -> u64 {
        self.listing_key_allocations.load(Ordering::Relaxed)
    }

    /// Charges `bytes` actually read from an ignore file, including a
    /// one-byte oversize probe. Stored ignore bytes are charged separately.
    pub fn add_ignore_read(&self, bytes: usize) {
        #[cfg(test)]
        self.ignore_read_bytes
            .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
        let _ = bytes;
    }

    /// Reserves stored ignore bytes against the invocation total.
    pub fn add_ignore_stored(&self, bytes: usize) -> io::Result<()> {
        let added = u64::try_from(bytes).unwrap_or(u64::MAX);
        let previous = self.ignore_bytes.fetch_add(added, Ordering::AcqRel);
        if previous.saturating_add(added) > self.max_ignore_bytes {
            self.stop("ignore size limit reached");
            return Err(io::Error::other("ignore file exceeds size limit"));
        }
        Ok(())
    }

    /// Reserves one ignore layer before any line is parsed or compiled.
    pub fn try_reserve_ignore_layer(&self) -> io::Result<()> {
        let previous = self.ignore_layers.fetch_add(1, Ordering::AcqRel);
        if previous >= u64::try_from(self.max_ignore_layers).unwrap_or(u64::MAX) {
            self.ignore_layers.fetch_sub(1, Ordering::AcqRel);
            self.stop("ignore layer limit reached");
            return Err(io::Error::other(
                "search ignore files cannot be loaded: layer limit reached",
            ));
        }
        Ok(())
    }

    /// Reserves one ignore rule before `add_line` / matcher compile.
    pub fn try_reserve_ignore_rule(&self) -> io::Result<()> {
        let previous = self.ignore_rules.fetch_add(1, Ordering::AcqRel);
        if previous >= u64::try_from(self.max_ignore_rules).unwrap_or(u64::MAX) {
            self.ignore_rules.fetch_sub(1, Ordering::AcqRel);
            self.stop("ignore rule limit reached");
            return Err(io::Error::other(
                "search ignore files cannot be loaded: rule limit reached",
            ));
        }
        Ok(())
    }

    /// Releases a layer reserved for a file that compiled to no rules.
    pub fn release_ignore_layer(&self) {
        self.ignore_layers.fetch_sub(1, Ordering::AcqRel);
    }

    /// Compiled ignore rules charged to this invocation (tests).
    #[cfg(test)]
    pub fn ignore_rules(&self) -> u64 {
        self.ignore_rules.load(Ordering::Acquire)
    }

    /// Bytes actually read from ignore files (tests).
    #[cfg(test)]
    pub fn ignore_read_bytes(&self) -> u64 {
        self.ignore_read_bytes.load(Ordering::Relaxed)
    }

    /// Compiled ignore layers charged to this invocation (tests).
    #[cfg(test)]
    pub fn ignore_layers(&self) -> u64 {
        self.ignore_layers.load(Ordering::Relaxed)
    }

    /// Reserves `bytes` in the result heap. Does not stop the walk.
    pub fn try_reserve_result_bytes(&self, bytes: usize) -> bool {
        let added = u64::try_from(bytes).unwrap_or(u64::MAX);
        let previous = self.result_bytes.fetch_add(added, Ordering::AcqRel);
        if previous.saturating_add(added) > self.max_result_bytes {
            self.result_bytes.fetch_sub(added, Ordering::AcqRel);
            self.result_store_truncated.store(true, Ordering::Release);
            return false;
        }
        true
    }

    /// Credits result-heap bytes after a stored entry is evicted.
    pub fn release_result_bytes(&self, bytes: usize) {
        let released = u64::try_from(bytes).unwrap_or(u64::MAX);
        self.result_bytes.fetch_sub(released, Ordering::AcqRel);
    }

    /// Whether the result heap refused a store because of the byte cap.
    pub fn result_store_truncated(&self) -> bool {
        self.result_store_truncated.load(Ordering::Acquire)
    }

    /// Result-heap bytes charged to this invocation (tests).
    #[cfg(test)]
    pub fn result_store_bytes(&self) -> u64 {
        self.result_bytes.load(Ordering::Acquire)
    }

    /// Whether this invocation should reverse the raw OS listing.
    #[cfg(test)]
    pub fn reverse_dir_enum(&self) -> bool {
        self.reverse_dir_enum.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn force_identity_error(&self) -> bool {
        self.force_identity_error.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn force_hidden_error(&self) -> bool {
        self.force_hidden_error.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn apply_open_fault(&self, name: &OsStr) -> io::Result<()> {
        let fault = self.open_fault.lock().expect("open_fault poisoned");
        if let Some(fault) = fault.as_ref() {
            return (fault.0)(name);
        }
        Ok(())
    }

    #[cfg(test)]
    fn child_device_override(&self, name: &OsStr) -> Option<u64> {
        let override_fn = self
            .child_device_override
            .lock()
            .expect("child_device_override poisoned");
        override_fn.as_ref().and_then(|hook| (hook.0)(name))
    }

    #[cfg(test)]
    fn apply_access_gate(&self, name: &OsStr, observed: ObservedOpen) -> io::Result<()> {
        let gate = self.access_gate.lock().expect("access_gate poisoned");
        if let Some(gate) = gate.as_ref() {
            return (gate.0)(name, observed);
        }
        Ok(())
    }

    #[cfg(all(test, windows))]
    fn apply_parent_discovery_hook(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        let hook = self
            .parent_discovery_hook
            .lock()
            .expect("parent_discovery_hook poisoned");
        if let Some(hook) = hook.as_ref() {
            return (hook.0)(path);
        }
        Ok(None)
    }

    /// Live handles charged to this invocation (tests).
    #[cfg(test)]
    pub fn live_handles(&self) -> u64 {
        self.handles.load(Ordering::Relaxed)
    }

    /// Peak live handles charged to this invocation (tests).
    #[cfg(test)]
    pub fn peak_handles(&self) -> u64 {
        self.peak_handles.load(Ordering::Relaxed)
    }

    /// Charges one live handle against the invocation budget.
    pub fn acquire_handle(&self) -> io::Result<()> {
        let previous = self.handles.fetch_add(1, Ordering::AcqRel);
        if previous >= self.max_open_handles {
            self.handles.fetch_sub(1, Ordering::AcqRel);
            self.stop("handle budget reached");
            return Err(io::Error::other("handle budget reached"));
        }
        #[cfg(test)]
        self.peak_handles.fetch_max(previous + 1, Ordering::Relaxed);
        Ok(())
    }

    /// Releases one live handle charge.
    pub fn release_handle(&self) {
        self.handles.fetch_sub(1, Ordering::AcqRel);
    }

    /// Charges one handle and returns a guard that releases it on drop.
    pub fn lease(self: &Arc<Self>) -> io::Result<HandleLease> {
        self.acquire_handle()?;
        Ok(HandleLease {
            limiter: Arc::clone(self),
        })
    }

    /// Stops all walkers; the first reason wins.
    pub fn stop(&self, reason: &'static str) {
        self.quit.store(true, Ordering::Release);
        let mut slot = self.stop_reason.lock().expect("stop_reason poisoned");
        if slot.is_none() {
            *slot = Some(reason);
        }
    }

    /// Returns why the walk stopped early, if it did.
    pub fn stopped_reason(&self) -> Option<&'static str> {
        *self.stop_reason.lock().expect("stop_reason poisoned")
    }

    /// Checks cancellation, deadline, and any prior global stop.
    pub fn check(&self, cancel: &CancellationToken) -> ignore::WalkState {
        use ignore::WalkState;
        if self.quit.load(Ordering::Acquire) {
            return WalkState::Quit;
        }
        if cancel.is_cancelled() {
            self.stop("cancelled");
            return WalkState::Quit;
        }
        let deadline = *self.deadline.lock().expect("deadline poisoned");
        if Instant::now() >= deadline {
            self.stop("time limit reached");
            return WalkState::Quit;
        }
        WalkState::Continue
    }

    /// Starts a new wall-clock phase; ignore/handle budgets stay accumulated.
    ///
    /// Dispatch preparation and execution use separate wall-clock phases, so
    /// preparation time does not consume the execution budget.
    pub fn refresh_deadline(&self, time_limit: Duration) {
        self.refresh_deadline_at(Instant::now() + time_limit);
    }

    /// Replaces the limiter deadline with the same absolute instant the
    /// outer timer uses.
    pub fn refresh_deadline_at(&self, deadline: Instant) {
        *self.deadline.lock().expect("deadline poisoned") = deadline;
    }

    /// Absolute deadline shared with the outer run_blocking timer.
    #[cfg(test)]
    pub fn deadline(&self) -> Instant {
        *self.deadline.lock().expect("deadline poisoned")
    }

    /// Atomically reserves capacity for one actual file read.
    pub fn reserve_scan(&self, requested: usize, cap: u64) -> ScanReservation {
        if requested == 0 {
            return ScanReservation::Granted(0);
        }
        let mut budget = self.scan_budget.lock().expect("scan budget poisoned");
        if budget.claimed >= cap {
            return if budget.inflight == 0 {
                ScanReservation::Exhausted
            } else {
                ScanReservation::Pending
            };
        }
        let available = cap - budget.claimed;
        let requested = u64::try_from(requested).unwrap_or(u64::MAX);
        let granted = available.min(requested);
        budget.claimed += granted;
        budget.inflight += granted;
        ScanReservation::Granted(
            usize::try_from(granted).expect("scan reservation never exceeds a usize request"),
        )
    }

    /// Settles one reservation with the bytes the OS actually returned.
    pub fn settle_scan(&self, reserved: usize, actual: usize) {
        assert!(actual <= reserved, "actual read exceeded its reservation");
        let reserved = u64::try_from(reserved).expect("reserved read size must fit in u64");
        let actual = u64::try_from(actual).expect("actual read size must fit in u64");
        let mut budget = self.scan_budget.lock().expect("scan budget poisoned");
        assert!(
            budget.inflight >= reserved,
            "settled scan exceeded in-flight reservations"
        );
        budget.claimed -= reserved - actual;
        budget.inflight -= reserved;
    }

    /// Returns settled bytes plus any currently outstanding reservations.
    #[cfg(test)]
    pub fn claimed_scan_bytes(&self) -> u64 {
        self.scan_budget
            .lock()
            .expect("scan budget poisoned")
            .claimed
    }
}

/// Releases one [`WalkLimiter`] handle charge when dropped.
pub(crate) struct HandleLease {
    limiter: Arc<WalkLimiter>,
}

impl Drop for HandleLease {
    fn drop(&mut self) {
        self.limiter.release_handle();
    }
}

/// Converts a limiter time/cancel stop into the documented execution error.
pub(crate) fn stop_reason_error(label: &str, limiter: &WalkLimiter) -> Option<ToolError> {
    match limiter.stopped_reason() {
        Some("time limit reached") => {
            Some(ToolError::Execution(format!("{label} time limit reached")))
        }
        Some("cancelled") => Some(ToolError::Execution(format!(
            "{label} cancelled before completion"
        ))),
        _ => None,
    }
}

#[cfg(test)]
thread_local! {
    static CURRENT_LIMITER: std::cell::RefCell<Option<Arc<WalkLimiter>>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) struct LimiterBindGuard {
    #[cfg(test)]
    previous: Option<Arc<WalkLimiter>>,
}

pub(super) fn bind_current_limiter(limiter: &Arc<WalkLimiter>) -> LimiterBindGuard {
    #[cfg(test)]
    {
        let previous = CURRENT_LIMITER.with(|slot| slot.replace(Some(Arc::clone(limiter))));
        LimiterBindGuard { previous }
    }
    #[cfg(not(test))]
    {
        let _ = limiter;
        LimiterBindGuard {}
    }
}

impl Drop for LimiterBindGuard {
    fn drop(&mut self) {
        #[cfg(test)]
        {
            let previous = self.previous.take();
            CURRENT_LIMITER.with(|slot| {
                *slot.borrow_mut() = previous;
            });
        }
    }
}

#[cfg(test)]
fn current_limiter<T>(f: impl FnOnce(&WalkLimiter) -> T) -> Option<T> {
    CURRENT_LIMITER.with(|slot| slot.borrow().as_ref().map(|limiter| f(limiter)))
}

/// Bounded collector for per-path I/O errors.
pub(crate) struct IoErrors {
    samples: Mutex<Vec<String>>,
    count: AtomicU64,
    sample_cap: usize,
}

impl IoErrors {
    /// Creates a collector retaining at most `sample_cap` messages.
    pub fn new(sample_cap: usize) -> Self {
        Self {
            samples: Mutex::new(Vec::new()),
            count: AtomicU64::new(0),
            sample_cap,
        }
    }

    /// Records one path-specific error.
    pub fn record(&self, rel: &str, err: &io::Error) {
        self.count.fetch_add(1, Ordering::Relaxed);
        let mut samples = self.samples.lock().expect("io error samples poisoned");
        if samples.len() < self.sample_cap {
            samples.push(format!("{rel}: {err}"));
        }
    }

    /// Returns `(count, retained_samples)`.
    pub fn summary(&self) -> (u64, Vec<String>) {
        (
            self.count.load(Ordering::Relaxed),
            self.samples
                .lock()
                .expect("io error samples poisoned")
                .clone(),
        )
    }
}

/// Lexically resolves `.` and `..` without accessing the filesystem.
pub(crate) fn lexical_normalize(path: &Path) -> PathBuf {
    let mut parts: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(Component::Normal(_)) => {
                    parts.pop();
                }
                _ => parts.push(component),
            },
            other => parts.push(other),
        }
    }
    parts.iter().collect()
}

/// Returns whether `candidate` is component-wise inside `root`.
///
/// Handle-proven paths use this exact comparison. User-typed aliases use
/// `strip_prefix_lexical` (Unicode-aware on Windows, never a string prefix).
#[cfg(any(test, windows))]
pub(crate) fn is_within(root: &Path, candidate: &Path) -> bool {
    components_within(root, candidate, |a: &OsStr, b: &OsStr| a == b)
}

/// Lexical containment is `strip_prefix_lexical` succeeding.
#[cfg(test)]
fn is_within_lexical(root: &Path, candidate: &Path) -> bool {
    strip_prefix_lexical(root, candidate).is_some()
}

/// Windows user-path equality: NT ordinal case-insensitive UTF-16.
///
/// Each UTF-16 code unit is mapped with `RtlUpcaseUnicodeChar` and
/// compared. That is a 1:1 NT upcase table lookup, not Unicode full
/// lowercase and not `to_string_lossy`. Full lowercase expands `İ` to
/// `i\u{307}`; lossy UTF-16 collapses unpaired surrogates to U+FFFD.
/// Unix keeps byte-exact names.
fn os_str_eq_lexical(left: &OsStr, right: &OsStr) -> bool {
    #[cfg(windows)]
    {
        windows_os_str_eq_ignore_case(left, right)
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

/// NT ordinal case-insensitive compare of two OS strings as UTF-16.
#[cfg(windows)]
fn windows_os_str_eq_ignore_case(left: &OsStr, right: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Wdk::System::SystemServices::RtlUpcaseUnicodeChar;

    let left: Vec<u16> = left.encode_wide().collect();
    let right: Vec<u16> = right.encode_wide().collect();
    if left.len() != right.len() {
        return false;
    }
    left.iter().zip(&right).all(|(left_unit, right_unit)| {
        // SAFETY: `RtlUpcaseUnicodeChar` is a pure 1:1 mapping on a
        // UTF-16 code unit and has no pointer preconditions.
        unsafe { RtlUpcaseUnicodeChar(*left_unit) == RtlUpcaseUnicodeChar(*right_unit) }
    })
}

/// Component-kind-safe equality used by lexical containment and extraction.
fn lexical_components_equal(left: &Component<'_>, right: &Component<'_>) -> bool {
    match (*left, *right) {
        (Component::Prefix(_), Component::Prefix(_))
        | (Component::Normal(_), Component::Normal(_)) => {
            os_str_eq_lexical(left.as_os_str(), right.as_os_str())
        }
        (Component::RootDir, Component::RootDir)
        | (Component::CurDir, Component::CurDir)
        | (Component::ParentDir, Component::ParentDir) => true,
        _ => false,
    }
}

#[cfg(any(test, windows))]
fn components_within(root: &Path, candidate: &Path, eq: impl Fn(&OsStr, &OsStr) -> bool) -> bool {
    let root: Vec<_> = root.components().collect();
    let candidate: Vec<_> = candidate.components().collect();
    candidate.len() >= root.len()
        && root
            .iter()
            .zip(candidate.iter())
            .all(|(left, right)| eq(left.as_os_str(), right.as_os_str()))
}

/// Returns the relative suffix of `candidate` under `root`, if contained.
///
/// Prefix, root-directory, and name components must each match as the same
/// kind. A leftover `Prefix` or `RootDir` (for example `C:` vs `C:\foo`)
/// is not a relative path and fails closed. Windows name and prefix text
/// uses `os_str_eq_lexical`.
fn strip_prefix_lexical(root: &Path, candidate: &Path) -> Option<PathBuf> {
    let root: Vec<_> = root.components().collect();
    let candidate: Vec<_> = candidate.components().collect();
    if candidate.len() < root.len() {
        return None;
    }
    if !root
        .iter()
        .zip(candidate.iter())
        .all(|(left, right)| lexical_components_equal(left, right))
    {
        return None;
    }
    let suffix = &candidate[root.len()..];
    if suffix
        .iter()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(suffix.iter().collect())
}

/// Converts Windows verbatim paths to stable plain forms.
///
/// Verbatim disks become `C:\...`, verbatim UNC paths preserve both the
/// server and share as `\\server\share\...`, and generic verbatim
/// prefixes such as `\\?\Volume{GUID}\...` become the absolute device
/// namespace `\\.\Volume{GUID}\...`. Device namespace and already
/// plain paths are unchanged.
pub(crate) fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::path::Prefix;

        let mut components = path.components();
        let Some(Component::Prefix(prefix)) = components.next() else {
            return path.to_path_buf();
        };
        match prefix.kind() {
            Prefix::VerbatimDisk(letter) => {
                let mut out = PathBuf::from(format!(r"{}:\", letter as char));
                push_non_root_components(&mut out, components);
                out
            }
            Prefix::VerbatimUNC(server, share) => {
                let mut out = PathBuf::from(r"\\");
                out.push(server);
                out.push(share);
                push_non_root_components(&mut out, components);
                out
            }
            Prefix::Verbatim(name) => {
                // Keep generic verbatim paths absolute. A bare
                // `Volume{GUID}\...` is relative, and `resolve_search_root`
                // would join it onto the process cwd.
                let mut raw = OsString::from(r"\\.\");
                raw.push(name);
                let mut out = PathBuf::from(raw);
                push_non_root_components(&mut out, components);
                out
            }
            Prefix::Disk(_) | Prefix::UNC(_, _) | Prefix::DeviceNS(_) => path.to_path_buf(),
        }
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

#[cfg(windows)]
fn push_non_root_components<'a>(
    out: &mut PathBuf,
    components: impl Iterator<Item = Component<'a>>,
) {
    for component in components {
        if !matches!(component, Component::RootDir) {
            out.push(component.as_os_str());
        }
    }
}

/// Converts absolute DOS and UNC paths to Win32 extended-length forms.
#[cfg(windows)]
fn windows_extended_length_path(path: &Path) -> PathBuf {
    use std::ffi::OsString;
    use std::path::Prefix;

    if !path.is_absolute() {
        return path.to_path_buf();
    }
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path.to_path_buf();
    };
    match prefix.kind() {
        Prefix::Disk(_) => {
            let mut extended = OsString::from(r"\\?\");
            extended.push(path.as_os_str());
            PathBuf::from(extended)
        }
        Prefix::UNC(server, share) => {
            let mut authority = OsString::from(r"\\?\UNC\");
            authority.push(server);
            authority.push(r"\");
            authority.push(share);
            let mut extended = PathBuf::from(authority);
            push_non_root_components(&mut extended, components);
            extended
        }
        Prefix::Verbatim(_)
        | Prefix::VerbatimDisk(_)
        | Prefix::VerbatimUNC(_, _)
        | Prefix::DeviceNS(_) => path.to_path_buf(),
    }
}

/// Type of object proven by an opened handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    volume: u64,
    file_id: [u8; 16],
}

#[cfg(all(test, windows))]
impl FileIdentity {
    /// Builds an identity for comparison tests, including ReFS-style 128-bit ids.
    pub(crate) fn from_raw(volume: u64, file_id: [u8; 16]) -> Self {
        Self { volume, file_id }
    }
}

#[derive(Debug)]
struct StableHandle {
    file: File,
    identity: FileIdentity,
    kind: EntryKind,
    /// When set, `file` is the parent directory and identity is `name`.
    ///
    /// Used on Unix platforms without `O_PATH` so find can retain a
    /// metadata-only capability for a mode-`000` file.
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    named_child: Option<OsString>,
    #[cfg(windows)]
    final_path: PathBuf,
    /// Windows metadata handles omit `SYNCHRONIZE` and must not be read.
    #[cfg(windows)]
    metadata_only: bool,
}

impl StableHandle {
    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            file: self.file.try_clone()?,
            identity: self.identity,
            kind: self.kind,
            #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
            named_child: self.named_child.clone(),
            #[cfg(windows)]
            final_path: self.final_path.clone(),
            #[cfg(windows)]
            metadata_only: self.metadata_only,
        })
    }

    fn current_identity(&self) -> io::Result<(FileIdentity, EntryKind)> {
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
        if let Some(name) = &self.named_child {
            return unix_named_identity(&self.file, name);
        }
        identity_and_kind(&self.file)
    }

    fn is_content_file(&self) -> bool {
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
        if self.named_child.is_some() {
            return false;
        }
        #[cfg(windows)]
        if self.metadata_only {
            return false;
        }
        true
    }
}

/// Distinctive error so callers skip a now-hidden entry without recording I/O.
fn hidden_entry_error() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "hidden entry")
}

/// Returns whether `error` is the silent hidden-skip marker.
pub(crate) fn is_hidden_skip(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound && error.to_string().contains("hidden entry")
}

fn handle_is_hidden(handle: &StableHandle, name: &OsStr) -> io::Result<bool> {
    if walk::name_is_hidden(name) {
        return Ok(true);
    }
    #[cfg(test)]
    if current_limiter(|limiter| limiter.force_hidden_error()).unwrap_or(false) {
        return Err(io::Error::other("injected hidden-attribute query failure"));
    }
    #[cfg(windows)]
    {
        let _ = name;
        windows_file_is_hidden(&handle.file)
    }
    #[cfg(not(windows))]
    {
        let _ = handle;
        Ok(false)
    }
}

/// Reads the current hidden bit from an already opened content handle.
pub(crate) fn opened_file_is_hidden(file: &File) -> io::Result<bool> {
    #[cfg(windows)]
    {
        windows_file_is_hidden(file)
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        Ok(false)
    }
}

/// Model-visible notice when per-path I/O made the report a lower bound.
pub(crate) fn io_incomplete_notice(io_count: u64) -> Option<String> {
    if io_count == 0 {
        None
    } else {
        Some(format!(
            "[search incomplete: {io_count} path(s) could not be read; matching results are a lower bound]"
        ))
    }
}

/// A search root backed by retained allowed-root and target handles.
///
/// `root` and `cwd` are for rendering and diagnostics only. Enumeration
/// and security decisions use `allowed`/`target` handles and each entry's
/// newly opened handle. Resolution and the walker share `limiter` so
/// ignore/handle budgets cannot be spent twice.
pub(crate) struct ResolvedRoot {
    /// Stable display spelling of the selected search root.
    pub root: PathBuf,
    /// Stable display spelling of the allowed session cwd.
    pub cwd: PathBuf,
    /// Handle-relative path from the allowed root to the selected target.
    target_relative: PathBuf,
    allowed: StableHandle,
    target: StableHandle,
    /// Ignore files from the allowed root through every ancestor opened while
    /// resolving `target`. Compiled from those retained handles, not by
    /// re-opening intermediate names later.
    ignores: walk::IgnoreStack,
    /// Invocation limiter shared by resolution and the subsequent walk.
    pub limiter: Arc<WalkLimiter>,
    /// True when any resolved component had a hidden name or attribute.
    hidden: bool,
    _allowed_lease: HandleLease,
    _target_lease: HandleLease,
}

impl std::fmt::Debug for ResolvedRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedRoot")
            .field("root", &self.root)
            .field("cwd", &self.cwd)
            .field("target_relative", &self.target_relative)
            .field("hidden", &self.hidden)
            .finish_non_exhaustive()
    }
}

impl ResolvedRoot {
    /// Returns whether the selected root is one file.
    pub fn is_file(&self) -> bool {
        self.target.kind == EntryKind::File
    }

    /// Returns whether the selected target is hidden or ignore-excluded.
    ///
    /// Applied to the on-disk allowed-relative spelling after alias open,
    /// so an explicit `path` of a gitignored file, a hidden name, a Windows
    /// `FILE_ATTRIBUTE_HIDDEN` component, or a case/8.3 alias of either is
    /// skipped the same way as a walker descendant. The session cwd itself
    /// is never skipped. Only a proven hidden bit is a silent skip; a
    /// hidden-attribute query failure is returned to the caller.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the hidden-attribute query fails.
    pub fn target_is_skipped(&self) -> io::Result<bool> {
        if self.target_relative.as_os_str().is_empty() {
            return Ok(false);
        }
        let name = self
            .target_relative
            .file_name()
            .unwrap_or_else(|| OsStr::new(""));
        if handle_is_hidden(&self.target, name)? {
            return Ok(true);
        }
        Ok(self.hidden
            || walk::relative_is_skipped(&self.ignores, &self.target_relative, !self.is_file()))
    }

    /// Handle-relative path from the allowed root to the selected target.
    ///
    /// On Windows this is the on-disk component spelling after alias open.
    #[cfg(test)]
    pub(crate) fn target_relative(&self) -> &Path {
        &self.target_relative
    }

    /// Returns a mutable reference to the already opened single-file target.
    pub fn target_file_mut(&mut self) -> io::Result<&mut File> {
        if !self.is_file() {
            return Err(io::Error::other("search target is not a file"));
        }
        if !self.target.is_content_file() {
            return Err(io::Error::other("search target was bound metadata-only"));
        }
        self.validate_target()?;
        Ok(&mut self.target.file)
    }

    /// Revalidates the retained target handle immediately before reporting.
    pub fn validate_target(&self) -> io::Result<()> {
        let (identity, kind) = self.target.current_identity()?;
        if identity != self.target.identity || kind != self.target.kind {
            return Err(io::Error::other("search target identity changed"));
        }
        let name = self
            .target_relative
            .file_name()
            .unwrap_or_else(|| OsStr::new(""));
        if !self.target_relative.as_os_str().is_empty() && handle_is_hidden(&self.target, name)? {
            return Err(hidden_entry_error());
        }
        let allowed_identity = identity_and_kind(&self.allowed.file)?.0;
        if allowed_identity != self.allowed.identity {
            return Err(io::Error::other("allowed-root identity changed"));
        }
        #[cfg(windows)]
        {
            let final_path = final_path_by_handle(&self.target.file)?;
            if !is_within(&self.allowed.final_path, &final_path) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "opened target is outside the allowed root",
                ));
            }
        }
        Ok(())
    }

    /// Opens one listed child for content read relative to the parent handle.
    ///
    /// `name` is the directory-entry spelling from the same parent handle.
    /// The open is parent-relative and no-follow; it never rebuilds a path
    /// and never re-walks ancestors from the selected target. Windows uses
    /// the enumerated spelling case-sensitively so a listing of `Visible.txt`
    /// cannot be redirected to ignore-excluded `visible.txt`. Unix requests
    /// `O_RDONLY`; Windows requests `FILE_GENERIC_READ`.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent is no longer a directory, `name` is not
    /// a single safe component, the child type changed, a link is encountered,
    /// or Windows containment against the allowed root fails.
    pub fn open_walked(
        &self,
        parent: &File,
        name: &OsStr,
        expected: EntryKind,
    ) -> io::Result<File> {
        self.validate_target()?;
        validate_component_name(name)?;
        let (_, parent_kind) = identity_and_kind(parent)?;
        if parent_kind != EntryKind::Directory {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "walk parent is no longer a directory",
            ));
        }

        let opened = open_child_handle(parent, name, Some(expected), SearchAccess::Content)?;
        let (identity, kind) = opened.current_identity()?;
        if identity != opened.identity || kind != expected || opened.kind != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "walked object type or identity changed before it was opened",
            ));
        }
        #[cfg(windows)]
        if !is_within(&self.allowed.final_path, &opened.final_path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "opened entry is outside the allowed root",
            ));
        }
        if handle_is_hidden(&opened, name)? {
            return Err(hidden_entry_error());
        }
        Ok(opened.file)
    }

    /// Opens a listed directory for descent after proving metadata identity.
    pub fn open_descended_dir(&self, parent: &File, name: &OsStr) -> io::Result<File> {
        self.validate_target()?;
        validate_component_name(name)?;
        let (_, parent_kind) = identity_and_kind(parent)?;
        if parent_kind != EntryKind::Directory {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "walk parent is no longer a directory",
            ));
        }
        let meta_identity = confirm_child_identity(parent, name, EntryKind::Directory)?;
        let opened = open_child_handle(
            parent,
            name,
            Some(EntryKind::Directory),
            SearchAccess::Content,
        )?;
        let (identity, kind) = opened.current_identity()?;
        if identity != meta_identity || identity != opened.identity || kind != EntryKind::Directory
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "walked object type or identity changed before it was opened",
            ));
        }
        #[cfg(windows)]
        if !is_within(&self.allowed.final_path, &opened.final_path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "opened entry is outside the allowed root",
            ));
        }
        if handle_is_hidden(&opened, name)? {
            return Err(hidden_entry_error());
        }
        Ok(opened.file)
    }

    /// Confirms one listed child without requiring content-read permission.
    ///
    /// Find reports names the caller can list even when file contents are
    /// unreadable. Linux/Android confirm with `openat2` + `O_PATH` and the
    /// same `RESOLVE_BENEATH | NO_XDEV | NO_SYMLINKS` bits as descent.
    /// Other Unix confirms with no-follow metadata and the current
    /// mount / type / `nlink` checks, and fails closed when that proof is
    /// unavailable. Windows opens with `FILE_READ_ATTRIBUTES` rather than
    /// `FILE_GENERIC_READ`.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent is no longer a directory, `name` is
    /// not a single safe component, the child type changed, a link is
    /// encountered, a mount boundary cannot be proven, or Windows containment
    /// against the allowed root fails.
    pub fn confirm_walked(
        &self,
        parent: &File,
        name: &OsStr,
        expected: EntryKind,
    ) -> io::Result<()> {
        self.validate_target()?;
        validate_component_name(name)?;
        let (_, parent_kind) = identity_and_kind(parent)?;
        if parent_kind != EntryKind::Directory {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "walk parent is no longer a directory",
            ));
        }
        let _identity = confirm_child_identity(parent, name, expected)?;
        Ok(())
    }
}

fn confirm_child_identity(
    parent: &File,
    name: &OsStr,
    expected: EntryKind,
) -> io::Result<FileIdentity> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let file = open_named_unix(parent, name, Some(expected), SearchAccess::Metadata)?;
        let opened = stable_from_file(file)?;
        if handle_is_hidden(&opened, name)? {
            return Err(hidden_entry_error());
        }
        Ok(opened.identity)
    }

    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    {
        confirm_named_unix_metadata(parent, name, Some(expected))?;
        let (identity, kind) = unix_named_identity(parent, name)?;
        if kind != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "walked object type or identity changed before it was opened",
            ));
        }
        if walk::name_is_hidden(name) {
            return Err(hidden_entry_error());
        }
        Ok(identity)
    }

    #[cfg(windows)]
    {
        let opened = open_named_windows(
            parent,
            name,
            Some(expected),
            NameMatch::Exact,
            SearchAccess::Metadata,
        )?;
        let (identity, kind) = identity_and_kind(&opened.file)?;
        if identity != opened.identity || kind != expected || opened.kind != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "walked object type or identity changed before it was opened",
            ));
        }
        if handle_is_hidden(&opened, name)? {
            return Err(hidden_entry_error());
        }
        Ok(opened.identity)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name, expected);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative child confirm is not implemented on this platform",
        ))
    }
}

fn open_child_handle(
    parent: &File,
    name: &OsStr,
    expected: Option<EntryKind>,
    access: SearchAccess,
) -> io::Result<StableHandle> {
    #[cfg(unix)]
    {
        stable_from_file(open_named_unix(parent, name, expected, access)?)
    }
    #[cfg(windows)]
    {
        open_named_windows(parent, name, expected, NameMatch::Exact, access)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name, expected, access);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative child open is not implemented on this platform",
        ))
    }
}

/// Resolves `cwd` and `path_arg` to a handle-backed search root.
///
/// Relative `cwd` is made absolute against the process cwd; an already
/// absolute `cwd` does not consult the process cwd. An omitted
/// path or any argument that lexically normalizes to `cwd` denotes the allowed
/// root. Relative arguments are normalized with an anchored component stack,
/// so a leading parent can never leave and later re-enter the root. Unix and
/// Windows target traversal is handle-relative and no-follow. Windows also
/// validates final Unicode paths and stores on-disk component spelling after
/// an alias open so anchored ignore matching sees `Visible`, not `visible`.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArgs`] for lexical escapes, symlink/reparse
/// targets, or handle-proven containment failures. Missing or inaccessible
/// roots, cancelled or overdue ignore reads, and oversized ignore files
/// return [`ToolError::Execution`].
#[cfg(test)]
pub(crate) fn resolve_search_root(
    cwd: &Path,
    path_arg: Option<&str>,
) -> Result<ResolvedRoot, ToolError> {
    resolve_search_root_cancel(cwd, path_arg, &CancellationToken::new(), &Limits::default())
}

/// Resolves a search root while honouring `cancel` and `limits`.
///
/// Ignore files loaded during resolution use the same cancel token and
/// [`WalkLimiter`] the walker will share, so a timeout cannot keep reading
/// and ignore/handle budgets cannot be spent twice.
///
/// # Errors
///
/// Same as [`resolve_search_root`], plus cancellation and deadline expiry
/// while reading ignore files.
#[cfg(test)]
pub(crate) fn resolve_search_root_cancel(
    cwd: &Path,
    path_arg: Option<&str>,
    cancel: &CancellationToken,
    limits: &Limits,
) -> Result<ResolvedRoot, ToolError> {
    resolve_search_root_with_access(cwd, path_arg, cancel, limits, SearchAccess::Content)
}

/// Returns whether `on_disk` and its lower-case alias name the same inode.
///
/// Independent of handle-relative resolve so casefold tests can skip a
/// case-sensitive volume without treating a later resolve error as "unsupported".
#[cfg(all(test, unix))]
pub(crate) fn unix_casefold_alias_supported(dir: &Path, on_disk: &str) -> bool {
    use std::os::unix::fs::MetadataExt;

    let folded = on_disk.to_lowercase();
    if folded == on_disk {
        return false;
    }
    let Ok(canonical) = std::fs::symlink_metadata(dir.join(on_disk)) else {
        return false;
    };
    let Ok(alias) = std::fs::symlink_metadata(dir.join(folded)) else {
        return false;
    };
    canonical.dev() == alias.dev() && canonical.ino() == alias.ino()
}

/// Resolves a search root with an explicit content/metadata capability.
pub(crate) fn resolve_search_root_with_access(
    cwd: &Path,
    path_arg: Option<&str>,
    cancel: &CancellationToken,
    limits: &Limits,
    access: SearchAccess,
) -> Result<ResolvedRoot, ToolError> {
    #[cfg(test)]
    if let Some(counter) = &limits.resolve_count {
        counter.fetch_add(1, Ordering::Relaxed);
    }
    let cwd = strip_verbatim_prefix(cwd);
    let absolute_cwd = if cwd.is_absolute() {
        lexical_normalize(&cwd)
    } else {
        let process_cwd = std::env::current_dir().map_err(|error| {
            ToolError::Execution(format!("process cwd is not accessible: {error}"))
        })?;
        lexical_normalize(&process_cwd.join(&cwd))
    };
    if !absolute_cwd.is_absolute()
        || absolute_cwd
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(ToolError::InvalidArgs(format!(
            "session cwd is not an absolute resolvable path: {}",
            cwd.display()
        )));
    }

    let allowed = open_allowed_root(&absolute_cwd)
        .map_err(|error| ToolError::Execution(format!("session cwd is not accessible: {error}")))?;
    if allowed.kind != EntryKind::Directory {
        return Err(ToolError::Execution(
            "session cwd is not a directory".to_owned(),
        ));
    }

    #[cfg(windows)]
    let allowed_path = allowed.final_path.clone();
    #[cfg(not(windows))]
    let allowed_path = absolute_cwd.clone();

    // Windows: the session-given cwd spelling can differ from the
    // handle-proven final path through an 8.3 alias (`RUNNER~1` vs
    // `runneradmin`); both spell the tree the retained handle anchors.
    #[cfg(windows)]
    let session_spelling: Option<&Path> = Some(absolute_cwd.as_path());
    #[cfg(not(windows))]
    let session_spelling: Option<&Path> = None;

    let relative = match path_arg {
        Some(raw) => resolve_relative_argument(&allowed_path, session_spelling, raw)
            .map_err(|()| ToolError::InvalidArgs(format!("path escapes the session cwd: {raw}")))?,
        None => PathBuf::new(),
    };
    let raw = path_arg.unwrap_or("");
    let limiter = Arc::new(WalkLimiter::new(limits));
    let _seams = bind_current_limiter(&limiter);
    if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
        return Err(ToolError::Execution(
            "search ignore files cannot be loaded: search stopped".to_owned(),
        ));
    }
    let allowed_lease = limiter
        .lease()
        .map_err(|error| ToolError::Execution(format!("search handle budget: {error}")))?;
    let (target, root_path, ignores, target_relative, hidden) =
        open_target_with_ignores(&allowed, &allowed_path, &relative, &limiter, cancel, access)
            .map_err(|error| {
                map_target_or_ignore_error(raw, relative.as_os_str().is_empty(), error)
            })?;
    let target_lease = limiter
        .lease()
        .map_err(|error| ToolError::Execution(format!("search handle budget: {error}")))?;

    // Keep both identities live for the full operation. `allowed` remains
    // separate even when `target` is a duplicated handle to the same object.
    let final_allowed_identity = identity_and_kind(&allowed.file)
        .map_err(|error| ToolError::Execution(format!("allowed-root validation failed: {error}")))?
        .0;
    if final_allowed_identity != allowed.identity {
        return Err(ToolError::Execution(
            "allowed-root identity changed during resolution".to_owned(),
        ));
    }

    Ok(ResolvedRoot {
        root: root_path,
        cwd: allowed_path,
        target_relative,
        allowed,
        target,
        ignores,
        limiter,
        hidden,
        _allowed_lease: allowed_lease,
        _target_lease: target_lease,
    })
}

/// Handle-backed grep/find target bound during dispatch preparation.
///
/// A value exists only as a ready retained root: `prepare_search` never
/// constructs a path key with an empty root. The dispatcher moves that root
/// into execution. A later path rewrite must re-prepare; execution never
/// re-resolves a prepared root, including after the root is taken.
pub struct PreparedSearch {
    key: String,
    access: SearchAccess,
    root: Mutex<Option<ResolvedRoot>>,
}

impl std::fmt::Debug for PreparedSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedSearch")
            .field("key", &self.key)
            .field("access", &self.access)
            .finish_non_exhaustive()
    }
}

impl PreparedSearch {
    /// Returns the cwd-relative on-disk spelling bound to this capability.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Access mode retained by this capability.
    pub fn access(&self) -> SearchAccess {
        self.access
    }

    /// Takes the retained root exactly once for execution.
    pub(crate) fn take_root(&self) -> Option<ResolvedRoot> {
        self.root.lock().ok().and_then(|mut guard| guard.take())
    }
}

/// Resolves `cwd` and `path_arg` once for retained-capability execution.
///
/// Any resolve, open, alias, ignore, missing, or sharing failure is a
/// terminating [`ToolError`]. Success is always a ready retained root.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] or [`ToolError::InvalidArgs`] when the
/// target cannot be bound, including missing paths, sharing violations,
/// cancellation, and deadline expiry.
pub fn prepare_search(
    cwd: &Path,
    path_arg: Option<&str>,
    cancel: &CancellationToken,
) -> Result<PreparedSearch, ToolError> {
    prepare_search_with_access(cwd, path_arg, cancel, SearchAccess::Content)
}

/// [`prepare_search`] with an explicit content/metadata capability.
///
/// # Errors
///
/// Same as [`prepare_search`].
pub fn prepare_search_with_access(
    cwd: &Path,
    path_arg: Option<&str>,
    cancel: &CancellationToken,
    access: SearchAccess,
) -> Result<PreparedSearch, ToolError> {
    prepare_search_with_limits_access(cwd, path_arg, cancel, &Limits::default(), access)
}

#[cfg(test)]
pub(crate) fn prepare_search_with_limits(
    cwd: &Path,
    path_arg: Option<&str>,
    cancel: &CancellationToken,
    limits: &Limits,
) -> Result<PreparedSearch, ToolError> {
    prepare_search_with_limits_access(cwd, path_arg, cancel, limits, SearchAccess::Content)
}

pub(crate) fn prepare_search_with_limits_access(
    cwd: &Path,
    path_arg: Option<&str>,
    cancel: &CancellationToken,
    limits: &Limits,
    access: SearchAccess,
) -> Result<PreparedSearch, ToolError> {
    let root = resolve_search_root_with_access(cwd, path_arg, cancel, limits, access)?;
    Ok(PreparedSearch {
        key: posix_relative_key(&root.target_relative),
        access,
        root: Mutex::new(Some(root)),
    })
}

/// Binds the grep/find root for execution.
///
/// A preflight [`PreparedSearch`] is always a ready retained root. Execution
/// takes that root once and never re-resolves, even when the root is missing
/// or already consumed. Only the internal tool path without dispatch
/// preparation may resolve once here.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] when a prepared root is absent, already
/// consumed, or does not match its path key. The no-preflight path returns the
/// same errors as [`resolve_search_root_cancel`].
#[cfg(test)]
pub(crate) fn bind_search_root(
    prepared: Option<&PreparedSearch>,
    cwd: &Path,
    path_arg: Option<&str>,
    cancel: &CancellationToken,
    limits: &Limits,
) -> Result<ResolvedRoot, ToolError> {
    bind_search_root_with_access(
        prepared,
        cwd,
        path_arg,
        cancel,
        limits,
        SearchAccess::Content,
    )
}

pub(crate) fn bind_search_root_with_access(
    prepared: Option<&PreparedSearch>,
    cwd: &Path,
    path_arg: Option<&str>,
    cancel: &CancellationToken,
    limits: &Limits,
    access: SearchAccess,
) -> Result<ResolvedRoot, ToolError> {
    if let Some(prepared) = prepared {
        if prepared.access() != access {
            return Err(ToolError::Execution(format!(
                "prepared search access mismatch: prepared {:?}, requested {:?}",
                prepared.access(),
                access
            )));
        }
        let Some(root) = prepared.take_root() else {
            return Err(ToolError::Execution(
                "prepared search root is missing or was already consumed".to_owned(),
            ));
        };
        let bound = posix_relative_key(&root.target_relative);
        if bound != prepared.key() {
            return Err(ToolError::Execution(
                "prepared search root does not match its path key".to_owned(),
            ));
        }
        if let Some(deadline) = limits.deadline {
            root.limiter.refresh_deadline_at(deadline);
        } else {
            root.limiter.refresh_deadline(limits.time_limit);
        }
        return Ok(root);
    }
    resolve_search_root_with_access(cwd, path_arg, cancel, limits, access)
}

pub(crate) fn posix_relative_key(relative: &Path) -> String {
    if relative.as_os_str().is_empty() {
        ".".to_owned()
    } else {
        to_posix(relative)
    }
}

fn open_target_with_ignores(
    allowed: &StableHandle,
    allowed_path: &Path,
    relative: &Path,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
    access: SearchAccess,
) -> io::Result<(StableHandle, PathBuf, walk::IgnoreStack, PathBuf, bool)> {
    let components: Vec<_> = relative.components().collect();
    if components.len() > limiter.max_walk_depth() {
        limiter.stop("walk depth limit reached");
        return Err(io::Error::other("walk depth limit reached"));
    }
    let mut ignores = walk::IgnoreStack::default();
    ignores
        .seed_git_boundary(&allowed.file, limiter, cancel)
        .map_err(wrap_ignore_error)?;
    ignores
        .ingest(&allowed.file, Path::new(""), limiter, cancel)
        .map_err(wrap_ignore_error)?;
    if relative.as_os_str().is_empty() {
        return Ok((
            allowed.try_clone()?,
            allowed_path.to_path_buf(),
            ignores,
            PathBuf::new(),
            false,
        ));
    }

    let mut parent = allowed.file.try_clone()?;
    let mut walked = PathBuf::new();
    let mut hidden = false;
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsafe relative component",
            ));
        };
        let is_last = index + 1 == components.len();
        if !limiter.try_reserve_entry() {
            return Err(io::Error::other("walk entry limit reached"));
        }
        if is_last {
            return open_final_target(
                allowed,
                allowed_path,
                relative,
                &parent,
                name,
                &walked,
                ignores,
                limiter,
                cancel,
                hidden,
                access,
            );
        }
        let (child, exact) = open_alias_component(
            &parent,
            name,
            Some(EntryKind::Directory),
            SearchAccess::Content,
            limiter,
            cancel,
        )?;
        hidden |= component_is_hidden(&exact, &child)?;
        walked.push(exact);
        ignores
            .ingest(&child, &walked, limiter, cancel)
            .map_err(wrap_ignore_error)?;
        parent = child;
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "empty relative path cannot be opened as a walked entry",
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "final target open needs the retained handles, walked spelling, ignore stop state, and hidden accumulation"
)]
fn open_final_target(
    allowed: &StableHandle,
    allowed_path: &Path,
    relative: &Path,
    parent: &File,
    name: &OsStr,
    walked: &Path,
    mut ignores: walk::IgnoreStack,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
    mut hidden: bool,
    access: SearchAccess,
) -> io::Result<(StableHandle, PathBuf, walk::IgnoreStack, PathBuf, bool)> {
    let (mut target, exact) = open_alias_target(allowed, parent, name, access, limiter, cancel)?;
    if target.kind == EntryKind::Directory && access == SearchAccess::Metadata {
        let (content, content_exact) = open_alias_target(
            allowed,
            parent,
            name,
            SearchAccess::Content,
            limiter,
            cancel,
        )?;
        if content.identity != target.identity || content_exact != exact {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "directory identity changed between metadata and content open",
            ));
        }
        target = content;
    }
    hidden |= handle_is_hidden(&target, &exact)?;
    let mut target_relative = walked.to_path_buf();
    target_relative.push(exact);
    if target.kind == EntryKind::Directory {
        if !target.is_content_file() {
            return Err(io::Error::other(
                "directory search root was bound metadata-only",
            ));
        }
        ignores
            .ingest(&target.file, &target_relative, limiter, cancel)
            .map_err(wrap_ignore_error)?;
    }
    #[cfg(unix)]
    let root_path = {
        let _ = relative;
        allowed_path.join(&target_relative)
    };
    #[cfg(windows)]
    let root_path = {
        let _ = (allowed_path, relative);
        target.final_path.clone()
    };
    #[cfg(not(any(unix, windows)))]
    let root_path = allowed_path.join(relative);
    Ok((target, root_path, ignores, target_relative, hidden))
}

fn component_is_hidden(name: &OsStr, file: &File) -> io::Result<bool> {
    if walk::name_is_hidden(name) {
        return Ok(true);
    }
    #[cfg(windows)]
    {
        windows_file_is_hidden(file)
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        Ok(false)
    }
}

/// Opens one user-typed child and returns the on-disk component spelling.
fn open_alias_component(
    parent: &File,
    name: &OsStr,
    expected: Option<EntryKind>,
    access: SearchAccess,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<(File, OsString)> {
    let (handle, exact) = open_alias_handle(parent, name, expected, access, limiter, cancel)?;
    Ok((handle.file, exact))
}

fn open_alias_target(
    allowed: &StableHandle,
    parent: &File,
    name: &OsStr,
    access: SearchAccess,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<(StableHandle, OsString)> {
    let (handle, exact) = open_alias_handle(parent, name, None, access, limiter, cancel)?;
    #[cfg(windows)]
    if !is_within(&allowed.final_path, &handle.final_path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "opened target is outside the allowed root",
        ));
    }
    #[cfg(not(windows))]
    let _ = allowed;
    Ok((handle, exact))
}

fn open_alias_handle(
    parent: &File,
    name: &OsStr,
    expected: Option<EntryKind>,
    access: SearchAccess,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<(StableHandle, OsString)> {
    #[cfg(unix)]
    {
        unix_open_alias(parent, name, expected, access, limiter, cancel)
    }
    #[cfg(windows)]
    {
        let _ = cancel;
        let _ = limiter;
        let handle = open_named_windows(parent, name, expected, NameMatch::Alias, access)?;
        let exact = on_disk_component_name(&handle)?;
        Ok((handle, exact))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name, expected, access, limiter, cancel);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative child open is not implemented on this platform",
        ))
    }
}

/// Opens a user-typed Unix component and returns the unique on-disk spelling.
///
/// The opened object's identity is matched against every directory entry of
/// `parent`. Zero or several matches fail closed so a case-insensitive alias
/// cannot keep the caller's spelling as the path or ignore key.
#[cfg(unix)]
fn unix_open_alias(
    parent: &File,
    name: &OsStr,
    expected: Option<EntryKind>,
    access: SearchAccess,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<(StableHandle, OsString)> {
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    if access == SearchAccess::Metadata && expected != Some(EntryKind::Directory) {
        confirm_named_unix_metadata(parent, name, expected)?;
        let (identity, kind) = unix_named_identity(parent, name)?;
        if let Some(expected) = expected
            && kind != expected
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "walked object type or identity changed before it was opened",
            ));
        }
        let exact = unix_on_disk_component_name(parent, identity, limiter, cancel)?;
        let handle = StableHandle {
            file: parent.try_clone()?,
            identity,
            kind,
            named_child: Some(exact.clone()),
        };
        return Ok((handle, exact));
    }
    let file = open_named_unix(parent, name, expected, access)?;
    let handle = stable_from_file(file)?;
    let exact = unix_on_disk_component_name(parent, handle.identity, limiter, cancel)?;
    Ok((handle, exact))
}

#[cfg(target_os = "macos")]
pub(crate) fn unix_device_identity(value: libc::dev_t) -> io::Result<u64> {
    // Darwin dev_t is signed. Match MetadataExt::dev's bit-preserving cast
    // rather than rejecting valid high-bit device identifiers.
    Ok(value as u64)
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn unix_device_identity(value: libc::dev_t) -> io::Result<u64> {
    unix_identity_part(value, "device")
}

#[cfg(target_os = "macos")]
pub(crate) fn unix_inode_identity(value: u64) -> io::Result<u64> {
    // Darwin ino_t is already u64. Keep the kernel identity bits unchanged.
    Ok(value)
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn unix_inode_identity<T>(value: T) -> io::Result<u64>
where
    T: TryInto<u64>,
{
    unix_identity_part(value, "inode")
}

#[cfg(unix)]
fn unix_identity_part<T>(value: T, label: &str) -> io::Result<u64>
where
    T: TryInto<u64>,
{
    value.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid {label} identity"),
        )
    })
}

/// Reopens `parent` as `"."` so listing does not share the parent's offset.
///
/// POSIX `fdopendir` starts at the current offset of the given descriptor.
/// `File::try_clone` duplicates the descriptor but not the open-file
/// description, so a cloned listing would resume after a previous scan's
/// EOF and miss every entry. Opening `"."` creates a distinct description.
#[cfg(unix)]
fn unix_reopen_dir_for_listing(parent: &File) -> io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let c_dot = c".";
    let flags =
        libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY;
    let descriptor = open_unix_descriptor(
        parent.as_raw_fd(),
        c_dot.as_ptr(),
        flags,
        SearchAccess::Content,
    )?;
    // SAFETY: `descriptor` is a fresh owned fd from `openat`/`openat2`.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

/// Unique directory-entry spelling in `parent` whose identity equals `want`.
#[cfg(unix)]
fn unix_on_disk_component_name(
    parent: &File,
    want: FileIdentity,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<OsString> {
    use std::ffi::{CStr, CString};
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, IntoRawFd};
    use std::os::unix::ffi::OsStrExt;

    struct DirOwner(*mut libc::DIR);
    impl Drop for DirOwner {
        fn drop(&mut self) {
            // SAFETY: `fdopendir` transferred exclusive ownership of this `DIR*`.
            unsafe {
                libc::closedir(self.0);
            }
        }
    }

    let listing = unix_reopen_dir_for_listing(parent)?;
    let fd = listing.into_raw_fd();
    // SAFETY: `fd` is exclusively owned. `fdopendir` takes it or we close it.
    let dirp = unsafe { libc::fdopendir(fd) };
    if dirp.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: `fdopendir` failed, so this process still owns `fd`.
        unsafe {
            libc::close(fd);
        }
        return Err(error);
    }
    let owner = DirOwner(dirp);
    let mut found: Option<OsString> = None;
    let mut scanned = 0usize;
    loop {
        if cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "search stopped"));
        }
        if !limiter.try_reserve_entry() {
            return Err(io::Error::other("walk entry limit reached"));
        }
        unix_clear_errno();
        #[cfg(test)]
        limiter.record_entry_access();
        // SAFETY: `owner.0` is a live `DIR*`. A non-null `dirent` is valid
        // until the next `readdir`/`closedir` on this stream.
        let entry = unsafe { libc::readdir(owner.0) };
        if entry.is_null() {
            limiter.release_entry();
            let error = io::Error::last_os_error();
            if error.raw_os_error().unwrap_or(0) == 0 {
                break;
            }
            return Err(error);
        }
        // SAFETY: `d_name` is a NUL-terminated component from `readdir`.
        let c_name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        let name = OsStr::from_bytes(c_name.to_bytes());
        if name == "." || name == ".." {
            limiter.release_entry();
            continue;
        }
        scanned += 1;
        if scanned > MAX_DIR_WIDTH {
            return Err(io::Error::other(
                "directory is too wide to prove unique on-disk component spelling",
            ));
        }
        let c_owned = CString::new(name.as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "path component contains NUL")
        })?;
        let mut stat = MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `parent` is a live directory fd, `c_owned` is a single
        // component, and `AT_SYMLINK_NOFOLLOW` inspects the named child.
        let status = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                c_owned.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if status != 0 {
            let error = io::Error::last_os_error();
            // A vanished sibling cannot currently name `want`. Failing the
            // whole scan would treat a busy case-sensitive parent (shared
            // temp directories) as identity ambiguity. Live hardlink and
            // case-alias duplicates are still observed below.
            if error.kind() == io::ErrorKind::NotFound {
                continue;
            }
            return Err(io::Error::other(
                "cannot prove unique on-disk component spelling",
            ));
        }
        // SAFETY: `fstatat` returned 0 and initialized `stat`.
        let stat = unsafe { stat.assume_init() };
        let device = unix_device_identity(stat.st_dev)?;
        let inode = unix_identity_part(stat.st_ino, "inode")?;
        let identity = FileIdentity { device, inode };
        if identity == want {
            if found.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "multiple directory entries share the opened identity",
                ));
            }
            found = Some(name.to_os_string());
        }
    }
    found.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "opened object has no unique on-disk file name",
        )
    })
}

/// Clears thread-local errno before `readdir` so EOF is not a stale error.
///
/// Symbols match `libc` 0.2.189. Unknown Unix errno ABIs fail at compile
/// time rather than treating a leftover errno as a listing failure.
#[cfg(unix)]
pub(super) fn unix_clear_errno() {
    // SAFETY: writing 0 into thread-local errno distinguishes `readdir`
    // EOF from a real failure after a previous fallible syscall.
    #[cfg(any(
        target_os = "linux",
        target_os = "emscripten",
        target_os = "fuchsia",
        target_os = "hurd",
        target_os = "l4re",
        target_os = "redox",
        target_os = "dragonfly"
    ))]
    unsafe {
        *libc::__errno_location() = 0;
    }
    #[cfg(any(target_os = "android", target_os = "openbsd", target_os = "netbsd"))]
    unsafe {
        *libc::__errno() = 0;
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos",
        target_os = "visionos",
        target_os = "freebsd"
    ))]
    unsafe {
        *libc::__error() = 0;
    }
    #[cfg(any(target_os = "solaris", target_os = "illumos"))]
    unsafe {
        *libc::___errno() = 0;
    }
    #[cfg(target_os = "aix")]
    unsafe {
        *libc::_Errno() = 0;
    }
    #[cfg(target_os = "haiku")]
    unsafe {
        *libc::_errnop() = 0;
    }
    #[cfg(target_os = "nto")]
    unsafe {
        *libc::__get_errno_ptr() = 0;
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "emscripten",
        target_os = "fuchsia",
        target_os = "hurd",
        target_os = "l4re",
        target_os = "redox",
        target_os = "dragonfly",
        target_os = "android",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos",
        target_os = "visionos",
        target_os = "freebsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "haiku",
        target_os = "nto"
    )))]
    {
        compile_error!("readdir errno ABI is not implemented for this Unix target");
    }
}

/// Last on-disk path component of a Windows handle opened by alias.
///
/// `GetFinalPathNameByHandleW` returns the NTFS long name with stored case,
/// so a user-typed `visible` or `VISIBL~1` becomes `Visible` for ignore
/// matching. The last component is taken from the UTF-16 handle path, not
/// `Path::file_name`, which can strip trailing dots and spaces. Unix keeps
/// the opened byte spelling.
#[cfg(windows)]
fn on_disk_component_name(handle: &StableHandle) -> io::Result<OsString> {
    last_wide_component(handle.final_path.as_os_str()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "opened object has no on-disk file name",
        )
    })
}

/// Last `\`/`/`-separated UTF-16 component, preserving stored spelling.
#[cfg(windows)]
fn last_wide_component(path: &OsStr) -> Option<OsString> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    let wide: Vec<u16> = path.encode_wide().collect();
    let start = wide
        .iter()
        .rposition(|&unit| unit == u16::from(b'\\') || unit == u16::from(b'/'))
        .map_or(0, |index| index + 1);
    let last = wide.get(start..)?;
    if last.is_empty() || last == [u16::from(b'.')] || last == [u16::from(b'.'), u16::from(b'.')] {
        return None;
    }
    Some(OsString::from_wide(last))
}

#[cfg(windows)]
fn open_windows_parent_directory(dir: &File) -> io::Result<ParentDirectory> {
    let path = final_path_by_handle(dir)?;
    let Some(parent_path) = windows_git_parent_path(&path) else {
        return Ok(ParentDirectory::FilesystemRoot);
    };
    let Some(child_name) = last_wide_component(path.as_os_str()) else {
        return Ok(ParentDirectory::FilesystemRoot);
    };
    let parent_path =
        apply_parent_discovery_hook(&path)?.unwrap_or_else(|| parent_path.to_path_buf());
    let parent = open_windows_path_nofollow(&parent_path)?;
    let reopened = open_named_windows(
        &parent,
        &child_name,
        Some(EntryKind::Directory),
        NameMatch::Exact,
        SearchAccess::Content,
    )?;
    let want = identity_and_kind(dir)?.0;
    if reopened.identity != want {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "parent directory no longer contains the opened child",
        ));
    }
    Ok(ParentDirectory::Parent(parent))
}

#[cfg(windows)]
fn windows_git_parent_path(path: &Path) -> Option<&Path> {
    let parent = path.parent()?;
    if parent.as_os_str().is_empty() || parent == path {
        return None;
    }
    if parent
        .components()
        .all(|component| matches!(component, Component::Prefix(_)))
    {
        return None;
    }
    Some(parent)
}

#[cfg(windows)]
fn open_windows_path_nofollow(path: &Path) -> io::Result<File> {
    let mut components = path.components();
    let Some(first) = components.next() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "parent path is empty",
        ));
    };
    let mut prefix = PathBuf::from(first.as_os_str());
    if let Some(Component::RootDir) = components.clone().next() {
        let _ = components.next();
        prefix.push(std::path::MAIN_SEPARATOR_STR);
    }
    let mut current = open_windows_prefix(&prefix)?.file;
    for component in components {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "parent path is not normalized",
                ));
            }
            Component::Normal(name) => {
                current = open_named_windows(
                    &current,
                    name,
                    Some(EntryKind::Directory),
                    NameMatch::Exact,
                    SearchAccess::Content,
                )?
                .file;
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "parent path has an interior prefix",
                ));
            }
        }
    }
    Ok(current)
}

#[cfg(windows)]
fn windows_child_name_in_parent(parent: &File, child: &File) -> io::Result<OsString> {
    let want = identity_and_kind(child)?.0;
    let child_path = final_path_by_handle(child)?;
    let name = last_wide_component(child_path.as_os_str()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "parent directory has no on-disk name",
        )
    })?;
    let reopened = open_named_windows(
        parent,
        &name,
        Some(EntryKind::Directory),
        NameMatch::Exact,
        SearchAccess::Content,
    )?;
    if reopened.identity != want {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "parent directory no longer contains the opened child",
        ));
    }
    Ok(name)
}

fn wrap_ignore_error(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::Interrupted || is_ignore_load_error(&error) {
        error
    } else {
        io::Error::other(format!("search ignore files cannot be loaded: {error}"))
    }
}

fn map_target_or_ignore_error(raw: &str, cwd_target: bool, error: io::Error) -> ToolError {
    if error.kind() == io::ErrorKind::Interrupted || is_ignore_load_error(&error) {
        let message = error.to_string();
        if message.contains("search ignore files cannot be loaded") {
            return ToolError::Execution(message);
        }
        return ToolError::Execution(format!("search ignore files cannot be loaded: {error}"));
    }
    if cwd_target {
        ToolError::Execution(format!("session cwd handle cannot be retained: {error}"))
    } else {
        map_target_open_error(raw, error)
    }
}

fn is_ignore_load_error(error: &io::Error) -> bool {
    let message = error.to_string();
    message.contains("ignore file exceeds size limit")
        || message.contains("ignore file is not valid UTF-8")
        || message.contains("search ignore files cannot be loaded")
}

fn map_target_open_error(raw: &str, error: io::Error) -> ToolError {
    if matches!(
        error.kind(),
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData
    ) {
        ToolError::InvalidArgs(format!(
            "path escapes the session cwd, crosses a link, or is not a regular file/directory: {raw}"
        ))
    } else {
        ToolError::Execution(format!(
            "search path does not exist or is inaccessible: {raw}: {error}"
        ))
    }
}

pub(crate) fn resolve_relative_argument(
    root: &Path,
    alias_root: Option<&Path>,
    raw: &str,
) -> Result<PathBuf, ()> {
    let argument = strip_verbatim_prefix(Path::new(raw));
    if argument.is_absolute() {
        let normalized = lexical_normalize(&argument);
        // One predicate: containment and relative extraction cannot diverge.
        // `alias_root` is the session-given spelling of the tree that `root`
        // proves by handle: Windows 8.3 aliases (`RUNNER~1` vs
        // `runneradmin`) can differ from the on-disk long name, and an
        // argument echoing the session spelling stays contained. The walk
        // still opens component-by-component from the retained handle, so
        // permissiveness here cannot escape the anchored tree.
        strip_prefix_lexical(root, &normalized)
            .or_else(|| alias_root.and_then(|alias| strip_prefix_lexical(alias, &normalized)))
            .ok_or(())
    } else {
        normalize_anchored_relative(&argument)
    }
}

fn normalize_anchored_relative(path: &Path) -> Result<PathBuf, ()> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => out.push(part),
            Component::ParentDir => {
                if !out.pop() {
                    return Err(());
                }
            }
            Component::Prefix(_) | Component::RootDir => return Err(()),
        }
    }
    Ok(out)
}

/// How a path component is matched when opening a child.
///
/// User-typed aliases stay case-insensitive on Windows. Names taken from a
/// directory listing must open the exact enumerated object so a different-case
/// sibling cannot be substituted on a case-sensitive volume.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NameMatch {
    /// User-typed path aliases. Windows opens with `OBJ_CASE_INSENSITIVE`.
    #[cfg(windows)]
    Alias,
    /// Directory-entry spellings and well-known ignore file names.
    Exact,
}

/// Opens one child name relative to an already retained parent handle.
fn open_child_file(
    parent: &File,
    name: &OsStr,
    expected: Option<EntryKind>,
    name_match: NameMatch,
) -> io::Result<File> {
    #[cfg(unix)]
    {
        let _ = name_match;
        open_named_unix(parent, name, expected, SearchAccess::Content)
    }
    #[cfg(windows)]
    {
        Ok(open_named_windows(parent, name, expected, name_match, SearchAccess::Content)?.file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name, expected, name_match);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative child open is not implemented on this platform",
        ))
    }
}

fn apply_open_fault(name: &OsStr) -> io::Result<()> {
    #[cfg(test)]
    if let Some(result) = current_limiter(|limiter| limiter.apply_open_fault(name)) {
        return result;
    }
    let _ = name;
    Ok(())
}

fn overridden_child_device(name: &OsStr, device: u64) -> u64 {
    #[cfg(test)]
    if let Some(over) = current_limiter(|limiter| limiter.child_device_override(name)).flatten() {
        return over;
    }
    let _ = name;
    device
}

#[cfg(test)]
fn apply_access_gate(name: &OsStr, observed: ObservedOpen) -> io::Result<()> {
    if let Some(result) = current_limiter(|limiter| limiter.apply_access_gate(name, observed)) {
        return result;
    }
    Ok(())
}

#[cfg(windows)]
fn apply_parent_discovery_hook(path: &Path) -> io::Result<Option<PathBuf>> {
    #[cfg(test)]
    if let Some(result) = current_limiter(|limiter| limiter.apply_parent_discovery_hook(path)) {
        return result;
    }
    let _ = path;
    Ok(None)
}

/// Result of opening the parent of a live directory handle.
#[derive(Debug)]
pub(super) enum ParentDirectory {
    /// Opened parent directory.
    Parent(File),
    /// `dir` is the filesystem root; there is no parent.
    FilesystemRoot,
}

/// Opens the parent directory of `dir` via handle-relative `..`.
///
/// Used only for Git-boundary discovery. Does not follow the final link
/// and does not apply search-root mount containment: walking up may
/// cross a bind mount to reach the real Git common dir. Missing parents
/// and open faults fail closed; only a proven filesystem root returns
/// [`ParentDirectory::FilesystemRoot`].
///
/// # Errors
///
/// Returns an I/O error when `..` cannot be opened as a directory, or
/// when a path-derived parent no longer contains `dir`.
pub(super) fn open_parent_directory(dir: &File) -> io::Result<ParentDirectory> {
    apply_open_fault(OsStr::new(".."))?;
    #[cfg(unix)]
    {
        use std::os::fd::{AsRawFd, FromRawFd};

        let c_dotdot = c"..";
        let flags = libc::O_RDONLY
            | libc::O_NONBLOCK
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_DIRECTORY;
        // Parent discovery must not use `RESOLVE_BENEATH`; `..` is the point.
        // SAFETY: `dir` is a live directory fd and `c_dotdot` is `..\0`.
        let descriptor = unsafe { libc::openat(dir.as_raw_fd(), c_dotdot.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `descriptor` is a fresh owned fd from `openat`.
        let parent = unsafe { File::from_raw_fd(descriptor) };
        if files_same_identity(&parent, dir)? {
            return Ok(ParentDirectory::FilesystemRoot);
        }
        Ok(ParentDirectory::Parent(parent))
    }
    #[cfg(windows)]
    {
        open_windows_parent_directory(dir)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "handle-relative parent open is not implemented on this platform",
        ))
    }
}

fn require_parent_directory(opened: ParentDirectory) -> io::Result<File> {
    match opened {
        ParentDirectory::Parent(file) => Ok(file),
        ParentDirectory::FilesystemRoot => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path escaped past filesystem root",
        )),
    }
}

/// Returns whether two opened objects share identity.
pub(super) fn files_same_identity(left: &File, right: &File) -> io::Result<bool> {
    Ok(identity_and_kind(left)?.0 == identity_and_kind(right)?.0)
}

/// Directory-entry spelling of `child` inside `parent`.
pub(super) fn child_name_in_parent(
    parent: &File,
    child: &File,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<OsString> {
    #[cfg(unix)]
    {
        let identity = identity_and_kind(child)?.0;
        unix_on_disk_component_name(parent, identity, limiter, cancel)
    }
    #[cfg(windows)]
    {
        let _ = (limiter, cancel);
        windows_child_name_in_parent(parent, child)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, child, limiter, cancel);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "child name proof is not implemented on this platform",
        ))
    }
}

/// Opens an absolute directory with no-follow component walks for Git metadata.
///
/// # Errors
///
/// Returns an I/O error when any component cannot be opened without following
/// a link, or when the path is not a directory.
pub(super) fn open_directory_nofollow(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;

        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "git metadata path is not absolute",
            ));
        }
        let c_root = c"/";
        let flags = libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_DIRECTORY;
        // SAFETY: `c_root` is a live C string; `AT_FDCWD` is the documented
        // cwd-relative starting point for the filesystem root.
        let root_fd = unsafe { libc::openat(libc::AT_FDCWD, c_root.as_ptr(), flags) };
        if root_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut current = unsafe { File::from_raw_fd(root_fd) };
        for component in path.components() {
            match component {
                Component::RootDir | Component::Prefix(_) => {}
                Component::CurDir => {}
                Component::ParentDir => {
                    current = require_parent_directory(open_parent_directory(&current)?)?;
                }
                Component::Normal(name) => {
                    current = open_named_unix(
                        &current,
                        name,
                        Some(EntryKind::Directory),
                        SearchAccess::Content,
                    )?;
                }
            }
        }
        Ok(current)
    }
    #[cfg(windows)]
    {
        let mut components = path.components();
        let Some(first) = components.next() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "git metadata path is empty",
            ));
        };
        let mut prefix = PathBuf::from(first.as_os_str());
        if let Some(Component::RootDir) = components.clone().next() {
            let _ = components.next();
            prefix.push(std::path::MAIN_SEPARATOR_STR);
        }
        let mut current = open_windows_prefix(&prefix)?.file;
        for component in components {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    current = require_parent_directory(open_parent_directory(&current)?)?;
                }
                Component::Normal(name) => {
                    current = open_named_windows(
                        &current,
                        name,
                        Some(EntryKind::Directory),
                        NameMatch::Exact,
                        SearchAccess::Content,
                    )?
                    .file;
                }
                Component::Prefix(_) | Component::RootDir => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "git metadata path has an interior prefix",
                    ));
                }
            }
        }
        Ok(current)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no-follow directory open is not implemented on this platform",
        ))
    }
}

/// Rejects empty names, `.`/`..`, NULs, and platform path separators.
///
/// Callers must pass a directory-entry spelling, never a reconstructed
/// relative path. A separator would re-walk ancestors from the parent.
pub(crate) fn validate_component_name(name: &OsStr) -> io::Result<()> {
    if name.is_empty() || name == "." || name == ".." || os_name_has_separator_or_nul(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "walked entry is not a single path component",
        ));
    }
    Ok(())
}

fn os_name_has_separator_or_nul(name: &OsStr) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        name.as_bytes()
            .iter()
            .any(|&byte| byte == 0 || byte == b'/')
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        // Colon is NTFS ADS / stream syntax (`file.txt:stream`). ADS names
        // are not directory-enumerated, so they would bypass ignore/policy
        // unless rejected before `NtOpenFile`.
        name.encode_wide().any(|unit| {
            unit == 0
                || unit == u16::from(b'/')
                || unit == u16::from(b'\\')
                || unit == u16::from(b':')
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        name.to_string_lossy()
            .chars()
            .any(|ch| ch == '\0' || ch == '/' || ch == '\\' || ch == ':')
    }
}

#[cfg(unix)]
fn open_allowed_root(path: &Path) -> io::Result<StableHandle> {
    stable_from_file(File::open(path)?)
}

#[cfg(windows)]
fn open_allowed_root(path: &Path) -> io::Result<StableHandle> {
    open_windows(path)
}

#[cfg(unix)]
fn stable_from_file(file: File) -> io::Result<StableHandle> {
    let (identity, kind) = identity_and_kind(&file)?;
    Ok(StableHandle {
        file,
        identity,
        kind,
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
        named_child: None,
    })
}

#[cfg(unix)]
fn identity_and_kind(file: &File) -> io::Result<(FileIdentity, EntryKind)> {
    use std::os::unix::fs::MetadataExt;

    #[cfg(test)]
    if current_limiter(|limiter| limiter.force_identity_error()).unwrap_or(false) {
        return Err(io::Error::other("injected file identity query failure"));
    }

    let metadata = file.metadata()?;
    let kind = if metadata.is_file() {
        EntryKind::File
    } else if metadata.is_dir() {
        EntryKind::Directory
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened object is neither a regular file nor a directory",
        ));
    };
    Ok((
        FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        kind,
    ))
}

#[cfg(unix)]
fn open_named_unix(
    parent: &File,
    name: &OsStr,
    expected: Option<EntryKind>,
    access: SearchAccess,
) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    apply_open_fault(name)?;
    validate_component_name(name)?;
    let c_name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path component contains NUL"))?;
    let mut flags = libc::O_CLOEXEC | libc::O_NOFOLLOW;
    match access {
        SearchAccess::Content => flags |= libc::O_RDONLY | libc::O_NONBLOCK,
        SearchAccess::Metadata => {
            #[cfg(any(target_os = "linux", target_os = "android"))]
            {
                flags |= libc::O_PATH;
            }
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            {
                let _ = flags;
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "metadata-only child fds require O_PATH",
                ));
            }
        }
    }
    if matches!(expected, Some(EntryKind::Directory)) {
        flags |= libc::O_DIRECTORY;
    }
    let descriptor = open_unix_descriptor(parent.as_raw_fd(), c_name.as_ptr(), flags, access)?;
    // SAFETY: the descriptor is a fresh owned fd from `openat`/`openat2`.
    let file = unsafe { File::from_raw_fd(descriptor) };
    enforce_unix_child_containment(parent, &file, expected, name)?;
    Ok(file)
}

/// Reads one named Unix child's identity without following its final link.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn unix_named_identity(parent: &File, name: &OsStr) -> io::Result<(FileIdentity, EntryKind)> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    validate_component_name(name)?;
    let c_name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path component contains NUL"))?;
    let mut stat = MaybeUninit::<libc::stat>::zeroed();
    #[cfg(test)]
    apply_access_gate(
        name,
        ObservedOpen {
            access: SearchAccess::Metadata,
            flags: 0,
        },
    )?;
    // SAFETY: `parent` is a live directory fd, `c_name` is a NUL-terminated
    // component, and `stat` is writable. The final link is not followed.
    let status = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            c_name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status != 0 {
        return Err(map_unix_open_error(io::Error::last_os_error()));
    }
    // SAFETY: successful `fstatat` initialized `stat`.
    let stat = unsafe { stat.assume_init() };
    let fmt = stat.st_mode & libc::S_IFMT;
    if fmt == libc::S_IFLNK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "symlink traversal is not permitted",
        ));
    }
    let parent_meta = parent.metadata()?;
    let child_dev = overridden_child_device(name, stat.st_dev as u64);
    if parent_meta.dev() != child_dev {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "mount traversal is not permitted",
        ));
    }
    let kind = if fmt == libc::S_IFREG {
        EntryKind::File
    } else if fmt == libc::S_IFDIR {
        EntryKind::Directory
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened object is neither a regular file nor a directory",
        ));
    };
    if kind == EntryKind::File && stat.st_nlink as u64 != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "multi-link regular files are not permitted",
        ));
    }
    let device = unix_device_identity(stat.st_dev)?;
    let inode = unix_identity_part(stat.st_ino, "inode")?;
    Ok((FileIdentity { device, inode }, kind))
}

/// Confirms `name` with no-follow metadata and mount/type/`nlink` proof.
///
/// Used on Unix platforms without `O_PATH`/`openat2`. Lookup does not open
/// the child for reading; inability to prove containment fails closed.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn confirm_named_unix_metadata(
    parent: &File,
    name: &OsStr,
    expected: Option<EntryKind>,
) -> io::Result<()> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    validate_component_name(name)?;
    let c_name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path component contains NUL"))?;
    let mut stat = MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `parent` is a live directory fd, `c_name` is a NUL-terminated
    // single component, and `stat` is writable `stat` storage. `AT_SYMLINK_NOFOLLOW`
    // prevents following the last component.
    let status = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            c_name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status != 0 {
        return Err(map_unix_open_error(io::Error::last_os_error()));
    }
    // SAFETY: `fstatat` returned 0 and initialized `stat`.
    let stat = unsafe { stat.assume_init() };
    let fmt = stat.st_mode & libc::S_IFMT;
    if fmt == libc::S_IFLNK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "symlink traversal is not permitted",
        ));
    }
    let parent_meta = parent.metadata()?;
    let child_dev = overridden_child_device(name, stat.st_dev as u64);
    if parent_meta.dev() != child_dev {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "mount traversal is not permitted",
        ));
    }
    let kind = if fmt == libc::S_IFREG {
        EntryKind::File
    } else if fmt == libc::S_IFDIR {
        EntryKind::Directory
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened object is neither a regular file nor a directory",
        ));
    };
    if let Some(expected) = expected
        && kind != expected
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "walked object type or identity changed before it was opened",
        ));
    }
    if kind == EntryKind::File && stat.st_nlink as u64 != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "multi-link regular files are not permitted",
        ));
    }
    Ok(())
}

/// Opens `name` relative to `dirfd` without following links.
///
/// Linux uses `openat2` with `RESOLVE_BENEATH | RESOLVE_NO_XDEV |
/// RESOLVE_NO_SYMLINKS`. Kernels without `openat2` fail closed rather than
/// falling back to mount-crossing `openat`. Other Unix uses `openat` and
/// relies on the post-open `st_dev` / `st_nlink` check.
#[cfg(unix)]
fn open_unix_descriptor(
    dirfd: std::os::fd::RawFd,
    c_name: *const libc::c_char,
    flags: libc::c_int,
    access: SearchAccess,
) -> io::Result<std::os::fd::RawFd> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let descriptor = openat2_beneath(dirfd, c_name, flags, access)?;
        Ok(descriptor)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        #[cfg(test)]
        {
            use std::ffi::CStr;
            use std::os::unix::ffi::OsStrExt;

            // SAFETY: the caller supplies a live NUL-terminated component.
            let name = unsafe { CStr::from_ptr(c_name) };
            apply_access_gate(
                OsStr::from_bytes(name.to_bytes()),
                ObservedOpen { access, flags },
            )?;
        }
        #[cfg(not(test))]
        let _ = access;
        // SAFETY: `dirfd` is a live directory fd, `c_name` is NUL-terminated,
        // and flags request a new owned descriptor only.
        let descriptor = unsafe { libc::openat(dirfd, c_name, flags) };
        if descriptor == -1 {
            return Err(map_unix_open_error(io::Error::last_os_error()));
        }
        Ok(descriptor)
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

/// Required Linux/Android `openat2` containment policy.
#[cfg(any(target_os = "linux", target_os = "android"))]
const OPENAT2_REQUIRED_RESOLVE: u64 = 0x01 | 0x04 | 0x08;

#[cfg(any(target_os = "linux", target_os = "android"))]
fn openat2_beneath(
    dirfd: std::os::fd::RawFd,
    c_name: *const libc::c_char,
    flags: libc::c_int,
    access: SearchAccess,
) -> io::Result<std::os::fd::RawFd> {
    let how = OpenHow {
        flags: flags as u64,
        mode: 0,
        resolve: OPENAT2_REQUIRED_RESOLVE,
    };
    #[cfg(test)]
    {
        use std::ffi::CStr;
        use std::os::unix::ffi::OsStrExt;

        // SAFETY: the caller supplies a live NUL-terminated component.
        let name = unsafe { CStr::from_ptr(c_name) };
        apply_access_gate(
            OsStr::from_bytes(name.to_bytes()),
            ObservedOpen {
                access,
                flags,
                resolve: how.resolve,
            },
        )?;
    }
    #[cfg(not(test))]
    let _ = access;
    // SAFETY: `dirfd` is a live directory fd, `c_name` is a NUL-terminated
    // component, and `how` is the documented 24-byte `open_how` layout.
    let descriptor = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd,
            c_name,
            std::ptr::addr_of!(how),
            std::mem::size_of::<OpenHow>(),
        )
    };
    if descriptor < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOSYS) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "openat2 is required to prove NO_XDEV/BENEATH containment",
            ));
        }
        return Err(map_unix_open_error(error));
    }
    Ok(descriptor as std::os::fd::RawFd)
}

#[cfg(unix)]
fn map_unix_open_error(error: io::Error) -> io::Error {
    match error.raw_os_error() {
        Some(libc::ELOOP) => io::Error::new(
            io::ErrorKind::InvalidInput,
            "symlink traversal is not permitted",
        ),
        Some(libc::EXDEV) => io::Error::new(
            io::ErrorKind::InvalidInput,
            "mount traversal is not permitted",
        ),
        _ => error,
    }
}

#[cfg(unix)]
fn enforce_unix_child_containment(
    parent: &File,
    child: &File,
    expected: Option<EntryKind>,
    name: &OsStr,
) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let parent_meta = parent.metadata()?;
    let child_meta = child.metadata()?;
    let child_dev = overridden_child_device(name, child_meta.dev());
    if parent_meta.dev() != child_dev {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "mount traversal is not permitted",
        ));
    }
    let (_, kind) = identity_and_kind(child)?;
    if let Some(expected) = expected
        && kind != expected
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "walked object type or identity changed before it was opened",
        ));
    }
    if kind == EntryKind::File && child_meta.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "multi-link regular files are not permitted",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn open_windows(path: &Path) -> io::Result<StableHandle> {
    open_windows_create(path, 0)
}

#[cfg(windows)]
fn open_windows_prefix(path: &Path) -> io::Result<StableHandle> {
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
    open_windows_create(path, FILE_FLAG_OPEN_REPARSE_POINT)
}

#[cfg(windows)]
fn open_windows_create(path: &Path, extra_flags: u32) -> io::Result<StableHandle> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let path = windows_extended_length_path(path);
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    wide.push(0);
    // SAFETY: `wide` is a live NUL-terminated UTF-16 path. Null optional
    // pointers satisfy `CreateFileW`; a successful handle is transferred
    // immediately into `File` ownership below.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | extra_flags,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is a fresh successful `CreateFileW` result and no
    // other owner will close it after this transfer.
    let file = unsafe { File::from_raw_handle(handle) };
    stable_from_windows_file(file, false, false)
}

#[cfg(windows)]
fn open_named_windows(
    parent: &File,
    name: &OsStr,
    expected: Option<EntryKind>,
    name_match: NameMatch,
    access: SearchAccess,
) -> io::Result<StableHandle> {
    apply_open_fault(name)?;
    validate_component_name(name)?;
    open_windows_component(parent, name, expected, name_match, access)
}

#[cfg(windows)]
fn open_windows_component(
    parent: &File,
    name: &OsStr,
    expected: Option<EntryKind>,
    name_match: NameMatch,
    access: SearchAccess,
) -> io::Result<StableHandle> {
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::ptr::null;
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT,
        FILE_SYNCHRONOUS_IO_NONALERT, NtOpenFile,
    };
    use windows_sys::Win32::Foundation::{
        INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_GENERIC_READ, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    let mut wide: Vec<u16> = name.encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path component contains NUL",
        ));
    }
    let byte_len = wide
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path component is too long"))?;
    let object_name = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: wide.as_mut_ptr(),
    };
    let object_attributes = OBJECT_ATTRIBUTES {
        Length: u32::try_from(size_of::<OBJECT_ATTRIBUTES>())
            .expect("OBJECT_ATTRIBUTES size must fit in u32"),
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &object_name,
        Attributes: match name_match {
            NameMatch::Alias => OBJ_CASE_INSENSITIVE,
            NameMatch::Exact => 0,
        },
        SecurityDescriptor: null(),
        SecurityQualityOfService: null(),
    };
    let mut options = FILE_OPEN_REPARSE_POINT;
    match expected {
        Some(EntryKind::File) => options |= FILE_NON_DIRECTORY_FILE,
        Some(EntryKind::Directory) => options |= FILE_DIRECTORY_FILE,
        None => {}
    }
    let mut handle = INVALID_HANDLE_VALUE;
    let mut io_status = IO_STATUS_BLOCK::default();
    // `FILE_SYNCHRONOUS_IO_NONALERT` requires `SYNCHRONIZE`. Metadata opens
    // omit both so a `FILE_READ_ATTRIBUTES`-only ACL still confirms, and the
    // resulting asynchronous handle is never used for content reads.
    let desired_access = match access {
        SearchAccess::Content => {
            options |= FILE_SYNCHRONOUS_IO_NONALERT;
            FILE_GENERIC_READ
        }
        SearchAccess::Metadata => FILE_READ_ATTRIBUTES,
    };
    #[cfg(test)]
    apply_access_gate(
        name,
        ObservedOpen {
            access,
            desired_access,
            options,
        },
    )?;
    // SAFETY: `parent` stays live; `object_name` references `wide` for this
    // call; all output pointers reference initialized writable storage.
    let status = unsafe {
        NtOpenFile(
            &mut handle,
            desired_access,
            &object_attributes,
            &mut io_status,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            options,
        )
    };
    if status < 0 {
        // `NtOpenFile` returns an NTSTATUS and does not define `GetLastError`.
        // SAFETY: converting that returned status is the documented use of
        // `RtlNtStatusToDosError` and has no pointer preconditions.
        let code = unsafe { RtlNtStatusToDosError(status) };
        let code = i32::try_from(code)
            .map_err(|_| io::Error::other(format!("unrepresentable Windows error code: {code}")))?;
        return Err(io::Error::from_raw_os_error(code));
    }
    // SAFETY: successful `NtOpenFile` returned a fresh owned handle and no
    // other owner will close it after this transfer.
    let file = unsafe { File::from_raw_handle(handle) };
    let opened = stable_from_windows_file(file, true, access == SearchAccess::Metadata)?;
    enforce_windows_child_containment(parent, &opened, name)?;
    Ok(opened)
}

#[cfg(windows)]
fn stable_from_windows_file(
    file: File,
    reject_reparse: bool,
    metadata_only: bool,
) -> io::Result<StableHandle> {
    let (identity, kind, reparse, links) = windows_identity_kind_reparse(&file)?;
    if reject_reparse && reparse {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reparse-point traversal is not permitted",
        ));
    }
    if kind == EntryKind::File && links != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "multi-link regular files are not permitted",
        ));
    }
    let final_path = final_path_by_handle(&file)?;
    Ok(StableHandle {
        file,
        identity,
        kind,
        final_path,
        metadata_only,
    })
}

#[cfg(windows)]
fn enforce_windows_child_containment(
    parent: &File,
    child: &StableHandle,
    name: &OsStr,
) -> io::Result<()> {
    let (parent_identity, parent_kind, _, _) = windows_identity_kind_reparse(parent)?;
    if parent_kind != EntryKind::Directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "walk parent is no longer a directory",
        ));
    }
    let child_volume = overridden_child_device(name, child.identity.volume);
    if parent_identity.volume != child_volume {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "mount traversal is not permitted",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn identity_and_kind(file: &File) -> io::Result<(FileIdentity, EntryKind)> {
    let (identity, kind, _, _) = windows_identity_kind_reparse(file)?;
    Ok((identity, kind))
}

#[cfg(windows)]
fn windows_file_is_hidden(file: &File) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_HIDDEN, GetFileInformationByHandle,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is borrowed from a live `File` and `information`
    // points to initialized writable storage of the documented type.
    let success = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(information.dwFileAttributes & FILE_ATTRIBUTE_HIDDEN != 0)
}

#[cfg(windows)]
fn windows_identity_kind_reparse(file: &File) -> io::Result<(FileIdentity, EntryKind, bool, u32)> {
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    };

    #[cfg(test)]
    if current_limiter(|limiter| limiter.force_identity_error()).unwrap_or(false) {
        return Err(io::Error::other("injected file identity query failure"));
    }

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is borrowed from a live `File` and `information`
    // points to initialized writable storage of the documented type.
    let success = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    let kind = if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        EntryKind::Directory
    } else {
        EntryKind::File
    };
    let mut id_info = FILE_ID_INFO::default();
    let id_size = u32::try_from(size_of::<FILE_ID_INFO>()).expect("FILE_ID_INFO fits in u32");
    // SAFETY: `file` is live; `id_info` is writable `FILE_ID_INFO` storage.
    // ReFS uniqueness requires the 128-bit `FileId`; the 64-bit
    // `BY_HANDLE_FILE_INFORMATION` index is not used as identity.
    let id_ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&raw mut id_info).cast(),
            id_size,
        )
    };
    if id_ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        FileIdentity {
            volume: id_info.VolumeSerialNumber,
            file_id: id_info.FileId.Identifier,
        },
        kind,
        information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        information.nNumberOfLinks,
    ))
}

#[cfg(windows)]
fn final_path_by_handle(file: &File) -> io::Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use std::ptr::null_mut;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };

    let flags = FILE_NAME_NORMALIZED | VOLUME_NAME_DOS;
    // SAFETY: the handle is borrowed from a live `File`; a null zero-sized
    // output buffer is the documented size-query form.
    let needed = unsafe { GetFinalPathNameByHandleW(file.as_raw_handle(), null_mut(), 0, flags) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u16; needed as usize + 1];
    loop {
        let capacity = u32::try_from(buffer.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "final path is too long"))?;
        // SAFETY: `buffer` is live writable UTF-16 storage with the exact
        // capacity passed to the API, and the file handle remains valid.
        let written = unsafe {
            GetFinalPathNameByHandleW(file.as_raw_handle(), buffer.as_mut_ptr(), capacity, flags)
        };
        if written == 0 {
            return Err(io::Error::last_os_error());
        }
        if written < capacity {
            buffer.truncate(written as usize);
            let path = PathBuf::from(OsString::from_wide(&buffer));
            return Ok(strip_verbatim_prefix(&path));
        }
        buffer.resize(written as usize + 1, 0);
    }
}

/// Returns a forward-slash relative path, or the full path for a root file.
pub(crate) fn rel_posix(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    if relative.as_os_str().is_empty() {
        to_posix(path)
    } else {
        to_posix(relative)
    }
}

/// Lossy display of one path component, matching frontier and top-N keys.
pub(crate) fn lossy_component(name: &OsStr) -> std::borrow::Cow<'_, str> {
    name.to_string_lossy()
}

/// Shared path order: lossy rendering first, original `OsString` for ties.
///
/// Directory listings, the walk frontier, and grep/find top-N heaps share
/// this key so two names that render identically stay deterministic.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct PathOrderKey {
    rendered: String,
    raw: OsString,
}

impl PathOrderKey {
    /// Builds a key from a relative or absolute path.
    pub(crate) fn from_path(path: &Path) -> Self {
        Self {
            rendered: to_posix(path),
            raw: path.as_os_str().to_os_string(),
        }
    }

    /// Builds a key when the display spelling is already known.
    pub(crate) fn from_rendered_and_raw(rendered: String, raw: impl Into<OsString>) -> Self {
        Self {
            rendered,
            raw: raw.into(),
        }
    }

    /// Lossy `/`-separated spelling used in reports.
    pub(crate) fn rendered(&self) -> &str {
        &self.rendered
    }

    /// Heap bytes charged for this key: rendered text plus the raw OS name.
    pub(crate) fn store_bytes(&self) -> usize {
        self.rendered
            .len()
            .saturating_add(self.raw.as_encoded_bytes().len())
    }
}

/// Renders a path with `/` separators on every platform.
///
/// Each component uses [`lossy_component`] so listing sort, frontier peek,
/// and reported top-N keys stay on one order.
pub(crate) fn to_posix(path: &Path) -> String {
    let path = strip_verbatim_prefix(path);
    let mut out = String::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                out.push_str(&lossy_component(prefix.as_os_str()).replace('\\', "/"));
            }
            Component::RootDir => {
                if out.is_empty() {
                    out.push('/');
                }
            }
            other => {
                if !out.is_empty() && !out.ends_with('/') {
                    out.push('/');
                }
                out.push_str(&lossy_component(other.as_os_str()));
            }
        }
    }
    out
}

/// Truncates bytes for display without splitting a UTF-8 character.
pub(crate) fn display_line(bytes: &[u8], cap: usize, truncated: &mut bool) -> String {
    let mut end = bytes.len().min(cap);
    while end > 0 && end < bytes.len() && (bytes[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    if end < bytes.len() {
        *truncated = true;
    }
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Runs deadline-bounded search work on a dedicated OS thread.
///
/// Startup first proves a usable interrupt authority. A supervisor owns the
/// actual worker join and platform authority for the complete lifetime.
/// Cancellation, timeout, or future drop publishes the worker token; Unix
/// pollable reads wake through a per-worker socket, while `SIGURG` and Windows
/// `CancelSynchronousIo` cover syscalls already in the kernel. The supervisor
/// uniquely joins the worker before releasing platform resources. A native
/// in-process component that replaces `SIGURG` during an active call can
/// defeat interruption of a non-pollable Unix syscall; uninterruptible kernel
/// waits can also delay the supervisor.
pub(crate) async fn run_blocking<F, T>(
    label: &str,
    cancel: &CancellationToken,
    time_limit: Duration,
    function: F,
) -> Result<T, ToolError>
where
    F: FnOnce(CancellationToken) -> Result<T, ToolError> + Send + 'static,
    T: Send + 'static,
{
    run_blocking_until(label, cancel, Instant::now() + time_limit, function).await
}

/// [`run_blocking`] driven by a shared absolute deadline.
pub(crate) async fn run_blocking_until<F, T>(
    label: &str,
    cancel: &CancellationToken,
    deadline: Instant,
    function: F,
) -> Result<T, ToolError>
where
    F: FnOnce(CancellationToken) -> Result<T, ToolError> + Send + 'static,
    T: Send + 'static,
{
    run_blocking_started(
        label,
        cancel,
        Some(deadline),
        WorkerStart::default(),
        function,
    )
    .await
}

/// Runs blocking work under a supervisor without an internal deadline.
///
/// Cancellation or future drop publishes the worker token. The detached
/// supervisor retains the unique worker join and interrupt authority.
///
/// # Errors
///
/// Returns worker, cancellation, startup, or interrupt-authority failures.
pub(crate) async fn run_blocking_supervised<F, T>(
    label: &str,
    cancel: &CancellationToken,
    function: F,
) -> Result<T, ToolError>
where
    F: FnOnce(CancellationToken) -> Result<T, ToolError> + Send + 'static,
    T: Send + 'static,
{
    run_blocking_started(label, cancel, None, WorkerStart::default(), function).await
}

/// Same as [`run_blocking`], with a per-call test hook that can block I/O.
///
/// # Errors
///
/// Same as [`run_blocking`].
#[cfg(test)]
pub(crate) async fn run_blocking_with_io_block<F, T>(
    label: &str,
    cancel: &CancellationToken,
    time_limit: Duration,
    block: BlockWorkerHook,
    function: F,
) -> Result<T, ToolError>
where
    F: FnOnce(CancellationToken) -> Result<T, ToolError> + Send + 'static,
    T: Send + 'static,
{
    run_blocking_started(
        label,
        cancel,
        Some(Instant::now() + time_limit),
        WorkerStart {
            block: Some(block),
            ..WorkerStart::default()
        },
        function,
    )
    .await
}

async fn run_blocking_started<F, T>(
    label: &str,
    cancel: &CancellationToken,
    deadline: Option<Instant>,
    start: WorkerStart,
    function: F,
) -> Result<T, ToolError>
where
    F: FnOnce(CancellationToken) -> Result<T, ToolError> + Send + 'static,
    T: Send + 'static,
{
    if cancel.is_cancelled() {
        return Err(ToolError::Execution(format!(
            "{label} cancelled before completion"
        )));
    }
    let worker_cancel = cancel.child_token();
    let function_cancel = worker_cancel.clone();
    let force_interrupt_setup_failure = start.force_interrupt_setup_failure();
    #[cfg(test)]
    let live_workers = start.live_workers.clone();
    #[cfg(test)]
    let live_handles = start.live_handles.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let worker = InterruptibleWorker::spawn(
        move || {
            start.apply(&function_cancel);
            if function_cancel.is_cancelled() {
                return Err(ToolError::Execution(
                    "cancelled before completion".to_owned(),
                ));
            }
            function(function_cancel)
        },
        tx,
        worker_cancel.clone(),
        force_interrupt_setup_failure,
        #[cfg(test)]
        live_workers,
        #[cfg(test)]
        live_handles,
    )?;
    let deadline_wait = async {
        match deadline {
            Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(deadline_wait);
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            worker_cancel.cancel();
            let authority_error = worker.join().await;
            let suffix = authority_error
                .map(|error| format!("; interrupt error: {error}"))
                .unwrap_or_default();
            Err(ToolError::Execution(format!(
                "{label} cancelled before completion{suffix}"
            )))
        },
        _ = &mut deadline_wait => {
            worker_cancel.cancel();
            let authority_error = worker.join().await;
            let suffix = authority_error
                .map(|error| format!("; interrupt error: {error}"))
                .unwrap_or_default();
            Err(ToolError::Execution(format!("{label} time limit reached{suffix}")))
        },
        joined = rx => {
            let authority_error = worker.join().await;
            // A worker result and cancellation/deadline can become ready in
            // the same scheduler turn. Revalidate terminal state after the
            // unique join so a late-selected result never publishes partial
            // output after an already-observed stop condition.
            if cancel.is_cancelled() {
                let suffix = authority_error
                    .map(|error| format!("; interrupt error: {error}"))
                    .unwrap_or_default();
                return Err(ToolError::Execution(format!(
                    "{label} cancelled before completion{suffix}"
                )));
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                let suffix = authority_error
                    .map(|error| format!("; interrupt error: {error}"))
                    .unwrap_or_default();
                return Err(ToolError::Execution(format!(
                    "{label} time limit reached{suffix}"
                )));
            }
            if let Some(error) = authority_error {
                return Err(ToolError::Execution(format!(
                    "{label} interrupt authority failed: {error}"
                )));
            }
            match joined {
                Ok(inner) => inner,
                Err(_) => Err(ToolError::Execution(format!(
                    "{label} worker dropped its result"
                ))),
            }
        },
    }
}

/// Resolves a grep/find target on a cancellable worker thread.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] when the worker is cancelled or exceeds
/// the search time limit.
pub async fn prepare_search_async(
    cwd: std::path::PathBuf,
    path_arg: Option<String>,
    cancel: CancellationToken,
) -> Result<PreparedSearch, ToolError> {
    prepare_search_async_with_access(cwd, path_arg, cancel, SearchAccess::Content).await
}

/// [`prepare_search_async`] with an explicit content/metadata capability.
///
/// # Errors
///
/// Same as [`prepare_search_async`].
pub async fn prepare_search_async_with_access(
    cwd: std::path::PathBuf,
    path_arg: Option<String>,
    cancel: CancellationToken,
    access: SearchAccess,
) -> Result<PreparedSearch, ToolError> {
    run_blocking(
        "search dispatch preflight",
        &cancel,
        SEARCH_TIME_LIMIT,
        move |worker_cancel| {
            prepare_search_with_access(&cwd, path_arg.as_deref(), &worker_cancel, access)
        },
    )
    .await
}

/// Same as [`prepare_search_async`], with a per-call blocking I/O hook.
///
/// # Errors
///
/// Same as [`prepare_search_async`].
#[cfg(test)]
pub(crate) async fn prepare_search_async_with_io_block(
    cwd: std::path::PathBuf,
    path_arg: Option<String>,
    cancel: CancellationToken,
    block: BlockWorkerHook,
) -> Result<PreparedSearch, ToolError> {
    run_blocking_with_io_block(
        "search dispatch preflight",
        &cancel,
        SEARCH_TIME_LIMIT,
        block,
        move |worker_cancel| prepare_search(&cwd, path_arg.as_deref(), &worker_cancel),
    )
    .await
}

static LIVE_SEARCH_WORKERS: AtomicU64 = AtomicU64::new(0);

#[cfg(windows)]
static LIVE_THREAD_HANDLES: AtomicU64 = AtomicU64::new(0);

/// Runs a search worker that blocks until `cancel` is published.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] when cancelled or timed out.
#[doc(hidden)]
pub async fn run_search_worker_until_cancel(cancel: CancellationToken) -> Result<(), ToolError> {
    run_blocking("search", &cancel, SEARCH_TIME_LIMIT, move |token| {
        while !token.is_cancelled() {
            std::thread::sleep(INTERRUPT_RETRY);
        }
        Err(ToolError::Execution(
            "search cancelled before completion".to_owned(),
        ))
    })
    .await
}

/// Live search worker threads. Integration tests assert this returns to zero.
pub fn live_search_workers() -> u64 {
    LIVE_SEARCH_WORKERS.load(Ordering::Acquire)
}

/// Live duplicated Windows worker thread handles.
pub fn live_search_thread_handles() -> u64 {
    #[cfg(windows)]
    {
        LIVE_THREAD_HANDLES.load(Ordering::Acquire)
    }
    #[cfg(not(windows))]
    {
        0
    }
}

#[cfg(test)]
type BlockWorkerHook = Arc<dyn Fn(&CancellationToken) + Send + Sync>;

/// Optional work to run on the worker thread before the real function.
#[derive(Default)]
struct WorkerStart {
    #[cfg(test)]
    block: Option<BlockWorkerHook>,
    #[cfg(test)]
    force_interrupt_setup_failure: bool,
    #[cfg(test)]
    live_workers: Option<Arc<AtomicU64>>,
    #[cfg(test)]
    live_handles: Option<Arc<AtomicU64>>,
}

impl WorkerStart {
    fn apply(&self, cancel: &CancellationToken) {
        #[cfg(test)]
        if let Some(hook) = &self.block {
            hook(cancel);
        }
        #[cfg(not(test))]
        let _ = cancel;
    }

    fn force_interrupt_setup_failure(&self) -> bool {
        #[cfg(test)]
        {
            self.force_interrupt_setup_failure
        }
        #[cfg(not(test))]
        {
            false
        }
    }
}

#[cfg(windows)]
struct SendHandle {
    handle: windows_sys::Win32::Foundation::HANDLE,
    live: Option<Arc<AtomicU64>>,
}

// SAFETY: the value is a uniquely owned kernel handle integer. Only the
// supervisor thread uses it, and `Drop` closes it exactly once.
#[cfg(windows)]
unsafe impl Send for SendHandle {}

#[cfg(windows)]
impl Drop for SendHandle {
    fn drop(&mut self) {
        // SAFETY: this is the owned duplicate returned by `DuplicateHandle`.
        let closed = unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
        debug_assert_ne!(closed, 0, "owned worker thread handle must close");
        LIVE_THREAD_HANDLES.fetch_sub(1, Ordering::AcqRel);
        if let Some(live) = &self.live {
            live.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

#[cfg(unix)]
std::thread_local! {
    static WORKER_WAKE_FD: std::cell::Cell<std::os::fd::RawFd> = const {
        std::cell::Cell::new(-1)
    };
}

#[cfg(unix)]
struct WorkerWake {
    reader: std::os::unix::net::UnixStream,
}

#[cfg(windows)]
struct WorkerWake;

#[cfg(not(any(unix, windows)))]
struct WorkerWake;

#[cfg(unix)]
struct WorkerWakeGuard {
    previous: std::os::fd::RawFd,
}

#[cfg(not(unix))]
struct WorkerWakeGuard;

#[cfg(unix)]
impl Drop for WorkerWakeGuard {
    fn drop(&mut self) {
        WORKER_WAKE_FD.with(|slot| slot.set(self.previous));
    }
}

impl WorkerWake {
    #[cfg(unix)]
    fn enter(&self) -> WorkerWakeGuard {
        use std::os::fd::AsRawFd;

        let previous = WORKER_WAKE_FD.with(|slot| slot.replace(self.reader.as_raw_fd()));
        WorkerWakeGuard { previous }
    }

    #[cfg(not(unix))]
    fn enter(&self) -> WorkerWakeGuard {
        WorkerWakeGuard
    }
}

/// Waits until a file descriptor is readable or the current worker is woken.
///
/// Outside a supervised Unix worker there is no wake descriptor, so the
/// caller proceeds directly. Windows uses `CancelSynchronousIo` instead.
pub(super) fn wait_for_worker_readable(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;

        let wake = WORKER_WAKE_FD.with(std::cell::Cell::get);
        if wake < 0 {
            return Ok(());
        }
        let mut descriptors = [
            libc::pollfd {
                fd: file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            // SAFETY: `descriptors` contains two initialized pollfd values for
            // live descriptors owned by this worker. The call mutates only
            // their `revents` fields.
            let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, -1) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            let wake_events = descriptors[1].revents;
            if wake_events != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "search worker was cancelled",
                ));
            }
            let file_events = descriptors[0].revents;
            if file_events & libc::POLLNVAL != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "search file descriptor became invalid",
                ));
            }
            if file_events != 0 {
                return Ok(());
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(())
    }
}

#[cfg(unix)]
struct InterruptAuthority {
    pthread: libc::pthread_t,
    wake: std::os::unix::net::UnixStream,
}

#[cfg(windows)]
struct InterruptAuthority {
    thread: SendHandle,
}

#[cfg(not(any(unix, windows)))]
struct InterruptAuthority;

impl InterruptAuthority {
    fn establish(
        force_failure: bool,
        #[cfg(windows)] live_handles: Option<Arc<AtomicU64>>,
    ) -> io::Result<(Self, WorkerWake)> {
        #[cfg(unix)]
        {
            if force_failure {
                return Err(io::Error::other("injected signal authority failure"));
            }
            unblock_interrupt_signal()?;
            let (reader, writer) = std::os::unix::net::UnixStream::pair()?;
            reader.set_nonblocking(true)?;
            writer.set_nonblocking(true)?;
            // SAFETY: called on the worker; its `pthread_t` stays valid until
            // the supervisor uniquely joins that worker.
            Ok((
                Self {
                    pthread: unsafe { libc::pthread_self() },
                    wake: writer,
                },
                WorkerWake { reader },
            ))
        }
        #[cfg(windows)]
        {
            Ok((
                Self {
                    thread: duplicate_current_thread(force_failure, live_handles)?,
                },
                WorkerWake,
            ))
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "search workers require an interrupt authority",
            ))
        }
    }

    fn interrupt(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::io::Write;

            // A per-worker socket wakes pollable reads without relying on the
            // process-global signal disposition. Repeated nonblocking writes
            // may fill the socket; that still means a wake byte is pending.
            let mut wake = &self.wake;
            match wake.write(&[1]) {
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::BrokenPipe
                    ) => {}
                Err(error) => return Err(error),
            }

            let replaced = current_sigurg_handler()? != our_sigurg_handler();
            // Never invoke a foreign process-global handler. The socket wake
            // above remains authoritative for pollable read waits.
            if replaced {
                return Err(io::Error::other(
                    "SIGURG handler was replaced during an active search",
                ));
            }
            // SIGURG remains a best-effort wakeup for non-pollable filesystem
            // syscalls while this crate still owns the disposition.
            // SAFETY: the supervisor owns the worker join handle, so this
            // published `pthread_t` cannot be reclaimed or reused yet.
            let status = unsafe { libc::pthread_kill(self.pthread, libc::SIGURG) };
            if status == 0 || status == libc::ESRCH {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(status))
            }
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::{ERROR_INVALID_HANDLE, ERROR_NOT_FOUND};
            // SAFETY: `thread` is the live owned duplicate for the worker.
            let cancelled =
                unsafe { windows_sys::Win32::System::IO::CancelSynchronousIo(self.thread.handle) };
            if cancelled != 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            let code = error.raw_os_error().map(|value| value as u32);
            if matches!(code, Some(ERROR_NOT_FOUND | ERROR_INVALID_HANDLE)) {
                return Ok(());
            }
            Err(error)
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "search workers require an interrupt authority",
            ))
        }
    }
}

/// Cancellation-safe owner for one worker supervisor.
///
/// The supervisor owns the actual worker join handle and platform interrupt
/// authority from startup. Dropping this value publishes cancellation and
/// detaches only the supervisor; that supervisor continues interrupting and
/// uniquely joins the actual worker before releasing platform handles.
struct InterruptibleWorker {
    cancel: CancellationToken,
    supervisor: Option<std::thread::JoinHandle<()>>,
    supervisor_error: Arc<Mutex<Option<String>>>,
}

impl InterruptibleWorker {
    fn spawn<T: Send + 'static>(
        work: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
        tx: tokio::sync::oneshot::Sender<Result<T, ToolError>>,
        cancel: CancellationToken,
        force_interrupt_setup_failure: bool,
        #[cfg(test)] live_workers: Option<Arc<AtomicU64>>,
        #[cfg(test)] live_handles: Option<Arc<AtomicU64>>,
    ) -> Result<Self, ToolError> {
        let (startup_tx, startup_rx) = std::sync::mpsc::sync_channel(1);
        let (go_tx, go_rx) = std::sync::mpsc::sync_channel(1);
        let supervisor_cancel = cancel.clone();
        let supervisor_error = Arc::new(Mutex::new(None));
        let error_slot = Arc::clone(&supervisor_error);
        #[cfg(test)]
        let worker_live_workers = live_workers.clone();
        #[cfg(all(test, windows))]
        let worker_live_handles = live_handles;
        #[cfg(all(test, not(windows)))]
        let _ = live_handles;
        let supervisor = std::thread::Builder::new()
            .name("mcode-search-supervisor".to_owned())
            .spawn(move || {
                #[cfg(unix)]
                let _signal = match acquire_interrupt_signal() {
                    Ok(guard) => guard,
                    Err(error) => {
                        let _ = startup_tx.send(Err(error));
                        return;
                    }
                };

                let (authority_tx, authority_rx) = std::sync::mpsc::sync_channel(1);
                let worker = std::thread::Builder::new()
                    .name("mcode-search-worker".to_owned())
                    .spawn(move || {
                        struct LiveGuard {
                            #[cfg(test)]
                            local: Option<Arc<AtomicU64>>,
                        }
                        impl Drop for LiveGuard {
                            fn drop(&mut self) {
                                LIVE_SEARCH_WORKERS.fetch_sub(1, Ordering::AcqRel);
                                #[cfg(test)]
                                if let Some(local) = &self.local {
                                    local.fetch_sub(1, Ordering::AcqRel);
                                }
                            }
                        }
                        LIVE_SEARCH_WORKERS.fetch_add(1, Ordering::AcqRel);
                        #[cfg(test)]
                        if let Some(local) = &worker_live_workers {
                            local.fetch_add(1, Ordering::AcqRel);
                        }
                        let _live = LiveGuard {
                            #[cfg(test)]
                            local: worker_live_workers,
                        };

                        #[cfg(windows)]
                        let local_handles = {
                            #[cfg(test)]
                            {
                                worker_live_handles
                            }
                            #[cfg(not(test))]
                            {
                                None
                            }
                        };
                        let (authority, wake) = match InterruptAuthority::establish(
                            force_interrupt_setup_failure,
                            #[cfg(windows)]
                            local_handles,
                        ) {
                            Ok(established) => established,
                            Err(error) => {
                                let _ = authority_tx.send(Err(error));
                                return;
                            }
                        };
                        if authority_tx.send(Ok(authority)).is_err() {
                            return;
                        }
                        if go_rx.recv().is_err() {
                            return;
                        }
                        let _wake = wake.enter();
                        let result = work();
                        let _ = tx.send(result);
                    });
                let worker = match worker {
                    Ok(worker) => worker,
                    Err(error) => {
                        let _ = startup_tx.send(Err(error));
                        return;
                    }
                };
                let authority = match authority_rx.recv() {
                    Ok(Ok(authority)) => authority,
                    Ok(Err(error)) => {
                        let _ = startup_tx.send(Err(error));
                        let _ = worker.join();
                        return;
                    }
                    Err(_) => {
                        let _ = startup_tx.send(Err(io::Error::other(
                            "worker dropped interrupt startup handshake",
                        )));
                        let _ = worker.join();
                        return;
                    }
                };
                if startup_tx.send(Ok(())).is_err() {
                    let _ = worker.join();
                    return;
                }

                while !worker.is_finished() {
                    if supervisor_cancel.is_cancelled()
                        && let Err(error) = authority.interrupt()
                    {
                        let mut slot = error_slot
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        if slot.is_none() {
                            *slot = Some(error.to_string());
                        }
                    }
                    std::thread::sleep(INTERRUPT_RETRY);
                }
                let _ = worker.join();
            })
            .map_err(|error| {
                ToolError::Execution(format!("search supervisor cannot start: {error}"))
            })?;

        match startup_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                drop(go_tx);
                let _ = supervisor.join();
                return Err(ToolError::Execution(format!(
                    "search interrupt authority cannot be established: {error}"
                )));
            }
            Err(_) => {
                drop(go_tx);
                let _ = supervisor.join();
                return Err(ToolError::Execution(
                    "search interrupt startup handshake was dropped".to_owned(),
                ));
            }
        }
        go_tx.send(()).map_err(|_| {
            ToolError::Execution("search worker dropped its startup gate".to_owned())
        })?;
        Ok(Self {
            cancel,
            supervisor: Some(supervisor),
            supervisor_error,
        })
    }

    async fn join(mut self) -> Option<String> {
        let supervisor = self.supervisor.take()?;
        let _ = tokio::task::spawn_blocking(move || {
            let _ = supervisor.join();
        })
        .await;
        self.supervisor_error
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

impl Drop for InterruptibleWorker {
    fn drop(&mut self) {
        self.cancel.cancel();
        // Dropping the supervisor join handle detaches only the supervisor.
        // It still owns and joins the actual worker and interrupt authority.
        let _ = self.supervisor.take();
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct SignalGuard;

#[cfg(unix)]
struct SignalState {
    refs: usize,
    previous: libc::sigaction,
}

#[cfg(unix)]
fn signal_state() -> &'static Mutex<Option<SignalState>> {
    static STATE: Mutex<Option<SignalState>> = Mutex::new(None);
    &STATE
}

#[cfg(unix)]
fn our_sigurg_handler() -> usize {
    interrupt_signal_handler as *const () as usize
}

#[cfg(unix)]
fn current_sigurg_handler() -> io::Result<usize> {
    // SAFETY: querying the current action with a null new-action pointer.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGURG, std::ptr::null(), &mut current) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(current.sa_sigaction)
    }
}

#[cfg(unix)]
fn handler_is_default(handler: usize) -> bool {
    handler == libc::SIG_DFL
}

/// Installs an owned `SIGURG` handler, or reuses the one this crate owns.
///
/// Fails closed when another component already owns `SIGURG`. The last guard
/// restores the previous disposition only while the current handler is still
/// this crate's. Cancellation uses [`CancellationToken`]; a per-worker socket
/// wakes pollable reads, while `SIGURG` is a best-effort wake for other Unix
/// syscalls only while this crate still owns the disposition.
///
/// # Errors
///
/// Returns an I/O error when `sigaction` fails or a foreign handler is
/// installed.
#[cfg(unix)]
fn acquire_interrupt_signal() -> io::Result<SignalGuard> {
    let mut slot = signal_state()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    match slot.as_mut() {
        Some(state) => {
            let current = current_sigurg_handler()?;
            if current != our_sigurg_handler() {
                return Err(io::Error::other(
                    "SIGURG handler was replaced; search interrupt cannot be established",
                ));
            }
            state.refs = state.refs.saturating_add(1);
            Ok(SignalGuard)
        }
        None => {
            let current = current_sigurg_handler()?;
            if current != our_sigurg_handler() && !handler_is_default(current) {
                return Err(io::Error::other(
                    "SIGURG is owned by another handler; search interrupt cannot be established",
                ));
            }
            let previous = install_our_sigurg()?;
            *slot = Some(SignalState { refs: 1, previous });
            Ok(SignalGuard)
        }
    }
}

#[cfg(unix)]
fn install_our_sigurg() -> io::Result<libc::sigaction> {
    // SAFETY: the action has the documented layout, uses an empty handler,
    // and omits SA_RESTART so restartable blocking calls return EINTR.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = our_sigurg_handler();
        if libc::sigemptyset(&mut action.sa_mask) != 0 {
            return Err(io::Error::last_os_error());
        }
        action.sa_flags = 0;
        let mut previous: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGURG, &action, &mut previous) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(previous)
    }
}

#[cfg(unix)]
impl Drop for SignalGuard {
    fn drop(&mut self) {
        let mut slot = signal_state()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(state) = slot.as_mut() else {
            return;
        };
        state.refs = state.refs.saturating_sub(1);
        if state.refs != 0 {
            return;
        }
        let previous = state.previous;
        *slot = None;
        let Ok(current) = current_sigurg_handler() else {
            return;
        };
        if current != our_sigurg_handler() {
            return;
        }
        // SAFETY: restore only while the current handler is still ours.
        unsafe {
            libc::sigaction(libc::SIGURG, &previous, std::ptr::null_mut());
        }
    }
}

#[cfg(unix)]
fn unblock_interrupt_signal() -> io::Result<()> {
    // SAFETY: `set` is initialized by `sigemptyset`/`sigaddset`; passing null
    // for the old set is documented. pthread APIs return an errno value.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        if libc::sigemptyset(&mut set) != 0 || libc::sigaddset(&mut set, libc::SIGURG) != 0 {
            return Err(io::Error::last_os_error());
        }
        let status = libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status));
        }
    }
    Ok(())
}

#[cfg(unix)]
extern "C" fn interrupt_signal_handler(_signal: libc::c_int) {}

#[cfg(windows)]
fn duplicate_current_thread(
    force_failure: bool,
    live: Option<Arc<AtomicU64>>,
) -> io::Result<SendHandle> {
    use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, HANDLE};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread};

    if force_failure {
        return Err(io::Error::other("injected DuplicateHandle failure"));
    }

    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: duplicating the calling thread pseudo-handle into this process
    // yields a fresh owned handle on success.
    let ok = unsafe {
        windows_sys::Win32::Foundation::DuplicateHandle(
            GetCurrentProcess(),
            GetCurrentThread(),
            GetCurrentProcess(),
            &mut handle,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    LIVE_THREAD_HANDLES.fetch_add(1, Ordering::AcqRel);
    if let Some(live) = &live {
        live.fetch_add(1, Ordering::AcqRel);
    }
    Ok(SendHandle { handle, live })
}

/// On-disk 8.3 path for a Windows test fixture.
///
/// # Errors
///
/// Returns an I/O error when `GetShortPathNameW` fails or the path contains NUL.
#[cfg(all(test, windows))]
pub(crate) fn windows_short_path(path: &Path) -> io::Result<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::ptr::null_mut;
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    wide.push(0);
    // SAFETY: `wide` is a live NUL-terminated UTF-16 path. A null
    // zero-sized output buffer is the documented size-query form.
    let needed = unsafe { GetShortPathNameW(wide.as_ptr(), null_mut(), 0) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u16; needed as usize];
    // SAFETY: `buffer` is writable UTF-16 storage with capacity `needed`.
    let written = unsafe { GetShortPathNameW(wide.as_ptr(), buffer.as_mut_ptr(), needed) };
    if written == 0 {
        return Err(io::Error::last_os_error());
    }
    if written >= needed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short path did not fit the queried buffer",
        ));
    }
    buffer.truncate(written as usize);
    Ok(PathBuf::from(OsString::from_wide(&buffer)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static PROCESS_CWD_LOCK: Mutex<()> = Mutex::new(());

    fn lock_process_cwd() -> MutexGuard<'static, ()> {
        PROCESS_CWD_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn signed_device_identity_preserves_kernel_bits() {
        let device: libc::dev_t = -1;
        assert_eq!(unix_device_identity(device).unwrap(), device as u64);
    }

    #[test]
    fn component_names_reject_separators_and_dots() {
        assert!(validate_component_name(OsStr::new("old_only.txt")).is_ok());
        assert!(validate_component_name(OsStr::new("nested/old_only.txt")).is_err());
        assert!(validate_component_name(OsStr::new(".")).is_err());
        assert!(validate_component_name(OsStr::new("..")).is_err());
        assert!(validate_component_name(OsStr::new("")).is_err());
        #[cfg(windows)]
        assert!(validate_component_name(OsStr::new(r"nested\old_only.txt")).is_err());
        #[cfg(windows)]
        assert!(validate_component_name(OsStr::new("file.txt:stream")).is_err());
        #[cfg(windows)]
        assert!(validate_component_name(OsStr::new("dir:stream:$DATA")).is_err());
        #[cfg(unix)]
        assert!(validate_component_name(OsStr::new(r"nested\old_only.txt")).is_ok());
        #[cfg(unix)]
        assert!(validate_component_name(OsStr::new("file.txt:stream")).is_ok());
    }

    #[test]
    fn lexical_normalize_resolves_dots_without_fs() {
        assert_eq!(
            lexical_normalize(Path::new("a/b/../c/./d")),
            PathBuf::from("a/c/d")
        );
        assert_eq!(lexical_normalize(Path::new("a/..")), PathBuf::new());
        assert_eq!(lexical_normalize(Path::new("../x")), PathBuf::from("../x"));
        let above_root = lexical_normalize(Path::new("/../etc"));
        assert!(
            above_root
                .components()
                .any(|component| matches!(component, Component::ParentDir)),
            "{above_root:?}"
        );
    }

    #[test]
    fn relative_cwd_and_root_aliases_have_exact_semantics() {
        let _cwd_lock = lock_process_cwd();
        let process_cwd = std::env::current_dir().unwrap();
        let base = tempfile::tempdir_in(&process_cwd).unwrap();
        std::fs::create_dir_all(base.path().join("sub/a")).unwrap();
        let relative_cwd = base.path().strip_prefix(&process_cwd).unwrap().join("sub");
        let expected = lexical_normalize(&process_cwd.join(&relative_cwd));

        let absolute_alias = expected.to_str().unwrap();
        for argument in [
            None,
            Some(""),
            Some("."),
            Some("./"),
            Some("a/.."),
            Some("missing/.."),
            Some(absolute_alias),
        ] {
            let resolved = resolve_search_root(&relative_cwd, argument).unwrap();
            assert_eq!(resolved.root, expected, "{argument:?}");
            assert_eq!(resolved.cwd, expected, "{argument:?}");
        }

        for argument in ["..", "../sub", "a/../.."] {
            let error = resolve_search_root(&relative_cwd, Some(argument)).unwrap_err();
            assert!(
                matches!(error, ToolError::InvalidArgs(_)),
                "{argument}: {error}"
            );
        }
    }

    /// An already-absolute session cwd must not consult the process cwd.
    ///
    /// The deleted-cwd scenario runs in a child process so it cannot steal
    /// the parent suite's working directory. libtest `--exact` matches the
    /// crate-relative module path, not the bare function name; a bare filter
    /// runs zero tests and still exits successfully.
    #[cfg(unix)]
    #[test]
    fn absolute_session_cwd_does_not_require_process_cwd() {
        const CHILD_ENV: &str = "MCODE_FS_SEARCH_INVALID_CWD_CHILD";
        // libtest prints this crate-relative path; `module_path!()` includes the crate name.
        const TEST_NAME: &str =
            "builtin::fs_search::tests::absolute_session_cwd_does_not_require_process_cwd";
        if std::env::var_os(CHILD_ENV).is_some() {
            let session = tempfile::tempdir().unwrap();
            std::fs::write(session.path().join("kept.txt"), "x").unwrap();
            let scratch = tempfile::tempdir().unwrap();
            std::env::set_current_dir(scratch.path()).unwrap();
            let scratch_path = scratch.path().to_path_buf();
            std::mem::forget(scratch);
            std::fs::remove_dir(&scratch_path).unwrap();
            let resolved = resolve_search_root(session.path(), None)
                .expect("absolute session cwd should resolve without process cwd");
            assert_eq!(resolved.root, session.path());
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", "--test-threads", "1", TEST_NAME])
            .env(CHILD_ENV, "1")
            .output()
            .expect("spawn invalid-cwd child");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "stdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            stdout.contains("running 1 test") && stdout.contains("test result: ok. 1 passed"),
            "child must execute the invalid-cwd branch, not an empty --exact filter\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }

    #[test]
    fn internal_parent_that_never_leaves_root_is_allowed() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("docs")).unwrap();
        let resolved = resolve_search_root(directory.path(), Some("docs/../docs")).unwrap();
        // The resolved root carries the handle-proven spelling; a tempdir on
        // a Windows host can hand out an 8.3 alias (`RUNNER~1`) while the
        // resolved root keeps the long on-disk name (`runneradmin`).
        #[cfg(windows)]
        let expected =
            strip_verbatim_prefix(&directory.path().join("docs").canonicalize().unwrap());
        #[cfg(not(windows))]
        let expected = directory.path().join("docs");
        assert_eq!(resolved.root, expected);
    }

    #[test]
    fn containment_is_component_based() {
        assert!(is_within(Path::new("/a/b"), Path::new("/a/b/c")));
        assert!(!is_within(Path::new("/a/b"), Path::new("/a/bb")));
        assert!(!is_within(Path::new("/a/b"), Path::new("/a")));
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_lexical_containment_and_relative_stay_case_sensitive() {
        let root = Path::new("/Ä/repo");
        assert!(!is_within_lexical(root, Path::new("/ä/repo")));
        assert!(strip_prefix_lexical(root, Path::new("/ä/repo")).is_none());
        assert_eq!(
            strip_prefix_lexical(root, Path::new("/Ä/repo/sub")),
            Some(PathBuf::from("sub"))
        );
        assert!(is_within_lexical(root, Path::new("/Ä/repo/sub")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_prefix_conversion_preserves_unc_authority() {
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\UNC\server\share\dir\file")),
            PathBuf::from(r"\\server\share\dir\file")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\C:\dir\file")),
            PathBuf::from(r"C:\dir\file")
        );
        let volume = strip_verbatim_prefix(Path::new(r"\\?\Volume{abc}\dir"));
        assert!(volume.is_absolute(), "{volume:?}");
        assert_eq!(volume, PathBuf::from(r"\\.\Volume{abc}\dir"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_extended_length_conversion_preserves_path_kinds() {
        assert_eq!(
            windows_extended_length_path(Path::new(r"C:\dir\file")),
            PathBuf::from(r"\\?\C:\dir\file")
        );
        assert_eq!(
            windows_extended_length_path(Path::new(r"\\server\share\dir\file")),
            PathBuf::from(r"\\?\UNC\server\share\dir\file")
        );
        for path in [
            r"\\?\C:\dir\file",
            r"\\?\UNC\server\share\dir",
            r"\\.\Device",
        ] {
            assert_eq!(
                windows_extended_length_path(Path::new(path)),
                Path::new(path)
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_long_cwd_opens_with_or_without_a_verbatim_prefix() {
        use std::os::windows::ffi::OsStrExt;

        let directory = tempfile::tempdir().unwrap();
        let mut long_cwd = directory.path().canonicalize().unwrap();
        while long_cwd.as_os_str().encode_wide().count() <= 300 {
            long_cwd.push("0123456789abcdef");
        }
        std::fs::create_dir_all(&long_cwd).unwrap();
        let plain_cwd = strip_verbatim_prefix(&long_cwd);
        let absolute_alias = plain_cwd.to_str().unwrap();

        for (cwd, argument) in [
            (long_cwd.as_path(), None),
            (plain_cwd.as_path(), None),
            (long_cwd.as_path(), Some(absolute_alias)),
        ] {
            let resolved = resolve_search_root(cwd, argument).unwrap();
            assert_eq!(resolved.root, resolved.cwd, "{argument:?}");
            assert_eq!(resolved.cwd, plain_cwd, "{argument:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_non_ascii_case_alias_resolves_like_the_retained_root() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().join("Ä").join("repo");
        std::fs::create_dir_all(cwd.join("sub")).unwrap();
        std::fs::write(cwd.join("sub").join("note.txt"), "hi").unwrap();

        let resolved = resolve_search_root(&cwd, None).unwrap();
        let retained = resolved.cwd.clone();
        let alias = retained
            .to_str()
            .expect("retained cwd is Unicode")
            .to_lowercase();
        assert_ne!(
            alias,
            retained.to_string_lossy().as_ref(),
            "fixture must include a letter that Unicode-lowercases"
        );
        let slash_alias = alias.replace('\\', "/");
        let verbatim_alias = windows_extended_length_path(Path::new(&alias));
        let verbatim_alias = verbatim_alias.to_str().expect("verbatim alias is Unicode");
        let child_alias = Path::new(&alias).join("sub");
        let parent_alias = Path::new(&alias).join("..");
        let escaped_alias = Path::new(&alias).join("sub").join("..").join("..");

        for argument in [alias.as_str(), slash_alias.as_str(), verbatim_alias] {
            let aliased = resolve_search_root(&cwd, Some(argument)).unwrap();
            assert_eq!(aliased.root, retained, "{argument}");
            assert_eq!(aliased.cwd, retained, "{argument}");
        }

        let child = resolve_search_root(
            &cwd,
            Some(child_alias.to_str().expect("child alias is Unicode")),
        )
        .unwrap();
        assert_eq!(child.cwd, retained);
        assert_eq!(child.root, retained.join("sub"));

        for argument in [
            format!("{alias}2"),
            parent_alias
                .to_str()
                .expect("parent alias is Unicode")
                .to_owned(),
            escaped_alias
                .to_str()
                .expect("escaped alias is Unicode")
                .to_owned(),
        ] {
            let error = resolve_search_root(&cwd, Some(&argument)).unwrap_err();
            assert!(
                matches!(error, ToolError::InvalidArgs(_)),
                "{argument}: {error}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_ordinal_case_rejects_turkish_i_expansion_alias() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().join("İ").join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        let resolved = resolve_search_root(&cwd, None).unwrap();
        let retained = resolved.cwd.to_str().expect("retained cwd is Unicode");
        let dotted_alias = retained.replace('İ', "i\u{307}");
        assert_ne!(dotted_alias, retained);
        let error = resolve_search_root(&cwd, Some(&dotted_alias)).unwrap_err();
        assert!(
            matches!(error, ToolError::InvalidArgs(_)),
            "{dotted_alias}: {error}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_ordinal_case_accepts_small_sigma_alias() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().join("Σ").join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        let resolved = resolve_search_root(&cwd, None).unwrap();
        let retained = resolved.cwd.clone();
        let alias = retained
            .to_str()
            .expect("retained cwd is Unicode")
            .replace('Σ', "σ");
        assert_ne!(alias, retained.to_string_lossy().as_ref());
        let aliased = resolve_search_root(&cwd, Some(&alias)).unwrap();
        assert_eq!(aliased.root, retained);
        assert_eq!(aliased.cwd, retained);
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_verbatim_device_and_unc_output_never_leaks_question_prefix() {
        let cases = [
            (r"C:\Users\name", "C:/Users/name"),
            (r"\\?\C:\Users\name", "C:/Users/name"),
            (r"\\server\share\dir", "//server/share/dir"),
            (r"\\?\UNC\server\share\dir", "//server/share/dir"),
            (r"\\.\PhysicalDrive0", "//./PhysicalDrive0"),
            (r"\\?\Volume{abc}\dir", "//./Volume{abc}/dir"),
        ];
        for (input, expected) in cases {
            let rendered = to_posix(Path::new(input));
            assert_eq!(rendered, expected, "{input}");
            assert!(!rendered.contains("//?/"), "{input}: {rendered}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_prefix_containment_is_separate_and_exact() {
        assert!(is_within(
            Path::new(r"C:\root"),
            Path::new(r"C:\root\child")
        ));
        assert!(!is_within(
            Path::new(r"C:\root"),
            Path::new(r"D:\root\child")
        ));
        let verbatim_drive_root = strip_verbatim_prefix(Path::new(r"\\?\C:\root"));
        let verbatim_drive_child = strip_verbatim_prefix(Path::new(r"\\?\C:\root\child"));
        assert!(is_within(&verbatim_drive_root, &verbatim_drive_child));
        let verbatim_root = strip_verbatim_prefix(Path::new(r"\\?\Volume{abc}\root"));
        let verbatim_child = strip_verbatim_prefix(Path::new(r"\\?\Volume{abc}\root\child"));
        assert!(is_within(&verbatim_root, &verbatim_child));
        let unc_root = strip_verbatim_prefix(Path::new(r"\\?\UNC\server\share\root"));
        let unc_child = strip_verbatim_prefix(Path::new(r"\\?\UNC\server\share\root\child"));
        let other_share = strip_verbatim_prefix(Path::new(r"\\?\UNC\server\other\root\child"));
        assert!(is_within(&unc_root, &unc_child));
        assert!(!is_within(&unc_root, &other_share));
        assert!(is_within(
            Path::new(r"\\.\Device\root"),
            Path::new(r"\\.\Device\root\child")
        ));
        assert!(!is_within(
            Path::new(r"\\.\Device\root"),
            Path::new(r"\\.\Other\root\child")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_volume_verbatim_argument_is_not_cwd_relative() {
        let directory = tempfile::tempdir().unwrap();
        let decoy = directory.path().join(r"Volume{abc}").join("dir");
        std::fs::create_dir_all(&decoy).unwrap();
        std::fs::write(decoy.join("secret.txt"), "x").unwrap();
        let error =
            resolve_search_root(directory.path(), Some(r"\\?\Volume{abc}\dir")).unwrap_err();
        assert!(matches!(error, ToolError::InvalidArgs(_)), "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_lexical_containment_and_relative_share_unicode_case() {
        let root = Path::new(r"C:\Ä\repo");
        let alias = Path::new(r"c:\ä\repo");
        let child = Path::new(r"c:\ä\repo\sub\file.rs");
        let mixed_sep = Path::new(r"c:/ä/repo/sub");
        let lookalike = Path::new(r"C:\Ärepo");
        let other_drive = Path::new(r"D:\ä\repo");
        let current_drive_abs = Path::new(r"\ä\repo");
        let drive_relative = Path::new(r"C:ä\repo");
        let drive_only = Path::new(r"C:");
        let verbatim_root = strip_verbatim_prefix(Path::new(r"\\?\C:\Ä\repo"));
        let verbatim_alias = strip_verbatim_prefix(Path::new(r"\\?\c:\ä\repo\sub"));

        assert!(is_within_lexical(root, alias));
        assert_eq!(strip_prefix_lexical(root, alias), Some(PathBuf::new()));
        assert!(
            !is_within(root, alias),
            "handle-proven containment stays exact"
        );

        assert!(is_within_lexical(root, child));
        assert_eq!(
            strip_prefix_lexical(root, child),
            Some(PathBuf::from("sub").join("file.rs"))
        );
        assert!(is_within_lexical(root, mixed_sep));
        assert_eq!(
            strip_prefix_lexical(root, mixed_sep),
            Some(PathBuf::from("sub"))
        );

        assert_eq!(verbatim_root, PathBuf::from(r"C:\Ä\repo"));
        assert!(is_within_lexical(&verbatim_root, &verbatim_alias));
        assert_eq!(
            strip_prefix_lexical(&verbatim_root, &verbatim_alias),
            Some(PathBuf::from("sub"))
        );

        let unc_root = Path::new(r"\\Server\Share\Ä");
        assert_eq!(
            strip_prefix_lexical(unc_root, Path::new(r"\\server\share\ä\child")),
            Some(PathBuf::from("child"))
        );
        assert!(strip_prefix_lexical(unc_root, Path::new(r"\\server\other\ä")).is_none());
        assert!(os_str_eq_lexical(OsStr::new("Σ"), OsStr::new("σ")));
        assert!(!os_str_eq_lexical(OsStr::new("Σ"), OsStr::new("ς")));
        assert_eq!(
            strip_prefix_lexical(Path::new(r"C:\Σ"), Path::new(r"C:\σ\child")),
            Some(PathBuf::from("child"))
        );
        assert!(strip_prefix_lexical(Path::new(r"C:\Σ"), Path::new(r"C:\ς\child")).is_none());
        assert!(!os_str_eq_lexical(OsStr::new("İ"), OsStr::new("i\u{307}")));
        assert!(strip_prefix_lexical(Path::new("İ"), Path::new("i\u{307}")).is_none());

        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;
        let unpaired_a = OsString::from_wide(&[0xD800]);
        let unpaired_b = OsString::from_wide(&[0xD801]);
        assert!(!os_str_eq_lexical(&unpaired_a, &unpaired_b));
        assert!(os_str_eq_lexical(
            &unpaired_a,
            &OsString::from_wide(&[0xD800])
        ));

        assert!(is_within_lexical(
            Path::new(r"\\.\Device\Ä"),
            Path::new(r"\\.\device\ä\child")
        ));
        assert_eq!(
            strip_prefix_lexical(Path::new(r"C:\"), Path::new(r"c:\ä")),
            Some(PathBuf::from("ä"))
        );

        for outsider in [lookalike, other_drive, current_drive_abs, drive_relative] {
            assert!(!is_within_lexical(root, outsider), "{outsider:?}");
            assert!(
                strip_prefix_lexical(root, outsider).is_none(),
                "{outsider:?}"
            );
        }
        assert!(
            strip_prefix_lexical(drive_only, Path::new(r"C:\foo")).is_none(),
            "drive-relative C: does not contain drive-root C:\\foo"
        );
    }

    #[test]
    fn rel_posix_and_absolute_output_are_normalized() {
        let root = Path::new("r");
        assert_eq!(rel_posix(root, &root.join("a/b.rs")), "a/b.rs");
        assert_eq!(rel_posix(&root.join("x.rs"), &root.join("x.rs")), "r/x.rs");
        assert_eq!(to_posix(Path::new("/a/b")), "/a/b");
        assert_eq!(to_posix(Path::new("/")), "/");
    }

    #[test]
    fn display_line_truncates_on_char_boundaries() {
        let mut truncated = false;
        assert_eq!(display_line(b"hello", 10, &mut truncated), "hello");
        assert!(!truncated);

        let multibyte = "é".repeat(50);
        let mut truncated = false;
        let output = display_line(multibyte.as_bytes(), 51, &mut truncated);
        assert!(truncated);
        assert_eq!(output.len(), 50);
    }

    #[test]
    fn scan_reservations_are_atomic_and_settled() {
        let limiter = WalkLimiter::new(&Limits::default());
        assert_eq!(limiter.reserve_scan(8, 10), ScanReservation::Granted(8));
        assert_eq!(limiter.reserve_scan(8, 10), ScanReservation::Granted(2));
        assert_eq!(limiter.reserve_scan(1, 10), ScanReservation::Pending);
        limiter.settle_scan(8, 3);
        assert_eq!(limiter.reserve_scan(5, 10), ScanReservation::Granted(5));
        limiter.settle_scan(2, 2);
        limiter.settle_scan(5, 5);
        assert_eq!(limiter.claimed_scan_bytes(), 10);
        assert_eq!(limiter.reserve_scan(1, 10), ScanReservation::Exhausted);
    }

    #[test]
    fn concurrent_short_read_releases_capacity_before_exhaustion() {
        use std::sync::Arc;
        use std::sync::mpsc;

        let limiter = Arc::new(WalkLimiter::new(&Limits::default()));
        assert_eq!(limiter.reserve_scan(10, 10), ScanReservation::Granted(10));
        let (checked_tx, checked_rx) = mpsc::channel();
        let (settled_tx, settled_rx) = mpsc::channel();
        let contender = {
            let limiter = Arc::clone(&limiter);
            std::thread::spawn(move || {
                checked_tx.send(limiter.reserve_scan(1, 10)).unwrap();
                settled_rx.recv().unwrap();
                limiter.reserve_scan(7, 10)
            })
        };

        assert_eq!(checked_rx.recv().unwrap(), ScanReservation::Pending);
        limiter.settle_scan(10, 3);
        settled_tx.send(()).unwrap();
        assert_eq!(contender.join().unwrap(), ScanReservation::Granted(7));
        limiter.settle_scan(7, 7);
        assert_eq!(limiter.reserve_scan(1, 10), ScanReservation::Exhausted);
    }

    #[test]
    fn target_access_denied_is_an_execution_error() {
        let error = map_target_open_error(
            "private",
            io::Error::new(io::ErrorKind::PermissionDenied, "access denied"),
        );
        assert!(matches!(error, ToolError::Execution(_)), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn fifo_target_resolution_does_not_wait_for_a_writer() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::sync::mpsc;

        let directory = tempfile::tempdir().unwrap();
        let fifo = directory.path().join("input.pipe");
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `fifo_name` is a live NUL-terminated path and the mode is
        // valid. The created FIFO remains owned by the temporary directory.
        let status = unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) };
        assert_eq!(status, 0, "{}", io::Error::last_os_error());

        let cwd = directory.path().to_path_buf();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            result_tx
                .send(resolve_search_root(&cwd, Some("input.pipe")))
                .unwrap();
        });
        let result = match result_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(result) => result,
            Err(error) => {
                // Opening both ends never waits and releases an implementation
                // that accidentally used blocking `O_RDONLY`.
                let _release = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&fifo)
                    .unwrap();
                let _ = result_rx.recv_timeout(Duration::from_secs(2));
                worker.join().unwrap();
                panic!("FIFO target resolution blocked: {error}");
            }
        };
        worker.join().unwrap();
        let error = result.unwrap_err();
        assert!(matches!(error, ToolError::InvalidArgs(_)), "{error}");
    }

    #[test]
    fn limiter_records_first_stop_reason() {
        let limiter = WalkLimiter::new(&Limits::default());
        limiter.stop("time limit reached");
        limiter.stop("cancelled");
        assert_eq!(limiter.stopped_reason(), Some("time limit reached"));
    }

    #[tokio::test]
    async fn run_blocking_returns_value_and_maps_panics() {
        let cancel = CancellationToken::new();
        assert_eq!(
            run_blocking("search", &cancel, SEARCH_TIME_LIMIT, |_| Ok(7usize))
                .await
                .unwrap(),
            7
        );
        let error = run_blocking(
            "search",
            &cancel,
            SEARCH_TIME_LIMIT,
            |_| -> Result<(), ToolError> {
                panic!("boom");
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ToolError::Execution(_)));
    }

    #[tokio::test]
    async fn run_blocking_cancellation_is_an_error() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = run_blocking("find", &cancel, SEARCH_TIME_LIMIT, |_| Ok(()))
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::Execution(_)));
        assert!(error.to_string().contains("cancelled"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_blocking_midflight_cancellation_never_returns_partial_output() {
        use std::sync::mpsc;

        let cancel = CancellationToken::new();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            run_blocking("search", &task_cancel, SEARCH_TIME_LIMIT, move |_| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok("partial")
            })
            .await
        });
        started_rx.recv().unwrap();
        cancel.cancel();
        // The worker must be allowed to unwind; run_blocking joins it.
        release_tx.send(()).unwrap();
        let error = task.await.unwrap().unwrap_err();
        assert!(matches!(error, ToolError::Execution(_)));
        assert!(error.to_string().contains("cancelled"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_blocking_joins_worker_before_returning_on_cancel() {
        use std::sync::mpsc;

        struct WorkerDrop {
            tx: mpsc::Sender<()>,
        }
        impl Drop for WorkerDrop {
            fn drop(&mut self) {
                let _ = self.tx.send(());
            }
        }

        let cancel = CancellationToken::new();
        let (started_tx, started_rx) = mpsc::channel();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let (mut reader, _writer) = blocking_pipe().unwrap();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            run_blocking("search", &task_cancel, SEARCH_TIME_LIMIT, move |token| {
                let _guard = WorkerDrop { tx: dropped_tx };
                started_tx.send(()).unwrap();
                // Block in a syscall so the oneshot cannot complete before
                // the outer cancel branch is chosen and joins this thread.
                interruptible_read(&mut reader);
                let _ = token;
                Ok(())
            })
            .await
        });
        started_rx.recv().unwrap();
        cancel.cancel();
        let error = task.await.unwrap().unwrap_err();
        assert!(dropped_rx.try_recv().is_ok(), "worker was detached");
        assert!(error.to_string().contains("cancelled"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn aborting_run_blocking_future_still_joins_worker_and_handles() {
        use std::sync::mpsc;

        let workers = Arc::new(AtomicU64::new(0));
        let handles = Arc::new(AtomicU64::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let (mut reader, _writer) = blocking_pipe().unwrap();
        let cancel = CancellationToken::new();
        let task_workers = Arc::clone(&workers);
        let task_handles = Arc::clone(&handles);
        let task = tokio::spawn(async move {
            run_blocking_started(
                "search",
                &cancel,
                Some(Instant::now() + SEARCH_TIME_LIMIT),
                WorkerStart {
                    live_workers: Some(task_workers),
                    live_handles: Some(task_handles),
                    ..WorkerStart::default()
                },
                move |_| {
                    started_tx.send(()).unwrap();
                    interruptible_read(&mut reader);
                    Ok(())
                },
            )
            .await
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if workers.load(Ordering::Acquire) == 0 && handles.load(Ordering::Acquire) == 0 {
                break;
            }
            assert!(Instant::now() < deadline, "worker or thread handle leaked");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_run_blocking_future_still_joins_worker_and_handles() {
        use std::sync::mpsc;

        let workers = Arc::new(AtomicU64::new(0));
        let handles = Arc::new(AtomicU64::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let (mut reader, _writer) = blocking_pipe().unwrap();
        let cancel = CancellationToken::new();
        let task_workers = Arc::clone(&workers);
        let task_handles = Arc::clone(&handles);
        let fut = run_blocking_started(
            "search",
            &cancel,
            Some(Instant::now() + SEARCH_TIME_LIMIT),
            WorkerStart {
                live_workers: Some(task_workers),
                live_handles: Some(task_handles),
                ..WorkerStart::default()
            },
            move |_| {
                started_tx.send(()).unwrap();
                interruptible_read(&mut reader);
                Ok(())
            },
        );
        {
            tokio::pin!(fut);
            let wait_start = Instant::now();
            loop {
                if started_rx.try_recv().is_ok() {
                    break;
                }
                assert!(
                    wait_start.elapsed() < Duration::from_secs(2),
                    "worker did not start"
                );
                tokio::select! {
                    biased;
                    _ = &mut fut => panic!("worker finished before drop"),
                    _ = tokio::task::yield_now() => {}
                }
            }
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if workers.load(Ordering::Acquire) == 0 && handles.load(Ordering::Acquire) == 0 {
                break;
            }
            assert!(Instant::now() < deadline, "worker or thread handle leaked");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn duplicate_handle_setup_failure_never_runs_work() {
        let ran = Arc::new(AtomicBool::new(false));
        let ran_worker = Arc::clone(&ran);
        let cancel = CancellationToken::new();
        let error = run_blocking_started(
            "search",
            &cancel,
            Some(Instant::now() + SEARCH_TIME_LIMIT),
            WorkerStart {
                force_interrupt_setup_failure: true,
                ..WorkerStart::default()
            },
            move |_| {
                ran_worker.store(true, Ordering::Release);
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(!ran.load(Ordering::Acquire));
        assert!(error.to_string().contains("interrupt authority"), "{error}");
        assert!(error.to_string().contains("DuplicateHandle"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn worker_unblocks_inherited_sigurg_before_work() {
        use std::sync::mpsc;

        struct RestoreMask(libc::sigset_t);
        impl Drop for RestoreMask {
            fn drop(&mut self) {
                // SAFETY: `self.0` was returned as this thread's prior mask.
                let status = unsafe {
                    libc::pthread_sigmask(libc::SIG_SETMASK, &self.0, std::ptr::null_mut())
                };
                assert_eq!(status, 0);
            }
        }

        // SAFETY: initialized signal sets and documented pthread mask calls.
        let restore = unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            assert_eq!(libc::sigemptyset(&mut set), 0);
            assert_eq!(libc::sigaddset(&mut set, libc::SIGURG), 0);
            let mut old: libc::sigset_t = std::mem::zeroed();
            assert_eq!(libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut old), 0);
            RestoreMask(old)
        };

        let (started_tx, started_rx) = mpsc::channel();
        let (mut reader, _writer) = blocking_pipe().unwrap();
        let cancel = CancellationToken::new();
        let canceller_token = cancel.clone();
        let canceller = std::thread::spawn(move || {
            started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            canceller_token.cancel();
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let error = runtime
            .block_on(run_blocking(
                "search",
                &cancel,
                SEARCH_TIME_LIMIT,
                move |_| {
                    started_tx.send(()).unwrap();
                    interruptible_read(&mut reader);
                    Ok(())
                },
            ))
            .unwrap_err();
        canceller.join().unwrap();
        drop(restore);
        assert!(error.to_string().contains("cancelled"), "{error}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_blocking_time_limit_cancels_the_worker() {
        use std::sync::mpsc;

        let cancel = CancellationToken::new();
        let (stopped_tx, stopped_rx) = mpsc::channel();
        let error = run_blocking(
            "find",
            &cancel,
            Duration::from_millis(20),
            move |worker_cancel| {
                while !worker_cancel.is_cancelled() {
                    std::thread::yield_now();
                }
                stopped_tx.send(()).unwrap();
                Ok(())
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ToolError::Execution(_)), "{error}");
        assert!(error.to_string().contains("time limit reached"), "{error}");
        stopped_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn cancelled_resolve_fails_closed_before_ignore_walk() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join(".ignore"), "secret.txt\n").unwrap();
        std::fs::write(directory.path().join("secret.txt"), "hello leak\n").unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = resolve_search_root_cancel(directory.path(), None, &cancel, &Limits::default())
            .unwrap_err();
        assert!(matches!(error, ToolError::Execution(_)), "{error}");
    }

    #[test]
    fn expired_deadline_fails_closed_on_resolve() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join(".ignore"), "secret.txt\n").unwrap();
        std::fs::write(directory.path().join("secret.txt"), "hello leak\n").unwrap();
        let error = resolve_search_root_cancel(
            directory.path(),
            None,
            &CancellationToken::new(),
            &Limits {
                time_limit: Duration::ZERO,
                ..Limits::default()
            },
        )
        .unwrap_err();
        assert!(matches!(error, ToolError::Execution(_)), "{error}");
    }

    #[test]
    fn huge_logical_ignore_fails_closed_quickly() {
        let directory = tempfile::tempdir().unwrap();
        let ignore_path = directory.path().join(".ignore");
        let file = std::fs::File::create(&ignore_path).unwrap();
        if file.set_len(1 << 40).is_err() {
            std::fs::write(&ignore_path, vec![b'x'; IGNORE_FILE_MAX_BYTES + 1]).unwrap();
        }
        std::fs::write(directory.path().join("secret.txt"), "hello leak\n").unwrap();
        let started = Instant::now();
        let error = resolve_search_root(directory.path(), None).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "huge ignore file hung the resolver"
        );
        assert!(matches!(error, ToolError::Execution(_)), "{error}");
        assert!(error.to_string().contains("ignore"), "{error}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_blocking_timeout_does_not_keep_reading_ignore() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join(".ignore"),
            vec![b'#'; IGNORE_FILE_MAX_BYTES],
        )
        .unwrap();
        std::fs::write(directory.path().join("secret.txt"), "hello leak\n").unwrap();
        let cwd = directory.path().to_path_buf();
        let cancel = CancellationToken::new();
        let started = Instant::now();
        let error = run_blocking(
            "search",
            &cancel,
            Duration::from_millis(20),
            move |worker_cancel| {
                while !worker_cancel.is_cancelled() {
                    let _ = resolve_search_root_cancel(
                        &cwd,
                        None,
                        &worker_cancel,
                        &Limits {
                            time_limit: Duration::from_millis(20),
                            ..Limits::default()
                        },
                    );
                }
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "ignore read continued after spawn_blocking timeout"
        );
        assert!(matches!(error, ToolError::Execution(_)), "{error}");
        assert!(error.to_string().contains("time limit reached"), "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn last_wide_component_keeps_stored_spelling() {
        assert_eq!(
            last_wide_component(OsStr::new(r"C:\Visible\Sub")),
            Some(OsString::from("Sub"))
        );
        assert_eq!(
            last_wide_component(OsStr::new(r"C:\Visible\file.")),
            Some(OsString::from("file."))
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_on_disk_component_name_can_scan_the_same_parent_twice() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("Visible")).unwrap();
        let parent = File::open(directory.path()).unwrap();
        let child = File::open(directory.path().join("Visible")).unwrap();
        let identity = identity_and_kind(&child).unwrap().0;
        drop(child);
        let cancel = CancellationToken::new();
        let limiter = WalkLimiter::new(&Limits::default());
        let first = unix_on_disk_component_name(&parent, identity, &limiter, &cancel).unwrap();
        let second = unix_on_disk_component_name(&parent, identity, &limiter, &cancel).unwrap();
        assert_eq!(first, OsString::from("Visible"));
        assert_eq!(second, first);
    }

    #[test]
    fn default_dot_and_empty_path_resolve_on_case_sensitive_tempdir() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("kept.txt"), "hello\n").unwrap();
        for path in [None, Some(""), Some(".")] {
            let resolved = resolve_search_root(directory.path(), path).unwrap_or_else(|error| {
                panic!("default/empty/dot path {path:?} must resolve: {error}")
            });
            assert_eq!(resolved.target_relative(), Path::new(""));
            assert!(!resolved.is_file());
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_hardlink_pair_fails_unique_on_disk_spelling() {
        let directory = tempfile::tempdir().unwrap();
        let alpha = directory.path().join("alpha");
        let beta = directory.path().join("beta");
        std::fs::write(&alpha, "x").unwrap();
        std::fs::hard_link(&alpha, &beta).expect("case-sensitive temp dir supports hardlinks");
        let parent = File::open(directory.path()).unwrap();
        let child = File::open(&alpha).unwrap();
        let identity = identity_and_kind(&child).unwrap().0;
        drop(child);
        let error = unix_on_disk_component_name(
            &parent,
            identity,
            &WalkLimiter::new(&Limits::default()),
            &CancellationToken::new(),
        )
        .expect_err("duplicate directory-entry identities must fail closed");
        assert!(
            error
                .to_string()
                .contains("multiple directory entries share the opened identity"),
            "{error}"
        );
    }

    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    #[test]
    fn unix_content_openat_flags_are_nofollow_rdonly() {
        use std::sync::{Arc, Mutex};

        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("kept.txt"), "x").unwrap();
        let seen = Arc::new(Mutex::new(None));
        let seen_gate = Arc::clone(&seen);
        let limits = Limits {
            access_gate: Some(AccessGate(Arc::new(move |name, observed: ObservedOpen| {
                if name == OsStr::new("kept.txt") {
                    *seen_gate.lock().expect("openat flags log") = Some(observed.flags);
                }
                Ok(())
            }))),
            ..Limits::default()
        };
        let resolved = resolve_search_root_with_access(
            directory.path(),
            Some("kept.txt"),
            &CancellationToken::new(),
            &limits,
            SearchAccess::Content,
        )
        .unwrap();
        drop(resolved);
        let flags = seen
            .lock()
            .expect("openat flags log")
            .expect("content open must observe Darwin/BSD openat flags");
        assert_eq!(flags & libc::O_ACCMODE, libc::O_RDONLY);
        assert_ne!(flags & libc::O_NOFOLLOW, 0);
        assert_ne!(flags & libc::O_CLOEXEC, 0);
        assert_ne!(flags & libc::O_NONBLOCK, 0);
        assert_eq!(flags & libc::O_DIRECTORY, 0);
    }

    #[cfg(unix)]
    #[test]
    fn unix_metadata_directory_open_survives_canonical_rescan() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("nested")).unwrap();
        std::fs::write(directory.path().join("nested/kept.txt"), "x").unwrap();
        let resolved = resolve_search_root_with_access(
            directory.path(),
            Some("nested"),
            &CancellationToken::new(),
            &Limits::default(),
            SearchAccess::Metadata,
        )
        .unwrap();
        assert_eq!(resolved.target_relative(), Path::new("nested"));
        assert!(!resolved.is_file());
        assert!(resolved.target.is_content_file());
    }

    #[cfg(unix)]
    #[test]
    fn unix_casefold_alias_uses_unique_on_disk_spelling_when_supported() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("Secrets/Keys")).unwrap();
        if !unix_casefold_alias_supported(directory.path(), "Secrets")
            || !unix_casefold_alias_supported(&directory.path().join("Secrets"), "Keys")
        {
            return;
        }
        let resolved = resolve_search_root(directory.path(), Some("secrets/keys")).unwrap();
        assert_eq!(
            resolved.target_relative(),
            Path::new("Secrets").join("Keys")
        );
        assert_eq!(resolved.root, directory.path().join("Secrets/Keys"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_alias_target_relative_uses_on_disk_component_spelling() {
        let directory = tempfile::tempdir().unwrap();
        let visible = directory.path().join("Visible").join("Sub");
        std::fs::create_dir_all(&visible).unwrap();
        std::fs::write(visible.join("secret.txt"), "x").unwrap();
        std::fs::write(
            directory.path().join(".ignore"),
            "/Visible/Sub/secret.txt\n",
        )
        .unwrap();
        let resolved = resolve_search_root(directory.path(), Some("visible/sub")).unwrap();
        assert_eq!(resolved.target_relative(), Path::new("Visible").join("Sub"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_eight_dot_three_alias_uses_on_disk_long_component() {
        let directory = tempfile::tempdir().unwrap();
        let long_dir = directory.path().join("LongVisibleName");
        std::fs::create_dir(&long_dir).unwrap();
        std::fs::write(long_dir.join("secret.txt"), "x").unwrap();
        std::fs::write(
            directory.path().join(".ignore"),
            "/LongVisibleName/secret.txt\n",
        )
        .unwrap();
        let short = windows_short_path(&long_dir).expect("GetShortPathNameW must succeed");
        let short_name = short
            .file_name()
            .expect("short path has a file name")
            .to_os_string();
        if short_name == long_dir.file_name().unwrap() {
            // Volume has 8.3 generation disabled; Unicode case covers aliases.
            return;
        }
        assert!(
            short_name.to_string_lossy().contains('~'),
            "expected an 8.3 alias, got {short_name:?}"
        );
        let argument = short_name.to_str().expect("8.3 name is UTF-8");
        let resolved = resolve_search_root(directory.path(), Some(argument)).unwrap();
        assert_eq!(resolved.target_relative(), Path::new("LongVisibleName"));
    }

    #[test]
    fn prepared_search_key_uses_on_disk_spelling() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("secrets/keys")).unwrap();
        let key = |path: &str| {
            prepare_search(directory.path(), Some(path), &CancellationToken::new())
                .unwrap()
                .key()
                .to_owned()
        };
        assert_eq!(key("./secrets/keys"), "secrets/keys");
        assert_eq!(key("x/../secrets/keys"), "secrets/keys");
        let absolute = directory.path().join("secrets").join("keys");
        assert_eq!(key(absolute.to_str().unwrap()), "secrets/keys");
    }

    #[test]
    fn ignore_read_requests_remaining_plus_one_probe_byte() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join(".ignore"),
            vec![b'x'; IGNORE_FILE_MAX_BYTES + 64],
        )
        .unwrap();
        let handle = open_allowed_root(directory.path()).unwrap();
        let limiter = WalkLimiter::new(&Limits::default());
        let error =
            walk::read_ignore_file_for_test(&handle.file, &limiter, &CancellationToken::new())
                .unwrap_err();
        assert!(error.to_string().contains("size limit"), "{error}");
        let read = limiter.ignore_read_bytes();
        assert!(
            read <= u64::try_from(IGNORE_FILE_MAX_BYTES + 1).unwrap(),
            "ignore I/O {read} exceeded remaining-plus-probe cap"
        );
        assert_eq!(read, u64::try_from(IGNORE_FILE_MAX_BYTES + 1).unwrap());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn same_device_hardlink_to_outside_file_is_rejected() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "SECRET_HARDLINK\n").unwrap();
        let allowed = tempfile::tempdir().unwrap();
        let alias = allowed.path().join("alias.txt");
        std::fs::hard_link(outside.path().join("secret.txt"), &alias)
            .expect("fixture requested a same-device hardlink");
        let error = resolve_search_root(allowed.path(), Some("alias.txt"));
        assert!(error.is_err(), "hardlink target must fail closed");
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "opt-in privileged FS fixture; set MCODE_PRIVILEGED_FS_TESTS=1"]
    fn bind_mount_of_outside_directory_is_rejected() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "SECRET_BIND\n").unwrap();
        let allowed = tempfile::tempdir().unwrap();
        let mount = allowed.path().join("mnt");
        std::fs::create_dir(&mount).unwrap();
        let status = std::process::Command::new("mount")
            .args([
                "--bind",
                outside.path().to_str().unwrap(),
                mount.to_str().unwrap(),
            ])
            .status();
        let status = status.expect("mount --bind must be invocable");
        assert!(
            status.success(),
            "fixture requested a bind mount but mount --bind failed: {status}"
        );
        struct Umount(std::path::PathBuf);
        impl Drop for Umount {
            fn drop(&mut self) {
                let _ = std::process::Command::new("umount").arg(&self.0).status();
            }
        }
        let _umount = Umount(mount.clone());
        let error = resolve_search_root(allowed.path(), Some("mnt"));
        assert!(error.is_err(), "bind mount must fail closed");
    }

    #[cfg(not(any(unix, windows)))]
    #[test]
    fn unsupported_platform_child_open_fails_closed() {
        let error = open_child_file(
            &File::open(".").unwrap(),
            OsStr::new("x"),
            None,
            NameMatch::Exact,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    fn counting_limits() -> (Limits, Arc<AtomicU64>) {
        let count = Arc::new(AtomicU64::new(0));
        let limits = Limits {
            resolve_count: Some(Arc::clone(&count)),
            ..Limits::default()
        };
        (limits, count)
    }

    #[test]
    fn prepare_search_resolves_the_target_once() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("safe/keys")).unwrap();
        let (limits, count) = counting_limits();
        let prepared = prepare_search_with_limits(
            directory.path(),
            Some("safe/keys"),
            &CancellationToken::new(),
            &limits,
        )
        .unwrap();

        assert_eq!(prepared.key(), "safe/keys");
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn prepare_search_missing_path_is_terminal() {
        let directory = tempfile::tempdir().unwrap();
        let error =
            prepare_search(directory.path(), Some("later"), &CancellationToken::new()).unwrap_err();
        assert!(matches!(error, ToolError::Execution(_)), "{error}");
        assert!(error.to_string().contains("does not exist"), "{error}");
    }

    #[test]
    fn missing_target_does_not_execute_after_it_appears() {
        let directory = tempfile::tempdir().unwrap();
        let error =
            prepare_search(directory.path(), Some("later"), &CancellationToken::new()).unwrap_err();
        assert!(matches!(error, ToolError::Execution(_)), "{error}");
        std::fs::create_dir(directory.path().join("later")).unwrap();
        std::fs::write(directory.path().join("later").join("secret.txt"), "x").unwrap();
        let (limits, count) = counting_limits();
        // No PreparedSearch exists to re-resolve. The internal no-preflight
        // path may resolve once; a consumed or absent prepared root must not.
        let prepared = prepare_search_with_limits(
            directory.path(),
            Some("later"),
            &CancellationToken::new(),
            &limits,
        )
        .unwrap();
        assert_eq!(count.load(Ordering::Relaxed), 1);
        let _root = bind_search_root(
            Some(&prepared),
            directory.path(),
            Some("later"),
            &CancellationToken::new(),
            &limits,
        )
        .unwrap();
        let replay = bind_search_root(
            Some(&prepared),
            directory.path(),
            Some("later"),
            &CancellationToken::new(),
            &limits,
        )
        .unwrap_err();
        assert!(
            replay
                .to_string()
                .contains("missing or was already consumed"),
            "{replay}"
        );
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn bind_search_root_consumed_prepared_does_not_reresolve() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("safe")).unwrap();
        let (limits, count) = counting_limits();
        let prepared = prepare_search_with_limits(
            directory.path(),
            Some("safe"),
            &CancellationToken::new(),
            &limits,
        )
        .unwrap();
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert!(prepared.take_root().is_some());
        let error = bind_search_root(
            Some(&prepared),
            directory.path(),
            Some("safe"),
            &CancellationToken::new(),
            &limits,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("missing or was already consumed"),
            "{error}"
        );
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[cfg(windows)]
    #[test]
    fn prepare_search_sharing_violation_on_alias_is_terminal() {
        let directory = tempfile::tempdir().unwrap();
        let visible = directory.path().join("Visible.txt");
        std::fs::write(&visible, "secret\n").unwrap();
        let _lock = exclusive_open(&visible).expect("exclusive open");
        let error = prepare_search(
            directory.path(),
            Some("visible.txt"),
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert!(matches!(error, ToolError::Execution(_)), "{error}");
        assert!(
            error.to_string().contains("does not exist")
                || error.to_string().to_ascii_lowercase().contains("sharing"),
            "{error}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn sharing_violation_alias_does_not_execute_after_unlock() {
        let directory = tempfile::tempdir().unwrap();
        let visible = directory.path().join("Visible.txt");
        std::fs::write(&visible, "secret\n").unwrap();
        let lock = exclusive_open(&visible).expect("exclusive open");
        let error = prepare_search(
            directory.path(),
            Some("visible.txt"),
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert!(matches!(error, ToolError::Execution(_)), "{error}");
        drop(lock);
        let (limits, count) = counting_limits();
        // Unlocking must not revive a lexical PreparedSearch; there is none.
        // A fresh prepare is a new resolve, not an execute of the failed one.
        let prepared = prepare_search_with_limits(
            directory.path(),
            Some("visible.txt"),
            &CancellationToken::new(),
            &limits,
        )
        .unwrap();
        assert_eq!(prepared.key(), "Visible.txt");
        assert_eq!(count.load(Ordering::Relaxed), 1);
        let replay = bind_search_root(
            Some(&prepared),
            directory.path(),
            Some("visible.txt"),
            &CancellationToken::new(),
            &limits,
        )
        .unwrap();
        drop(replay);
        let consumed = bind_search_root(
            Some(&prepared),
            directory.path(),
            Some("visible.txt"),
            &CancellationToken::new(),
            &limits,
        )
        .unwrap_err();
        assert!(
            consumed
                .to_string()
                .contains("missing or was already consumed"),
            "{consumed}"
        );
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn binding_prepared_root_refreshes_execution_deadline() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("keep.txt"), "hello\n").unwrap();
        let limits = Limits {
            time_limit: Duration::from_millis(40),
            ..Limits::default()
        };
        let prepared =
            prepare_search_with_limits(directory.path(), None, &CancellationToken::new(), &limits)
                .unwrap();
        std::thread::sleep(Duration::from_millis(80));
        let root = bind_search_root(
            Some(&prepared),
            directory.path(),
            None,
            &CancellationToken::new(),
            &limits,
        )
        .unwrap();
        assert!(
            matches!(
                root.limiter.check(&CancellationToken::new()),
                ignore::WalkState::Continue
            ),
            "wait between prepare and execute must not spend the execution budget"
        );
        assert_eq!(root.limiter.stopped_reason(), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocking_io_timeout_after_final_check_joins() {
        assert_interrupt_after_final_check(InterruptKind::Timeout).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocking_io_cancel_after_final_check_joins() {
        assert_interrupt_after_final_check(InterruptKind::Cancel).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preflight_cancel_does_not_stall_executor_heartbeat() {
        let ticks = Arc::new(AtomicU64::new(0));
        let ticker = {
            let ticks = Arc::clone(&ticks);
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(10));
                loop {
                    interval.tick().await;
                    ticks.fetch_add(1, Ordering::Relaxed);
                }
            })
        };
        let (reader, _writer) = blocking_pipe().unwrap();
        let reader = std::sync::Mutex::new(Some(reader));
        let block: BlockWorkerHook = Arc::new(move |cancel: &CancellationToken| {
            let Some(mut reader) = reader.lock().ok().and_then(|mut guard| guard.take()) else {
                return;
            };
            interruptible_block(&mut reader, cancel);
        });
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            prepare_search_async_with_io_block(PathBuf::from("."), None, task_cancel, block).await
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel.cancel();
        let _ = task.await;
        tokio::time::sleep(Duration::from_millis(40)).await;
        ticker.abort();
        assert!(ticks.load(Ordering::Relaxed) >= 2);
    }

    enum InterruptKind {
        Timeout,
        Cancel,
    }

    async fn assert_interrupt_after_final_check(kind: InterruptKind) {
        let past_check = Arc::new(AtomicBool::new(false));
        let enter_read = Arc::new(AtomicBool::new(false));
        let (reader, _writer) = blocking_pipe().unwrap();
        let cancel = CancellationToken::new();
        let work_cancel = cancel.clone();
        let started = Instant::now();
        let work = tokio::spawn({
            let past_check = Arc::clone(&past_check);
            let enter_read = Arc::clone(&enter_read);
            async move {
                run_blocking(
                    "search",
                    &work_cancel,
                    Duration::from_millis(80),
                    move |worker_cancel| {
                        let mut reader = reader;
                        interruptible_block_after_final_check(
                            &mut reader,
                            &worker_cancel,
                            &past_check,
                            &enter_read,
                        );
                        Ok(())
                    },
                )
                .await
            }
        });
        wait_flag(&past_check, "worker past last token check").await;
        match kind {
            InterruptKind::Timeout => {
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            InterruptKind::Cancel => {
                cancel.cancel();
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        // First interrupt was published after the last token check and may
        // already have been consumed. Enter the blocking read now.
        enter_read.store(true, Ordering::Release);
        let error = work.await.expect("join run_blocking").unwrap_err();
        match kind {
            InterruptKind::Timeout => {
                assert!(error.to_string().contains("time limit reached"), "{error}");
            }
            InterruptKind::Cancel => {
                assert!(error.to_string().contains("cancelled"), "{error}");
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    async fn wait_flag(flag: &AtomicBool, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !flag.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    #[cfg(windows)]
    fn exclusive_open(path: &Path) -> io::Result<File> {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_BACKUP_SEMANTICS: required to open a directory handle.
        // Without it, exclusive directory locks fail and the sharing barrier
        // would be skipped.
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    }

    fn interruptible_block_after_final_check(
        reader: &mut File,
        cancel: &CancellationToken,
        past_check: &AtomicBool,
        enter_read: &AtomicBool,
    ) {
        if cancel.is_cancelled() {
            return;
        }
        past_check.store(true, Ordering::Release);
        while !enter_read.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        interruptible_read(reader);
    }

    fn blocking_pipe() -> io::Result<(File, File)> {
        #[cfg(unix)]
        {
            use std::os::fd::FromRawFd;
            let mut fds = [0; 2];
            // SAFETY: `fds` is two writable integers; a successful `pipe` fills
            // both with owned descriptors transferred into `File` below.
            let status = unsafe { libc::pipe(fds.as_mut_ptr()) };
            if status != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: exclusive ownership of the new pipe ends.
            Ok(unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::FromRawHandle;
            use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
            use windows_sys::Win32::System::Pipes::CreatePipe;
            let mut read = INVALID_HANDLE_VALUE;
            let mut write = INVALID_HANDLE_VALUE;
            // SAFETY: output handles are writable; a successful call yields
            // owned pipe ends transferred into `File`.
            let ok = unsafe { CreatePipe(&mut read, &mut write, std::ptr::null_mut(), 0) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: exclusive ownership of the new pipe ends.
            Ok(unsafe { (File::from_raw_handle(read), File::from_raw_handle(write)) })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
    }

    fn interruptible_block(reader: &mut File, cancel: &CancellationToken) {
        if !cancel.is_cancelled() {
            interruptible_read(reader);
        }
    }

    fn interruptible_read(reader: &mut File) {
        let mut byte = [0u8; 1];
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;

            if wait_for_worker_readable(reader).is_err() {
                return;
            }
            // SAFETY: `reader` is a live pipe fd; a one-byte buffer is valid.
            let read = unsafe { libc::read(reader.as_raw_fd(), byte.as_mut_ptr().cast(), 1) };
            if read < 0 {
                // After cancel, SIGURG makes read return EINTR. Do not retry:
                // the writer is still open, so a retry would block forever.
                let _ = io::Error::last_os_error();
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            let mut read = 0u32;
            // SAFETY: blocking read of one byte from a live pipe handle.
            let ok = unsafe {
                windows_sys::Win32::Storage::FileSystem::ReadFile(
                    reader.as_raw_handle(),
                    byte.as_mut_ptr().cast(),
                    1,
                    &mut read,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                // Capture immediately. CancelSynchronousIo fails the
                // read with ERROR_OPERATION_ABORTED; return so the
                // worker can observe the published cancel token.
                let _ = io::Error::last_os_error();
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = reader.read(&mut byte);
        }
    }

    #[cfg(windows)]
    #[test]
    fn file_identity_distinguishes_128_bit_ids_with_same_low_64() {
        let low = FileIdentity::from_raw(1, [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let mut high = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        high[8] = 1;
        let other = FileIdentity::from_raw(1, high);
        assert_ne!(low, other);
    }

    #[test]
    fn identity_query_failure_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let limits = Limits {
            force_identity_error: true,
            ..Limits::default()
        };
        let error =
            resolve_search_root_cancel(directory.path(), None, &CancellationToken::new(), &limits)
                .unwrap_err();
        assert!(
            error.to_string().contains("identity") || error.to_string().contains("accessible"),
            "{error}"
        );
    }

    #[test]
    fn hidden_query_failure_is_not_a_silent_skip() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("visible.txt"), "x").unwrap();
        let limits = Limits {
            force_hidden_error: true,
            ..Limits::default()
        };
        let root = resolve_search_root_cancel(
            directory.path(),
            Some("visible.txt"),
            &CancellationToken::new(),
            &limits,
        );
        match root {
            Ok(root) => {
                let error = root.target_is_skipped().unwrap_err();
                assert!(error.to_string().contains("hidden-attribute"), "{error}");
            }
            Err(error) => {
                assert!(
                    error.to_string().contains("hidden") || error.to_string().contains("identity"),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn git_open_permission_denied_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".git")).unwrap();
        std::fs::write(directory.path().join(".gitignore"), "secret.txt\n").unwrap();
        std::fs::write(directory.path().join("secret.txt"), "x").unwrap();
        let limits = Limits {
            open_fault: Some(OpenFault(Arc::new(|name| {
                if name == OsStr::new(".git") {
                    Err(io::Error::from(io::ErrorKind::PermissionDenied))
                } else {
                    Ok(())
                }
            }))),
            ..Limits::default()
        };
        let error =
            resolve_search_root_cancel(directory.path(), None, &CancellationToken::new(), &limits)
                .unwrap_err();
        assert!(error.to_string().contains("ignore"), "{error}");
    }

    #[test]
    fn parent_gitignore_applies_when_cwd_is_a_subdirectory() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".git/info")).unwrap();
        std::fs::write(repo.path().join(".gitignore"), "secret.txt\n").unwrap();
        let sub = repo.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("secret.txt"), "x").unwrap();
        std::fs::write(sub.join("kept.txt"), "x").unwrap();
        let root = resolve_search_root(&sub, None).unwrap();
        assert!(walk::relative_is_skipped(
            &root.ignores,
            Path::new("secret.txt"),
            false
        ));
        assert!(!walk::relative_is_skipped(
            &root.ignores,
            Path::new("kept.txt"),
            false
        ));
    }

    #[test]
    fn linked_worktree_relative_commondir_loads_exclude() {
        let repo = tempfile::tempdir().unwrap();
        let git = repo.path().join(".git");
        std::fs::create_dir_all(git.join("info")).unwrap();
        std::fs::write(git.join("info/exclude"), "from_exclude.txt\n").unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let wt_git = git.join("worktrees/wt1");
        std::fs::create_dir_all(&wt_git).unwrap();
        let rel = pathdiff_from_to(worktree.path(), &wt_git);
        std::fs::write(
            worktree.path().join(".git"),
            format!("gitdir: {}\n", rel.display()),
        )
        .unwrap();
        std::fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        std::fs::write(worktree.path().join("from_exclude.txt"), "x").unwrap();
        std::fs::write(worktree.path().join("kept.txt"), "x").unwrap();
        let root = resolve_search_root(worktree.path(), None).unwrap();
        assert!(walk::relative_is_skipped(
            &root.ignores,
            Path::new("from_exclude.txt"),
            false
        ));
        assert!(!walk::relative_is_skipped(
            &root.ignores,
            Path::new("kept.txt"),
            false
        ));
    }

    #[test]
    fn linked_worktree_absolute_commondir_loads_exclude() {
        let repo = tempfile::tempdir().unwrap();
        let git = repo.path().join(".git");
        std::fs::create_dir_all(git.join("info")).unwrap();
        std::fs::write(git.join("info/exclude"), "abs_exclude.txt\n").unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let wt_git = git.join("worktrees/wt2");
        std::fs::create_dir_all(&wt_git).unwrap();
        let canonical_wt_git = std::fs::canonicalize(&wt_git).unwrap();
        std::fs::write(
            worktree.path().join(".git"),
            format!("gitdir: {}\n", canonical_wt_git.display()),
        )
        .unwrap();
        let canonical_git = std::fs::canonicalize(&git).unwrap();
        std::fs::write(
            wt_git.join("commondir"),
            format!("{}\n", canonical_git.display()),
        )
        .unwrap();
        std::fs::write(worktree.path().join("abs_exclude.txt"), "x").unwrap();
        std::fs::write(worktree.path().join("kept.txt"), "x").unwrap();
        let root = resolve_search_root(worktree.path(), None).unwrap();
        assert!(walk::relative_is_skipped(
            &root.ignores,
            Path::new("abs_exclude.txt"),
            false
        ));
        assert!(!walk::relative_is_skipped(
            &root.ignores,
            Path::new("kept.txt"),
            false
        ));
    }

    #[test]
    fn malformed_gitdir_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join(".git"), "not-a-gitdir\n").unwrap();
        let error = resolve_search_root(directory.path(), None).unwrap_err();
        assert!(error.to_string().contains("ignore"), "{error}");
    }

    #[test]
    fn ignore_rules_are_reserved_before_compile() {
        let directory = tempfile::tempdir().unwrap();
        let mut text = String::new();
        for index in 0..32 {
            text.push_str(&format!("rule{index}\n"));
        }
        std::fs::write(directory.path().join(".ignore"), text).unwrap();
        let limits = Limits {
            max_ignore_rules: 4,
            ..Limits::default()
        };
        let limiter = WalkLimiter::new(&limits);
        let _seams = bind_current_limiter(&Arc::new(WalkLimiter::new(&limits)));
        let error =
            resolve_search_root_cancel(directory.path(), None, &CancellationToken::new(), &limits)
                .unwrap_err();
        assert!(error.to_string().contains("rule limit"), "{error}");
        assert!(limiter.ignore_rules() <= 4, "{}", limiter.ignore_rules());
        let _ = _seams;
    }

    #[test]
    fn explicit_target_depth_is_limited_before_open() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("a/b/c")).unwrap();
        std::fs::write(directory.path().join("a/b/c/leaf.txt"), "x").unwrap();
        let limits = Limits {
            max_walk_depth: 2,
            ..Limits::default()
        };
        let error = resolve_search_root_cancel(
            directory.path(),
            Some("a/b/c"),
            &CancellationToken::new(),
            &limits,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("depth") || error.to_string().contains("ignore"),
            "{error}"
        );
    }

    #[test]
    fn walk_entry_budget_is_reserved_per_name() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".git")).unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(directory.path().join(name), "x").unwrap();
        }
        let limits = Limits {
            max_walk_entries: 1,
            ..Limits::default()
        };
        let root =
            resolve_search_root_cancel(directory.path(), None, &CancellationToken::new(), &limits)
                .unwrap();
        let limiter = Arc::clone(&root.limiter);
        let io = IoErrors::new(1);
        let _ = walk_retained_tree(
            &root,
            &limiter,
            &CancellationToken::new(),
            &io,
            |_, _, _, _| ignore::WalkState::Continue,
        );
        assert!(limiter.walk_entries() <= 1, "{}", limiter.walk_entries());
        let accesses_at_exhaustion = limiter.entry_accesses();
        assert!(accesses_at_exhaustion >= 1);
        let _ = walk_retained_tree(
            &root,
            &limiter,
            &CancellationToken::new(),
            &io,
            |_, _, _, _| ignore::WalkState::Continue,
        );
        assert_eq!(
            limiter.entry_accesses(),
            accesses_at_exhaustion,
            "an exhausted entry budget must stop before another listing syscall"
        );
        assert_eq!(limiter.result_store_bytes(), 0);
        assert_eq!(limiter.stopped_reason(), Some("walk entry limit reached"));
    }

    #[cfg(unix)]
    #[test]
    fn canonical_name_scan_shares_entry_budget() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("Visible")).unwrap();
        std::fs::create_dir(directory.path().join("Other")).unwrap();
        std::fs::write(directory.path().join("extra.txt"), "x").unwrap();
        let parent = File::open(directory.path()).unwrap();
        let child = File::open(directory.path().join("Visible")).unwrap();
        let identity = identity_and_kind(&child).unwrap().0;
        drop(child);
        let cancel = CancellationToken::new();
        let limiter = WalkLimiter::new(&Limits {
            max_walk_entries: 1,
            ..Limits::default()
        });
        let _ = unix_on_disk_component_name(&parent, identity, &limiter, &cancel);
        let accesses_at_exhaustion = limiter.entry_accesses();
        assert!(accesses_at_exhaustion >= 1);
        assert!(limiter.walk_entries() <= 1, "{}", limiter.walk_entries());
        let _ = unix_on_disk_component_name(&parent, identity, &limiter, &cancel);
        assert_eq!(
            limiter.entry_accesses(),
            accesses_at_exhaustion,
            "an exhausted canonical scan must stop before another readdir"
        );
    }

    #[test]
    fn parent_open_fault_fails_closed() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".git/info")).unwrap();
        std::fs::write(repo.path().join(".gitignore"), "secret.txt\n").unwrap();
        let sub = repo.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("secret.txt"), "x").unwrap();
        let limits = Limits {
            open_fault: Some(OpenFault(Arc::new(|name| {
                if name == OsStr::new("..") {
                    Err(io::Error::from(io::ErrorKind::NotFound))
                } else {
                    Ok(())
                }
            }))),
            ..Limits::default()
        };
        let error =
            resolve_search_root_cancel(&sub, None, &CancellationToken::new(), &limits).unwrap_err();
        assert!(
            error.to_string().contains("ignore") || error.to_string().contains("parent"),
            "{error}"
        );
    }

    #[cfg(windows)]
    fn open_shared_directory(path: &Path) -> File {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .expect("open directory with delete sharing")
    }

    #[cfg(windows)]
    #[test]
    fn parent_rename_race_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let original = tmp.path().join("repo");
        let sub = original.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let child = open_shared_directory(&sub);
        let decoy = tmp.path().join("decoy");
        std::fs::create_dir_all(decoy.join("sub")).unwrap();
        let swapped = Arc::new(AtomicBool::new(false));
        let swapped_hook = Arc::clone(&swapped);
        let limits = Limits {
            parent_discovery_hook: Some(ParentDiscoveryHook(Arc::new(move |_path| {
                if swapped_hook.swap(true, Ordering::SeqCst) {
                    return Ok(None);
                }
                // Snapshot already captured `original`; opening this decoy
                // is the rename TOCTOU where that path now names another dir.
                Ok(Some(decoy.clone()))
            }))),
            ..Limits::default()
        };
        let limiter = WalkLimiter::new(&limits);
        let _seams = bind_current_limiter(&Arc::new(limiter));
        let error = open_parent_directory(&child).unwrap_err();
        assert!(
            error.to_string().contains("no longer contains")
                || error.kind() == io::ErrorKind::NotFound
                || error.kind() == io::ErrorKind::InvalidData,
            "{error}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn parent_reparse_race_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let original = tmp.path().join("repo");
        let sub = original.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let child = open_shared_directory(&sub);
        let target = tmp.path().join("target");
        std::fs::create_dir_all(target.join("sub")).unwrap();
        let junction_path = tmp.path().join("junction");
        junction::create(&target, &junction_path).expect("create junction decoy");
        let swapped = Arc::new(AtomicBool::new(false));
        let swapped_hook = Arc::clone(&swapped);
        let limits = Limits {
            parent_discovery_hook: Some(ParentDiscoveryHook(Arc::new(move |_path| {
                if swapped_hook.swap(true, Ordering::SeqCst) {
                    return Ok(None);
                }
                Ok(Some(junction_path.clone()))
            }))),
            ..Limits::default()
        };
        let limiter = WalkLimiter::new(&limits);
        let _seams = bind_current_limiter(&Arc::new(limiter));
        let error = open_parent_directory(&child).unwrap_err();
        assert!(
            error.to_string().contains("no longer contains")
                || error.to_string().contains("reparse")
                || error.kind() == io::ErrorKind::NotFound
                || error.kind() == io::ErrorKind::InvalidData
                || error.kind() == io::ErrorKind::InvalidInput,
            "{error}"
        );
    }

    #[test]
    fn limiter_deadline_matches_injected_instant() {
        let deadline = Instant::now() + Duration::from_millis(5);
        let limits = Limits {
            deadline: Some(deadline),
            ..Limits::default()
        };
        let limiter = WalkLimiter::new(&limits);
        assert_eq!(limiter.deadline(), deadline);
        std::thread::sleep(Duration::from_millis(10));
        assert!(matches!(
            limiter.check(&CancellationToken::new()),
            ignore::WalkState::Quit
        ));
        assert_eq!(limiter.stopped_reason(), Some("time limit reached"));
    }

    #[cfg(unix)]
    #[test]
    fn clear_errno_makes_readdir_eof_succeed() {
        unix_clear_errno();
        let err = io::Error::from_raw_os_error(libc::EIO);
        assert!(err.raw_os_error() == Some(libc::EIO));
        unix_clear_errno();
        let after = io::Error::last_os_error();
        assert_eq!(after.raw_os_error().unwrap_or(0), 0, "{after}");
    }

    #[cfg(unix)]
    #[test]
    fn foreign_sigurg_handler_fails_closed_and_is_restored() {
        const CHILD_ENV: &str = "MCODE_FS_SEARCH_SIGURG_CHILD";
        const TEST_NAME: &str =
            "builtin::fs_search::tests::foreign_sigurg_handler_fails_closed_and_is_restored";
        if let Some(mode) = std::env::var_os(CHILD_ENV) {
            // SAFETY: install a foreign disposition and verify acquisition
            // fails without replacing it.
            unsafe extern "C" fn dummy(_signal: libc::c_int) {}
            let expected = if mode == "ignore" {
                libc::SIG_IGN
            } else {
                dummy as *const () as usize
            };
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = expected;
                assert_eq!(libc::sigemptyset(&mut action.sa_mask), 0);
                action.sa_flags = 0;
                assert_eq!(
                    libc::sigaction(libc::SIGURG, &action, std::ptr::null_mut()),
                    0
                );
            }
            let error = acquire_interrupt_signal().unwrap_err();
            assert!(error.to_string().contains("another handler"), "{error}");
            unsafe {
                let mut current: libc::sigaction = std::mem::zeroed();
                assert_eq!(
                    libc::sigaction(libc::SIGURG, std::ptr::null(), &mut current),
                    0
                );
                assert_eq!(current.sa_sigaction, expected);
            }
            return;
        }
        for mode in ["handler", "ignore"] {
            let output =
                std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args(["--exact", "--test-threads", "1", TEST_NAME])
                    .env(CHILD_ENV, mode)
                    .output()
                    .expect("spawn sigurg child");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "mode={mode}\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
            assert!(
                stdout.contains("running 1 test") && stdout.contains("test result: ok. 1 passed"),
                "child must execute {mode} branch\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replaced_sigurg_is_not_restored_and_worker_stops() {
        const CHILD_ENV: &str = "MCODE_FS_SEARCH_SIGURG_REPLACE_CHILD";
        const TEST_NAME: &str =
            "builtin::fs_search::tests::replaced_sigurg_is_not_restored_and_worker_stops";
        if let Some(mode) = std::env::var_os(CHILD_ENV) {
            use std::sync::mpsc;

            unsafe extern "C" fn restart_handler(_signal: libc::c_int) {}

            let mode = mode.to_string_lossy();
            let ignored = mode.starts_with("ignore");
            let abort = mode.ends_with("abort");
            let expected_handler = if ignored {
                libc::SIG_IGN
            } else {
                restart_handler as *const () as usize
            };
            let workers = Arc::new(AtomicU64::new(0));
            let (started_tx, started_rx) = mpsc::channel();
            let (mut reader, _writer) = blocking_pipe().unwrap();
            let cancel = CancellationToken::new();
            let trigger = cancel.clone();
            let task_workers = Arc::clone(&workers);
            let task = tokio::spawn(async move {
                run_blocking_started(
                    "search",
                    &cancel,
                    Some(Instant::now() + SEARCH_TIME_LIMIT),
                    WorkerStart {
                        live_workers: Some(task_workers),
                        ..WorkerStart::default()
                    },
                    move |_| {
                        started_tx.send(()).unwrap();
                        interruptible_read(&mut reader);
                        Ok(())
                    },
                )
                .await
            });
            started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = expected_handler;
                assert_eq!(libc::sigemptyset(&mut action.sa_mask), 0);
                action.sa_flags = if ignored { 0 } else { libc::SA_RESTART };
                assert_eq!(
                    libc::sigaction(libc::SIGURG, &action, std::ptr::null_mut()),
                    0
                );
            }
            if abort {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                trigger.cancel();
                let result = tokio::time::timeout(Duration::from_secs(2), task)
                    .await
                    .expect("cancelled worker must join")
                    .expect("search task must not panic")
                    .unwrap_err();
                assert!(result.to_string().contains("cancelled"), "{result}");
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if workers.load(Ordering::Acquire) == 0 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "worker did not stop with {mode:?} SIGURG disposition"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            unsafe {
                let mut current: libc::sigaction = std::mem::zeroed();
                assert_eq!(
                    libc::sigaction(libc::SIGURG, std::ptr::null(), &mut current),
                    0
                );
                assert_eq!(
                    current.sa_sigaction, expected_handler,
                    "SignalGuard must not restore over a foreign handler"
                );
            }
            return;
        }
        for mode in [
            "restart-cancel",
            "restart-abort",
            "ignore-cancel",
            "ignore-abort",
        ] {
            let output =
                std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args(["--exact", "--test-threads", "1", TEST_NAME])
                    .env(CHILD_ENV, mode)
                    .output()
                    .expect("spawn sigurg replacement child");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "mode={mode}\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
            assert!(
                stdout.contains("running 1 test") && stdout.contains("test result: ok. 1 passed"),
                "child must execute {mode} branch\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
        }
    }

    fn pathdiff_from_to(from: &Path, to: &Path) -> PathBuf {
        let mut prefix = PathBuf::new();
        let mut current = from.to_path_buf();
        loop {
            if let Ok(suffix) = to.strip_prefix(&current) {
                prefix.push(suffix);
                return prefix;
            }
            prefix.push("..");
            if !current.pop() {
                return to.to_path_buf();
            }
        }
    }
}
