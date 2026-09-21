//! `read` — read a UTF-8 text file with optional line windowing and
//! output truncation (pi-style: ~2000 lines / 50 KiB with a notice).
//!
//! Execution uses the host file kernel: a prepared handle-relative capability,
//! chunked read under scan/line/deadline/cancel caps, UTF-8 (BOM stripped
//! from displayed text; hash of raw bytes), and an opaque revision token.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::builtin::fs_io::{FileAccess, read_file_async};
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

/// The `read` builtin.
pub struct ReadTool;

/// Arguments for [`ReadTool`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadArgs {
    /// Path of the file to read. Relative paths resolve against the
    /// session cwd.
    pub path: String,
    /// 1-based line number to start reading from (default: the first
    /// line).
    pub offset: Option<usize>,
    /// Maximum number of lines to return (default: all, up to the
    /// tool's output cap).
    pub limit: Option<usize>,
}

#[async_trait]
impl Tool for ReadTool {
    type Args = ReadArgs;
    type Output = ();

    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a UTF-8 text file from the local filesystem. Use offset/limit to \
         window into large files; output is truncated with a notice beyond \
         2000 lines or 50 KiB per call. Hidden files are readable. Returns an \
         opaque revision token for later writes."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some("read: fetch file contents (path, optional 1-based offset / limit).")
    }

    fn file_access(&self) -> Option<FileAccess> {
        Some(FileAccess::ExistingContent)
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let outcome = read_file_async(
            ctx.prepared_file.clone(),
            ctx.cwd.clone(),
            args.path,
            args.offset,
            args.limit,
            ctx.cancel.clone(),
        )
        .await?;
        Ok(ToolResult::text(outcome.displayed).with_details(json!({
            "path": outcome.path_key,
            "total_lines": outcome.total_lines,
            "returned_lines": outcome.returned_lines,
            "truncated": outcome.truncated,
            "revision": outcome.revision.as_str(),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::fs_io::{MAX_BYTES, MAX_LINES};
    use crate::builtin::test_support::{ctx_at, run_dyn, text_of};
    use serde_json::json;

    fn body_of(result: &ToolResult) -> &str {
        let text = text_of(result);
        text.rsplit_once("\n[revision ")
            .map(|(body, _)| body)
            .or_else(|| text.strip_prefix("[revision ").map(|_| ""))
            .unwrap_or(text)
    }

    fn revision_of(result: &ToolResult) -> String {
        result.details.as_ref().unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[tokio::test]
    async fn reads_small_file_completely() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&ReadTool, json!({"path": "notes.txt"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(body_of(&result), "alpha\nbeta\ngamma\n".trim_end());
        let details = result.details.as_ref().unwrap();
        assert_eq!(details["total_lines"], 3);
        assert!(
            details["revision"]
                .as_str()
                .unwrap()
                .starts_with("mycode-rev1-")
        );
        assert!(text_of(&result).contains("[revision mycode-rev1-"));
    }

    #[tokio::test]
    async fn missing_file_is_an_execution_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());

        let err = run_dyn(&ReadTool, json!({"path": "nope.txt"}), &ctx)
            .await
            .unwrap_err();
        match err {
            ToolError::Execution(msg) => {
                assert!(msg.contains("nope.txt"), "{msg}");
            }
            other => panic!("expected Execution, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_utf8_file_is_an_execution_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blob.bin"), b"\xff\xfe\x00binary").unwrap();
        let ctx = ctx_at(dir.path());

        let err = run_dyn(&ReadTool, json!({"path": "blob.bin"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)));
    }

    #[tokio::test]
    async fn utf16_bom_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("u16.txt"), b"\xff\xfeh\x00i\x00").unwrap();
        let ctx = ctx_at(dir.path());
        let err = run_dyn(&ReadTool, json!({"path": "u16.txt"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("UTF-16"), "{err}");
    }

    #[tokio::test]
    async fn utf8_bom_is_stripped_from_display_not_hash() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = b"\xef\xbb\xbfhello".to_vec();
        std::fs::write(dir.path().join("bom.txt"), &bytes).unwrap();
        let ctx = ctx_at(dir.path());
        let with_bom = run_dyn(&ReadTool, json!({"path": "bom.txt"}), &ctx)
            .await
            .unwrap();
        assert_eq!(body_of(&with_bom), "hello");
        bytes = b"hello".to_vec();
        std::fs::write(dir.path().join("plain.txt"), &bytes).unwrap();
        let plain = run_dyn(&ReadTool, json!({"path": "plain.txt"}), &ctx)
            .await
            .unwrap();
        assert_ne!(revision_of(&with_bom), revision_of(&plain));
    }

    #[tokio::test]
    async fn offset_and_limit_window_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let body: Vec<String> = (1..=100).map(|i| format!("line-{i}")).collect();
        std::fs::write(dir.path().join("num.txt"), body.join("\n")).unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(
            &ReadTool,
            json!({"path": "num.txt", "offset": 10, "limit": 3}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(body_of(&result), "line-10\nline-11\nline-12");
        assert_eq!(result.details.unwrap()["truncated"], false);
    }

    #[tokio::test]
    async fn usize_max_limit_does_not_overflow_or_bypass_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let body: Vec<String> = (1..=2500).map(|i| format!("{i}")).collect();
        std::fs::write(dir.path().join("num.txt"), body.join("\n")).unwrap();
        let ctx = ctx_at(dir.path());

        // `offset=2, limit=usize::MAX` used to overflow `start + limit` in
        // debug builds and wrap in release, underflowing `capped_end - start`
        // and bypassing the line cap. It must saturate instead.
        let result = run_dyn(
            &ReadTool,
            json!({"path": "num.txt", "offset": 2, "limit": usize::MAX}),
            &ctx,
        )
        .await
        .unwrap();
        assert!(!result.is_error);
        let text = body_of(&result);
        let details = result.details.as_ref().unwrap();
        assert_eq!(details["total_lines"], 2500);
        assert_eq!(details["returned_lines"], MAX_LINES);
        assert_eq!(details["truncated"], true);
        assert_eq!(text.lines().next(), Some("2"));
        assert!(
            text.contains("[output truncated: showing lines 2-2001 of 2500"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn offset_beyond_eof_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tiny.txt"), "one line\n").unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&ReadTool, json!({"path": "tiny.txt", "offset": 999}), &ctx)
            .await
            .unwrap();
        assert_eq!(body_of(&result), "");
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn line_cap_truncates_with_notice() {
        let dir = tempfile::tempdir().unwrap();
        let body: Vec<String> = (1..=2500).map(|i| format!("{i}")).collect();
        std::fs::write(dir.path().join("big.txt"), body.join("\n")).unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&ReadTool, json!({"path": "big.txt"}), &ctx)
            .await
            .unwrap();
        let text = body_of(&result);
        let returned = text.lines().count();
        assert_eq!(returned, MAX_LINES + 1, "content + notice line");
        assert!(
            text.contains("[output truncated: showing lines 1-2000 of 2500"),
            "{text}"
        );

        let details = result.details.unwrap();
        assert_eq!(details["truncated"], true);
        assert_eq!(details["total_lines"], 2500);
        assert_eq!(details["returned_lines"], MAX_LINES);

        let result = run_dyn(
            &ReadTool,
            json!({"path": "big.txt", "offset": 2001, "limit": 2}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(body_of(&result), "2001\n2002");
    }

    #[tokio::test]
    async fn byte_cap_truncates_single_huge_line() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("wide.txt"), "x".repeat(60 * 1024)).unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&ReadTool, json!({"path": "wide.txt"}), &ctx)
            .await
            .unwrap();
        let text = body_of(&result);
        assert!(text.len() > MAX_BYTES, "notice is appended");
        assert!(text.starts_with(&"x".repeat(MAX_BYTES)));
        assert!(text.contains("[output truncated"));
        assert_eq!(result.details.unwrap()["truncated"], true);
    }

    #[tokio::test]
    async fn empty_file_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("empty.txt"), "").unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(&ReadTool, json!({"path": "empty.txt"}), &ctx)
            .await
            .unwrap();
        assert_eq!(body_of(&result), "");
        assert_eq!(result.details.unwrap()["total_lines"], 0);
    }

    #[tokio::test]
    async fn hidden_dotfile_is_readable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".secret"), "hidden-ok").unwrap();
        let ctx = ctx_at(dir.path());
        let result = run_dyn(&ReadTool, json!({"path": ".secret"}), &ctx)
            .await
            .unwrap();
        assert_eq!(body_of(&result), "hidden-ok");
    }

    #[tokio::test]
    async fn absolute_path_inside_cwd_works() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("in.txt"), "inside").unwrap();
        let ctx = ctx_at(dir.path());
        let abs = dir.path().join("in.txt");
        let result = run_dyn(&ReadTool, json!({"path": abs.to_str().unwrap()}), &ctx)
            .await
            .unwrap();
        assert_eq!(body_of(&result), "inside");
    }

    #[tokio::test]
    async fn path_outside_cwd_and_dotdot_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("x.txt"), "nope").unwrap();
        let ctx = ctx_at(dir.path());
        let abs = outside.path().join("x.txt");
        let err = run_dyn(&ReadTool, json!({"path": abs.to_str().unwrap()}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
        let err = run_dyn(&ReadTool, json!({"path": "../x.txt"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    }

    #[tokio::test]
    async fn unicode_filename_is_readable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("café.txt"), "ok").unwrap();
        let ctx = ctx_at(dir.path());
        let result = run_dyn(&ReadTool, json!({"path": "café.txt"}), &ctx)
            .await
            .unwrap();
        assert_eq!(body_of(&result), "ok");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_and_fifo_are_rejected() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::FileTypeExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.txt"), "x").unwrap();
        std::os::unix::fs::symlink("real.txt", dir.path().join("link.txt")).unwrap();
        let fifo = dir.path().join("pipe");
        let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c_fifo` is a live NUL-terminated path; `mkfifo` only
        // creates the named FIFO.
        let rc = unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o644) };
        assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
        assert!(
            std::fs::metadata(&fifo).unwrap().file_type().is_fifo(),
            "fixture must be a FIFO"
        );
        let ctx = ctx_at(dir.path());
        let err = run_dyn(&ReadTool, json!({"path": "link.txt"}), &ctx)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArgs(_)) || err.to_string().contains("symlink"),
            "{err}"
        );
        let err = run_dyn(&ReadTool, json!({"path": "pipe"}), &ctx)
            .await
            .unwrap_err();
        assert!(
            err.to_string().to_ascii_lowercase().contains("fifo")
                || matches!(err, ToolError::InvalidArgs(_)),
            "{err}"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn ads_component_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plain.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path());
        let err = run_dyn(&ReadTool, json!({"path": "plain.txt:stream"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    }

    #[tokio::test]
    async fn revision_changes_when_content_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("v.txt"), "one").unwrap();
        let ctx = ctx_at(dir.path());
        let first = run_dyn(&ReadTool, json!({"path": "v.txt"}), &ctx)
            .await
            .unwrap();
        std::fs::write(dir.path().join("v.txt"), "two").unwrap();
        let second = run_dyn(&ReadTool, json!({"path": "v.txt"}), &ctx)
            .await
            .unwrap();
        assert_ne!(revision_of(&first), revision_of(&second));
        assert!(revision_of(&first).starts_with("mycode-rev1-"));
        assert!(!revision_of(&first).contains("one"));
    }

    #[tokio::test]
    async fn prepared_path_replacement_fails_closed() {
        use crate::builtin::fs_io::{FileAccess, prepare_file};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("swap.txt"), "first").unwrap();
        let prepared = prepare_file(
            dir.path(),
            "swap.txt",
            &tokio_util::sync::CancellationToken::new(),
            FileAccess::ExistingContent,
        )
        .unwrap();
        std::fs::remove_file(dir.path().join("swap.txt")).unwrap();
        std::fs::write(dir.path().join("swap.txt"), "second").unwrap();
        let ctx = ctx_at(dir.path()).with_prepared_file(Arc::new(prepared));
        let err = run_dyn(&ReadTool, json!({"path": "swap.txt"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)), "{err}");
    }

    #[tokio::test]
    async fn file_over_scan_cap_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let oversized = "x".repeat(crate::builtin::fs_io::MAX_READ_SCAN_BYTES as usize + 1);
        std::fs::write(dir.path().join("huge.txt"), oversized).unwrap();
        let ctx = ctx_at(dir.path());

        let err = run_dyn(&ReadTool, json!({"path": "huge.txt"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("read size limit"), "{err}");
    }

    #[tokio::test]
    async fn cancel_after_prepare_returns_no_partial() {
        use crate::builtin::fs_io::{FileAccess, prepare_file};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cancel.txt"), "stable").unwrap();
        let prepared = prepare_file(
            dir.path(),
            "cancel.txt",
            &tokio_util::sync::CancellationToken::new(),
            FileAccess::ExistingContent,
        )
        .unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let ctx = ctx_at(dir.path())
            .with_prepared_file(Arc::new(prepared))
            .with_cancel(token);

        let err = run_dyn(&ReadTool, json!({"path": "cancel.txt"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cancelled"), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cancel.txt")).unwrap(),
            "stable"
        );
    }

    #[tokio::test]
    async fn nul_in_path_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());
        let err = run_dyn(&ReadTool, json!({"path": "bad\u{0}name"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn intermediate_symlink_directory_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("real")).unwrap();
        std::fs::write(dir.path().join("real/inner.txt"), "x").unwrap();
        std::os::unix::fs::symlink("real", dir.path().join("alias")).unwrap();
        let ctx = ctx_at(dir.path());

        let err = run_dyn(&ReadTool, json!({"path": "alias/inner.txt"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn junction_component_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("inner.txt"), "x").unwrap();
        junction::create(&real, dir.path().join("jd")).unwrap();
        let ctx = ctx_at(dir.path());

        let err = run_dyn(&ReadTool, json!({"path": "jd/inner.txt"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn final_symlink_is_rejected_when_creation_is_permitted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.txt"), "x").unwrap();
        // Creating a symbolic link needs Developer Mode or SeCreateSymbolicLink
        // privilege. Skip honestly when the fixture cannot be created; the
        // reparse rejection itself is covered by the junction test.
        if std::os::windows::fs::symlink_file("real.txt", dir.path().join("link.txt")).is_err() {
            eprintln!("skipped: symbolic link creation is not permitted on this host");
            return;
        }
        let ctx = ctx_at(dir.path());

        let err = run_dyn(&ReadTool, json!({"path": "link.txt"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    }

    #[test]
    fn read_declares_file_preflight() {
        let tool: &dyn crate::tool::ToolDyn = &ReadTool;
        assert!(tool.requires_file_preflight());
        assert!(!tool.requires_search_preflight());
    }

    #[test]
    fn file_read_debug_redacts_displayed_text() {
        use crate::builtin::fs_io::{FileRead, FileRevision};

        const SECRET: &str = "MYCODE-SECRET-SENTINEL-9f3a";
        let outcome = FileRead {
            displayed: SECRET.to_owned(),
            truncated: false,
            total_lines: 1,
            returned_lines: 1,
            revision: FileRevision::from_debug_token("mycode-rev1-test"),
            path_key: "notes.txt".to_owned(),
        };
        let rendered = format!("{outcome:?}");
        assert!(rendered.contains("FileRead"), "{rendered}");
        assert!(!rendered.contains(SECRET), "{rendered}");
    }
}
