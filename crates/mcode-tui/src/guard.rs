//! RAII terminal restore for raw mode, the alternate screen, and output UTF-8.
//!
//! [`TerminalGuard`] records entered modes and restores them on Drop, on
//! explicit [`restore_on_abnormal_exit`], and from a panic hook installed by
//! [`TerminalGuard::enter`]. Tests use [`TerminalGuard::new_mocked`] so the
//! real console is never touched. Native crossterm calls run only on
//! [`TerminalGuard::enter`]. On Windows, enter also owns a UTF-8 output
//! code-page switch when that change can be restored exactly and the output
//! has a non-raster font, or virtual-terminal support when font metadata is
//! unavailable.
use std::io::{self, stdout};
use std::panic;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once, Weak};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

#[cfg(test)]
use crate::output_cp::OutputCodePage;
use crate::output_cp::Utf8OutputLease;

static GUARD_SLOT: Mutex<GuardSlot> = Mutex::new(GuardSlot {
    next_owner: 1,
    state: SlotState::Vacant,
});
static PANIC_HOOK: Once = Once::new();

/// Identifies one reservation across entering, active, and pending states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GuardOwner(u64);

#[derive(Debug)]
enum SlotState {
    Vacant,
    Entering {
        owner: GuardOwner,
        inner: Arc<GuardInner>,
    },
    Active {
        owner: GuardOwner,
        inner: Weak<GuardInner>,
    },
    PendingGuard {
        owner: GuardOwner,
        inner: Arc<GuardInner>,
    },
}

#[derive(Debug)]
struct GuardSlot {
    next_owner: u64,
    state: SlotState,
}

/// Process-wide state for terminal ownership and deferred restoration.
fn lock_slot() -> std::sync::MutexGuard<'static, GuardSlot> {
    GUARD_SLOT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Terminal-enter stage restored in reverse order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnterStage {
    RawMode,
    AlternateScreen,
    HideCursor,
    BracketedPaste,
}

const ENTER_STAGES: [EnterStage; 4] = [
    EnterStage::RawMode,
    EnterStage::AlternateScreen,
    EnterStage::HideCursor,
    EnterStage::BracketedPaste,
];
const RESTORE_STAGES: [EnterStage; 4] = [
    EnterStage::BracketedPaste,
    EnterStage::HideCursor,
    EnterStage::AlternateScreen,
    EnterStage::RawMode,
];

trait TerminalModes: std::fmt::Debug + Send + Sync {
    fn enter(&self, stage: EnterStage) -> io::Result<()>;
    fn leave(&self, stage: EnterStage) -> io::Result<()>;
}

#[derive(Debug)]
struct NativeTerminalModes;

impl TerminalModes for NativeTerminalModes {
    fn enter(&self, stage: EnterStage) -> io::Result<()> {
        match stage {
            EnterStage::RawMode => enable_raw_mode(),
            EnterStage::AlternateScreen => execute!(stdout(), EnterAlternateScreen),
            EnterStage::HideCursor => execute!(stdout(), Hide),
            EnterStage::BracketedPaste => execute!(stdout(), EnableBracketedPaste),
        }
    }

    fn leave(&self, stage: EnterStage) -> io::Result<()> {
        match stage {
            EnterStage::RawMode => disable_raw_mode(),
            EnterStage::AlternateScreen => execute!(stdout(), LeaveAlternateScreen),
            EnterStage::HideCursor => execute!(stdout(), Show),
            EnterStage::BracketedPaste => execute!(stdout(), DisableBracketedPaste),
        }
    }
}

#[derive(Debug)]
struct MockTerminalModes {
    fail_at: Option<EnterStage>,
}

impl MockTerminalModes {
    fn new(fail_at: Option<EnterStage>) -> Self {
        Self { fail_at }
    }
}

impl TerminalModes for MockTerminalModes {
    fn enter(&self, stage: EnterStage) -> io::Result<()> {
        if self.fail_at == Some(stage) {
            Err(io::Error::other("mock terminal enter failed"))
        } else {
            Ok(())
        }
    }

    fn leave(&self, _stage: EnterStage) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Default)]
struct TerminalState {
    raw: bool,
    alternate: bool,
    cursor_hidden: bool,
    bracketed_paste: bool,
    restore_started: bool,
}

impl TerminalState {
    fn is_entered(&self, stage: EnterStage) -> bool {
        match stage {
            EnterStage::RawMode => self.raw,
            EnterStage::AlternateScreen => self.alternate,
            EnterStage::HideCursor => self.cursor_hidden,
            EnterStage::BracketedPaste => self.bracketed_paste,
        }
    }

    fn set_entered(&mut self, stage: EnterStage, entered: bool) {
        match stage {
            EnterStage::RawMode => self.raw = entered,
            EnterStage::AlternateScreen => self.alternate = entered,
            EnterStage::HideCursor => self.cursor_hidden = entered,
            EnterStage::BracketedPaste => self.bracketed_paste = entered,
        }
    }

    fn is_clear(&self) -> bool {
        !self.raw && !self.alternate && !self.cursor_hidden && !self.bracketed_paste
    }
}

#[derive(Debug)]
struct GuardInner {
    terminal: Arc<dyn TerminalModes>,
    terminal_state: Mutex<TerminalState>,
    cancelled: AtomicBool,
    restore_count: AtomicUsize,
    output_cp: Utf8OutputLease,
}

impl GuardInner {
    fn new(output_cp: Utf8OutputLease, terminal: Arc<dyn TerminalModes>) -> Self {
        Self {
            terminal,
            terminal_state: Mutex::new(TerminalState::default()),
            cancelled: AtomicBool::new(false),
            restore_count: AtomicUsize::new(0),
            output_cp,
        }
    }

    fn request_cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn cancellation_error(&self) -> io::Error {
        io::Error::other("terminal entry was cancelled")
    }

    fn acquire_output(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(self.cancellation_error());
        }
        self.output_cp.acquire();
        if self.cancelled.load(Ordering::SeqCst) {
            Err(self.cancellation_error())
        } else {
            Ok(())
        }
    }

    fn enter_stage(&self, stage: EnterStage) -> io::Result<()> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(self.cancellation_error());
        }
        let mut state = self
            .terminal_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(self.cancellation_error());
        }
        // Terminal writes can apply a control sequence before reporting a
        // flush error, so every attempt carries restore responsibility.
        state.set_entered(stage, true);
        self.terminal.enter(stage)?;
        if self.cancelled.load(Ordering::SeqCst) {
            Err(self.cancellation_error())
        } else {
            Ok(())
        }
    }

    fn restore(&self) -> bool {
        let terminal_restored = {
            let mut state = self
                .terminal_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.is_clear() && !state.restore_started {
                state.restore_started = true;
                self.restore_count.fetch_add(1, Ordering::SeqCst);
            }
            for stage in RESTORE_STAGES {
                if state.is_entered(stage) && self.terminal.leave(stage).is_ok() {
                    state.set_entered(stage, false);
                }
            }
            state.is_clear()
        };
        let output_restored = self.output_cp.restore();
        terminal_restored && output_restored
    }

    fn is_restored(&self) -> bool {
        self.terminal_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_clear()
            && self.output_cp.restore_obligation_cleared()
    }

    fn is_stage_entered(&self, stage: EnterStage) -> bool {
        self.terminal_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_entered(stage)
    }
}

/// Holds the active-slot reservation until terminal entry commits or rolls back.
#[derive(Debug)]
struct EnterTransaction {
    owner: GuardOwner,
    inner: Arc<GuardInner>,
    claimed: bool,
}

impl EnterTransaction {
    fn begin(output_cp: Utf8OutputLease, terminal: Arc<dyn TerminalModes>) -> io::Result<Self> {
        let inner = Arc::new(GuardInner::new(output_cp, terminal));
        Ok(Self {
            owner: claim_active_slot(Arc::clone(&inner))?,
            inner,
            claimed: true,
        })
    }

    #[cfg(test)]
    fn new() -> io::Result<Self> {
        Self::begin(
            Utf8OutputLease::supported_without_switch(),
            Arc::new(MockTerminalModes::new(None)),
        )
    }

    fn acquire_output(&self) -> io::Result<()> {
        self.inner.acquire_output()
    }

    fn enter_stage(&self, stage: EnterStage) -> io::Result<()> {
        self.inner.enter_stage(stage)
    }

    fn finish(mut self) -> io::Result<TerminalGuard> {
        let committed = {
            let mut slot = lock_slot();
            match &slot.state {
                SlotState::Entering { owner, .. } if *owner == self.owner => {
                    if self.inner.cancelled.load(Ordering::SeqCst) {
                        false
                    } else {
                        slot.state = SlotState::Active {
                            owner: self.owner,
                            inner: Arc::downgrade(&self.inner),
                        };
                        true
                    }
                }
                _ => panic!("terminal guard entry lost its owned reservation"),
            }
        };
        if !committed {
            return Err(self.inner.cancellation_error());
        }
        self.claimed = false;
        Ok(TerminalGuard {
            owner: self.owner,
            inner: Arc::clone(&self.inner),
        })
    }
}

impl Drop for EnterTransaction {
    fn drop(&mut self) {
        if !self.claimed {
            return;
        }
        self.inner.request_cancel();
        let restored = self.inner.restore();
        finish_entering(self.owner, &self.inner, restored);
        self.claimed = false;
    }
}

/// Owns terminal modes and an optional Windows output code-page switch.
#[derive(Debug)]
pub struct TerminalGuard {
    owner: GuardOwner,
    inner: Arc<GuardInner>,
}

impl TerminalGuard {
    /// Enters raw mode and the alternate screen on the real terminal.
    ///
    /// Also installs a once-per-process panic hook that restores the active
    /// guard. On Windows, the console output code page is switched to UTF-8
    /// when that change can be owned and later restored exactly and the output
    /// has a non-raster font, or virtual-terminal support when font metadata is
    /// unavailable. Tests must use [`Self::new_mocked`] instead of this
    /// constructor.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when another guard owns the terminal, deferred
    /// restoration cannot complete, a terminal-enter stage fails, or abnormal
    /// restore cancels the in-flight entry. Each attempted stage is
    /// conservatively rolled back if it fails, a later stage fails, or the
    /// entry is cancelled. A failed output
    /// capability query or UTF-8 switch does not fail entry;
    /// [`Self::supports_unicode`] is then `false`.
    pub fn enter() -> io::Result<Self> {
        let entry = EnterTransaction::begin(native_utf8_output(), Arc::new(NativeTerminalModes))?;
        entry.acquire_output()?;
        for stage in ENTER_STAGES {
            entry.enter_stage(stage)?;
        }
        install_panic_hook();
        entry.finish()
    }

    /// Creates an entered mock guard that never talks to the console.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when another guard is active or deferred
    /// restoration cannot complete.
    pub fn new_mocked() -> io::Result<(Self, RestoreProbe)> {
        Self::enter_mocked(
            Utf8OutputLease::supported_without_switch(),
            Arc::new(MockTerminalModes::new(None)),
            || {},
        )
    }

    fn enter_mocked(
        output_cp: Utf8OutputLease,
        terminal: Arc<dyn TerminalModes>,
        on_output: impl FnOnce(),
    ) -> io::Result<(Self, RestoreProbe)> {
        let entry = EnterTransaction::begin(output_cp, terminal)?;
        entry.acquire_output()?;
        on_output();
        for stage in ENTER_STAGES {
            entry.enter_stage(stage)?;
        }
        let guard = entry.finish()?;
        let probe = RestoreProbe {
            inner: Arc::clone(&guard.inner),
        };
        Ok((guard, probe))
    }

    /// Enters a mock guard against an injected output code-page backend.
    ///
    /// `fail_at` rolls back an owned code-page switch without touching a TTY.
    #[cfg(test)]
    pub(crate) fn enter_mocked_with<T>(
        backend: Arc<T>,
        fail_at: Option<EnterStage>,
    ) -> io::Result<(Self, RestoreProbe)>
    where
        T: OutputCodePage + 'static,
    {
        Self::enter_mocked_with_on_output(backend, fail_at, || {})
    }

    /// Enters a mock guard and runs `on_output` after UTF-8 acquisition.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when another guard is active, deferred restoration
    /// cannot complete, a mock enter stage fails, or abnormal restore cancels
    /// the in-flight entry.
    #[cfg(test)]
    pub(crate) fn enter_mocked_with_on_output<T>(
        backend: Arc<T>,
        fail_at: Option<EnterStage>,
        on_output: impl FnOnce(),
    ) -> io::Result<(Self, RestoreProbe)>
    where
        T: OutputCodePage + 'static,
    {
        Self::enter_mocked(
            Utf8OutputLease::pending(backend),
            Arc::new(MockTerminalModes::new(fail_at)),
            on_output,
        )
    }

    #[cfg(test)]
    fn enter_mocked_with_terminal<T, U>(
        output: Arc<T>,
        terminal: Arc<U>,
    ) -> io::Result<(Self, RestoreProbe)>
    where
        T: OutputCodePage + 'static,
        U: TerminalModes + 'static,
    {
        Self::enter_mocked(Utf8OutputLease::pending(output), terminal, || {})
    }

    /// Restores terminal modes and any owned output code-page switch.
    pub fn restore(&self) {
        self.inner.restore();
    }

    /// Returns whether terminal cleanup finished and no output restore remains.
    #[must_use]
    pub fn is_restored(&self) -> bool {
        self.inner.is_restored()
    }

    /// Returns whether Unicode output was proven during enter.
    ///
    /// On Windows this is `true` only for non-raster output, or virtual-terminal
    /// output when font metadata is unavailable, after a successful UTF-8
    /// code-page query or owned switch. Non-Windows entry keeps Unicode enabled.
    /// Failed capability, query, or switch leaves this `false`.
    #[must_use]
    pub fn supports_unicode(&self) -> bool {
        self.inner.output_cp.supports_unicode()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let restored = self.inner.restore();
        finish_guard(self.owner, &self.inner, restored);
    }
}

/// Observes mock restore without holding the RAII guard.
#[derive(Debug, Clone)]
pub struct RestoreProbe {
    inner: Arc<GuardInner>,
}

impl RestoreProbe {
    /// Returns whether terminal cleanup finished and no output restore remains.
    #[must_use]
    pub fn is_restored(&self) -> bool {
        self.inner.is_restored()
    }

    /// Returns how many terminal cleanup sequences started.
    #[must_use]
    pub fn restore_count(&self) -> usize {
        self.inner.restore_count.load(Ordering::SeqCst)
    }

    /// Returns whether mock raw mode is still entered.
    #[must_use]
    pub fn is_raw_mode(&self) -> bool {
        self.inner.is_stage_entered(EnterStage::RawMode)
    }

    /// Returns whether the mock alternate screen is still entered.
    #[must_use]
    pub fn is_alternate_screen(&self) -> bool {
        self.inner.is_stage_entered(EnterStage::AlternateScreen)
    }

    /// Returns whether the mock cursor is still hidden.
    #[must_use]
    pub fn is_cursor_hidden(&self) -> bool {
        self.inner.is_stage_entered(EnterStage::HideCursor)
    }

    /// Returns whether mock bracketed paste is still entered.
    #[must_use]
    pub fn is_bracketed_paste(&self) -> bool {
        self.inner.is_stage_entered(EnterStage::BracketedPaste)
    }

    /// Returns whether Unicode output was proven during enter.
    #[must_use]
    pub fn supports_unicode(&self) -> bool {
        self.inner.output_cp.supports_unicode()
    }
}

/// Restores the active or entering guard after an abnormal exit request.
///
/// On Windows, a host can call this from a console control callback. The
/// function is the same path used by the panic hook installed in
/// [`TerminalGuard::enter`]. An in-flight enter is cancelled, waits for its
/// current mutation, and restores every published terminal and output stage
/// before this function returns. Only that entering owner releases its slot.
pub fn restore_on_abnormal_exit() {
    let Some(target) = abnormal_restore_target() else {
        return;
    };
    match target {
        RestoreTarget::Entering(inner) => {
            inner.restore();
        }
        RestoreTarget::Guard(owner, inner) => {
            if inner.restore() {
                clear_restored_owner(owner);
            }
        }
    }
}

fn native_utf8_output() -> Utf8OutputLease {
    #[cfg(windows)]
    {
        Utf8OutputLease::pending(Arc::new(crate::output_cp::NativeOutputCodePage))
    }
    #[cfg(not(windows))]
    {
        Utf8OutputLease::supported_without_switch()
    }
}

#[derive(Debug)]
enum RestoreTarget {
    Entering(Arc<GuardInner>),
    Guard(GuardOwner, Arc<GuardInner>),
}

fn claim_active_slot(inner: Arc<GuardInner>) -> io::Result<GuardOwner> {
    loop {
        let pending = {
            let mut slot = lock_slot();
            match &slot.state {
                SlotState::Vacant => {
                    let owner = GuardOwner(slot.next_owner);
                    slot.next_owner = slot
                        .next_owner
                        .checked_add(1)
                        .expect("terminal guard owner space exhausted");
                    slot.state = SlotState::Entering { owner, inner };
                    return Ok(owner);
                }
                SlotState::Entering { .. } | SlotState::Active { .. } => {
                    return Err(io::Error::other("a terminal guard is already active"));
                }
                SlotState::PendingGuard { owner, inner } => {
                    RestoreTarget::Guard(*owner, Arc::clone(inner))
                }
            }
        };
        let RestoreTarget::Guard(owner, pending_inner) = pending else {
            unreachable!("pending claim cannot observe an in-flight enter");
        };
        if !pending_inner.restore() {
            return Err(io::Error::other(
                "a previous terminal guard still owns restoration",
            ));
        }
        clear_restored_owner(owner);
    }
}

fn finish_entering(owner: GuardOwner, inner: &Arc<GuardInner>, restored: bool) {
    let old_state = {
        let mut slot = lock_slot();
        if !matches!(&slot.state, SlotState::Entering { owner: current, .. } if *current == owner) {
            None
        } else if restored {
            Some(std::mem::replace(&mut slot.state, SlotState::Vacant))
        } else {
            Some(std::mem::replace(
                &mut slot.state,
                SlotState::PendingGuard {
                    owner,
                    inner: Arc::clone(inner),
                },
            ))
        }
    };
    drop(old_state);
}

fn finish_guard(owner: GuardOwner, inner: &Arc<GuardInner>, restored: bool) {
    let old_state = {
        let mut slot = lock_slot();
        let owned = matches!(
            &slot.state,
            SlotState::Active { owner: current, .. }
                | SlotState::PendingGuard { owner: current, .. }
                if *current == owner
        );
        if !owned {
            None
        } else if restored {
            Some(std::mem::replace(&mut slot.state, SlotState::Vacant))
        } else {
            Some(std::mem::replace(
                &mut slot.state,
                SlotState::PendingGuard {
                    owner,
                    inner: Arc::clone(inner),
                },
            ))
        }
    };
    drop(old_state);
}

fn abnormal_restore_target() -> Option<RestoreTarget> {
    let slot = lock_slot();
    match &slot.state {
        SlotState::Entering { inner, .. } => {
            inner.request_cancel();
            Some(RestoreTarget::Entering(Arc::clone(inner)))
        }
        SlotState::Active { owner, inner } => inner
            .upgrade()
            .map(|inner| RestoreTarget::Guard(*owner, inner)),
        SlotState::PendingGuard { owner, inner } => {
            Some(RestoreTarget::Guard(*owner, Arc::clone(inner)))
        }
        SlotState::Vacant => None,
    }
}

fn clear_restored_owner(owner: GuardOwner) {
    let old_state = {
        let mut slot = lock_slot();
        let owned = matches!(
            &slot.state,
            SlotState::Active { owner: current, .. }
                | SlotState::PendingGuard { owner: current, .. }
                if *current == owner
        );
        owned.then(|| std::mem::replace(&mut slot.state, SlotState::Vacant))
    };
    drop(old_state);
}

fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            restore_on_abnormal_exit();
            previous(info);
        }));
    });
}

#[cfg(test)]
mod tests;
