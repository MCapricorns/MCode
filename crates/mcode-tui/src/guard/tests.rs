use super::{
    EnterStage, EnterTransaction, MockTerminalModes, TerminalGuard, TerminalModes,
    restore_on_abnormal_exit,
};
use crate::output_cp::{CP_UTF8, MockOutputCodePage, OutputCodePage};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

#[derive(Debug)]
struct BlockingState {
    current: u32,
    fail_next_restore: bool,
    restore_started: bool,
    allow_restore: bool,
}

#[derive(Debug)]
struct BlockingOutputCodePage {
    state: Mutex<BlockingState>,
    restore_started: Condvar,
    allow_restore: Condvar,
}

impl BlockingOutputCodePage {
    fn new(initial: u32) -> Arc<Self> {
        Self::with_failed_first_restore(initial, false)
    }

    fn failing_once(initial: u32) -> Arc<Self> {
        Self::with_failed_first_restore(initial, true)
    }

    fn with_failed_first_restore(initial: u32, fail_next_restore: bool) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(BlockingState {
                current: initial,
                fail_next_restore,
                restore_started: false,
                allow_restore: false,
            }),
            restore_started: Condvar::new(),
            allow_restore: Condvar::new(),
        })
    }

    fn wait_for_restore(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !state.restore_started {
            state = self
                .restore_started
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn unblock_restore(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.allow_restore = true;
        self.allow_restore.notify_one();
    }
}

impl OutputCodePage for BlockingOutputCodePage {
    fn supports_unicode_glyphs(&self) -> io::Result<bool> {
        Ok(true)
    }

    fn output_code_page(&self) -> io::Result<u32> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(state.current)
    }

    fn set_output_code_page(&self, code_page: u32) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if code_page != CP_UTF8 {
            if state.fail_next_restore {
                state.fail_next_restore = false;
                return Err(io::Error::other("mock transient restore failure"));
            }
            state.restore_started = true;
            self.restore_started.notify_one();
            while !state.allow_restore {
                state = self
                    .allow_restore
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
        state.current = code_page;
        Ok(())
    }
}

fn guard_test_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug)]
struct EnterPause {
    ready: Mutex<bool>,
    ready_signal: Condvar,
    go: Mutex<bool>,
    go_signal: Condvar,
}

impl EnterPause {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            ready: Mutex::new(false),
            ready_signal: Condvar::new(),
            go: Mutex::new(false),
            go_signal: Condvar::new(),
        })
    }

    fn wait_ready(&self) {
        let mut ready = self
            .ready
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*ready {
            ready = self
                .ready_signal
                .wait(ready)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn allow(&self) {
        let mut go = self
            .go
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *go = true;
        self.go_signal.notify_one();
    }

    fn gate(&self) {
        {
            let mut ready = self
                .ready
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *ready = true;
            self.ready_signal.notify_one();
        }
        let mut go = self
            .go
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*go {
            go = self
                .go_signal
                .wait(go)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

fn stage_index(stage: EnterStage) -> usize {
    match stage {
        EnterStage::RawMode => 0,
        EnterStage::AlternateScreen => 1,
        EnterStage::HideCursor => 2,
        EnterStage::BracketedPaste => 3,
    }
}

#[derive(Debug)]
struct FailOnceLeaveTerminal {
    fail_at: EnterStage,
    failed: Mutex<bool>,
    entered: Mutex<[bool; 4]>,
}

impl FailOnceLeaveTerminal {
    fn new(fail_at: EnterStage) -> Arc<Self> {
        Arc::new(Self {
            fail_at,
            failed: Mutex::new(false),
            entered: Mutex::new([false; 4]),
        })
    }

    fn is_clear(&self) -> bool {
        self.entered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .all(|entered| !entered)
    }
}

impl TerminalModes for FailOnceLeaveTerminal {
    fn enter(&self, stage: EnterStage) -> io::Result<()> {
        self.entered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[stage_index(stage)] = true;
        Ok(())
    }

    fn leave(&self, stage: EnterStage) -> io::Result<()> {
        if stage == self.fail_at {
            let mut failed = self
                .failed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !*failed {
                *failed = true;
                return Err(io::Error::other("mock terminal restore failed"));
            }
        }
        self.entered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[stage_index(stage)] = false;
        Ok(())
    }
}

#[derive(Debug)]
struct FailAfterMutationTerminal {
    fail_at: EnterStage,
    fail_leave: Mutex<bool>,
    entered: Mutex<[bool; 4]>,
}

impl FailAfterMutationTerminal {
    fn new(fail_at: EnterStage) -> Arc<Self> {
        Arc::new(Self {
            fail_at,
            fail_leave: Mutex::new(true),
            entered: Mutex::new([false; 4]),
        })
    }

    fn allow_leave(&self) {
        *self
            .fail_leave
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
    }

    fn is_clear(&self) -> bool {
        self.entered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .all(|entered| !entered)
    }
}

impl TerminalModes for FailAfterMutationTerminal {
    fn enter(&self, stage: EnterStage) -> io::Result<()> {
        self.entered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[stage_index(stage)] = true;
        if stage == self.fail_at {
            Err(io::Error::other("mock error after terminal mutation"))
        } else {
            Ok(())
        }
    }

    fn leave(&self, stage: EnterStage) -> io::Result<()> {
        if *self
            .fail_leave
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return Err(io::Error::other("mock terminal restore failed"));
        }
        self.entered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[stage_index(stage)] = false;
        Ok(())
    }
}

#[derive(Debug)]
struct PausingTerminalState {
    entered: [bool; 4],
    mutation_ready: bool,
    allow_return: bool,
}

#[derive(Debug)]
struct PausingTerminalModes {
    pause_at: EnterStage,
    state: Mutex<PausingTerminalState>,
    mutation_ready: Condvar,
    allow_return: Condvar,
}

impl PausingTerminalModes {
    fn new(pause_at: EnterStage) -> Arc<Self> {
        Arc::new(Self {
            pause_at,
            state: Mutex::new(PausingTerminalState {
                entered: [false; 4],
                mutation_ready: false,
                allow_return: false,
            }),
            mutation_ready: Condvar::new(),
            allow_return: Condvar::new(),
        })
    }

    fn wait_for_mutation(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !state.mutation_ready {
            state = self
                .mutation_ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn allow_mutation_return(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.allow_return = true;
        self.allow_return.notify_one();
    }

    fn is_clear(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entered
            .iter()
            .all(|entered| !entered)
    }
}

impl TerminalModes for PausingTerminalModes {
    fn enter(&self, stage: EnterStage) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.entered[stage_index(stage)] = true;
        if stage == self.pause_at {
            state.mutation_ready = true;
            self.mutation_ready.notify_one();
            while !state.allow_return {
                state = self
                    .allow_return
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
        Ok(())
    }

    fn leave(&self, stage: EnterStage) -> io::Result<()> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entered[stage_index(stage)] = false;
        Ok(())
    }
}

#[derive(Debug)]
struct PausingOutputState {
    current: u32,
    mutation_ready: bool,
    allow_return: bool,
}

#[derive(Debug)]
struct PausingOutputCodePage {
    state: Mutex<PausingOutputState>,
    mutation_ready: Condvar,
    allow_return: Condvar,
}

impl PausingOutputCodePage {
    fn new(initial: u32) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(PausingOutputState {
                current: initial,
                mutation_ready: false,
                allow_return: false,
            }),
            mutation_ready: Condvar::new(),
            allow_return: Condvar::new(),
        })
    }

    fn wait_for_mutation(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !state.mutation_ready {
            state = self
                .mutation_ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn allow_mutation_return(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.allow_return = true;
        self.allow_return.notify_one();
    }

    fn current(&self) -> u32 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .current
    }
}

impl OutputCodePage for PausingOutputCodePage {
    fn supports_unicode_glyphs(&self) -> io::Result<bool> {
        Ok(true)
    }

    fn output_code_page(&self) -> io::Result<u32> {
        Ok(self.current())
    }

    fn set_output_code_page(&self, code_page: u32) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.current = code_page;
        if code_page == CP_UTF8 {
            state.mutation_ready = true;
            self.mutation_ready.notify_one();
            while !state.allow_return {
                state = self
                    .allow_return
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
        Ok(())
    }
}

fn spawn_abnormal_restore() -> (std::thread::JoinHandle<()>, mpsc::Receiver<()>) {
    let (finished_tx, finished_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        restore_on_abnormal_exit();
        finished_tx
            .send(())
            .expect("abnormal completion receiver must remain alive");
    });
    (worker, finished_rx)
}

fn assert_restore_is_blocked(finished: &mpsc::Receiver<()>) {
    assert_eq!(
        finished.recv_timeout(Duration::from_millis(50)),
        Err(RecvTimeoutError::Timeout),
        "abnormal restore returned before the in-flight mutation completed"
    );
}

#[test]
fn already_utf8_does_not_issue_unnecessary_mutation() {
    let _lock = guard_test_lock();
    let backend = MockOutputCodePage::new(CP_UTF8);
    let (guard, probe) =
        TerminalGuard::enter_mocked_with(Arc::clone(&backend), None).expect("enter");
    assert!(guard.supports_unicode());
    assert_eq!(backend.get_count(), 1);
    assert!(backend.set_calls().is_empty());
    guard.restore();
    drop(guard);
    assert!(probe.is_restored());
    assert_eq!(probe.restore_count(), 1);
    assert!(backend.set_calls().is_empty());
    assert_eq!(backend.current(), CP_UTF8);
}

#[test]
fn gbk_936_switches_to_utf8_and_restores_exactly_once() {
    let _lock = guard_test_lock();
    let backend = MockOutputCodePage::new(936);
    let (guard, probe) =
        TerminalGuard::enter_mocked_with(Arc::clone(&backend), None).expect("enter");
    assert!(guard.supports_unicode());
    assert_eq!(backend.set_calls(), vec![CP_UTF8]);
    assert_eq!(backend.current(), CP_UTF8);
    guard.restore();
    drop(guard);
    assert_eq!(probe.restore_count(), 1);
    assert_eq!(backend.set_calls(), vec![CP_UTF8, 936]);
    assert_eq!(backend.current(), 936);
}

#[test]
fn switch_failure_uses_ascii_and_does_not_restore_unowned_change() {
    let _lock = guard_test_lock();
    let backend = MockOutputCodePage::new(936);
    backend.fail_set();
    let (guard, probe) =
        TerminalGuard::enter_mocked_with(Arc::clone(&backend), None).expect("enter");
    assert!(!guard.supports_unicode());
    assert!(!probe.supports_unicode());
    assert_eq!(backend.set_calls(), vec![CP_UTF8]);
    assert_eq!(backend.current(), 936);
    guard.restore();
    drop(guard);
    assert_eq!(probe.restore_count(), 1);
    assert_eq!(backend.set_calls(), vec![CP_UTF8]);
    assert_eq!(backend.current(), 936);
}

#[test]
fn partial_enter_failure_restores_owned_code_page() {
    let _lock = guard_test_lock();
    let backend = MockOutputCodePage::new(936);
    let error =
        TerminalGuard::enter_mocked_with(Arc::clone(&backend), Some(EnterStage::AlternateScreen))
            .expect_err("later stage must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
    assert_eq!(backend.set_calls(), vec![CP_UTF8, 936]);
    assert_eq!(backend.current(), 936);
    let (guard, _) = TerminalGuard::new_mocked().expect("slot released");
    drop(guard);
}

#[test]
fn failed_entry_restore_is_retried_before_the_next_claim() {
    let _lock = guard_test_lock();
    let backend = BlockingOutputCodePage::failing_once(936);
    let error =
        TerminalGuard::enter_mocked_with(Arc::clone(&backend), Some(EnterStage::AlternateScreen))
            .expect_err("later stage must fail");
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(
        backend
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .current,
        CP_UTF8
    );

    backend.unblock_restore();
    let (guard, _) = TerminalGuard::new_mocked().expect("pending entry restore retried");
    assert_eq!(
        backend
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .current,
        936
    );
    drop(guard);
}

#[test]
fn guard_restore_retries_a_transient_output_failure() {
    let _lock = guard_test_lock();
    let backend = MockOutputCodePage::new(936);
    let (guard, probe) =
        TerminalGuard::enter_mocked_with(Arc::clone(&backend), None).expect("enter");
    backend.fail_next_set();
    guard.restore();
    assert!(!guard.is_restored());
    assert!(!probe.is_restored());
    assert_eq!(backend.current(), CP_UTF8);
    assert_eq!(probe.restore_count(), 1);
    guard.restore();
    assert!(guard.is_restored());
    assert!(probe.is_restored());
    assert_eq!(backend.current(), 936);
    assert_eq!(backend.set_calls(), vec![CP_UTF8, 936, 936]);
    assert_eq!(probe.restore_count(), 1);
}

#[test]
fn failed_entry_keeps_slot_reserved_until_output_restore_finishes() {
    let _lock = guard_test_lock();
    let backend = BlockingOutputCodePage::new(936);
    let worker_backend = Arc::clone(&backend);
    let worker = std::thread::spawn(move || {
        TerminalGuard::enter_mocked_with(worker_backend, Some(EnterStage::AlternateScreen))
            .expect_err("later stage must fail")
    });

    backend.wait_for_restore();
    let abnormal = std::thread::spawn(restore_on_abnormal_exit);
    let competing = TerminalGuard::new_mocked();
    assert!(competing.is_err(), "slot released before output restore");
    backend.unblock_restore();
    abnormal.join().expect("abnormal restore must not panic");
    let error = worker.join().expect("entry worker must not panic");
    assert_eq!(error.kind(), io::ErrorKind::Other);

    let (guard, _) = TerminalGuard::new_mocked().expect("slot released after restore");
    drop(guard);
}

#[test]
fn abnormal_restore_during_enter_cancels_commit_and_does_not_affect_later_owner() {
    let _lock = guard_test_lock();
    let backend_a = MockOutputCodePage::new(936);
    let pause_a = EnterPause::new();
    let worker_pause = Arc::clone(&pause_a);
    let worker_backend = Arc::clone(&backend_a);
    let worker_a = std::thread::spawn(move || {
        TerminalGuard::enter_mocked_with_on_output(worker_backend, None, || worker_pause.gate())
    });

    pause_a.wait_ready();
    assert_eq!(backend_a.current(), CP_UTF8);
    restore_on_abnormal_exit();
    assert_eq!(backend_a.current(), 936);
    assert!(
        TerminalGuard::new_mocked().is_err(),
        "reservation released before entering owner rolled back"
    );
    pause_a.allow();
    let result = worker_a.join().expect("enter worker must not panic");
    assert!(result.is_err(), "commit must refuse activation");

    let backend_b = MockOutputCodePage::new(437);
    let pause_b = EnterPause::new();
    let worker_pause = Arc::clone(&pause_b);
    let worker_backend = Arc::clone(&backend_b);
    let worker_b = std::thread::spawn(move || {
        TerminalGuard::enter_mocked_with_on_output(worker_backend, None, || worker_pause.gate())
    });
    pause_b.wait_ready();
    assert_eq!(backend_b.current(), CP_UTF8);
    assert_eq!(backend_a.current(), 936);
    assert!(
        TerminalGuard::new_mocked().is_err(),
        "later owner reservation was not exclusive"
    );
    pause_b.allow();
    let (guard_b, _) = worker_b
        .join()
        .expect("later owner must not panic")
        .expect("later owner must activate");
    assert_eq!(backend_b.current(), CP_UTF8);
    drop(guard_b);
    assert_eq!(backend_b.current(), 437);
    assert_eq!(backend_a.current(), 936);
}

#[test]
fn abnormal_restore_waits_for_output_switch_and_restores_it() {
    let _lock = guard_test_lock();
    let output = PausingOutputCodePage::new(936);
    let worker_output = Arc::clone(&output);
    let worker = std::thread::spawn(move || {
        TerminalGuard::enter_mocked_with_terminal(
            worker_output,
            Arc::new(MockTerminalModes::new(None)),
        )
    });

    output.wait_for_mutation();
    assert_eq!(output.current(), CP_UTF8);
    let (abnormal, finished) = spawn_abnormal_restore();
    assert_restore_is_blocked(&finished);
    output.allow_mutation_return();
    finished
        .recv_timeout(Duration::from_secs(1))
        .expect("abnormal restore must finish after output acquisition returns");
    abnormal.join().expect("abnormal restore must not panic");
    let result = worker.join().expect("enter worker must not panic");
    assert!(
        result.is_err(),
        "cancelled output acquisition must not commit"
    );
    assert_eq!(output.current(), 936);

    let (guard, _) = TerminalGuard::new_mocked().expect("entering owner released restored slot");
    drop(guard);
}

#[test]
fn abnormal_restore_waits_for_each_terminal_mutation_and_restores_it() {
    let _lock = guard_test_lock();
    for stage in [
        EnterStage::RawMode,
        EnterStage::AlternateScreen,
        EnterStage::HideCursor,
        EnterStage::BracketedPaste,
    ] {
        let terminal = PausingTerminalModes::new(stage);
        let worker_terminal = Arc::clone(&terminal);
        let worker = std::thread::spawn(move || {
            TerminalGuard::enter_mocked_with_terminal(
                MockOutputCodePage::new(CP_UTF8),
                worker_terminal,
            )
        });

        terminal.wait_for_mutation();
        let (abnormal, finished) = spawn_abnormal_restore();
        assert_restore_is_blocked(&finished);
        terminal.allow_mutation_return();
        finished
            .recv_timeout(Duration::from_secs(1))
            .expect("abnormal restore must finish after terminal mutation returns");
        abnormal.join().expect("abnormal restore must not panic");
        let result = worker.join().expect("enter worker must not panic");
        assert!(result.is_err(), "cancelled terminal entry must not commit");
        assert!(
            terminal.is_clear(),
            "all entered terminal stages must restore"
        );

        let (guard, _) =
            TerminalGuard::new_mocked().expect("entering owner released restored slot");
        drop(guard);
    }
}

#[test]
fn failed_stage_after_mutation_keeps_owner_until_full_restore() {
    let _lock = guard_test_lock();
    for stage in [
        EnterStage::RawMode,
        EnterStage::AlternateScreen,
        EnterStage::HideCursor,
        EnterStage::BracketedPaste,
    ] {
        let terminal = FailAfterMutationTerminal::new(stage);
        let error = TerminalGuard::enter_mocked_with_terminal(
            MockOutputCodePage::new(CP_UTF8),
            Arc::clone(&terminal),
        )
        .expect_err("stage error after mutation must fail entry");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(
            !terminal.is_clear(),
            "failed mutation must retain restore responsibility"
        );
        assert!(
            TerminalGuard::new_mocked().is_err(),
            "failed rollback released the entering owner"
        );

        terminal.allow_leave();
        let (guard, _) =
            TerminalGuard::new_mocked().expect("complete retry must release the old owner");
        assert!(terminal.is_clear(), "every attempted stage must restore");
        drop(guard);
    }
}

#[test]
fn terminal_restore_failure_keeps_owner_until_each_stage_retries() {
    let _lock = guard_test_lock();
    for stage in [
        EnterStage::RawMode,
        EnterStage::AlternateScreen,
        EnterStage::HideCursor,
        EnterStage::BracketedPaste,
    ] {
        let terminal = FailOnceLeaveTerminal::new(stage);
        let (old_guard, probe) = TerminalGuard::enter_mocked_with_terminal(
            MockOutputCodePage::new(CP_UTF8),
            Arc::clone(&terminal),
        )
        .expect("mock guard must enter");

        restore_on_abnormal_exit();
        assert!(!old_guard.is_restored());
        assert!(!probe.is_restored());
        assert_eq!(probe.restore_count(), 1);
        assert!(!terminal.is_clear());
        assert!(
            TerminalGuard::new_mocked().is_err(),
            "failed terminal restoration released its owner"
        );

        restore_on_abnormal_exit();
        assert!(old_guard.is_restored());
        assert!(probe.is_restored());
        assert_eq!(probe.restore_count(), 1);
        assert!(terminal.is_clear());

        let (new_guard, _) =
            TerminalGuard::new_mocked().expect("successful terminal retry released the slot");
        drop(new_guard);
        drop(old_guard);
    }
}

#[test]
fn stale_guard_drop_does_not_release_a_new_entry() {
    let _lock = guard_test_lock();
    let (old_guard, _probe) = TerminalGuard::new_mocked().expect("old guard");
    restore_on_abnormal_exit();

    let entry = EnterTransaction::new().expect("new entry reservation");
    drop(old_guard);
    assert!(
        TerminalGuard::new_mocked().is_err(),
        "stale guard drop released the new entry"
    );
    drop(entry);

    let (guard, _) = TerminalGuard::new_mocked().expect("slot released by its owner");
    drop(guard);
}

#[test]
fn drop_retry_keeps_slot_until_output_restore_finishes() {
    let _lock = guard_test_lock();
    let backend = BlockingOutputCodePage::failing_once(936);
    let (guard, probe) =
        TerminalGuard::enter_mocked_with(Arc::clone(&backend), None).expect("enter");
    guard.restore();
    drop(probe);

    let worker = std::thread::spawn(move || drop(guard));
    backend.wait_for_restore();
    assert!(
        TerminalGuard::new_mocked().is_err(),
        "slot released while Drop was retrying output restore"
    );
    backend.unblock_restore();
    worker.join().expect("guard Drop must not panic");

    let (guard, _) = TerminalGuard::new_mocked().expect("slot released after Drop restore");
    drop(guard);
}

#[test]
fn failed_drop_restore_is_retried_before_the_next_claim() {
    let _lock = guard_test_lock();
    let backend = MockOutputCodePage::new(936);
    let (guard, probe) =
        TerminalGuard::enter_mocked_with(Arc::clone(&backend), None).expect("enter");
    drop(probe);
    backend.fail_next_set();
    drop(guard);
    assert_eq!(backend.current(), CP_UTF8);

    let (guard, _) = TerminalGuard::new_mocked().expect("pending restore retried");
    assert_eq!(backend.current(), 936);
    drop(guard);
}

#[test]
fn failed_abnormal_restore_does_not_release_the_slot() {
    let _lock = guard_test_lock();
    let backend = MockOutputCodePage::new(936);
    let (old_guard, probe) =
        TerminalGuard::enter_mocked_with(Arc::clone(&backend), None).expect("enter");
    backend.fail_next_set();

    restore_on_abnormal_exit();
    assert!(!old_guard.is_restored());
    assert!(!probe.is_restored());
    assert_eq!(probe.restore_count(), 1);
    assert!(
        TerminalGuard::new_mocked().is_err(),
        "failed abnormal restore released the slot"
    );
    restore_on_abnormal_exit();
    assert!(old_guard.is_restored());
    assert!(probe.is_restored());
    assert_eq!(probe.restore_count(), 1);
    assert_eq!(backend.current(), 936);

    let (guard, _) = TerminalGuard::new_mocked().expect("successful retry released slot");
    drop(guard);
    drop(old_guard);
}

#[test]
fn explicit_restore_drop_and_abnormal_exit_are_idempotent() {
    let _lock = guard_test_lock();
    let backend = MockOutputCodePage::new(936);
    let (guard, probe) =
        TerminalGuard::enter_mocked_with(Arc::clone(&backend), None).expect("enter");
    guard.restore();
    guard.restore();
    drop(guard);
    assert_eq!(probe.restore_count(), 1);
    assert_eq!(backend.set_calls(), vec![CP_UTF8, 936]);

    let backend = MockOutputCodePage::new(936);
    let (guard, probe) =
        TerminalGuard::enter_mocked_with(Arc::clone(&backend), None).expect("enter");
    std::mem::forget(guard);
    restore_on_abnormal_exit();
    restore_on_abnormal_exit();
    assert_eq!(probe.restore_count(), 1);
    assert_eq!(backend.set_calls(), vec![CP_UTF8, 936]);
    assert_eq!(backend.current(), 936);

    let backend = MockOutputCodePage::new(936);
    let (guard, probe) =
        TerminalGuard::enter_mocked_with(Arc::clone(&backend), None).expect("enter");
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _guard = guard;
        panic!("terminal guard unwind");
    }));
    assert!(result.is_err());
    assert_eq!(probe.restore_count(), 1);
    assert_eq!(backend.set_calls(), vec![CP_UTF8, 936]);
    assert_eq!(backend.current(), 936);
}
