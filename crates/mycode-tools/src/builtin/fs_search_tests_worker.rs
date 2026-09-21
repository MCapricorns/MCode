//! Blocking-worker runtime tests split from `fs_search_tests`.
use super::tests::*;
use super::*;

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
            let status =
                unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &self.0, std::ptr::null_mut()) };
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
    let error = walk::read_ignore_file_for_test(&handle.file, &limiter, &CancellationToken::new())
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
#[ignore = "opt-in privileged FS fixture; set MYCODE_PRIVILEGED_FS_TESTS=1"]
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
