//! Tests for the structured exec builtin.
use super::*;
use crate::builtin::test_support::{ctx_at, run_dyn, text_of};
use crate::ctx::ToolCtx;
use crate::tool::{ToolDyn, ToolError};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const EXEC_PROBE_MARKER: &str = "exec-probe-marker";
const EXEC_PROBE_TEST: &str = "builtin::exec::tests::exec_probe_writes_marker";
const PATH_PIN_NAME_FILE: &str = "path-pin-name";
const PATH_PIN_PROBE_TEST: &str = "builtin::exec::tests::path_pin_selected_image_probe";

#[path = "tests/lifecycle.rs"]
mod lifecycle;

#[test]
#[ignore = "spawned by structured-exec probe-marker tests"]
fn exec_probe_writes_marker() {
    let exe = std::env::current_exe().unwrap();
    std::fs::write(EXEC_PROBE_MARKER, exe.to_string_lossy().as_bytes()).unwrap();
}

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
fn linux_process_identity(pid: libc::pid_t) -> std::io::Result<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields = stat
        .rsplit_once(") ")
        .map(|(_, fields)| fields)
        .ok_or_else(|| std::io::Error::other("Linux process stat has no command terminator"))?;
    let start_time = fields
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| std::io::Error::other("Linux process stat has no start time"))?;
    Ok(format!("{pid} {start_time}"))
}

#[cfg(all(windows, target_arch = "x86_64"))]
#[test]
#[ignore = "spawned by runtime_shutdown_keeps_cleanup_outside_the_runtime"]
fn runtime_shutdown_probe() {
    let identity = std::process::id().to_string();
    std::fs::write("runtime-shutdown-starting", identity).unwrap();
    std::fs::rename("runtime-shutdown-starting", "runtime-shutdown-started").unwrap();
    std::thread::sleep(Duration::from_secs(2));
    std::fs::write("runtime-shutdown-survived", b"survived").unwrap();
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[test]
fn runtime_shutdown_keeps_cleanup_outside_the_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    let started = cwd.join("runtime-shutdown-started");
    let survived = cwd.join("runtime-shutdown-survived");
    #[cfg(all(windows, target_arch = "x86_64"))]
    let (program, args): (String, Vec<String>) = {
        let current = std::env::current_exe().unwrap();
        (
            current.to_string_lossy().into_owned(),
            vec![
                "--ignored".into(),
                "--exact".into(),
                "builtin::exec::tests::runtime_shutdown_probe".into(),
            ],
        )
    };
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    let (program, args): (String, Vec<String>) = {
        if !Path::new("/bin/sh").is_file() {
            eprintln!("skipping: /bin/sh is not present");
            return;
        }
        (
            "/bin/sh".into(),
            vec![
                "-c".into(),
                r#"printf '%s\n' "$$" > runtime-shutdown-starting && mv runtime-shutdown-starting runtime-shutdown-started && sleep 2 && printf survived > runtime-shutdown-survived"#
                    .into(),
            ],
        )
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let task_cwd = cwd.clone();
    let task = runtime.spawn(async move {
        let _ = run_dyn(
            &ExecTool::new(),
            json!({
                "program": program,
                "args": args,
                "timeout_secs": 30
            }),
            &ctx_at(&task_cwd),
        )
        .await;
    });
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !started.is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("exec probe did not start");
    });
    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    let (started_pid, started_identity) = {
        let pid = std::fs::read_to_string(&started)
            .expect("runtime shutdown probe did not publish its pid")
            .trim()
            .parse::<libc::pid_t>()
            .expect("runtime shutdown probe pid was invalid");
        let identity = linux_process_identity(pid).expect("runtime shutdown probe was not live");
        (pid, identity)
    };
    drop(runtime);
    drop(task);

    let verifier = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let lease = verifier
        .block_on(async {
            tokio::time::timeout(
                Duration::from_secs(10),
                crate::builtin::process::acquire_execution_lease(),
            )
            .await
        })
        .expect("runtime shutdown abandoned exec cleanup");
    drop(lease);
    drop(verifier);

    #[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
    {
        match linux_process_identity(started_pid) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(current_identity) => assert_ne!(
                current_identity, started_identity,
                "runtime shutdown left child {started_pid} unreaped"
            ),
            Err(error) => {
                panic!("failed to inspect runtime shutdown child {started_pid}: {error}")
            }
        }
    }

    std::thread::sleep(Duration::from_millis(2_250));
    assert!(
        !survived.exists(),
        "exec child survived after its owning runtime shut down"
    );
}

#[tokio::test]
async fn missing_program_is_invalid_args_not_execution() {
    let dir = tempfile::tempdir().unwrap();
    let err = run_dyn(
        &ExecTool::new(),
        json!({"program": "mcode-exec-missing-binary-xyz"}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)));
    assert!(err.to_string().contains("not found on PATH"), "{err}");
}

#[tokio::test]
async fn directory_as_program_is_invalid_args() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().to_string_lossy().into_owned();
    let err = run_dyn(
        &ExecTool::new(),
        json!({"program": program}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)));
}

#[cfg(unix)]
#[tokio::test]
async fn fifo_program_is_rejected_without_retaining_the_execution_lease() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("program.fifo");
    let native_path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: `native_path` is NUL-terminated and points to a path in the
    // private temporary directory. The mode contains only permission bits.
    let created = unsafe { libc::mkfifo(native_path.as_ptr(), 0o600) };
    assert_eq!(created, 0, "{}", std::io::Error::last_os_error());

    let lease = crate::builtin::process::acquire_execution_lease().await;
    let cwd = dir.path().to_path_buf();
    let program = fifo.to_string_lossy().into_owned();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let mut worker = tokio::task::spawn_blocking(move || {
        let _ = started_tx.send(());
        let result = super::resolve::pin_program(&cwd, &program, &[], &CancellationToken::new());
        drop(lease);
        result
    });
    started_rx.await.unwrap();
    let run = tokio::time::timeout(Duration::from_secs(1), &mut worker).await;
    let error = match run {
        Ok(joined) => joined.unwrap().unwrap_err(),
        Err(_) => {
            let writer_path = fifo.clone();
            let writer = tokio::task::spawn_blocking(move || {
                std::fs::OpenOptions::new().write(true).open(writer_path)
            });
            let _ = worker.await;
            writer.await.unwrap().unwrap();
            panic!("opening a FIFO as an executable blocked");
        }
    };
    assert!(matches!(error, ToolError::InvalidArgs(_)), "{error}");
    assert!(error.to_string().contains("regular file"), "{error}");
    let lease = tokio::time::timeout(
        Duration::from_secs(1),
        crate::builtin::process::acquire_execution_lease(),
    )
    .await
    .expect("rejected FIFO retained the execution lease");
    drop(lease);
}

#[tokio::test]
async fn cancelled_call_never_spawns_the_program() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join(EXEC_PROBE_MARKER);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let ctx = ToolCtx::new(dir.path()).with_cancel(cancel);
    let args = json!({
        "program": std::env::current_exe().unwrap().to_string_lossy(),
        "args": exec_probe_args(),
    });
    let err = run_dyn(&ExecTool::new(), args, &ctx).await.unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)));
    assert!(err.to_string().contains("cancelled"), "{err}");
    assert!(!marker.exists(), "pre-cancelled program was started");
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[tokio::test]
async fn cancellation_after_spawn_reaps_the_program() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let (program, args) = host_long_running_command();
    let task = tokio::spawn(async move {
        let ctx = ToolCtx::new(&cwd).with_cancel(task_cancel);
        run_dyn(
            &ExecTool::new(),
            json!({"program": program, "args": args, "timeout_secs": 30}),
            &ctx,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(400)).await;
    cancel.cancel();
    let error = task.await.unwrap().unwrap_err();
    assert!(matches!(error, ToolError::Execution(_)));
    assert!(error.to_string().contains("cancelled"), "{error}");
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[tokio::test]
async fn path_spoof_in_cwd_is_never_picked_for_a_basename() {
    let dir = tempfile::tempdir().unwrap();
    let name = unique_probe_basename("cwd-spoof");
    let spoof = dir.path().join(&name);
    plant_probe_image(&spoof);
    let marker = dir.path().join(EXEC_PROBE_MARKER);
    let err = run_dyn(
        &ExecTool::new(),
        json!({"program": name, "args": exec_probe_args()}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert!(err.to_string().contains("not found on PATH"), "{err}");
    assert!(
        !marker.exists(),
        "cwd spoof probe ran: {}",
        marker.display()
    );
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[test]
#[ignore = "spawned by path_pin_runs_selected_image_not_cwd_spoof"]
fn path_pin_selected_image_probe() {
    let cwd = std::env::current_dir().unwrap();
    let name = std::fs::read_to_string(PATH_PIN_NAME_FILE).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime
        .block_on(run_dyn(
            &ExecTool::new(),
            json!({"program": name.trim()}),
            &ctx_at(&cwd),
        ))
        .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    let program = result.details.as_ref().expect("details")["program"]
        .as_str()
        .expect("program");
    std::fs::write(EXEC_PROBE_MARKER, program).unwrap();
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[test]
fn path_pin_runs_selected_image_not_cwd_spoof() {
    let dir = tempfile::tempdir().unwrap();
    let path_dir = dir.path().join("path-bin");
    std::fs::create_dir(&path_dir).unwrap();
    let name = unique_probe_basename("path-pin");
    let selected = path_dir.join(&name);
    let spoof = dir.path().join(&name);
    plant_native_image(&host_success_image(), &selected);
    plant_native_image(&host_failure_image(), &spoof);
    std::fs::write(dir.path().join(PATH_PIN_NAME_FILE), &name).unwrap();

    let mut entries = vec![path_dir];
    if let Some(path) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&path));
    }
    let child_path = std::env::join_paths(entries).unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .current_dir(dir.path())
        .env("PATH", child_path)
        .args([
            "--ignored",
            "--exact",
            PATH_PIN_PROBE_TEST,
            "--test-threads=1",
        ])
        .status()
        .unwrap();
    assert!(status.success(), "PATH pin probe failed: {status}");

    let marker = dir.path().join(EXEC_PROBE_MARKER);
    assert!(marker.is_file(), "selected PATH image did not run");
    let reported = PathBuf::from(std::fs::read_to_string(&marker).unwrap());
    assert!(
        same_exe_path(&reported, &selected),
        "selected image was {reported:?}, expected PATH image {selected:?}"
    );
    assert!(
        !same_exe_path(&reported, &spoof),
        "cwd spoof image ran: {reported:?}"
    );
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[tokio::test]
async fn native_image_with_script_extension_executes_by_header() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("renamed-native.sh");
    plant_native_image(&host_success_image(), &program);

    let result = run_dyn(
        &ExecTool::new(),
        json!({"program": program.to_string_lossy()}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();

    assert!(!result.is_error, "{}", text_of(&result));
}

#[tokio::test]
async fn shebang_program_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("shebang-program.bin");
    std::fs::write(&program, b"#!/bin/sh\nprintf 'shebang-ran\\n'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&program, permissions).unwrap();
    }
    let err = run_dyn(
        &ExecTool::new(),
        json!({"program": program.to_string_lossy()}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    let text = err.to_string();
    assert!(
        text.contains("shebang") || text.contains("kernel-loadable") || text.contains("script"),
        "{text}"
    );
}

#[tokio::test]
async fn interior_nul_argument_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let err = run_dyn(
        &ExecTool::new(),
        json!({"program": host_probe_program(), "args": ["ok\0bad"]}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert!(err.to_string().contains("NUL"), "{err}");
}

#[cfg(any(
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[test]
#[ignore = "spawned by exec_survives_closed_standard_descriptors"]
fn closed_standard_descriptors_probe() {
    use std::os::fd::{AsRawFd as _, IntoRawFd as _};

    let cwd = std::env::current_dir().unwrap();
    let mut reserves = Vec::new();
    loop {
        let reserve = std::fs::File::open("/dev/null").unwrap();
        let fd = reserve.as_raw_fd();
        reserves.push(reserve);
        if fd >= 32 {
            break;
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut retained_reserves = Vec::new();
    for reserve in reserves {
        if reserve.as_raw_fd() <= 3 {
            let fd = reserve.into_raw_fd();
            // SAFETY: ownership was removed from `File`; this closes it once.
            let _ = unsafe { libc::close(fd) };
        } else {
            retained_reserves.push(reserve);
        }
    }
    for fd in 0..=3 {
        // SAFETY: this isolated probe intentionally starts exec without the
        // conventional standard descriptors or fd 3.
        let _ = unsafe { libc::close(fd) };
    }

    let result = runtime
        .block_on(run_dyn(
            &ExecTool::new(),
            json!({"program": host_probe_program()}),
            &ctx_at(&cwd),
        ))
        .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    std::fs::write(EXEC_PROBE_MARKER, b"ok").unwrap();
    drop(retained_reserves);
}

#[cfg(any(
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[test]
fn exec_survives_closed_standard_descriptors() {
    let dir = tempfile::tempdir().unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .current_dir(dir.path())
        .args([
            "--ignored",
            "--exact",
            "builtin::exec::tests::closed_standard_descriptors_probe",
            "--test-threads=1",
        ])
        .status()
        .unwrap();
    assert!(status.success(), "closed-descriptor probe failed: {status}");
    assert!(dir.path().join(EXEC_PROBE_MARKER).is_file());
}

#[cfg(not(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
)))]
#[tokio::test]
async fn unsupported_target_rejects_launch() {
    let dir = tempfile::tempdir().unwrap();
    let error = run_dyn(
        &ExecTool::new(),
        json!({"program": host_probe_program()}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ToolError::Execution(_)), "{error}");
    assert!(error.to_string().contains("not supported"), "{error}");
}

#[test]
fn result_details_redact_and_bound_argument_metadata() {
    let secret = "secret-argument-marker";
    let args: Vec<String> = (0..4_096)
        .map(|index| format!("{secret}-{index:04}-{}", "x".repeat(220)))
        .collect();
    let result = format_result(
        None,
        "program",
        &args,
        "identity",
        "digest",
        CapturedStream::default(),
        CapturedStream::default(),
        1,
        false,
        None,
    );
    let details = result.details.unwrap();
    let encoded = serde_json::to_vec(&details).unwrap();
    assert!(
        encoded.len() < 4_096,
        "details grew to {} bytes",
        encoded.len()
    );
    assert!(!String::from_utf8(encoded).unwrap().contains(secret));
    assert_eq!(details["args_count"], 4_096);
    assert_eq!(
        details["args_summary"]["byte_lengths"]
            .as_array()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(details["args_summary"]["omitted"], 4_032);
    assert!(details.get("args").is_none());
}

#[test]
fn argument_digest_is_length_framed() {
    let left = vec!["ab".to_owned(), "c".to_owned()];
    let right = vec!["a".to_owned(), "bc".to_owned()];
    assert_ne!(argument_digest(&left), argument_digest(&right));
    assert_eq!(argument_digest(&left), argument_digest(&left));
}

#[cfg(all(windows, target_arch = "x86_64"))]
#[path = "tests/windows.rs"]
mod windows_native;

#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
#[path = "tests/linux.rs"]
mod linux_native;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[path = "tests/macos.rs"]
mod macos_native;

fn host_probe_program() -> String {
    #[cfg(windows)]
    {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        PathBuf::from(root)
            .join("System32")
            .join("whoami.exe")
            .to_string_lossy()
            .into_owned()
    }
    #[cfg(target_os = "macos")]
    {
        "/usr/bin/true".into()
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        "/bin/true".into()
    }
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn host_long_running_command() -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        (
            PathBuf::from(root)
                .join("System32")
                .join("ping.exe")
                .to_string_lossy()
                .into_owned(),
            vec!["-n".into(), "30".into(), "127.0.0.1".into()],
        )
    }
    #[cfg(not(windows))]
    {
        ("/bin/sleep".into(), vec!["30".into()])
    }
}

fn exec_probe_args() -> Vec<String> {
    vec!["--ignored".into(), "--exact".into(), EXEC_PROBE_TEST.into()]
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn unique_probe_basename(tag: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = format!(
        "mcode-exec-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    #[cfg(windows)]
    {
        format!("{id}.exe")
    }
    #[cfg(not(windows))]
    {
        id
    }
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn host_success_image() -> PathBuf {
    PathBuf::from(host_probe_program())
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn host_failure_image() -> PathBuf {
    #[cfg(windows)]
    {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        PathBuf::from(root).join("System32").join("where.exe")
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/usr/bin/false")
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        PathBuf::from("/bin/false")
    }
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn plant_native_image(source: &Path, dest: &Path) {
    assert!(
        source.is_file(),
        "required host image {} is absent",
        source.display()
    );
    std::fs::copy(source, dest).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(dest).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(dest, permissions).unwrap();
    }
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn plant_probe_image(path: &Path) {
    plant_native_image(&host_success_image(), path);
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn same_exe_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}
