//! Spawn-gate tests for structured exec.
use std::sync::mpsc;
use std::time::Duration;

use super::*;

struct CleanupPin(Arc<AtomicBool>);

impl Drop for CleanupPin {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct CleanupOwner {
    _lease: crate::builtin::process::ExecutionLease,
    _pin: CleanupPin,
}

#[tokio::test]
async fn failed_live_cleanup_retains_owner_until_supervised_retry_succeeds() {
    let lease = crate::builtin::process::acquire_execution_lease().await;
    let pin_released = Arc::new(AtomicBool::new(false));
    let worker_pin_released = Arc::clone(&pin_released);
    let (failed_tx, failed_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let cleanup = tokio::task::spawn_blocking(move || {
        let owner = CleanupOwner {
            _lease: lease,
            _pin: CleanupPin(worker_pin_released),
        };
        let mut attempts = 0;
        let mut failed_tx = Some(failed_tx);
        finish_owned_spawn_cleanup(owner, |_| {
            attempts += 1;
            if attempts == 1 {
                if let Some(failed_tx) = failed_tx.take() {
                    let _ = failed_tx.send(());
                }
                return Err(std::io::Error::other("injected teardown failure"));
            }
            let _ = release_rx.recv();
            Ok(())
        });
    });

    failed_rx.await.expect("first cleanup attempt");
    let contender = tokio::spawn(crate::builtin::process::acquire_execution_lease());
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!pin_released.load(Ordering::Acquire));
    assert!(
        !contender.is_finished(),
        "failed teardown released the execution lease before confirmed cleanup"
    );

    release_tx.send(()).expect("release retry");
    cleanup.await.expect("cleanup worker");
    assert!(pin_released.load(Ordering::Acquire));
    drop(contender.await.expect("lease contender"));
}

#[tokio::test]
async fn deadline_cancels_blocking_spawn_worker_before_launch() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("spawned");
    let worker_marker = marker.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();

    let lease = crate::builtin::process::acquire_execution_lease().await;
    let runner = tokio::spawn(async move {
        let cancel = CancellationToken::new();
        let deadline = tokio::time::sleep(Duration::from_millis(100));
        tokio::pin!(deadline);
        wait_for_spawn(
            move |gate| {
                let _lease = lease;
                let _ = started_tx.send(());
                let _ = release_rx.recv();
                if gate.begin_spawn().is_ok() {
                    std::fs::write(worker_marker, b"started").unwrap();
                }
                let _ = done_tx.send(());
                Ok(())
            },
            &cancel,
            &mut deadline,
        )
        .await
    });

    started_rx.await.unwrap();
    let outcome = runner.await.unwrap().unwrap();
    assert!(matches!(outcome, SpawnWait::Timeout { started: false, .. }));
    let contender = tokio::spawn(crate::builtin::process::acquire_execution_lease());
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !contender.is_finished(),
        "timed-out worker released its execution lease before finishing"
    );
    release_tx.send(()).unwrap();
    done_rx.await.unwrap();
    drop(contender.await.unwrap());
    assert!(
        !marker.exists(),
        "timed-out spawn worker launched a program"
    );
}

#[tokio::test]
async fn timeout_reports_a_program_that_crossed_the_launch_boundary() {
    let cancel = CancellationToken::new();
    let deadline = tokio::time::sleep(Duration::from_millis(25));
    tokio::pin!(deadline);
    let outcome = wait_for_spawn(
        move |gate| {
            gate.begin_spawn()?;
            gate.mark_launched();
            std::thread::sleep(Duration::from_millis(75));
            Ok(())
        },
        &cancel,
        &mut deadline,
    )
    .await
    .unwrap();

    assert!(matches!(
        outcome,
        SpawnWait::Timeout {
            started: true,
            teardown: Ok(()),
            ..
        }
    ));
}

#[tokio::test]
async fn timeout_awaits_failed_spawn_cleanup_and_propagates_teardown() {
    let lease = crate::builtin::process::acquire_execution_lease().await;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let runner = tokio::spawn(async move {
        let cancel = CancellationToken::new();
        let deadline = tokio::time::sleep(Duration::from_millis(25));
        tokio::pin!(deadline);
        wait_for_spawn::<(), _>(
            move |gate| {
                let _lease = lease;
                gate.begin_spawn()?;
                let _ = started_tx.send(());
                let _ = release_rx.recv();
                Err(SpawnFailure::new(
                    ToolError::Execution("injected post-create failure".into()),
                    Err(std::io::Error::other("injected reap failure")),
                ))
            },
            &cancel,
            &mut deadline,
        )
        .await
    });

    started_rx.await.unwrap();
    tokio::time::sleep(Duration::from_millis(75)).await;
    assert!(
        !runner.is_finished(),
        "timeout returned before spawn cleanup"
    );
    let contender = tokio::spawn(crate::builtin::process::acquire_execution_lease());
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !contender.is_finished(),
        "timeout released the lease before cleanup"
    );
    release_tx.send(()).unwrap();
    let outcome = runner.await.unwrap().unwrap();
    assert!(matches!(
        outcome,
        SpawnWait::Timeout {
            started: false,
            teardown: Err(ref error),
        } if error.to_string().contains("injected reap failure")
    ));
    drop(contender.await.unwrap());
}

#[tokio::test]
async fn cancellation_awaits_failed_spawn_cleanup_and_propagates_teardown() {
    let lease = crate::builtin::process::acquire_execution_lease().await;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let cancel = CancellationToken::new();
    let runner_cancel = cancel.clone();
    let runner = tokio::spawn(async move {
        let deadline = tokio::time::sleep(Duration::from_secs(30));
        tokio::pin!(deadline);
        wait_for_spawn::<(), _>(
            move |gate| {
                let _lease = lease;
                gate.begin_spawn()?;
                let _ = started_tx.send(());
                let _ = release_rx.recv();
                Err(SpawnFailure::new(
                    ToolError::Execution("injected post-create failure".into()),
                    Err(std::io::Error::other("injected termination failure")),
                ))
            },
            &runner_cancel,
            &mut deadline,
        )
        .await
    });

    started_rx.await.unwrap();
    cancel.cancel();
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !runner.is_finished(),
        "cancellation returned before spawn cleanup"
    );
    let contender = tokio::spawn(crate::builtin::process::acquire_execution_lease());
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !contender.is_finished(),
        "cancellation released the lease before cleanup"
    );
    release_tx.send(()).unwrap();
    let outcome = runner.await.unwrap().unwrap();
    assert!(matches!(
        outcome,
        SpawnWait::Cancelled {
            teardown: Err(ref error),
        } if error.to_string().contains("injected termination failure")
    ));
    drop(contender.await.unwrap());
}

struct FakeProgram {
    lease: Option<crate::builtin::process::ExecutionLease>,
    terminate: Result<(), std::io::Error>,
    reap_block: Option<mpsc::Receiver<()>>,
    finished: bool,
}

impl FakeProgram {
    async fn run_cleanup(&mut self) -> Result<(), std::io::Error> {
        let terminate = std::mem::replace(
            &mut self.terminate,
            Err(std::io::Error::other("cleanup already consumed")),
        );
        if let Some(reap_block) = self.reap_block.take() {
            let _ = tokio::task::spawn_blocking(move || reap_block.recv()).await;
        }
        drop(self.lease.take());
        self.finished = true;
        terminate
    }
}

impl SpawnCleanup for FakeProgram {
    async fn cleanup(mut self) -> Result<(), std::io::Error> {
        self.run_cleanup().await
    }
}

impl Drop for FakeProgram {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let lease = self.lease.take();
        let reap_block = self.reap_block.take();
        self.finished = true;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            drop(handle.spawn(async move {
                if let Some(reap_block) = reap_block {
                    let _ = tokio::task::spawn_blocking(move || reap_block.recv()).await;
                }
                drop(lease);
            }));
            return;
        }
        let _ = std::thread::Builder::new()
            .name("mcode-exec-cleanup".into())
            .spawn(move || {
                if let Some(reap_block) = reap_block {
                    let _ = reap_block.recv();
                }
                drop(lease);
            });
    }
}

#[tokio::test]
async fn deadline_after_launch_before_worker_return_awaits_cleanup() {
    let lease = crate::builtin::process::acquire_execution_lease().await;
    let (reap_tx, reap_rx) = mpsc::channel();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let runner = tokio::spawn(async move {
        let cancel = CancellationToken::new();
        let deadline = tokio::time::sleep(Duration::from_millis(25));
        tokio::pin!(deadline);
        wait_for_spawn(
            move |gate| {
                gate.begin_spawn()?;
                gate.mark_launched();
                let program = FakeProgram {
                    lease: Some(lease),
                    terminate: Err(std::io::Error::other("injected termination failure")),
                    reap_block: Some(reap_rx),
                    finished: false,
                };
                let _ = started_tx.send(());
                std::thread::sleep(Duration::from_millis(75));
                Ok(program)
            },
            &cancel,
            &mut deadline,
        )
        .await
    });

    started_rx.await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !runner.is_finished(),
        "spawn timeout returned before the launched child was reaped"
    );
    let contender = tokio::spawn(crate::builtin::process::acquire_execution_lease());
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !contender.is_finished(),
        "timeout released the execution lease before reap finished"
    );
    reap_tx.send(()).unwrap();
    let outcome = runner.await.unwrap().unwrap();
    match outcome {
        SpawnWait::Timeout {
            started: true,
            teardown: Err(error),
            ..
        } => {
            assert!(
                error.to_string().contains("injected termination failure"),
                "{error}"
            );
        }
        _ => panic!("unexpected spawn wait outcome"),
    }
    drop(contender.await.unwrap());
}

#[tokio::test]
async fn dropped_spawn_future_keeps_lease_until_supervised_reap() {
    let lease = crate::builtin::process::acquire_execution_lease().await;
    let (reap_tx, reap_rx) = mpsc::channel();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let runner = tokio::spawn(async move {
        let cancel = CancellationToken::new();
        let deadline = tokio::time::sleep(Duration::from_secs(30));
        tokio::pin!(deadline);
        wait_for_spawn(
            move |gate| {
                gate.begin_spawn()?;
                gate.mark_launched();
                let program = FakeProgram {
                    lease: Some(lease),
                    terminate: Ok(()),
                    reap_block: Some(reap_rx),
                    finished: false,
                };
                let _ = started_tx.send(());
                Ok(program)
            },
            &cancel,
            &mut deadline,
        )
        .await
    });

    started_rx.await.unwrap();
    runner.abort();
    assert!(matches!(runner.await, Err(error) if error.is_cancelled()));
    let contender = tokio::spawn(crate::builtin::process::acquire_execution_lease());
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !contender.is_finished(),
        "dropped spawn future released the execution lease before reap finished"
    );
    reap_tx.send(()).unwrap();
    drop(contender.await.unwrap());
}

#[tokio::test]
async fn aborted_spawn_future_keeps_lease_until_worker_finishes() {
    let lease = crate::builtin::process::acquire_execution_lease().await;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let runner = tokio::spawn(async move {
        let cancel = CancellationToken::new();
        let deadline = tokio::time::sleep(Duration::from_secs(30));
        tokio::pin!(deadline);
        wait_for_spawn(
            move |_gate| {
                let _lease = lease;
                let _ = started_tx.send(());
                let _ = release_rx.recv();
                let _ = done_tx.send(());
                Ok(())
            },
            &cancel,
            &mut deadline,
        )
        .await
    });

    started_rx.await.unwrap();
    runner.abort();
    assert!(matches!(runner.await, Err(error) if error.is_cancelled()));
    let contender = tokio::spawn(crate::builtin::process::acquire_execution_lease());
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !contender.is_finished(),
        "aborted spawn worker released its execution lease before finishing"
    );
    release_tx.send(()).unwrap();
    done_rx.await.unwrap();
    drop(contender.await.unwrap());
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
async fn failed_macos_waiter_falls_back_to_reap_before_releasing_lease() {
    let sleep = std::path::Path::new("/bin/sleep");
    if !sleep.is_file() {
        eprintln!("skipping: /bin/sleep is not present");
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let pinned =
        super::super::resolve::pin_program(directory.path(), "/bin/sleep", &[], &cancel).unwrap();
    let argv0 = pinned
        .canonical_path
        .to_str()
        .expect("pinned path is Unicode")
        .to_owned();
    let lease = crate::builtin::process::acquire_execution_lease().await;
    let gate = SpawnGate::new();
    let env = super::super::env::snapshot_child_environment().expect("env");
    let spawned = super::super::macos::spawn_macos(
        pinned,
        &argv0,
        &["30".to_owned()],
        directory.path(),
        &env,
        lease,
        &gate,
    );
    let (mut child, process_tree, pinned, lease, _) = match spawned {
        Ok(spawned) => spawned,
        Err(failure) => panic!("{}", failure.error),
    };
    let pid = child.pid().expect("spawned child must retain its pid");

    let failed_waiter = tokio::spawn(async {
        std::future::pending::<std::io::Result<std::process::ExitStatus>>().await
    });
    failed_waiter.abort();
    child.inject_waiter(failed_waiter);

    let mut live = LiveSpawn {
        inner: Inner::Mac(child),
        process_tree,
        _pin: pinned,
        _lease: lease,
        _on_release: ReleaseFlag(None),
    };
    teardown_live(&mut live)
        .await
        .expect("blocking fallback must reap the terminated child");
    match &live.inner {
        Inner::Mac(child) => assert!(child.pid().is_none(), "reap did not clear the pid"),
        Inner::Fixture(_) => unreachable!("macos waiter test uses a real child"),
    }
    // SAFETY: signal 0 only checks whether the reaped numeric pid still exists.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );

    let contender = tokio::spawn(crate::builtin::process::acquire_execution_lease());
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !contender.is_finished(),
        "teardown released the execution lease before dropping the cleanup owner"
    );
    drop(live);
    drop(
        tokio::time::timeout(Duration::from_secs(1), contender)
            .await
            .expect("reaped child retained the execution lease")
            .unwrap(),
    );
}

fn dummy_pinned_image() -> super::super::resolve::PinnedImage {
    super::super::resolve::PinnedImage {
        file: tempfile::tempfile().expect("fixture pin file"),
        canonical_path: std::path::PathBuf::from("fixture-pin"),
        digest: [0; 32],
        identity: super::super::resolve::FileIdentity {
            #[cfg(unix)]
            device: 0,
            #[cfg(unix)]
            inode: 0,
            #[cfg(windows)]
            volume: 0,
            #[cfg(windows)]
            file_id: [0; 16],
        },
        kind: super::super::image::ImageKind::Pe,
    }
}

fn fixture_program(
    lease: crate::builtin::process::ExecutionLease,
    pin_released: Arc<AtomicBool>,
    behavior: FixtureBehavior,
) -> SpawnedProgram {
    SpawnedProgram {
        metadata: ExecutionMetadata::default(),
        live: Some(LiveSpawn {
            inner: Inner::Fixture(behavior),
            process_tree: crate::builtin::process::ProcessTree::for_cleanup_fixture(),
            _pin: dummy_pinned_image(),
            _lease: lease,
            _on_release: ReleaseFlag(Some(pin_released)),
        }),
    }
}

fn assert_injected_teardown(error: &std::io::Error) {
    assert!(
        error.to_string().contains("injected teardown failure"),
        "{error}"
    );
}

/// Cancels a lease waiter instead of requiring it to acquire the global lock.
///
/// Unrelated parallel tests may take the process-wide lease first, so a
/// post-release acquisition deadline would flake. Aborting and joining avoids
/// leaking the waiter.
async fn cancel_and_join_lease_contender(
    contender: tokio::task::JoinHandle<crate::builtin::process::ExecutionLease>,
) {
    contender.abort();
    match contender.await {
        Ok(_lease) => {}
        Err(error) if error.is_cancelled() => {}
        Err(error) => panic!("lease contender task failed: {error}"),
    }
}

#[derive(Clone, Copy)]
enum ExplicitTeardown {
    Timeout,
    Cancel,
    CollectFailed,
}

async fn assert_explicit_async_teardown_transfers(kind: ExplicitTeardown) {
    let (probe, failed_rx, release_tx) = teardown_probe::install_first_failure_probe();
    let lease = crate::builtin::process::acquire_execution_lease().await;
    let pin_released = Arc::new(AtomicBool::new(false));
    let behavior = match kind {
        ExplicitTeardown::CollectFailed => FixtureBehavior::FailCollect,
        ExplicitTeardown::Timeout | ExplicitTeardown::Cancel => FixtureBehavior::Hang,
    };
    let program = fixture_program(lease, Arc::clone(&pin_released), behavior);
    let runner = tokio::spawn(async move {
        let cancel = CancellationToken::new();
        match kind {
            ExplicitTeardown::Cancel => cancel.cancel(),
            ExplicitTeardown::Timeout | ExplicitTeardown::CollectFailed => {}
        }
        let deadline = match kind {
            ExplicitTeardown::Timeout => tokio::time::sleep(Duration::ZERO),
            ExplicitTeardown::Cancel | ExplicitTeardown::CollectFailed => {
                tokio::time::sleep(Duration::from_secs(30))
            }
        };
        tokio::pin!(deadline);
        program.run_until(&cancel, &mut deadline).await
    });

    failed_rx.await.expect("first teardown attempt");
    let outcome = tokio::time::timeout(Duration::from_secs(5), runner)
        .await
        .expect("explicit teardown should return the first error")
        .expect("teardown task");
    match (kind, outcome) {
        (ExplicitTeardown::Timeout, RunOutcome::Timeout { teardown, .. }) => {
            assert_injected_teardown(&teardown.expect_err("first teardown error"));
        }
        (ExplicitTeardown::Cancel, RunOutcome::Cancelled { teardown }) => {
            assert_injected_teardown(&teardown.expect_err("first teardown error"));
        }
        (ExplicitTeardown::CollectFailed, RunOutcome::CollectFailed { teardown, .. }) => {
            assert_injected_teardown(&teardown.expect_err("first teardown error"));
        }
        _ => panic!("unexpected teardown outcome"),
    }

    let contender = tokio::spawn(crate::builtin::process::acquire_execution_lease());
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!pin_released.load(Ordering::Acquire));
    assert!(
        !contender.is_finished(),
        "failed teardown released the execution lease before confirmed cleanup"
    );
    assert!(probe.attempts() >= 1, "teardown never ran");
    assert_eq!(probe.max_in_flight(), 1, "duplicate teardown owner acted");

    release_tx.send(()).expect("release retry");
    let released = tokio::time::timeout(Duration::from_secs(5), async {
        while !pin_released.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    cancel_and_join_lease_contender(contender).await;
    released.expect("supervisor should release the pin after the second attempt");
    assert_eq!(probe.attempts(), 2);
    assert_eq!(probe.max_in_flight(), 1);
}

#[tokio::test]
async fn explicit_async_timeout_teardown_transfers_after_injected_first_failure() {
    assert_explicit_async_teardown_transfers(ExplicitTeardown::Timeout).await;
}

#[tokio::test]
async fn explicit_async_cancel_teardown_transfers_after_injected_first_failure() {
    assert_explicit_async_teardown_transfers(ExplicitTeardown::Cancel).await;
}

#[tokio::test]
async fn explicit_async_collect_teardown_transfers_after_injected_first_failure() {
    assert_explicit_async_teardown_transfers(ExplicitTeardown::CollectFailed).await;
}

#[tokio::test]
async fn dropped_program_blocking_supervisor_retries_after_injected_first_failure() {
    let (probe, failed_rx, release_tx) = teardown_probe::install_first_failure_probe();
    let lease = crate::builtin::process::acquire_execution_lease().await;
    let pin_released = Arc::new(AtomicBool::new(false));
    let program = fixture_program(lease, Arc::clone(&pin_released), FixtureBehavior::Hang);
    let dropped = tokio::task::spawn_blocking(move || drop(program));

    failed_rx.await.expect("first teardown attempt");
    let contender = tokio::spawn(crate::builtin::process::acquire_execution_lease());
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!pin_released.load(Ordering::Acquire));
    assert!(
        !contender.is_finished(),
        "drop supervisor released the execution lease before confirmed cleanup"
    );
    assert!(probe.attempts() >= 1, "teardown never ran");
    assert_eq!(probe.max_in_flight(), 1, "duplicate teardown owner acted");

    release_tx.send(()).expect("release retry");
    dropped.await.expect("drop worker");
    let released = tokio::time::timeout(Duration::from_secs(5), async {
        while !pin_released.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    cancel_and_join_lease_contender(contender).await;
    released.expect("supervisor should release the pin after the second attempt");
    assert_eq!(probe.attempts(), 2);
    assert_eq!(probe.max_in_flight(), 1);
}
