//! Lifecycle, lease, and public-contract tests for structured exec.
use super::*;
use crate::builtin::edit::EditTool;
use crate::builtin::fs_io::{install_pre_publish_hook, serialize_pre_publish_tests};
#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
use crate::builtin::shell::ShellTool;
use crate::builtin::write::WriteTool;

#[test]
fn public_contract_keeps_exec_name_and_arguments() {
    let tool = ExecTool::new();
    let dyn_tool: &dyn ToolDyn = &tool;
    let spec = dyn_tool.spec();

    assert_eq!(spec.name, "exec");
    assert!(spec.params_schema["properties"]["program"].is_object());
    assert!(spec.params_schema["properties"]["args"].is_object());
    assert!(spec.params_schema["properties"]["timeout_secs"].is_object());
    assert!(spec.params_schema["properties"].get("env").is_none());
    assert!(spec.params_schema["properties"].get("cwd").is_none());
    assert!(spec.description.contains("does not insert a shell"));
    assert!(spec.description.contains("unsandboxed"));
    assert!(spec.description.contains("no Core permission prompt"));
    assert!(spec.description.contains("outside the security boundary"));
    assert!(
        spec.description
            .contains("resolved against the session cwd")
    );
    assert!(!spec.description.contains("under the session root"));
    assert!(tool.prompt_snippet().unwrap().contains("exec:"));
    assert_eq!(dyn_tool.concurrency(), Concurrency::Exclusive);
    assert!(dyn_tool.mutates_fs());
    assert!(!dyn_tool.requires_file_preflight());
    assert!(!dyn_tool.requires_search_preflight());
}

#[test]
fn timeout_before_spawn_preserves_redacted_argument_identity() {
    let argv = vec!["one".to_owned(), "two words".to_owned()];
    let result = timed_out_before_spawn_result("tool", &argv, 25, Duration::from_secs(1));
    let details = result.details.unwrap();
    assert_eq!(details["program"], "tool");
    assert_eq!(details["args_count"], 2);
    assert_eq!(details["args_digest_sha256"], argument_digest(&argv));
    assert_eq!(details["args_summary"]["byte_lengths"], json!([3, 9]));
    assert!(details.get("args").is_none());
    assert_eq!(details["timed_out"], true);
}

#[test]
fn launched_timeout_reports_that_the_program_was_killed() {
    let result = timed_out_result(
        "tool",
        &[],
        "identity",
        "digest",
        CapturedStream::default(),
        CapturedStream::default(),
        25,
        Duration::from_secs(1),
        Ok(()),
        true,
    );
    assert!(text_of(&result).contains("and was killed"));
    assert!(!text_of(&result).contains("before the program started"));
}

#[test]
fn launched_timeout_reports_injected_termination_failure() {
    let result = timed_out_result(
        "tool",
        &[],
        "identity",
        "digest",
        CapturedStream::default(),
        CapturedStream::default(),
        25,
        Duration::from_secs(1),
        Err(std::io::Error::other("injected termination failure")),
        true,
    );
    let text = text_of(&result);
    assert!(
        text.contains("termination failed: injected termination failure"),
        "{text}"
    );
    assert!(!text.contains("was killed"), "{text}");
}

#[tokio::test]
async fn lease_wait_counts_toward_timeout() {
    let guard = crate::builtin::process::acquire_execution_lease().await;
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let result = run_dyn(
        &ExecTool::new(),
        json!({"program": "must-not-resolve", "args": ["kept"], "timeout_secs": 1}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    let details = result.details.as_ref().unwrap();
    assert_eq!(details["timed_out"], true);
    assert_eq!(details["args_count"], 1);
    assert_eq!(
        details["args_digest_sha256"],
        argument_digest(&["kept".into()])
    );
    assert!(details.get("args").is_none());
    drop(guard);
}

#[tokio::test]
async fn cancelled_lease_wait_returns_before_release() {
    let guard = crate::builtin::process::acquire_execution_lease().await;
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        let ctx = ToolCtx::new(&cwd).with_cancel(task_cancel);
        run_dyn(
            &ExecTool::new(),
            json!({"program": "must-not-resolve"}),
            &ctx,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();
    let error = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("cancelled waiter stayed blocked")
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error}");
    drop(guard);
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
async fn serialize_initial_hash_tests() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn block_initial_hash_for(
    program: &Path,
) -> (
    super::resolve::InitialHashHookGuard,
    tokio::sync::oneshot::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let target_name = program.file_name().unwrap().to_owned();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let started = std::sync::Mutex::new(Some(started_tx));
    let release = std::sync::Mutex::new(Some(release_rx));
    let hook = super::resolve::install_initial_hash_hook(std::sync::Arc::new(move |path| {
        if path.file_name() != Some(target_name.as_os_str()) {
            return;
        }
        if let Some(started_tx) = started.lock().unwrap().take() {
            let _ = started_tx.send(());
        }
        if let Some(release_rx) = release.lock().unwrap().take() {
            let _ = release_rx.recv();
        }
    }));
    (hook, started_rx, release_tx)
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[tokio::test]
async fn cancelling_blocked_initial_hash_releases_the_lease_after_worker_exit() {
    let _serialize = serialize_initial_hash_tests().await;
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join(unique_probe_basename("cancel-pin"));
    plant_probe_image(&program);
    let (_hook, started_rx, release_tx) = block_initial_hash_for(&program);
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let cwd = dir.path().to_path_buf();
    let task = tokio::spawn(async move {
        let ctx = ToolCtx::new(&cwd).with_cancel(task_cancel);
        run_dyn(
            &ExecTool::new(),
            json!({"program": program.to_string_lossy()}),
            &ctx,
        )
        .await
    });

    started_rx.await.unwrap();
    cancel.cancel();
    let contender = tokio::spawn(crate::builtin::process::acquire_execution_lease());
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !contender.is_finished(),
        "cancelled pin released the lease before its worker exited"
    );
    release_tx.send(()).unwrap();
    let error = task.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error}");
    let lease = tokio::time::timeout(Duration::from_secs(10), contender)
        .await
        .expect("cancelled pin worker retained the execution lease")
        .unwrap();
    drop(lease);
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[tokio::test]
async fn dropping_blocked_initial_hash_leaves_cleanup_with_the_supervisor() {
    let _serialize = serialize_initial_hash_tests().await;
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join(unique_probe_basename("drop-pin"));
    plant_probe_image(&program);
    let (_hook, started_rx, release_tx) = block_initial_hash_for(&program);
    let cwd = dir.path().to_path_buf();
    let task = tokio::spawn(async move {
        run_dyn(
            &ExecTool::new(),
            json!({"program": program.to_string_lossy()}),
            &ctx_at(&cwd),
        )
        .await
    });

    started_rx.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let contender = tokio::spawn(crate::builtin::process::acquire_execution_lease());
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !contender.is_finished(),
        "dropped pin future released the lease before its worker exited"
    );
    release_tx.send(()).unwrap();
    drop(contender.await.unwrap());
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn available_shell_pin_name() -> Option<&'static str> {
    #[cfg(windows)]
    let candidates = ["pwsh.exe"].as_slice();
    #[cfg(not(windows))]
    let candidates = ["/bin/bash", "bash", "sh"].as_slice();

    for candidate in candidates {
        let path = Path::new(candidate);
        if path.is_absolute() {
            match std::fs::metadata(path) {
                Ok(metadata) if metadata.is_file() => return path.file_name()?.to_str(),
                Ok(_) => return None,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return None,
            }
        }
        for directory in
            std::env::split_paths(std::env::var_os("PATH").as_deref().unwrap_or_default())
        {
            if !directory.is_absolute() {
                continue;
            }
            match std::fs::metadata(directory.join(candidate)) {
                Ok(metadata) if metadata.is_file() => return Some(candidate),
                Ok(_) => return None,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return None,
            }
        }
    }
    None
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[tokio::test]
async fn cancelling_shell_pin_keeps_lease_until_resolution_worker_exits() {
    let _serialize = serialize_initial_hash_tests().await;
    let Some(pin_name) = available_shell_pin_name() else {
        eprintln!("skipping: no shell candidate is available to pin");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (_hook, started_rx, release_tx) = block_initial_hash_for(Path::new(pin_name));
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let cwd = dir.path().to_path_buf();
    let task = tokio::spawn(async move {
        let ctx = ToolCtx::new(&cwd).with_cancel(task_cancel);
        run_dyn(&ShellTool::new(), json!({"command": "exit 0"}), &ctx).await
    });

    started_rx.await.unwrap();
    cancel.cancel();
    let error = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("cancelled shell pin stayed blocked on its worker")
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error}");

    let (contender_started_tx, contender_started_rx) = tokio::sync::oneshot::channel();
    let contender = tokio::spawn(async move {
        let _ = contender_started_tx.send(());
        crate::builtin::process::acquire_execution_lease().await
    });
    contender_started_rx.await.unwrap();
    assert!(
        !contender.is_finished(),
        "cancelled shell pin released the lease before its worker exited"
    );

    release_tx.send(()).unwrap();
    let lease = tokio::time::timeout(Duration::from_secs(10), contender)
        .await
        .expect("shell pin worker retained the execution lease after exit")
        .unwrap();
    drop(lease);
}

#[tokio::test]
#[expect(
    clippy::await_holding_lock,
    reason = "process-global pre-publish hook is not async-aware; this test must not overlap other writes"
)]
async fn dropping_write_future_keeps_lease_until_worker_exits() {
    let _serialize = serialize_pre_publish_tests();
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (block_tx, block_rx) = std::sync::mpsc::channel();
    const KEY: &str = "lease-drop-write.txt";
    let started = std::sync::Mutex::new(Some(started_tx));
    let blocked = std::sync::Mutex::new(Some(block_rx));
    let _hook = install_pre_publish_hook(std::sync::Arc::new(move |key| {
        if key == KEY {
            if let Some(started_tx) = started.lock().unwrap().take() {
                let _ = started_tx.send(());
            }
            if let Some(block_rx) = blocked.lock().unwrap().take() {
                let _ = block_rx.recv();
            }
        }
    }));
    let task = tokio::spawn(async move {
        run_dyn(
            &WriteTool,
            json!({"path": KEY, "content": "payload"}),
            &ctx_at(&cwd),
        )
        .await
    });
    started_rx.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let contender = tokio::spawn(crate::builtin::process::acquire_execution_lease());
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !contender.is_finished(),
        "dropped write future released the execution lease before the worker exited"
    );
    block_tx.send(()).unwrap();
    drop(contender.await.unwrap());
}

#[tokio::test]
#[expect(
    clippy::await_holding_lock,
    reason = "process-global pre-publish hook is not async-aware; this test must not overlap other writes"
)]
async fn dropping_edit_future_keeps_lease_until_publication_exits() {
    let _serialize = serialize_pre_publish_tests();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lease-drop-edit.txt"), "hello").unwrap();
    let cwd = dir.path().to_path_buf();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (block_tx, block_rx) = std::sync::mpsc::channel();
    const KEY: &str = "lease-drop-edit.txt";
    let started = std::sync::Mutex::new(Some(started_tx));
    let blocked = std::sync::Mutex::new(Some(block_rx));
    let _hook = install_pre_publish_hook(std::sync::Arc::new(move |key| {
        if key == KEY {
            if let Some(started_tx) = started.lock().unwrap().take() {
                let _ = started_tx.send(());
            }
            if let Some(block_rx) = blocked.lock().unwrap().take() {
                let _ = block_rx.recv();
            }
        }
    }));
    let task = tokio::spawn(async move {
        run_dyn(
            &EditTool,
            json!({
                "path": KEY,
                "old_string": "hello",
                "new_string": "world"
            }),
            &ctx_at(&cwd),
        )
        .await
    });
    started_rx.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let contender = tokio::spawn(crate::builtin::process::acquire_execution_lease());
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !contender.is_finished(),
        "dropped edit future released the execution lease before publication exited"
    );
    block_tx.send(()).unwrap();
    drop(contender.await.unwrap());
}

#[tokio::test]
async fn write_waits_for_the_execution_lease() {
    let guard = crate::builtin::process::acquire_execution_lease().await;
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    let marker = cwd.join("marker.txt");
    let task = tokio::spawn(async move {
        run_dyn(
            &WriteTool,
            json!({"path": "marker.txt", "content": "written"}),
            &ctx_at(&cwd),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!marker.exists(), "write bypassed the execution lease");
    drop(guard);
    let result = task.await.unwrap().unwrap();
    assert!(!result.is_error);
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "written");
}
