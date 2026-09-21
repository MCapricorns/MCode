use super::*;
use crate::builtin::test_support::{ctx_at, run_dyn, text_of};
use serde_json::json;

#[tokio::test]
async fn writes_file_and_creates_missing_parents() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    const CONTENT: &str = "hello mycode";
    let result = run_dyn(
        &WriteTool,
        json!({"path": "deep/nested/new.txt", "content": CONTENT}),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!result.is_error);

    let on_disk = std::fs::read_to_string(dir.path().join("deep/nested/new.txt")).unwrap();
    assert_eq!(on_disk, CONTENT);
    let text = text_of(&result);
    assert!(
        text.starts_with(&format!("Wrote {} bytes to ", CONTENT.len())),
        "{text}"
    );
    let details = result.details.unwrap();
    assert_eq!(details["bytes_written"], CONTENT.len());
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

pub(crate) fn temp_leftovers(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("mycode-write-"))
        .collect()
}

pub(crate) fn serialize_pre_publish_tests() -> std::sync::MutexGuard<'static, ()> {
    crate::builtin::fs_io::serialize_pre_publish_tests()
}

/// The process-global temp-link hook cannot be shared by concurrent tests.
#[cfg(windows)]
pub(crate) fn serialize_temp_link_tests() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(unix)]
pub(crate) fn crate_relative_test_name(full_name: &'static str) -> &'static str {
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
pub(crate) fn assert_one_child_test_ran(output: &std::process::Output, context: &str) {
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
pub(crate) fn unix_mode_t(bits: u32) -> libc::mode_t {
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
    let fault =
        install_unlink_fault_under(dir.path(), None).expect("unlink fault fixture must install");
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
