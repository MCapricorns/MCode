//! Execution-lease timing tests for [`super::EditTool`].
//!
//! Planning and no-op paths must not hold the process-wide write/edit/exec
//! lease. Publication takes the lease and keeps it on the write worker even
//! when the caller future is dropped.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::oneshot;

use super::{EditTool, install_planning_hook, install_pre_lease_hook};
use crate::builtin::fs_io::{install_pre_publish_hook, serialize_pre_publish_tests};
use crate::builtin::process::acquire_execution_lease;
use crate::builtin::test_support::{ctx_at, run_dyn};
use crate::builtin::{ExecTool, WriteTool};
use crate::tool::ToolError;

/// Allows unrelated process-wide lease users in the parallel suite to drain.
const CONTENDED_LEASE_TIMEOUT: Duration = Duration::from_secs(30);

struct ReleaseGate {
    started: oneshot::Receiver<()>,
    release: std::sync::mpsc::Sender<()>,
}

fn keyed_blocking_gate(key: &'static str) -> (super::EditStageHook, ReleaseGate) {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let started = Mutex::new(Some(started_tx));
    let release = Mutex::new(Some(release_rx));
    let hook = Arc::new(move |observed: &str| {
        if observed != key {
            return;
        }
        if let Some(tx) = started
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = tx.send(());
        }
        if let Some(rx) = release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = rx.recv();
        }
    });
    (
        hook,
        ReleaseGate {
            started: started_rx,
            release: release_tx,
        },
    )
}

fn exec_ok_args() -> serde_json::Value {
    #[cfg(windows)]
    {
        json!({"program": "where.exe", "args": ["where.exe"], "timeout_secs": 30})
    }
    #[cfg(not(windows))]
    {
        json!({"program": "true", "timeout_secs": 30})
    }
}

async fn serialize_edit_lease_tests() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

#[tokio::test]
async fn non_publishing_edit_paths_do_not_acquire_the_execution_lease() {
    let _serialize = serialize_edit_lease_tests().await;
    let _held = acquire_execution_lease().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("edit-lease-noop.txt"), "keep").unwrap();
    let ctx = ctx_at(dir.path());

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_dyn(
            &EditTool,
            json!({
                "path": "edit-lease-noop.txt",
                "old_string": "keep",
                "new_string": "keep",
            }),
            &ctx,
        ),
    )
    .await
    .expect("no-op edit acquired the execution lease")
    .unwrap();
    assert!(!result.is_error);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("edit-lease-noop.txt")).unwrap(),
        "keep"
    );

    let err = tokio::time::timeout(
        Duration::from_secs(5),
        run_dyn(
            &EditTool,
            json!({
                "path": "edit-lease-noop.txt",
                "old_string": "missing",
                "new_string": "x",
            }),
            &ctx,
        ),
    )
    .await
    .expect("planning error acquired the execution lease")
    .unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)), "{err}");

    let err = tokio::time::timeout(
        Duration::from_secs(5),
        run_dyn(
            &EditTool,
            json!({
                "path": "edit-lease-noop.txt",
                "old_string": "",
                "new_string": "x",
            }),
            &ctx,
        ),
    )
    .await
    .expect("invalid edit args acquired the execution lease")
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
}

#[tokio::test]
async fn blocked_planning_does_not_block_write_or_exec_lease_acquisition() {
    let _serialize = serialize_edit_lease_tests().await;
    const KEY: &str = "edit-lease-plan.txt";
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(KEY), "alpha").unwrap();
    let (hook, gate) = keyed_blocking_gate(KEY);
    let _hook = install_planning_hook(hook);
    let cwd = dir.path().to_path_buf();
    let edit = tokio::spawn(async move {
        let ctx = ctx_at(&cwd);
        run_dyn(
            &EditTool,
            json!({
                "path": KEY,
                "old_string": "alpha",
                "new_string": "beta",
            }),
            &ctx,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), gate.started)
        .await
        .expect("edit planning hook did not start")
        .unwrap();

    let lease = tokio::time::timeout(CONTENDED_LEASE_TIMEOUT, acquire_execution_lease())
        .await
        .expect("blocked planning held the execution lease");
    drop(lease);

    let write_ctx = ctx_at(dir.path());
    tokio::time::timeout(
        CONTENDED_LEASE_TIMEOUT,
        run_dyn(
            &WriteTool,
            json!({"path": "edit-lease-write-sidecar.txt", "content": "sidecar"}),
            &write_ctx,
        ),
    )
    .await
    .expect("blocked planning blocked Write lease acquisition")
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("edit-lease-write-sidecar.txt")).unwrap(),
        "sidecar"
    );

    let exec_result = tokio::time::timeout(
        CONTENDED_LEASE_TIMEOUT,
        run_dyn(&ExecTool::new(), exec_ok_args(), &write_ctx),
    )
    .await
    .expect("blocked planning blocked Exec lease acquisition")
    .expect("Exec failed while edit planning was blocked");
    assert!(!exec_result.is_error);

    gate.release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), edit)
        .await
        .expect("edit did not finish after planning released")
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join(KEY)).unwrap(),
        "beta"
    );
}

#[tokio::test]
async fn mutation_between_planning_and_lease_fails_revision_cas() {
    let _serialize = serialize_edit_lease_tests().await;
    const KEY: &str = "edit-lease-cas.txt";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(KEY);
    std::fs::write(&path, "alpha").unwrap();
    let mutated = path.clone();
    let _hook = install_pre_lease_hook(Arc::new(move |observed| {
        if observed == KEY {
            std::fs::write(&mutated, "foreign").unwrap();
        }
    }));
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": KEY,
            "old_string": "alpha",
            "new_string": "beta",
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("stale expected_revision"), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "foreign");
}

#[tokio::test]
#[expect(
    clippy::await_holding_lock,
    reason = "process-global pre-publish hook is not async-aware; this test must not overlap other writes"
)]
async fn publication_serializes_with_write_and_exec() {
    let _serialize_lease = serialize_edit_lease_tests().await;
    let _serialize = serialize_pre_publish_tests();
    const KEY: &str = "edit-lease-publish.txt";
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(KEY), "alpha").unwrap();
    let (hook, gate) = keyed_blocking_gate(KEY);
    let _hook = install_pre_publish_hook(hook);
    let cwd = dir.path().to_path_buf();
    let edit = tokio::spawn(async move {
        let ctx = ctx_at(&cwd);
        run_dyn(
            &EditTool,
            json!({
                "path": KEY,
                "old_string": "alpha",
                "new_string": "beta",
            }),
            &ctx,
        )
        .await
    });
    tokio::time::timeout(CONTENDED_LEASE_TIMEOUT, gate.started)
        .await
        .expect("edit publication hook did not start")
        .unwrap();

    let contender = tokio::spawn(acquire_execution_lease());
    let write_cwd = dir.path().to_path_buf();
    let write = tokio::spawn(async move {
        let ctx = ctx_at(&write_cwd);
        run_dyn(
            &WriteTool,
            json!({"path": "edit-lease-publish-sidecar.txt", "content": "sidecar"}),
            &ctx,
        )
        .await
    });
    let exec_cwd = dir.path().to_path_buf();
    let exec = tokio::spawn(async move {
        let ctx = ctx_at(&exec_cwd);
        run_dyn(&ExecTool::new(), exec_ok_args(), &ctx).await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !contender.is_finished(),
        "publication released the lease before publish finished"
    );
    assert!(
        !write.is_finished(),
        "Write acquired the lease during edit publication"
    );
    assert!(
        !exec.is_finished(),
        "Exec acquired the lease during edit publication"
    );

    gate.release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), edit)
        .await
        .expect("edit publication did not finish")
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join(KEY)).unwrap(),
        "beta"
    );

    let lease = tokio::time::timeout(Duration::from_secs(10), contender)
        .await
        .expect("edit publication retained the execution lease")
        .unwrap();
    drop(lease);
    tokio::time::timeout(Duration::from_secs(10), write)
        .await
        .expect("Write stayed blocked after edit publication")
        .unwrap()
        .unwrap();
    let exec_result = tokio::time::timeout(Duration::from_secs(10), exec)
        .await
        .expect("Exec stayed blocked after edit publication")
        .expect("Exec task panicked")
        .expect("Exec failed after edit publication");
    assert!(!exec_result.is_error);
}

#[tokio::test]
#[expect(
    clippy::await_holding_lock,
    reason = "process-global pre-publish hook is not async-aware; this test must not overlap other writes"
)]
async fn dropped_publish_future_keeps_the_lease_until_worker_exit() {
    let _serialize_lease = serialize_edit_lease_tests().await;
    let _serialize = serialize_pre_publish_tests();
    const KEY: &str = "edit-lease-drop.txt";
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(KEY), "alpha").unwrap();
    let (hook, gate) = keyed_blocking_gate(KEY);
    let _hook = install_pre_publish_hook(hook);
    let cwd = dir.path().to_path_buf();
    let edit = tokio::spawn(async move {
        let ctx = ctx_at(&cwd);
        run_dyn(
            &EditTool,
            json!({
                "path": KEY,
                "old_string": "alpha",
                "new_string": "beta",
            }),
            &ctx,
        )
        .await
    });
    tokio::time::timeout(CONTENDED_LEASE_TIMEOUT, gate.started)
        .await
        .expect("edit publication hook did not start")
        .unwrap();
    edit.abort();
    assert!(edit.await.unwrap_err().is_cancelled());

    let contender = tokio::spawn(acquire_execution_lease());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !contender.is_finished(),
        "dropped edit future released the lease before its worker exited"
    );
    gate.release.send(()).unwrap();
    let lease = tokio::time::timeout(Duration::from_secs(10), contender)
        .await
        .expect("dropped edit worker retained the execution lease")
        .unwrap();
    drop(lease);
    assert_eq!(
        std::fs::read_to_string(dir.path().join(KEY)).unwrap(),
        "alpha"
    );
}
