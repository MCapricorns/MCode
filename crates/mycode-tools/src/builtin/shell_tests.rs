use super::*;
use crate::builtin::exec::{ResolveError, prepare_from_snapshot, snapshot_child_environment};
use crate::builtin::test_support::{run_dyn, text_of};
use crate::ctx::ToolCtx;
use crate::tool::{ToolDyn, ToolError};
use serde_json::json;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
#[path = "shell_windows_tests.rs"]
mod windows;

#[cfg(unix)]
#[path = "shell_unix_tests.rs"]
mod unix;

fn assert_truncated_counts(result: &ToolResult, stdout_bytes: u64, stderr_bytes: u64) {
    assert!(!result.is_error, "{}", text_of(result));
    let text = text_of(result);
    let total = stdout_bytes.saturating_add(stderr_bytes);
    assert!(
        text.contains(&format!(
            "[output truncated: showed first {MAX_OUTPUT_BYTES} of {total} bytes]"
        )),
        "{text}"
    );
    let details = result.details.as_ref().unwrap();
    assert_eq!(details["truncated"], true);
    assert_eq!(details["stdout_bytes"], stdout_bytes);
    assert_eq!(details["stderr_bytes"], stderr_bytes);
}

fn repeated_encoded_ascii(prefix: &[u8], encoded_x: &[u8]) -> Vec<u8> {
    let count = MAX_OUTPUT_BYTES + 1;
    let mut output = Vec::with_capacity(prefix.len() + encoded_x.len() * count);
    output.extend_from_slice(prefix);
    for _ in 0..count {
        output.extend_from_slice(encoded_x);
    }
    output
}

async fn assert_encoded_ascii_truncates(output: Vec<u8>) {
    let total_bytes = u64::try_from(output.len()).unwrap();
    let mut pipe = Some(std::io::Cursor::new(output));
    let mut stdout = CapturedStream::new();

    read_bounded(&mut pipe, &mut stdout).await.unwrap();

    assert!(pipe.is_none());
    let result = format_result(
        None,
        "fixture",
        "fixture",
        stdout,
        CapturedStream::default(),
        0,
        false,
        None,
    );
    assert_truncated_counts(&result, total_bytes, 0);
    let text = text_of(&result);
    assert!(text.starts_with(&"x".repeat(MAX_OUTPUT_BYTES)), "{text}");
    assert!(!text.contains('\u{fffd}'), "{text}");
}

fn decode_utf16le_base64(encoded: &str) -> String {
    let bytes = BASE64_STANDARD.decode(encoded).unwrap();
    let (chunks, remainder) = bytes.as_chunks::<2>();
    let units = chunks
        .iter()
        .copied()
        .map(u16::from_le_bytes)
        .collect::<Vec<_>>();
    assert!(remainder.is_empty());
    String::from_utf16(&units).unwrap()
}

#[test]
fn powershell_encoding_is_direct_and_round_trips_without_a_wrapper() {
    let command = "using namespace System.Text\nWrite-Output '中文 ''quote'' \"double\" & $()'";
    let executable = Path::new("pwsh.exe");
    let encoded = encode_powershell_command(command, executable).unwrap();
    let decoded = decode_utf16le_base64(&encoded);

    assert_eq!(decoded, command);
    for forbidden in [
        "UTF8Encoding]::new",
        "ScriptBlock]::Create",
        "Encoding]::Unicode.GetString",
        "Convert]::FromBase64String",
    ] {
        assert!(
            !decoded.contains(forbidden),
            "unexpected wrapper API: {forbidden}"
        );
    }
    assert!(
        powershell_command_line_units(executable, encoded.len()).unwrap()
            <= WINDOWS_COMMAND_LINE_LIMIT_UTF16_UNITS
    );
}

#[test]
fn powershell_command_line_budget_counts_the_utf16_terminator_exactly() {
    let executable = Path::new("pwsh.exe");
    let maximum = maximum_encoded_command_chars(executable).unwrap();

    assert_eq!(
        powershell_command_line_units(executable, maximum),
        Some(WINDOWS_COMMAND_LINE_LIMIT_UTF16_UNITS)
    );
    assert_eq!(
        powershell_command_line_units(executable, maximum + 1),
        Some(WINDOWS_COMMAND_LINE_LIMIT_UTF16_UNITS + 1)
    );
}

#[test]
fn powershell_encoding_rejects_commands_above_the_exact_limit() {
    let executable = Path::new("pwsh.exe");
    let err = encode_powershell_command(&"界".repeat(20_000), executable).unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)));
    assert!(err.to_string().contains("32,767 UTF-16-code-unit"), "{err}");
    assert!(
        err.to_string().contains("including the terminator"),
        "{err}"
    );
    assert!(err.to_string().contains("maximum for executable"), "{err}");
}

#[test]
fn powershell_encoding_error_does_not_embed_absolute_executable_path() {
    let executable = Path::new("C:/Users/host/.mycode/bin/powershell/7.6.5/x86_64/pwsh.exe");
    let err = encode_powershell_command(&"世".repeat(20_000), executable).unwrap_err();
    let msg = err.to_string();
    assert!(!msg.contains("C:/Users/host"), "{msg}");
    assert!(!msg.contains(".mycode"), "{msg}");
    assert!(msg.contains("pwsh.exe"), "{msg}");
    assert!(msg.contains("maximum for executable"), "{msg}");
}

#[test]
fn powershell_encoding_accepts_an_empty_command() {
    let encoded = encode_powershell_command("", Path::new("pwsh.exe")).unwrap();
    assert!(encoded.is_empty(), "{encoded}");
    assert_eq!(decode_utf16le_base64(&encoded), "");
}

#[test]
fn backend_preference_order_matches_the_platform_contract() {
    #[cfg(windows)]
    assert_eq!(WINDOWS_SHELL_EXECUTABLE, "pwsh.exe");
    #[cfg(not(windows))]
    assert_eq!(
        SHELL_CANDIDATES
            .iter()
            .map(|candidate| candidate.executable)
            .collect::<Vec<_>>(),
        ["/bin/bash", "bash", "sh"]
    );
}

#[test]
fn typed_not_found_is_the_only_fallback() {
    let not_found = ResolveError::NotFound {
        program: "pwsh.exe".into(),
        searched: Some(3),
    };
    assert!(not_found.is_not_found());
    assert_eq!(
        shell_candidate_action(&not_found),
        ShellCandidateAction::TryNext
    );

    for message in [
        "program is not a kernel-loadable PE image",
        "pinned executable changed before launch",
        "program could not be opened: Access is denied. (os error 5)",
        "command cancelled before completion",
    ] {
        let error = ResolveError::Other(ToolError::InvalidArgs(message.into()));
        assert!(!error.is_not_found(), "{message}");
        assert_eq!(
            shell_candidate_action(&error),
            ShellCandidateAction::FailClosed,
            "{message}"
        );
    }
}

#[test]
fn empty_path_snapshot_classifies_basename_as_not_found() {
    let cwd = tempfile::tempdir().unwrap();
    let error = prepare_from_snapshot(cwd.path(), "pwsh.exe", &[], &[], &CancellationToken::new())
        .expect_err("empty PATH must not invent an executable");
    assert!(error.is_not_found(), "{error}");
    assert_eq!(
        shell_candidate_action(&error),
        ShellCandidateAction::TryNext
    );
}

#[cfg(any(windows, unix))]
#[test]
fn non_regular_path_hit_blocks_later_shell_candidate() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    #[cfg(windows)]
    let (name, source) = {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        (
            "pwsh.exe",
            std::path::PathBuf::from(system_root)
                .join("System32")
                .join("whoami.exe"),
        )
    };
    #[cfg(target_os = "macos")]
    let (name, source) = ("sh", std::path::PathBuf::from("/usr/bin/true"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let (name, source) = ("sh", std::path::PathBuf::from("/bin/true"));
    assert!(
        source.is_file(),
        "required host image is absent: {source:?}"
    );
    std::fs::create_dir(first.join(name)).unwrap();
    std::fs::copy(source, second.join(name)).unwrap();
    let path = std::env::join_paths([first, second]).unwrap();
    let env = vec![(std::ffi::OsString::from("PATH"), path)];

    let error = prepare_from_snapshot(root.path(), name, &[], &env, &CancellationToken::new())
        .expect_err("a non-regular first hit must block later shell candidates");
    assert!(!error.is_not_found(), "{error}");
    assert_eq!(
        shell_candidate_action(&error),
        ShellCandidateAction::FailClosed
    );
    assert!(error.to_string().contains("not a regular file"), "{error}");
}

#[cfg(windows)]
#[test]
fn non_pe_path_hit_is_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let decoy = dir.path().join("pwsh.exe");
    std::fs::write(&decoy, b"not a pe image").unwrap();
    let env = vec![(
        std::ffi::OsString::from("PATH"),
        dir.path().as_os_str().to_os_string(),
    )];
    let error = prepare_from_snapshot(dir.path(), "pwsh.exe", &[], &env, &CancellationToken::new())
        .expect_err("non-PE decoy must not look like NotFound");
    assert!(!error.is_not_found(), "{error}");
    assert_eq!(
        shell_candidate_action(&error),
        ShellCandidateAction::FailClosed
    );
    let text = error.into_tool_error().to_string();
    assert!(text.contains("not a kernel-loadable"), "{text}");
    assert!(!text.contains("not found"), "{text}");
}

#[tokio::test]
async fn bounded_reader_drains_all_bytes_and_retains_only_render_prefix() {
    let produced = 5 * 1024 * 1024;
    let mut pipe = Some(std::io::Cursor::new(vec![b'x'; produced]));
    let mut captured = CapturedStream::new();

    read_bounded(&mut pipe, &mut captured).await.unwrap();

    assert!(pipe.is_none());
    assert_eq!(captured.total_bytes, produced as u64);
    assert_eq!(captured.retained.len(), MAX_RETAINED_OUTPUT_BYTES);
    assert!(captured.retained.iter().all(|byte| *byte == b'x'));
}

#[tokio::test]
async fn bounded_reader_preserves_utf8_bom_past_render_limit() {
    let output = repeated_encoded_ascii(&[0xef, 0xbb, 0xbf], b"x");
    assert_encoded_ascii_truncates(output).await;
}

#[tokio::test]
async fn bounded_reader_preserves_utf16_prefix_past_render_limit() {
    for (bom, encoded_x) in [([0xff, 0xfe], [b'x', 0]), ([0xfe, 0xff], [0, b'x'])] {
        let output = repeated_encoded_ascii(&bom, &encoded_x);
        assert_encoded_ascii_truncates(output).await;
    }
}

#[test]
fn collection_error_reports_teardown_failure() {
    let collection = std::io::Error::other("pipe read failed");
    let teardown = std::io::Error::other("kill denied");
    let combined = collection_error(&collection, Some(teardown)).to_string();
    assert!(combined.contains("pipe read failed"), "{combined}");
    assert!(
        combined.contains("termination failed: kill denied"),
        "{combined}"
    );

    let collection_only = collection_error(&collection, None).to_string();
    assert!(
        collection_only.contains("pipe read failed"),
        "{collection_only}"
    );
    assert!(
        !collection_only.contains("termination failed"),
        "{collection_only}"
    );
}

#[test]
fn timeout_notice_does_not_claim_kill_when_teardown_failed() {
    let failed = timed_out_result(
        "fixture",
        "pwsh.exe",
        CapturedStream::default(),
        CapturedStream::default(),
        1,
        Duration::from_secs(1),
        Err(std::io::Error::other("job terminate denied")),
        true,
    );
    let failed_text = text_of(&failed);
    assert!(failed_text.contains("timed out after 1s"), "{failed_text}");
    assert!(failed_text.contains("termination failed"), "{failed_text}");
    assert!(
        failed_text.contains("job terminate denied"),
        "{failed_text}"
    );
    assert!(!failed_text.contains("was killed"), "{failed_text}");

    let killed = timed_out_result(
        "fixture",
        "pwsh.exe",
        CapturedStream::default(),
        CapturedStream::default(),
        1,
        Duration::from_secs(2),
        Ok(()),
        true,
    );
    assert_eq!(
        text_of(&killed).trim(),
        "[command timed out after 2s and was killed]"
    );
}

#[test]
fn cancelled_error_reports_teardown_failure() {
    let err = command_cancelled_error(Some(std::io::Error::other("sigkill denied")));
    let text = err.to_string();
    assert!(text.contains("cancelled before completion"), "{text}");
    assert!(text.contains("termination failed"), "{text}");
    assert!(text.contains("sigkill denied"), "{text}");
    assert!(
        !command_cancelled_error(None)
            .to_string()
            .contains("termination failed")
    );
}

#[test]
fn public_contract_keeps_shell_name_and_unsandboxed_boundary() {
    let tool = ShellTool::new();
    let dyn_tool: &dyn ToolDyn = &tool;
    let spec = dyn_tool.spec();

    assert_eq!(spec.name, "shell");
    assert_ne!(spec.name, "bash");
    assert!(spec.params_schema["properties"]["command"].is_object());
    assert!(spec.params_schema["properties"]["timeout_secs"].is_object());
    assert!(spec.description.contains("platform-shell"));
    assert!(spec.description.contains("PowerShell 7"));
    assert!(spec.description.contains("unsandboxed"));
    assert!(spec.description.contains("not a sandbox"));
    assert!(spec.description.contains("no Core permission prompt"));
    assert!(spec.description.contains("pipelines"));
    assert!(tool.prompt_snippet().unwrap().contains("platform shell"));
    assert_eq!(dyn_tool.concurrency(), Concurrency::Exclusive);
    assert!(dyn_tool.mutates_fs());
    assert!(!dyn_tool.requires_file_preflight());
    assert!(!dyn_tool.requires_search_preflight());
}

#[test]
fn captured_text_prefers_utf8_over_legacy_code_pages() {
    assert_eq!(decode_captured_text("中文".as_bytes()), "中文");

    let mut utf16le = vec![0xff, 0xfe];
    utf16le.extend("中文".encode_utf16().flat_map(u16::to_le_bytes));
    assert_eq!(decode_captured_text(&utf16le), "中文");

    let mut utf16be = vec![0xfe, 0xff];
    utf16be.extend("中文".encode_utf16().flat_map(u16::to_be_bytes));
    assert_eq!(decode_captured_text(&utf16be), "中文");

    // GBK bytes for "中文" (code page 936): decoded when the console output
    // code page matches, kept lossy otherwise.
    let legacy_code_page = [0xd6, 0xd0, 0xce, 0xc4];
    #[cfg(windows)]
    {
        let console = unsafe { windows_sys::Win32::System::Console::GetConsoleOutputCP() };
        let decoded = decode_captured_text(&legacy_code_page);
        if console == 936 {
            assert_eq!(decoded, "中文", "GBK console output must decode");
        } else {
            assert_ne!(decoded, "中文");
            assert!(decoded.contains('\u{fffd}'));
        }
    }
    #[cfg(not(windows))]
    {
        let decoded = decode_captured_text(&legacy_code_page);
        assert_ne!(decoded, "中文");
        assert!(decoded.contains('\u{fffd}'));
    }
}

#[test]
fn allowlisted_snapshot_omits_secrets_and_loader_variables() {
    let env = snapshot_child_environment().expect("environment snapshot");
    let keys: Vec<String> = env
        .iter()
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .collect();
    for forbidden in [
        "AWS_SECRET_ACCESS_KEY",
        "GITHUB_TOKEN",
        "SSH_AUTH_SOCK",
        "LD_PRELOAD",
        "PYTHONPATH",
        "NODE_OPTIONS",
        "BASH_ENV",
        "IFS",
    ] {
        assert!(
            !keys.iter().any(|key| key.eq_ignore_ascii_case(forbidden)),
            "allowlist leaked {forbidden}: {keys:?}"
        );
    }
}

#[tokio::test]
async fn missing_cwd_is_invalid_args_without_host_path() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing-cwd");
    let ctx = ToolCtx::new(&missing);
    let err = run_dyn(&ShellTool::new(), json!({"command": "echo hi"}), &ctx)
        .await
        .unwrap_err();
    let msg = err.to_string();
    let abs = missing.to_string_lossy();
    let parent = dir.path().to_string_lossy();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{msg}");
    assert!(!msg.contains(abs.as_ref()), "{msg}");
    assert!(!msg.contains(parent.as_ref()), "{msg}");
    assert!(msg.contains("working directory ."), "{msg}");
}

#[tokio::test]
async fn cancellation_token_aborts_before_spawning_or_provisioning() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("cancelled-command-ran");
    let cancel = CancellationToken::new();
    cancel.cancel();
    let ctx = ToolCtx::new(dir.path()).with_cancel(cancel);
    let err = run_dyn(&ShellTool::new(), json!({"command": "echo hi"}), &ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)));
    assert!(err.to_string().contains("cancelled"), "{err}");
    assert!(!marker.exists(), "pre-cancelled command was started");
}
