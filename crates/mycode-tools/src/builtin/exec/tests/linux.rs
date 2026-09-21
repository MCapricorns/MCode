//! Linux x86_64 GNU native structured-exec tests.
use super::*;

fn require(path: &str) -> bool {
    Path::new(path).is_file()
}

#[tokio::test]
async fn empty_args_and_silent_success() {
    if !require("/bin/true") {
        eprintln!("skipping: /bin/true is not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let result = run_dyn(
        &ExecTool::new(),
        json!({"program": "/bin/true"}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    assert!(!result.is_error);
    assert_eq!(text_of(&result), "");
}

#[tokio::test]
async fn argv_is_passed_verbatim() {
    if !require("/bin/printf") {
        eprintln!("skipping: /bin/printf is not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let result = run_dyn(
        &ExecTool::new(),
        json!({
            "program": "/bin/printf",
            "args": ["[%s]", "a b"]
        }),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    assert_eq!(text_of(&result), "[a b]");
}

#[tokio::test]
async fn non_zero_exit_is_error_result_not_tool_error() {
    if !require("/bin/false") {
        eprintln!("skipping: /bin/false is not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let result = run_dyn(
        &ExecTool::new(),
        json!({"program": "/bin/false"}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    assert!(result.is_error);
    assert!(text_of(&result).contains("[exit code:"));
}

#[tokio::test]
async fn timeout_kills_the_program() {
    if !require("/bin/sleep") {
        eprintln!("skipping: /bin/sleep is not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let result = run_dyn(
        &ExecTool::new(),
        json!({"program": "/bin/sleep", "args": ["30"], "timeout_secs": 1}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();
    assert!(result.is_error);
    assert!(text_of(&result).contains("timed out after 1s"));
    assert!(elapsed < Duration::from_secs(10), "took {elapsed:?}");
    assert_eq!(result.details.unwrap()["timed_out"], true);
}

#[tokio::test]
async fn huge_output_is_truncated() {
    if !require("/bin/dd") {
        eprintln!("skipping: /bin/dd is not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let result = run_dyn(
        &ExecTool::new(),
        json!({
            "program": "/bin/dd",
            "args": ["if=/dev/zero", "bs=60000", "count=1", "status=none"]
        }),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    let text = text_of(&result);
    assert!(text.contains("[output truncated:"), "{text}");
    assert_eq!(result.details.unwrap()["truncated"], true);
}

#[tokio::test]
async fn last_component_symlink_executes_as_the_target() {
    if !require("/bin/true") {
        eprintln!("skipping: /bin/true is not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("true-link");
    std::os::unix::fs::symlink("/bin/true", &link).unwrap();
    let via_link = run_dyn(
        &ExecTool::new(),
        json!({"program": link.to_string_lossy()}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    let via_target = run_dyn(
        &ExecTool::new(),
        json!({"program": "/bin/true"}),
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

/// High enough that libtest setup is unlikely to reuse the number before the
/// child's `/proc/self/fd` probe. Changing this does not relax the CLOEXEC contract.
const SENTINEL_MIN_FD: libc::c_int = 128;

#[tokio::test]
async fn inherited_nonstd_fd_is_not_inherited() {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    if !require("/usr/bin/test") {
        eprintln!("skipping: /usr/bin/test is not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let file = std::fs::File::open("/dev/null").unwrap();
    // SAFETY: `file` is live. F_DUPFD copies it to the first free descriptor
    // at or above `SENTINEL_MIN_FD` without setting FD_CLOEXEC.
    let raw = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, SENTINEL_MIN_FD) };
    assert!(
        raw >= SENTINEL_MIN_FD,
        "failed to duplicate sentinel fd: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: `raw` is a live descriptor we uniquely own after F_DUPFD.
    let cleared = unsafe { libc::fcntl(raw, libc::F_SETFD, 0) };
    assert_ne!(
        cleared,
        -1,
        "failed to clear FD_CLOEXEC: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: F_GETFD only reads flags on the live sentinel.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    assert_eq!(flags & libc::FD_CLOEXEC, 0, "sentinel still had FD_CLOEXEC");
    // SAFETY: `raw` is a uniquely owned descriptor after F_DUPFD.
    let sentinel = unsafe { OwnedFd::from_raw_fd(raw) };

    let result = run_dyn(
        &ExecTool::new(),
        json!({
            "program": "/usr/bin/test",
            "args": ["!", "-e", format!("/proc/self/fd/{raw}")],
        }),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    drop(sentinel);
    drop(file);
    assert!(
        !result.is_error,
        "inherited sentinel fd {raw} was still visible: {}",
        text_of(&result)
    );
}

#[tokio::test]
async fn execveat_failure_returns_through_command_error_channel() {
    if !require("/bin/true") {
        eprintln!("skipping: /bin/true is not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("truncated-elf");
    let mut header = std::fs::read("/bin/true").unwrap();
    header.truncate(64);
    std::fs::write(&program, header).unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&program, permissions).unwrap();
    }

    let started = Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        run_dyn(
            &ExecTool::new(),
            json!({"program": program.to_string_lossy()}),
            &ctx_at(dir.path()),
        ),
    )
    .await
    .expect("execveat failure hung instead of returning through Command");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "execveat failure took {:?}",
        started.elapsed()
    );
    let err = outcome.expect_err("truncated ELF must fail spawn, not run");
    assert!(matches!(err, ToolError::Execution(_)), "{err}");
    let text = err.to_string();
    assert!(text.contains("failed to spawn"), "{text}");
    assert!(
        !text.contains("end of file") && !text.to_ascii_lowercase().contains("eof"),
        "exec error channel returned EOF instead of the kernel failure: {text}"
    );
}
