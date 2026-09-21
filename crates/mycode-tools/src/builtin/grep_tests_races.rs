//! Replacement-race and budget tests split from `grep_tests`.
use super::tests::*;
use super::*;
use crate::builtin::test_support::{ctx_at, run_dyn, text_of, unwrap_tool};
use serde_json::json;
use std::path::Path;
use tokio_util::sync::CancellationToken;

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
        let root = resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits)
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
        let root = resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits)
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
        let root = resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits)
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
    let root =
        resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits).unwrap();
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
    let root =
        resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits).unwrap();
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
    let root =
        resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits).unwrap();
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
    let root =
        resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits).unwrap();
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
