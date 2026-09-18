//! Owned Windows console output code-page setup for Unicode rendering.
//!
//! Host shells may report UTF-8 while the console output page stays on a
//! legacy identifier such as 936. This module switches only the output page
//! to [`CP_UTF8`] when that change can be owned, and records the original
//! identifier so restore can put it back. Unicode rendering also requires a
//! non-raster console font, or virtual-terminal output when font metadata is
//! unavailable. The input page is never modified.
use std::fmt;
use std::io;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

/// UTF-8 console output code page (`CP_UTF8` in windows-sys 0.61.2).
pub(crate) const CP_UTF8: u32 = 65001;

#[cfg(windows)]
const _: () = assert!(
    CP_UTF8 == windows_sys::Win32::Globalization::CP_UTF8,
    "CP_UTF8 must match locked windows-sys 0.61.2"
);

/// Console output code-page get/set used by [`TerminalGuard`](crate::TerminalGuard).
///
/// Native Windows calls `GetConsoleOutputCP` / `SetConsoleOutputCP`. Tests
/// inject [`MockOutputCodePage`] so the process page is never mutated.
pub(crate) trait OutputCodePage: Send + Sync {
    /// Returns whether the output surface can display Unicode glyphs.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when output capability cannot be queried.
    fn supports_unicode_glyphs(&self) -> io::Result<bool>;

    /// Returns the current output code page identifier.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the identifier cannot be queried.
    fn output_code_page(&self) -> io::Result<u32>;

    /// Sets the output code page identifier.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the identifier cannot be applied.
    fn set_output_code_page(&self, code_page: u32) -> io::Result<()>;
}

/// Mutable state serialized across acquisition and restoration.
#[derive(Debug)]
struct OutputLeaseState {
    acquired: bool,
    unicode: bool,
    restore_to: Option<u32>,
}

/// Owns a successful UTF-8 output code-page switch.
pub(crate) struct Utf8OutputLease {
    backend: Option<Arc<dyn OutputCodePage>>,
    state: Mutex<OutputLeaseState>,
}

impl fmt::Debug for Utf8OutputLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Utf8OutputLease")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl Utf8OutputLease {
    /// Creates an acquired lease that requires no code-page mutation.
    pub(crate) fn supported_without_switch() -> Self {
        Self {
            backend: None,
            state: Mutex::new(OutputLeaseState {
                acquired: true,
                unicode: true,
                restore_to: None,
            }),
        }
    }

    /// Creates an unpublished-mutation lease for `backend`.
    ///
    /// The caller can publish this value before [`Self::acquire`] performs any
    /// process-global mutation. Acquisition and restoration share one lock.
    #[cfg(any(windows, test))]
    pub(crate) fn pending<T>(backend: Arc<T>) -> Self
    where
        T: OutputCodePage + 'static,
    {
        let backend: Arc<dyn OutputCodePage> = backend;
        Self {
            backend: Some(backend),
            state: Mutex::new(OutputLeaseState {
                acquired: false,
                unicode: false,
                restore_to: None,
            }),
        }
    }

    /// Acquires UTF-8 output support once.
    ///
    /// The original page is recorded before the native switch so unwinding can
    /// restore it if the backend panics after applying the mutation.
    pub(crate) fn acquire(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.acquired {
            return;
        }
        let Some(backend) = &self.backend else {
            state.acquired = true;
            return;
        };
        if !matches!(backend.supports_unicode_glyphs(), Ok(true)) {
            state.acquired = true;
            return;
        }
        let Ok(code_page) = backend.output_code_page() else {
            state.acquired = true;
            return;
        };
        if code_page == CP_UTF8 {
            state.unicode = true;
            state.acquired = true;
            return;
        }

        state.restore_to = Some(code_page);
        if backend.set_output_code_page(CP_UTF8).is_ok() {
            state.unicode = true;
        } else {
            state.restore_to = None;
        }
        state.acquired = true;
    }

    /// Returns whether this lease proved UTF-8 output support.
    pub(crate) fn supports_unicode(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unicode
    }

    /// Restores the original output page, retaining failed work for retry.
    ///
    /// Returns `true` when no restore obligation remains. Concurrent acquisition
    /// and restore callers are serialized through the native write.
    pub(crate) fn restore(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(original) = state.restore_to else {
            return true;
        };
        let Some(backend) = &self.backend else {
            return false;
        };
        if backend.set_output_code_page(original).is_err() {
            return false;
        }
        state.restore_to = None;
        true
    }

    /// Returns whether no restore obligation remains.
    #[must_use]
    pub(crate) fn restore_obligation_cleared(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .restore_to
            .is_none()
    }
}

impl Drop for Utf8OutputLease {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Queries the output page and acquires a UTF-8 lease when possible.
///
/// Already-UTF-8 pages are left untouched. Capability, query, or switch
/// failure yields ASCII output and no restore obligation. An owned switch is
/// restored by [`Utf8OutputLease::restore`] or automatically during unwinding.
#[cfg(test)]
pub(crate) fn acquire_utf8_output<T>(backend: Arc<T>) -> Utf8OutputLease
where
    T: OutputCodePage + 'static,
{
    let lease = Utf8OutputLease::pending(backend);
    lease.acquire();
    lease
}

/// Process console output page. Compiled only for native Windows enter.
#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct NativeOutputCodePage;

#[cfg(windows)]
impl OutputCodePage for NativeOutputCodePage {
    fn supports_unicode_glyphs(&self) -> io::Result<bool> {
        use std::mem::size_of;
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Graphics::Gdi::TMPF_TRUETYPE;
        use windows_sys::Win32::System::Console::{
            CONSOLE_FONT_INFOEX, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode,
            GetCurrentConsoleFontEx, GetStdHandle, STD_OUTPUT_HANDLE,
        };

        // SAFETY: GetStdHandle takes a constant selector and returns a borrowed
        // process handle. No handle ownership is transferred.
        let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        if output == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        if output.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "standard output has no console handle",
            ));
        }

        let mut mode = 0;
        // SAFETY: output is a borrowed standard-output handle and mode points
        // to initialized writable storage for the duration of the call.
        let virtual_terminal = unsafe { GetConsoleMode(output, &mut mode) } != 0
            && mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0;

        let cb_size = u32::try_from(size_of::<CONSOLE_FONT_INFOEX>()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "console font structure size exceeds Win32 ULONG",
            )
        })?;
        let mut font = CONSOLE_FONT_INFOEX {
            cbSize: cb_size,
            ..CONSOLE_FONT_INFOEX::default()
        };
        // SAFETY: output is borrowed, font has the required cbSize, and its
        // writable storage remains valid for the duration of the call.
        if unsafe { GetCurrentConsoleFontEx(output, 0, &mut font) } != 0 {
            return Ok(font.FontFamily & u32::from(TMPF_TRUETYPE) != 0);
        }
        let font_error = io::Error::last_os_error();
        if virtual_terminal {
            Ok(true)
        } else {
            Err(font_error)
        }
    }

    fn output_code_page(&self) -> io::Result<u32> {
        use windows_sys::Win32::System::Console::GetConsoleOutputCP;

        // SAFETY: GetConsoleOutputCP takes no parameters and retains no
        // pointers. The documented failure value is 0; GetLastError is valid
        // only in that case. These APIs have no A/W variants.
        let code_page = unsafe { GetConsoleOutputCP() };
        if code_page == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(code_page)
        }
    }

    fn set_output_code_page(&self, code_page: u32) -> io::Result<()> {
        use windows_sys::Win32::System::Console::SetConsoleOutputCP;

        // SAFETY: SetConsoleOutputCP takes a UINT code-page id by value and
        // retains no pointers. The documented failure value is a zero BOOL;
        // GetLastError is valid only in that case. No input-page API is used.
        let succeeded = unsafe { SetConsoleOutputCP(code_page) };
        if succeeded == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

/// In-memory output code page for non-TTY tests.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct MockOutputCodePage {
    current: Mutex<u32>,
    gets: AtomicUsize,
    sets: Mutex<Vec<u32>>,
    unicode_glyphs: AtomicBool,
    fail_get: AtomicBool,
    fail_set: AtomicBool,
    fail_next_set: AtomicBool,
}

#[cfg(test)]
impl MockOutputCodePage {
    /// Creates a mock parked on `initial`.
    pub(crate) fn new(initial: u32) -> Arc<Self> {
        Arc::new(Self {
            current: Mutex::new(initial),
            gets: AtomicUsize::new(0),
            sets: Mutex::new(Vec::new()),
            unicode_glyphs: AtomicBool::new(true),
            fail_get: AtomicBool::new(false),
            fail_set: AtomicBool::new(false),
            fail_next_set: AtomicBool::new(false),
        })
    }

    /// Marks the output surface as unable to display Unicode glyphs.
    #[cfg(test)]
    pub(crate) fn disable_unicode_glyphs(&self) {
        self.unicode_glyphs.store(false, Ordering::SeqCst);
    }

    /// Makes the next get fail without changing the stored page.
    #[cfg(test)]
    pub(crate) fn fail_get(&self) {
        self.fail_get.store(true, Ordering::SeqCst);
    }

    /// Makes every set fail without changing the stored page.
    #[cfg(test)]
    pub(crate) fn fail_set(&self) {
        self.fail_set.store(true, Ordering::SeqCst);
    }

    /// Makes the next set fail without changing the stored page.
    #[cfg(test)]
    pub(crate) fn fail_next_set(&self) {
        self.fail_next_set.store(true, Ordering::SeqCst);
    }

    /// Returns how many times the page was queried.
    #[cfg(test)]
    pub(crate) fn get_count(&self) -> usize {
        self.gets.load(Ordering::SeqCst)
    }

    /// Returns every set identifier, including failed attempts.
    #[cfg(test)]
    pub(crate) fn set_calls(&self) -> Vec<u32> {
        lock_vec(&self.sets).clone()
    }

    /// Returns the page currently stored by the mock.
    pub(crate) fn current(&self) -> u32 {
        *lock_u32(&self.current)
    }
}

#[cfg(test)]
impl OutputCodePage for MockOutputCodePage {
    fn supports_unicode_glyphs(&self) -> io::Result<bool> {
        Ok(self.unicode_glyphs.load(Ordering::SeqCst))
    }

    fn output_code_page(&self) -> io::Result<u32> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        if self.fail_get.load(Ordering::SeqCst) {
            return Err(io::Error::other("mock GetConsoleOutputCP failed"));
        }
        Ok(self.current())
    }

    fn set_output_code_page(&self, code_page: u32) -> io::Result<()> {
        lock_vec(&self.sets).push(code_page);
        if self.fail_next_set.swap(false, Ordering::SeqCst) || self.fail_set.load(Ordering::SeqCst)
        {
            return Err(io::Error::other("mock SetConsoleOutputCP failed"));
        }
        *lock_u32(&self.current) = code_page;
        Ok(())
    }
}

#[cfg(test)]
fn lock_u32(mutex: &Mutex<u32>) -> std::sync::MutexGuard<'_, u32> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
fn lock_vec(mutex: &Mutex<Vec<u32>>) -> std::sync::MutexGuard<'_, Vec<u32>> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::{CP_UTF8, MockOutputCodePage, OutputCodePage, acquire_utf8_output};
    use std::io;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::{Arc, Condvar, Mutex, PoisonError};

    #[derive(Debug)]
    struct BlockingState {
        current: u32,
        restore_calls: usize,
        restore_started: bool,
        allow_restore: bool,
    }

    #[derive(Debug)]
    struct BlockingRestoreOutputCodePage {
        state: Mutex<BlockingState>,
        restore_started: Condvar,
        allow_restore: Condvar,
    }

    impl BlockingRestoreOutputCodePage {
        fn new(initial: u32) -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new(BlockingState {
                    current: initial,
                    restore_calls: 0,
                    restore_started: false,
                    allow_restore: false,
                }),
                restore_started: Condvar::new(),
                allow_restore: Condvar::new(),
            })
        }

        fn wait_for_restore(&self) {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            while !state.restore_started {
                state = self
                    .restore_started
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }

        fn unblock_restore(&self) {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.allow_restore = true;
            self.allow_restore.notify_one();
        }

        fn restore_calls(&self) -> usize {
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .restore_calls
        }
    }

    impl OutputCodePage for BlockingRestoreOutputCodePage {
        fn supports_unicode_glyphs(&self) -> io::Result<bool> {
            Ok(true)
        }

        fn output_code_page(&self) -> io::Result<u32> {
            Ok(self
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .current)
        }

        fn set_output_code_page(&self, code_page: u32) -> io::Result<()> {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if code_page != CP_UTF8 {
                state.restore_calls += 1;
                state.restore_started = true;
                self.restore_started.notify_one();
                while !state.allow_restore {
                    state = self
                        .allow_restore
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
            state.current = code_page;
            Ok(())
        }
    }

    #[test]
    fn already_utf8_does_not_issue_unnecessary_mutation() {
        let backend = MockOutputCodePage::new(CP_UTF8);
        let lease = acquire_utf8_output(Arc::clone(&backend));
        assert!(lease.supports_unicode());
        assert_eq!(backend.get_count(), 1);
        assert!(backend.set_calls().is_empty());
        lease.restore();
        assert!(backend.set_calls().is_empty());
        assert_eq!(backend.current(), CP_UTF8);
    }

    #[test]
    fn gbk_936_switch_records_the_original_page() {
        let backend = MockOutputCodePage::new(936);
        let lease = acquire_utf8_output(Arc::clone(&backend));
        assert!(lease.supports_unicode());
        assert_eq!(backend.set_calls(), vec![CP_UTF8]);
        assert_eq!(backend.current(), CP_UTF8);
        lease.restore();
        assert_eq!(backend.set_calls(), vec![CP_UTF8, 936]);
        assert_eq!(backend.current(), 936);
    }

    #[test]
    fn switch_failure_uses_ascii_and_owns_nothing() {
        let backend = MockOutputCodePage::new(936);
        backend.fail_set();
        let lease = acquire_utf8_output(Arc::clone(&backend));
        assert!(!lease.supports_unicode());
        assert_eq!(backend.set_calls(), vec![CP_UTF8]);
        assert_eq!(backend.current(), 936);
        lease.restore();
        assert_eq!(backend.set_calls(), vec![CP_UTF8]);
        assert_eq!(backend.current(), 936);
    }

    #[test]
    fn query_failure_uses_ascii_without_set() {
        let backend = MockOutputCodePage::new(936);
        backend.fail_get();
        let lease = acquire_utf8_output(Arc::clone(&backend));
        assert!(!lease.supports_unicode());
        assert_eq!(backend.get_count(), 1);
        assert!(backend.set_calls().is_empty());
        lease.restore();
        assert!(backend.set_calls().is_empty());
        assert_eq!(backend.current(), 936);
    }

    #[test]
    fn raster_font_uses_ascii_without_code_page_mutation() {
        let backend = MockOutputCodePage::new(936);
        backend.disable_unicode_glyphs();
        let lease = acquire_utf8_output(Arc::clone(&backend));
        assert!(!lease.supports_unicode());
        assert_eq!(backend.get_count(), 0);
        assert!(backend.set_calls().is_empty());
        assert_eq!(backend.current(), 936);
    }

    #[test]
    fn failed_restore_is_retried_on_drop() {
        let backend = MockOutputCodePage::new(936);
        {
            let lease = acquire_utf8_output(Arc::clone(&backend));
            backend.fail_next_set();
            assert!(!lease.restore());
            assert_eq!(backend.set_calls(), vec![CP_UTF8, 936]);
            assert_eq!(backend.current(), CP_UTF8);
        }
        assert_eq!(backend.set_calls(), vec![CP_UTF8, 936, 936]);
        assert_eq!(backend.current(), 936);
    }

    #[test]
    fn concurrent_restore_calls_issue_one_native_restore() {
        let backend = BlockingRestoreOutputCodePage::new(936);
        let lease = Arc::new(acquire_utf8_output(Arc::clone(&backend)));
        let first_lease = Arc::clone(&lease);
        let first = std::thread::spawn(move || first_lease.restore());
        backend.wait_for_restore();

        let second_lease = Arc::clone(&lease);
        let second = std::thread::spawn(move || second_lease.restore());
        backend.unblock_restore();

        assert!(first.join().expect("first restore must not panic"));
        assert!(second.join().expect("second restore must not panic"));
        assert_eq!(backend.restore_calls(), 1);
    }

    #[test]
    fn owned_switch_restores_during_unwind() {
        let backend = MockOutputCodePage::new(936);
        let unwind_backend = Arc::clone(&backend);
        let result = catch_unwind(AssertUnwindSafe(move || {
            let _lease = acquire_utf8_output(unwind_backend);
            panic!("panic after output code-page acquisition");
        }));
        assert!(result.is_err());
        assert_eq!(backend.set_calls(), vec![CP_UTF8, 936]);
        assert_eq!(backend.current(), 936);
    }
}
