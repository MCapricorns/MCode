//! Windows native structured-exec tests.
use std::sync::atomic::{AtomicUsize, Ordering};

use super::super::spawn::{SpawnFailure, SpawnFailureKind};
use super::super::windows::{launch_with_nested_job_fallback, nested_job_enrollment_kind};
use super::*;

const ENVIRONMENT_PROBE_FILE: &str = "exec-environment-keys.json";
const ENVIRONMENT_PROBE_TEST: &str =
    "builtin::exec::tests::windows_native::environment_isolation_probe";
const ENVIRONMENT_ISOLATION_DRIVER: &str =
    "builtin::exec::tests::windows_native::environment_isolation_driver";
const INJECTED_CHILD_ENVIRONMENT: &[(&str, &str)] = &[
    ("AWS_SECRET_ACCESS_KEY", "ambient-secret-fixture"),
    ("OPENAI_API_KEY", "provider-secret-fixture"),
    ("MCODE_PLUGIN_SECRET", "plugin-secret-fixture"),
    ("MCP_AUTH_TOKEN", "mcp-secret-fixture"),
    ("NODE_OPTIONS", "--require=loader-fixture.js"),
    ("PYTHONPATH", r"C:\interpreter-fixture"),
    ("DOTNET_STARTUP_HOOKS", r"C:\loader-fixture.dll"),
];
const FORBIDDEN_CHILD_ENVIRONMENT: &[&str] = &[
    "AWS_SECRET_ACCESS_KEY",
    "OPENAI_API_KEY",
    "MCODE_PLUGIN_SECRET",
    "MCP_AUTH_TOKEN",
    "NODE_OPTIONS",
    "PYTHONPATH",
    "DOTNET_STARTUP_HOOKS",
];
const ALLOWED_CHILD_ENVIRONMENT: &[&str] = &[
    "ALLUSERSPROFILE",
    "COMPUTERNAME",
    "HOMEDRIVE",
    "HOMEPATH",
    "NUMBER_OF_PROCESSORS",
    "OS",
    "PATH",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "PROGRAMDATA",
    "PUBLIC",
    "SYSTEMDRIVE",
    "SYSTEMROOT",
    "TEMP",
    "TMP",
    "TMPDIR",
    "USERNAME",
    "USERPROFILE",
    "WINDIR",
];

fn system32(name: &str) -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    PathBuf::from(root).join("System32").join(name)
}

fn require_pe(path: &Path) -> bool {
    path.is_file()
}

#[tokio::test]
async fn runs_whoami_with_empty_args() {
    let program = system32("whoami.exe");
    if !require_pe(&program) {
        eprintln!("skipping: {} is not present", program.display());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let result = run_dyn(
        &ExecTool::new(),
        json!({"program": program.to_string_lossy()}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    assert!(!text_of(&result).trim().is_empty());
    let details = result.details.unwrap();
    assert!(
        details["digest_sha256"].as_str().unwrap().len() == 64,
        "{details}"
    );
    let identity = details["identity"].as_str().unwrap();
    assert_eq!(identity.len(), 64, "{details}");
    assert_eq!(details["invocation_digest_sha256"], identity);
    assert!(
        details["image_identity"].as_str().unwrap().contains("vol:"),
        "{details}"
    );
}

#[tokio::test]
async fn basename_whoami_resolves_from_absolute_path_entries() {
    let dir = tempfile::tempdir().unwrap();
    let result = run_dyn(
        &ExecTool::new(),
        json!({"program": "whoami.exe"}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    let program = result.details.unwrap()["program"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(program.to_lowercase().ends_with("whoami.exe"), "{program}");
}

#[tokio::test]
async fn nonzero_exit_is_error_result() {
    let program = system32("where.exe");
    if !require_pe(&program) {
        eprintln!("skipping: {} is not present", program.display());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let result = run_dyn(
        &ExecTool::new(),
        json!({
            "program": program.to_string_lossy(),
            "args": ["mcode-exec-no-such-binary-xyz"]
        }),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    assert!(result.is_error);
    assert!(text_of(&result).contains("[exit code:"));
    assert!(result.details.unwrap()["exit_code"].as_i64().unwrap() != 0);
}

#[tokio::test]
async fn timeout_kills_the_program() {
    let program = system32("ping.exe");
    if !require_pe(&program) {
        eprintln!("skipping: {} is not present", program.display());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let result = run_dyn(
        &ExecTool::new(),
        json!({
            "program": program.to_string_lossy(),
            "args": ["-n", "30", "-w", "1000", "127.0.0.1"],
            "timeout_secs": 1
        }),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();
    assert!(result.is_error);
    assert!(text_of(&result).contains("timed out after 1s"));
    assert!(elapsed < Duration::from_secs(15), "took {elapsed:?}");
    assert_eq!(result.details.unwrap()["timed_out"], true);
}

#[tokio::test]
async fn dropping_execution_future_tears_down() {
    let program = system32("ping.exe");
    if !require_pe(&program) {
        eprintln!("skipping: {} is not present", program.display());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    let program = program.to_string_lossy().into_owned();
    let task = tokio::spawn(async move {
        run_dyn(
            &ExecTool::new(),
            json!({
                "program": program,
                "args": ["-n", "30", "127.0.0.1"],
                "timeout_secs": 60
            }),
            &ctx_at(&cwd),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(400)).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn in_place_write_of_pinned_copy_is_sharing_violation() {
    let source = system32("whoami.exe");
    if !require_pe(&source) {
        eprintln!("skipping: {} is not present", source.display());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("whoami-copy.exe");
    std::fs::copy(&source, &copy).unwrap();
    let pinned = super::super::resolve::pin_program(
        dir.path(),
        copy.to_str().unwrap(),
        &[],
        &CancellationToken::new(),
    )
    .unwrap();
    let write = std::fs::OpenOptions::new().write(true).open(&copy);
    assert!(
        write.is_err(),
        "in-place write succeeded against FILE_SHARE_READ pin"
    );
    drop(pinned);
}

#[tokio::test]
async fn replacement_after_pin_is_detected_by_identity() {
    let source = system32("whoami.exe");
    let other = system32("hostname.exe");
    if !require_pe(&source) || !require_pe(&other) {
        eprintln!("skipping: whoami/hostname not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("swappable.exe");
    std::fs::copy(&source, &copy).unwrap();
    let pinned = super::super::resolve::pin_program(
        dir.path(),
        copy.to_str().unwrap(),
        &[],
        &CancellationToken::new(),
    )
    .unwrap();
    let swapped = dir.path().join("swapped.exe");
    std::fs::copy(&other, &swapped).unwrap();
    let replace = std::fs::rename(&swapped, &copy);
    assert!(
        replace.is_err(),
        "rename-over succeeded against FILE_SHARE_READ pin without DELETE share"
    );
    drop(pinned);
}

#[test]
#[ignore = "spawned by inherited_handle_list_excludes_ambient_handle"]
fn inherited_handle_probe() {
    use windows_sys::Win32::System::Threading::SetEvent;

    let encoded = std::fs::read_to_string("sentinel-handle.txt").unwrap();
    let value = encoded.trim().parse::<usize>().unwrap();
    let handle = value as windows_sys::Win32::Foundation::HANDLE;
    // SAFETY: the numeric value is only a borrowed probe. A leaked event is
    // signaled for the parent to observe; an invalid or reused non-event
    // handle fails without transferring ownership.
    if unsafe { SetEvent(handle) } == 0 {
        assert!(
            std::io::Error::last_os_error()
                .raw_os_error()
                .is_some_and(|code| code != 0),
            "SetEvent failed without a Windows error"
        );
    }
}

#[tokio::test]
async fn inherited_handle_list_excludes_ambient_handle() {
    use std::mem::size_of;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use windows_sys::Win32::Foundation::{WAIT_FAILED, WAIT_TIMEOUT};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    let mut security = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap(),
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    // SAFETY: `security` is initialized, the event is unnamed, and a
    // non-null return is a newly owned kernel handle.
    let raw = unsafe { CreateEventW(&raw mut security, 1, 0, std::ptr::null()) };
    assert!(!raw.is_null(), "{}", std::io::Error::last_os_error());
    // SAFETY: CreateEventW returned a newly owned handle.
    let sentinel = unsafe { OwnedHandle::from_raw_handle(raw) };
    let event_state = || {
        // SAFETY: `sentinel` owns a valid event handle for the duration of the wait.
        let wait = unsafe { WaitForSingleObject(sentinel.as_raw_handle() as _, 0) };
        assert_ne!(wait, WAIT_FAILED, "{}", std::io::Error::last_os_error());
        wait
    };
    assert_eq!(
        event_state(),
        WAIT_TIMEOUT,
        "sentinel event started signaled"
    );

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("sentinel-handle.txt"),
        (sentinel.as_raw_handle() as usize).to_string(),
    )
    .unwrap();
    let current = std::env::current_exe().unwrap();
    let result = run_dyn(
        &ExecTool::new(),
        json!({
            "program": current.to_string_lossy(),
            "args": [
                "--ignored",
                "--exact",
                "builtin::exec::tests::windows_native::inherited_handle_probe"
            ]
        }),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    assert!(
        text_of(&result).contains("1 passed"),
        "{}",
        text_of(&result)
    );
    assert_eq!(
        event_state(),
        WAIT_TIMEOUT,
        "ambient inheritable event reached the child"
    );
}

#[test]
#[ignore = "spawned by child_environment_is_allowlisted"]
fn environment_isolation_probe() {
    let mut keys: Vec<String> = std::env::vars_os()
        .map(|(key, _)| key.to_string_lossy().to_ascii_uppercase())
        .collect();
    keys.sort_unstable();
    for forbidden in FORBIDDEN_CHILD_ENVIRONMENT {
        assert!(
            !keys.iter().any(|key| key.eq_ignore_ascii_case(forbidden)),
            "forbidden environment variable reached exec child: {forbidden}"
        );
    }
    std::fs::write(ENVIRONMENT_PROBE_FILE, serde_json::to_vec(&keys).unwrap()).unwrap();
}

#[tokio::test]
#[ignore = "spawned by child_environment_is_allowlisted"]
async fn environment_isolation_driver() {
    for (key, value) in INJECTED_CHILD_ENVIRONMENT {
        assert_eq!(
            std::env::var(key).as_deref(),
            Ok(*value),
            "driver must receive injected {key}"
        );
    }
    let cwd = std::env::current_dir().unwrap();
    let mut expected_keys: Vec<String> = super::super::snapshot_child_environment()
        .expect("child environment snapshot")
        .into_iter()
        .map(|(key, _)| key.to_string_lossy().to_ascii_uppercase())
        .collect();
    expected_keys.sort_unstable();
    assert!(expected_keys.iter().any(|key| key == "PATH"));
    assert!(expected_keys.len() <= ALLOWED_CHILD_ENVIRONMENT.len());
    assert!(
        expected_keys
            .iter()
            .all(|key| ALLOWED_CHILD_ENVIRONMENT.contains(&key.as_str())),
        "prepared environment exceeded the Windows allowlist: {expected_keys:?}"
    );
    for forbidden in FORBIDDEN_CHILD_ENVIRONMENT {
        assert!(
            !expected_keys
                .iter()
                .any(|key| key.eq_ignore_ascii_case(forbidden)),
            "forbidden key entered the prepared environment: {forbidden}"
        );
    }

    let current = std::env::current_exe().unwrap();
    let result = run_dyn(
        &ExecTool::new(),
        json!({
            "program": current.to_string_lossy(),
            "args": ["--ignored", "--exact", ENVIRONMENT_PROBE_TEST, "--test-threads=1"]
        }),
        &ctx_at(&cwd),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    let details = result.details.as_ref().expect("exec details");
    assert_eq!(
        details["env_summary"]["count"],
        u64::try_from(expected_keys.len()).unwrap()
    );
    assert_eq!(details["env_summary"]["omitted"], 0);

    let observed_keys: Vec<String> =
        serde_json::from_slice(&std::fs::read(cwd.join(ENVIRONMENT_PROBE_FILE)).unwrap()).unwrap();
    assert_eq!(observed_keys, expected_keys);
}

#[test]
fn child_environment_is_allowlisted() {
    let dir = tempfile::tempdir().unwrap();
    let current = std::env::current_exe().unwrap();
    let mut command = std::process::Command::new(current);
    command.current_dir(dir.path());
    for (key, value) in INJECTED_CHILD_ENVIRONMENT {
        command.env(*key, *value);
    }
    let status = command
        .args([
            "--ignored",
            "--exact",
            ENVIRONMENT_ISOLATION_DRIVER,
            "--test-threads=1",
        ])
        .status()
        .unwrap();
    assert!(
        status.success(),
        "environment isolation driver failed: {status}"
    );
}

#[tokio::test]
async fn explicit_symlink_executes_as_the_target() {
    use std::os::windows::fs::symlink_file;

    let program = system32("whoami.exe");
    if !require_pe(&program) {
        eprintln!("skipping: {} is not present", program.display());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("whoami-alias.exe");
    if let Err(err) = symlink_file(&program, &link) {
        eprintln!("skipping: file symlink creation is unavailable: {err}");
        return;
    }
    let via_link = run_dyn(
        &ExecTool::new(),
        json!({"program": link.to_string_lossy()}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    let via_target = run_dyn(
        &ExecTool::new(),
        json!({"program": program.to_string_lossy()}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    assert!(!via_link.is_error, "{}", text_of(&via_link));
    assert_eq!(
        via_link.details.as_ref().unwrap()["identity"],
        via_target.details.as_ref().unwrap()["identity"]
    );
    assert_eq!(
        via_link.details.as_ref().unwrap()["program"],
        via_target.details.as_ref().unwrap()["program"]
    );
}

#[tokio::test]
async fn batch_script_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("evil.cmd"), b"@echo off\n").unwrap();
    let err = run_dyn(
        &ExecTool::new(),
        json!({"program": dir.path().join("evil.cmd").to_string_lossy()}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)));
    assert!(err.to_string().contains("cmd.exe"), "{err}");
}

fn injected_failure(
    kind: SpawnFailureKind,
    message: &str,
    teardown: Result<(), std::io::Error>,
) -> SpawnFailure {
    match kind {
        SpawnFailureKind::NestedJobEnrollmentRejected => {
            SpawnFailure::nested_job_enrollment_rejected(
                ToolError::Execution(message.into()),
                teardown,
            )
        }
        SpawnFailureKind::Unrelated => {
            SpawnFailure::new(ToolError::Execution(message.into()), teardown)
        }
    }
}

#[test]
fn nested_job_enrollment_rejection_retries_once_with_breakaway() {
    let calls = AtomicUsize::new(0);
    let launched = launch_with_nested_job_fallback(true, |breakaway| {
        match calls.fetch_add(1, Ordering::SeqCst) {
            0 => {
                assert!(!breakaway, "first attempt must not request breakaway");
                Err(injected_failure(
                    SpawnFailureKind::NestedJobEnrollmentRejected,
                    "failed to enroll the suspended child in its dedicated Job Object",
                    Ok(()),
                ))
            }
            1 => {
                assert!(breakaway, "retry must request CREATE_BREAKAWAY_FROM_JOB");
                Ok("launched")
            }
            _ => panic!("nested-Job fallback retried more than once"),
        }
    });
    let launched = match launched {
        Ok(value) => value,
        Err(err) => panic!("{}", err.error),
    };
    assert_eq!(launched, "launched");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn nested_job_retry_failure_preserves_second_teardown() {
    let calls = AtomicUsize::new(0);
    let failure = launch_with_nested_job_fallback(true, |breakaway| {
        match calls.fetch_add(1, Ordering::SeqCst) {
            0 => {
                assert!(!breakaway);
                Err(injected_failure(
                    SpawnFailureKind::NestedJobEnrollmentRejected,
                    "failed to enroll the suspended child in its dedicated Job Object",
                    Ok(()),
                ))
            }
            1 => {
                assert!(breakaway);
                Err::<&str, _>(injected_failure(
                    SpawnFailureKind::Unrelated,
                    "failed to spawn breakaway child",
                    Err(std::io::Error::other("injected reap failure")),
                ))
            }
            _ => panic!("nested-Job fallback retried more than once"),
        }
    })
    .unwrap_err();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(failure.kind, SpawnFailureKind::Unrelated);
    let text = failure.error.to_string();
    assert!(
        text.contains("parent Job rejected nested dedicated Job enrollment"),
        "{text}"
    );
    assert!(
        text.contains("CREATE_BREAKAWAY_FROM_JOB fallback failed"),
        "{text}"
    );
    assert!(text.contains("failed to spawn breakaway child"), "{text}");
    assert!(
        failure
            .teardown
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("injected reap failure"),
        "{:?}",
        failure.teardown
    );
}

#[test]
fn non_enrollment_failures_never_retry_or_relabel() {
    struct Case {
        name: &'static str,
        parent_in_job: bool,
        kind: SpawnFailureKind,
        teardown_ok: bool,
        message: &'static str,
    }
    let cases = [
        Case {
            name: "create_process",
            parent_in_job: true,
            kind: SpawnFailureKind::Unrelated,
            teardown_ok: true,
            message: "failed to spawn C:\\image.exe",
        },
        Case {
            name: "image_verification",
            parent_in_job: true,
            kind: SpawnFailureKind::Unrelated,
            teardown_ok: true,
            message: "process image identity does not match the retained executable",
        },
        Case {
            name: "job_creation",
            parent_in_job: true,
            kind: SpawnFailureKind::Unrelated,
            teardown_ok: true,
            message: "failed to create the child Job Object",
        },
        Case {
            name: "job_configuration",
            parent_in_job: true,
            kind: SpawnFailureKind::Unrelated,
            teardown_ok: true,
            message: "failed to configure the child Job Object",
        },
        Case {
            name: "resume",
            parent_in_job: true,
            kind: SpawnFailureKind::Unrelated,
            teardown_ok: true,
            message: "failed to resume the enrolled child",
        },
        Case {
            name: "unrelated",
            parent_in_job: true,
            kind: SpawnFailureKind::Unrelated,
            teardown_ok: true,
            message: "injected unrelated failure",
        },
        Case {
            name: "cleanup_teardown",
            parent_in_job: true,
            kind: SpawnFailureKind::NestedJobEnrollmentRejected,
            teardown_ok: false,
            message: "failed to enroll the suspended child in its dedicated Job Object",
        },
        Case {
            name: "create_process_teardown",
            parent_in_job: true,
            kind: SpawnFailureKind::Unrelated,
            teardown_ok: false,
            message: "failed to spawn C:\\image.exe",
        },
        Case {
            name: "host_not_in_job",
            parent_in_job: false,
            kind: SpawnFailureKind::NestedJobEnrollmentRejected,
            teardown_ok: true,
            message: "failed to enroll the suspended child in its dedicated Job Object",
        },
    ];
    for case in cases {
        let calls = AtomicUsize::new(0);
        let failure = launch_with_nested_job_fallback(case.parent_in_job, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            let teardown = if case.teardown_ok {
                Ok(())
            } else {
                Err(std::io::Error::other("injected reap failure"))
            };
            Err::<&str, _>(injected_failure(case.kind, case.message, teardown))
        })
        .unwrap_err();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{}", case.name);
        assert_eq!(failure.kind, case.kind, "{}", case.name);
        let text = failure.error.to_string();
        assert!(text.contains(case.message), "{}: {text}", case.name);
        assert!(
            !text.contains("CREATE_BREAKAWAY_FROM_JOB"),
            "{} relabeled as nested-Job fallback: {text}",
            case.name
        );
        assert!(
            !text.contains("parent Job rejected nested dedicated Job enrollment"),
            "{} relabeled as nested-Job rejection: {text}",
            case.name
        );
        if case.teardown_ok {
            assert!(failure.teardown.is_ok(), "{}", case.name);
        } else {
            assert!(
                failure
                    .teardown
                    .as_ref()
                    .unwrap_err()
                    .to_string()
                    .contains("injected reap failure"),
                "{}: {:?}",
                case.name,
                failure.teardown
            );
        }
    }
}

#[test]
fn first_success_does_not_request_breakaway() {
    let calls = AtomicUsize::new(0);
    let launched = launch_with_nested_job_fallback(true, |breakaway| {
        calls.fetch_add(1, Ordering::SeqCst);
        assert!(!breakaway);
        Ok("launched")
    });
    let launched = match launched {
        Ok(value) => value,
        Err(err) => panic!("{}", err.error),
    };
    assert_eq!(launched, "launched");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn assign_access_denied_is_nested_job_enrollment_rejected() {
    use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
    let err = std::io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32);
    assert_eq!(
        nested_job_enrollment_kind(&err),
        SpawnFailureKind::NestedJobEnrollmentRejected
    );
}

#[test]
fn assign_unrelated_os_error_is_not_nested_job_enrollment() {
    use windows_sys::Win32::Foundation::ERROR_INVALID_HANDLE;
    let err = std::io::Error::from_raw_os_error(ERROR_INVALID_HANDLE as i32);
    assert_eq!(
        nested_job_enrollment_kind(&err),
        SpawnFailureKind::Unrelated
    );
}
