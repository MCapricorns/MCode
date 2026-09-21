//! First-party session service facade.
//!
//! [`SessionService`] owns one publication generation: the actor runs on the
//! T8 typed task runtime, durable work is admitted through the Host
//! generation fence, and every facade call drives its operation to exactly
//! one terminal pull under a bounded deadline.
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use tokio::time::Instant;

use mycode_config::HomeLayout;

use crate::session::generation::{GenerationDomain, GenerationFence, HostGeneration};
use crate::session::runtime::{TaskActorClient, TaskActorError, TaskCloseSignal};

use super::actor::{SessionActor, SessionTaskError};
use super::dto::{
    AppendedResult, BranchMutationKind, BranchedResult, CreatedResult, EventKind,
    EventReservationView, EventsResult, HeadStamp, LoadedEvent, OpenedResult, SessionError,
    SessionRequest, SessionResult,
};
use super::ids::{BranchId, SessionCallId, SessionEventId, SessionId};

/// Default per-operation deadline.
const DEFAULT_DEADLINE: Duration = Duration::from_secs(30);

/// The first-party session service.
///
/// The service is cheap to construct and owns one serialized actor; clones
/// share one inner owner, so dropping an individual clone is harmless —
/// only the last drop retires the publication, closes live operations, and
/// aborts the worker. Use [`SessionService::shutdown`] to additionally
/// await quiescence before reclaiming the store.
#[derive(Clone)]
pub struct SessionService {
    inner: Arc<ServiceInner>,
}

/// Shared owner behind every `SessionService` clone.
struct ServiceInner {
    client: TaskActorClient<SessionActor>,
    fence: Arc<GenerationFence>,
}

impl Drop for ServiceInner {
    fn drop(&mut self) {
        self.fence.mark_retired();
        self.fence.close_publication();
    }
}

impl SessionService {
    /// Starts the service over one owned home without creating any object.
    #[must_use]
    pub fn new(home: &HomeLayout) -> Self {
        let fence = Arc::new(GenerationFence::new(
            Arc::new(AtomicU64::new(0)),
            GenerationDomain::Session,
            HostGeneration::new(1).expect("first generation is JSON-safe"),
        ));
        fence.mark_current();
        let actor = SessionActor::new(home.clone(), Arc::clone(&fence));
        let client = TaskActorClient::start(actor);
        Self {
            inner: Arc::new(ServiceInner { client, fence }),
        }
    }

    /// Returns the actor client.
    fn client(&self) -> &TaskActorClient<SessionActor> {
        &self.inner.client
    }

    /// Creates a fresh session with a Host-minted root branch.
    ///
    /// # Errors
    ///
    /// Returns the actor's terminal error; `Unavailable` when the storage
    /// substrate or the generation fence rejects the publication.
    pub async fn create(&self) -> Result<CreatedResult, SessionError> {
        match self.run(SessionRequest::Create).await? {
            SessionResult::Created(created) => Ok(created),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Recovers one session and returns every branch head.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::NotFound`] for an unknown session,
    /// [`SessionError::Corrupt`] when recovery fails validation, and the
    /// actor's terminal error otherwise.
    pub async fn open(&self, session: &SessionId) -> Result<OpenedResult, SessionError> {
        match self
            .run(SessionRequest::Open {
                session: session.clone(),
            })
            .await?
        {
            SessionResult::Opened(opened) => Ok(opened),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Validates one event, durably stages its payload, and issues the
    /// single-use reservation.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Limit`] for payload or session bounds,
    /// [`SessionError::InvalidArgument`] when the call/result ordering check
    /// rejects the event, and the actor's terminal error otherwise.
    pub async fn reserve_event(
        &self,
        session: &SessionId,
        branch: &BranchId,
        kind: EventKind,
        call_id: Option<SessionCallId>,
        payload: &[u8],
    ) -> Result<EventReservationView, SessionError> {
        match self
            .run(SessionRequest::ReserveEvent {
                session: session.clone(),
                branch: branch.clone(),
                kind,
                call_id,
                payload: payload.to_vec(),
            })
            .await?
        {
            SessionResult::ReservedEvent(view) => Ok(view),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Commits one reserved event under expected-head compare-and-swap.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Conflict`] with the actual head when the CAS
    /// lost, [`SessionError::NotFound`] for a missing or replayed
    /// reservation, and the actor's terminal error otherwise.
    pub async fn append(
        &self,
        session: &SessionId,
        branch: &BranchId,
        expected_head: &HeadStamp,
        reservation: &EventReservationView,
    ) -> Result<AppendedResult, SessionError> {
        match self
            .run(SessionRequest::Append {
                session: session.clone(),
                branch: branch.clone(),
                expected_head: expected_head.clone(),
                reservation: reservation.clone(),
            })
            .await?
        {
            SessionResult::Appended(appended) => Ok(appended),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Reads one bounded page of one immutable branch snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Limit`] for a page size outside `1..=256`,
    /// [`SessionError::InvalidArgument`] for stale or foreign cursors, and
    /// the actor's terminal error otherwise.
    pub async fn read(
        &self,
        session: &SessionId,
        branch: &BranchId,
        snapshot_head: &HeadStamp,
        after: Option<&SessionEventId>,
        limit: u16,
    ) -> Result<EventsResult, SessionError> {
        match self
            .run(SessionRequest::Read {
                session: session.clone(),
                branch: branch.clone(),
                snapshot_head: snapshot_head.clone(),
                after: after.cloned(),
                limit,
            })
            .await?
        {
            SessionResult::Events(events) => Ok(events),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Issues one single-use branch mutation reservation.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::NotFound`] for an unknown branch or target
    /// event, [`SessionError::Limit`] at the branch bound, and the actor's
    /// terminal error otherwise.
    pub async fn reserve_branch(
        &self,
        session: &SessionId,
        kind: BranchMutationKind,
        source_branch: &BranchId,
        target_event: &SessionEventId,
    ) -> Result<super::dto::BranchReservationView, SessionError> {
        match self
            .run(SessionRequest::ReserveBranch {
                session: session.clone(),
                kind,
                source_branch: source_branch.clone(),
                target_event: target_event.clone(),
            })
            .await?
        {
            SessionResult::ReservedBranch(view) => Ok(view),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Consumes one fork reservation under source-head compare-and-swap.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Conflict`] when the source head moved,
    /// [`SessionError::InvalidArgument`] for a crossed reservation, and the
    /// actor's terminal error otherwise.
    pub async fn fork(
        &self,
        session: &SessionId,
        from_branch: &BranchId,
        at_event: &SessionEventId,
        reservation: &super::dto::BranchReservationView,
    ) -> Result<BranchedResult, SessionError> {
        match self
            .run(SessionRequest::Fork {
                session: session.clone(),
                from_branch: from_branch.clone(),
                at_event: at_event.clone(),
                reservation: reservation.clone(),
            })
            .await?
        {
            SessionResult::Branched(branched) => Ok(branched),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Consumes one rewind reservation under source-head compare-and-swap.
    ///
    /// # Errors
    ///
    /// Mirrors [`SessionService::fork`].
    pub async fn rewind(
        &self,
        session: &SessionId,
        branch: &BranchId,
        to_event: &SessionEventId,
        reservation: &super::dto::BranchReservationView,
    ) -> Result<BranchedResult, SessionError> {
        match self
            .run(SessionRequest::Rewind {
                session: session.clone(),
                branch: branch.clone(),
                to_event: to_event.clone(),
                reservation: reservation.clone(),
            })
            .await?
        {
            SessionResult::Branched(branched) => Ok(branched),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Loads one committed event with its digest-verified payload.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::NotFound`] for an unknown event and the
    /// actor's terminal error otherwise.
    pub async fn load_event(
        &self,
        session: &SessionId,
        branch: &BranchId,
        event: &SessionEventId,
    ) -> Result<LoadedEvent, SessionError> {
        match self
            .run(SessionRequest::LoadEvent {
                session: session.clone(),
                branch: branch.clone(),
                event: event.clone(),
            })
            .await?
        {
            SessionResult::Loaded(loaded) => Ok(loaded),
            _ => Err(SessionError::Unavailable),
        }
    }

    /// Retires the publication, closes live operations, and awaits drain.
    ///
    /// After this call the service rejects every further operation; other
    /// clones observe the same retirement because the fence is shared.
    pub async fn shutdown(self) {
        self.inner.fence.mark_retired();
        self.inner.client.shutdown();
        self.inner.fence.wait_drained().await;
        self.inner.fence.close_publication();
    }

    async fn run(&self, request: SessionRequest) -> Result<SessionResult, SessionError> {
        let close = TaskCloseSignal::new();
        let deadline = Instant::now() + DEFAULT_DEADLINE;
        let operation = self
            .client()
            .invoke(request, deadline, close)
            .await
            .map_err(map_task_error)?;
        loop {
            // Refresh the deadline per pull: recovery replays in 1 MiB
            // chunks, so a large session needs many sequential pulls and
            // each one gets its own full window instead of sharing the
            // invoke's budget.
            let deadline = Instant::now() + DEFAULT_DEADLINE;
            match self.client().pull(operation, deadline).await {
                Ok(pull) => match pull {
                    super::dto::SessionPull::Complete(result) => {
                        self.client().close(operation);
                        return Ok(result);
                    }
                    super::dto::SessionPull::Failed(error) => {
                        self.client().close(operation);
                        return Err(error);
                    }
                    super::dto::SessionPull::Progress(_) => {
                        continue;
                    }
                },
                Err(error) => {
                    self.client().close(operation);
                    return Err(map_task_error(error));
                }
            }
        }
    }
}

fn map_task_error(error: TaskActorError<SessionTaskError>) -> SessionError {
    match error {
        TaskActorError::UnknownOperation | TaskActorError::Unavailable => SessionError::Unavailable,
        TaskActorError::Cancelled => SessionError::Cancelled,
        TaskActorError::Pack(SessionTaskError::Admission) => SessionError::Limit,
        TaskActorError::Pack(SessionTaskError::Storage) => SessionError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use mycode_config::HomeLayout;

    use super::super::dto::{EventKind, HeadStamp};
    use super::super::ids::SessionCallId;
    use super::super::store::SessionPaths;
    use super::{SessionError, SessionService};

    fn home() -> (tempfile::TempDir, HomeLayout) {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        (parent, layout)
    }

    #[tokio::test]
    async fn dropped_clone_keeps_the_service_available() {
        let (_parent, layout) = home();
        let service = SessionService::new(&layout);
        drop(service.clone());
        service.create().await.expect("session created");
    }

    #[tokio::test]
    async fn shutdown_retires_every_clone() {
        let (_parent, layout) = home();
        let service = SessionService::new(&layout);
        let survivor = service.clone();
        service.shutdown().await;
        let error = survivor.create().await.expect_err("shutdown rejects");
        assert_eq!(error, SessionError::Unavailable);
    }

    /// Regression: recovery must carry the open tool-call set into the
    /// assembled branch, so a ToolResult for a ToolCall committed before a
    /// restart passes the ordering check instead of failing with
    /// `InvalidArgument`.
    #[tokio::test]
    async fn recovered_open_tool_call_accepts_its_tool_result() {
        let (_parent, layout) = home();
        let call = SessionCallId::parse("call1-0123456789abcdef0123456789abcde1").expect("call id");
        let (session, branch, head) = {
            let service = SessionService::new(&layout);
            let created = service.create().await.expect("session created");
            let reservation = service
                .reserve_event(
                    &created.session_id,
                    &created.branch_id,
                    EventKind::ToolCall,
                    Some(call.clone()),
                    b"tool call payload",
                )
                .await
                .expect("tool call reserved");
            let appended = service
                .append(
                    &created.session_id,
                    &created.branch_id,
                    &HeadStamp::Empty,
                    &reservation,
                )
                .await
                .expect("tool call committed");
            service.shutdown().await;
            (created.session_id, created.branch_id, appended.head)
        };

        // A fresh service recovers the ledger from disk; the recovered
        // branch must still hold the call open for its result.
        let service = SessionService::new(&layout);
        service.open(&session).await.expect("session recovered");
        let reservation = service
            .reserve_event(
                &session,
                &branch,
                EventKind::ToolResult,
                Some(call),
                b"tool result payload",
            )
            .await
            .expect("recovered ledger accepts the tool result");
        service
            .append(&session, &branch, &head, &reservation)
            .await
            .expect("tool result committed");
        service.shutdown().await;
    }

    /// Regression: a committed length that is not record-aligned is
    /// corruption for that session, not a storage failure that retires the
    /// worker for everyone.
    #[tokio::test]
    async fn misaligned_committed_length_is_corrupt_not_unavailable() {
        let (_parent, layout) = home();
        let (session, branch) = {
            let service = SessionService::new(&layout);
            let created = service.create().await.expect("session created");
            let reservation = service
                .reserve_event(
                    &created.session_id,
                    &created.branch_id,
                    EventKind::Message,
                    None,
                    b"payload",
                )
                .await
                .expect("event reserved");
            service
                .append(
                    &created.session_id,
                    &created.branch_id,
                    &HeadStamp::Empty,
                    &reservation,
                )
                .await
                .expect("event committed");
            service.shutdown().await;
            (created.session_id, created.branch_id)
        };

        // Pad the log and bump the manifest's committed length by two bytes
        // so the committed prefix ends inside the frame header.
        let paths = SessionPaths::new(&layout, &session);
        let log_path = layout
            .owned_join(paths.branch_events(&branch))
            .expect("log path");
        let mut log_bytes = std::fs::read(&log_path).expect("branch log readable");
        log_bytes.extend_from_slice(b"xx");
        std::fs::write(&log_path, &log_bytes).expect("branch log padded");
        let manifest_path = layout.owned_join(paths.manifest()).expect("manifest path");
        let mut manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&manifest_path).expect("manifest readable"),
        )
        .expect("manifest json");
        let committed = manifest["branches"][0]["committedBytes"]
            .as_u64()
            .expect("committed bytes");
        manifest["branches"][0]["committedBytes"] = serde_json::Value::from(committed + 2);
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&manifest).expect("manifest encodable"),
        )
        .expect("manifest rewritten");

        let service = SessionService::new(&layout);
        let error = service
            .open(&session)
            .await
            .expect_err("misaligned committed length fails closed");
        assert_eq!(
            error,
            SessionError::Corrupt,
            "misclassification would brick the worker as Storage"
        );
        // The worker survives the corruption and still serves others.
        service
            .create()
            .await
            .expect("worker still serves fresh sessions");
        service.shutdown().await;
    }

    /// Regression: replay verification is chunked per pull, so a session
    /// larger than one verification budget recovers across several pulls,
    /// each with its own fresh deadline window.
    #[tokio::test]
    async fn large_session_recovers_across_chunked_pulls() {
        let (_parent, layout) = home();
        let payload = vec![b'a'; 600 * 1024];
        let (session, branch, head) = {
            let service = SessionService::new(&layout);
            let created = service.create().await.expect("session created");
            let mut head = HeadStamp::Empty;
            for _ in 0..3 {
                let reservation = service
                    .reserve_event(
                        &created.session_id,
                        &created.branch_id,
                        EventKind::Message,
                        None,
                        &payload,
                    )
                    .await
                    .expect("event reserved");
                let appended = service
                    .append(&created.session_id, &created.branch_id, &head, &reservation)
                    .await
                    .expect("event committed");
                head = appended.head;
            }
            service.shutdown().await;
            (created.session_id, created.branch_id, head)
        };

        // Roughly 1.8 MiB of committed records: verification needs more
        // than one 1 MiB pull before the open action can run.
        let service = SessionService::new(&layout);
        let opened = service
            .open(&session)
            .await
            .expect("multi-pull recovery completes");
        assert_eq!(opened.heads.len(), 1);
        assert_eq!(opened.heads[0].branch_id, branch);
        assert_eq!(opened.heads[0].head, head);
        service.shutdown().await;
    }
}
