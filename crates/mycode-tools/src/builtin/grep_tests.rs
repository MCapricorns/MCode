use super::*;
use crate::builtin::test_support::{ctx_at, run_dyn, text_of, unwrap_tool};
use serde_json::json;
use std::path::Path;
use tokio_util::sync::CancellationToken;

pub(crate) fn assert_no_diagnostic_needle(result: &ToolResult, needle: &str) {
    if let Some(details) = result.details.as_ref() {
        let rendered = details.to_string();
        assert!(!rendered.contains(needle), "{details}");
    }
}

pub(crate) fn fixture(dir: &Path) {
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
    // …and the single-file report renders the cwd-relative posix
    // path (`src/util.rs`), the same base as walk-mode results.
    let first = text.lines().next().unwrap();
    assert!(first.starts_with("src/util.rs:"), "{text}");
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
