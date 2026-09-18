//! `grep` — secure in-process content search.
//!
//! The tool uses ripgrep's matcher/searcher libraries and a handle-relative
//! walker without spawning external executables. Walked names are opened
//! exactly once through the retained root handle, validated as stable and
//! contained, and that same [`std::fs::File`] is passed to the searcher.
//! Every source byte, including binary classification drains, is charged
//! through one atomic scan budget. Per-file matches remain provisional
//! until the same reader reaches EOF without a NUL byte; binary or
//! incompletely classified files publish nothing.
//!
//! Results are deterministic top-N by the shared path order key (lossy
//! rendered path, original `OsString` tie-break) plus line number. Matching
//! line text is not part of that order. Path keys are interned and charged
//! only while at least one retained line of that path remains in the
//! provisional or global result heap; discard, zero retained lines,
//! last-line eviction, and `max_results = 0` refund the charge. Current
//! Windows hidden bits are re-read on the opened handle before content
//! access. Ignore parse/build/read or boundary uncertainty fails closed;
//! ordinary per-file I/O produces a model-visible incomplete lower-bound
//! notice. Paths use `/`, and cancellation or future drop is supervised
//! until the worker is interrupted and joined.
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use globset::GlobMatcher;
use grep_matcher::LineTerminator;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, MmapChoice, Searcher, SearcherBuilder, Sink, SinkMatch};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

use super::fs_search::{
    EntryKind, IO_ERROR_SAMPLES, IoErrors, Limits, MAX_PATTERN_BYTES, PathOrderKey,
    REGEX_DFA_SIZE_LIMIT, REGEX_SIZE_LIMIT, ResolvedRoot, ScanReservation, SearchAccess,
    WalkLimiter, bind_search_root_with_access, display_line, io_incomplete_notice, is_hidden_skip,
    opened_file_is_hidden, rel_posix, run_blocking_until, stop_reason_error, to_posix,
    walk_retained_tree,
};

#[cfg(all(test, windows))]
use super::fs_search::windows_short_path;
#[cfg(test)]
use super::fs_search::{IGNORE_FILE_MAX_BYTES, resolve_search_root, resolve_search_root_cancel};

/// Default cap on reported matching lines.
pub const MAX_MATCHES: usize = 200;

/// The `grep` builtin.
#[derive(Debug)]
pub struct GrepTool;

/// Arguments for [`GrepTool`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GrepArgs {
    /// Pattern to search for: literal text by default, or a regular
    /// expression when `is_regex` is set.
    pub pattern: String,
    /// Interpret `pattern` as a regular expression.
    #[serde(default)]
    pub is_regex: bool,
    /// File or directory to search (relative to the session cwd).
    pub path: Option<String>,
    /// Only search files whose relative path matches this glob.
    pub include: Option<String>,
    /// Skip files whose relative path matches this glob.
    pub exclude: Option<String>,
    /// Maximum number of matching lines to report.
    pub max_results: Option<usize>,
}

struct LineMatch {
    path: Arc<PathOrderKey>,
    line_no: u64,
    line: String,
}

impl LineMatch {
    fn store_bytes(&self) -> usize {
        self.line.len().saturating_add(std::mem::size_of::<u64>())
    }
}

impl PartialEq for LineMatch {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.line_no == other.line_no
    }
}

impl Eq for LineMatch {}

impl PartialOrd for LineMatch {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LineMatch {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.path
            .cmp(&other.path)
            .then(self.line_no.cmp(&other.line_no))
    }
}

struct SearchState {
    heap: Mutex<BinaryHeap<LineMatch>>,
    /// Text matches whose classification reached EOF without NUL.
    total_matches: AtomicU64,
    /// Committed text matches plus provisional, not-yet-classified matches.
    match_slots: AtomicU64,
    /// A text file had callbacks stopped while provisional slots were full.
    count_incomplete: AtomicBool,
    match_limit: u64,
    files_searched: AtomicU64,
    lines_truncated: AtomicBool,
    io_errors: IoErrors,
    limiter: Arc<WalkLimiter>,
}

impl SearchState {
    fn new(limits: &Limits, limiter: Arc<WalkLimiter>) -> Self {
        Self {
            heap: Mutex::new(BinaryHeap::new()),
            total_matches: AtomicU64::new(0),
            match_slots: AtomicU64::new(0),
            count_incomplete: AtomicBool::new(false),
            match_limit: limits.count_budget,
            files_searched: AtomicU64::new(0),
            lines_truncated: AtomicBool::new(false),
            io_errors: IoErrors::new(IO_ERROR_SAMPLES),
            limiter,
        }
    }

    /// Atomically reserves one callback slot. The returned value is the
    /// number of committed-plus-provisional slots after reservation.
    fn reserve_match(&self) -> Option<u64> {
        loop {
            let current = self.match_slots.load(Ordering::Acquire);
            if current >= self.match_limit {
                return None;
            }
            match self.match_slots.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(current + 1),
                Err(_) => continue,
            }
        }
    }

    fn release_provisional(&self, count: u64) {
        if count > 0 {
            self.match_slots.fetch_sub(count, Ordering::AcqRel);
        }
    }

    fn settle_text(&self, count: u64) -> u64 {
        self.total_matches.fetch_add(count, Ordering::AcqRel) + count
    }
}

struct FileSink<'a> {
    path: Arc<PathOrderKey>,
    lines: Vec<LineMatch>,
    binary: bool,
    reserved_matches: u64,
    callbacks_stopped: bool,
    line_truncated: bool,
    settled: bool,
    path_charged: bool,
    state: &'a SearchState,
    cap: usize,
    limits: &'a Limits,
}

impl FileSink<'_> {
    fn matched_event(&mut self, matched: &SinkMatch<'_>) -> bool {
        let Some(slots_after) = self.state.reserve_match() else {
            self.callbacks_stopped = true;
            return false;
        };
        self.reserved_matches += 1;

        if self.lines.len() < self.cap {
            let mut bytes = matched.bytes();
            if bytes.last() == Some(&b'\n') {
                bytes = &bytes[..bytes.len() - 1];
            }
            if bytes.last() == Some(&b'\r') {
                bytes = &bytes[..bytes.len() - 1];
            }
            let mut truncated = false;
            let line = display_line(bytes, self.limits.line_bytes, &mut truncated);
            self.line_truncated |= truncated;
            let candidate = LineMatch {
                path: Arc::clone(&self.path),
                line_no: matched.line_number().unwrap_or(0),
                line,
            };
            if self
                .state
                .limiter
                .try_reserve_result_bytes(candidate.store_bytes())
            {
                if self.charge_path() {
                    self.lines.push(candidate);
                } else {
                    self.state
                        .limiter
                        .release_result_bytes(candidate.store_bytes());
                }
            }
        }

        if slots_after >= self.state.match_limit {
            self.callbacks_stopped = true;
            false
        } else {
            true
        }
    }

    fn discard(mut self) {
        self.refund_retained();
        self.state.release_provisional(self.reserved_matches);
        self.settled = true;
    }

    fn commit_text(mut self) {
        let total = self.state.settle_text(self.reserved_matches);
        if self.callbacks_stopped {
            self.state.count_incomplete.store(true, Ordering::Release);
        }
        if self.line_truncated {
            self.state.lines_truncated.store(true, Ordering::Release);
        }
        self.state.files_searched.fetch_add(1, Ordering::Relaxed);

        let lines = std::mem::take(&mut self.lines);
        if lines.is_empty() {
            self.release_path_charge();
        } else {
            let mut heap = self.state.heap.lock().expect("grep results lock poisoned");
            for line in lines {
                if heap.len() < self.cap {
                    heap.push(line);
                } else if heap.peek().is_some_and(|worst| line < *worst) {
                    if let Some(evicted) = heap.pop() {
                        self.state
                            .limiter
                            .release_result_bytes(evicted.store_bytes());
                        if !Arc::ptr_eq(&evicted.path, &self.path)
                            && !heap_has_path(&heap, &evicted.path)
                        {
                            self.state
                                .limiter
                                .release_result_bytes(evicted.path.store_bytes());
                        }
                    }
                    heap.push(line);
                } else {
                    self.state.limiter.release_result_bytes(line.store_bytes());
                }
            }
            if self.path_charged && !heap_has_path(&heap, &self.path) {
                self.release_path_charge();
            }
        }
        self.settled = true;

        if self.callbacks_stopped && total >= self.state.match_limit {
            self.state.limiter.stop("match-count budget reached");
        }
    }

    fn charge_path(&mut self) -> bool {
        if self.path_charged {
            return true;
        }
        if !self
            .state
            .limiter
            .try_reserve_result_bytes(self.path.store_bytes())
        {
            return false;
        }
        self.path_charged = true;
        true
    }

    fn release_path_charge(&mut self) {
        if self.path_charged {
            self.state
                .limiter
                .release_result_bytes(self.path.store_bytes());
            self.path_charged = false;
        }
    }

    fn refund_retained(&mut self) {
        for line in self.lines.drain(..) {
            self.state.limiter.release_result_bytes(line.store_bytes());
        }
        self.release_path_charge();
    }
}

fn heap_has_path(heap: &BinaryHeap<LineMatch>, path: &Arc<PathOrderKey>) -> bool {
    heap.iter().any(|line| Arc::ptr_eq(&line.path, path))
}

impl Drop for FileSink<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.refund_retained();
            self.state.release_provisional(self.reserved_matches);
            self.settled = true;
        }
    }
}

impl Sink for FileSink<'_> {
    type Error = io::Error;

    fn matched(&mut self, _searcher: &Searcher, matched: &SinkMatch<'_>) -> io::Result<bool> {
        Ok(self.matched_event(matched))
    }

    fn binary_data(&mut self, _searcher: &Searcher, _offset: u64) -> io::Result<bool> {
        self.binary = true;
        Ok(false)
    }
}

#[cfg(test)]
type BeforeOpenHook = Arc<dyn Fn(&Path) + Send + Sync>;

#[derive(Clone, Default)]
struct SearchHooks {
    #[cfg(test)]
    before_open: Option<BeforeOpenHook>,
}

impl SearchHooks {
    fn before_open(&self, path: &Path) {
        #[cfg(test)]
        if let Some(hook) = &self.before_open {
            hook(path);
        }
        #[cfg(not(test))]
        let _ = path;
    }
}

#[async_trait]
impl Tool for GrepTool {
    type Args = GrepArgs;
    type Output = ();

    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents under a directory (or the session cwd). Literal \
         text by default, regex with is_regex; optional include/exclude globs. \
         Reports up to 200 matching lines as `path:line:text`, with a notice \
         when more matches exist. Hidden and gitignored files are skipped."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some("grep: search file contents (pattern, optional is_regex/path/include/exclude).")
    }

    fn search_access(&self) -> Option<SearchAccess> {
        Some(SearchAccess::Content)
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        reject_pattern_bytes(&args.pattern, "pattern")?;
        if let Some(include) = args.include.as_deref() {
            reject_pattern_bytes(include, "include glob")?;
        }
        if let Some(exclude) = args.exclude.as_deref() {
            reject_pattern_bytes(exclude, "exclude glob")?;
        }
        let cwd = ctx.cwd.clone();
        let path = args.path;
        let pattern = args.pattern;
        let is_regex = args.is_regex;
        let include_pat = args.include;
        let exclude_pat = args.exclude;
        let max_results = args.max_results;
        let deadline = Instant::now() + Limits::default().time_limit;
        let limits = Limits {
            deadline: Some(deadline),
            ..Limits::default()
        };
        let cancel = ctx.cancel.clone();
        let prepared = ctx.prepared_search.clone();
        run_blocking_until("search", &cancel, deadline, move |worker_cancel| {
            if worker_cancel.is_cancelled() {
                return Err(ToolError::Execution(
                    "search cancelled before completion".to_owned(),
                ));
            }
            let matcher = compile_matcher(&pattern, is_regex)?;
            let include = compile_glob(include_pat.as_deref(), "include")?;
            let exclude = compile_glob(exclude_pat.as_deref(), "exclude")?;
            let root = bind_search_root_with_access(
                prepared.as_deref(),
                &cwd,
                path.as_deref(),
                &worker_cancel,
                &limits,
                SearchAccess::Content,
            )?;
            run_search(
                matcher,
                root,
                include,
                exclude,
                max_results,
                &worker_cancel,
                &limits,
            )
        })
        .await
    }
}

fn run_search(
    matcher: RegexMatcher,
    root: ResolvedRoot,
    include: Option<GlobMatcher>,
    exclude: Option<GlobMatcher>,
    max_results: Option<usize>,
    cancel: &CancellationToken,
    limits: &Limits,
) -> Result<ToolResult, ToolError> {
    run_search_core(
        matcher,
        root,
        include,
        exclude,
        max_results,
        cancel,
        limits,
        &SearchHooks::default(),
    )
}

#[cfg(test)]
#[expect(
    clippy::too_many_arguments,
    reason = "test hook mirrors the production search inputs"
)]
fn run_search_with_hooks(
    matcher: RegexMatcher,
    root: ResolvedRoot,
    include: Option<GlobMatcher>,
    exclude: Option<GlobMatcher>,
    max_results: Option<usize>,
    cancel: &CancellationToken,
    limits: &Limits,
    hooks: &SearchHooks,
) -> Result<ToolResult, ToolError> {
    run_search_core(
        matcher,
        root,
        include,
        exclude,
        max_results,
        cancel,
        limits,
        hooks,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "search policy inputs remain explicit and immutable"
)]
fn run_search_core(
    matcher: RegexMatcher,
    mut root: ResolvedRoot,
    include: Option<GlobMatcher>,
    exclude: Option<GlobMatcher>,
    max_results: Option<usize>,
    cancel: &CancellationToken,
    limits: &Limits,
    hooks: &SearchHooks,
) -> Result<ToolResult, ToolError> {
    let cap = max_results
        .unwrap_or(MAX_MATCHES)
        .min(limits.stored_ceiling);
    let state = Arc::new(SearchState::new(limits, Arc::clone(&root.limiter)));
    let report_root = root.root.clone();

    match root.target_is_skipped() {
        Ok(true) => return finish_search_report(&report_root, &state, cap, limits),
        Ok(false) => {}
        Err(error) if is_hidden_skip(&error) => {
            return finish_search_report(&report_root, &state, cap, limits);
        }
        Err(error) => {
            return Ok(ToolResult::error(format!(
                "search target hidden check failed: {error}"
            )));
        }
    }

    if root.is_file() {
        let relative = rel_posix(&root.root, &root.root);
        search_one_file(
            &matcher, &mut root, &relative, &include, &exclude, &state, cap, cancel, limits,
        );
        // `root` and the allowed-root identity handle remain alive through
        // report assembly.
        finish_search_report(&report_root, &state, cap, limits)
    } else {
        let root_guard = Arc::new(root);
        if let Err(error) = walk_and_search(
            &matcher,
            Arc::clone(&root_guard),
            &include,
            &exclude,
            &state,
            cap,
            cancel,
            limits,
            hooks,
        ) {
            return Ok(ToolResult::error(format!(
                "search ignore boundary could not be established: {error}"
            )));
        }
        // Assemble before `root_guard` drops, retaining the root identity for
        // the complete operation rather than only walker enumeration.
        finish_search_report(&report_root, &state, cap, limits)
    }
}

fn finish_search_report(
    root: &Path,
    state: &Arc<SearchState>,
    cap: usize,
    limits: &Limits,
) -> Result<ToolResult, ToolError> {
    if let Some(error) = stop_reason_error("search", &state.limiter) {
        return Err(error);
    }
    Ok(assemble_report(root, state, cap, limits))
}

#[expect(
    clippy::too_many_arguments,
    reason = "walker workers need explicit shared search policy"
)]
fn walk_and_search(
    matcher: &RegexMatcher,
    root: Arc<ResolvedRoot>,
    include: &Option<GlobMatcher>,
    exclude: &Option<GlobMatcher>,
    state: &Arc<SearchState>,
    cap: usize,
    cancel: &CancellationToken,
    limits: &Limits,
    hooks: &SearchHooks,
) -> io::Result<()> {
    let mut searcher = build_searcher(limits);
    walk_retained_tree(
        &root,
        &state.limiter,
        cancel,
        &state.io_errors,
        |relative_path, name, kind, parent| {
            if matches!(state.limiter.check(cancel), ignore::WalkState::Quit) {
                return ignore::WalkState::Quit;
            }
            if kind != EntryKind::File {
                return ignore::WalkState::Continue;
            }
            let relative = to_posix(relative_path);
            if include
                .as_ref()
                .is_some_and(|glob| !glob.is_match(&relative))
                || exclude
                    .as_ref()
                    .is_some_and(|glob| glob.is_match(&relative))
            {
                return ignore::WalkState::Continue;
            }

            // The test hook is deliberately after enumeration and before the
            // one secure open, making name-replacement races deterministic.
            hooks.before_open(&root.root.join(relative_path));
            let mut file = match root.open_walked(parent, name, EntryKind::File) {
                Ok(file) => file,
                Err(error) => {
                    if !is_hidden_skip(&error) {
                        state.io_errors.record(&relative, &error);
                    }
                    return if state.limiter.quit.load(Ordering::Acquire) {
                        ignore::WalkState::Quit
                    } else {
                        ignore::WalkState::Continue
                    };
                }
            };
            let path = PathOrderKey::from_rendered_and_raw(relative, relative_path.as_os_str());
            if let Err(error) = search_open_file(
                &mut searcher,
                matcher,
                &mut file,
                path,
                state,
                cap,
                cancel,
                limits,
            ) && !state.limiter.quit.load(Ordering::Acquire)
                && !cancel.is_cancelled()
            {
                state.io_errors.record(&to_posix(relative_path), &error);
            }
            if state.limiter.quit.load(Ordering::Acquire) {
                ignore::WalkState::Quit
            } else {
                ignore::WalkState::Continue
            }
        },
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "single-file behavior mirrors walker policy"
)]
fn search_one_file(
    matcher: &RegexMatcher,
    root: &mut ResolvedRoot,
    relative: &str,
    include: &Option<GlobMatcher>,
    exclude: &Option<GlobMatcher>,
    state: &Arc<SearchState>,
    cap: usize,
    cancel: &CancellationToken,
    limits: &Limits,
) {
    if matches!(state.limiter.check(cancel), ignore::WalkState::Quit)
        || include
            .as_ref()
            .is_some_and(|glob| !glob.is_match(relative))
        || exclude.as_ref().is_some_and(|glob| glob.is_match(relative))
    {
        return;
    }
    let mut searcher = build_searcher(limits);
    let raw = root.root.as_os_str().to_os_string();
    let file = match root.target_file_mut() {
        Ok(file) => file,
        Err(error) => {
            state.io_errors.record(relative, &error);
            return;
        }
    };
    if let Err(error) = search_open_file(
        &mut searcher,
        matcher,
        file,
        PathOrderKey::from_rendered_and_raw(relative.to_owned(), raw),
        state,
        cap,
        cancel,
        limits,
    ) && !state.limiter.quit.load(Ordering::Acquire)
        && !cancel.is_cancelled()
    {
        state.io_errors.record(relative, &error);
    }
}

fn build_searcher(limits: &Limits) -> Searcher {
    SearcherBuilder::new()
        .line_number(true)
        .line_terminator(LineTerminator::crlf())
        .binary_detection(BinaryDetection::quit(0))
        .memory_map(MmapChoice::never())
        .heap_limit(Some(limits.line_heap))
        .build()
}

struct PolledReader<'a> {
    file: &'a mut File,
    state: &'a SearchState,
    cancel: &'a CancellationToken,
    scan_cap: u64,
    eof_seen: bool,
    nul_seen: bool,
}

impl PolledReader<'_> {
    fn drain_classification(&mut self) -> io::Result<()> {
        let mut buffer = [0u8; 8 * 1024];
        while !self.eof_seen && !self.nul_seen {
            let bytes = self.read(&mut buffer)?;
            if bytes == 0 {
                break;
            }
        }
        Ok(())
    }
}

impl Read for PolledReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            if matches!(
                self.state.limiter.check(self.cancel),
                ignore::WalkState::Quit
            ) {
                return Err(io::Error::other("search stopped early"));
            }
            match self.state.limiter.reserve_scan(buffer.len(), self.scan_cap) {
                ScanReservation::Granted(reserved) => {
                    if let Err(error) = super::fs_search::wait_for_worker_readable(self.file) {
                        self.state.limiter.settle_scan(reserved, 0);
                        return Err(error);
                    }
                    let result = self.file.read(&mut buffer[..reserved]);
                    match result {
                        Ok(actual) => {
                            self.state.limiter.settle_scan(reserved, actual);
                            if actual == 0 {
                                self.eof_seen = true;
                            } else {
                                self.nul_seen |= buffer[..actual].contains(&0);
                            }
                            return Ok(actual);
                        }
                        Err(error) => {
                            self.state.limiter.settle_scan(reserved, 0);
                            return Err(error);
                        }
                    }
                }
                ScanReservation::Pending => std::thread::yield_now(),
                ScanReservation::Exhausted => {
                    // Reaching the hard cap is itself an early stop. We do
                    // not use metadata as a synthetic EOF: only an actual
                    // zero-byte read can finish text classification.
                    self.state.limiter.stop("scanned-bytes limit reached");
                    return Err(io::Error::other("search stopped early"));
                }
            }
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "opened-file processing receives all immutable limits explicitly"
)]
fn search_open_file(
    searcher: &mut Searcher,
    matcher: &RegexMatcher,
    file: &mut File,
    path: PathOrderKey,
    state: &SearchState,
    cap: usize,
    cancel: &CancellationToken,
    limits: &Limits,
) -> io::Result<()> {
    let path = Arc::new(path);
    let mut sink = FileSink {
        path,
        lines: Vec::new(),
        binary: false,
        reserved_matches: 0,
        callbacks_stopped: false,
        line_truncated: false,
        settled: false,
        path_charged: false,
        state,
        cap,
        limits,
    };
    let (nul_seen, eof_seen) = {
        let mut reader = PolledReader {
            file,
            state,
            cancel,
            scan_cap: limits.scan_bytes,
            eof_seen: false,
            nul_seen: false,
        };

        let search_result = searcher.search_reader(matcher.clone(), &mut reader, &mut sink);
        if reader.nul_seen || sink.binary {
            sink.discard();
            return Ok(());
        }
        search_result?;

        // A sink stop (count budget) can return success before EOF. Finish only
        // binary classification through this reader and the same byte budget;
        // no matcher callbacks are made during the drain.
        if !reader.eof_seen {
            reader.drain_classification()?;
        }
        (reader.nul_seen, reader.eof_seen)
    };
    if nul_seen {
        sink.discard();
    } else if eof_seen {
        if opened_file_is_hidden(file)? {
            sink.discard();
            return Ok(());
        }
        sink.commit_text();
    } else {
        return Err(io::Error::other("file classification did not reach EOF"));
    }
    Ok(())
}

fn assemble_report(
    root: &Path,
    state: &Arc<SearchState>,
    cap: usize,
    limits: &Limits,
) -> ToolResult {
    let heap = std::mem::take(&mut *state.heap.lock().expect("grep results lock poisoned"));
    let matches = heap.into_sorted_vec();

    let mut text = String::new();
    let mut shown = 0usize;
    let mut output_truncated = false;
    for matched in &matches {
        let entry = format!(
            "{}:{}:{}",
            matched.path.rendered(),
            matched.line_no,
            matched.line
        );
        if text.len() + entry.len() + 1 > limits.output_bytes {
            output_truncated = true;
            break;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&entry);
        shown += 1;
    }

    let total = state.total_matches.load(Ordering::Acquire);
    let count_incomplete = state.count_incomplete.load(Ordering::Acquire);
    let stop_reason = state
        .limiter
        .stopped_reason()
        .or(count_incomplete.then_some("match-count budget reached"))
        .or(state
            .limiter
            .result_store_truncated()
            .then_some("result store limit reached"));
    let (io_count, io_samples) = state.io_errors.summary();
    let exact = stop_reason.is_none() && io_count == 0;
    let truncated = total > shown as u64 || !exact;

    if truncated {
        let count = if exact {
            total.to_string()
        } else {
            format!("at least {total}")
        };
        let reason = stop_reason
            .map(|reason| format!("; stopped early: {reason}"))
            .unwrap_or_default();
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!(
            "[showing first {shown} of {count} matching lines; narrow the pattern or raise max_results{reason}]"
        ));
    }
    if state.lines_truncated.load(Ordering::Acquire) {
        text.push_str(&format!(
            "\n[some matching lines truncated to {} bytes; use read to see the full line]",
            limits.line_bytes
        ));
    }
    if output_truncated {
        text.push_str(&format!(
            "\n[output truncated at {} bytes; narrow the search or lower max_results]",
            limits.output_bytes
        ));
    }

    if let Some(notice) = io_incomplete_notice(io_count) {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&notice);
    }

    let mut details = json!({
        "root": root.display().to_string(),
        "files_searched": state.files_searched.load(Ordering::Relaxed),
        "matches": total,
        "shown": shown,
        "truncated": truncated,
    });
    if !exact {
        details["matches_lower_bound"] = json!(true);
    }
    if let Some(reason) = stop_reason {
        details["stopped_early"] = json!(reason);
    }
    if state.lines_truncated.load(Ordering::Acquire) {
        details["lines_truncated"] = json!(true);
    }
    if output_truncated {
        details["output_truncated"] = json!(true);
    }
    if io_count > 0 {
        details["io_error_count"] = json!(io_count);
        details["io_errors"] = json!(io_samples);
    }

    debug_assert!(matches.len() <= cap);
    debug_assert!(shown <= cap);
    ToolResult::text(text).with_details(details)
}

fn reject_pattern_bytes(pattern: &str, label: &str) -> Result<(), ToolError> {
    if pattern.as_bytes().contains(&0) {
        return Err(ToolError::InvalidArgs(format!(
            "{label} contains a NUL byte"
        )));
    }
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "{label} exceeds {MAX_PATTERN_BYTES} bytes"
        )));
    }
    Ok(())
}

fn compile_matcher(pattern: &str, is_regex: bool) -> Result<RegexMatcher, ToolError> {
    reject_pattern_bytes(pattern, "pattern")?;
    RegexMatcherBuilder::new()
        .fixed_strings(!is_regex)
        .crlf(true)
        .ban_byte(Some(0))
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
        .build(pattern)
        .map_err(|error| ToolError::InvalidArgs(format!("invalid regex: {error}")))
}

fn compile_glob(glob: Option<&str>, label: &str) -> Result<Option<GlobMatcher>, ToolError> {
    match glob {
        None => Ok(None),
        Some(pattern) => {
            reject_pattern_bytes(pattern, &format!("{label} glob"))?;
            globset::Glob::new(pattern)
                .map(|glob| Some(glob.compile_matcher()))
                .map_err(|error| {
                    ToolError::InvalidArgs(format!("invalid {label} glob `{pattern}`: {error}"))
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::test_support::{ctx_at, run_dyn, text_of, unwrap_tool};
    use serde_json::json;
    use std::path::Path;
    use tokio_util::sync::CancellationToken;

    fn assert_no_diagnostic_needle(result: &ToolResult, needle: &str) {
        if let Some(details) = result.details.as_ref() {
            let rendered = details.to_string();
            assert!(!rendered.contains(needle), "{details}");
        }
    }

    fn fixture(dir: &Path) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {\n    // hello\n}\n").unwrap();
        std::fs::write(
            dir.join("src/util.rs"),
            "// hello from util\npub fn x() {}\n",
        )
        .unwrap();
        std::fs::write(dir.join("docs/notes.md"), "# notes\nhello world\n").unwrap();
        std::fs::write(dir.join("binary.bin"), b"\xff\xfe\x00hello").unwrap();
    }

    #[tokio::test]
    async fn literal_search_finds_matches_with_rel_paths() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&GrepTool, json!({"pattern": "hello"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(text.contains("docs/notes.md:2:hello world"), "{text}");
        assert!(text.contains("src/main.rs:2:    // hello"), "{text}");
        assert!(text.contains("src/util.rs:1:// hello from util"), "{text}");
        // The binary file is skipped silently.
        assert!(!text.contains("binary.bin"), "{text}");
        assert!(!result.is_error);

        let details = result.details.unwrap();
        assert_eq!(details["matches"], 3);
        assert_eq!(details["files_searched"], 3);
    }

    #[tokio::test]
    async fn literal_patterns_do_not_act_as_regex() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        // "h.llo" as a literal must not match "hello".
        let result = run_dyn(&GrepTool, json!({"pattern": "h.llo"}), &ctx)
            .await
            .unwrap();
        assert_eq!(text_of(&result), "");
        assert_eq!(result.details.unwrap()["matches"], 0);
    }

    #[tokio::test]
    async fn regex_mode_matches_patterns() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        let result = run_dyn(
            &GrepTool,
            json!({"pattern": "hello (world|from)", "is_regex": true}),
            &ctx,
        )
        .await
        .unwrap();
        let text = text_of(&result);
        assert!(text.contains("hello world"), "{text}");
        assert!(text.contains("hello from"), "{text}");
    }

    #[tokio::test]
    async fn invalid_regex_is_invalid_args() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());

        let err = run_dyn(
            &GrepTool,
            json!({"pattern": "(unclosed", "is_regex": true}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn include_glob_filters_to_matching_files() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        let result = run_dyn(
            &GrepTool,
            json!({"pattern": "hello", "include": "*.rs"}),
            &ctx,
        )
        .await
        .unwrap();
        let text = text_of(&result);
        // "*.rs" matches nested paths too (globset `*` crosses `/`).
        assert!(text.contains("src/main.rs"), "{text}");
        assert!(text.contains("src/util.rs"), "{text}");
        assert!(!text.contains("notes.md"), "{text}");
    }

    #[tokio::test]
    async fn exclude_glob_skips_matching_files() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        let result = run_dyn(
            &GrepTool,
            json!({"pattern": "hello", "exclude": "*.md"}),
            &ctx,
        )
        .await
        .unwrap();
        let text = text_of(&result);
        assert!(!text.contains("notes.md"), "{text}");
        assert!(text.contains("src/util.rs"), "{text}");
    }

    #[tokio::test]
    async fn result_cap_reports_total_with_notice() {
        let dir = tempfile::tempdir().unwrap();
        let many: Vec<String> = (1..=205).map(|i| format!("hit {i}")).collect();
        std::fs::write(dir.path().join("many.txt"), many.join("\n")).unwrap();
        let ctx = ctx_at(dir.path());

        // Default cap.
        let result = run_dyn(&GrepTool, json!({"pattern": "hit"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(
            text.contains("[showing first 200 of 205 matching lines"),
            "{text}"
        );
        let details = result.details.unwrap();
        assert_eq!(details["matches"], 205);
        assert_eq!(details["shown"], 200);
        assert_eq!(details["truncated"], true);

        // Custom cap via max_results.
        let result = run_dyn(&GrepTool, json!({"pattern": "hit", "max_results": 5}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(text.contains("[showing first 5 of 205"), "{text}");
        assert_eq!(text.lines().filter(|l| !l.starts_with('[')).count(), 5);
    }

    #[tokio::test]
    async fn path_can_target_a_single_file() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        let result = run_dyn(
            &GrepTool,
            json!({"pattern": "hello", "path": "src/util.rs"}),
            &ctx,
        )
        .await
        .unwrap();
        let text = text_of(&result);
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("hello from util"), "{text}");
        assert_eq!(result.details.unwrap()["files_searched"], 1);
    }

    #[tokio::test]
    async fn path_can_target_a_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&GrepTool, json!({"pattern": "hello", "path": "docs"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(text.contains("notes.md:2:hello world"), "{text}");
        assert!(!text.contains("main.rs"), "{text}");
    }

    #[tokio::test]
    async fn nonexistent_path_is_an_execution_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());

        // Missing directory...
        let err = run_dyn(
            &GrepTool,
            json!({"pattern": "x", "path": "no/such/dir"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)), "{err}");

        // ...and missing single-file target.
        let err = run_dyn(
            &GrepTool,
            json!({"pattern": "x", "path": "no-such-file.txt"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)), "{err}");
    }

    #[tokio::test]
    async fn no_matches_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&GrepTool, json!({"pattern": "zzz-nothing"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(text_of(&result), "");
        assert_eq!(result.details.unwrap()["matches"], 0);
    }

    #[tokio::test]
    async fn malformed_include_glob_is_invalid_args() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());

        let err = run_dyn(
            &GrepTool,
            json!({"pattern": "x", "include": "[unclosed"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    // ---- new coverage: in-process engine, caps, cancellation, safety ----

    /// No external rg/fd binary is involved anywhere: the default-path
    /// (and empty-path) searches must work purely in-process.
    #[tokio::test]
    async fn default_and_empty_path_work_without_external_binaries() {
        use crate::builtin::fs_search::MAX_LINE_BYTES;
        let _ = MAX_LINE_BYTES; // (covered by the long-line test below)
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&GrepTool, json!({"pattern": "hello"}), &ctx)
            .await
            .unwrap();
        assert!(result.details.unwrap()["matches"].as_u64().unwrap() > 0);

        // Explicit empty string also means "the whole cwd".
        let result = run_dyn(&GrepTool, json!({"pattern": "hello", "path": ""}), &ctx)
            .await
            .unwrap();
        assert!(result.details.unwrap()["matches"].as_u64().unwrap() > 0);

        // "." is the same default root after lexical normalization.
        let result = run_dyn(&GrepTool, json!({"pattern": "hello", "path": "."}), &ctx)
            .await
            .unwrap();
        assert!(result.details.unwrap()["matches"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn crlf_files_line_numbers_without_stray_cr() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("crlf.txt"),
            "one\r\nhello world\r\nthree\r\n",
        )
        .unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&GrepTool, json!({"pattern": "hello world"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert_eq!(text.lines().count(), 1, "{text}");
        let line = text.lines().next().unwrap();
        assert!(line.starts_with("crlf.txt:2:"), "{text}");
        assert!(!line.contains('\r'), "{text:?}");
        assert_eq!(result.details.unwrap()["matches"], 1);
    }

    #[tokio::test]
    async fn binary_file_with_match_before_nul_is_skipped_wholesale() {
        let dir = tempfile::tempdir().unwrap();
        // The match appears *before* the NUL; the file is still treated
        // as binary and skipped entirely (old non-UTF-8 semantics).
        std::fs::write(dir.path().join("late.bin"), b"hello\x00world").unwrap();
        std::fs::write(dir.path().join("ok.txt"), "hello\n").unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&GrepTool, json!({"pattern": "hello"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(text.contains("ok.txt:1:hello"), "{text}");
        assert!(!text.contains("late.bin"), "{text}");
        let details = result.details.unwrap();
        assert_eq!(details["matches"], 1);
        assert_eq!(details["files_searched"], 1);
    }

    #[tokio::test]
    async fn long_lines_are_truncated_with_notice() {
        use crate::builtin::fs_search::MAX_LINE_BYTES;
        let dir = tempfile::tempdir().unwrap();
        let long_line = format!("hit {}", "x".repeat(200_000));
        std::fs::write(
            dir.path().join("long.txt"),
            format!("{long_line}\nplain hit\n"),
        )
        .unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&GrepTool, json!({"pattern": "hit"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        let first = text.lines().next().unwrap();
        assert!(first.starts_with("long.txt:1:hit "), "{text}");
        // Line body capped at MAX_LINE_BYTES (path/lineno prefix aside).
        assert!(
            first.len() < "long.txt:1:".len() + MAX_LINE_BYTES + 16,
            "{first}"
        );
        assert!(!first.contains(&"x".repeat(600)), "{first}");
        assert!(text.contains("plain hit"), "{text}");
        assert!(
            text.contains("[some matching lines truncated to 500 bytes"),
            "{text}"
        );
        assert_eq!(result.details.unwrap()["lines_truncated"], true);
    }

    #[tokio::test]
    async fn unicode_filenames_and_content_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("日本語");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("ünïcode.md"), "héllo wörld\n").unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&GrepTool, json!({"pattern": "héllo wörld"}), &ctx)
            .await
            .unwrap();
        assert_eq!(
            text_of(&result).as_bytes(),
            "日本語/ünïcode.md:1:héllo wörld".as_bytes()
        );
        let details = result.details.unwrap();
        assert_eq!(details["files_searched"], 1);
        assert_eq!(details["matches"], 1);
        assert_eq!(details["shown"], 1);
        assert_eq!(details["truncated"], false);
    }

    #[tokio::test]
    async fn gitignore_and_hidden_files_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        // The ignore crate honors .gitignore only inside a git repo; an
        // empty .git marker directory is enough for detection.
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "hello ignored\n").unwrap();
        std::fs::write(dir.path().join(".hidden.txt"), "hello hidden\n").unwrap();
        std::fs::write(dir.path().join("kept.txt"), "hello kept\n").unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&GrepTool, json!({"pattern": "hello"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(text.contains("kept.txt:1:hello kept"), "{text}");
        assert!(!text.contains("ignored.txt"), "{text}");
        assert!(!text.contains("hidden.txt"), "{text}");
        assert_eq!(result.details.unwrap()["files_searched"], 1);
    }

    #[tokio::test]
    async fn zero_width_pattern_matches_lines_without_hanging() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("z.txt"), "aaa\nbbb\n\n").unwrap();
        let ctx = ctx_at(dir.path());

        // Zero-width patterns (empty literal, or a regex like "b*"
        // that matches empty) must not hang or error; zero-width line
        // matches are reported as line matches, like the old engine.
        let empty = run_dyn(&GrepTool, json!({"pattern": ""}), &ctx)
            .await
            .unwrap();
        let text = text_of(&empty).to_owned();
        let details = empty.details.unwrap();
        // "aaa", "bbb" and the trailing empty line all match.
        assert_eq!(details["matches"], 3, "{text}");

        let star = run_dyn(&GrepTool, json!({"pattern": "b*", "is_regex": true}), &ctx)
            .await
            .unwrap();
        let star_text = text_of(&star).to_owned();
        let star_matches = star.details.unwrap()["matches"].clone();
        // "aaa", "bbb" and the trailing empty line all match.
        assert_eq!(star_matches, 3, "{star_text}");
    }

    #[tokio::test]
    async fn parallel_multi_dir_results_are_sorted_and_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        for (d, n) in [("b", 5), ("a", 5), ("c", 5)] {
            let sub = dir.path().join(d);
            std::fs::create_dir_all(&sub).unwrap();
            for i in 0..n {
                std::fs::write(sub.join(format!("f{i}.txt")), format!("hello {d} {i}\n")).unwrap();
            }
        }
        let ctx = ctx_at(dir.path());

        let r1 = run_dyn(&GrepTool, json!({"pattern": "hello"}), &ctx)
            .await
            .unwrap();
        let first = text_of(&r1).to_owned();
        let r2 = run_dyn(&GrepTool, json!({"pattern": "hello"}), &ctx)
            .await
            .unwrap();
        let second = text_of(&r2).to_owned();
        assert_eq!(first, second, "parallel walks must sort deterministically");
        let paths: Vec<&str> = first
            .lines()
            .map(|l| l.split(':').next().unwrap())
            .collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted, "{first}");
        assert_eq!(paths.len(), 15);
    }

    /// Truncation keeps the smallest (path, line) keys — the first N of the
    /// fully sorted result — regardless of directory enumeration order.
    #[tokio::test]
    async fn truncation_keeps_lowest_keys_deterministically() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["z.txt", "a.txt", "m.txt"] {
            std::fs::write(dir.path().join(name), "hit\n").unwrap();
        }
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&GrepTool, json!({"pattern": "hit", "max_results": 2}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        let body: Vec<&str> = text.lines().filter(|l| !l.starts_with('[')).collect();
        assert_eq!(body, vec!["a.txt:1:hit", "m.txt:1:hit"], "{text}");
        let details = result.details.unwrap();
        assert_eq!(details["shown"], 2, "{details}");
        assert_eq!(details["matches"], 3, "{details}");
        assert_eq!(details["truncated"], true, "{details}");
    }

    /// A binary file (matches before the NUL byte) buffers only
    /// locally and is discarded wholesale at commit — it never
    /// occupies global result slots, so it cannot evict other text
    /// matches.
    #[tokio::test]
    async fn binary_files_cannot_evict_text_matches() {
        let dir = tempfile::tempdir().unwrap();
        let mut bin = Vec::new();
        for _ in 0..10 {
            bin.extend_from_slice(b"hit\n");
        }
        bin.push(0); // NUL: the file is binary and skipped wholesale
        bin.extend_from_slice(b"tail");
        std::fs::write(dir.path().join("b.bin"), &bin).unwrap();
        std::fs::write(dir.path().join("a.txt"), "hit\n").unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&GrepTool, json!({"pattern": "hit", "max_results": 1}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(text.contains("a.txt:1:hit"), "{text}");
        assert!(!text.contains("b.bin"), "{text}");
        let details = result.details.unwrap();
        assert_eq!(details["matches"], 1, "{details}");
        assert_eq!(details["files_searched"], 1, "{details}");
        assert_eq!(details["truncated"], false, "{details}");
    }

    #[tokio::test]
    async fn cancellation_token_aborts_the_search() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let ctx = ToolCtx::new(dir.path()).with_cancel(cancel);

        let err = run_dyn(&GrepTool, json!({"pattern": "hello"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)));
        assert!(err.to_string().contains("cancelled"), "{err}");
    }

    #[tokio::test]
    async fn path_escape_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());

        // Relative `..` escape.
        let err = run_dyn(
            &GrepTool,
            json!({"pattern": "x", "path": "../outside"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
        assert!(err.to_string().contains("escapes"), "{err}");

        // Absolute path outside the session cwd.
        let outside = dir.path().parent().unwrap().to_path_buf();
        let err = run_dyn(
            &GrepTool,
            json!({"pattern": "x", "path": outside.display().to_string()}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");

        // `..` that resolves back inside is fine (must exist, too).
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        let result = run_dyn(
            &GrepTool,
            json!({"pattern": "x", "path": "docs/../docs"}),
            &ctx,
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_root_escape_is_rejected() {
        use std::os::unix::fs::symlink;

        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "top secret\n").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("in.txt"), "hello\n").unwrap();
        symlink(outside.path(), dir.path().join("leak")).unwrap();
        let ctx = ctx_at(dir.path());

        let err = run_dyn(
            &GrepTool,
            json!({"pattern": "secret", "path": "leak"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
        assert!(err.to_string().contains("escapes"), "{err}");
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn replaced_file_is_reported_but_does_not_break_results() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("good.txt"), "hello good\n").unwrap();
        std::fs::write(directory.path().join("bad.txt"), "hello bad\n").unwrap();
        let context = ctx_at(directory.path());
        let root = resolve_search_root(&context.cwd, None).unwrap();
        let hooks = SearchHooks {
            before_open: Some(Arc::new(|path| {
                if path.ends_with("bad.txt") {
                    std::fs::remove_file(path).unwrap();
                    std::fs::create_dir(path).unwrap();
                }
            })),
        };
        let mut builder = RegexMatcherBuilder::new();
        builder.fixed_strings(true);
        let matcher = builder.build("hello").unwrap();

        let result = unwrap_tool(run_search_with_hooks(
            matcher,
            root,
            None,
            None,
            None,
            &context.cancel,
            &Limits::default(),
            &hooks,
        ));
        let text = text_of(&result);
        assert!(text.contains("good.txt:1:hello good"), "{text}");
        assert!(text.contains("search incomplete"), "{text}");
        let details = result.details.unwrap();
        assert_eq!(details["io_error_count"], 1, "{details}");
        assert_eq!(details["matches_lower_bound"], true, "{details}");
        assert!(
            details["io_errors"].as_array().unwrap()[0]
                .as_str()
                .unwrap()
                .contains("bad.txt"),
            "{details}"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn verbatim_cwd_accepts_plain_absolute_paths_and_renders_plain() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        // CLI-style canonicalized (`\\?\C:\…`) session cwd.
        let canonical = dir.path().canonicalize().unwrap();
        let ctx = ctx_at(&canonical);

        // A plain absolute argument inside the cwd is accepted (the
        // verbatim prefix is stripped before the gates)…
        let plain = crate::builtin::fs_search::strip_verbatim_prefix(&canonical)
            .join("src")
            .join("util.rs");
        let result = run_dyn(
            &GrepTool,
            json!({"pattern": "hello", "path": plain.to_str().unwrap()}),
            &ctx,
        )
        .await
        .unwrap();
        let text = text_of(&result);
        assert!(text.contains("hello from util"), "{text}");
        // …and the single-file report renders the normalized posix
        // path (`C:/…`), never the verbatim `//?/C:/…` form.
        let first = text.lines().next().unwrap();
        assert!(
            first.starts_with(|c: char| c.is_ascii_alphabetic()),
            "{text}"
        );
        assert!(!first.contains("//?/"), "{text}");
        assert_eq!(result.details.unwrap()["files_searched"], 1);

        // Relative paths under the verbatim cwd keep working.
        let result = run_dyn(&GrepTool, json!({"pattern": "hello", "path": "docs"}), &ctx)
            .await
            .unwrap();
        assert!(text_of(&result).contains("notes.md:2:hello world"));
    }

    /// One huge text file may invoke at most the atomic match budget;
    /// remaining bytes are classification-only and produce no callbacks.
    #[test]
    fn single_large_file_stops_callbacks_at_count_budget() {
        use crate::builtin::fs_search::COUNT_BUDGET;

        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("large.txt"),
            "hit\n".repeat(COUNT_BUDGET as usize + 500),
        )
        .unwrap();
        let context = ToolCtx::new(directory.path());
        let limits = Limits::default();
        let mut builder = RegexMatcherBuilder::new();
        builder.fixed_strings(true);
        let matcher = builder.build("hit").unwrap();
        let root = resolve_search_root(&context.cwd, None).unwrap();

        let result = unwrap_tool(run_search(
            matcher,
            root,
            None,
            None,
            Some(0),
            &context.cancel,
            &limits,
        ));
        let details = result.details.unwrap();
        assert_eq!(details["matches"], COUNT_BUDGET, "{details}");
        assert_eq!(details["shown"], 0, "{details}");
        assert_eq!(details["stopped_early"], "match-count budget reached");
        assert_eq!(details["matches_lower_bound"], true);
    }

    /// Provisional reservations from a binary flood are released after the
    /// same reader drains to its NUL suffix, leaving the final text quota free.
    #[test]
    fn nul_suffix_releases_provisional_count_reservations() {
        let directory = tempfile::tempdir().unwrap();
        let binary_path = directory.path().join("binary.bin");
        let mut binary = b"hit\n".repeat(50_000);
        binary.push(0);
        std::fs::write(&binary_path, binary).unwrap();
        let text_path = directory.path().join("text.txt");
        std::fs::write(&text_path, "hit\n".repeat(100)).unwrap();

        let limits = Limits {
            count_budget: 10,
            ..Limits::default()
        };
        let state = SearchState::new(&limits, Arc::new(WalkLimiter::new(&limits)));
        let cancel = CancellationToken::new();
        let mut builder = RegexMatcherBuilder::new();
        builder.fixed_strings(true);
        let matcher = builder.build("hit").unwrap();
        let mut searcher = build_searcher(&limits);

        let mut binary_file = File::open(&binary_path).unwrap();
        search_open_file(
            &mut searcher,
            &matcher,
            &mut binary_file,
            PathOrderKey::from_rendered_and_raw("binary.bin".to_owned(), "binary.bin"),
            &state,
            0,
            &cancel,
            &limits,
        )
        .unwrap();
        assert_eq!(state.total_matches.load(Ordering::Acquire), 0);
        assert_eq!(state.match_slots.load(Ordering::Acquire), 0);
        assert_eq!(state.files_searched.load(Ordering::Acquire), 0);
        assert_eq!(state.limiter.stopped_reason(), None);

        let mut text_file = File::open(&text_path).unwrap();
        search_open_file(
            &mut searcher,
            &matcher,
            &mut text_file,
            PathOrderKey::from_rendered_and_raw("text.txt".to_owned(), "text.txt"),
            &state,
            0,
            &cancel,
            &limits,
        )
        .unwrap();
        assert_eq!(state.total_matches.load(Ordering::Acquire), 10);
        assert_eq!(state.match_slots.load(Ordering::Acquire), 10);
        assert_eq!(state.files_searched.load(Ordering::Acquire), 1);
        assert_eq!(
            state.limiter.stopped_reason(),
            Some("match-count budget reached")
        );
    }

    /// All files share one atomic reservation ceiling and cannot overshoot it.
    #[test]
    fn concurrent_files_share_one_count_budget() {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..64 {
            std::fs::write(
                directory.path().join(format!("f{index:02}.txt")),
                "hit\n".repeat(1_000),
            )
            .unwrap();
        }
        let context = ToolCtx::new(directory.path());
        let limits = Limits {
            count_budget: 250,
            ..Limits::default()
        };
        let mut builder = RegexMatcherBuilder::new();
        builder.fixed_strings(true);
        let matcher = builder.build("hit").unwrap();
        let root = resolve_search_root(&context.cwd, None).unwrap();
        let result = unwrap_tool(run_search(
            matcher,
            root,
            None,
            None,
            Some(0),
            &context.cancel,
            &limits,
        ));
        let details = result.details.unwrap();
        assert_eq!(details["matches"], 250, "{details}");
        assert_eq!(details["stopped_early"], "match-count budget reached");
    }

    /// Every zero-match file byte goes through the same budget; there is no
    /// uncharged binary probe that reopens each file.
    #[test]
    fn many_zero_match_files_obey_actual_read_cap() {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..32 {
            std::fs::write(
                directory.path().join(format!("f{index:02}.txt")),
                "x".repeat(256),
            )
            .unwrap();
        }
        let context = ToolCtx::new(directory.path());
        let limits = Limits {
            scan_bytes: 1_024,
            ..Limits::default()
        };
        let mut builder = RegexMatcherBuilder::new();
        builder.fixed_strings(true);
        let matcher = builder.build("missing").unwrap();
        let root = resolve_search_root(&context.cwd, None).unwrap();
        let result = unwrap_tool(run_search(
            matcher,
            root,
            None,
            None,
            None,
            &context.cancel,
            &limits,
        ));
        let details = result.details.unwrap();
        assert_eq!(details["stopped_early"], "scanned-bytes limit reached");
        assert!(
            details["files_searched"].as_u64().unwrap() <= 4,
            "{details}"
        );
        assert_eq!(details["matches"], 0);
    }

    /// A file below the cap reaches a real EOF. Exactly-at and over-cap
    /// files both stop without publishing an unclassified prefix.
    #[test]
    fn scan_cap_handles_below_exactly_at_and_over() {
        for (length, should_stop) in [(1_023usize, false), (1_024usize, true), (1_025usize, true)] {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(directory.path().join("data.txt"), vec![b'x'; length]).unwrap();
            let context = ToolCtx::new(directory.path());
            let limits = Limits {
                scan_bytes: 1_024,
                ..Limits::default()
            };
            let mut builder = RegexMatcherBuilder::new();
            builder.fixed_strings(true);
            let matcher = builder.build("missing").unwrap();
            let root = resolve_search_root(&context.cwd, Some("data.txt")).unwrap();
            let result = unwrap_tool(run_search(
                matcher,
                root,
                None,
                None,
                None,
                &context.cancel,
                &limits,
            ));
            let details = result.details.unwrap();
            if should_stop {
                assert_eq!(details["stopped_early"], "scanned-bytes limit reached");
                assert_eq!(details["files_searched"], 0, "{details}");
            } else {
                assert!(details.get("stopped_early").is_none(), "{details}");
                assert_eq!(details["files_searched"], 1, "{details}");
            }
        }
    }

    /// Growth after the original handle is opened cannot evade the actual
    /// read budget, even though initial metadata fitted under the cap.
    #[test]
    fn growing_file_is_stopped_by_actual_read_budget() {
        use std::io::Write as _;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grow.txt");
        std::fs::write(&path, b"abcd").unwrap();
        let limits = Limits {
            scan_bytes: 4,
            ..Limits::default()
        };
        let state = SearchState::new(&limits, Arc::new(WalkLimiter::new(&limits)));
        let cancel = CancellationToken::new();
        let mut file = File::open(&path).unwrap();
        let mut reader = PolledReader {
            file: &mut file,
            state: &state,
            cancel: &cancel,
            scan_cap: limits.scan_bytes,
            eof_seen: false,
            nul_seen: false,
        };
        let mut buffer = [0u8; 2];
        assert_eq!(reader.read(&mut buffer).unwrap(), 2);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"ef")
            .unwrap();
        assert_eq!(reader.read(&mut buffer).unwrap(), 2);
        assert!(reader.read(&mut buffer).is_err());
        assert_eq!(
            state.limiter.stopped_reason(),
            Some("scanned-bytes limit reached")
        );
        assert_eq!(state.limiter.claimed_scan_bytes(), 4);
    }

    /// A barrier after walker enumeration makes replacement deterministic.
    /// The replacement name must never be reopened and followed.
    #[cfg(any(unix, windows))]
    #[test]
    fn enumerated_entry_replacement_cannot_escape_opened_root() {
        use std::sync::Barrier;

        let allowed = tempfile::tempdir().unwrap();
        let victim_directory = allowed.path().join("victim");
        std::fs::create_dir_all(&victim_directory).unwrap();
        std::fs::write(victim_directory.join("data.txt"), "safe\n").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("data.txt"), "SECRET_OUTSIDE\n").unwrap();

        let context = ToolCtx::new(allowed.path());
        let root = resolve_search_root(&context.cwd, None).unwrap();
        let reached = Arc::new(Barrier::new(2));
        let replaced = Arc::new(Barrier::new(2));
        let hooks = SearchHooks {
            before_open: Some({
                let reached = Arc::clone(&reached);
                let replaced = Arc::clone(&replaced);
                Arc::new(move |path| {
                    if path.ends_with(Path::new("victim").join("data.txt")) {
                        reached.wait();
                        replaced.wait();
                    }
                })
            }),
        };
        let cancel = context.cancel.clone();
        let worker = std::thread::spawn(move || {
            let mut builder = RegexMatcherBuilder::new();
            builder.fixed_strings(true);
            let matcher = builder.build("SECRET_OUTSIDE").unwrap();
            run_search_with_hooks(
                matcher,
                root,
                None,
                None,
                None,
                &cancel,
                &Limits::default(),
                &hooks,
            )
        });

        reached.wait();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            std::fs::remove_file(victim_directory.join("data.txt")).unwrap();
            symlink(
                outside.path().join("data.txt"),
                victim_directory.join("data.txt"),
            )
            .unwrap();
        }
        #[cfg(windows)]
        {
            std::fs::remove_dir_all(&victim_directory).unwrap();
            junction::create(outside.path(), &victim_directory).unwrap();
        }
        replaced.wait();
        let result = unwrap_tool(worker.join().unwrap());
        assert!(
            !text_of(&result).contains("SECRET_OUTSIDE"),
            "{}",
            text_of(&result)
        );
        assert_eq!(result.details.as_ref().unwrap()["matches"], 0);
        #[cfg(windows)]
        junction::delete(&victim_directory).unwrap();
    }

    /// Replacing the selected root with a link to another allowed directory
    /// cannot redirect opens away from the retained selected-root handle.
    #[cfg(any(unix, windows))]
    #[test]
    fn selected_root_replacement_cannot_redirect_within_allowed_root() {
        let allowed = tempfile::tempdir().unwrap();
        let scan = allowed.path().join("scan");
        std::fs::create_dir_all(&scan).unwrap();
        std::fs::write(scan.join("inside.txt"), "inside\n").unwrap();
        let redirected = allowed.path().join("redirected");
        std::fs::create_dir_all(&redirected).unwrap();
        std::fs::write(redirected.join("secret.txt"), "SECRET_REDIRECTED\n").unwrap();
        let context = ToolCtx::new(allowed.path());
        let root = resolve_search_root(&context.cwd, Some("scan")).unwrap();
        let retained = allowed.path().join("retained");
        std::fs::rename(&scan, &retained).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&redirected, &scan).unwrap();
        #[cfg(windows)]
        junction::create(&redirected, &scan).unwrap();

        let mut builder = RegexMatcherBuilder::new();
        builder.fixed_strings(true);
        let matcher = builder.build("SECRET_REDIRECTED").unwrap();
        let result = unwrap_tool(run_search(
            matcher,
            root,
            None,
            None,
            None,
            &context.cancel,
            &Limits::default(),
        ));
        assert!(
            !text_of(&result).contains("SECRET_REDIRECTED"),
            "{}",
            text_of(&result)
        );
        assert_eq!(result.details.as_ref().unwrap()["matches"], 0);
        assert_no_diagnostic_needle(&result, "secret.txt");
        assert_no_diagnostic_needle(&result, "SECRET_REDIRECTED");
        #[cfg(windows)]
        junction::delete(&scan).unwrap();
    }

    /// Replacement-tree `.gitignore` must not cause a retained-tree file of
    /// the same name to be read when the retained ignore rules hide it.
    #[cfg(any(unix, windows))]
    #[test]
    fn selected_root_replacement_cannot_apply_replacement_gitignore() {
        let allowed = tempfile::tempdir().unwrap();
        let scan = allowed.path().join("scan");
        std::fs::create_dir_all(scan.join(".git")).unwrap();
        std::fs::write(scan.join(".gitignore"), "secret.txt\n").unwrap();
        std::fs::write(scan.join("secret.txt"), "RETAINED_SECRET\n").unwrap();
        std::fs::write(scan.join("kept.txt"), "kept visible\n").unwrap();
        let redirected = allowed.path().join("redirected");
        std::fs::create_dir_all(redirected.join(".git")).unwrap();
        std::fs::write(redirected.join(".gitignore"), "\n").unwrap();
        std::fs::write(redirected.join("secret.txt"), "REPLACEMENT_SECRET\n").unwrap();
        std::fs::write(redirected.join("kept.txt"), "kept visible\n").unwrap();
        let context = ToolCtx::new(allowed.path());
        let root = resolve_search_root(&context.cwd, Some("scan")).unwrap();
        std::fs::rename(&scan, allowed.path().join("retained")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&redirected, &scan).unwrap();
        #[cfg(windows)]
        junction::create(&redirected, &scan).unwrap();

        let mut builder = RegexMatcherBuilder::new();
        builder.fixed_strings(true);
        let matcher = builder.build("RETAINED_SECRET").unwrap();
        let hidden = unwrap_tool(run_search(
            matcher,
            root,
            None,
            None,
            None,
            &context.cancel,
            &Limits::default(),
        ));
        assert_eq!(text_of(&hidden), "");
        assert_eq!(hidden.details.as_ref().unwrap()["matches"], 0);
        assert_no_diagnostic_needle(&hidden, "secret.txt");
        assert_no_diagnostic_needle(&hidden, "RETAINED_SECRET");

        let root = resolve_search_root(&context.cwd, Some("retained")).unwrap();
        let mut builder = RegexMatcherBuilder::new();
        builder.fixed_strings(true);
        let matcher = builder.build("kept visible").unwrap();
        let kept = unwrap_tool(run_search(
            matcher,
            root,
            None,
            None,
            None,
            &context.cancel,
            &Limits::default(),
        ));
        assert!(
            text_of(&kept).contains("kept.txt:1:kept visible"),
            "{}",
            text_of(&kept)
        );
        #[cfg(windows)]
        junction::delete(&scan).unwrap();
    }

    /// The engine's line buffer is heap-capped and an oversized line is
    /// discarded without affecting another file.
    #[test]
    fn line_heap_limit_bounds_the_line_buffer() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("huge.txt"),
            format!("hit {}", "x".repeat(64 * 1024)),
        )
        .unwrap();
        std::fs::write(directory.path().join("ok.txt"), "hit fine\n").unwrap();
        let context = ToolCtx::new(directory.path());
        let limits = Limits {
            line_heap: 8 * 1024,
            ..Limits::default()
        };
        let mut builder = RegexMatcherBuilder::new();
        builder.fixed_strings(true);
        let matcher = builder.build("hit").unwrap();
        let root = resolve_search_root(&context.cwd, None).unwrap();
        let result = unwrap_tool(run_search(
            matcher,
            root,
            None,
            None,
            None,
            &context.cancel,
            &limits,
        ));
        let text = text_of(&result).to_owned();
        let details = result.details.unwrap();
        assert!(text.contains("ok.txt:1:hit fine"), "{text}");
        assert!(!text.contains("huge.txt"), "{text}");
        assert_eq!(details["matches"], 1, "{details}");
        assert_eq!(details["io_error_count"], 1, "{details}");
    }

    /// Nested directory rename plus a same-name ordinary replacement must not
    /// mix retained-handle ignore decisions with replacement-tree bytes.
    #[cfg(any(unix, windows))]
    #[test]
    fn nested_directory_replacement_cannot_mix_old_ignore_with_new_content() {
        use std::sync::Barrier;

        let allowed = tempfile::tempdir().unwrap();
        let nested = allowed.path().join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("visible.txt"), "RETAINED_VISIBLE\n").unwrap();
        // Build the replacement outside the allowed root so the walker cannot
        // list it as a sibling before the same-name swap.
        let outside = tempfile::tempdir().unwrap();
        let replacement = outside.path().join("replacement");
        std::fs::create_dir_all(&replacement).unwrap();
        std::fs::write(replacement.join(".ignore"), "visible.txt\n").unwrap();
        std::fs::write(replacement.join("visible.txt"), "REPLACEMENT_SECRET\n").unwrap();
        std::fs::write(replacement.join("new_only.txt"), "REPLACEMENT_SECRET\n").unwrap();

        let context = ToolCtx::new(allowed.path());
        let root = resolve_search_root(&context.cwd, None).unwrap();
        let reached = Arc::new(Barrier::new(2));
        let replaced = Arc::new(Barrier::new(2));
        let hooks = SearchHooks {
            before_open: Some({
                let reached = Arc::clone(&reached);
                let replaced = Arc::clone(&replaced);
                Arc::new(move |path| {
                    if path.ends_with(Path::new("nested").join("visible.txt")) {
                        reached.wait();
                        replaced.wait();
                    }
                })
            }),
        };
        let cancel = context.cancel.clone();
        let worker = std::thread::spawn(move || {
            let matcher = RegexMatcherBuilder::new()
                .build("RETAINED_VISIBLE|REPLACEMENT_SECRET")
                .unwrap();
            run_search_with_hooks(
                matcher,
                root,
                None,
                None,
                None,
                &cancel,
                &Limits::default(),
                &hooks,
            )
        });

        reached.wait();
        std::fs::rename(&nested, allowed.path().join("retained_nested")).unwrap();
        std::fs::rename(&replacement, &nested).unwrap();
        replaced.wait();
        let result = unwrap_tool(worker.join().unwrap());
        let text = text_of(&result);
        assert!(text.contains("RETAINED_VISIBLE"), "{text}");
        assert!(!text.contains("REPLACEMENT_SECRET"), "{text}");
        assert!(!text.contains("new_only.txt"), "{text}");
        assert_no_diagnostic_needle(&result, "REPLACEMENT_SECRET");
        assert_eq!(result.details.as_ref().unwrap()["matches"], 1);
    }

    #[tokio::test]
    async fn oversized_root_ignore_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".ignore"),
            vec![b'x'; IGNORE_FILE_MAX_BYTES + 1],
        )
        .unwrap();
        std::fs::write(dir.path().join("secret.txt"), "hello leak\n").unwrap();
        let ctx = ctx_at(dir.path());
        let err = run_dyn(&GrepTool, json!({"pattern": "hello"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)), "{err}");
        assert!(err.to_string().contains("ignore"), "{err}");
    }

    #[tokio::test]
    async fn nested_git_root_does_not_keep_outer_gitignore_hits_hidden() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "secret.txt\n").unwrap();
        std::fs::write(dir.path().join(".ignore"), "ignored_by_ignore.txt\n").unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir_all(nested.join(".git")).unwrap();
        std::fs::write(nested.join("secret.txt"), "hello nested-git\n").unwrap();
        std::fs::write(nested.join("ignored_by_ignore.txt"), "hello ignored\n").unwrap();
        std::fs::write(nested.join("kept.txt"), "hello kept\n").unwrap();
        let ctx = ctx_at(dir.path());
        let result = run_dyn(&GrepTool, json!({"pattern": "hello"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(
            text.contains("nested/secret.txt:1:hello nested-git"),
            "{text}"
        );
        assert!(text.contains("nested/kept.txt:1:hello kept"), "{text}");
        assert!(!text.contains("ignored_by_ignore"), "{text}");
    }

    #[cfg(windows)]
    #[test]
    fn hidden_open_handle_discards_matches_before_commit() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, SetFileAttributesW};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("late.txt");
        std::fs::write(
            &path,
            "SECRET_LATE
",
        )
        .unwrap();
        let mut file = std::fs::File::open(&path).unwrap();
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        // SAFETY: `wide` is a live NUL-terminated UTF-16 path.
        let ok = unsafe { SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_HIDDEN) };
        assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());

        let limits = Limits::default();
        let state = SearchState::new(&limits, Arc::new(WalkLimiter::new(&limits)));
        let matcher = RegexMatcherBuilder::new().build("SECRET_LATE").unwrap();
        let mut searcher = build_searcher(&limits);
        search_open_file(
            &mut searcher,
            &matcher,
            &mut file,
            PathOrderKey::from_path(Path::new("late.txt")),
            &state,
            MAX_MATCHES,
            &CancellationToken::new(),
            &limits,
        )
        .unwrap();
        assert_eq!(state.total_matches.load(Ordering::Acquire), 0);
        assert!(state.heap.lock().unwrap().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn hidden_after_listing_is_not_read_or_reported() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, SetFileAttributesW};

        let dir = tempfile::tempdir().unwrap();
        let late = dir.path().join("late.txt");
        std::fs::write(&late, "SECRET_LATE\n").unwrap();
        let context = ctx_at(dir.path());
        let root = resolve_search_root(&context.cwd, None).unwrap();
        let hooks = SearchHooks {
            before_open: Some(Arc::new(|path| {
                if path.ends_with("late.txt") {
                    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
                    wide.push(0);
                    // SAFETY: `wide` is a live NUL-terminated UTF-16 path.
                    let ok = unsafe { SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_HIDDEN) };
                    assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
                }
            })),
        };
        let matcher = RegexMatcherBuilder::new().build("SECRET_LATE").unwrap();
        let result = unwrap_tool(run_search_with_hooks(
            matcher,
            root,
            None,
            None,
            None,
            &context.cancel,
            &Limits::default(),
            &hooks,
        ));
        assert_eq!(text_of(&result), "", "{}", text_of(&result));
        assert_eq!(result.details.as_ref().unwrap()["matches"], 0);
        assert!(
            result
                .details
                .as_ref()
                .unwrap()
                .get("io_error_count")
                .is_none()
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_case_alias_honors_on_disk_anchored_ignore() {
        let dir = tempfile::tempdir().unwrap();
        let visible = dir.path().join("Visible");
        std::fs::create_dir(&visible).unwrap();
        std::fs::write(dir.path().join(".ignore"), "/Visible/secret.txt\n").unwrap();
        std::fs::write(visible.join("secret.txt"), "hello secret\n").unwrap();
        std::fs::write(visible.join("kept.txt"), "hello kept\n").unwrap();
        let ctx = ctx_at(dir.path());
        let result = run_dyn(
            &GrepTool,
            json!({"pattern": "hello", "path": "visible"}),
            &ctx,
        )
        .await
        .unwrap();
        let text = text_of(&result);
        assert!(text.contains("kept.txt:1:hello kept"), "{text}");
        assert!(!text.contains("secret"), "{text}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_eight_dot_three_alias_honors_on_disk_anchored_ignore() {
        let dir = tempfile::tempdir().unwrap();
        let visible = dir.path().join("LongVisibleName");
        std::fs::create_dir(&visible).unwrap();
        std::fs::write(dir.path().join(".ignore"), "/LongVisibleName/secret.txt\n").unwrap();
        std::fs::write(visible.join("secret.txt"), "hello secret\n").unwrap();
        std::fs::write(visible.join("kept.txt"), "hello kept\n").unwrap();
        let short = windows_short_path(&visible).unwrap();
        let short_name = short.file_name().unwrap().to_os_string();
        if short_name == visible.file_name().unwrap() {
            return;
        }
        assert!(short_name.to_string_lossy().contains('~'), "{short_name:?}");
        let ctx = ctx_at(dir.path());
        let result = run_dyn(
            &GrepTool,
            json!({
                "pattern": "hello",
                "path": short_name.to_str().unwrap()
            }),
            &ctx,
        )
        .await
        .unwrap();
        let text = text_of(&result);
        assert!(text.contains("kept.txt:1:hello kept"), "{text}");
        assert!(!text.contains("secret"), "{text}");
    }

    #[tokio::test]
    async fn explicit_ignored_and_hidden_file_targets_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "secret.txt\n").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "hello secret\n").unwrap();
        std::fs::write(dir.path().join(".hidden.txt"), "hello hidden\n").unwrap();
        let ctx = ctx_at(dir.path());

        let ignored = run_dyn(
            &GrepTool,
            json!({"pattern": "hello", "path": "secret.txt"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(text_of(&ignored), "");
        assert_eq!(ignored.details.unwrap()["matches"], 0);

        let hidden = run_dyn(
            &GrepTool,
            json!({"pattern": "hello", "path": ".hidden.txt"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(text_of(&hidden), "");
        assert_eq!(hidden.details.unwrap()["matches"], 0);
    }

    #[tokio::test]
    async fn pattern_nul_and_size_and_glob_limits_are_rejected() {
        use crate::builtin::fs_search::MAX_PATTERN_BYTES;
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());

        let nul = run_dyn(&GrepTool, json!({"pattern": "ok\u{0000}bad"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(nul, ToolError::InvalidArgs(_)), "{nul}");
        assert!(nul.to_string().contains("NUL"), "{nul}");

        let huge = "a".repeat(MAX_PATTERN_BYTES + 1);
        let over = run_dyn(&GrepTool, json!({"pattern": huge}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(over, ToolError::InvalidArgs(_)), "{over}");

        let glob_nul = run_dyn(
            &GrepTool,
            json!({"pattern": "x", "include": "a\u{0000}b"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(glob_nul, ToolError::InvalidArgs(_)), "{glob_nul}");

        let glob_huge = "a".repeat(MAX_PATTERN_BYTES + 1);
        let glob_over = run_dyn(
            &GrepTool,
            json!({"pattern": "x", "exclude": glob_huge}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(glob_over, ToolError::InvalidArgs(_)),
            "{glob_over}"
        );

        let banned = run_dyn(
            &GrepTool,
            json!({"pattern": "\\x00", "is_regex": true}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(banned, ToolError::InvalidArgs(_)), "{banned}");
    }

    #[tokio::test]
    async fn reverse_directory_enumeration_keeps_the_same_top_n() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["z.txt", "a.txt", "m.txt"] {
            std::fs::write(dir.path().join(name), "hit\n").unwrap();
        }
        let mut builder = RegexMatcherBuilder::new();
        builder.fixed_strings(true);
        let run = |reverse: bool| {
            let matcher = builder.build("hit").unwrap();
            let limits = Limits {
                reverse_dir_enum: reverse,
                ..Limits::default()
            };
            let root =
                resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits)
                    .unwrap();
            let result = unwrap_tool(run_search(
                matcher,
                root,
                None,
                None,
                Some(2),
                &CancellationToken::new(),
                &limits,
            ));
            (text_of(&result).to_owned(), result.details.unwrap())
        };
        let forward = run(false);
        let reverse = run(true);
        assert_eq!(forward, reverse);
        assert_eq!(
            forward.0,
            "a.txt:1:hit\nm.txt:1:hit\n[showing first 2 of 3 matching lines; narrow the pattern or raise max_results]"
        );
    }

    /// Two non-UTF-8 names map to the same replacement display. A one-match
    /// budget plus reversed OS order must still return the globally smallest
    /// rendered path, with the original `OsString` breaking ties.
    // APFS rejects invalid UTF-8 byte names with EILSEQ; Linux provides this fixture.
    #[cfg(target_os = "linux")]
    #[test]
    fn non_utf8_names_low_count_budget_is_global_rendered_min() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(OsStr::from_bytes(b"\x80.txt")), "hit80\n").unwrap();
        std::fs::write(dir.path().join(OsStr::from_bytes(b"\x81.txt")), "hit81\n").unwrap();
        std::fs::write(dir.path().join("\u{00ff}.txt"), "hitY\n").unwrap();
        let mut builder = RegexMatcherBuilder::new();
        builder.fixed_strings(true);
        let run = |reverse: bool| {
            let limits = Limits {
                count_budget: 1,
                reverse_dir_enum: reverse,
                ..Limits::default()
            };
            let matcher = builder.build("hit").unwrap();
            let root =
                resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits)
                    .unwrap();
            let result = unwrap_tool(run_search(
                matcher,
                root,
                None,
                None,
                Some(1),
                &CancellationToken::new(),
                &limits,
            ));
            text_of(&result).to_owned()
        };
        let forward = run(false);
        let reverse = run(true);
        assert!(forward.contains("\u{00ff}.txt:1:hitY"), "{forward}");
        assert!(!forward.contains("hit80"), "{forward}");
        assert!(!forward.contains("hit81"), "{forward}");
        assert_eq!(forward, reverse);

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(OsStr::from_bytes(b"\x80.txt")), "hit80\n").unwrap();
        std::fs::write(dir.path().join(OsStr::from_bytes(b"\x81.txt")), "hit81\n").unwrap();
        let run_tie = |reverse: bool| {
            let limits = Limits {
                count_budget: 1,
                reverse_dir_enum: reverse,
                ..Limits::default()
            };
            let matcher = builder.build("hit").unwrap();
            let root =
                resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits)
                    .unwrap();
            let result = unwrap_tool(run_search(
                matcher,
                root,
                None,
                None,
                Some(1),
                &CancellationToken::new(),
                &limits,
            ));
            text_of(&result).to_owned()
        };
        let forward = run_tie(false);
        let reverse = run_tie(true);
        assert!(forward.contains("hit80"), "{forward}");
        assert!(!forward.contains("hit81"), "{forward}");
        assert_eq!(forward, reverse);
    }

    /// Two non-UTF-8 names can share one replacement display. Top-1 must
    /// follow the shared path key, not the matching line text.
    // APFS rejects invalid UTF-8 byte names with EILSEQ; Linux provides this fixture.
    #[cfg(target_os = "linux")]
    #[test]
    fn lossy_path_collision_top_n_ignores_line_text() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        // `\x80` sorts before `\x81` as OsString; "zzz" sorts after "aaa".
        std::fs::write(dir.path().join(OsStr::from_bytes(b"\x80.txt")), "zzz hit\n").unwrap();
        std::fs::write(dir.path().join(OsStr::from_bytes(b"\x81.txt")), "aaa hit\n").unwrap();
        let mut builder = RegexMatcherBuilder::new();
        builder.fixed_strings(true);
        let matcher = builder.build("hit").unwrap();
        let root = resolve_search_root(dir.path(), None).unwrap();
        let result = unwrap_tool(run_search(
            matcher,
            root,
            None,
            None,
            Some(1),
            &CancellationToken::new(),
            &Limits::default(),
        ));
        let text = text_of(&result);
        assert!(text.contains("zzz hit"), "{text}");
        assert!(!text.contains("aaa hit"), "{text}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn explicit_windows_file_alias_honors_ignore() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".ignore"), "Visible.txt\n").unwrap();
        std::fs::write(dir.path().join("Visible.txt"), "hello secret\n").unwrap();
        let ctx = ctx_at(dir.path());
        let result = run_dyn(
            &GrepTool,
            json!({"pattern": "hello", "path": "visible.txt"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(text_of(&result), "");

        let long = dir.path().join("LongIgnoredName.txt");
        std::fs::write(dir.path().join(".ignore"), "LongIgnoredName.txt\n").unwrap();
        std::fs::write(&long, "hello secret\n").unwrap();
        let short = windows_short_path(&long).unwrap();
        let short_name = short.file_name().unwrap().to_os_string();
        if short_name == long.file_name().unwrap() {
            return;
        }
        let result = run_dyn(
            &GrepTool,
            json!({"pattern": "hello", "path": short_name.to_str().unwrap()}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(text_of(&result), "");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_ads_stream_syntax_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "hello visible\n").unwrap();
        let _ = std::fs::write(dir.path().join("file.txt:hidden"), "hello secret\n");
        let ctx = ctx_at(dir.path());
        let err = run_dyn(
            &GrepTool,
            json!({"pattern": "hello", "path": "file.txt:hidden"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");

        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        let _ = std::fs::write(dir.path().join("subdir:stream"), "hello secret\n");
        let err = run_dyn(
            &GrepTool,
            json!({"pattern": "hello", "path": "subdir:stream"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    }

    #[test]
    fn result_store_byte_budget_interns_and_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let deep = "n".repeat(200);
        std::fs::write(dir.path().join(&deep), "hit\nhit\n").unwrap();
        let limits = Limits {
            max_result_bytes: 40,
            ..Limits::default()
        };
        let mut builder = RegexMatcherBuilder::new();
        builder.fixed_strings(true);
        let matcher = builder.build("hit").unwrap();
        let root = resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits)
            .unwrap();
        let result = unwrap_tool(run_search(
            matcher,
            root,
            None,
            None,
            None,
            &CancellationToken::new(),
            &limits,
        ));
        let details = result.details.unwrap();
        assert_eq!(details["stopped_early"], "result store limit reached");
        assert!(details["truncated"].as_bool().unwrap());
    }

    fn hit_matcher() -> RegexMatcher {
        let mut builder = RegexMatcherBuilder::new();
        builder.fixed_strings(true);
        builder.build("hit").unwrap()
    }

    #[test]
    fn discarded_files_do_not_block_later_matches() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..20 {
            std::fs::write(dir.path().join(format!("n{index:02}.txt")), "nope\n").unwrap();
        }
        std::fs::write(dir.path().join("nbin.bin"), b"hit\0rest").unwrap();
        std::fs::write(dir.path().join("z_hit.txt"), "hit\n").unwrap();
        let limits = Limits {
            max_result_bytes: 80,
            ..Limits::default()
        };
        let root = resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits)
            .unwrap();
        let limiter = Arc::clone(&root.limiter);
        let result = unwrap_tool(run_search(
            hit_matcher(),
            root,
            None,
            None,
            None,
            &CancellationToken::new(),
            &limits,
        ));
        let text = text_of(&result);
        assert!(text.contains("z_hit.txt:1:hit"), "{text}");
        let details = result.details.unwrap();
        assert_eq!(details["matches"], 1, "{details}");
        assert_ne!(
            details
                .get("stopped_early")
                .and_then(|value| value.as_str()),
            Some("result store limit reached"),
            "{details}"
        );
        let kept = PathOrderKey::from_path(Path::new("z_hit.txt"));
        let expected = kept.store_bytes() + "hit".len() + std::mem::size_of::<u64>();
        assert_eq!(limiter.result_store_bytes(), expected as u64);
    }

    #[test]
    fn empty_result_heap_returns_path_charge_to_zero() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("none.txt"), "nope\n").unwrap();
        std::fs::write(dir.path().join("binary.bin"), b"hit\0rest").unwrap();
        let limits = Limits::default();
        let root = resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits)
            .unwrap();
        let limiter = Arc::clone(&root.limiter);
        let result = unwrap_tool(run_search(
            hit_matcher(),
            root,
            None,
            None,
            None,
            &CancellationToken::new(),
            &limits,
        ));
        assert_eq!(text_of(&result), "");
        assert_eq!(result.details.unwrap()["matches"], 0);
        assert_eq!(limiter.result_store_bytes(), 0);
        assert!(!limiter.result_store_truncated());

        std::fs::write(dir.path().join("hit.txt"), "hit\n").unwrap();
        let limits = Limits::default();
        let root = resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits)
            .unwrap();
        let limiter = Arc::clone(&root.limiter);
        let result = unwrap_tool(run_search(
            hit_matcher(),
            root,
            None,
            None,
            Some(0),
            &CancellationToken::new(),
            &limits,
        ));
        assert_eq!(result.details.unwrap()["matches"], 1);
        assert_eq!(limiter.result_store_bytes(), 0);
        assert!(!limiter.result_store_truncated());
    }
}
