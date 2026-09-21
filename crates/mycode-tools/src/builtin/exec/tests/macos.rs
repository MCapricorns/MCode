//! macOS native structured-exec tests.
use super::*;

#[tokio::test]
async fn true_fixture_is_required_or_skipped() {
    if !Path::new("/usr/bin/true").is_file() {
        eprintln!("skipping: /usr/bin/true is not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let result = run_dyn(
        &ExecTool::new(),
        json!({"program": "/usr/bin/true"}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    assert_execution_metadata(&result, "arm64", false);
}

#[test]
fn current_path_identity_detects_replacement_and_accepts_restoration() {
    use std::os::unix::fs::PermissionsExt as _;

    let source = Path::new("/usr/bin/true");
    if !source.is_file() {
        eprintln!("skipping: /usr/bin/true is not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let candidate = dir.path().join("candidate");
    let displaced = dir.path().join("candidate.displaced");
    std::fs::copy(source, &candidate).unwrap();
    std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o700)).unwrap();
    let pinned = super::super::resolve::pin_program(
        dir.path(),
        candidate.to_str().unwrap(),
        &[],
        &tokio_util::sync::CancellationToken::new(),
    )
    .unwrap();
    super::super::macos::verify_current_path_identity(&pinned).unwrap();

    std::fs::rename(&candidate, &displaced).unwrap();
    std::fs::copy(source, &candidate).unwrap();
    std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o700)).unwrap();
    let error =
        super::super::macos::verify_current_path_identity(&pinned).expect_err("replacement vnode");
    assert!(error.to_string().contains("does not match"), "{error}");

    std::fs::remove_file(&candidate).unwrap();
    std::fs::rename(&displaced, &candidate).unwrap();
    super::super::macos::verify_current_path_identity(&pinned).unwrap();
}

#[tokio::test]
async fn rosetta_execution_identity_is_recorded_when_available() {
    use std::os::unix::fs::PermissionsExt as _;

    let lipo = Path::new("/usr/bin/lipo");
    let source = Path::new("/usr/bin/true");
    if !lipo.is_file() || !source.is_file() {
        eprintln!("skipping: lipo or /usr/bin/true is unavailable");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path().join("true-x86_64");
    let thin = std::process::Command::new(lipo)
        .args(["-thin", "x86_64"])
        .arg(source)
        .arg("-output")
        .arg(&fixture)
        .status();
    if !thin.is_ok_and(|status| status.success()) {
        eprintln!("skipping: no extractable x86_64 fixture");
        return;
    }
    std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o700)).unwrap();
    let rosetta_probe = std::process::Command::new(&fixture).status();
    if !rosetta_probe.is_ok_and(|status| status.success()) {
        eprintln!("skipping: Rosetta is unavailable");
        return;
    }

    let result = run_dyn(
        &ExecTool::new(),
        json!({"program": fixture.to_string_lossy()}),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    assert_execution_metadata(&result, "x86_64", true);
}

fn assert_execution_metadata(result: &ToolResult, architecture: &str, translated: bool) {
    let details = result.details.as_ref().expect("details");
    assert_eq!(details["loaded_architecture"], architecture);
    assert_eq!(details["translated"], translated);
    let identity = details["identity"].as_str().expect("identity");
    assert!(
        identity.contains(&format!("arch:{architecture}")),
        "{identity}"
    );
    assert!(
        identity.contains(&format!("translated:{translated}")),
        "{identity}"
    );
}

#[tokio::test]
async fn escaped_pipe_holder_does_not_block_timeout() {
    let required = ["/bin/bash", "/bin/sleep", "/usr/bin/true"];
    if required.iter().any(|path| !Path::new(path).is_file()) {
        eprintln!("skipping: bash, sleep, or true is not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let result = run_dyn(
        &ExecTool::new(),
        json!({
            "program": "/bin/bash",
            "args": ["-c", "set -m; /bin/sleep 3 & exec /usr/bin/true"],
            "timeout_secs": 1
        }),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();
    assert!(result.is_error);
    assert!(text_of(&result).contains("timed out after 1s"));
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
}

#[tokio::test]
async fn timeout_reuses_waiter_after_child_closes_stdio() {
    if !Path::new("/bin/sh").is_file() {
        eprintln!("skipping: /bin/sh is not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let result = run_dyn(
        &ExecTool::new(),
        json!({
            "program": "/bin/sh",
            "args": ["-c", "exec >/dev/null 2>&1; sleep 30"],
            "timeout_secs": 1
        }),
        &ctx_at(dir.path()),
    )
    .await
    .unwrap();
    let text = text_of(&result);
    assert!(result.is_error);
    assert!(text.contains("timed out after 1s"), "{text}");
    assert!(!text.contains("termination failed"), "{text}");
}

#[tokio::test]
async fn aborted_execution_kills_and_reaps_the_leader() {
    if !Path::new("/bin/sh").is_file() {
        eprintln!("skipping: /bin/sh is not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    let pid_file = cwd.join("child.pid");
    let task = tokio::spawn(async move {
        run_dyn(
            &ExecTool::new(),
            json!({
                "program": "/bin/sh",
                "args": ["-c", "echo $$ > child.pid; exec sleep 30"],
                "timeout_secs": 60
            }),
            &ctx_at(&cwd),
        )
        .await
    });
    for _ in 0..100 {
        if pid_file.is_file() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let pid = std::fs::read_to_string(&pid_file)
        .expect("child did not publish its pid")
        .trim()
        .parse::<libc::pid_t>()
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    for _ in 0..100 {
        // SAFETY: signal 0 only checks whether the numeric pid exists.
        if unsafe { libc::kill(pid, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("aborted exec child {pid} was not reaped");
}
