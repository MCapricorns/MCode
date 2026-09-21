//! `write` — create or replace a UTF-8 text file through the host file kernel.
//!
//! Missing targets are atomic create-only (missing parents are created
//! safely). Existing targets require `expected_revision` or `overwrite=true`.
//! Hidden files are writable. Unconditional overwrite is not the default.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::builtin::fs_io::{FileAccess, write_file_with_lease};
use crate::builtin::process::acquire_execution_lease;
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

/// The `write` builtin.
pub struct WriteTool;

/// Arguments for [`WriteTool`].
#[derive(Deserialize, JsonSchema)]
pub struct WriteArgs {
    /// Path of the file to write. Relative paths resolve against the
    /// session cwd; missing parent directories are created.
    pub path: String,
    /// The complete new content of the file.
    pub content: String,
    /// Opaque revision from a prior `read`. Required to replace an existing
    /// file unless `overwrite` is true.
    pub expected_revision: Option<String>,
    /// Replace an existing file without a revision check. Default false.
    /// Cannot be combined with `expected_revision`.
    #[serde(default)]
    pub overwrite: bool,
}

impl std::fmt::Debug for WriteArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteArgs")
            .field("path", &self.path)
            .field("content", &"<redacted>")
            .field("expected_revision", &self.expected_revision)
            .field("overwrite", &self.overwrite)
            .finish()
    }
}

#[async_trait]
impl Tool for WriteTool {
    type Args = WriteArgs;
    type Output = ();

    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write a UTF-8 text file inside the session cwd. Missing files are \
         created atomically (parents created as needed). Existing files are \
         replaced only when `expected_revision` matches a prior read, or \
         `overwrite` is true. The two options cannot be combined. Hidden \
         files are writable. Does not follow symlinks or reparse points."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "write: create or replace a file (path, content, optional \
             expected_revision/overwrite).",
        )
    }

    fn mutates_fs(&self) -> bool {
        true
    }

    fn file_access(&self) -> Option<FileAccess> {
        Some(FileAccess::ExistingOrMissing)
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let lease = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => {
                return Err(ToolError::Execution("write cancelled before execution".into()));
            }
            lease = acquire_execution_lease() => lease,
        };
        let outcome = write_file_with_lease(
            ctx.prepared_file.clone(),
            ctx.cwd.clone(),
            args.path,
            args.content,
            args.expected_revision,
            args.overwrite,
            lease,
            ctx.cancel.clone(),
        )
        .await?;
        let mut text = format!(
            "Wrote {} bytes to {}",
            outcome.bytes_written, outcome.path_key
        );
        if outcome.detached_hardlink {
            text.push_str(" (detached_hardlink=true: this directory entry now names a new inode)");
        }
        text.push_str(&format!("\n[revision {}]", outcome.revision));
        Ok(ToolResult::text(text).with_details(json!({
            "path": outcome.path_key,
            "bytes_written": outcome.bytes_written,
            "revision": outcome.revision.as_str(),
            "detached_hardlink": outcome.detached_hardlink,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::test_support::{ctx_at, run_dyn, text_of};
    use crate::tool::{Concurrency, ToolDyn};
    use serde_json::json;

    #[tokio::test]
    async fn writes_file_and_creates_missing_parents() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(
            &WriteTool,
            json!({"path": "deep/nested/new.txt", "content": "hello mycode"}),
            &ctx,
        )
        .await
        .unwrap();
        assert!(!result.is_error);

        let on_disk = std::fs::read_to_string(dir.path().join("deep/nested/new.txt")).unwrap();
        assert_eq!(on_disk, "hello mycode");
        assert!(
            text_of(&result).starts_with("Wrote 11 bytes to "),
            "{}",
            text_of(&result)
        );
        let details = result.details.unwrap();
        assert_eq!(details["bytes_written"], "hello mycode".len());
        assert_eq!(details["detached_hardlink"], false);
        assert!(
            details["revision"]
                .as_str()
                .unwrap()
                .starts_with("mycode-rev1-")
        );
    }

    #[tokio::test]
    async fn existing_file_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "previous content").unwrap();
        let ctx = ctx_at(dir.path());

        let err = run_dyn(
            &WriteTool,
            json!({"path": "old.txt", "content": "new"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("old.txt")).unwrap(),
            "previous content"
        );
    }

    #[tokio::test]
    async fn overwrite_true_replaces_existing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "previous content").unwrap();
        let ctx = ctx_at(dir.path());

        run_dyn(
            &WriteTool,
            json!({"path": "old.txt", "content": "new", "overwrite": true}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("old.txt")).unwrap(),
            "new"
        );
    }

    #[tokio::test]
    async fn expected_revision_and_overwrite_together_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path());
        let err = run_dyn(
            &WriteTool,
            json!({
                "path": "old.txt",
                "content": "y",
                "expected_revision": "mycode-rev1-dead",
                "overwrite": true
            }),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    }

    #[tokio::test]
    async fn stale_expected_revision_does_not_mutate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path());
        let err = run_dyn(
            &WriteTool,
            json!({
                "path": "old.txt",
                "content": "y",
                "expected_revision": "mycode-rev1-not-a-real-revision"
            }),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("old.txt")).unwrap(),
            "x"
        );
    }

    #[tokio::test]
    async fn matching_expected_revision_replaces() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path());
        let read = crate::builtin::test_support::run_dyn(
            &crate::builtin::ReadTool,
            json!({"path": "old.txt"}),
            &ctx,
        )
        .await
        .unwrap();
        let revision = read.details.unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        run_dyn(
            &WriteTool,
            json!({
                "path": "old.txt",
                "content": "y",
                "expected_revision": revision
            }),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("old.txt")).unwrap(),
            "y"
        );
    }

    #[tokio::test]
    async fn empty_content_writes_zero_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());

        let result = run_dyn(
            &WriteTool,
            json!({"path": "empty.txt", "content": ""}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(result.details.unwrap()["bytes_written"], 0);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("empty.txt")).unwrap(),
            ""
        );
    }

    #[tokio::test]
    async fn parent_is_a_regular_file_fails() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blocker"), "file").unwrap();
        let ctx = ctx_at(dir.path());

        let err = run_dyn(
            &WriteTool,
            json!({"path": "blocker/sub/x.txt", "content": "x"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, ToolError::Execution(_) | ToolError::InvalidArgs(_)),
            "{err}"
        );
    }

    #[tokio::test]
    async fn hidden_dotfile_is_writable() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());
        run_dyn(
            &WriteTool,
            json!({"path": ".hidden", "content": "secret"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".hidden")).unwrap(),
            "secret"
        );
    }

    #[tokio::test]
    async fn pre_cancel_has_no_side_effects() {
        let dir = tempfile::tempdir().unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let ctx = ctx_at(dir.path()).with_cancel(token);
        let err = run_dyn(
            &WriteTool,
            json!({"path": "never.txt", "content": "x"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)), "{err}");
        assert!(!dir.path().join("never.txt").exists());
        let leftover: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(leftover.is_empty(), "{leftover:?}");
    }

    #[tokio::test]
    async fn concurrent_same_revision_exactly_one_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("race.txt"), "start").unwrap();
        let ctx = ctx_at(dir.path());
        let read = crate::builtin::test_support::run_dyn(
            &crate::builtin::ReadTool,
            json!({"path": "race.txt"}),
            &ctx,
        )
        .await
        .unwrap();
        let revision = read.details.unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let a = run_dyn(
            &WriteTool,
            json!({
                "path": "race.txt",
                "content": "alpha",
                "expected_revision": revision.clone()
            }),
            &ctx,
        );
        let b = run_dyn(
            &WriteTool,
            json!({
                "path": "race.txt",
                "content": "beta",
                "expected_revision": revision
            }),
            &ctx,
        );
        let (ra, rb) = tokio::join!(a, b);
        let wins = [&ra, &rb].iter().filter(|r| r.is_ok()).count();
        assert_eq!(wins, 1, "a={ra:?} b={rb:?}");
        let disk = std::fs::read_to_string(dir.path().join("race.txt")).unwrap();
        assert!(disk == "alpha" || disk == "beta", "{disk}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_hardlink_detach_replaces_directory_entry() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("shared.txt");
        std::fs::write(&original, "shared").unwrap();
        std::fs::create_dir(dir.path().join("linkdir")).unwrap();
        std::fs::hard_link(&original, dir.path().join("linkdir").join("alias.txt")).unwrap();
        let ctx = ctx_at(dir.path());
        let result = run_dyn(
            &WriteTool,
            json!({"path": "linkdir/alias.txt", "content": "detached", "overwrite": true}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(result.details.unwrap()["detached_hardlink"], true);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("linkdir").join("alias.txt")).unwrap(),
            "detached"
        );
        assert_eq!(std::fs::read_to_string(&original).unwrap(), "shared");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hardlink_detach_replaces_directory_entry() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("shared.txt");
        std::fs::write(&original, "shared").unwrap();
        std::fs::create_dir(dir.path().join("linkdir")).unwrap();
        std::fs::hard_link(&original, dir.path().join("linkdir").join("alias.txt")).unwrap();
        let ctx = ctx_at(dir.path());
        let result = run_dyn(
            &WriteTool,
            json!({"path": "linkdir/alias.txt", "content": "detached", "overwrite": true}),
            &ctx,
        )
        .await
        .unwrap();
        assert!(text_of(&result).contains("detached_hardlink=true"));
        assert_eq!(result.details.unwrap()["detached_hardlink"], true);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("linkdir").join("alias.txt")).unwrap(),
            "detached"
        );
        assert_eq!(std::fs::read_to_string(&original).unwrap(), "shared");
    }

    #[tokio::test]
    async fn schema_rejects_missing_content() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());
        let err = run_dyn(&WriteTool, json!({"path": "x.txt"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
        assert!(!dir.path().join("x.txt").exists());
    }

    #[tokio::test]
    async fn content_over_cap_is_rejected_without_side_effects() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());
        let oversized = "x".repeat(crate::builtin::fs_io::MAX_WRITE_BYTES + 1);
        let err = run_dyn(
            &WriteTool,
            json!({"path": "big.txt", "content": oversized}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
        assert!(!dir.path().join("big.txt").exists());
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    fn temp_leftovers(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("mycode-write-"))
            .collect()
    }

    fn serialize_pre_publish_tests() -> std::sync::MutexGuard<'static, ()> {
        crate::builtin::fs_io::serialize_pre_publish_tests()
    }

    /// The process-global temp-link hook cannot be shared by concurrent tests.
    #[cfg(windows)]
    fn serialize_temp_link_tests() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(unix)]
    fn crate_relative_test_name(full_name: &'static str) -> &'static str {
        full_name
            .strip_prefix(env!("CARGO_CRATE_NAME"))
            .and_then(|name| name.strip_prefix("::"))
            .expect("module_path must start with the crate name")
    }

    /// Child-process marker for the observer test body (umask isolation).
    #[cfg(unix)]
    const OBSERVER_PROBE_ENV: &str = "MYCODE_TOOLS_OBSERVER_PROBE";

    /// Payload bytes used by the Unix leak observer and its negative control.
    #[cfg(unix)]
    const PAYLOAD_LEAK_MARKER: &str = "MYCODE-PAYLOAD-LEAK-MARKER";

    /// Test-only override of the payload temp privacy mode; see
    /// `fs_io::unix::create_temp`. Set to `0640` it simulates a regression
    /// to a group-readable payload temp.
    #[cfg(unix)]
    const TEMP_PRIVATE_MODE_ENV: &str = "MYCODE_TOOLS_TEST_TEMP_PRIVATE_MODE";

    #[cfg(unix)]
    fn assert_one_child_test_ran(output: &std::process::Output, context: &str) {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{context} failed\nstdout: {stdout}\nstderr: {stderr}"
        );
        assert!(
            stdout.contains("running 1 test") && stdout.contains("1 passed; 0 failed"),
            "{context} did not execute exactly one test\nstdout: {stdout}\nstderr: {stderr}"
        );
    }

    /// Permission bits as platform `mode_t` (`u16` on Darwin, `u32` on Linux).
    ///
    /// Checked through `u64` so Linux Clippy does not see a same-type
    /// `try_from` and Darwin cannot silently truncate.
    #[cfg(unix)]
    fn unix_mode_t(bits: u32) -> libc::mode_t {
        libc::mode_t::try_from(u64::from(bits)).expect("Unix permission bits fit mode_t")
    }

    /// Restores the previous process umask when dropped.
    #[cfg(unix)]
    #[must_use = "the umask is restored when this guard is dropped"]
    struct UmaskRestore {
        previous: libc::mode_t,
    }

    #[cfg(unix)]
    impl UmaskRestore {
        fn apply(bits: u32) -> Self {
            let mask = unix_mode_t(bits);
            // SAFETY: `umask(2)` only alters the calling process's mask.
            // Drop restores `previous`.
            let previous = unsafe { libc::umask(mask) };
            Self { previous }
        }
    }

    #[cfg(unix)]
    impl Drop for UmaskRestore {
        fn drop(&mut self) {
            // SAFETY: restores the mask captured by `apply`.
            unsafe { libc::umask(self.previous) };
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_mode_t_is_exact_for_permission_bits() {
        for bits in [0o0002u32, 0o0022, 0o0077] {
            assert_eq!(u64::from(unix_mode_t(bits)), u64::from(bits));
        }
    }

    #[tokio::test]
    async fn stale_revision_failure_cleans_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "x").unwrap();
        let ctx = ctx_at(dir.path());

        let err = run_dyn(
            &WriteTool,
            json!({
                "path": "old.txt",
                "content": "y",
                "expected_revision": "mycode-rev1-stale"
            }),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("old.txt")).unwrap(),
            "x"
        );
        assert!(temp_leftovers(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn create_only_race_file_appeared_is_refused() {
        use crate::builtin::fs_io::{FileAccess, prepare_file};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let prepared = prepare_file(
            dir.path(),
            "spawn.txt",
            &tokio_util::sync::CancellationToken::new(),
            FileAccess::ExistingOrMissing,
        )
        .unwrap();
        // Another writer creates the target between prepare and publish.
        std::fs::write(dir.path().join("spawn.txt"), "won the race").unwrap();
        let ctx = ctx_at(dir.path()).with_prepared_file(Arc::new(prepared));

        let err = run_dyn(
            &WriteTool,
            json!({"path": "spawn.txt", "content": "lost"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("appeared after create-only"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("spawn.txt")).unwrap(),
            "won the race"
        );
        assert!(temp_leftovers(dir.path()).is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn new_file_and_dir_modes_honor_umask() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        // umask is process-global while tests run concurrently in one
        // process, so each probe runs in an isolated child that executes
        // only this test with `MYCODE_TOOLS_UMASK_PROBE` set.
        const PROBE_ENV: &str = "MYCODE_TOOLS_UMASK_PROBE";
        let test_name = crate_relative_test_name(concat!(
            module_path!(),
            "::new_file_and_dir_modes_honor_umask"
        ));

        let Some(probe) = std::env::var(PROBE_ENV).ok() else {
            for (umask, _file_mode, _dir_mode) in
                [(0o0077u32, 0o600u32, 0o700u32), (0o0002, 0o664, 0o775)]
            {
                let output = Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", test_name, "--test-threads=1"])
                    .env(PROBE_ENV, format!("{umask:o}"))
                    .output()
                    .unwrap();
                assert_one_child_test_ran(&output, &format!("umask {umask:o} probe"));
            }
            return;
        };

        let umask = u32::from_str_radix(&probe, 8).expect("octal umask probe");
        let _umask = UmaskRestore::apply(umask);
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());
        run_dyn(
            &WriteTool,
            json!({"path": "sub/new.txt", "content": "umask probe"}),
            &ctx,
        )
        .await
        .unwrap();
        let file_mode = std::fs::metadata(dir.path().join("sub/new.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let dir_mode = std::fs::metadata(dir.path().join("sub"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let (want_file, want_dir) = if umask == 0o0077 {
            (0o600, 0o700)
        } else {
            (0o664, 0o775)
        };
        assert_eq!(file_mode, want_file, "file mode under umask {umask:o}");
        assert_eq!(dir_mode, want_dir, "directory mode under umask {umask:o}");
    }

    /// A foreign observer (a user limited to files with any group- or
    /// other-read bit) holds every handle it can legitimately open while a
    /// write runs and the publish is forced to fail afterwards. The only
    /// foreign-readable inode is the never-written `0666` mode probe, so
    /// every retained handle stays empty; the payload inode is private
    /// from creation.
    #[cfg(unix)]
    #[tokio::test]
    async fn probe_inode_never_exposes_payload_on_failed_publish() {
        use std::io::{Read, Seek, SeekFrom};
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        use std::process::Command;
        use std::sync::{Arc, Mutex};

        use crate::builtin::fs_io::{FileAccess, install_temp_links_hook, prepare_file};

        // umask is process-global while tests run concurrently in one
        // process, so the body runs in an isolated child with umask `022`,
        // guaranteeing the `0666` probe is other-readable (`0644`) and the
        // observer provably holds it.
        let test_name = crate_relative_test_name(concat!(
            module_path!(),
            "::probe_inode_never_exposes_payload_on_failed_publish"
        ));

        if std::env::var_os(OBSERVER_PROBE_ENV).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test_name, "--test-threads=1"])
                .env(OBSERVER_PROBE_ENV, "1")
                .output()
                .unwrap();
            assert_one_child_test_ran(&output, "observer probe child");
            return;
        }

        let _umask = UmaskRestore::apply(0o022);

        let dir = tempfile::tempdir().unwrap();
        let prepared = prepare_file(
            dir.path(),
            "leak-target.txt",
            &tokio_util::sync::CancellationToken::new(),
            FileAccess::ExistingOrMissing,
        )
        .unwrap();
        // A FIFO at the destination makes the create-only publish fail
        // with EEXIST after the payload temp exists (the probe open rejects
        // FIFOs, but the rename still sees the existing name), which forces
        // the publish failure deterministically instead of by racing.
        let fifo_path = dir.path().join("leak-target.txt");
        let c_fifo = std::ffi::CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c_fifo` is a live NUL-terminated path; `mkfifo` only
        // creates the named FIFO.
        let rc = unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o644) };
        assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());

        let payload = format!("{PAYLOAD_LEAK_MARKER}\n").repeat(65536);

        let held = Arc::new(Mutex::new(Vec::<std::fs::File>::new()));
        let observed = held.clone();
        let observe_dir = dir.path().to_path_buf();
        // `write_missing` invokes this hook synchronously after both names
        // are linked and before the first payload byte is written. It is a
        // deterministic barrier: every temp inode visible to a foreign
        // reader is opened and retained before the write can continue.
        let _hook = install_temp_links_hook(Arc::new(move || {
            let mut opened = Vec::new();
            let mut temps = 0usize;
            for entry in std::fs::read_dir(&observe_dir).unwrap().flatten() {
                let name = entry.file_name();
                if !name.to_string_lossy().starts_with("mycode-write-") {
                    continue;
                }
                let Ok(meta) = std::fs::metadata(entry.path()) else {
                    continue;
                };
                if !meta.is_file() {
                    // Never open a non-regular inode.
                    continue;
                }
                temps += 1;
                if meta.permissions().mode() & 0o044 == 0 {
                    // No group or other read bit: invisible to a foreign
                    // user no matter which of the two bits a policy grants.
                    continue;
                }
                if let Ok(mut file) = std::fs::File::open(entry.path()) {
                    let mut text = String::new();
                    file.read_to_string(&mut text).unwrap();
                    assert_eq!(text, "", "visible pre-write inode must be empty");
                    opened.push(file);
                }
            }
            assert_eq!(
                temps, 2,
                "the barrier must see the payload temp and the mode probe"
            );
            observed.lock().unwrap().extend(opened);
        }));

        let ctx = ctx_at(dir.path()).with_prepared_file(Arc::new(prepared));
        let err = run_dyn(
            &WriteTool,
            json!({"path": "leak-target.txt", "content": payload}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)), "{err}");
        assert!(err.to_string().contains("failed to create"), "{err}");

        let mut held = held.lock().unwrap();
        assert!(
            !held.is_empty(),
            "the other-readable mode probe must be observable before payload write"
        );
        for file in held.iter_mut() {
            let mut text = String::new();
            file.seek(SeekFrom::Start(0)).unwrap();
            file.read_to_string(&mut text).unwrap();
            assert_eq!(
                text, "",
                "retained foreign-readable handle must never expose payload"
            );
        }
        assert_eq!(
            held.len(),
            1,
            "only the never-written mode probe may be foreign-readable"
        );
        assert!(temp_leftovers(dir.path()).is_empty());
        assert!(
            std::fs::metadata(&fifo_path).unwrap().file_type().is_fifo(),
            "the FIFO destination must be untouched"
        );
    }

    /// Proves the observer test above actually catches the regression it
    /// guards against: rerunning it with the payload temp forced to a
    /// group-readable `0640` must fail through the leak assertion, not
    /// pass because the observer only looked at the other-read bit.
    #[cfg(unix)]
    #[tokio::test]
    async fn observer_catches_group_readable_payload_temp() {
        use std::process::Command;

        let test_name = crate_relative_test_name(concat!(
            module_path!(),
            "::probe_inode_never_exposes_payload_on_failed_publish"
        ));
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--test-threads=1"])
            .env(OBSERVER_PROBE_ENV, "1")
            .env(TEMP_PRIVATE_MODE_ENV, "640")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{stdout}{stderr}");
        assert!(
            !output.status.success(),
            "a group-readable payload temp must fail the observer test\
stdout: {stdout}\
stderr: {stderr}"
        );
        assert!(
            combined.contains("running 1 test")
                && combined.contains("0 passed; 1 failed")
                && combined.contains(test_name)
                && combined.contains("expose payload")
                && combined.contains(PAYLOAD_LEAK_MARKER),
            "the failure must be the leak assertion from exactly one child test, got\
stdout: {stdout}\
stderr: {stderr}"
        );
    }

    /// A mandatory cleanup that fails after the publish must be reported as
    /// a failure (never a silent success with residue), and the destination
    /// that was already published keeps the written content. The unlink
    /// fault is keyed to this test's directory, so concurrently running
    /// write tests in other directories are unaffected.
    #[cfg(unix)]
    #[tokio::test]
    async fn post_publish_cleanup_failure_is_reported_not_swallowed() {
        use crate::builtin::fs_io::install_unlink_fault_under;

        let dir = tempfile::tempdir().unwrap();
        let fault = install_unlink_fault_under(dir.path(), None)
            .expect("unlink fault fixture must install");
        let ctx = ctx_at(dir.path());
        let err = run_dyn(
            &WriteTool,
            json!({"path": "residue-target.txt", "content": "published payload"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)), "{err}");
        assert!(
            err.to_string().contains("injected mycode unlink failure"),
            "the error must include the cleanup failure: {err}"
        );
        // The rename/link publish itself completed before the cleanup
        // failure, so the destination holds the written content.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("residue-target.txt")).unwrap(),
            "published payload"
        );
        let residue = temp_leftovers(dir.path());
        assert!(
            !residue.is_empty(),
            "the faulted cleanup left documented residue"
        );
        drop(fault);
        for name in &residue {
            std::fs::remove_file(dir.path().join(name)).unwrap();
        }
        assert!(temp_leftovers(dir.path()).is_empty());
    }

    /// Cancelling at the deterministic pre-publish barrier must block both
    /// publish flavors, leave the original target untouched, and clean
    /// every temp name. The hook filters on the path key so concurrently
    /// running writes are untouched.
    #[tokio::test]
    #[expect(
        clippy::await_holding_lock,
        reason = "process-global pre-publish hook is not async-aware; this test must not overlap other writes"
    )]
    async fn pre_publish_cancel_blocks_publish_and_cleans_temps() {
        let _serialize = serialize_pre_publish_tests();
        use crate::builtin::fs_io::install_pre_publish_hook;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        let dir = tempfile::tempdir().unwrap();
        const NEW_KEY: &str = "pre-publish-cancel-new.txt";
        const OLD_KEY: &str = "pre-publish-cancel-old.txt";
        std::fs::write(dir.path().join(OLD_KEY), "original").unwrap();

        // Create-only publish never happens.
        {
            let token = CancellationToken::new();
            let cancel = token.clone();
            let _hook = install_pre_publish_hook(Arc::new(move |key| {
                if key == NEW_KEY {
                    cancel.cancel();
                }
            }));
            let ctx = ctx_at(dir.path()).with_cancel(token);
            let err = run_dyn(
                &WriteTool,
                json!({"path": NEW_KEY, "content": "late"}),
                &ctx,
            )
            .await
            .unwrap_err();
            assert!(matches!(err, ToolError::Execution(_)), "{err}");
            assert!(
                !dir.path().join(NEW_KEY).exists(),
                "a cancelled write must not publish"
            );
            assert!(temp_leftovers(dir.path()).is_empty());
        }

        // Replace publish never happens; the original is byte-identical.
        {
            let token = CancellationToken::new();
            let cancel = token.clone();
            let _hook = install_pre_publish_hook(Arc::new(move |key| {
                if key == OLD_KEY {
                    cancel.cancel();
                }
            }));
            let ctx = ctx_at(dir.path()).with_cancel(token);
            let err = run_dyn(
                &WriteTool,
                json!({"path": OLD_KEY, "content": "replacement", "overwrite": true}),
                &ctx,
            )
            .await
            .unwrap_err();
            assert!(matches!(err, ToolError::Execution(_)), "{err}");
            assert_eq!(
                std::fs::read_to_string(dir.path().join(OLD_KEY)).unwrap(),
                "original"
            );
            assert!(temp_leftovers(dir.path()).is_empty());
        }
    }

    /// Replacing the just-published name with same-length foreign content
    /// — in place (same inode, content differs) or via rename-aside plus a
    /// fresh file (new inode, identity differs) — must fail verification
    /// instead of minting a revision for content this write never wrote.
    #[tokio::test]
    async fn post_publish_replacement_fails_verification() {
        use crate::builtin::fs_io::install_post_publish_hook;
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();

        // Same inode, same length, different content.
        #[cfg(unix)]
        {
            const KEY: &str = "race-inplace.txt";
            let hook_dir = dir.path().to_path_buf();
            let _hook = install_post_publish_hook(Arc::new(move |key| {
                if key == KEY {
                    std::fs::write(hook_dir.join(KEY), "BBBB").unwrap();
                }
            }));
            let ctx = ctx_at(dir.path());
            let err = run_dyn(&WriteTool, json!({"path": KEY, "content": "AAAA"}), &ctx)
                .await
                .unwrap_err();
            assert!(err.to_string().contains("content does not match"), "{err}");
            assert_eq!(
                std::fs::read_to_string(dir.path().join(KEY)).unwrap(),
                "BBBB",
                "the foreign replacement stays; the tool must not rewrite it"
            );
            assert!(temp_leftovers(dir.path()).is_empty());
        }

        // New inode with the same length (rename-aside works on Windows
        // too: the retained temp handle grants FILE_SHARE_DELETE).
        {
            const KEY: &str = "race-replaced.txt";
            let hook_dir = dir.path().to_path_buf();
            let _hook = install_post_publish_hook(Arc::new(move |key| {
                if key == KEY {
                    let aside = hook_dir.join("race-aside.tmp");
                    std::fs::rename(hook_dir.join(KEY), &aside).unwrap();
                    std::fs::write(hook_dir.join(KEY), "CCCC").unwrap();
                    std::fs::remove_file(&aside).unwrap();
                }
            }));
            let ctx = ctx_at(dir.path());
            let err = run_dyn(&WriteTool, json!({"path": KEY, "content": "AAAA"}), &ctx)
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains("replaced before verification"),
                "{err}"
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join(KEY)).unwrap(),
                "CCCC",
                "the foreign replacement stays; the tool must not rewrite it"
            );
            assert!(temp_leftovers(dir.path()).is_empty());
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn restricted_dacl_failed_publish_leaves_no_temp() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{
            CloseHandle, ERROR_SUCCESS, GENERIC_READ, INVALID_HANDLE_VALUE, LocalFree,
        };
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SE_FILE_OBJECT,
            SetNamedSecurityInfoW,
        };
        use windows_sys::Win32::Security::{
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        };

        fn wide(path: &std::path::Path) -> Vec<u16> {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        }

        /// Replaces `path`'s DACL with a protected one parsed from `sddl`.
        /// The owner keeps the implicit right to run this again.
        fn apply_dacl(path: &std::path::Path, sddl: &str) {
            let text: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
            let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            // SAFETY: `text` is a live NUL-terminated UTF-16 string; on
            // success `sd` is an allocation that `LocalFree` releases.
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    1, // SDDL_REVISION_1
                    &mut sd,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(ok, 0, "SDDL parse failed");
            let mut present = 0;
            let mut defaulted = 0;
            let mut dacl = std::ptr::null_mut();
            // SAFETY: `sd` is a valid descriptor from the call above.
            let ok =
                unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) };
            assert_ne!(ok, 0, "GetSecurityDescriptorDacl failed");
            assert_ne!(present, 0, "descriptor must carry a DACL");
            let name = wide(path);
            // SAFETY: `name` is live NUL-terminated; `dacl` aliases the
            // live `sd` allocation during the call.
            let status = unsafe {
                SetNamedSecurityInfoW(
                    name.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    dacl,
                    std::ptr::null(),
                )
            };
            // SAFETY: release the descriptor allocation after the apply.
            unsafe { LocalFree(sd.cast()) };
            assert_eq!(status, ERROR_SUCCESS, "SetNamedSecurityInfo failed");
        }

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("locked.txt");
        std::fs::write(&target, "v1").unwrap();
        // Set the read-only bit while the DACL is still the inherited
        // full-access one: it is copied onto the temp together with the
        // DACL, so cleanup must also defeat FILE_ATTRIBUTE_READONLY, not
        // just the DACL.
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&target, perms).unwrap();
        // Everyone may read (prepare and identity checks keep working) but
        // write/delete are denied, so a by-name cleanup open of a temp that
        // inherited this DACL would be refused.
        apply_dacl(&target, "D:PAI(A;;FR;;;WD)");

        // Force the publish (rename-over) to fail: hold the target open
        // without FILE_SHARE_DELETE for the whole write attempt.
        let name = wide(&target);
        // SAFETY: `name` is a live NUL-terminated UTF-16 path; the returned
        // handle is owned and closed below.
        let blocker = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(blocker, INVALID_HANDLE_VALUE, "blocker open failed");

        let ctx = ctx_at(dir.path());
        let err = run_dyn(
            &WriteTool,
            json!({"path": "locked.txt", "content": "v2", "overwrite": true}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)), "{err}");
        assert!(
            err.to_string().contains("failed to publish"),
            "restrictive DACL fixture must fail at publish, not earlier: {err}"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1");
        // Cleanup must run on the retained creation-time DELETE handle
        // (never a fresh by-name open the copied DACL would deny) and must
        // ignore the copied read-only attribute instead of failing with
        // STATUS_CANNOT_DELETE and stranding the temp.
        assert!(
            temp_leftovers(dir.path()).is_empty(),
            "temp files left behind: {:?}",
            temp_leftovers(dir.path())
        );

        // SAFETY: release the blocker handle opened above.
        unsafe { CloseHandle(blocker) };
        // Restore full control, then clear the read-only bit (needs write
        // access), so the tempdir cleanup can delete the file.
        apply_dacl(&target, "D:PAI(A;;FA;;;WD)");
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "Windows-only test cleanup: this only clears the read-only bit"
        )]
        perms.set_readonly(false);
        std::fs::set_permissions(&target, perms).unwrap();
    }

    #[tokio::test]
    async fn capability_is_consumed_once() {
        use crate::builtin::fs_io::{FileAccess, prepare_file};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let prepared = prepare_file(
            dir.path(),
            "once.txt",
            &tokio_util::sync::CancellationToken::new(),
            FileAccess::ExistingOrMissing,
        )
        .unwrap();
        let ctx = ctx_at(dir.path()).with_prepared_file(Arc::new(prepared));

        run_dyn(
            &WriteTool,
            json!({"path": "once.txt", "content": "first"}),
            &ctx,
        )
        .await
        .unwrap();
        let err = run_dyn(
            &WriteTool,
            json!({"path": "once.txt", "content": "second", "overwrite": true}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("already consumed"), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("once.txt")).unwrap(),
            "first"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn ads_write_target_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());
        let err = run_dyn(
            &WriteTool,
            json!({"path": "plain.txt:ads", "content": "x"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
        assert!(!dir.path().join("plain.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn intermediate_symlink_directory_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink("real", dir.path().join("alias")).unwrap();
        let ctx = ctx_at(dir.path());

        let err = run_dyn(
            &WriteTool,
            json!({"path": "alias/new.txt", "content": "x"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
        assert!(!real.join("new.txt").exists());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn junction_component_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        junction::create(&real, dir.path().join("jd")).unwrap();
        let ctx = ctx_at(dir.path());

        let err = run_dyn(
            &WriteTool,
            json!({"path": "jd/new.txt", "content": "x"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
        assert!(!real.join("new.txt").exists());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn final_symlink_target_is_rejected_when_creation_is_permitted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.txt"), "x").unwrap();
        // Creating a symbolic link needs Developer Mode or
        // SeCreateSymbolicLink privilege. Skip honestly when the fixture
        // cannot be created; reparse rejection is covered by the junction
        // tests.
        if std::os::windows::fs::symlink_file("real.txt", dir.path().join("link.txt")).is_err() {
            eprintln!("skipped: symbolic link creation is not permitted on this host");
            return;
        }
        let ctx = ctx_at(dir.path());

        let err = run_dyn(
            &WriteTool,
            json!({"path": "link.txt", "content": "y"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("real.txt")).unwrap(),
            "x"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_mode_is_preserved_on_overwrite() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mode.txt");
        std::fs::write(&target, "v1").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o601)).unwrap();
        let ctx = ctx_at(dir.path());

        run_dyn(
            &WriteTool,
            json!({"path": "mode.txt", "content": "v2", "overwrite": true}),
            &ctx,
        )
        .await
        .unwrap();
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o601, "mode bits must survive the rewrite");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn protected_dacl_is_preserved_on_overwrite() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
        use windows_sys::Win32::Security::Authorization::{
            GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
        };
        use windows_sys::Win32::Security::{
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
        };

        fn wide(path: &std::path::Path) -> Vec<u16> {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        }

        /// Returns the DACL-protection flag of `path`'s security descriptor.
        fn dacl_protected(path: &std::path::Path) -> bool {
            let name = wide(path);
            let mut owner = std::ptr::null_mut();
            let mut group = std::ptr::null_mut();
            let mut dacl = std::ptr::null_mut();
            let mut sacl = std::ptr::null_mut();
            let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            // SAFETY: `name` is a live NUL-terminated UTF-16 path; all output
            // pointers are writable. `sd` aliases every other output pointer,
            // so freeing only `sd` releases the whole allocation.
            let status = unsafe {
                GetNamedSecurityInfoW(
                    name.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    &mut owner,
                    &mut group,
                    &mut dacl,
                    &mut sacl,
                    &mut sd,
                )
            };
            assert_eq!(status, ERROR_SUCCESS, "GetNamedSecurityInfo failed");
            let mut control = 0u16;
            let mut revision = 0u32;
            // SAFETY: `sd` is live from GetNamedSecurityInfo.
            let ok = unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) };
            assert_ne!(ok, 0, "GetSecurityDescriptorControl failed");
            // SAFETY: `sd` is the allocation root returned above.
            unsafe { LocalFree(sd.cast()) };
            control & SE_DACL_PROTECTED != 0
        }

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("dacl.txt");
        std::fs::write(&target, "v1").unwrap();
        assert!(!dacl_protected(&target), "fresh file DACL must inherit");

        // Mark the DACL protected. The protection flag is the observable
        // marker: a temp file created fresh would always inherit.
        let name = wide(&target);
        let mut owner = std::ptr::null_mut();
        let mut group = std::ptr::null_mut();
        let mut dacl = std::ptr::null_mut();
        let mut sacl = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: as in `dacl_protected`; the returned DACL is applied back
        // while `sd` is still alive.
        let status = unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                &mut owner,
                &mut group,
                &mut dacl,
                &mut sacl,
                &mut sd,
            )
        };
        assert_eq!(status, ERROR_SUCCESS, "GetNamedSecurityInfo failed");
        // SAFETY: `dacl` aliases the live `sd` allocation and stays alive
        // through the call.
        let status = unsafe {
            SetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        // SAFETY: release the descriptor allocation after the apply.
        unsafe { LocalFree(sd.cast()) };
        assert_eq!(status, ERROR_SUCCESS, "SetNamedSecurityInfo failed");
        assert!(dacl_protected(&target));

        let ctx = ctx_at(dir.path());
        run_dyn(
            &WriteTool,
            json!({"path": "dacl.txt", "content": "v2", "overwrite": true}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "v2");
        assert!(
            dacl_protected(&target),
            "published file must keep the protected DACL of the original"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn unprotected_inherited_dacl_is_not_frozen() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
        use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
        use windows_sys::Win32::Security::{
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, PSECURITY_DESCRIPTOR,
            SE_DACL_PROTECTED,
        };

        fn wide(path: &std::path::Path) -> Vec<u16> {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        }

        fn dacl_protected(path: &std::path::Path) -> bool {
            let mut owner = std::ptr::null_mut();
            let mut group = std::ptr::null_mut();
            let mut dacl = std::ptr::null_mut();
            let mut sacl = std::ptr::null_mut();
            let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            let name = wide(path);
            // SAFETY: `name` is live NUL-terminated; outputs are writable.
            let status = unsafe {
                GetNamedSecurityInfoW(
                    name.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    &mut owner,
                    &mut group,
                    &mut dacl,
                    &mut sacl,
                    &mut sd,
                )
            };
            assert_eq!(status, ERROR_SUCCESS, "GetNamedSecurityInfo failed");
            let mut control = 0;
            let mut revision = 0u32;
            // SAFETY: `sd` is a live descriptor from the call above.
            let ok = unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) };
            // SAFETY: release the descriptor after reading control.
            unsafe { LocalFree(sd.cast()) };
            assert_ne!(ok, 0, "GetSecurityDescriptorControl failed");
            control & SE_DACL_PROTECTED != 0
        }

        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("inherit.txt");
        std::fs::write(&existing, "v1").unwrap();
        assert!(!dacl_protected(&existing), "fresh file DACL must inherit");
        let ctx = ctx_at(dir.path());
        run_dyn(
            &WriteTool,
            json!({"path": "inherit.txt", "content": "v2", "overwrite": true}),
            &ctx,
        )
        .await
        .unwrap();
        assert!(
            !dacl_protected(&existing),
            "overwrite must not freeze an inheriting DACL as protected"
        );

        run_dyn(
            &WriteTool,
            json!({"path": "new-inherit.txt", "content": "created"}),
            &ctx,
        )
        .await
        .unwrap();
        assert!(
            !dacl_protected(&dir.path().join("new-inherit.txt")),
            "new file must keep the directory's unprotected inherited DACL"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    #[expect(
        clippy::await_holding_lock,
        reason = "process-global temp-link hook is not async-aware; these tests must not overlap"
    )]
    async fn missing_target_probe_cleanup_survives_restrictive_dacl() {
        let _serialize = serialize_temp_link_tests();
        use std::os::windows::ffi::OsStrExt;
        use std::sync::Arc;
        use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SE_FILE_OBJECT,
            SetNamedSecurityInfoW,
        };
        use windows_sys::Win32::Security::{
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        };

        use crate::builtin::fs_io::install_temp_links_hook;

        fn wide(path: &std::path::Path) -> Vec<u16> {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        }

        fn apply_dacl(path: &std::path::Path, sddl: &str) {
            let text: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
            let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            // SAFETY: `text` is live NUL-terminated UTF-16; `sd` is freed below.
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    1,
                    &mut sd,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(ok, 0, "SDDL parse failed");
            let mut present = 0;
            let mut defaulted = 0;
            let mut dacl = std::ptr::null_mut();
            // SAFETY: `sd` is a valid descriptor from the call above.
            let ok =
                unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) };
            assert_ne!(ok, 0, "GetSecurityDescriptorDacl failed");
            let name = wide(path);
            // SAFETY: `name` and `dacl` stay live for the call.
            let status = unsafe {
                SetNamedSecurityInfoW(
                    name.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    dacl,
                    std::ptr::null(),
                )
            };
            // SAFETY: release the descriptor after apply.
            unsafe { LocalFree(sd.cast()) };
            assert_eq!(status, ERROR_SUCCESS, "SetNamedSecurityInfo failed");
        }

        let dir = tempfile::tempdir().unwrap();
        let observe_dir = dir.path().to_path_buf();
        let _hook = install_temp_links_hook(Arc::new(move || {
            for entry in std::fs::read_dir(&observe_dir).unwrap().flatten() {
                let name = entry.file_name();
                if !name.to_string_lossy().starts_with("mycode-write-") {
                    continue;
                }
                // Payload temps deny by-name opens. The inherited probe is
                // still named and can receive a restrictive DACL.
                if std::fs::OpenOptions::new()
                    .read(true)
                    .open(entry.path())
                    .is_ok()
                {
                    apply_dacl(&entry.path(), "D:PAI(A;;FR;;;WD)");
                }
            }
        }));
        let ctx = ctx_at(dir.path());
        run_dyn(
            &WriteTool,
            json!({"path": "probed.txt", "content": "ok"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("probed.txt")).unwrap(),
            "ok"
        );
        assert!(
            temp_leftovers(dir.path()).is_empty(),
            "restrictive probe DACL must not leave named residue: {:?}",
            temp_leftovers(dir.path())
        );
    }

    #[test]
    fn write_args_debug_redacts_content() {
        const SECRET: &str = "MYCODE-SECRET-SENTINEL-9f3a";
        let args = WriteArgs {
            path: "secret.txt".to_owned(),
            content: SECRET.to_owned(),
            expected_revision: None,
            overwrite: false,
        };
        let rendered = format!("{args:?}");
        assert!(rendered.contains("WriteArgs"), "{rendered}");
        assert!(!rendered.contains(SECRET), "{rendered}");
    }

    #[cfg(windows)]
    #[tokio::test]
    #[expect(
        clippy::await_holding_lock,
        reason = "process-global temp-link hook is not async-aware; these tests must not overlap"
    )]
    async fn permissive_parent_cannot_read_payload_temp() {
        let _serialize = serialize_temp_link_tests();
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::FromRawHandle;
        use std::sync::Arc;
        use windows_sys::Win32::Foundation::{
            CloseHandle, ERROR_SUCCESS, GENERIC_READ, INVALID_HANDLE_VALUE, LocalFree,
        };
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SE_FILE_OBJECT,
            SetNamedSecurityInfoW,
        };
        use windows_sys::Win32::Security::{
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, OPEN_EXISTING,
        };

        use crate::builtin::fs_io::install_temp_links_hook;

        fn wide(path: &std::path::Path) -> Vec<u16> {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        }

        fn apply_dacl(path: &std::path::Path, sddl: &str) {
            let text: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
            let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            // SAFETY: `text` is live NUL-terminated UTF-16; `sd` is freed below.
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    1,
                    &mut sd,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(ok, 0, "SDDL parse failed");
            let mut present = 0;
            let mut defaulted = 0;
            let mut dacl = std::ptr::null_mut();
            // SAFETY: `sd` is a valid descriptor from the call above.
            let ok =
                unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) };
            assert_ne!(ok, 0, "GetSecurityDescriptorDacl failed");
            let name = wide(path);
            // SAFETY: `name` and `dacl` stay live for the call.
            let status = unsafe {
                SetNamedSecurityInfoW(
                    name.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    dacl,
                    std::ptr::null(),
                )
            };
            // SAFETY: release the descriptor after apply.
            unsafe { LocalFree(sd.cast()) };
            assert_eq!(status, ERROR_SUCCESS, "SetNamedSecurityInfo failed");
        }

        let dir = tempfile::tempdir().unwrap();
        apply_dacl(dir.path(), "D:PAI(A;;FA;;;WD)");
        let marker = "MYCODE-PAYLOAD-LEAK-MARKER";
        let ctx = ctx_at(dir.path());

        {
            let observe_dir = dir.path().to_path_buf();
            let denied = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let denied_hook = denied.clone();
            let _hook = install_temp_links_hook(Arc::new(move || {
                let mut denied_opens = 0usize;
                let mut seen = 0usize;
                for entry in std::fs::read_dir(&observe_dir).unwrap().flatten() {
                    let name = entry.file_name();
                    if !name.to_string_lossy().starts_with("mycode-write-") {
                        continue;
                    }
                    seen += 1;
                    let path = wide(&entry.path());
                    // SAFETY: `path` is a live NUL-terminated UTF-16 name.
                    let handle = unsafe {
                        CreateFileW(
                            path.as_ptr(),
                            GENERIC_READ,
                            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                            std::ptr::null(),
                            OPEN_EXISTING,
                            FILE_ATTRIBUTE_NORMAL,
                            std::ptr::null_mut(),
                        )
                    };
                    if handle == INVALID_HANDLE_VALUE {
                        denied_opens += 1;
                        continue;
                    }
                    // SAFETY: `CreateFileW` returned an owned handle.
                    let mut file = unsafe { std::fs::File::from_raw_handle(handle) };
                    let mut text = String::new();
                    use std::io::Read;
                    file.read_to_string(&mut text).unwrap();
                    assert_eq!(text, "", "readable temp must be the never-written probe");
                    assert!(!text.contains(marker));
                }
                if seen > 0 {
                    denied_hook.store(denied_opens, std::sync::atomic::Ordering::SeqCst);
                }
            }));
            run_dyn(
                &WriteTool,
                json!({"path": "new.txt", "content": format!("{marker}\n").repeat(1024)}),
                &ctx,
            )
            .await
            .unwrap();
            assert!(
                denied.load(std::sync::atomic::Ordering::SeqCst) >= 1,
                "payload temp must deny foreign read opens"
            );
        }

        std::fs::write(dir.path().join("old.txt"), "old").unwrap();
        {
            let denied = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let denied_hook = denied.clone();
            let observe_dir = dir.path().to_path_buf();
            let _hook = install_temp_links_hook(Arc::new(move || {
                let mut denied_opens = 0usize;
                let mut seen = 0usize;
                for entry in std::fs::read_dir(&observe_dir).unwrap().flatten() {
                    let name = entry.file_name();
                    if !name.to_string_lossy().starts_with("mycode-write-") {
                        continue;
                    }
                    seen += 1;
                    let path = wide(&entry.path());
                    // SAFETY: `path` is a live NUL-terminated UTF-16 name.
                    let handle = unsafe {
                        CreateFileW(
                            path.as_ptr(),
                            GENERIC_READ,
                            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                            std::ptr::null(),
                            OPEN_EXISTING,
                            FILE_ATTRIBUTE_NORMAL,
                            std::ptr::null_mut(),
                        )
                    };
                    if handle == INVALID_HANDLE_VALUE {
                        denied_opens += 1;
                    } else {
                        // SAFETY: `CreateFileW` returned an owned handle.
                        unsafe { CloseHandle(handle) };
                    }
                }
                if seen > 0 {
                    denied_hook.store(denied_opens, std::sync::atomic::Ordering::SeqCst);
                }
            }));
            run_dyn(
                &WriteTool,
                json!({"path": "old.txt", "content": "new", "overwrite": true}),
                &ctx,
            )
            .await
            .unwrap();
            assert!(
                denied.load(std::sync::atomic::Ordering::SeqCst) >= 1,
                "existing-target payload temp must deny foreign read opens"
            );
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn failed_publish_reports_cleanup_failure() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        };

        use crate::builtin::fs_io::install_delete_fault_under;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("locked.txt");
        std::fs::write(&target, "v1").unwrap();
        let fault = install_delete_fault_under(dir.path()).expect("delete fault must install");
        let mut wide: Vec<u16> = target
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide` is a live NUL-terminated UTF-16 path; the handle is
        // closed below.
        let blocker = unsafe {
            CreateFileW(
                wide.as_mut_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(blocker, INVALID_HANDLE_VALUE, "blocker open failed");
        let ctx = ctx_at(dir.path());
        let err = run_dyn(
            &WriteTool,
            json!({"path": "locked.txt", "content": "v2", "overwrite": true}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("failed to publish"),
            "must fail at publish: {err}"
        );
        assert!(
            err.to_string().contains("injected mycode delete failure"),
            "cleanup failure must enter the returned error: {err}"
        );
        assert!(
            !temp_leftovers(dir.path()).is_empty(),
            "faulted cleanup must leave documented residue"
        );
        // SAFETY: release the blocker opened above.
        unsafe { CloseHandle(blocker) };
        drop(fault);
        for name in temp_leftovers(dir.path()) {
            std::fs::remove_file(dir.path().join(name)).ok();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_publish_reports_cleanup_failure() {
        use std::os::unix::ffi::OsStrExt;
        use std::sync::Arc;

        use crate::builtin::fs_io::{FileAccess, install_unlink_fault_under, prepare_file};

        let dir = tempfile::tempdir().unwrap();
        let prepared = prepare_file(
            dir.path(),
            "blocked.txt",
            &tokio_util::sync::CancellationToken::new(),
            FileAccess::ExistingOrMissing,
        )
        .unwrap();
        let fifo = dir.path().join("blocked.txt");
        let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c_fifo` is a live NUL-terminated path; `mkfifo` only
        // creates the named FIFO after create-only prepare.
        let rc = unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o644) };
        assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
        let fault =
            install_unlink_fault_under(dir.path(), None).expect("unlink fault must install");
        let ctx = ctx_at(dir.path()).with_prepared_file(Arc::new(prepared));
        let err = run_dyn(
            &WriteTool,
            json!({"path": "blocked.txt", "content": "late"}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("failed to create"), "{err}");
        assert!(
            err.to_string().contains("injected mycode unlink failure"),
            "cleanup failure must enter the returned error: {err}"
        );
        assert!(
            !temp_leftovers(dir.path()).is_empty(),
            "faulted cleanup must leave documented residue"
        );
        drop(fault);
        for name in temp_leftovers(dir.path()) {
            std::fs::remove_file(dir.path().join(name)).ok();
        }
    }

    #[test]
    fn capability_markers() {
        let tool: &dyn ToolDyn = &WriteTool;
        assert!(tool.mutates_fs());
        assert_eq!(tool.concurrency(), Concurrency::Parallel);
        assert!(tool.requires_file_preflight());
        assert!(!tool.requires_search_preflight());
    }
}
