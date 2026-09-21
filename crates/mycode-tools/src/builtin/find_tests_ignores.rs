//! Ignore-boundary, hidden-target, and enumeration tests split from `find_tests`.
#[cfg(unix)]
use super::tests::chmod;
use super::*;
use crate::builtin::test_support::{ctx_at, run_dyn, text_of, unwrap_tool};
use serde_json::json;
use tokio_util::sync::CancellationToken;

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
        let root = resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits)
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
        let root = resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits)
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
    let root =
        resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits).unwrap();
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
    let root = resolve_search_root_cancel(&context.cwd, None, &context.cancel, &limits).unwrap();
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
    let root = resolve_search_root_cancel(&context.cwd, None, &context.cancel, &limits).unwrap();
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
    let error =
        match resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits) {
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
    let root = resolve_search_root_cancel(&context.cwd, None, &context.cancel, &limits).unwrap();
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
    let root = resolve_search_root_cancel(&context.cwd, None, &context.cancel, &limits).unwrap();
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
    let root = resolve_search_root_cancel(&context.cwd, None, &context.cancel, &limits).unwrap();
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
    let root =
        resolve_search_root_cancel(dir.path(), None, &CancellationToken::new(), &limits).unwrap();
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
    use crate::builtin::fs_search::{AccessGate, ObservedOpen, resolve_search_root_with_access};

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
    use crate::builtin::fs_search::{AccessGate, ObservedOpen, resolve_search_root_with_access};

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
            if name == std::ffi::OsStr::new("acl.txt") && observed.access == SearchAccess::Content {
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
            if name == std::ffi::OsStr::new("acl.txt") && observed.access == SearchAccess::Metadata
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
