//! Deterministic PID-authority tests for macOS suspended verification.
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};

use super::{SpawnedPid, StopWait, wait_until_stopped};
use crate::builtin::process::ProcessTree;
use crate::tool::ToolError;

struct PidHooks {
    pid: libc::pid_t,
    waits: Mutex<VecDeque<io::Result<(libc::pid_t, libc::c_int)>>>,
    wait_calls: AtomicU32,
    kill_calls: AtomicU32,
    cleanup_calls: AtomicU32,
}

struct PidHookGuard {
    _serialize: std::sync::MutexGuard<'static, ()>,
    hooks: Arc<PidHooks>,
}

impl Drop for PidHookGuard {
    fn drop(&mut self) {
        *hook_slot()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

impl PidHookGuard {
    fn wait_calls(&self) -> u32 {
        self.hooks.wait_calls.load(Ordering::SeqCst)
    }

    fn kill_calls(&self) -> u32 {
        self.hooks.kill_calls.load(Ordering::SeqCst)
    }

    fn cleanup_calls(&self) -> u32 {
        self.hooks.cleanup_calls.load(Ordering::SeqCst)
    }
}

fn hook_slot() -> &'static Mutex<Option<Arc<PidHooks>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<PidHooks>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn hook_serialize() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn current_hooks() -> Option<Arc<PidHooks>> {
    hook_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

pub(super) fn intercept_waitpid(
    pid: libc::pid_t,
    _options: libc::c_int,
) -> Option<io::Result<(libc::pid_t, libc::c_int)>> {
    let hooks = current_hooks()?;
    if hooks.pid != pid {
        return None;
    }
    hooks.wait_calls.fetch_add(1, Ordering::SeqCst);
    Some(
        hooks
            .waits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .unwrap_or_else(|| Err(io::Error::from_raw_os_error(libc::ECHILD))),
    )
}

pub(super) fn intercept_kill(pid: libc::pid_t, _sig: libc::c_int) -> Option<libc::c_int> {
    let hooks = current_hooks()?;
    if hooks.pid != pid {
        return None;
    }
    hooks.kill_calls.fetch_add(1, Ordering::SeqCst);
    Some(0)
}

pub(super) fn note_cleanup(pid: libc::pid_t) {
    let Some(hooks) = current_hooks() else {
        return;
    };
    if hooks.pid == pid {
        hooks.cleanup_calls.fetch_add(1, Ordering::SeqCst);
    }
}

struct ContainmentProbe {
    remaining_failures: AtomicUsize,
    attempts: AtomicUsize,
    failed: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

struct ContainmentProbeGuard {
    probe: Arc<ContainmentProbe>,
}

fn containment_slot() -> &'static Mutex<Option<Arc<ContainmentProbe>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<ContainmentProbe>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

impl Drop for ContainmentProbeGuard {
    fn drop(&mut self) {
        *containment_slot()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

impl ContainmentProbeGuard {
    fn attempts(&self) -> usize {
        self.probe.attempts.load(Ordering::SeqCst)
    }
}

fn wait_for_release(probe: &ContainmentProbe) {
    if let Some(release) = probe
        .release
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        let _ = release.recv();
    }
}

pub(super) fn observe_containment() -> io::Result<()> {
    let probe = containment_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let Some(probe) = probe else {
        return Ok(());
    };
    probe.attempts.fetch_add(1, Ordering::SeqCst);
    match probe
        .remaining_failures
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
            remaining.checked_sub(1)
        }) {
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
            Err(io::Error::other("injected containment failure"))
        }
        Err(_) => Ok(()),
    }
}

fn install_containment_first_failure()
-> (ContainmentProbeGuard, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (failed_tx, failed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let probe = Arc::new(ContainmentProbe {
        remaining_failures: AtomicUsize::new(1),
        attempts: AtomicUsize::new(0),
        failed: Mutex::new(Some(failed_tx)),
        release: Mutex::new(Some(release_rx)),
    });
    let mut slot = containment_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(slot.is_none(), "containment probe already installed");
    *slot = Some(Arc::clone(&probe));
    drop(slot);
    (ContainmentProbeGuard { probe }, failed_rx, release_tx)
}

fn unique_pid() -> libc::pid_t {
    static NEXT: AtomicI32 = AtomicI32::new(2_000_000_000);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn install_pid_hooks(
    pid: libc::pid_t,
    wait: io::Result<(libc::pid_t, libc::c_int)>,
) -> PidHookGuard {
    let serialize = hook_serialize()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let hooks = Arc::new(PidHooks {
        pid,
        waits: Mutex::new(VecDeque::from([wait])),
        wait_calls: AtomicU32::new(0),
        kill_calls: AtomicU32::new(0),
        cleanup_calls: AtomicU32::new(0),
    });
    let mut slot = hook_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(slot.is_none(), "PID syscall hooks already installed");
    *slot = Some(Arc::clone(&hooks));
    drop(slot);
    PidHookGuard {
        _serialize: serialize,
        hooks,
    }
}

fn enrolled(pid: libc::pid_t) -> SpawnedPid {
    let mut spawned = SpawnedPid::new(pid);
    spawned.set_process_tree(ProcessTree::enroll_leader_pid(pid as u32).unwrap());
    spawned
}

fn exit_wait_status(code: i32) -> libc::c_int {
    code << 8
}

fn signaled_wait_status(sig: i32) -> libc::c_int {
    sig
}

fn stopped_wait_status(sig: i32) -> libc::c_int {
    (sig << 8) | 0o177
}

fn assert_no_pid_ops(guard: &PidHookGuard, teardown: &io::Result<()>) {
    if let Err(err) = teardown {
        panic!("authority loss must be a clean terminal state: {err}");
    }
    assert_eq!(guard.wait_calls(), 1, "exactly one waitpid owns this PID");
    assert_eq!(
        guard.kill_calls(),
        0,
        "kill must not run after authority loss"
    );
    assert_eq!(
        guard.cleanup_calls(),
        0,
        "SpawnedPid cleanup must not run after authority loss"
    );
}

#[test]
fn reaped_exit_before_suspend_does_not_signal_or_wait_again() {
    let pid = unique_pid();
    let guard = install_pid_hooks(pid, Ok((pid, exit_wait_status(0))));
    let spawned = enrolled(pid);
    let outcome = wait_until_stopped(pid);
    assert!(matches!(outcome, StopWait::ReapedExit));
    let failure = match spawned.finish_stop_wait(outcome) {
        Err(failure) => failure,
        Ok(_) => panic!("reaped exit must consume SpawnedPid"),
    };
    assert!(
        failure
            .error
            .to_string()
            .contains("exited before suspended verification")
    );
    assert_no_pid_ops(&guard, &failure.teardown);
}

#[test]
fn reaped_signal_before_suspend_does_not_signal_or_wait_again() {
    let pid = unique_pid();
    let guard = install_pid_hooks(pid, Ok((pid, signaled_wait_status(libc::SIGKILL))));
    let spawned = enrolled(pid);
    let outcome = wait_until_stopped(pid);
    assert!(matches!(outcome, StopWait::ReapedSignal));
    let failure = match spawned.finish_stop_wait(outcome) {
        Err(failure) => failure,
        Ok(_) => panic!("reaped signal must consume SpawnedPid"),
    };
    assert!(
        failure
            .error
            .to_string()
            .contains("signaled before suspended verification")
    );
    assert_no_pid_ops(&guard, &failure.teardown);
}

#[test]
fn echild_before_suspend_does_not_signal_or_wait_again() {
    let pid = unique_pid();
    let guard = install_pid_hooks(pid, Err(io::Error::from_raw_os_error(libc::ECHILD)));
    let spawned = enrolled(pid);
    let outcome = wait_until_stopped(pid);
    assert!(
        matches!(outcome, StopWait::NoChild(ref err) if err.raw_os_error() == Some(libc::ECHILD))
    );
    let failure = match spawned.finish_stop_wait(outcome) {
        Err(failure) => failure,
        Ok(_) => panic!("ECHILD must consume SpawnedPid"),
    };
    let message = failure.error.to_string();
    let echild = io::Error::from_raw_os_error(libc::ECHILD).to_string();
    assert!(
        message.contains("failed to wait for the suspended child"),
        "{message}"
    );
    assert!(message.contains(&echild), "{message}");
    assert_no_pid_ops(&guard, &failure.teardown);
}

#[test]
fn stopped_child_keeps_pid_authority() {
    let pid = unique_pid();
    let guard = install_pid_hooks(pid, Ok((pid, stopped_wait_status(libc::SIGSTOP))));
    let spawned = enrolled(pid);
    let outcome = wait_until_stopped(pid);
    assert!(matches!(outcome, StopWait::Stopped));
    let spawned = match spawned.finish_stop_wait(outcome) {
        Ok(spawned) => spawned,
        Err(failure) => panic!("stopped child remains owned: {}", failure.error),
    };
    assert_eq!(guard.wait_calls(), 1);
    assert_eq!(guard.kill_calls(), 0);
    assert_eq!(guard.cleanup_calls(), 0);
    let _ = spawned.disarm(ToolError::Execution("test drop".into()));
    assert_eq!(guard.kill_calls(), 0);
    assert_eq!(guard.cleanup_calls(), 0);
}

#[test]
fn unenrolled_suspended_child_uses_leader_only_cleanup() {
    let pid = unique_pid();
    let guard = install_pid_hooks(pid, Ok((pid, signaled_wait_status(libc::SIGKILL))));
    let spawned = SpawnedPid::new(pid);
    let failure = spawned.fail(ToolError::Execution("injected enrollment failure".into()));
    assert!(failure.teardown.is_ok(), "{:?}", failure.teardown);
    assert_eq!(guard.cleanup_calls(), 1);
    assert_eq!(
        guard.kill_calls(),
        1,
        "pre-enrollment cleanup kills the leader"
    );
    assert_eq!(
        guard.wait_calls(),
        1,
        "pre-enrollment cleanup reaps the leader"
    );
}

#[test]
fn enrolled_pending_cleanup_does_not_signal_or_reap_until_containment_succeeds() {
    let pid = unique_pid();
    let guard = install_pid_hooks(pid, Ok((pid, exit_wait_status(0))));
    let (probe, failed_rx, release_tx) = install_containment_first_failure();
    let worker = std::thread::spawn(move || {
        let spawned = enrolled(pid);
        spawned.fail(ToolError::Execution("injected verification failure".into()))
    });

    failed_rx.recv().expect("first containment attempt failed");
    assert_eq!(probe.attempts(), 1);
    assert_eq!(
        guard.cleanup_calls(),
        1,
        "first containment failure must keep the pending owner"
    );
    assert_eq!(
        guard.kill_calls(),
        0,
        "first containment failure must not signal a leader pid"
    );
    assert_eq!(
        guard.wait_calls(),
        0,
        "first containment failure must not reap a leader pid"
    );

    release_tx.send(()).expect("release containment retry");
    let failure = worker.join().expect("pending owner joined");
    assert!(
        failure.teardown.is_err(),
        "first containment attempt must be reported as teardown failure"
    );
    assert_eq!(probe.attempts(), 2);
    assert_eq!(guard.cleanup_calls(), 2);
    assert_eq!(
        guard.kill_calls(),
        0,
        "enrolled cleanup must not fall back to leader kill"
    );
    assert_eq!(
        guard.wait_calls(),
        1,
        "leader is reaped only after containment succeeds"
    );
}
