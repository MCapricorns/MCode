//! Identity, hidden-state, and path-spelling tests split from `fs_search_tests`.
use super::tests::*;
use super::*;

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
    const CHILD_ENV: &str = "MYCODE_FS_SEARCH_SIGURG_CHILD";
    const TEST_NAME: &str =
        "builtin::fs_search::tests_security::foreign_sigurg_handler_fails_closed_and_is_restored";
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
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
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
    const CHILD_ENV: &str = "MYCODE_FS_SEARCH_SIGURG_REPLACE_CHILD";
    const TEST_NAME: &str =
        "builtin::fs_search::tests_security::replaced_sigurg_is_not_restored_and_worker_stops";
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
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
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
