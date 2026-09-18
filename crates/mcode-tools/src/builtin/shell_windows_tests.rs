use super::*;
use crate::builtin::fs_search::lexical_normalize;
use crate::builtin::test_support::ctx_at;

fn path_pwsh_candidate(path_var: Option<&std::ffi::OsStr>) -> Option<std::path::PathBuf> {
    for entry in std::env::split_paths(path_var.unwrap_or_default()) {
        if !lexical_normalize(&entry).is_absolute() {
            continue;
        }
        let candidate = entry.join("pwsh.exe");
        match std::fs::metadata(&candidate) {
            Ok(metadata) if metadata.is_file() => return Some(candidate),
            Ok(_) => return None,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    None
}

fn path_pwsh_is_usable() -> bool {
    let Some(candidate) = path_pwsh_candidate(std::env::var_os("PATH").as_deref()) else {
        return false;
    };
    std::process::Command::new(candidate)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "exit 0",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn pwsh_preflight_ignores_non_path_search_locations() {
    assert!(path_pwsh_candidate(None).is_none());
    let relative = std::env::join_paths([
        std::path::PathBuf::from("."),
        std::path::PathBuf::from("relative-bin"),
    ])
    .unwrap();
    assert!(path_pwsh_candidate(Some(&relative)).is_none());
}

macro_rules! require_path_pwsh {
    ($result:expr) => {{
        if !path_pwsh_is_usable() {
            eprintln!("skipping integration test: usable pwsh.exe is not on PATH");
            return;
        }
        $result
    }};
}

fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn assert_execution_identity(details: &serde_json::Value) {
    assert_eq!(details["shell"], "pwsh.exe");
    assert_eq!(details["image"], "pe");
    assert_eq!(
        details["digest_sha256"].as_str().unwrap().len(),
        64,
        "{details}"
    );
    assert_eq!(
        details["invocation_digest_sha256"].as_str().unwrap().len(),
        64,
        "{details}"
    );
    assert_eq!(
        details["identity"], details["invocation_digest_sha256"],
        "{details}"
    );
    assert!(
        details["image_identity"].as_str().unwrap().contains("vol:"),
        "{details}"
    );
    assert!(
        details["env_summary"]["count"].as_u64().unwrap() >= 1,
        "{details}"
    );
    let encoded = details["env_summary"].to_string();
    assert!(!encoded.contains("AWS_SECRET_ACCESS_KEY"), "{encoded}");
    assert!(!encoded.contains("NODE_OPTIONS"), "{encoded}");
}

#[tokio::test]
async fn captures_stdout_and_records_selected_shell() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result = require_path_pwsh!(
        run_dyn(
            &ShellTool::new(),
            json!({"command": "Write-Output 'hello'"}),
            &ctx,
        )
        .await
    )
    .unwrap();
    assert!(!result.is_error);
    assert_eq!(text_of(&result).trim(), "hello");
    assert_execution_identity(result.details.as_ref().unwrap());
}

#[tokio::test]
async fn cwd_reflects_session_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result = require_path_pwsh!(
        run_dyn(
            &ShellTool::new(),
            json!({"command": "Write-Output (Get-Location).Path"}),
            &ctx,
        )
        .await
    )
    .unwrap();
    let printed = std::fs::canonicalize(text_of(&result).trim()).unwrap();
    let expected = std::fs::canonicalize(dir.path()).unwrap();
    assert_eq!(printed, expected);
}

#[tokio::test]
async fn unicode_quotes_and_metacharacters_survive_without_requoting() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result = require_path_pwsh!(
        run_dyn(
            &ShellTool::new(),
            json!({"command": "Write-Output '中文 ''quote'' & $()'"}),
            &ctx,
        )
        .await
    )
    .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    assert_eq!(text_of(&result).trim(), "中文 'quote' & $()");
}

#[tokio::test]
async fn empty_command_succeeds_with_empty_output() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result =
        require_path_pwsh!(run_dyn(&ShellTool::new(), json!({"command": ""}), &ctx).await).unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    assert_eq!(text_of(&result), "");
    assert_execution_identity(result.details.as_ref().unwrap());
}

#[tokio::test]
async fn using_statement_remains_first_in_the_user_script() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result = require_path_pwsh!(
        run_dyn(
            &ShellTool::new(),
            json!({
                "command": "using namespace System.Text\nWrite-Output ([Encoding]::UTF8.WebName)"
            }),
            &ctx,
        )
        .await
    )
    .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    assert_eq!(text_of(&result).trim(), "utf-8");
}

#[tokio::test]
async fn constrained_language_runs_basic_cmdlets_without_a_launcher() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result = require_path_pwsh!(
        run_dyn(
            &ShellTool::new(),
            json!({
                "command": concat!(
                    "$ExecutionContext.SessionState.LanguageMode = ",
                    "'ConstrainedLanguage'\n",
                    "Write-Output $ExecutionContext.SessionState.LanguageMode\n",
                    "Get-Location | Select-Object -ExpandProperty Path\n",
                    "Write-Output 'restricted-basic-ok'"
                )
            }),
            &ctx,
        )
        .await
    )
    .unwrap();
    let text = text_of(&result);
    assert!(!result.is_error, "{text}");
    assert!(text.contains("ConstrainedLanguage"), "{text}");
    assert!(text.contains("restricted-basic-ok"), "{text}");
    assert!(!text.contains("Cannot invoke method"), "{text}");
    assert!(!text.contains("Only core types are supported"), "{text}");
}

#[tokio::test]
async fn non_zero_exit_is_error_result_not_tool_error() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result = require_path_pwsh!(
        run_dyn(
            &ShellTool::new(),
            json!({"command": "Write-Output 'oops'; exit 3"}),
            &ctx,
        )
        .await
    )
    .unwrap();
    assert!(result.is_error);
    assert!(text_of(&result).contains("oops"));
    assert!(text_of(&result).contains("[exit code: 3]"));
    assert_eq!(result.details.unwrap()["exit_code"], 3);
}

#[tokio::test]
async fn utf8_stderr_is_captured_and_labelled() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result = require_path_pwsh!(
        run_dyn(
            &ShellTool::new(),
            json!({"command": "Write-Error -Message '错误 err'; exit 0"}),
            &ctx,
        )
        .await
    )
    .unwrap();
    let text = text_of(&result);
    assert!(text.contains("[stderr]"), "{text}");
    assert!(text.contains("错误 err"), "{text}");
    assert!(!result.is_error);
}

const SHELL_SECRET_FILTER_DRIVER: &str =
    "builtin::shell::tests::windows::shell_secret_filter_driver";
const SHELL_INJECTED_SECRET: &str = "mcode-shell-secret-value";
const SHELL_INJECTED_NODE: &str = "--require=./not-a-real-loader.js";
const SHELL_INJECTED_PYTHONPATH: &str = r"C:\not-a-real-python-path";

#[tokio::test]
#[ignore = "spawned by child_omits_ambient_secrets_and_loader_variables"]
async fn shell_secret_filter_driver() {
    assert_eq!(
        std::env::var("AWS_SECRET_ACCESS_KEY").as_deref(),
        Ok(SHELL_INJECTED_SECRET)
    );
    assert_eq!(
        std::env::var("NODE_OPTIONS").as_deref(),
        Ok(SHELL_INJECTED_NODE)
    );
    assert_eq!(
        std::env::var("PYTHONPATH").as_deref(),
        Ok(SHELL_INJECTED_PYTHONPATH)
    );
    if !path_pwsh_is_usable() {
        eprintln!("skipping integration test: usable pwsh.exe is not on PATH");
        return;
    }
    let cwd = std::env::current_dir().unwrap();
    let result = run_dyn(
        &ShellTool::new(),
        json!({
            "command": concat!(
                "Write-Output (\"SECRET=$env:AWS_SECRET_ACCESS_KEY;\", ",
                "\"NODE=$env:NODE_OPTIONS;\", ",
                "\"PY=$env:PYTHONPATH\")"
            )
        }),
        &ctx_at(&cwd),
    )
    .await
    .unwrap();
    let text = text_of(&result);
    assert!(!result.is_error, "{text}");
    assert!(!text.contains(SHELL_INJECTED_SECRET), "{text}");
    assert!(!text.contains(SHELL_INJECTED_NODE), "{text}");
    assert!(!text.contains("not-a-real-python-path"), "{text}");
    let encoded = result.details.as_ref().unwrap().to_string();
    assert!(!encoded.contains(SHELL_INJECTED_SECRET), "{encoded}");
}

#[test]
fn child_omits_ambient_secrets_and_loader_variables() {
    if !path_pwsh_is_usable() {
        eprintln!("skipping integration test: usable pwsh.exe is not on PATH");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let current = std::env::current_exe().unwrap();
    let status = std::process::Command::new(current)
        .current_dir(dir.path())
        .env("AWS_SECRET_ACCESS_KEY", SHELL_INJECTED_SECRET)
        .env("NODE_OPTIONS", SHELL_INJECTED_NODE)
        .env("PYTHONPATH", SHELL_INJECTED_PYTHONPATH)
        .args([
            "--ignored",
            "--exact",
            SHELL_SECRET_FILTER_DRIVER,
            "--test-threads=1",
        ])
        .status()
        .unwrap();
    assert!(
        status.success(),
        "shell secret filter driver failed: {status}"
    );
}

#[tokio::test]
async fn timeout_kills_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let started = Instant::now();
    let result = require_path_pwsh!(
        run_dyn(
            &ShellTool::new(),
            json!({
                "command": "Write-Output 'started'; Start-Sleep -Seconds 30",
                "timeout_secs": 1,
            }),
            &ctx,
        )
        .await
    )
    .unwrap();
    let elapsed = started.elapsed();
    assert!(result.is_error);
    assert!(text_of(&result).contains("timed out after 1s"));
    assert!(elapsed < Duration::from_secs(15), "took {elapsed:?}");
    assert_eq!(result.details.unwrap()["timed_out"], true);
}

fn system32_ping() -> Option<std::path::PathBuf> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let ping = std::path::PathBuf::from(root)
        .join("System32")
        .join("ping.exe");
    ping.is_file().then_some(ping)
}

#[tokio::test]
async fn timeout_terminates_job_members_after_the_shell_has_exited() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());
    let Some(ping) = system32_ping() else {
        eprintln!("skipping: ping.exe is not present");
        return;
    };
    let ping_arg = powershell_quote(&ping.to_string_lossy());
    let command = format!(
        "$ErrorActionPreference = 'Stop'; \
         Start-Process -FilePath {ping_arg} -WindowStyle Hidden \
         -ArgumentList @('-n','50','-w','1000','127.0.0.1'); \
         Start-Sleep -Seconds 30"
    );

    let started = Instant::now();
    let result = require_path_pwsh!(
        run_dyn(
            &ShellTool::new(),
            json!({"command": command, "timeout_secs": 3}),
            &ctx,
        )
        .await
    )
    .unwrap();
    let elapsed = started.elapsed();
    assert!(result.is_error, "job-member timeout: {}", text_of(&result));
    assert!(
        text_of(&result).contains("timed out after 3s"),
        "job-member text: {}",
        text_of(&result)
    );
    assert!(elapsed < Duration::from_secs(12), "took {elapsed:?}");
}

#[tokio::test]
async fn timeout_kills_grandchild_process_tree() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());
    let Some(ping) = system32_ping() else {
        eprintln!("skipping: ping.exe is not present");
        return;
    };
    let ping_arg = powershell_quote(&ping.to_string_lossy());
    let command = format!(
        "$ErrorActionPreference = 'Stop'; \
         $p = Start-Process -FilePath {ping_arg} -Wait -NoNewWindow -PassThru \
         -ArgumentList @('-n','30','-w','1000','127.0.0.1'); \
         if ($null -ne $p.ExitCode -and $p.ExitCode -ne 0) {{ exit $p.ExitCode }}"
    );

    let started = Instant::now();
    let result = require_path_pwsh!(
        run_dyn(
            &ShellTool::new(),
            json!({"command": command, "timeout_secs": 5}),
            &ctx,
        )
        .await
    )
    .unwrap();
    let elapsed = started.elapsed();
    assert!(result.is_error, "grandchild timeout: {}", text_of(&result));
    assert!(text_of(&result).contains("timed out after 5s"));
    assert!(elapsed < Duration::from_secs(15), "took {elapsed:?}");
}

#[tokio::test]
async fn huge_output_is_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result = require_path_pwsh!(
        run_dyn(
            &ShellTool::new(),
            json!({"command": "[Console]::Out.Write(('x' * 60000))"}),
            &ctx,
        )
        .await
    )
    .unwrap();
    assert_truncated_counts(&result, 60_000, 0);
    assert!(text_of(&result).len() < 60_000);
}

#[tokio::test]
async fn five_mib_stdout_and_stderr_stay_bounded_and_complete() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());
    let started = Instant::now();
    let result = require_path_pwsh!(
        run_dyn(
            &ShellTool::new(),
            json!({
                "command": concat!(
                    "[Console]::Out.Write(('x' * 5242880)); ",
                    "[Console]::Error.Write(('y' * 5242880))"
                )
            }),
            &ctx,
        )
        .await
    )
    .unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(20),
        "bounded capture took {elapsed:?}"
    );
    assert_truncated_counts(&result, 5 * 1024 * 1024, 5 * 1024 * 1024);
}

#[tokio::test]
async fn oversized_encoded_command_is_invalid_args() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let err = run_dyn(
        &ShellTool::new(),
        json!({"command": "界".repeat(20_000)}),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)));
    assert!(err.to_string().contains("32,767 UTF-16-code-unit"), "{err}");
    assert!(err.to_string().contains("maximum for executable"), "{err}");
}

#[tokio::test]
async fn empty_output_success() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let result =
        require_path_pwsh!(run_dyn(&ShellTool::new(), json!({"command": "$null"}), &ctx).await)
            .unwrap();
    assert!(!result.is_error);
    assert_eq!(text_of(&result), "");
}
