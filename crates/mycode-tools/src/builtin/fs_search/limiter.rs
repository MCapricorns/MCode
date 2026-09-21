//! Cooperative budgets, stop state, and test fault hooks for one search.
#[cfg(test)]
use std::ffi::OsStr;
use std::io;
#[cfg(all(test, windows))]
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::tool::ToolError;

use super::*;

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

    /// Maximum nesting depth, counted in path components relative to the
    /// allowed root and including the selected target itself.
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

    /// Records `bytes` actually read from ignore files for the test-only
    /// read counter. Production code does not charge a budget here; stored
    /// ignore bytes are charged separately via [`Self::add_ignore_stored`].
    pub fn record_ignore_read(&self, bytes: usize) {
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
    pub(crate) fn force_identity_error(&self) -> bool {
        self.force_identity_error.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn force_hidden_error(&self) -> bool {
        self.force_hidden_error.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn apply_open_fault(&self, name: &OsStr) -> io::Result<()> {
        let fault = self.open_fault.lock().expect("open_fault poisoned");
        if let Some(fault) = fault.as_ref() {
            return (fault.0)(name);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn child_device_override(&self, name: &OsStr) -> Option<u64> {
        let override_fn = self
            .child_device_override
            .lock()
            .expect("child_device_override poisoned");
        override_fn.as_ref().and_then(|hook| (hook.0)(name))
    }

    #[cfg(test)]
    pub(crate) fn apply_access_gate(&self, name: &OsStr, observed: ObservedOpen) -> io::Result<()> {
        let gate = self.access_gate.lock().expect("access_gate poisoned");
        if let Some(gate) = gate.as_ref() {
            return (gate.0)(name, observed);
        }
        Ok(())
    }

    #[cfg(all(test, windows))]
    pub(crate) fn apply_parent_discovery_hook(&self, path: &Path) -> io::Result<Option<PathBuf>> {
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

pub(crate) struct LimiterBindGuard {
    #[cfg(test)]
    previous: Option<Arc<WalkLimiter>>,
}

pub(crate) fn bind_current_limiter(limiter: &Arc<WalkLimiter>) -> LimiterBindGuard {
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
pub(crate) fn current_limiter<T>(f: impl FnOnce(&WalkLimiter) -> T) -> Option<T> {
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
