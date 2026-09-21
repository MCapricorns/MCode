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
pub(crate) struct RestoreUnixMode {
    path: std::path::PathBuf,
    mode: u32,
}

#[cfg(unix)]
impl Drop for RestoreUnixMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(self.mode));
    }
}

#[cfg(unix)]
pub(crate) fn chmod(path: &Path, mode: u32) -> RestoreUnixMode {
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
