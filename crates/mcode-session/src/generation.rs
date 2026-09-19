//! Host-owned generation fence for atomic Pack set publication.
//!
//! One fence owns one family's generation lifecycle. Preparation cannot admit
//! Host work. Publication and retirement use one atomic phase-plus-count state
//! so retirement linearizes with every activity reservation before quiescent
//! Store cleanup.
//!
//! The shared publication state is a monotonically increasing epoch: even
//! values mark a stable published authority, odd values mark a publication
//! transition in progress, and [`PUBLICATION_CLOSED`] marks the authority as
//! finally closed. The owning publisher is the only writer. Closing the
//! publication only rejects new admissions; every fence must additionally be
//! retired (and drained via [`GenerationFence::wait_drained`]) before its
//! Store ownership is reclaimed.
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as SyncMutex, MutexGuard};

use mcode_config::PluginFamily;
use tokio::sync::Notify;

const MAX_HOST_GENERATION: u64 = 9_007_199_254_740_991;

/// Identifies one Host generation of one family's active Pack set.
///
/// Values start at one. Zero is the reserved [`HostGeneration::ABSENT`]
/// binding used before any generation has been published.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct HostGeneration(u64);

impl HostGeneration {
    /// Creates one generation value in the exact JSON-safe positive range.
    pub(crate) const fn new(value: u64) -> Option<Self> {
        if value == 0 || value > MAX_HOST_GENERATION {
            None
        } else {
            Some(Self(value))
        }
    }

    /// Returns the exact generation value.
    #[expect(dead_code, reason = "T16+ multi-domain fences read the value")]
    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

/// Marks a publication authority as finally closed.
pub(crate) const PUBLICATION_CLOSED: u64 = u64::MAX;

/// Names the authority one generation fence gates.
///
/// External Pack families are gated by their plugin family; first-party
/// built-in services are gated by their dedicated fixed domain so a built-in
/// generation can never be confused with an external Pack publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GenerationDomain {
    /// One external Plugin family's active Pack set.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "T10+ publishes family Pack generations")
    )]
    Family(PluginFamily),
    /// The first-party built-in Session service publication.
    Session,
}

pub(crate) struct GenerationFence {
    publication_state: Arc<AtomicU64>,
    domain: GenerationDomain,
    generation: HostGeneration,
    state: AtomicUsize,
    drained: Notify,
    commit: SyncMutex<()>,
}

// The low two bits carry the phase so retirement and activity admission share
// one CAS linearization point. All remaining bits carry the activity count.
const GENERATION_PHASE_MASK: usize = 0b11;
const GENERATION_PREPARING: usize = 0;
const GENERATION_CURRENT: usize = 1;
const GENERATION_RETIRED: usize = 2;
const GENERATION_ACTIVITY_INCREMENT: usize = GENERATION_PHASE_MASK + 1;
const MAX_GENERATION_ACTIVITIES: usize = usize::MAX >> 2;

impl GenerationFence {
    pub(crate) fn new(
        publication_state: Arc<AtomicU64>,
        domain: GenerationDomain,
        generation: HostGeneration,
    ) -> Self {
        Self {
            publication_state,
            domain,
            generation,
            state: AtomicUsize::new(GENERATION_PREPARING),
            drained: Notify::new(),
            commit: SyncMutex::new(()),
        }
    }

    /// Returns the authority domain this fence gates.
    #[expect(dead_code, reason = "T16+ multi-domain fences distinguish authorities")]
    pub(crate) const fn domain(&self) -> GenerationDomain {
        self.domain
    }

    /// Returns the generation this fence gates.
    #[expect(dead_code, reason = "T16+ multi-domain fences distinguish authorities")]
    pub(crate) const fn generation(&self) -> HostGeneration {
        self.generation
    }

    /// Returns whether the publication authority is finally closed.
    pub(crate) fn publication_closed(&self) -> bool {
        self.publication_state.load(Ordering::SeqCst) == PUBLICATION_CLOSED
    }

    /// Closes the shared publication authority forever.
    pub(crate) fn close_publication(&self) {
        self.publication_state
            .store(PUBLICATION_CLOSED, Ordering::SeqCst);
    }

    pub(crate) fn enter(self: &Arc<Self>) -> Option<GenerationActivity> {
        let publication_epoch = self.publication_state.load(Ordering::SeqCst);
        if publication_epoch == PUBLICATION_CLOSED || !publication_epoch.is_multiple_of(2) {
            return None;
        }
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if generation_phase(state) != GENERATION_CURRENT
                || generation_activity_count(state) == MAX_GENERATION_ACTIVITIES
            {
                return None;
            }
            match self.state.compare_exchange_weak(
                state,
                state + GENERATION_ACTIVITY_INCREMENT,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => state = observed,
            }
        }
        if self.publication_state.load(Ordering::SeqCst) != publication_epoch {
            self.release();
            return None;
        }
        Some(GenerationActivity {
            fence: Arc::clone(self),
        })
    }

    pub(crate) fn mark_current(&self) {
        self.state
            .compare_exchange(
                GENERATION_PREPARING,
                GENERATION_CURRENT,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .expect("a generation fence becomes current exactly once");
    }

    pub(crate) fn mark_retired(&self) {
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            match generation_phase(state) {
                GENERATION_RETIRED => return,
                GENERATION_PREPARING | GENERATION_CURRENT => {}
                _ => unreachable!("generation fence phase uses two frozen bits"),
            }
            let retired = (state & !GENERATION_PHASE_MASK) | GENERATION_RETIRED;
            match self.state.compare_exchange_weak(
                state,
                retired,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(observed) => state = observed,
            }
        }
    }

    /// Waits until every admitted activity has released its reservation.
    ///
    /// Retirement only rejects new admissions; consumers must await this
    /// quiescence point before reclaiming the generation's Store ownership.
    pub(crate) async fn wait_drained(&self) {
        loop {
            let notified = self.drained.notified();
            if generation_activity_count(self.state.load(Ordering::Acquire)) == 0 {
                return;
            }
            notified.await;
        }
    }

    fn release(&self) {
        let previous = self
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (generation_activity_count(active) > 0)
                    .then(|| active - GENERATION_ACTIVITY_INCREMENT)
            })
            .expect("a generation activity releases its reservation exactly once");
        if generation_activity_count(previous) == 1 {
            self.drained.notify_one();
        }
    }
}

const fn generation_phase(state: usize) -> usize {
    state & GENERATION_PHASE_MASK
}

const fn generation_activity_count(state: usize) -> usize {
    state >> 2
}

/// One admitted activity reservation on a current generation fence.
pub(crate) struct GenerationActivity {
    fence: Arc<GenerationFence>,
}

impl GenerationActivity {
    /// Begins one exclusive commit window on the still-current generation.
    ///
    /// The returned guard linearizes with retirement: a fence cannot retire
    /// while the commit window is open.
    pub(crate) fn begin_commit(&self) -> Result<GenerationCommit<'_>, GenerationCommitError> {
        let commit = self
            .fence
            .commit
            .lock()
            .map_err(|_| GenerationCommitError::Unavailable)?;
        if self.fence.publication_closed()
            || generation_phase(self.fence.state.load(Ordering::Acquire)) != GENERATION_CURRENT
        {
            return Err(GenerationCommitError::Stale);
        }
        Ok(GenerationCommit { _commit: commit })
    }
}

/// Holds one generation's exclusive commit window.
pub(crate) struct GenerationCommit<'a> {
    _commit: MutexGuard<'a, ()>,
}

/// Reports why a generation commit window could not open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GenerationCommitError {
    /// The generation retired before the commit began.
    Stale,
    /// The commit mutex was poisoned.
    Unavailable,
}

impl Drop for GenerationActivity {
    fn drop(&mut self) {
        self.fence.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fence(publication: u64) -> (Arc<AtomicU64>, Arc<GenerationFence>) {
        let publication_state = Arc::new(AtomicU64::new(publication));
        let fence = Arc::new(GenerationFence::new(
            Arc::clone(&publication_state),
            GenerationDomain::Family(PluginFamily::Providers),
            HostGeneration::new(1).expect("nonzero generation"),
        ));
        (publication_state, fence)
    }

    #[test]
    fn preparing_fence_admits_no_activity() {
        let (_publication, fence) = fence(0);
        assert!(fence.enter().is_none());
    }

    #[test]
    fn in_progress_publication_admits_no_activity() {
        let (_publication, fence) = fence(1);
        fence.mark_current();
        assert!(fence.enter().is_none());
    }

    #[test]
    fn closed_publication_admits_no_activity() {
        let (_publication, fence) = fence(0);
        fence.mark_current();
        fence.close_publication();
        assert!(fence.enter().is_none());
    }

    #[test]
    fn retired_fence_rejects_entry_and_commit() {
        let (_publication, fence) = fence(0);
        fence.mark_current();
        let activity = fence.enter().expect("current fence admits");
        fence.mark_retired();
        assert!(fence.enter().is_none());
        assert_eq!(
            activity.begin_commit().err(),
            Some(GenerationCommitError::Stale),
            "retired fence is stale"
        );
    }

    #[test]
    fn closed_publication_stales_pending_commit() {
        let (_publication, fence) = fence(0);
        fence.mark_current();
        let activity = fence.enter().expect("current fence admits");
        fence.close_publication();
        assert_eq!(
            activity.begin_commit().err(),
            Some(GenerationCommitError::Stale),
            "closed publication is stale"
        );
    }

    #[test]
    fn commit_window_linearizes_with_retirement() {
        let (_publication, fence) = fence(0);
        fence.mark_current();
        let activity = fence.enter().expect("current fence admits");
        let commit = activity.begin_commit().unwrap();
        let retired = std::thread::spawn({
            let fence = Arc::clone(&fence);
            move || {
                fence.mark_retired();
            }
        });
        drop(commit);
        retired.join().expect("retirement completes");
        assert!(fence.enter().is_none());
    }

    #[tokio::test]
    async fn wait_drained_observes_quiescence_after_retirement() {
        let (_publication, fence) = fence(0);
        fence.mark_current();
        let first = fence.enter().expect("first activity");
        let second = fence.enter().expect("second activity");
        fence.mark_retired();
        let drained = tokio::spawn({
            let fence = Arc::clone(&fence);
            async move { fence.wait_drained().await }
        });
        drop(first);
        drop(second);
        drained.await.expect("drain observation completes");
    }

    #[test]
    fn concurrent_admission_and_release_returns_count_to_zero() {
        let (_publication, fence) = fence(0);
        fence.mark_current();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..1_000 {
                        drop(fence.enter().expect("current fence admits"));
                    }
                });
            }
        });
        assert_eq!(
            generation_activity_count(fence.state.load(Ordering::Acquire)),
            0
        );
    }
}
