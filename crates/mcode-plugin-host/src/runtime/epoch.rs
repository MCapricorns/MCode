//! Private monotonic epoch interruption and deadline policy.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use wasmtime::{Engine, Store};

use super::RuntimeError;

// Fifty 10 ms epochs keep the nominal ceiling below the required two-second
// maximum while leaving scheduler headroom. Delayed ticks never charge time
// that elapsed before a newly armed segment.
const EPOCH_INTERVAL: Duration = Duration::from_millis(10);
const GUEST_DEADLINE_TICKS: u64 = 50;

pub(super) struct EpochTicker {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl EpochTicker {
    pub(super) fn start(engine: Engine) -> Result<Self, RuntimeError> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("mcode-wasmtime-epoch".into())
            .spawn(move || tick_epochs(&engine, &worker_stop))
            .map_err(|_| RuntimeError::EpochTicker)?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

pub(super) fn arm_guest_deadline<T>(store: &mut Store<T>) {
    store.set_epoch_deadline(GUEST_DEADLINE_TICKS);
}

pub(super) fn park_guest_deadline<T>(store: &mut Store<T>) {
    // An idle Store executes no guest code. Zero makes any unguarded re-entry
    // trap immediately and avoids `current_epoch + u64::MAX` overflow.
    store.set_epoch_deadline(0);
}

fn tick_epochs(engine: &Engine, stop: &AtomicBool) {
    loop {
        let Some(next_tick) = Instant::now().checked_add(EPOCH_INTERVAL) else {
            return;
        };
        loop {
            if stop.load(Ordering::Acquire) {
                return;
            }
            let now = Instant::now();
            if now >= next_tick {
                break;
            }
            thread::park_timeout(next_tick.duration_since(now));
        }
        engine.increment_epoch();
    }
}
