use super::*;
use crate::builtin::test_support::ctx_at;

fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

#[tokio::test]
async fn captures_stdout_and_runs_in_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result = run_dyn(&ShellTool::new(), json!({"command": "echo hello"}), &ctx)
        .await
        .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    assert_eq!(text_of(&result), "hello\n");
    assert!(matches!(
        result.details.as_ref().unwrap()["shell"].as_str(),
        Some("/bin/bash" | "bash" | "sh")
    ));
}

#[tokio::test]
async fn selected_shell_identifier_is_effective_argv0() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result = run_dyn(
        &ShellTool::new(),
        json!({"command": "printf '%s' \"$0\""}),
        &ctx,
    )
    .await
    .unwrap();
    let identifier = result.details.as_ref().unwrap()["shell"].as_str().unwrap();
    assert_eq!(text_of(&result), identifier);
}

#[tokio::test]
async fn symlinked_shell_preserves_alias_argv0_for_multicall_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let target = std::path::Path::new("/bin/sh");
    if !target.is_file() {
        eprintln!("skipping: /bin/sh is not present");
        return;
    }
    let alias = root.path().join("sh");
    std::os::unix::fs::symlink(target, &alias).unwrap();
    let alias = alias
        .to_str()
        .expect("temporary path is Unicode")
        .to_owned();
    let args = vec!["-c".to_owned(), "printf '%s' \"$0\"".to_owned()];
    let env = snapshot_child_environment().unwrap();
    let cancel = CancellationToken::new();
    let prepared = prepare_from_snapshot(root.path(), &alias, &args, &env, &cancel).unwrap();
    let lease = acquire_execution_lease().await;
    let deadline = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(deadline);
    let outcome = run_prepared(prepared, lease, &cancel, &mut deadline)
        .await
        .unwrap();

    let (status, stdout) = match outcome {
        RunOutcome::Done { status, stdout, .. } => (status, stdout),
        _ => panic!("symlinked shell did not complete"),
    };
    assert!(status.success(), "symlinked shell exited with {status}");
    assert_eq!(decode_captured_text(&stdout.retained), alias);
}

#[tokio::test]
async fn pwd_reflects_session_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result = run_dyn(&ShellTool::new(), json!({"command": "pwd"}), &ctx)
        .await
        .unwrap();
    let printed = std::fs::canonicalize(text_of(&result).trim()).unwrap();
    let expected = std::fs::canonicalize(dir.path()).unwrap();
    assert_eq!(printed, expected);
}

#[tokio::test]
async fn non_zero_exit_is_error_result_not_tool_error() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result = run_dyn(
        &ShellTool::new(),
        json!({"command": "echo oops; exit 3"}),
        &ctx,
    )
    .await
    .unwrap();
    assert!(result.is_error);
    assert!(text_of(&result).contains("oops"));
    assert!(text_of(&result).contains("[exit code: 3]"));
    assert_eq!(result.details.unwrap()["exit_code"], 3);
}

#[tokio::test]
async fn stderr_is_captured_and_labelled() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result = run_dyn(
        &ShellTool::new(),
        json!({"command": "echo out; echo err >&2"}),
        &ctx,
    )
    .await
    .unwrap();
    let text = text_of(&result);
    assert!(text.contains("out"));
    assert!(text.contains("[stderr]"));
    assert!(text.contains("err"));
    assert!(!result.is_error);
}

#[tokio::test]
async fn timeout_kills_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let started = Instant::now();
    let result = run_dyn(
        &ShellTool::new(),
        json!({"command": "echo started; sleep 30", "timeout_secs": 1}),
        &ctx,
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
async fn timeout_kills_grandchildren_not_just_the_shell() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());
    let log = dir.path().join("beat.log");
    let log_arg = shell_quote(&log);

    // The call deadline starts at the tool call and covers the process-wide
    // execution lease wait, so the window must stay wide enough to start the
    // shell under a fully parallel suite; the timeout must still land while
    // the grandchild loop is running.
    let result = run_dyn(
        &ShellTool::new(),
        json!({
            "command": format!(
                "echo go; while true; do echo x >> {log_arg}; sleep 0.2; done & sleep 30"
            ),
            "timeout_secs": 5,
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(result.is_error);
    assert!(text_of(&result).contains("timed out after 5s"));

    tokio::time::sleep(Duration::from_millis(800)).await;
    let first = std::fs::read_to_string(&log)
        .map(|s| s.lines().count())
        .unwrap_or(0);
    tokio::time::sleep(Duration::from_millis(800)).await;
    let second = std::fs::read_to_string(&log)
        .map(|s| s.lines().count())
        .unwrap_or(0);
    assert!(first > 0, "grandchild never ran (log missing)");
    assert_eq!(first, second, "grandchild survived the group kill");
}

#[tokio::test]
async fn huge_output_is_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result = run_dyn(
        &ShellTool::new(),
        json!({"command": "printf '%60000s' '' | tr ' ' x"}),
        &ctx,
    )
    .await
    .unwrap();
    assert_truncated_counts(&result, 60_000, 0);
    assert!(text_of(&result).len() < 60_000);
}

#[tokio::test]
async fn five_mib_stdout_and_stderr_stay_bounded_and_complete() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());
    let started = Instant::now();
    let result = run_dyn(
        &ShellTool::new(),
        json!({
            "command": concat!(
                "dd if=/dev/zero bs=65536 count=80 2>/dev/null; ",
                "dd if=/dev/zero bs=65536 count=80 2>/dev/null | cat >&2"
            )
        }),
        &ctx,
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(20),
        "bounded capture took {elapsed:?}"
    );
    assert_truncated_counts(&result, 5 * 1024 * 1024, 5 * 1024 * 1024);
}

#[tokio::test]
async fn per_call_timeout_overrides_default() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result = run_dyn(
        &ShellTool::with_default_timeout(2),
        json!({"command": "sleep 4; echo done", "timeout_secs": 30}),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{:?}", text_of(&result));
    assert_eq!(text_of(&result).trim(), "done");

    let result = run_dyn(
        &ShellTool::with_default_timeout(1),
        json!({"command": "sleep 30"}),
        &ctx,
    )
    .await
    .unwrap();
    assert!(result.is_error);
    assert!(text_of(&result).contains("timed out after 1s"));
}

#[tokio::test]
async fn empty_output_success() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result = run_dyn(&ShellTool::new(), json!({"command": "true"}), &ctx)
        .await
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(text_of(&result), "");
}
