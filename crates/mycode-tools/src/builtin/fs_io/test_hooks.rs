//! Process-global test observers and fault injection for the write kernel.
#[cfg(all(test, unix))]
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Mutex;

use super::*;

#[cfg(test)]
type TempLinksHook = std::sync::Arc<dyn Fn() + Send + Sync>;

/// Test-only observer invoked after payload and probe names are linked.
#[cfg(test)]
static TEMP_LINKS_HOOK: Mutex<Option<TempLinksHook>> = Mutex::new(None);

/// Test-only observer invoked at the final pre-publish cancel gate.
///
/// The hook receives the path key of the target being written so a test can
/// act only on its own write while other tests run concurrently
/// in the same process.
#[cfg(test)]
static PRE_PUBLISH_HOOK: Mutex<Option<PublishHook>> = Mutex::new(None);

/// Test-only observer invoked after publish, before verification.
///
/// The hook receives the path key of the published target so a test can act
/// only on its own write while other tests run concurrently.
#[cfg(test)]
static POST_PUBLISH_HOOK: Mutex<Option<PublishHook>> = Mutex::new(None);

#[cfg(test)]
type PublishHook = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

/// Clears the installed test hook when its fixture leaves scope.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct TempLinksHookGuard;

#[cfg(test)]
impl Drop for TempLinksHookGuard {
    fn drop(&mut self) {
        if let Ok(mut hook) = TEMP_LINKS_HOOK.lock() {
            *hook = None;
        }
    }
}

/// Installs one test observer for the pre-write linked-name boundary.
#[cfg(test)]
pub(crate) fn install_temp_links_hook(hook: TempLinksHook) -> TempLinksHookGuard {
    let mut slot = TEMP_LINKS_HOOK
        .lock()
        .expect("temporary link test hook lock must not be poisoned");
    assert!(slot.replace(hook).is_none(), "test hook already installed");
    TempLinksHookGuard
}

#[cfg(test)]
pub(super) fn run_temp_links_hook() {
    let hook = TEMP_LINKS_HOOK
        .lock()
        .expect("temporary link test hook lock must not be poisoned")
        .clone();
    if let Some(hook) = hook {
        hook();
    }
}

/// Clears the installed pre-publish test hook when its fixture leaves scope.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct PrePublishHookGuard;

#[cfg(test)]
impl Drop for PrePublishHookGuard {
    fn drop(&mut self) {
        if let Ok(mut hook) = PRE_PUBLISH_HOOK.lock() {
            *hook = None;
        }
    }
}

/// Serializes tests that install the process-global pre-publish hook.
#[cfg(test)]
pub(crate) fn serialize_pre_publish_tests() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Installs one test observer for the final pre-publish cancel gate.
#[cfg(test)]
pub(crate) fn install_pre_publish_hook(hook: PublishHook) -> PrePublishHookGuard {
    let mut slot = PRE_PUBLISH_HOOK
        .lock()
        .expect("pre-publish test hook lock must not be poisoned");
    assert!(slot.replace(hook).is_none(), "test hook already installed");
    PrePublishHookGuard
}

#[cfg(test)]
pub(super) fn run_pre_publish_hook(key: &str) {
    let hook = PRE_PUBLISH_HOOK
        .lock()
        .expect("pre-publish test hook lock must not be poisoned")
        .clone();
    if let Some(hook) = hook {
        hook(key);
    }
}

/// Clears the installed post-publish test hook when its fixture leaves scope.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct PostPublishHookGuard;

#[cfg(test)]
impl Drop for PostPublishHookGuard {
    fn drop(&mut self) {
        if let Ok(mut hook) = POST_PUBLISH_HOOK.lock() {
            *hook = None;
        }
    }
}

/// Installs one test observer for the post-publish verification boundary.
#[cfg(test)]
pub(crate) fn install_post_publish_hook(hook: PublishHook) -> PostPublishHookGuard {
    let mut slot = POST_PUBLISH_HOOK
        .lock()
        .expect("post-publish test hook lock must not be poisoned");
    assert!(slot.replace(hook).is_none(), "test hook already installed");
    PostPublishHookGuard
}

#[cfg(test)]
pub(super) fn run_post_publish_hook(key: &str) {
    let hook = POST_PUBLISH_HOOK
        .lock()
        .expect("post-publish test hook lock must not be poisoned")
        .clone();
    if let Some(hook) = hook {
        hook(key);
    }
}

/// Test-only unlink fault: `unlink_child` fails for a matching parent
/// directory identity (optionally one exact name) instead of calling the
/// kernel. The fault is keyed to a single directory so concurrently
/// running tests that write elsewhere are unaffected.
#[cfg(all(test, unix))]
struct UnlinkFault {
    dir: FileIdentity,
    name: Option<OsString>,
}

#[cfg(all(test, unix))]
static UNLINK_FAULT: Mutex<Option<UnlinkFault>> = Mutex::new(None);

/// Serializes unlink-fault fixtures: the injected fault is process-global, so
/// a second concurrent install would trip the misuse assert instead of
/// testing its own cleanup path.
#[cfg(all(test, unix))]
static UNLINK_FAULT_SERIAL: Mutex<()> = Mutex::new(());

/// Clears the installed unlink fault when its fixture leaves scope.
#[cfg(all(test, unix))]
pub(crate) struct UnlinkFaultGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
}

#[cfg(all(test, unix))]
impl Drop for UnlinkFaultGuard {
    fn drop(&mut self) {
        *lock_unlink_fault() = None;
    }
}

/// Locks the injected-fault slot, recovering from a poisoned mutex: the slot
/// holds one `Option`, so a panic that left it poisoned must not turn every
/// later unlink into a second panic.
#[cfg(all(test, unix))]
fn lock_unlink_fault() -> std::sync::MutexGuard<'static, Option<UnlinkFault>> {
    UNLINK_FAULT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Installs a test fault that fails unlinks under `dir` (all names, or one
/// exact `name`), so cleanup failure handling can be exercised
/// deterministically.
///
/// # Errors
///
/// Returns an I/O error when `dir` cannot be opened or stat.
#[cfg(all(test, unix))]
pub(crate) fn install_unlink_fault_under(
    dir: &Path,
    name: Option<&OsStr>,
) -> io::Result<UnlinkFaultGuard> {
    let serial = UNLINK_FAULT_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = sys::open_allowed_root(dir)?;
    let meta = sys::current_meta(&root)?;
    let mut fault = lock_unlink_fault();
    assert!(fault.is_none(), "an unlink fault is already installed");
    *fault = Some(UnlinkFault {
        dir: meta.identity,
        name: name.map(OsStr::to_os_string),
    });
    drop(fault);
    Ok(UnlinkFaultGuard { _serial: serial })
}

/// Returns the injected failure when `parent`/`name` match the fault.
#[cfg(all(test, unix))]
pub(super) fn unlink_fault(parent: &File, name: &OsStr) -> Option<io::Error> {
    let fault = lock_unlink_fault();
    let fault = fault.as_ref()?;
    if fault.name.as_ref().is_some_and(|want| want != name) {
        return None;
    }
    let meta = sys::current_meta(parent).ok()?;
    (meta.identity == fault.dir).then(|| io::Error::other("injected mycode unlink failure"))
}

/// Test-only Windows delete fault: `mark_delete` fails for temps created
/// under one parent directory identity so concurrently running tests that
/// write elsewhere are unaffected.
#[cfg(all(test, windows))]
struct DeleteFault {
    dir: FileIdentity,
    temps: Vec<FileIdentity>,
}

#[cfg(all(test, windows))]
static DELETE_FAULT: Mutex<Option<DeleteFault>> = Mutex::new(None);

/// Clears the installed delete fault when its fixture leaves scope.
#[cfg(all(test, windows))]
#[derive(Debug)]
pub(crate) struct DeleteFaultGuard;

#[cfg(all(test, windows))]
impl Drop for DeleteFaultGuard {
    fn drop(&mut self) {
        if let Ok(mut fault) = DELETE_FAULT.lock() {
            *fault = None;
        }
    }
}

/// Installs a test fault that fails `mark_delete` for temps created under
/// `dir`.
///
/// # Errors
///
/// Returns an I/O error when `dir` cannot be opened or stat.
#[cfg(all(test, windows))]
pub(crate) fn install_delete_fault_under(dir: &Path) -> io::Result<DeleteFaultGuard> {
    let root = sys::open_allowed_root(dir)?;
    let meta = sys::current_meta(&root)?;
    let mut fault = DELETE_FAULT
        .lock()
        .expect("delete fault lock must not be poisoned");
    assert!(fault.is_none(), "a delete fault is already installed");
    *fault = Some(DeleteFault {
        dir: meta.identity,
        temps: Vec::new(),
    });
    Ok(DeleteFaultGuard)
}

/// Records a just-created temp so a matching delete fault can target it.
#[cfg(all(test, windows))]
pub(super) fn note_delete_fault_temp(parent: &File, file: &File) {
    let Ok(mut fault) = DELETE_FAULT.lock() else {
        return;
    };
    let Some(fault) = fault.as_mut() else {
        return;
    };
    let Ok(parent_meta) = sys::current_meta(parent) else {
        return;
    };
    if parent_meta.identity != fault.dir {
        return;
    }
    if let Ok(meta) = sys::current_meta(file) {
        fault.temps.push(meta.identity);
    }
}

/// Returns the injected failure when `file` was created under the faulted
/// directory.
#[cfg(all(test, windows))]
pub(super) fn delete_fault(file: &File) -> Option<io::Error> {
    let fault = DELETE_FAULT
        .lock()
        .expect("delete fault lock must not be poisoned");
    let fault = fault.as_ref()?;
    let meta = sys::current_meta(file).ok()?;
    fault
        .temps
        .contains(&meta.identity)
        .then(|| io::Error::other("injected mycode delete failure"))
}
