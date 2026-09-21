//! `find` — secure in-process file and directory discovery.
//!
//! A handle-relative walker supplies candidate names. Explicit and walked
//! regular files retain metadata-only capabilities, so discovery does not
//! require content-read access. Directories are reopened for content only
//! after metadata/content identities match. Current Windows hidden bits are
//! re-read before confirmation, reporting, and descent. Symlinks, reparse
//! points, and uncertain ignore boundaries fail closed. Ordinary per-path
//! I/O keeps confirmed results but adds a model-visible incomplete lower-bound
//! notice. Output uses `/`; no external `fd` executable is used. Cancellation
//! or future drop is supervised until the worker is interrupted and joined.
use std::collections::BinaryHeap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use globset::GlobMatcher;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

use super::fs_search::{
    IO_ERROR_SAMPLES, IoErrors, Limits, MAX_PATTERN_BYTES, PathOrderKey, ResolvedRoot,
    SearchAccess, WalkLimiter, bind_search_root_with_access, io_incomplete_notice, is_hidden_skip,
    rel_posix, run_blocking_until, stop_reason_error, to_posix, walk_retained_tree,
};

#[cfg(all(test, windows))]
use super::fs_search::windows_short_path;
#[cfg(test)]
use super::fs_search::{IGNORE_FILE_MAX_BYTES, resolve_search_root, resolve_search_root_cancel};
#[cfg(all(test, unix))]
use super::fs_search::{resolve_search_root_with_access, unix_casefold_alias_supported};

/// Default cap on reported paths.
pub const DEFAULT_LIMIT: usize = 1000;

/// The `find` builtin.
#[derive(Debug)]
pub struct FindTool;

/// Arguments for [`FindTool`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindArgs {
    /// Glob matched against each path relative to the search root.
    pub pattern: String,
    /// Directory or single file to search, relative to the session cwd.
    pub path: Option<String>,
    /// Maximum number of paths to report.
    pub limit: Option<usize>,
}

struct FindState {
    heap: Mutex<BinaryHeap<PathOrderKey>>,
    total: AtomicU64,
    io_errors: IoErrors,
    limiter: Arc<WalkLimiter>,
}

impl FindState {
    fn new(limiter: Arc<WalkLimiter>) -> Self {
        Self {
            heap: Mutex::new(BinaryHeap::new()),
            total: AtomicU64::new(0),
            io_errors: IoErrors::new(IO_ERROR_SAMPLES),
            limiter,
        }
    }
}

fn offer(
    heap: &mut BinaryHeap<PathOrderKey>,
    limiter: &WalkLimiter,
    limit: usize,
    path: PathOrderKey,
) {
    let bytes = path.store_bytes();
    if !limiter.try_reserve_result_bytes(bytes) {
        return;
    }
    if heap.len() < limit {
        heap.push(path);
    } else if heap.peek().is_some_and(|worst| &path < worst) {
        if let Some(evicted) = heap.pop() {
            limiter.release_result_bytes(evicted.store_bytes());
        }
        heap.push(path);
    } else {
        limiter.release_result_bytes(bytes);
    }
}

#[cfg(test)]
type BeforeOpenHook = Arc<dyn Fn(&Path) + Send + Sync>;

#[derive(Clone, Default)]
struct FindHooks {
    #[cfg(test)]
    before_open: Option<BeforeOpenHook>,
}

impl FindHooks {
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
impl Tool for FindTool {
    type Args = FindArgs;
    type Output = ();

    fn name(&self) -> &str {
        "find"
    }

    fn description(&self) -> &str {
        "Find files and directories by glob pattern under a directory (default: \
         the session cwd). Respects .gitignore; hidden and gitignored paths are \
         skipped. Reports up to `limit` (default 1000) matching paths relative \
         to the search root, sorted, using `/` separators, with a notice when \
         more matches exist. Cancellation or the wall-clock limit returns an \
         execution error rather than a partial report."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some("find: find files by glob pattern (pattern, optional path/limit).")
    }

    fn search_access(&self) -> Option<SearchAccess> {
        Some(SearchAccess::Metadata)
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        reject_glob_bytes(&args.pattern)?;
        let cwd = ctx.cwd.clone();
        let path = args.path;
        let pattern = args.pattern;
        let limit = args.limit;
        let deadline = Instant::now() + Limits::default().time_limit;
        let limits = Limits {
            deadline: Some(deadline),
            ..Limits::default()
        };
        let cancel = ctx.cancel.clone();
        let prepared = ctx.prepared_search.clone();
        run_blocking_until("find", &cancel, deadline, move |worker_cancel| {
            if worker_cancel.is_cancelled() {
                return Err(ToolError::Execution(
                    "find cancelled before completion".to_owned(),
                ));
            }
            let glob = compile_find_glob(&pattern)?;
            let root = bind_search_root_with_access(
                prepared.as_deref(),
                &cwd,
                path.as_deref(),
                &worker_cancel,
                &limits,
                SearchAccess::Metadata,
            )?;
            run_find(glob, root, limit, &worker_cancel, &limits)
        })
        .await
    }
}

fn run_find(
    glob: GlobMatcher,
    root: ResolvedRoot,
    limit: Option<usize>,
    cancel: &CancellationToken,
    limits: &Limits,
) -> Result<ToolResult, ToolError> {
    run_find_core(glob, root, limit, cancel, limits, &FindHooks::default())
}

#[cfg(test)]
fn run_find_with_hooks(
    glob: GlobMatcher,
    root: ResolvedRoot,
    limit: Option<usize>,
    cancel: &CancellationToken,
    limits: &Limits,
    hooks: &FindHooks,
) -> Result<ToolResult, ToolError> {
    run_find_core(glob, root, limit, cancel, limits, hooks)
}

fn run_find_core(
    glob: GlobMatcher,
    root: ResolvedRoot,
    limit: Option<usize>,
    cancel: &CancellationToken,
    limits: &Limits,
    hooks: &FindHooks,
) -> Result<ToolResult, ToolError> {
    let effective_limit = limit.unwrap_or(DEFAULT_LIMIT).min(limits.stored_ceiling);
    let state = Arc::new(FindState::new(Arc::clone(&root.limiter)));
    let report_root = root.root.clone();

    match root.target_is_skipped() {
        Ok(true) => return finish_find_report(&report_root, &state, effective_limit, limits),
        Ok(false) => {}
        Err(error) if is_hidden_skip(&error) => {
            return finish_find_report(&report_root, &state, effective_limit, limits);
        }
        Err(error) => {
            return Ok(ToolResult::error(format!(
                "search target hidden check failed: {error}"
            )));
        }
    }

    if root.is_file() {
        if !matches!(state.limiter.check(cancel), ignore::WalkState::Quit) {
            let relative = rel_posix(&root.cwd, &root.root);
            let candidate = if relative.is_empty() {
                to_posix(&root.root)
            } else {
                relative
            };
            if glob.is_match(&candidate) {
                match root.validate_target() {
                    Ok(()) => {
                        state.total.fetch_add(1, Ordering::Relaxed);
                        let mut heap = state.heap.lock().expect("find results lock poisoned");
                        offer(
                            &mut heap,
                            &state.limiter,
                            effective_limit,
                            PathOrderKey::from_rendered_and_raw(candidate, root.root.as_os_str()),
                        );
                    }
                    Err(error) => state.io_errors.record(&candidate, &error),
                }
            }
        }
        // The single-file target and allowed-root handles remain alive until
        // this report has been assembled.
        finish_find_report(&report_root, &state, effective_limit, limits)
    } else {
        if let Err(error) = walk_retained_tree(
            &root,
            &state.limiter,
            cancel,
            &state.io_errors,
            |relative_path, name, expected, parent| {
                if matches!(state.limiter.check(cancel), ignore::WalkState::Quit) {
                    return ignore::WalkState::Quit;
                }
                let relative = to_posix(relative_path);
                if !glob.is_match(&relative) {
                    return ignore::WalkState::Continue;
                }

                // Deterministic race hook: enumeration has completed, but the
                // candidate has not yet been opened or trusted.
                hooks.before_open(&root.root.join(relative_path));
                if let Err(error) = root.confirm_walked(parent, name, expected) {
                    if !is_hidden_skip(&error) {
                        state.io_errors.record(&relative, &error);
                    }
                    return ignore::WalkState::Continue;
                }
                // Report only after metadata confirmation. The enumerated
                // name is never sufficient by itself.
                state.total.fetch_add(1, Ordering::Relaxed);
                let mut heap = state.heap.lock().expect("find results lock poisoned");
                offer(
                    &mut heap,
                    &state.limiter,
                    effective_limit,
                    PathOrderKey::from_rendered_and_raw(relative, relative_path.as_os_str()),
                );
                drop(heap);
                ignore::WalkState::Continue
            },
        ) {
            return Ok(ToolResult::error(format!(
                "search ignore boundary could not be established: {error}"
            )));
        }
        // Report while `root` still retains both root identities.
        finish_find_report(&report_root, &state, effective_limit, limits)
    }
}

fn finish_find_report(
    root: &Path,
    state: &Arc<FindState>,
    effective_limit: usize,
    limits: &Limits,
) -> Result<ToolResult, ToolError> {
    if let Some(error) = stop_reason_error("find", &state.limiter) {
        return Err(error);
    }
    Ok(find_report(root, state, effective_limit, limits))
}

fn find_report(
    root: &Path,
    state: &Arc<FindState>,
    effective_limit: usize,
    limits: &Limits,
) -> ToolResult {
    let heap = std::mem::take(&mut *state.heap.lock().expect("find results lock poisoned"));
    let paths = heap.into_sorted_vec();

    let mut text = String::new();
    let mut shown = 0usize;
    let mut output_truncated = false;
    for path in &paths {
        let rendered = path.rendered();
        if text.len() + rendered.len() + 1 > limits.output_bytes {
            output_truncated = true;
            break;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(rendered);
        shown += 1;
    }

    let total = state.total.load(Ordering::Relaxed);
    let stop_reason = state.limiter.stopped_reason().or(state
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
            "[showing first {shown} of {count} matching paths; refine the pattern or raise limit{reason}]"
        ));
    }
    if output_truncated {
        text.push_str(&format!(
            "\n[output truncated at {} bytes; refine the pattern or lower limit]",
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
        "matches": total,
        "shown": shown,
        "truncated": truncated,
        "limit": effective_limit,
    });
    if !exact {
        details["matches_lower_bound"] = json!(true);
    }
    if let Some(reason) = stop_reason {
        details["stopped_early"] = json!(reason);
    }
    if output_truncated {
        details["output_truncated"] = json!(true);
    }
    if io_count > 0 {
        details["io_error_count"] = json!(io_count);
        details["io_errors"] = json!(io_samples);
    }

    debug_assert!(shown <= effective_limit);
    ToolResult::text(text).with_details(details)
}

fn reject_glob_bytes(pattern: &str) -> Result<(), ToolError> {
    if pattern.as_bytes().contains(&0) {
        return Err(ToolError::InvalidArgs(
            "pattern contains a NUL byte".to_owned(),
        ));
    }
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "pattern exceeds {MAX_PATTERN_BYTES} bytes"
        )));
    }
    Ok(())
}

fn compile_find_glob(pattern: &str) -> Result<GlobMatcher, ToolError> {
    reject_glob_bytes(pattern)?;
    globset::Glob::new(pattern)
        .map(|glob| glob.compile_matcher())
        .map_err(|error| {
            ToolError::InvalidArgs(format!("invalid pattern glob `{pattern}`: {error}"))
        })
}

#[cfg(test)]
#[path = "find_performance_tests.rs"]
mod performance_tests;

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
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("src/deep/a.rs"), "// a\n").unwrap();
        std::fs::write(dir.join("src/deep/b.ts"), "// b\n").unwrap();
        std::fs::write(dir.join("tests/main.rs"), "// t\n").unwrap();
        std::fs::write(dir.join("readme.md"), "# readme\n").unwrap();
    }

    #[tokio::test]
    async fn glob_matches_nested_paths_sorted_deterministically() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "*.rs"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        // Sorted, forward-slash rel paths, `*` crosses separators.
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            vec!["src/deep/a.rs", "src/main.rs", "tests/main.rs"],
            "{text}"
        );
        assert!(!result.is_error);
        let details = result.details.unwrap();
        assert_eq!(details["shown"], 3);
        assert_eq!(details["truncated"], false);
        assert_eq!(details["limit"], DEFAULT_LIMIT);
    }

    #[tokio::test]
    async fn anchored_glob_matches_relative_to_root() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "src/**/*.rs"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            vec!["src/deep/a.rs", "src/main.rs"],
            "{text}"
        );
        assert!(!text.contains("tests/main.rs"), "{text}");
    }

    #[tokio::test]
    async fn directories_match_the_glob_too() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "**/deep"}), &ctx)
            .await
            .unwrap();
        assert_eq!(text_of(&result), "src/deep");
    }

    #[tokio::test]
    async fn limit_truncates_with_notice() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        // The fixture tree has 8 matching entries (dirs included).
        let result = run_dyn(&FindTool, json!({"pattern": "*", "limit": 3}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        // Truncation keeps the lexicographically smallest paths.
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            vec![
                "readme.md",
                "src",
                "src/deep",
                "[showing first 3 of 8 matching paths; refine the pattern or raise limit]",
            ],
            "{text}"
        );
        let details = result.details.unwrap();
        assert_eq!(details["shown"], 3);
        assert_eq!(details["matches"], 8);
        assert_eq!(details["truncated"], true);
        assert_eq!(details["limit"], 3);
    }

    /// `limit: 0` is schema-valid and must report nothing while still
    /// telling the caller that matches exist — including the
    /// single-file `path` target branch.
    #[tokio::test]
    async fn limit_zero_reports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "*", "limit": 0}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert_eq!(
            text,
            "[showing first 0 of 8 matching paths; refine the pattern or raise limit]"
        );
        let details = result.details.unwrap();
        assert_eq!(details["shown"], 0);
        assert_eq!(details["matches"], 8);
        assert_eq!(details["truncated"], true);

        // Single-file target: the path matches, but 0 means 0.
        let result = run_dyn(
            &FindTool,
            json!({"pattern": "src/*.rs", "path": "src/main.rs", "limit": 0}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(
            text_of(&result),
            "[showing first 0 of 1 matching paths; refine the pattern or raise limit]"
        );
    }

    /// Truncation retains the smallest output paths regardless of directory
    /// enumeration order.
    #[tokio::test]
    async fn truncation_keeps_smallest_paths_deterministically() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["z.txt", "a.txt", "m.txt"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "*.txt", "limit": 2}), &ctx)
            .await
            .unwrap();
        assert_eq!(
            text_of(&result),
            "a.txt\nm.txt\n[showing first 2 of 3 matching paths; refine the pattern or raise limit]"
        );
    }

    #[tokio::test]
    async fn default_limit_is_1000() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..1005 {
            std::fs::write(dir.path().join(format!("f{i:04}.txt")), "x").unwrap();
        }
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "*.txt"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        let shown = text.lines().filter(|l| !l.starts_with('[')).count();
        assert_eq!(shown, DEFAULT_LIMIT, "{text}");
        assert!(
            text.contains("[showing first 1000 of 1005 matching paths"),
            "{text}"
        );
        assert_eq!(result.details.unwrap()["truncated"], true);
    }

    #[tokio::test]
    async fn path_targets_a_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "*.rs", "path": "src"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert_eq!(text, "deep/a.rs\nmain.rs");
    }

    #[tokio::test]
    async fn path_targeting_a_file_matches_its_cwd_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        let hit = run_dyn(
            &FindTool,
            json!({"pattern": "src/*.rs", "path": "src/main.rs"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(text_of(&hit), "src/main.rs");

        let miss = run_dyn(
            &FindTool,
            json!({"pattern": "*.md", "path": "src/main.rs"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(text_of(&miss), "");
    }

    #[tokio::test]
    async fn gitignore_and_hidden_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored/\n").unwrap();
        std::fs::create_dir_all(dir.path().join("ignored")).unwrap();
        std::fs::write(dir.path().join("ignored/x.txt"), "x").unwrap();
        std::fs::create_dir_all(dir.path().join(".hidden")).unwrap();
        std::fs::write(dir.path().join("kept.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "*"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert_eq!(text, "kept.txt", "{text}");
    }

    #[tokio::test]
    async fn nested_target_loads_ancestor_ignore_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/.gitignore"), "secret.txt\n").unwrap();
        std::fs::write(dir.path().join("a/b/secret.txt"), "secret\n").unwrap();
        std::fs::write(dir.path().join("a/b/kept.txt"), "kept\n").unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "*", "path": "a/b"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(text.lines().any(|line| line == "kept.txt"), "{text}");
        assert!(!text.contains("secret.txt"), "{text}");
    }

    #[tokio::test]
    async fn child_gitignore_cannot_whitelist_parent_ignore() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join(".ignore"), "secret.txt\n").unwrap();
        std::fs::write(dir.path().join("sub/.gitignore"), "!secret.txt\n").unwrap();
        std::fs::write(dir.path().join("sub/secret.txt"), "secret\n").unwrap();
        std::fs::write(dir.path().join("sub/kept.txt"), "kept\n").unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "*"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(text.lines().any(|line| line == "sub/kept.txt"), "{text}");
        assert!(!text.contains("secret.txt"), "{text}");
    }

    #[tokio::test]
    async fn child_ignore_can_whitelist_parent_ignore() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join(".ignore"), "secret.txt\n").unwrap();
        std::fs::write(dir.path().join("sub/.ignore"), "!secret.txt\n").unwrap();
        std::fs::write(dir.path().join("sub/secret.txt"), "secret\n").unwrap();
        std::fs::write(dir.path().join("sub/kept.txt"), "kept\n").unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "*"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(text.lines().any(|line| line == "sub/kept.txt"), "{text}");
        assert!(text.lines().any(|line| line == "sub/secret.txt"), "{text}");
    }

    #[tokio::test]
    async fn unicode_filenames_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("日本語");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("ünïcode.md"), "x").unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "**/*.md"}), &ctx)
            .await
            .unwrap();
        assert_eq!(text_of(&result).as_bytes(), "日本語/ünïcode.md".as_bytes());
        let details = result.details.unwrap();
        assert_eq!(details["matches"], 1);
        assert_eq!(details["shown"], 1);
        assert_eq!(details["truncated"], false);
        assert_eq!(details["limit"], DEFAULT_LIMIT);
    }

    #[tokio::test]
    async fn no_matches_is_an_empty_result_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "*.zig"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(text_of(&result), "");
        assert_eq!(result.details.unwrap()["shown"], 0);
    }

    #[tokio::test]
    async fn invalid_glob_is_invalid_args() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());

        let err = run_dyn(&FindTool, json!({"pattern": "[unclosed"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
        assert!(err.to_string().contains("pattern"), "{err}");
    }

    #[tokio::test]
    async fn nonexistent_path_is_an_execution_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());

        let err = run_dyn(
            &FindTool,
            json!({"pattern": "*", "path": "no/such/dir"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)), "{err}");
    }

    #[tokio::test]
    async fn path_escape_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());

        let err = run_dyn(
            &FindTool,
            json!({"pattern": "*", "path": "../outside"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
        assert!(err.to_string().contains("escapes"), "{err}");

        let outside = dir.path().parent().unwrap().to_path_buf();
        let err = run_dyn(
            &FindTool,
            json!({"pattern": "*", "path": outside.display().to_string()}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_entries_are_never_reported() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.txt"), "x").unwrap();
        symlink("real.txt", dir.path().join("link.txt")).unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "*.txt"}), &ctx)
            .await
            .unwrap();
        assert_eq!(text_of(&result), "real.txt");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn verbatim_cwd_single_file_stays_cwd_relative() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        // CLI-style canonicalized (`\\?\C:\…`) session cwd: the
        // single-file report must still render the cwd-relative path.
        let ctx = ctx_at(&dir.path().canonicalize().unwrap());

        let result = run_dyn(
            &FindTool,
            json!({"pattern": "src/*.rs", "path": "src/main.rs"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(text_of(&result), "src/main.rs");
    }

    /// A matching directory replaced after enumeration is not reported:
    /// containment is decided from the one opened handle, never the name.
    #[cfg(any(unix, windows))]
    #[test]
    fn enumerated_object_replacement_is_rejected() {
        use std::sync::Barrier;

        let allowed = tempfile::tempdir().unwrap();
        let victim = allowed.path().join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        let root = resolve_search_root(allowed.path(), None).unwrap();
        let reached = Arc::new(Barrier::new(2));
        let replaced = Arc::new(Barrier::new(2));
        let hooks = FindHooks {
            before_open: Some({
                let reached = Arc::clone(&reached);
                let replaced = Arc::clone(&replaced);
                Arc::new(move |path| {
                    if path.ends_with("victim") {
                        reached.wait();
                        replaced.wait();
                    }
                })
            }),
        };
        let worker = std::thread::spawn(move || {
            let glob = globset::Glob::new("*").unwrap().compile_matcher();
            run_find_with_hooks(
                glob,
                root,
                None,
                &CancellationToken::new(),
                &Limits::default(),
                &hooks,
            )
        });

        reached.wait();
        std::fs::remove_dir(&victim).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), &victim).unwrap();
        #[cfg(windows)]
        junction::create(outside.path(), &victim).unwrap();
        replaced.wait();
        let result = unwrap_tool(worker.join().unwrap());
        let text = text_of(&result);
        assert!(!text.lines().any(|line| line == "victim"), "{text}");
        assert!(!text.contains("secret.txt"), "{text}");
        #[cfg(windows)]
        junction::delete(&victim).unwrap();
    }

    /// Replacing the selected root with a link to another allowed directory
    /// cannot make find report objects outside the retained selected root.
    #[cfg(any(unix, windows))]
    #[test]
    fn selected_root_replacement_cannot_redirect_within_allowed_root() {
        let allowed = tempfile::tempdir().unwrap();
        let scan = allowed.path().join("scan");
        std::fs::create_dir_all(&scan).unwrap();
        let redirected = allowed.path().join("redirected");
        std::fs::create_dir_all(&redirected).unwrap();
        std::fs::write(redirected.join("secret.txt"), "secret").unwrap();
        let root = resolve_search_root(allowed.path(), Some("scan")).unwrap();
        std::fs::rename(&scan, allowed.path().join("retained")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&redirected, &scan).unwrap();
        #[cfg(windows)]
        junction::create(&redirected, &scan).unwrap();

        let glob = globset::Glob::new("*").unwrap().compile_matcher();
        let result = unwrap_tool(run_find(
            glob,
            root,
            None,
            &CancellationToken::new(),
            &Limits::default(),
        ));
        assert!(
            !text_of(&result).contains("secret.txt"),
            "{}",
            text_of(&result)
        );
        assert_eq!(result.details.as_ref().unwrap()["matches"], 0);
        assert_no_diagnostic_needle(&result, "secret.txt");
        #[cfg(windows)]
        junction::delete(&scan).unwrap();
    }

    /// Replacement-tree `.gitignore` must not un-ignore a same-named file
    /// that the retained tree's ignore rules hide.
    #[cfg(any(unix, windows))]
    #[test]
    fn selected_root_replacement_cannot_apply_replacement_gitignore() {
        let allowed = tempfile::tempdir().unwrap();
        let scan = allowed.path().join("scan");
        std::fs::create_dir_all(scan.join(".git")).unwrap();
        std::fs::write(scan.join(".gitignore"), "secret.txt\n").unwrap();
        std::fs::write(scan.join("secret.txt"), "RETAINED_SECRET\n").unwrap();
        std::fs::write(scan.join("kept.txt"), "kept\n").unwrap();
        let redirected = allowed.path().join("redirected");
        std::fs::create_dir_all(redirected.join(".git")).unwrap();
        std::fs::write(redirected.join(".gitignore"), "\n").unwrap();
        std::fs::write(redirected.join("secret.txt"), "REPLACEMENT_SECRET\n").unwrap();
        std::fs::write(redirected.join("kept.txt"), "kept\n").unwrap();
        let root = resolve_search_root(allowed.path(), Some("scan")).unwrap();
        std::fs::rename(&scan, allowed.path().join("retained")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&redirected, &scan).unwrap();
        #[cfg(windows)]
        junction::create(&redirected, &scan).unwrap();

        let glob = globset::Glob::new("*").unwrap().compile_matcher();
        let result = unwrap_tool(run_find(
            glob,
            root,
            None,
            &CancellationToken::new(),
            &Limits::default(),
        ));
        let text = text_of(&result);
        assert!(text.lines().any(|line| line == "kept.txt"), "{text}");
        assert!(!text.contains("secret.txt"), "{text}");
        assert!(!text.contains("RETAINED_SECRET"), "{text}");
        assert!(!text.contains("REPLACEMENT_SECRET"), "{text}");
        assert_no_diagnostic_needle(&result, "secret.txt");
        #[cfg(windows)]
        junction::delete(&scan).unwrap();
    }

    #[tokio::test]
    async fn cancellation_token_aborts_the_find() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let ctx = ToolCtx::new(dir.path()).with_cancel(cancel);

        let err = run_dyn(&FindTool, json!({"pattern": "*"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)));
        assert!(err.to_string().contains("cancelled"), "{err}");
    }

    #[tokio::test]
    async fn parallel_walks_are_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        for d in 0..6 {
            let sub = dir.path().join(format!("d{d}"));
            std::fs::create_dir_all(&sub).unwrap();
            for i in 0..6 {
                std::fs::write(sub.join(format!("f{i}.txt")), "x").unwrap();
            }
        }
        let ctx = ctx_at(dir.path());

        let r1 = run_dyn(&FindTool, json!({"pattern": "*.txt"}), &ctx)
            .await
            .unwrap();
        let first = text_of(&r1).to_owned();
        let r2 = run_dyn(&FindTool, json!({"pattern": "*.txt"}), &ctx)
            .await
            .unwrap();
        let second = text_of(&r2).to_owned();
        assert_eq!(first, second);
        assert_eq!(first.lines().count(), 36);
    }

    /// Nested directory rename plus a same-name ordinary replacement must keep
    /// parent-relative opens on the retained handle.
    #[cfg(any(unix, windows))]
    #[test]
    fn nested_directory_replacement_cannot_list_replacement_only_names() {
        use std::sync::Barrier;

        let allowed = tempfile::tempdir().unwrap();
        let nested = allowed.path().join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("old_only.txt"), "old\n").unwrap();
        let outside = tempfile::tempdir().unwrap();
        let replacement = outside.path().join("replacement");
        std::fs::create_dir_all(&replacement).unwrap();
        std::fs::write(replacement.join(".ignore"), "old_only.txt\n").unwrap();
        std::fs::write(replacement.join("new_only.txt"), "new\n").unwrap();

        let root = resolve_search_root(allowed.path(), None).unwrap();
        let reached = Arc::new(Barrier::new(2));
        let replaced = Arc::new(Barrier::new(2));
        let hooks = FindHooks {
            before_open: Some({
                let reached = Arc::clone(&reached);
                let replaced = Arc::clone(&replaced);
                Arc::new(move |path| {
                    if path.ends_with(Path::new("nested").join("old_only.txt")) {
                        reached.wait();
                        replaced.wait();
                    }
                })
            }),
        };
        let worker = std::thread::spawn(move || {
            let glob = globset::Glob::new("*").unwrap().compile_matcher();
            run_find_with_hooks(
                glob,
                root,
                None,
                &CancellationToken::new(),
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
        assert!(
            text.lines().any(|line| line == "nested/old_only.txt"),
            "{text}"
        );
        assert!(!text.contains("new_only.txt"), "{text}");
    }

    #[tokio::test]
    async fn nested_git_root_truncates_outer_git_layers_but_keeps_ignore() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git/info")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "from_gitignore.txt\n").unwrap();
        std::fs::write(dir.path().join(".git/info/exclude"), "from_exclude.txt\n").unwrap();
        std::fs::write(dir.path().join(".ignore"), "from_ignore.txt\n").unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir_all(nested.join(".git")).unwrap();
        std::fs::write(nested.join(".gitignore"), "from_nested_git.txt\n").unwrap();
        std::fs::write(nested.join("from_gitignore.txt"), "x").unwrap();
        std::fs::write(nested.join("from_exclude.txt"), "x").unwrap();
        std::fs::write(nested.join("from_ignore.txt"), "x").unwrap();
        std::fs::write(nested.join("from_nested_git.txt"), "x").unwrap();
        std::fs::write(nested.join("kept.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "*"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(text.lines().any(|line| line == "nested/kept.txt"), "{text}");
        assert!(
            text.lines().any(|line| line == "nested/from_gitignore.txt"),
            "{text}"
        );
        assert!(
            text.lines().any(|line| line == "nested/from_exclude.txt"),
            "{text}"
        );
        assert!(!text.contains("from_ignore.txt"), "{text}");
        assert!(!text.contains("from_nested_git.txt"), "{text}");
    }

    #[tokio::test]
    async fn nested_git_keeps_ordinary_ignore_whitelist_hierarchy() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".ignore"), "secret.txt\n").unwrap();
        std::fs::create_dir_all(dir.path().join("nested/.git")).unwrap();
        std::fs::write(dir.path().join("nested/.ignore"), "!secret.txt\n").unwrap();
        std::fs::write(dir.path().join("nested/secret.txt"), "x").unwrap();
        std::fs::write(dir.path().join("nested/kept.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "*"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(text.lines().any(|line| line == "nested/kept.txt"), "{text}");
        assert!(
            text.lines().any(|line| line == "nested/secret.txt"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn oversized_nested_ignore_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("kept.txt"), "x").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(
            dir.path().join("sub/.ignore"),
            vec![b'x'; IGNORE_FILE_MAX_BYTES + 1],
        )
        .unwrap();
        std::fs::write(dir.path().join("sub/secret.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "*"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(result.is_error, "{result:?}");
        assert!(text.contains("ignore boundary"), "{text}");
        assert!(text.contains("size limit"), "{text}");
        assert!(!text.contains("secret.txt"), "{text}");
    }

    #[tokio::test]
    async fn malformed_nested_ignore_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("kept.txt"), "x").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/.ignore"), "foo\\\n").unwrap();
        std::fs::write(dir.path().join("sub/secret.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&FindTool, json!({"pattern": "*"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(result.is_error, "{result:?}");
        assert!(text.contains("ignore boundary"), "{text}");
        assert!(!text.contains("secret.txt"), "{text}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_casefold_alias_honors_canonical_anchored_ignore_when_supported() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join("Secrets");
        std::fs::create_dir(&secrets).unwrap();
        std::fs::write(
            dir.path().join(".ignore"),
            "/Secrets/secret.txt
",
        )
        .unwrap();
        std::fs::write(secrets.join("secret.txt"), "x").unwrap();
        std::fs::write(secrets.join("kept.txt"), "x").unwrap();
        if !unix_casefold_alias_supported(dir.path(), "Secrets") {
            return;
        }
        let root = resolve_search_root_with_access(
            dir.path(),
            Some("secrets"),
            &CancellationToken::new(),
            &Limits::default(),
            SearchAccess::Metadata,
        )
        .unwrap();
        let glob = globset::Glob::new("*").unwrap().compile_matcher();
        let result = unwrap_tool(run_find(
            glob,
            root,
            None,
            &CancellationToken::new(),
            &Limits::default(),
        ));
        let text = text_of(&result);
        assert!(text.lines().any(|line| line == "kept.txt"), "{text}");
        assert!(!text.contains("secret.txt"), "{text}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn explicit_metadata_directory_lists_children() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/kept.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path());
        let result = run_dyn(&FindTool, json!({"pattern": "*", "path": "nested"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(!result.is_error, "{result:?}");
        assert!(text.lines().any(|line| line == "kept.txt"), "{text}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_case_alias_find_honors_on_disk_anchored_ignore() {
        let dir = tempfile::tempdir().unwrap();
        let visible = dir.path().join("Visible");
        std::fs::create_dir(&visible).unwrap();
        std::fs::write(dir.path().join(".ignore"), "/Visible/secret.txt\n").unwrap();
        std::fs::write(visible.join("secret.txt"), "x").unwrap();
        std::fs::write(visible.join("kept.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path());
        let result = run_dyn(&FindTool, json!({"pattern": "*", "path": "visible"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(text.lines().any(|line| line == "kept.txt"), "{text}");
        assert!(!text.contains("secret.txt"), "{text}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_eight_dot_three_alias_find_honors_on_disk_anchored_ignore() {
        let dir = tempfile::tempdir().unwrap();
        let visible = dir.path().join("LongVisibleName");
        std::fs::create_dir(&visible).unwrap();
        std::fs::write(dir.path().join(".ignore"), "/LongVisibleName/secret.txt\n").unwrap();
        std::fs::write(visible.join("secret.txt"), "x").unwrap();
        std::fs::write(visible.join("kept.txt"), "x").unwrap();
        let short = windows_short_path(&visible).unwrap();
        let short_name = short.file_name().unwrap().to_os_string();
        if short_name == visible.file_name().unwrap() {
            return;
        }
        assert!(short_name.to_string_lossy().contains('~'), "{short_name:?}");
        let ctx = ctx_at(dir.path());
        let result = run_dyn(
            &FindTool,
            json!({"pattern": "*", "path": short_name.to_str().unwrap()}),
            &ctx,
        )
        .await
        .unwrap();
        let text = text_of(&result);
        assert!(text.lines().any(|line| line == "kept.txt"), "{text}");
        assert!(!text.contains("secret.txt"), "{text}");
    }

    #[cfg(unix)]
    struct RestoreUnixMode {
        path: std::path::PathBuf,
        mode: u32,
    }

    #[cfg(unix)]
    impl Drop for RestoreUnixMode {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(self.mode));
        }
    }

    #[cfg(unix)]
    fn chmod(path: &Path, mode: u32) -> RestoreUnixMode {
        use std::os::unix::fs::PermissionsExt;
        let previous = std::fs::metadata(path).unwrap().permissions().mode();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        RestoreUnixMode {
            path: path.to_path_buf(),
            mode: previous,
        }
    }

    /// Discovery must not require content-read permission.
    ///
    /// A privileged process that can still `open` mode `000` paths would not
    /// prove this, so the test refuses to pass in that environment.
    #[cfg(unix)]
    #[tokio::test]
    async fn find_reports_unreadable_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("secret.txt"), "x").unwrap();
        std::fs::write(dir.path().join("kept.txt"), "x").unwrap();
        std::fs::create_dir(dir.path().join("locked")).unwrap();
        std::fs::write(dir.path().join("locked").join("inside.txt"), "x").unwrap();
        let _restore_file = chmod(&dir.path().join("secret.txt"), 0o000);
        let _restore_dir = chmod(&dir.path().join("locked"), 0o000);
        assert!(
            std::fs::File::open(dir.path().join("secret.txt")).is_err(),
            "process can open a mode 000 file; refuse to pass as root"
        );
        assert!(
            std::fs::File::open(dir.path().join("locked")).is_err(),
            "process can open a mode 000 directory; refuse to pass as root"
        );
        let ctx = ctx_at(dir.path());
        let result = run_dyn(&FindTool, json!({"pattern": "*"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(text.lines().any(|line| line == "secret.txt"), "{text}");
        assert!(text.lines().any(|line| line == "kept.txt"), "{text}");
        assert!(text.lines().any(|line| line == "locked"), "{text}");

        let explicit = run_dyn(
            &FindTool,
            json!({"pattern": "*", "path": "secret.txt"}),
            &ctx,
        )
        .await
        .unwrap();
        assert!(
            text_of(&explicit).lines().any(|line| line == "secret.txt"),
            "{}",
            text_of(&explicit)
        );
    }

    /// Permission errors probing `.git` must fail closed, not skip gitignore.
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "opt-in privileged FS fixture; set MYCODE_PRIVILEGED_FS_TESTS=1"]
    async fn unreadable_root_git_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join(".git");
        std::fs::create_dir(&git).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "secret.txt\n").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "x").unwrap();
        let _restore = chmod(&git, 0o000);
        assert!(
            std::fs::File::open(&git).is_err(),
            "fixture requested unreadable .git but the process can still open it",
        );
        let ctx = ctx_at(dir.path());
        let err = run_dyn(&FindTool, json!({"pattern": "*"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)), "{err}");
        assert!(err.to_string().contains("ignore"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "opt-in privileged FS fixture; set MYCODE_PRIVILEGED_FS_TESTS=1"]
    async fn unreadable_root_git_info_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let info = dir.path().join(".git/info");
        std::fs::create_dir_all(&info).unwrap();
        std::fs::write(info.join("exclude"), "from_exclude.txt\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "from_gitignore.txt\n").unwrap();
        std::fs::write(dir.path().join("from_exclude.txt"), "x").unwrap();
        let _restore = chmod(&info, 0o000);
        assert!(
            std::fs::File::open(&info).is_err(),
            "fixture requested unreadable .git/info but the process can still open it",
        );
        let ctx = ctx_at(dir.path());
        let err = run_dyn(&FindTool, json!({"pattern": "*"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)), "{err}");
        assert!(err.to_string().contains("ignore"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "opt-in privileged FS fixture; set MYCODE_PRIVILEGED_FS_TESTS=1"]
    async fn unreadable_nested_git_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("kept.txt"), "x").unwrap();
        let nested = dir.path().join("sub");
        std::fs::create_dir(&nested).unwrap();
        let git = nested.join(".git");
        std::fs::create_dir(&git).unwrap();
        std::fs::write(
            nested.join(".gitignore"),
            "secret.txt
",
        )
        .unwrap();
        std::fs::write(nested.join("secret.txt"), "x").unwrap();
        std::fs::write(nested.join("visible.txt"), "x").unwrap();
        let _restore = chmod(&git, 0o000);
        assert!(
            std::fs::File::open(&git).is_err(),
            "fixture requested unreadable nested .git but the process can still open it",
        );
        let ctx = ctx_at(dir.path());
        let result = run_dyn(&FindTool, json!({"pattern": "*"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(result.is_error, "{result:?}");
        assert!(text.contains("ignore boundary"), "{text}");
        assert!(!text.contains("secret.txt"), "{text}");
        assert!(!text.contains("visible.txt"), "{text}");
    }

    #[tokio::test]
    async fn explicit_ignored_and_hidden_file_targets_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "secret.txt\n").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "x").unwrap();
        std::fs::write(dir.path().join(".hidden.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path());

        let ignored = run_dyn(
            &FindTool,
            json!({"pattern": "*", "path": "secret.txt"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(text_of(&ignored), "");

        let hidden = run_dyn(
            &FindTool,
            json!({"pattern": "*", "path": ".hidden.txt"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(text_of(&hidden), "");
    }

    #[tokio::test]
    async fn pattern_nul_and_size_limits_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());
        let nul = run_dyn(&FindTool, json!({"pattern": "a\u{0000}b"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(nul, ToolError::InvalidArgs(_)), "{nul}");
        assert!(nul.to_string().contains("NUL"), "{nul}");

        let huge = "a".repeat(MAX_PATTERN_BYTES + 1);
        let over = run_dyn(&FindTool, json!({"pattern": huge}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(over, ToolError::InvalidArgs(_)), "{over}");
    }

    #[tokio::test]
    async fn reverse_directory_enumeration_keeps_the_same_top_n() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["z.txt", "a.txt", "m.txt"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        let run = |reverse: bool| {
            let limits = Limits {
                reverse_dir_enum: reverse,
                ..Limits::default()
            };
            let glob = globset::Glob::new("*.txt").unwrap().compile_matcher();
            let root =
                resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits)
                    .unwrap();
            let result = unwrap_tool(run_find(
                glob,
                root,
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
            "a.txt\nm.txt\n[showing first 2 of 3 matching paths; refine the pattern or raise limit]"
        );
    }

    /// Two distinct non-UTF-8 names can share one replacement display.
    /// Low `limit` plus reversed OS order must still return the globally
    /// smallest rendered key, with the original `OsString` as tie-break.
    // APFS rejects invalid UTF-8 byte names with EILSEQ; Linux provides this fixture.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn non_utf8_names_use_rendered_key_and_os_tie_break() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(OsStr::from_bytes(b"\x80.txt")), "x").unwrap();
        std::fs::write(dir.path().join(OsStr::from_bytes(b"\x81.txt")), "x").unwrap();
        std::fs::write(dir.path().join("\u{00ff}.txt"), "x").unwrap();
        let run = |reverse: bool| {
            let limits = Limits {
                reverse_dir_enum: reverse,
                ..Limits::default()
            };
            let glob = globset::Glob::new("*.txt").unwrap().compile_matcher();
            let root =
                resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits)
                    .unwrap();
            let result = unwrap_tool(run_find(
                glob,
                root,
                Some(1),
                &CancellationToken::new(),
                &limits,
            ));
            text_of(&result).to_owned()
        };
        let first = run(false);
        let second = run(true);
        let expected = "\u{00ff}.txt";
        assert_eq!(first.lines().next(), Some(expected), "{first}");
        assert_eq!(first, second);
    }

    // APFS rejects invalid UTF-8 byte names with EILSEQ; Linux provides this fixture.
    #[cfg(target_os = "linux")]
    #[test]
    fn non_utf8_listing_visits_smallest_rendered_key_first() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(OsStr::from_bytes(b"\x80.txt")), "x").unwrap();
        std::fs::write(dir.path().join(OsStr::from_bytes(b"\x81.txt")), "x").unwrap();
        std::fs::write(dir.path().join("\u{00ff}.txt"), "x").unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let hooks = FindHooks {
            before_open: Some({
                let seen = Arc::clone(&seen);
                Arc::new(move |path: &Path| {
                    if let Some(name) = path.file_name() {
                        seen.lock().expect("visit log").push(name.to_os_string());
                    }
                })
            }),
        };
        let glob = globset::Glob::new("*.txt").unwrap().compile_matcher();
        let limits = Limits {
            reverse_dir_enum: true,
            ..Limits::default()
        };
        let root = resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits)
            .unwrap();
        let _ = unwrap_tool(run_find_with_hooks(
            glob,
            root,
            Some(1),
            &CancellationToken::new(),
            &limits,
            &hooks,
        ));
        let seen = seen.lock().expect("visit log");
        assert_eq!(
            seen.first()
                .map(|name| name.to_string_lossy().into_owned())
                .as_deref(),
            Some("\u{00ff}.txt"),
            "{seen:?}"
        );
    }

    #[tokio::test]
    async fn walk_depth_limit_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mut nested = dir.path().to_path_buf();
        for index in 0..8 {
            nested.push(format!("d{index}"));
        }
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("leaf.txt"), "x").unwrap();
        let context = ctx_at(dir.path());
        let glob = globset::Glob::new("*").unwrap().compile_matcher();
        let limits = Limits {
            max_walk_depth: 3,
            ..Limits::default()
        };
        let root =
            resolve_search_root_cancel(&context.cwd, None, &context.cancel, &limits).unwrap();
        let result = unwrap_tool(run_find(glob, root, None, &context.cancel, &limits));
        assert!(!text_of(&result).contains("leaf.txt"));
        let details = result.details.as_ref().unwrap();
        assert_eq!(details["stopped_early"], "walk depth limit reached");
    }

    #[tokio::test]
    async fn walk_width_limit_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..6 {
            std::fs::write(dir.path().join(format!("f{index}.txt")), "x").unwrap();
        }
        let context = ctx_at(dir.path());
        let glob = globset::Glob::new("*").unwrap().compile_matcher();
        let limits = Limits {
            max_dir_width: 3,
            ..Limits::default()
        };
        let root =
            resolve_search_root_cancel(&context.cwd, None, &context.cancel, &limits).unwrap();
        let result = unwrap_tool(run_find(glob, root, None, &context.cancel, &limits));
        let details = result.details.unwrap();
        assert_eq!(details["stopped_early"], "directory width limit reached");
    }

    #[test]
    fn time_limit_stop_is_an_execution_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let limits = Limits {
            deadline: Some(Instant::now() - std::time::Duration::from_secs(1)),
            ..Limits::default()
        };
        let glob = globset::Glob::new("*").unwrap().compile_matcher();
        let error = match resolve_search_root_cancel(
            dir.path(),
            None,
            &CancellationToken::new(),
            &limits,
        ) {
            Err(error) => error,
            Ok(root) => run_find(glob, root, None, &CancellationToken::new(), &limits)
                .expect_err("expired deadline must not publish a partial report"),
        };
        assert!(
            error.to_string().contains("time limit") || error.to_string().contains("ignore"),
            "{error}"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn explicit_windows_file_alias_find_honors_ignore() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".ignore"), "Visible.txt\n").unwrap();
        std::fs::write(dir.path().join("Visible.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path());
        let result = run_dyn(
            &FindTool,
            json!({"pattern": "*", "path": "visible.txt"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(text_of(&result), "");

        let long = dir.path().join("LongIgnoredName.txt");
        std::fs::write(dir.path().join(".ignore"), "LongIgnoredName.txt\n").unwrap();
        std::fs::write(&long, "x").unwrap();
        let short = windows_short_path(&long).unwrap();
        let short_name = short.file_name().unwrap().to_os_string();
        if short_name == long.file_name().unwrap() {
            return;
        }
        let result = run_dyn(
            &FindTool,
            json!({"pattern": "*", "path": short_name.to_str().unwrap()}),
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
        std::fs::write(dir.path().join("file.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path());
        let err = run_dyn(
            &FindTool,
            json!({"pattern": "*", "path": "file.txt:hidden"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");

        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        let err = run_dyn(
            &FindTool,
            json!({"pattern": "*", "path": "subdir:stream"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    }

    #[tokio::test]
    async fn prefix_sibling_is_global_top_n() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        std::fs::write(dir.path().join("a").join("hit.txt"), "x").unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path());
        let result = run_dyn(&FindTool, json!({"pattern": "*.txt", "limit": 1}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(text.starts_with("a.txt"), "{text}");
        assert!(!text.contains("hit.txt"), "{text}");
    }

    #[tokio::test]
    async fn ignore_budget_is_shared_across_resolve_and_walk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".ignore"), "rootskip\n").unwrap();
        let mut nested = dir.path().to_path_buf();
        for index in 0..6 {
            nested.push(format!("d{index}"));
            std::fs::create_dir_all(&nested).unwrap();
            std::fs::write(nested.join(".ignore"), format!("skip{index}\n")).unwrap();
        }
        std::fs::write(nested.join("leaf.txt"), "x").unwrap();
        let context = ctx_at(dir.path());
        let limits = Limits {
            max_ignore_layers: 3,
            ..Limits::default()
        };
        let glob = globset::Glob::new("*").unwrap().compile_matcher();
        let root =
            resolve_search_root_cancel(&context.cwd, None, &context.cancel, &limits).unwrap();
        let charged = root.limiter.ignore_layers();
        assert!(charged >= 1, "resolve must charge ignore layers");
        let result = unwrap_tool(run_find(glob, root, None, &context.cancel, &limits));
        assert!(result.is_error, "{result:?}");
        assert!(
            text_of(&result).contains("layer limit reached"),
            "{}",
            text_of(&result)
        );
    }

    #[tokio::test]
    async fn handle_budget_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mut nested = dir.path().to_path_buf();
        for index in 0..6 {
            nested.push(format!("d{index}"));
            std::fs::create_dir_all(&nested).unwrap();
            std::fs::write(nested.join("f.txt"), "x").unwrap();
        }
        let context = ctx_at(dir.path());
        let limits = Limits {
            max_open_handles: 3,
            max_walk_depth: 16,
            ..Limits::default()
        };
        let glob = globset::Glob::new("*").unwrap().compile_matcher();
        let root =
            resolve_search_root_cancel(&context.cwd, None, &context.cancel, &limits).unwrap();
        assert_eq!(root.limiter.live_handles(), 2);
        let result = unwrap_tool(run_find(glob, root, None, &context.cancel, &limits));
        let details = result.details.unwrap();
        assert_eq!(details["stopped_early"], "handle budget reached");
    }

    #[tokio::test]
    async fn empty_sibling_directories_release_handles() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..32 {
            std::fs::create_dir(dir.path().join(format!("d{index:02}"))).unwrap();
        }
        std::fs::write(dir.path().join("z.txt"), "x").unwrap();
        let context = ctx_at(dir.path());
        let limits = Limits {
            max_open_handles: 4,
            max_walk_depth: 16,
            ..Limits::default()
        };
        let glob = globset::Glob::new("z.txt").unwrap().compile_matcher();
        let root =
            resolve_search_root_cancel(&context.cwd, None, &context.cancel, &limits).unwrap();
        assert_eq!(root.limiter.live_handles(), 2);
        let result = unwrap_tool(run_find(glob, root, None, &context.cancel, &limits));
        let text = text_of(&result);
        assert!(text.contains("z.txt"), "{text}");
        assert!(!result.is_error, "{result:?}");
        if let Some(details) = result.details.as_ref() {
            assert_ne!(
                details
                    .get("stopped_early")
                    .and_then(|value| value.as_str()),
                Some("handle budget reached"),
                "{details}"
            );
        }
    }

    #[tokio::test]
    async fn prepared_root_survives_on_disk_replacement() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("safe")).unwrap();
        std::fs::write(dir.path().join("safe").join("keep.txt"), "x").unwrap();
        std::fs::create_dir(dir.path().join("secrets")).unwrap();
        std::fs::write(dir.path().join("secrets").join("leak.txt"), "x").unwrap();
        let prepared =
            crate::prepare_search(dir.path(), Some("safe"), &CancellationToken::new()).unwrap();
        std::fs::rename(dir.path().join("safe"), dir.path().join("safe.bak")).unwrap();
        std::fs::rename(dir.path().join("secrets"), dir.path().join("safe")).unwrap();
        let root = prepared.take_root().unwrap();
        let glob = globset::Glob::new("*.txt").unwrap().compile_matcher();
        let result = unwrap_tool(run_find(
            glob,
            root,
            None,
            &CancellationToken::new(),
            &Limits::default(),
        ));
        let text = text_of(&result);
        assert!(text.contains("keep.txt"), "{text}");
        assert!(!text.contains("leak.txt"), "{text}");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "opt-in privileged FS fixture; set MYCODE_PRIVILEGED_FS_TESTS=1"]
    async fn find_from_parent_skips_bind_mount_directory() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "SECRET_BIND\n").unwrap();
        let allowed = tempfile::tempdir().unwrap();
        std::fs::write(allowed.path().join("visible.txt"), "x").unwrap();
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
        let _umount = Umount(mount);
        let ctx = ctx_at(allowed.path());
        let result = run_dyn(&FindTool, json!({"pattern": "*"}), &ctx)
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(text.contains("visible.txt"), "{text}");
        assert!(!text.contains("mnt"), "{text}");
        assert!(!text.contains("secret.txt"), "{text}");
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "opt-in privileged FS fixture; set MYCODE_PRIVILEGED_FS_TESTS=1"]
    async fn explicit_attribute_only_file_is_reported() {
        struct RestoreAcl {
            path: std::path::PathBuf,
            user: String,
        }
        impl Drop for RestoreAcl {
            fn drop(&mut self) {
                let _ = std::process::Command::new("icacls.exe")
                    .arg(&self.path)
                    .arg("/grant:r")
                    .arg(format!("{}:(F)", self.user))
                    .output();
            }
        }

        let user = std::env::var_os("USERNAME")
            .expect("USERNAME is required for the ACL fixture")
            .to_string_lossy()
            .into_owned();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("attributes-only.txt");
        std::fs::write(&file, "content read must be denied").unwrap();
        let status = std::process::Command::new("icacls.exe")
            .arg(&file)
            .arg("/inheritance:r")
            .arg("/grant:r")
            .arg(format!("{user}:(RA)"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "fixture requested an attributes-only ACL but icacls failed: {status}"
        );
        let _restore = RestoreAcl {
            path: file.clone(),
            user,
        };
        assert!(
            std::fs::File::open(&file).is_err(),
            "fixture must deny content-read access"
        );

        let ctx = ctx_at(dir.path());
        let result = run_dyn(
            &FindTool,
            json!({"pattern": "*", "path": "attributes-only.txt"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(text_of(&result), "attributes-only.txt");
    }

    #[cfg(windows)]
    #[test]
    fn hidden_after_listing_is_neither_reported_nor_descended() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, SetFileAttributesW};

        fn set_hidden(path: &Path) {
            let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
            wide.push(0);
            // SAFETY: `wide` is a live NUL-terminated UTF-16 path.
            let ok = unsafe { SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_HIDDEN) };
            assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
        }

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("late.txt"), "x").unwrap();
        std::fs::create_dir(dir.path().join("late-dir")).unwrap();
        std::fs::write(dir.path().join("late-dir/child.txt"), "x").unwrap();
        let root = resolve_search_root(dir.path(), None).unwrap();
        let hooks = FindHooks {
            before_open: Some(Arc::new(|path| {
                if path.ends_with("late.txt") || path.ends_with("late-dir") {
                    set_hidden(path);
                }
            })),
        };
        let glob = globset::Glob::new("*").unwrap().compile_matcher();
        let result = unwrap_tool(run_find_with_hooks(
            glob,
            root,
            None,
            &CancellationToken::new(),
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
    async fn explicit_hidden_file_and_directory_are_skipped() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, SetFileAttributesW};

        let dir = tempfile::tempdir().unwrap();
        let hidden_file = dir.path().join("HiddenFile.txt");
        std::fs::write(&hidden_file, "x").unwrap();
        let hidden_dir = dir.path().join("HiddenDir");
        std::fs::create_dir(&hidden_dir).unwrap();
        std::fs::write(hidden_dir.join("child.txt"), "x").unwrap();
        set_hidden(&hidden_file);
        set_hidden(&hidden_dir);
        let ctx = ctx_at(dir.path());

        let file = run_dyn(
            &FindTool,
            json!({"pattern": "*", "path": "HiddenFile.txt"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(text_of(&file), "");

        let nested = run_dyn(
            &FindTool,
            json!({"pattern": "*", "path": "HiddenDir/child.txt"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(text_of(&nested), "");

        let short = crate::builtin::fs_search::windows_short_path(&hidden_file).unwrap();
        let short_name = short.file_name().unwrap().to_os_string();
        if short_name != hidden_file.file_name().unwrap() {
            let aliased = run_dyn(
                &FindTool,
                json!({"pattern": "*", "path": short_name.to_str().unwrap()}),
                &ctx,
            )
            .await
            .unwrap();
            assert_eq!(text_of(&aliased), "");
        }

        fn set_hidden(path: &Path) {
            let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
            wide.push(0);
            // SAFETY: `wide` is a live NUL-terminated UTF-16 path.
            let ok = unsafe { SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_HIDDEN) };
            assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
        }
    }

    #[test]
    fn result_store_byte_budget_truncates_deep_paths() {
        let dir = tempfile::tempdir().unwrap();
        let deep = "n".repeat(200);
        std::fs::write(dir.path().join(&deep), "x").unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let limits = Limits {
            max_result_bytes: 32,
            ..Limits::default()
        };
        let glob = globset::Glob::new("*").unwrap().compile_matcher();
        let root = resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits)
            .unwrap();
        let result = unwrap_tool(run_find(
            glob,
            root,
            None,
            &CancellationToken::new(),
            &limits,
        ));
        let details = result.details.unwrap();
        assert_eq!(details["stopped_early"], "result store limit reached");
        assert!(details["truncated"].as_bool().unwrap());
    }

    #[test]
    fn mount_identity_mismatch_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("mnt")).unwrap();
        std::fs::write(dir.path().join("mnt/secret.txt"), "x").unwrap();
        let limits = Limits {
            child_device_override: Some(crate::builtin::fs_search::ChildDeviceOverride(Arc::new(
                |name| {
                    if name == std::ffi::OsStr::new("mnt") {
                        Some(0xDEAD_BEEF)
                    } else {
                        None
                    }
                },
            ))),
            ..Limits::default()
        };
        let error =
            resolve_search_root_cancel(dir.path(), Some("mnt"), &CancellationToken::new(), &limits)
                .unwrap_err();
        assert!(
            error.to_string().contains("mount") || error.to_string().contains("link"),
            "{error}"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn openat2_policy_is_observed_for_metadata_and_content() {
        use crate::builtin::fs_search::{
            AccessGate, ObservedOpen, resolve_search_root_with_access,
        };

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("policy.txt"), "x").unwrap();
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed_gate = Arc::clone(&observed);
        let limits = Limits {
            access_gate: Some(AccessGate(Arc::new(move |name, open: ObservedOpen| {
                if name == std::ffi::OsStr::new("policy.txt") {
                    observed_gate
                        .lock()
                        .expect("openat2 policy log")
                        .push((open.access, open.resolve));
                }
                Ok(())
            }))),
            ..Limits::default()
        };
        for access in [SearchAccess::Metadata, SearchAccess::Content] {
            let root = resolve_search_root_with_access(
                dir.path(),
                Some("policy.txt"),
                &CancellationToken::new(),
                &limits,
                access,
            )
            .unwrap();
            drop(root);
        }
        let observed = observed.lock().expect("openat2 policy log");
        // Independent values from linux/openat2.h: NO_XDEV, NO_SYMLINKS,
        // and BENEATH. Do not reuse the production constant in this gate.
        for access in [SearchAccess::Metadata, SearchAccess::Content] {
            assert!(
                observed
                    .iter()
                    .any(|&(actual, resolve)| actual == access && resolve == (0x01 | 0x04 | 0x08)),
                "missing {access:?} openat2 policy observation: {observed:?}"
            );
        }
    }

    #[test]
    fn metadata_open_succeeds_when_content_is_denied() {
        use crate::builtin::fs_search::{
            AccessGate, ObservedOpen, resolve_search_root_with_access,
        };

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("acl.txt"), "content read must be denied").unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_gate = Arc::clone(&seen);
        let limits = Limits {
            access_gate: Some(AccessGate(Arc::new(move |name, observed: ObservedOpen| {
                seen_gate
                    .lock()
                    .expect("access log")
                    .push((name.to_os_string(), observed.access));
                if name == std::ffi::OsStr::new("acl.txt")
                    && observed.access == SearchAccess::Content
                {
                    return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
                }
                #[cfg(any(target_os = "linux", target_os = "android"))]
                if name == std::ffi::OsStr::new("acl.txt") {
                    assert_eq!(
                        observed.resolve,
                        0x01 | 0x04 | 0x08,
                        "openat2 must set NO_XDEV, NO_SYMLINKS, and BENEATH"
                    );
                    if observed.access == SearchAccess::Metadata {
                        assert!(
                            observed.flags & libc::O_PATH != 0,
                            "metadata open must use O_PATH, flags={:#x}",
                            observed.flags
                        );
                    }
                }
                #[cfg(windows)]
                if name == std::ffi::OsStr::new("acl.txt")
                    && observed.access == SearchAccess::Metadata
                {
                    assert_eq!(
                        observed.desired_access,
                        windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES,
                        "metadata open must use FILE_READ_ATTRIBUTES"
                    );
                    assert_ne!(
                        observed.options
                            & windows_sys::Wdk::Storage::FileSystem::FILE_OPEN_REPARSE_POINT,
                        0,
                        "metadata open must not follow reparse points"
                    );
                }
                Ok(())
            }))),
            ..Limits::default()
        };
        let root = resolve_search_root_with_access(
            dir.path(),
            Some("acl.txt"),
            &CancellationToken::new(),
            &limits,
            SearchAccess::Metadata,
        )
        .unwrap();
        let glob = globset::Glob::new("*").unwrap().compile_matcher();
        let result = unwrap_tool(run_find(
            glob,
            root,
            None,
            &CancellationToken::new(),
            &limits,
        ));
        assert_eq!(text_of(&result), "acl.txt");
        let log = seen.lock().expect("access log");
        assert!(
            log.iter()
                .any(|(name, access)| name == std::ffi::OsStr::new("acl.txt")
                    && *access == SearchAccess::Metadata),
            "{log:?}"
        );
        assert!(
            !log.iter()
                .any(|(name, access)| name == std::ffi::OsStr::new("acl.txt")
                    && *access == SearchAccess::Content),
            "content open must not succeed for acl.txt: {log:?}"
        );
    }
}
