//! Linux x86_64 GNU spawn: `execveat(AT_EMPTY_PATH)` from a retained fd.
//!
//! Product launch is Linux x86_64 GNU only; musl, Android, and BSD stay
//! unsupported. The child is forked with `process_group(0)` and a `pre_exec`
//! hook that fail-closed marks every fd ≥ 3 `FD_CLOEXEC` via raw
//! `close_range`, then launches the already-opened descriptor. There is no
//! verify-then-path reopen and no `execvp`/`ENOEXEC` shell fallback.
//! Re-hashing happens in the parent immediately before `spawn`. A same-uid
//! writer that already holds the vnode can still rewrite bytes in the
//! fork-to-`execveat` window; public APIs cannot close that race without
//! allocating in the child.
#![cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]

use std::ffi::{CString, OsString};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::path::Path;
use std::process::Stdio;
use std::ptr;

use tokio::process::{Child, Command};

use super::resolve::{PinnedImage, verify_pinned_digest};
use super::spawn::{
    SpawnFailure, SpawnGate, finish_pending_spawn_cleanup, wait_tokio_child_blocking,
};
use super::unix::{build_cstring_vec, build_env_cstrings};
use crate::builtin::process::{ExecutionLease, ProcessTree};
use crate::tool::ToolError;

/// First descriptor outside the standard streams and macOS hold-fd range.
const MIN_LAUNCH_FD: libc::c_int = 4;
/// First fd past stdin/stdout/stderr. `close_range` must start here so fd 3
/// (std's exec-error pipe and any other inherited capability) is sealed.
const FIRST_NONSTD_FD: libc::c_uint = 3;

/// Spawns `pinned` with `args` in `cwd` via `execveat` from the retained fd.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] when digest re-check, spawn, or
/// process-group enrollment fails.
pub(super) fn spawn_linux(
    pinned: PinnedImage,
    argv0: &str,
    args: &[String],
    cwd: &Path,
    env: &[(OsString, OsString)],
    lease: ExecutionLease,
    gate: &SpawnGate,
) -> Result<(Child, ProcessTree, PinnedImage, ExecutionLease), SpawnFailure> {
    spawn_linux_with_enroller(
        pinned,
        argv0,
        args,
        cwd,
        env,
        lease,
        gate,
        ProcessTree::enroll_unix,
    )
}

fn spawn_linux_with_enroller<F>(
    mut pinned: PinnedImage,
    argv0: &str,
    args: &[String],
    cwd: &Path,
    env: &[(OsString, OsString)],
    lease: ExecutionLease,
    gate: &SpawnGate,
    enroll: F,
) -> Result<(Child, ProcessTree, PinnedImage, ExecutionLease), SpawnFailure>
where
    F: FnOnce(&Child) -> std::io::Result<ProcessTree>,
{
    verify_pinned_digest(&mut pinned, gate)?;

    let argv = ExecvePointerTable::new(build_cstring_vec(argv0, args)?);
    let env = ExecvePointerTable::new(build_env_cstrings(env)?);
    let launch_fd = duplicate_launch_fd(&pinned.file)?;

    let mut process = Command::new(&pinned.canonical_path);
    process
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .kill_on_drop(true)
        .process_group(0);
    // SAFETY: `pre_exec` is Command's documented child hook. The closure only
    // performs async-signal-safe kernel operations (`close_range` then
    // `execveat`) with pointers into `argv` / `env` that are moved into the
    // closure and remain valid until the call. `launch_fd` is owned by the
    // closure and remains open across fork; `CLOSE_RANGE_CLOEXEC` does not
    // close it until a successful exec, so `execveat(AT_EMPTY_PATH)` still
    // sees the retained image. No allocation, lock, formatting, or
    // environment access happens inside the closure.
    unsafe {
        process.pre_exec(move || {
            mark_nonstandard_fds_cloexec()?;
            execveat_empty_path(&launch_fd, &argv, &env)
        });
    }

    gate.begin_spawn()?;
    let child = process.spawn().map_err(|err| {
        ToolError::Execution(format!(
            "failed to spawn {}: {err}",
            pinned.canonical_path.display()
        ))
    })?;
    gate.mark_launched();

    let mut pending = PendingLinuxSpawn::new(child, pinned, lease);
    match enroll(pending.child()) {
        Ok(process_tree) => pending.set_process_tree(process_tree),
        Err(err) => {
            let error = ToolError::Execution(format!(
                "failed to enroll the process group for {}: {err}",
                pending.canonical_path().display()
            ));
            let teardown = pending.cleanup();
            return Err(SpawnFailure::new(error, teardown));
        }
    }
    Ok(pending.into_parts())
}

/// Owns a launched Linux child, optional process tree, and image pin until
/// group enrollment completes or cleanup reaps the leader.
///
/// Containment (enrollment, then process-group terminate) must succeed before
/// the leader is reaped. A failed attempt keeps the unreaped child, any enrolled
/// tree, and the image pin so Drop can retry against the same identities.
struct PendingLinuxSpawn {
    child: Option<Child>,
    process_tree: Option<ProcessTree>,
    pinned: Option<PinnedImage>,
    lease: Option<ExecutionLease>,
}

impl PendingLinuxSpawn {
    fn new(child: Child, pinned: PinnedImage, lease: ExecutionLease) -> Self {
        Self {
            child: Some(child),
            process_tree: None,
            pinned: Some(pinned),
            lease: Some(lease),
        }
    }

    fn child(&self) -> &Child {
        self.child.as_ref().expect("pending child must be present")
    }

    fn set_process_tree(&mut self, process_tree: ProcessTree) {
        self.process_tree = Some(process_tree);
    }

    fn canonical_path(&self) -> &Path {
        &self
            .pinned
            .as_ref()
            .expect("pending image pin must be present")
            .canonical_path
    }

    fn into_parts(mut self) -> (Child, ProcessTree, PinnedImage, ExecutionLease) {
        let child = self.child.take().expect("pending child must be present");
        let process_tree = self
            .process_tree
            .take()
            .expect("process-group enrollment completed");
        let pinned = self
            .pinned
            .take()
            .expect("pending image pin must be present");
        let lease = self
            .lease
            .take()
            .expect("pending execution lease must be present");
        (child, process_tree, pinned, lease)
    }

    fn cleanup(&mut self) -> std::io::Result<()> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        if self.process_tree.is_none() {
            self.process_tree = Some(ProcessTree::enroll_unix(child)?);
        }
        #[cfg(test)]
        pending_containment_probe::observe()?;
        self.process_tree
            .as_ref()
            .expect("pending process tree must be present after enrollment")
            .terminate(Some(child))?;
        wait_tokio_child_blocking(child)?;
        drop(self.child.take());
        self.process_tree = None;
        Ok(())
    }
}

impl Drop for PendingLinuxSpawn {
    fn drop(&mut self) {
        finish_pending_spawn_cleanup(|| self.cleanup());
    }
}

fn duplicate_launch_fd(file: &std::fs::File) -> Result<OwnedFd, ToolError> {
    // SAFETY: `file` is live. F_DUPFD_CLOEXEC duplicates it to the first
    // available descriptor at or above `MIN_LAUNCH_FD`.
    let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, MIN_LAUNCH_FD) };
    if fd == -1 {
        return Err(ToolError::Execution(format!(
            "failed to duplicate the pinned executable descriptor: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: fcntl returned a fresh descriptor uniquely owned here.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Replaces the child image with `execveat(AT_EMPTY_PATH)` on `launch_fd`.
///
/// A successful call does not return. The helper performs only the syscall
/// and reads `errno`, so it stays async-signal-safe for `pre_exec`.
fn execveat_empty_path(
    launch_fd: &OwnedFd,
    argv: &ExecvePointerTable,
    env: &ExecvePointerTable,
) -> std::io::Result<()> {
    // SAFETY: `launch_fd` is still open, pathname is empty with
    // AT_EMPTY_PATH, and argv/envp are NUL-terminated `*mut c_char`
    // arrays matching execveat's ABI. The pointed-to CString bytes stay
    // immutable for the life of `argv` / `env`.
    let rc = unsafe {
        libc::execveat(
            launch_fd.as_raw_fd(),
            c"".as_ptr(),
            argv.as_ptr(),
            env.as_ptr(),
            libc::AT_EMPTY_PATH,
        )
    };
    if rc == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Marks every descriptor above stderr `FD_CLOEXEC` via raw `close_range`.
///
/// `CLOSE_RANGE_CLOEXEC` does not close the descriptors, so std's exec-error
/// pipe remains writable if `execveat` fails. The launch fd is included and
/// stays usable until a successful exec. `ENOSYS`, `EINVAL`, and any other
/// kernel error fail-close the spawn; there is no inheritance fallback.
fn mark_nonstandard_fds_cloexec() -> std::io::Result<()> {
    // SAFETY: `SYS_close_range` with `CLOSE_RANGE_CLOEXEC` is an
    // async-signal-safe kernel operation. It does not allocate, take locks,
    // inspect the environment, or close descriptors. `first` is 3 and
    // `last` is `c_uint::MAX`, so `first <= last`.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            FIRST_NONSTD_FD,
            libc::c_uint::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if rc == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Owns immutable C strings and their NUL-terminated pointer table.
///
/// libc 0.2.189 `execveat` takes `argv`/`envp` as `*const *mut c_char`.
/// The table stores those ABI pointers without offering a Rust-side write
/// path: elements are derived from `CString::as_ptr()` and only the kernel
/// observes them. The trailing null pointer is retained.
struct ExecvePointerTable {
    pointers: Vec<*mut libc::c_char>,
    _entries: Vec<CString>,
}

impl ExecvePointerTable {
    fn new(entries: Vec<CString>) -> Self {
        let mut pointers: Vec<*mut libc::c_char> = entries
            .iter()
            .map(|entry| entry.as_ptr().cast_mut())
            .collect();
        pointers.push(ptr::null_mut());
        Self {
            pointers,
            _entries: entries,
        }
    }

    fn as_ptr(&self) -> *const *mut libc::c_char {
        self.pointers.as_ptr()
    }
}

// SAFETY: every pointer targets CString heap storage owned by `_entries`.
// Moving the table cannot relocate those allocations. The `*mut` element
// type matches execveat's argv/envp ABI (`char *const []`); this table never
// writes through the pointers, and no safe method returns a mutable view.
unsafe impl Send for ExecvePointerTable {}
// SAFETY: after construction the CString bytes and pointer vector are
// immutable. Concurrent reads of those bytes are sound.
unsafe impl Sync for ExecvePointerTable {}

#[cfg(test)]
mod pending_containment_probe {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex, OnceLock};

    pub(super) struct ProbeGuard {
        probe: Arc<Probe>,
        _serialize: tokio::sync::MutexGuard<'static, ()>,
    }

    struct Probe {
        remaining_failures: AtomicUsize,
        attempts: AtomicUsize,
        failed: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: Mutex<Option<mpsc::Receiver<()>>>,
    }

    fn serialize_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    fn slot() -> &'static Mutex<Option<Arc<Probe>>> {
        static SLOT: OnceLock<Mutex<Option<Arc<Probe>>>> = OnceLock::new();
        SLOT.get_or_init(|| Mutex::new(None))
    }

    impl Drop for ProbeGuard {
        fn drop(&mut self) {
            *slot()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
    }

    impl ProbeGuard {
        pub(super) fn attempts(&self) -> usize {
            self.probe.attempts.load(Ordering::Acquire)
        }
    }

    pub(super) async fn serialize() -> tokio::sync::MutexGuard<'static, ()> {
        serialize_lock().lock().await
    }

    pub(super) async fn install_first_failure() -> (
        ProbeGuard,
        tokio::sync::oneshot::Receiver<()>,
        mpsc::Sender<()>,
    ) {
        let serialize = serialize().await;
        let (failed_tx, failed_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let probe = Arc::new(Probe {
            remaining_failures: AtomicUsize::new(1),
            attempts: AtomicUsize::new(0),
            failed: Mutex::new(Some(failed_tx)),
            release: Mutex::new(Some(release_rx)),
        });
        *slot()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&probe));
        (
            ProbeGuard {
                probe,
                _serialize: serialize,
            },
            failed_rx,
            release_tx,
        )
    }

    fn wait_for_release(probe: &Probe) {
        if let Some(release) = probe
            .release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = release.recv();
        }
    }

    pub(super) fn observe() -> std::io::Result<()> {
        let probe = slot()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(probe) = probe else {
            return Ok(());
        };
        probe.attempts.fetch_add(1, Ordering::AcqRel);
        match probe.remaining_failures.try_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |remaining| remaining.checked_sub(1),
        ) {
            Ok(_) => {
                if let Some(failed) = probe
                    .failed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    let _ = failed.send(());
                }
                wait_for_release(&probe);
                Err(std::io::Error::other("injected containment failure"))
            }
            Err(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn spawn_after_close_range_with_error(errno: i32) -> std::io::Error {
        let mut process = tokio::process::Command::new(std::env::current_exe().unwrap());
        process
            .args(["--exact", "close_range_injection_must_not_exec"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // SAFETY: the closure only calls the async-signal-safe close_range
        // helper and then returns a captured errno. The errno is chosen in
        // the parent, so the child never reads shared test state.
        unsafe {
            process.pre_exec(move || {
                mark_nonstandard_fds_cloexec()?;
                Err(std::io::Error::from_raw_os_error(errno))
            });
        }
        let started = Instant::now();
        let err = process
            .spawn()
            .expect_err("close_range failure must fail spawn");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "Command hung after close_range; elapsed {:?}",
            started.elapsed()
        );
        err
    }

    #[tokio::test]
    async fn injected_close_range_enosys_fails_spawn_without_hanging() {
        let err = spawn_after_close_range_with_error(libc::ENOSYS);
        assert_eq!(err.raw_os_error(), Some(libc::ENOSYS), "{err}");
    }

    #[tokio::test]
    async fn injected_close_range_einval_fails_spawn_without_hanging() {
        let err = spawn_after_close_range_with_error(libc::EINVAL);
        assert_eq!(err.raw_os_error(), Some(libc::EINVAL), "{err}");
    }

    fn linux_proc_state(pid: libc::pid_t) -> char {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("proc stat");
        let after_comm = text.rsplit_once(')').expect("comm").1;
        after_comm
            .trim_start()
            .chars()
            .next()
            .expect("process state")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn enrollment_failure_reaps_before_releasing_pin_and_lease() {
        let _serialize = super::pending_containment_probe::serialize().await;
        let sleep = Path::new("/bin/sleep");
        if !sleep.is_file() {
            eprintln!("skipping: /bin/sleep is not present");
            return;
        }

        let directory = tempfile::tempdir().expect("tempdir");
        let cancel = tokio_util::sync::CancellationToken::new();
        let pinned = super::super::resolve::pin_program(
            directory.path(),
            sleep.to_str().expect("ASCII path"),
            &["30".to_owned()],
            &cancel,
        )
        .expect("pin sleep");
        let pinned_fd = std::os::fd::AsRawFd::as_raw_fd(&pinned.file);
        let argv0 = pinned
            .canonical_path
            .to_str()
            .expect("pinned path is Unicode")
            .to_owned();
        let lease = crate::builtin::process::acquire_execution_lease().await;
        let gate = SpawnGate::new();
        let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let cwd = directory.path().to_path_buf();
        let env = super::super::env::snapshot_child_environment().expect("env");
        let runner = tokio::task::spawn_blocking(move || {
            spawn_linux_with_enroller(
                pinned,
                &argv0,
                &["30".to_owned()],
                &cwd,
                &env,
                lease,
                &gate,
                move |child| {
                    let _ = pid_tx.send(child.id().expect("launched child has a pid"));
                    let _ = release_rx.recv();
                    Err(std::io::Error::other("injected enrollment failure"))
                },
            )
        });

        let pid = pid_rx.await.expect("enrollment hook ran");
        assert!(
            Path::new(&format!("/proc/self/fd/{pinned_fd}")).exists(),
            "pinned executable fd was released before enrollment completed"
        );
        let contender = tokio::spawn(crate::builtin::process::acquire_execution_lease());
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            !contender.is_finished(),
            "execution lease was released before enrollment cleanup"
        );

        release_tx.send(()).expect("release enrollment hook");
        let failure = match runner.await.expect("spawn worker joined") {
            Ok(_) => panic!("injected enrollment failure unexpectedly spawned"),
            Err(failure) => failure,
        };
        assert!(failure.teardown.is_ok(), "{:?}", failure.teardown);
        let pid = libc::pid_t::try_from(pid).expect("child pid fits pid_t");
        // SAFETY: signal 0 only checks whether the reaped numeric pid exists.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        drop(
            tokio::time::timeout(Duration::from_secs(1), contender)
                .await
                .expect("reaped child retained the execution lease")
                .expect("lease contender joined"),
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pending_cleanup_retains_unreaped_leader_until_containment_succeeds() {
        let sleep = Path::new("/bin/sleep");
        if !sleep.is_file() {
            eprintln!("skipping: /bin/sleep is not present");
            return;
        }

        let (probe, failed_rx, release_tx) =
            super::pending_containment_probe::install_first_failure().await;
        let directory = tempfile::tempdir().expect("tempdir");
        let cancel = tokio_util::sync::CancellationToken::new();
        let pinned = super::super::resolve::pin_program(
            directory.path(),
            sleep.to_str().expect("ASCII path"),
            &["30".to_owned()],
            &cancel,
        )
        .expect("pin sleep");
        let pinned_fd = std::os::fd::AsRawFd::as_raw_fd(&pinned.file);
        let argv0 = pinned
            .canonical_path
            .to_str()
            .expect("pinned path is Unicode")
            .to_owned();
        let lease = crate::builtin::process::acquire_execution_lease().await;
        let gate = SpawnGate::new();
        let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
        let cwd = directory.path().to_path_buf();
        let env = super::super::env::snapshot_child_environment().expect("env");
        let runner = tokio::task::spawn_blocking(move || {
            spawn_linux_with_enroller(
                pinned,
                &argv0,
                &["30".to_owned()],
                &cwd,
                &env,
                lease,
                &gate,
                move |child| {
                    let _ = pid_tx.send(child.id().expect("launched child has a pid"));
                    Err(std::io::Error::other("injected enrollment failure"))
                },
            )
        });

        failed_rx.await.expect("first containment attempt failed");
        let pid = pid_rx.await.expect("enrollment hook ran");
        let pid = libc::pid_t::try_from(pid).expect("child pid fits pid_t");
        assert_eq!(
            probe.attempts(),
            1,
            "first containment failure must be a single attempt"
        );
        assert!(
            Path::new(&format!("/proc/self/fd/{pinned_fd}")).exists(),
            "pinned executable fd was released before containment succeeded"
        );
        // SAFETY: signal 0 only checks whether the numeric pid still exists.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            0,
            "first containment failure reaped or reused the leader pid"
        );
        let state = linux_proc_state(pid);
        assert_ne!(
            state, 'Z',
            "first containment failure signaled the leader into a zombie"
        );
        let contender = tokio::spawn(crate::builtin::process::acquire_execution_lease());
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            !contender.is_finished(),
            "execution lease was released before containment succeeded"
        );

        release_tx.send(()).expect("release containment retry");
        let failure = match runner.await.expect("spawn worker joined") {
            Ok(_) => panic!("injected enrollment failure unexpectedly spawned"),
            Err(failure) => failure,
        };
        assert!(
            failure.teardown.is_err(),
            "first containment attempt must be reported as teardown failure"
        );
        assert_eq!(probe.attempts(), 2);
        // SAFETY: signal 0 only checks whether the reaped numeric pid exists.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        drop(
            tokio::time::timeout(Duration::from_secs(1), contender)
                .await
                .expect("pending owner retained the execution lease until reap")
                .expect("lease contender joined"),
        );
    }
}
