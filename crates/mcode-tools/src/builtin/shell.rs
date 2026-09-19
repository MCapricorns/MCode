//! `shell` — run a platform-native shell command in the session cwd.
//!
//! The public name is `shell`. There is no `bash` alias. Windows uses
//! PowerShell 7; POSIX hosts use an explicit POSIX shell candidate list.
//! Launch always goes through structured exec: one cwd/env/PATH snapshot per
//! call, allowlisted environment, pinned identity, and contained spawn.
//! Candidate fallback is allowed only for a typed executable-not-found
//! result. Execution is unsandboxed current-user file and network authority;
//! environment filtering is not a sandbox. Valid calls run directly with no
//! Core permission prompt. Use this tool for pipelines, redirection,
//! expansion, and scripts; filesystem and search tools stay in-process.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

#[cfg(any(windows, test))]
use base64::Engine as _;
#[cfg(any(windows, test))]
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use crate::builtin::exec::{
    ExecutionMetadata, PreparedIdentity, PreparedInvocation, ResolveError, RunOutcome,
    apply_execution_details, prepare_from_snapshot, prepared_identity, run_prepared,
    snapshot_child_environment,
};
use crate::builtin::fs_search::run_blocking_supervised;
use crate::builtin::process::{
    CapturedStream, ExecutionLease, MAX_OUTPUT_BYTES, acquire_execution_lease, decode_captured_text,
};
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Concurrency, Tool, ToolError, ToolResult};

/// Default command timeout (seconds).
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Maximum `CreateProcessW` command-line length, including its terminator.
#[cfg(any(windows, test))]
const WINDOWS_COMMAND_LINE_LIMIT_UTF16_UNITS: usize = 32_767;

/// PowerShell arguments placed before the directly encoded user script.
#[cfg(any(windows, test))]
const POWERSHELL_ARGUMENTS: &[&str] = &[
    "-NoLogo",
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy",
    "Bypass",
    "-EncodedCommand",
];

#[cfg(windows)]
const WINDOWS_SHELL_EXECUTABLE: &str = "pwsh.exe";

#[cfg(not(windows))]
#[derive(Debug, Clone, Copy)]
struct ShellCandidate {
    executable: &'static str,
}

#[cfg(not(windows))]
const SHELL_CANDIDATES: &[ShellCandidate] = &[
    ShellCandidate {
        executable: "/bin/bash",
    },
    ShellCandidate { executable: "bash" },
    ShellCandidate { executable: "sh" },
];

#[cfg(test)]
pub(crate) use crate::builtin::process::{MAX_RETAINED_OUTPUT_BYTES, read_bounded};

/// Whether another shell candidate may be attempted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShellCandidateAction {
    /// PATH or path lookup missed the executable.
    TryNext,
    /// Image, identity, digest, permission, or any other failure.
    FailClosed,
}

/// Classifies a prepare failure for shell candidate fallback.
#[must_use]
pub(crate) fn shell_candidate_action(error: &ResolveError) -> ShellCandidateAction {
    if error.is_not_found() {
        ShellCandidateAction::TryNext
    } else {
        ShellCandidateAction::FailClosed
    }
}

/// Returns the identifier used before shell selection finishes.
pub(crate) fn preferred_identifier() -> &'static str {
    #[cfg(windows)]
    return WINDOWS_SHELL_EXECUTABLE;
    #[cfg(not(windows))]
    SHELL_CANDIDATES[0].executable
}

/// The `shell` builtin.
#[derive(Debug)]
pub struct ShellTool {
    default_timeout: Duration,
}

impl ShellTool {
    /// A shell tool with the default 120 s timeout.
    #[must_use]
    pub fn new() -> Self {
        Self {
            default_timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    /// A shell tool with a custom default timeout; per-call `timeout_secs`
    /// arguments still take precedence.
    #[must_use]
    pub fn with_default_timeout(secs: u64) -> Self {
        Self {
            default_timeout: Duration::from_secs(secs),
        }
    }
}

impl Default for ShellTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Arguments for [`ShellTool`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShellArgs {
    /// Command to execute with the platform shell (POSIX shell on
    /// macOS/Linux, PowerShell 7 on Windows) using the session cwd.
    pub command: String,
    /// Timeout in seconds for this command (default: 120).
    pub timeout_secs: Option<u64>,
}

struct PreparedShell {
    identifier: &'static str,
    invocation: PreparedInvocation,
    lease: ExecutionLease,
}

#[async_trait]
impl Tool for ShellTool {
    type Args = ShellArgs;
    type Output = ();

    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a platform-shell script for pipelines, redirection, expansion, \
         and shell syntax. Filesystem and search tools stay in-process; do not \
         use this tool to read, write, edit, grep, or find files. Windows uses \
         PowerShell 7 (`pwsh.exe`); POSIX hosts use a POSIX shell. Execution is \
         unsandboxed current-user execution with normal file and network access; \
         environment filtering is not a sandbox. Same-account processes outside \
         this host are outside the security boundary. Captured stdout/stderr is \
         truncated beyond 50 KiB; a non-zero exit is an error result, not a \
         tool failure. Default timeout: 120 s. There is no Core permission prompt."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "shell: run a platform shell script (command, optional \
             timeout_secs).",
        )
    }

    fn concurrency(&self) -> Concurrency {
        Concurrency::Exclusive
    }

    fn mutates_fs(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let timeout = Duration::from_secs(
            args.timeout_secs
                .unwrap_or(self.default_timeout.as_secs())
                .max(1),
        );
        let started = Instant::now();
        if ctx.cancel.is_cancelled() {
            return Err(command_cancelled_error(None));
        }

        let command = args.command;
        let identifier = preferred_identifier();
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let lease = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return Err(command_cancelled_error(None)),
            _ = &mut deadline => {
                return Ok(timed_out_before_spawn_result(
                    &command,
                    identifier,
                    started.elapsed().as_millis() as u64,
                    timeout,
                ));
            }
            lease = acquire_execution_lease() => lease,
        };

        let prepared =
            match prepare_shell(&command, &ctx.cwd, lease, &ctx.cancel, &mut deadline).await? {
                Some(prepared) => prepared,
                None => {
                    return Ok(timed_out_before_spawn_result(
                        &command,
                        identifier,
                        started.elapsed().as_millis() as u64,
                        timeout,
                    ));
                }
            };
        if ctx.cancel.is_cancelled() {
            return Err(command_cancelled_error(None));
        }

        let identity = prepared_identity(&prepared.invocation);
        let PreparedShell {
            identifier: shell_identifier,
            invocation,
            lease,
        } = prepared;
        let outcome = run_prepared(invocation, lease, &ctx.cancel, &mut deadline).await?;
        let duration_ms = started.elapsed().as_millis() as u64;
        match outcome {
            RunOutcome::Done {
                status,
                stdout,
                stderr,
                metadata,
            } => Ok(with_identity(
                format_result(
                    Some(status),
                    &command,
                    shell_identifier,
                    stdout,
                    stderr,
                    duration_ms,
                    false,
                    None,
                ),
                &identity,
                metadata,
            )),
            RunOutcome::CollectFailed { error, teardown } => {
                Err(collection_error(&error, teardown.err()))
            }
            RunOutcome::Timeout {
                stdout,
                stderr,
                teardown,
                started: launched,
                metadata,
            } => Ok(with_identity(
                timed_out_result(
                    &command,
                    shell_identifier,
                    stdout,
                    stderr,
                    duration_ms,
                    timeout,
                    teardown,
                    launched,
                ),
                &identity,
                metadata,
            )),
            RunOutcome::Cancelled { teardown } => Err(command_cancelled_error(teardown.err())),
        }
    }
}

async fn prepare_shell(
    command: &str,
    cwd: &Path,
    lease: ExecutionLease,
    cancel: &tokio_util::sync::CancellationToken,
    deadline: &mut std::pin::Pin<&mut tokio::time::Sleep>,
) -> Result<Option<PreparedShell>, ToolError> {
    #[cfg(windows)]
    {
        prepare_windows_shell(command, cwd, lease, cancel, deadline).await
    }
    #[cfg(not(windows))]
    {
        prepare_posix_shell(command, cwd, lease, cancel, deadline).await
    }
}

/// Adaptively locates a PowerShell interpreter: any installed PowerShell 7
/// (well-known install roots beyond PATH), then the inbox Windows
/// PowerShell 5.1. Returns `None` only when nothing usable exists locally,
/// leaving the pinned network provision as the last resort.
#[cfg(windows)]
fn discover_windows_powershell() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        candidates.push(
            PathBuf::from(&program_files)
                .join("PowerShell")
                .join("7")
                .join("pwsh.exe"),
        );
        // Store (MSIX) installs live in versioned directories such as
        // `Microsoft.PowerShell_7.6.6.0_x64__8wekyb3d8bbwe`; the directory
        // name changes per release, so scan for the prefix. Listing
        // WindowsApps can be ACL-denied; that just skips this source.
        candidates.extend(fuzzy_windows_apps_pwsh(&PathBuf::from(&program_files)));
    }
    if let Some(program_files_x86) = std::env::var_os("ProgramFiles(x86)") {
        candidates.push(
            PathBuf::from(program_files_x86)
                .join("PowerShell")
                .join("7")
                .join("pwsh.exe"),
        );
    }
    if let Some(local_app_data) = std::env::var_os("LocalAppData") {
        candidates.push(
            PathBuf::from(local_app_data)
                .join("Microsoft")
                .join("WindowsApps")
                .join("pwsh.exe"),
        );
    }
    if let Some(user_profile) = std::env::var_os("USERPROFILE") {
        candidates.push(
            PathBuf::from(user_profile)
                .join("scoop")
                .join("shims")
                .join("pwsh.exe"),
        );
    }
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        candidates.push(
            PathBuf::from(system_root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
        );
    }
    // `exists()` (not `is_file()`): the Store execution alias is an
    // appexec reparse point whose metadata is not a regular file.
    candidates.into_iter().find(|path| path.exists())
}

/// Finds `pwsh.exe` inside versioned `Microsoft.PowerShell_*` package
/// directories under a WindowsApps root, newest version first.
#[cfg(windows)]
fn fuzzy_windows_apps_pwsh(windows_apps: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(windows_apps) else {
        return Vec::new();
    };
    let mut packages: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("Microsoft.PowerShell_"))
        })
        .collect();
    // Descending lexical order puts the highest version first for the
    // stable `Major.Minor.Patch.Build` naming scheme.
    packages.sort_unstable_by(|a, b| b.cmp(a));
    packages
        .into_iter()
        .map(|package| package.join("pwsh.exe"))
        .filter(|pwsh| pwsh.exists())
        .collect()
}

#[cfg(windows)]
async fn prepare_windows_shell(
    command: &str,
    cwd: &Path,
    lease: ExecutionLease,
    cancel: &tokio_util::sync::CancellationToken,
    deadline: &mut std::pin::Pin<&mut tokio::time::Sleep>,
) -> Result<Option<PreparedShell>, ToolError> {
    let encoded = encode_powershell_command(
        powershell_script(command),
        Path::new(WINDOWS_SHELL_EXECUTABLE),
    )?;
    let args = powershell_args(encoded);
    let pin_cwd = cwd.to_path_buf();
    let pin_args = args;
    let pin_work = run_blocking_supervised("shell resolution", cancel, move |worker_cancel| {
        let env = snapshot_child_environment()?;
        let prepared = match prepare_from_snapshot(
            &pin_cwd,
            WINDOWS_SHELL_EXECUTABLE,
            &pin_args,
            &env,
            &worker_cancel,
        ) {
            Ok(prepared) => Some(prepared),
            Err(error) => match shell_candidate_action(&error) {
                ShellCandidateAction::TryNext => None,
                ShellCandidateAction::FailClosed => return Err(error.into_tool_error()),
            },
        };
        Ok((prepared, env, lease))
    });
    tokio::pin!(pin_work);
    let first = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(command_cancelled_error(None)),
        _ = deadline.as_mut() => return Ok(None),
        prepared = &mut pin_work => prepared?,
    };
    let (invocation, env, lease) = first;
    match invocation {
        Some(invocation) => Ok(Some(PreparedShell {
            identifier: WINDOWS_SHELL_EXECUTABLE,
            invocation,
            lease,
        })),
        None => {
            // Prefer the inbox Windows PowerShell over a network provision:
            // a large pinned download inside a tool call reads as a hang.
            let managed = match discover_windows_powershell() {
                Some(path) => path,
                None => {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return Err(command_cancelled_error(None)),
                        _ = deadline.as_mut() => return Ok(None),
                        managed = crate::builtin::powershell::ensure_pwsh() => managed?,
                    }
                }
            };
            let encoded = encode_powershell_command(powershell_script(command), &managed)?;
            let args = powershell_args(encoded);
            let managed_program = managed.to_str().ok_or_else(|| {
                ToolError::InvalidArgs(
                    "managed PowerShell path is not valid Unicode and cannot be recorded".into(),
                )
            })?;
            let managed_program = managed_program.to_owned();
            let pin_cwd = cwd.to_path_buf();
            let pin_work =
                run_blocking_supervised("shell resolution", cancel, move |worker_cancel| {
                    let invocation = prepare_from_snapshot(
                        &pin_cwd,
                        &managed_program,
                        &args,
                        &env,
                        &worker_cancel,
                    )
                    .map_err(ResolveError::into_tool_error)?;
                    Ok((invocation, lease))
                });
            tokio::pin!(pin_work);
            let (invocation, lease) = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(command_cancelled_error(None)),
                _ = deadline.as_mut() => return Ok(None),
                prepared = &mut pin_work => prepared?,
            };
            Ok(Some(PreparedShell {
                identifier: WINDOWS_SHELL_EXECUTABLE,
                invocation,
                lease,
            }))
        }
    }
}

#[cfg(not(windows))]
async fn prepare_posix_shell(
    command: &str,
    cwd: &Path,
    lease: ExecutionLease,
    cancel: &tokio_util::sync::CancellationToken,
    deadline: &mut std::pin::Pin<&mut tokio::time::Sleep>,
) -> Result<Option<PreparedShell>, ToolError> {
    let args = vec!["-c".to_owned(), command.to_owned()];
    let pin_cwd = cwd.to_path_buf();
    let pin_work = run_blocking_supervised("shell resolution", cancel, move |worker_cancel| {
        let env = snapshot_child_environment()?;
        let mut last_not_found = None;
        for candidate in SHELL_CANDIDATES {
            match prepare_from_snapshot(&pin_cwd, candidate.executable, &args, &env, &worker_cancel)
            {
                Ok(invocation) => {
                    return Ok(PreparedShell {
                        identifier: candidate.executable,
                        invocation,
                        lease,
                    });
                }
                Err(error) => match shell_candidate_action(&error) {
                    ShellCandidateAction::TryNext => last_not_found = Some(error),
                    ShellCandidateAction::FailClosed => return Err(error.into_tool_error()),
                },
            }
        }
        Err(last_not_found
            .unwrap_or_else(|| ResolveError::NotFound {
                program: "shell".into(),
                searched: Some(0),
            })
            .into_tool_error())
    });
    tokio::pin!(pin_work);
    let prepared = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(command_cancelled_error(None)),
        _ = deadline.as_mut() => return Ok(None),
        prepared = &mut pin_work => prepared?,
    };
    Ok(Some(prepared))
}

#[cfg(windows)]
fn powershell_script(command: &str) -> &str {
    if command.is_empty() {
        // PowerShell 7 rejects an empty -EncodedCommand payload as not Base64.
        "#"
    } else {
        command
    }
}

#[cfg(windows)]
fn powershell_args(encoded_command: String) -> Vec<String> {
    let mut args = Vec::with_capacity(POWERSHELL_ARGUMENTS.len() + 1);
    args.extend(
        POWERSHELL_ARGUMENTS
            .iter()
            .map(|argument| (*argument).to_owned()),
    );
    args.push(encoded_command);
    args
}

fn command_cancelled_error(teardown: Option<std::io::Error>) -> ToolError {
    match teardown {
        Some(err) => ToolError::Execution(format!(
            "command cancelled before completion; termination failed: {err}"
        )),
        None => ToolError::Execution("command cancelled before completion".into()),
    }
}

fn collection_error(collection: &std::io::Error, teardown: Option<std::io::Error>) -> ToolError {
    match teardown {
        Some(err) => ToolError::Execution(format!(
            "failed to collect command output: {collection}; termination failed: {err}"
        )),
        None => ToolError::Execution(format!("failed to collect command output: {collection}")),
    }
}

fn timed_out_before_spawn_result(
    command: &str,
    shell_identifier: &str,
    duration_ms: u64,
    timeout: Duration,
) -> ToolResult {
    let notice = format!(
        "[command timed out after {}s before the shell started]",
        timeout.as_secs()
    );
    mark_timed_out(format_result(
        None,
        command,
        shell_identifier,
        CapturedStream::default(),
        CapturedStream::default(),
        duration_ms,
        true,
        Some(&notice),
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "timeout result assembly mirrors the tool's stable output fields"
)]
fn timed_out_result(
    command: &str,
    shell_identifier: &str,
    stdout: CapturedStream,
    stderr: CapturedStream,
    duration_ms: u64,
    timeout: Duration,
    teardown: Result<(), std::io::Error>,
    started: bool,
) -> ToolResult {
    let notice = match (started, teardown.as_ref().err()) {
        (_, Some(err)) => format!(
            "[command timed out after {}s; termination failed: {err}]",
            timeout.as_secs()
        ),
        (true, None) => format!(
            "[command timed out after {}s and was killed]",
            timeout.as_secs()
        ),
        (false, None) => format!(
            "[command timed out after {}s before the shell started]",
            timeout.as_secs()
        ),
    };
    mark_timed_out(format_result(
        None,
        command,
        shell_identifier,
        stdout,
        stderr,
        duration_ms,
        true,
        Some(&notice),
    ))
}

fn mark_timed_out(mut result: ToolResult) -> ToolResult {
    result.details.as_mut().expect("details were populated")["timed_out"] = json!(true);
    result
}

fn with_identity(
    mut result: ToolResult,
    identity: &PreparedIdentity,
    metadata: ExecutionMetadata,
) -> ToolResult {
    let details = result.details.as_mut().expect("details were populated");
    apply_execution_details(details, identity, metadata);
    result
}

/// Assemble the tool result from collected output.
///
/// `status == None` marks a command that did not finish (timeout path);
/// such results are always `is_error`.
#[expect(
    clippy::too_many_arguments,
    reason = "result assembly mirrors the tool's stable output fields"
)]
fn format_result(
    status: Option<std::process::ExitStatus>,
    command: &str,
    shell: &str,
    stdout: CapturedStream,
    stderr: CapturedStream,
    duration_ms: u64,
    forced_error: bool,
    notice: Option<&str>,
) -> ToolResult {
    let stdout_text = decode_captured_text(&stdout.retained);
    let stderr_text = decode_captured_text(&stderr.retained);

    let mut text = String::new();
    if !stdout_text.trim().is_empty() {
        text.push_str(&stdout_text);
    }
    if !stderr_text.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str("[stderr]\n");
        text.push_str(&stderr_text);
    }

    let (text, truncated) = crate::builtin::truncate_bytes(&text, MAX_OUTPUT_BYTES);
    let mut text = text;
    if truncated {
        let total = stdout.total_bytes.saturating_add(stderr.total_bytes);
        text.push_str(&format!(
            "\n[output truncated: showed first {MAX_OUTPUT_BYTES} of {total} bytes]"
        ));
    }

    let is_error = forced_error || status.is_some_and(|s| !s.success());
    if is_error {
        match &status {
            Some(s) => text.push_str(&format!("\n[exit code: {}]", display_exit(s))),
            None => match notice {
                Some(notice) => {
                    text.push('\n');
                    text.push_str(notice);
                }
                None => text.push_str("\n[no exit status: command did not finish]"),
            },
        }
    } else if let Some(notice) = notice {
        text.push('\n');
        text.push_str(notice);
    }

    let mut details = json!({
        "command": command,
        "shell": shell,
        "stdout_bytes": stdout.total_bytes,
        "stderr_bytes": stderr.total_bytes,
        "duration_ms": duration_ms,
        "truncated": truncated,
    });
    if let Some(s) = status {
        details["exit_code"] = json!(display_exit(&s));
    }

    ToolResult {
        content: vec![mcode_core::message::ContentBlock::Text(text.into())],
        is_error,
        details: Some(details),
    }
}

fn display_exit(status: &std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

/// Encode the user script itself as PowerShell's UTF-16LE Base64 transport.
///
/// No launcher script or .NET decoding API is inserted, so a leading `using`
/// statement remains the first statement and ConstrainedLanguage can execute
/// its permitted cmdlets. The exact `CreateProcessW` budget includes the quoted
/// executable, fixed arguments, encoded payload, spaces, and final UTF-16 NUL.
#[cfg(any(windows, test))]
pub(crate) fn encode_powershell_command(
    command: &str,
    executable: &Path,
) -> Result<String, ToolError> {
    let command_byte_len = command
        .encode_utf16()
        .count()
        .checked_mul(2)
        .ok_or_else(|| command_too_long(executable, None))?;
    let encoded_len =
        base64_encoded_len(command_byte_len).ok_or_else(|| command_too_long(executable, None))?;
    let command_line_units = powershell_command_line_units(executable, encoded_len)
        .ok_or_else(|| command_too_long(executable, Some(encoded_len)))?;
    if command_line_units > WINDOWS_COMMAND_LINE_LIMIT_UTF16_UNITS {
        return Err(command_too_long(executable, Some(encoded_len)));
    }

    Ok(BASE64_STANDARD.encode(utf16le_bytes(command, command_byte_len)))
}

#[cfg(any(windows, test))]
fn powershell_command_line_units(executable: &Path, encoded_len: usize) -> Option<usize> {
    // `std::process::Command` quotes argv[0] on Windows even when it contains no
    // spaces. Structured exec quotes argv0 the same way. Every fixed argument
    // and Base64 character needs no extra quoting; an empty Base64 argument is
    // represented as `""`.
    let mut units = executable_utf16_units(executable).checked_add(2)?;
    for argument in POWERSHELL_ARGUMENTS {
        units = units
            .checked_add(1)?
            .checked_add(argument.encode_utf16().count())?;
    }
    units = units
        .checked_add(1)?
        .checked_add(if encoded_len == 0 { 2 } else { encoded_len })?;
    units.checked_add(1)
}

#[cfg(windows)]
fn executable_utf16_units(executable: &Path) -> usize {
    use std::os::windows::ffi::OsStrExt as _;

    executable.as_os_str().encode_wide().count()
}

#[cfg(all(test, not(windows)))]
fn executable_utf16_units(executable: &Path) -> usize {
    executable
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .count()
}

#[cfg(any(windows, test))]
fn maximum_encoded_command_chars(executable: &Path) -> Option<usize> {
    let one_character_line = powershell_command_line_units(executable, 1)?;
    WINDOWS_COMMAND_LINE_LIMIT_UTF16_UNITS.checked_sub(one_character_line.checked_sub(1)?)
}

#[cfg(any(windows, test))]
fn base64_encoded_len(byte_len: usize) -> Option<usize> {
    byte_len.checked_add(2)?.checked_div(3)?.checked_mul(4)
}

#[cfg(any(windows, test))]
fn utf16le_bytes(value: &str, byte_len: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(byte_len);
    for unit in value.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

#[cfg(any(windows, test))]
fn command_too_long(executable: &Path, encoded_len: Option<usize>) -> ToolError {
    let maximum = maximum_encoded_command_chars(executable)
        .map_or_else(|| "unrepresentable".to_owned(), |value| value.to_string());
    let encoded =
        encoded_len.map_or_else(|| "overflowed usize".to_owned(), |value| value.to_string());
    let executable_name = executable.file_name().map_or_else(
        || std::borrow::Cow::Borrowed("pwsh.exe"),
        |name| name.to_string_lossy(),
    );
    ToolError::InvalidArgs(format!(
        "command is too long for PowerShell 7's 32,767 UTF-16-code-unit CreateProcessW \
         command-line limit (including the terminator): encoded length is {encoded}, maximum \
         for executable {executable_name} is {maximum}"
    ))
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
