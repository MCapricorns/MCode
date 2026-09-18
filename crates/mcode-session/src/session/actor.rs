//! Serialized first-party session actor over the typed task runtime.
//!
//! [`SessionActor`] implements [`PackTaskActor`]: every request is admitted
//! through the Host admission ledger, and durable effects run inside the
//! generation fence with manifest commits taken under the fence's exclusive
//! commit window. Recovery runs chunked per pull and reports the frozen
//! `recovering`/`replaying` progress phases before the bound action runs.
use std::collections::HashMap;
use std::sync::Arc;

use mcode_config::{
    ConfigError, HomeLayout, ensure_owned_directory, locked_update_owned_file, read_owned_file,
};

use crate::generation::GenerationFence;
use crate::runtime::admission::AdmissionLedger;
use crate::runtime::{AdmissionError, PackTaskActor, TaskOperationAdmission};

use super::digest::{
    BranchMutationDigestInput, branch_mutation_digest, format_digest, is_canonical_digest,
    payload_digest,
};
use super::dto::{
    AppendedResult, BranchMutationKind, BranchedResult, ConflictResult, CreatedResult, EventKind,
    EventReservationView, HeadStamp, LoadedEvent, MAX_BRANCHES, MAX_READ_LIMIT, OpenedResult,
    SessionError, SessionProgress, SessionPull, SessionRequest, SessionResult,
};
use super::fs;
use super::ids::{BranchId, BranchReservationId, SessionCallId, SessionEventId, SessionId};
use super::ledger::{
    BranchLedger, BranchReservationRow, EventMeta, EventReservationRow, SessionLedger,
};
use super::store::{
    self, MANIFEST_FORMAT_VERSION, MANIFEST_KIND, MAX_MANIFEST_BYTES, MAX_SESSION_TOTAL_BYTES,
    ManifestBranchFile, ManifestFile, ParentageFile, SessionPaths,
};

/// Byte budget of replay verification per pull.
const VERIFY_BUDGET_BYTES: u64 = 1024 * 1024;

/// Infrastructure-level actor failures; domain failures travel inside
/// [`SessionPull::Failed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionTaskError {
    /// The Host admission ledger is saturated.
    Admission,
    /// The storage substrate failed; the actor is permanently unavailable.
    Storage,
}

/// The serialized session owner loop.
pub(crate) struct SessionActor {
    home: HomeLayout,
    fence: Arc<GenerationFence>,
    admission: AdmissionLedger,
    sessions: HashMap<SessionId, SessionLedger>,
}

/// One admitted session operation.
pub(crate) struct SessionOperation {
    admission: Option<TaskOperationAdmission>,
    stage: Stage,
}

enum Stage {
    /// The operation failed with a domain error.
    Failed(SessionError),
    /// The operation completed; repeated pulls replay the terminal.
    Terminal(SessionPull),
    /// Recovery is running before the bound action.
    Load(LoadState),
    /// The action is ready to run.
    Run(Action),
}

enum Action {
    Create {
        session: SessionId,
        branch: BranchId,
    },
    Heads {
        session: SessionId,
    },
    ReserveEvent {
        session: SessionId,
        branch: BranchId,
        kind: EventKind,
        call_id: Option<SessionCallId>,
        payload: Vec<u8>,
    },
    Append {
        session: SessionId,
        branch: BranchId,
        expected_head: HeadStamp,
        reservation: EventReservationView,
    },
    Read {
        session: SessionId,
        branch: BranchId,
        snapshot_head: HeadStamp,
        after: Option<SessionEventId>,
        limit: u16,
    },
    ReserveBranch {
        session: SessionId,
        kind: BranchMutationKind,
        source_branch: BranchId,
        target_event: SessionEventId,
    },
    BranchCommit {
        session: SessionId,
        kind: BranchMutationKind,
        source_branch: BranchId,
        target_event: SessionEventId,
        reservation: super::dto::BranchReservationView,
    },
    LoadEvent {
        session: SessionId,
        branch: BranchId,
        event: SessionEventId,
    },
}

struct BranchPlan {
    branch: BranchId,
    committed_bytes: u64,
    event_count: u64,
    head: HeadStamp,
}

struct LoadState {
    session: SessionId,
    plan: Vec<BranchPlan>,
    index: usize,
    offset: u64,
    events: Vec<EventMeta>,
    open_calls: Vec<SessionCallId>,
    assembled: Vec<(BranchId, BranchLedger)>,
    recovered: bool,
    next: Box<Action>,
}

/// Why a recovery plan could not be built.
enum PlanError {
    NotFound,
    Corrupt,
}

/// Classifies one action-path failure.
enum OpFail {
    /// Durable substrate failure; fatal for the actor.
    Storage,
    /// Terminal domain failure for this operation.
    Domain(SessionError),
}

impl SessionActor {
    pub(crate) fn new(home: HomeLayout, fence: Arc<GenerationFence>) -> Self {
        Self {
            home,
            fence,
            admission: AdmissionLedger::new(),
            sessions: HashMap::new(),
        }
    }

    fn mint_admission(&self) -> Result<TaskOperationAdmission, SessionTaskError> {
        let operation = self.admission.open_operation().map_err(admission_error)?;
        let resource = self.admission.admit_resource().map_err(admission_error)?;
        Ok(TaskOperationAdmission::new(operation, resource))
    }

    fn invoke_sync(
        &mut self,
        request: &SessionRequest,
    ) -> Result<SessionOperation, SessionTaskError> {
        fn failed(
            admission: Option<TaskOperationAdmission>,
            error: SessionError,
        ) -> Result<SessionOperation, SessionTaskError> {
            Ok(SessionOperation {
                admission,
                stage: Stage::Failed(error),
            })
        }
        let admission = Some(self.mint_admission()?);
        let stage = match request {
            SessionRequest::Create => match (SessionId::generate(), BranchId::generate()) {
                (Some(session), Some(branch)) => Stage::Run(Action::Create { session, branch }),
                _ => return failed(admission, SessionError::Unavailable),
            },
            SessionRequest::Open { session } => self.stage_for(
                session.clone(),
                Action::Heads {
                    session: session.clone(),
                },
            ),
            SessionRequest::ReserveEvent {
                session,
                branch,
                kind,
                call_id,
                payload,
            } => {
                if payload.is_empty() || payload.len() > kind.payload_bound() {
                    return failed(admission, SessionError::Limit);
                }
                if matches!(kind, EventKind::ToolCall | EventKind::ToolResult) != call_id.is_some()
                {
                    return failed(admission, SessionError::InvalidArgument);
                }
                self.stage_for(
                    session.clone(),
                    Action::ReserveEvent {
                        session: session.clone(),
                        branch: branch.clone(),
                        kind: *kind,
                        call_id: call_id.clone(),
                        payload: payload.clone(),
                    },
                )
            }
            SessionRequest::Append {
                session,
                branch,
                expected_head,
                reservation,
            } => {
                if !is_canonical_digest(&reservation.payload_digest) {
                    return failed(admission, SessionError::InvalidArgument);
                }
                self.stage_for(
                    session.clone(),
                    Action::Append {
                        session: session.clone(),
                        branch: branch.clone(),
                        expected_head: expected_head.clone(),
                        reservation: reservation.clone(),
                    },
                )
            }
            SessionRequest::Read {
                session,
                branch,
                snapshot_head,
                after,
                limit,
            } => {
                if *limit == 0 || *limit > MAX_READ_LIMIT {
                    return failed(admission, SessionError::Limit);
                }
                self.stage_for(
                    session.clone(),
                    Action::Read {
                        session: session.clone(),
                        branch: branch.clone(),
                        snapshot_head: snapshot_head.clone(),
                        after: after.clone(),
                        limit: *limit,
                    },
                )
            }
            SessionRequest::ReserveBranch {
                session,
                kind,
                source_branch,
                target_event,
            } => self.stage_for(
                session.clone(),
                Action::ReserveBranch {
                    session: session.clone(),
                    kind: *kind,
                    source_branch: source_branch.clone(),
                    target_event: target_event.clone(),
                },
            ),
            SessionRequest::Fork {
                session,
                from_branch,
                at_event,
                reservation,
            } => {
                if !is_canonical_digest(&reservation.mutation_digest) {
                    return failed(admission, SessionError::InvalidArgument);
                }
                self.stage_for(
                    session.clone(),
                    Action::BranchCommit {
                        session: session.clone(),
                        kind: BranchMutationKind::Fork,
                        source_branch: from_branch.clone(),
                        target_event: at_event.clone(),
                        reservation: reservation.clone(),
                    },
                )
            }
            SessionRequest::Rewind {
                session,
                branch,
                to_event,
                reservation,
            } => {
                if !is_canonical_digest(&reservation.mutation_digest) {
                    return failed(admission, SessionError::InvalidArgument);
                }
                self.stage_for(
                    session.clone(),
                    Action::BranchCommit {
                        session: session.clone(),
                        kind: BranchMutationKind::Rewind,
                        source_branch: branch.clone(),
                        target_event: to_event.clone(),
                        reservation: reservation.clone(),
                    },
                )
            }
            SessionRequest::LoadEvent {
                session,
                branch,
                event,
            } => self.stage_for(
                session.clone(),
                Action::LoadEvent {
                    session: session.clone(),
                    branch: branch.clone(),
                    event: event.clone(),
                },
            ),
        };
        Ok(SessionOperation { admission, stage })
    }

    /// Routes an action for a known session: directly when its ledger is
    /// loaded, through chunked recovery otherwise.
    fn stage_for(&self, session: SessionId, next: Action) -> Stage {
        if self.sessions.contains_key(&session) {
            return Stage::Run(next);
        }
        match self.recovery_plan(&session) {
            Ok(plan) => Stage::Load(LoadState {
                session,
                plan,
                index: 0,
                offset: 0,
                events: Vec::new(),
                open_calls: Vec::new(),
                assembled: Vec::new(),
                recovered: false,
                next: Box::new(next),
            }),
            Err(PlanError::NotFound) => Stage::Failed(SessionError::NotFound),
            Err(PlanError::Corrupt) => Stage::Failed(SessionError::Corrupt),
        }
    }

    /// Reads the manifest and builds the per-branch recovery plan.
    fn recovery_plan(&self, session: &SessionId) -> Result<Vec<BranchPlan>, PlanError> {
        let paths = SessionPaths::new(&self.home, session);
        let bytes = match read_owned_file(&self.home, paths.manifest(), MAX_MANIFEST_BYTES) {
            Ok(Some(bytes)) => bytes.to_vec(),
            Ok(None) => return Err(PlanError::NotFound),
            Err(_) => return Err(PlanError::Corrupt),
        };
        let manifest = store::decode_manifest(&bytes).map_err(|_| PlanError::Corrupt)?;
        if manifest.session_id != session.as_str() {
            return Err(PlanError::Corrupt);
        }
        let mut plan = Vec::with_capacity(manifest.branches.len());
        for branch in &manifest.branches {
            let branch_id = BranchId::parse(&branch.branch_id).ok_or(PlanError::Corrupt)?;
            plan.push(BranchPlan {
                branch: branch_id,
                committed_bytes: branch.committed_bytes,
                event_count: branch.event_count,
                head: store::decode_head(&branch.head).ok_or(PlanError::Corrupt)?,
            });
        }
        Ok(plan)
    }

    fn pull_sync(
        &mut self,
        operation: &mut SessionOperation,
    ) -> Result<SessionPull, SessionTaskError> {
        {
            match &mut operation.stage {
                Stage::Terminal(pull) => Ok(pull.clone()),
                Stage::Failed(error) => Ok(SessionPull::Failed(error.clone())),
                Stage::Run(_) => {
                    let stage = std::mem::replace(
                        &mut operation.stage,
                        Stage::Failed(SessionError::Unavailable),
                    );
                    let Stage::Run(action) = stage else {
                        unreachable!("the stage was just replaced from Run");
                    };
                    let pull = self.execute(action)?;
                    operation.stage = match &pull {
                        SessionPull::Complete(_) => Stage::Terminal(pull.clone()),
                        SessionPull::Failed(error) => Stage::Failed(error.clone()),
                        _ => unreachable!("execute always returns a terminal"),
                    };
                    Ok(pull)
                }
                Stage::Load(load) => {
                    if !load.recovered {
                        match self.recover_tails(load) {
                            Ok(()) => {}
                            Err(OpFail::Domain(error)) => {
                                operation.stage = Stage::Failed(error.clone());
                                return Ok(SessionPull::Failed(error));
                            }
                            Err(OpFail::Storage) => return Err(SessionTaskError::Storage),
                        }
                        load.recovered = true;
                        return Ok(SessionPull::Progress(SessionProgress::Recovering));
                    }
                    match self.verify_budget(load) {
                        Ok(true) => {}
                        Ok(false) => return Ok(SessionPull::Progress(SessionProgress::Replaying)),
                        Err(OpFail::Domain(error)) => {
                            operation.stage = Stage::Failed(error.clone());
                            return Ok(SessionPull::Failed(error));
                        }
                        Err(OpFail::Storage) => return Err(SessionTaskError::Storage),
                    }
                    let stage = std::mem::replace(
                        &mut operation.stage,
                        Stage::Failed(SessionError::Unavailable),
                    );
                    let Stage::Load(load) = stage else {
                        unreachable!("the stage was just replaced from Load");
                    };
                    let LoadState {
                        session,
                        assembled,
                        next,
                        ..
                    } = load;
                    let action = *next;
                    let ledger = SessionLedger {
                        total_bytes: assembled
                            .iter()
                            .map(|(_, branch)| branch.committed_bytes)
                            .sum(),
                        branches: assembled.into_iter().collect(),
                        event_reservations: HashMap::new(),
                        branch_reservations: HashMap::new(),
                    };
                    self.sessions.insert(session, ledger);
                    let pull = self.execute(action)?;
                    operation.stage = match &pull {
                        SessionPull::Complete(_) => Stage::Terminal(pull.clone()),
                        SessionPull::Failed(error) => Stage::Failed(error.clone()),
                        _ => unreachable!("execute always returns a terminal"),
                    };
                    Ok(pull)
                }
            }
        }
    }

    /// Discards torn tails and orphan staged payloads.
    fn recover_tails(&self, load: &LoadState) -> Result<(), OpFail> {
        let paths = SessionPaths::new(&self.home, &load.session);
        for plan in &load.plan {
            let relative = paths.branch_events(&plan.branch);
            let absolute = paths.absolute(&relative).map_err(|_| OpFail::Storage)?;
            match fs::file_len(&absolute) {
                Ok(length) => {
                    if length > plan.committed_bytes {
                        fs::truncate(&absolute, plan.committed_bytes)
                            .map_err(|_| OpFail::Storage)?;
                    } else if length < plan.committed_bytes {
                        return Err(OpFail::Domain(SessionError::Corrupt));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if plan.committed_bytes > 0 {
                        return Err(OpFail::Domain(SessionError::Corrupt));
                    }
                }
                Err(_) => return Err(OpFail::Storage),
            }
        }
        // Staged payloads are orphaned by any restart: reservations are
        // actor-local, so every remaining staged file is unreferenced.
        if let Ok(absolute) = paths.absolute(&paths.pending_dir())
            && let Ok(names) = fs::list_files(&absolute)
        {
            for name in names {
                fs::remove(&absolute.join(name));
            }
        }
        Ok(())
    }

    /// Verifies within this pull's budget; returns `true` when the whole
    /// plan finished.
    fn verify_budget(&mut self, load: &mut LoadState) -> Result<bool, OpFail> {
        let mut spent = 0_u64;
        while load.index < load.plan.len() && spent < VERIFY_BUDGET_BYTES {
            self.verify_one(load, VERIFY_BUDGET_BYTES - spent, &mut spent)?;
        }
        Ok(load.index == load.plan.len())
    }

    /// Verifies exactly one record of the current branch.
    fn verify_one(
        &mut self,
        load: &mut LoadState,
        budget: u64,
        spent: &mut u64,
    ) -> Result<(), OpFail> {
        let plan = &load.plan[load.index];
        let paths = SessionPaths::new(&self.home, &load.session);
        let corrupt = || OpFail::Domain(SessionError::Corrupt);
        if plan.committed_bytes == 0 {
            // An empty branch has no log file at all.
            if plan.event_count != 0 || !plan.head.is_empty() || !load.events.is_empty() {
                return Err(corrupt());
            }
            let branch = BranchLedger {
                head: plan.head.clone(),
                committed_bytes: 0,
                open_calls: std::collections::HashSet::new(),
                events: Vec::new(),
            };
            load.assembled.push((plan.branch.clone(), branch));
            load.index += 1;
            load.offset = 0;
            return Ok(());
        }
        let absolute = paths
            .absolute(&paths.branch_events(&plan.branch))
            .map_err(|_| OpFail::Storage)?;
        let remaining = plan.committed_bytes - load.offset;
        let window = remaining.min(budget.saturating_sub(*spent)).max(4);
        let bytes = fs::read_range(&absolute, load.offset, window).map_err(|_| OpFail::Storage)?;
        let decoded = match store::decode_record(&bytes) {
            Ok(decoded) => decoded,
            Err(store::RecordError::Incomplete) => {
                let total =
                    u64::from(u32::from_be_bytes(bytes[..4].try_into().expect("header"))) + 4;
                if total > remaining {
                    return Err(corrupt());
                }
                let full =
                    fs::read_range(&absolute, load.offset, total).map_err(|_| OpFail::Storage)?;
                store::decode_record(&full).map_err(|_| corrupt())?
            }
            Err(store::RecordError::Corrupt) => return Err(corrupt()),
        };
        let offset = load.offset;
        load.offset += decoded.record_len;
        *spent += decoded.record_len;

        let event_id = SessionEventId::parse(&decoded.event_id).ok_or_else(corrupt)?;
        let kind = EventKind::from_tag(decoded.kind).ok_or_else(corrupt)?;
        let call_id = decoded.call_id.as_deref().and_then(SessionCallId::parse);
        if matches!(kind, EventKind::ToolCall | EventKind::ToolResult) != call_id.is_some() {
            return Err(corrupt());
        }
        match kind {
            EventKind::ToolCall => {
                if load.events.iter().any(|row| row.call_id == call_id) {
                    return Err(corrupt());
                }
                load.open_calls
                    .push(call_id.clone().expect("tool call carries an id"));
            }
            EventKind::ToolResult => {
                let call = call_id.clone().expect("tool result carries an id");
                let position = load
                    .open_calls
                    .iter()
                    .position(|open| *open == call)
                    .ok_or_else(corrupt)?;
                load.open_calls.swap_remove(position);
            }
            EventKind::Message | EventKind::Usage | EventKind::Task => {}
        }
        load.events.push(EventMeta {
            digest: format_digest(&decoded.payload_digest),
            bytes: decoded.payload_len,
            event_id,
            kind,
            call_id,
            offset,
            record_len: decoded.record_len,
        });

        if load.offset == plan.committed_bytes {
            if load.events.len() as u64 != plan.event_count {
                return Err(corrupt());
            }
            match (&plan.head, load.events.last()) {
                (HeadStamp::Empty, None) => {}
                (HeadStamp::Event(head), Some(last)) if &last.event_id == head => {}
                _ => return Err(corrupt()),
            }
            let events = std::mem::take(&mut load.events);
            load.open_calls.clear();
            let branch = BranchLedger {
                head: plan.head.clone(),
                committed_bytes: plan.committed_bytes,
                open_calls: load
                    .open_calls
                    .drain(..)
                    .collect::<std::collections::HashSet<_>>(),
                events,
            };
            let branch_id = plan.branch.clone();
            load.assembled.push((branch_id, branch));
            load.index += 1;
            load.offset = 0;
        }
        Ok(())
    }

    /// Executes one terminal action.
    fn execute(&mut self, action: Action) -> Result<SessionPull, SessionTaskError> {
        match self.run(action) {
            Ok(pull) => Ok(pull),
            Err(OpFail::Storage) => Err(SessionTaskError::Storage),
            Err(OpFail::Domain(error)) => Ok(SessionPull::Failed(error)),
        }
    }

    fn run(&mut self, action: Action) -> Result<SessionPull, OpFail> {
        match action {
            Action::Create { session, branch } => self.action_create(&session, &branch),
            Action::Heads { session } => self.action_heads(&session),
            Action::ReserveEvent {
                session,
                branch,
                kind,
                call_id,
                payload,
            } => self.action_reserve_event(&session, &branch, kind, call_id, &payload),
            Action::Append {
                session,
                branch,
                expected_head,
                reservation,
            } => self.action_append(&session, &branch, &expected_head, &reservation),
            Action::Read {
                session,
                branch,
                snapshot_head,
                after,
                limit,
            } => self.action_read(&session, &branch, &snapshot_head, after.as_ref(), limit),
            Action::ReserveBranch {
                session,
                kind,
                source_branch,
                target_event,
            } => self.action_reserve_branch(&session, kind, &source_branch, &target_event),
            Action::BranchCommit {
                session,
                kind,
                source_branch,
                target_event,
                reservation,
            } => self.action_branch_commit(
                &session,
                kind,
                &source_branch,
                &target_event,
                &reservation,
            ),
            Action::LoadEvent {
                session,
                branch,
                event,
            } => self.action_load_event(&session, &branch, &event),
        }
    }

    fn action_create(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
    ) -> Result<SessionPull, OpFail> {
        let unavailable = || OpFail::Domain(SessionError::Unavailable);
        let Some(activity) = self.fence.enter() else {
            return Err(OpFail::Domain(SessionError::Unavailable));
        };
        let paths = SessionPaths::new(&self.home, session);
        let manifest = ManifestFile {
            format_version: MANIFEST_FORMAT_VERSION,
            kind: MANIFEST_KIND.to_owned(),
            session_id: session.as_str().to_owned(),
            branches: vec![ManifestBranchFile {
                branch_id: branch.as_str().to_owned(),
                parentage: ParentageFile {
                    kind: "root".to_owned(),
                    source_branch_id: None,
                    at_event_id: None,
                    to_event_id: None,
                },
                head: store::encode_head(&HeadStamp::Empty),
                event_count: 0,
                committed_bytes: 0,
            }],
        };
        let bytes = store::encode_manifest(&manifest).map_err(|_| unavailable())?;
        let mut collision = false;
        let commit = activity.begin_commit().map_err(|_| unavailable())?;
        let update = locked_update_owned_file(
            &self.home,
            paths.manifest(),
            MAX_MANIFEST_BYTES,
            |current| match current {
                None => Ok(bytes),
                Some(_) => {
                    collision = true;
                    Err(ConfigError::authority_rejection())
                }
            },
        );
        drop(commit);
        match update {
            Ok(()) => {}
            Err(_) if collision => return Err(unavailable()),
            Err(_) => return Err(OpFail::Storage),
        }
        for directory in [paths.branches_dir(), paths.pending_dir()] {
            ensure_owned_directory(&self.home, directory).map_err(|_| OpFail::Storage)?;
        }
        let mut ledger = SessionLedger::empty();
        ledger.branches.insert(branch.clone(), BranchLedger::root());
        self.sessions.insert(session.clone(), ledger);
        Ok(SessionPull::Complete(SessionResult::Created(
            CreatedResult {
                session_id: session.clone(),
                branch_id: branch.clone(),
            },
        )))
    }

    fn action_heads(&mut self, session: &SessionId) -> Result<SessionPull, OpFail> {
        let Some(ledger) = self.sessions.get(session) else {
            return Err(OpFail::Domain(SessionError::NotFound));
        };
        Ok(SessionPull::Complete(SessionResult::Opened(OpenedResult {
            heads: ledger.heads(),
        })))
    }

    fn action_reserve_event(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
        kind: EventKind,
        call_id: Option<SessionCallId>,
        payload: &[u8],
    ) -> Result<SessionPull, OpFail> {
        let Some(_activity) = self.fence.enter() else {
            return Err(OpFail::Domain(SessionError::Unavailable));
        };
        let paths = SessionPaths::new(&self.home, session);
        let ledger = self
            .sessions
            .get_mut(session)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        let branch_state = ledger
            .branches
            .get(branch)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        SessionLedger::check_ordering(branch_state, kind, call_id.as_ref())
            .map_err(OpFail::Domain)?;
        let total = ledger
            .total_bytes
            .checked_add(payload.len() as u64)
            .ok_or(OpFail::Domain(SessionError::Limit))?;
        if total > MAX_SESSION_TOTAL_BYTES {
            return Err(OpFail::Domain(SessionError::Limit));
        }
        let event_id =
            SessionEventId::generate().ok_or(OpFail::Domain(SessionError::Unavailable))?;
        let digest_raw = payload_digest(payload);
        ensure_owned_directory(&self.home, paths.pending_dir()).map_err(|_| OpFail::Storage)?;
        let pending = paths
            .absolute(&paths.pending_payload(&event_id))
            .map_err(|_| OpFail::Storage)?;
        fs::create_exclusive(&pending, payload).map_err(|_| OpFail::Storage)?;
        let view = EventReservationView {
            payload_digest: format_digest(&digest_raw),
            expected_head: branch_state.head.clone(),
            branch_id: branch.clone(),
            event_id: event_id.clone(),
        };
        ledger.event_reservations.insert(
            event_id.clone(),
            EventReservationRow {
                view: view.clone(),
                kind,
                call_id,
            },
        );
        Ok(SessionPull::Complete(SessionResult::ReservedEvent(view)))
    }

    #[allow(clippy::too_many_lines)]
    fn action_append(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
        expected_head: &HeadStamp,
        reservation: &EventReservationView,
    ) -> Result<SessionPull, OpFail> {
        let Some(activity) = self.fence.enter() else {
            return Err(OpFail::Domain(SessionError::Unavailable));
        };
        let paths = SessionPaths::new(&self.home, session);
        let ledger = self
            .sessions
            .get_mut(session)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        // Consume the single-use reservation regardless of the commit result.
        let Some(row) = ledger.event_reservations.remove(&reservation.event_id) else {
            return Err(OpFail::Domain(SessionError::NotFound));
        };
        if row.view != *reservation {
            return Err(OpFail::Domain(SessionError::InvalidArgument));
        }
        if &row.view.branch_id != branch {
            return Err(OpFail::Domain(SessionError::InvalidArgument));
        }
        let branch_state = ledger
            .branches
            .get_mut(branch)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        if branch_state.head != *expected_head || row.view.expected_head != *expected_head {
            return Err(OpFail::Domain(SessionError::Conflict(ConflictResult {
                actual: branch_state.head.clone(),
            })));
        }
        // The staged payload was created by this actor's exclusive create;
        // read it back with the same bounded plain-FS primitive.
        let pending_path = paths
            .absolute(&paths.pending_payload(&reservation.event_id))
            .map_err(|_| OpFail::Storage)?;
        let payload = match fs::file_len(&pending_path) {
            Ok(length) if length > 0 && length <= row.kind.payload_bound() as u64 => {
                fs::read_range(&pending_path, 0, length).map_err(|_| OpFail::Storage)?
            }
            Ok(_) => return Err(OpFail::Domain(SessionError::Corrupt)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(OpFail::Domain(SessionError::Corrupt));
            }
            Err(_) => return Err(OpFail::Storage),
        };
        if format_digest(&payload_digest(&payload)) != reservation.payload_digest {
            return Err(OpFail::Domain(SessionError::Corrupt));
        }
        let record = store::encode_record(
            &reservation.event_id,
            row.kind,
            row.call_id.as_ref(),
            &payload,
        );
        let total = ledger
            .total_bytes
            .checked_add(record.len() as u64)
            .ok_or(OpFail::Domain(SessionError::Limit))?;
        if total > MAX_SESSION_TOTAL_BYTES {
            return Err(OpFail::Domain(SessionError::Limit));
        }
        let log_path = paths
            .absolute(&paths.branch_events(branch))
            .map_err(|_| OpFail::Storage)?;
        // Any bytes beyond the committed prefix are earlier uncommitted
        // attempts; drop them so this record lands contiguously.
        match fs::file_len(&log_path) {
            Ok(length) if length > branch_state.committed_bytes => {
                fs::truncate(&log_path, branch_state.committed_bytes)
                    .map_err(|_| OpFail::Storage)?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(OpFail::Storage),
        }
        fs::append(&log_path, &record).map_err(|_| OpFail::Storage)?;

        let commit = activity
            .begin_commit()
            .map_err(|_| OpFail::Domain(SessionError::Unavailable))?;
        let expected_count = branch_state.events.len() as u64;
        let expected_committed = branch_state.committed_bytes;
        let expected_head_encoded = store::encode_head(expected_head);
        let event_id = reservation.event_id.clone();
        let mut conflict: Option<HeadStamp> = None;
        let update = locked_update_owned_file(
            &self.home,
            paths.manifest(),
            MAX_MANIFEST_BYTES,
            |current| {
                let mut manifest: ManifestFile =
                    serde_json::from_slice(current.ok_or_else(ConfigError::authority_rejection)?)
                        .map_err(|_| ConfigError::authority_rejection())?;
                let target = manifest
                    .branches
                    .iter_mut()
                    .find(|row| row.branch_id == branch.as_str())
                    .ok_or_else(ConfigError::authority_rejection)?;
                if target.head != expected_head_encoded
                    || target.event_count != expected_count
                    || target.committed_bytes != expected_committed
                {
                    conflict = store::decode_head(&target.head);
                    return Err(ConfigError::authority_rejection());
                }
                target.head = event_id.as_str().to_owned();
                target.event_count = expected_count + 1;
                target.committed_bytes = expected_committed + record.len() as u64;
                store::encode_manifest(&manifest).map_err(|_| ConfigError::authority_rejection())
            },
        );
        drop(commit);
        match update {
            Ok(()) => {}
            Err(_) => {
                if let Some(actual) = conflict.take() {
                    return Err(OpFail::Domain(SessionError::Conflict(ConflictResult {
                        actual,
                    })));
                }
                return Err(OpFail::Domain(SessionError::Corrupt));
            }
        }

        let offset = branch_state.committed_bytes;
        branch_state.committed_bytes += record.len() as u64;
        match row.kind {
            EventKind::ToolCall => {
                branch_state
                    .open_calls
                    .insert(row.call_id.clone().expect("tool call carries an id"));
            }
            EventKind::ToolResult => {
                branch_state
                    .open_calls
                    .remove(&row.call_id.clone().expect("tool result carries an id"));
            }
            EventKind::Message | EventKind::Usage | EventKind::Task => {}
        }
        branch_state.head = HeadStamp::Event(reservation.event_id.clone());
        branch_state.events.push(EventMeta {
            digest: reservation.payload_digest.clone(),
            bytes: payload.len() as u64,
            event_id: reservation.event_id.clone(),
            kind: row.kind,
            call_id: row.call_id.clone(),
            offset,
            record_len: record.len() as u64,
        });
        ledger.total_bytes = total;
        if let Ok(pending) = paths.absolute(&paths.pending_payload(&reservation.event_id)) {
            // Orphan-tolerant: recovery removes anything left behind.
            fs::remove(&pending);
        }
        Ok(SessionPull::Complete(SessionResult::Appended(
            AppendedResult {
                head: HeadStamp::Event(reservation.event_id.clone()),
            },
        )))
    }

    fn action_read(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
        snapshot_head: &HeadStamp,
        after: Option<&SessionEventId>,
        limit: u16,
    ) -> Result<SessionPull, OpFail> {
        let Some(ledger) = self.sessions.get(session) else {
            return Err(OpFail::Domain(SessionError::NotFound));
        };
        let result = ledger
            .page(branch, snapshot_head, after, limit)
            .map_err(OpFail::Domain)?;
        Ok(SessionPull::Complete(SessionResult::Events(result)))
    }

    fn action_reserve_branch(
        &mut self,
        session: &SessionId,
        kind: BranchMutationKind,
        source_branch: &BranchId,
        target_event: &SessionEventId,
    ) -> Result<SessionPull, OpFail> {
        let Some(_activity) = self.fence.enter() else {
            return Err(OpFail::Domain(SessionError::Unavailable));
        };
        let ledger = self
            .sessions
            .get_mut(session)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        let branch_state = ledger
            .branches
            .get(source_branch)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        if !branch_state
            .events
            .iter()
            .any(|row| &row.event_id == target_event)
        {
            return Err(OpFail::Domain(SessionError::NotFound));
        }
        if ledger.branches.len() >= MAX_BRANCHES {
            return Err(OpFail::Domain(SessionError::Limit));
        }
        let new_branch = BranchId::generate().ok_or(OpFail::Domain(SessionError::Unavailable))?;
        let reservation_id =
            BranchReservationId::generate().ok_or(OpFail::Domain(SessionError::Unavailable))?;
        let digest = branch_mutation_digest(&BranchMutationDigestInput {
            session_id: session.as_str(),
            reservation_id: reservation_id.as_str(),
            kind,
            source_branch_id: source_branch.as_str(),
            source_head: &branch_state.head,
            target_event_id: target_event,
            new_branch_id: new_branch.as_str(),
        })
        .ok_or(OpFail::Domain(SessionError::Unavailable))?;
        let view = super::dto::BranchReservationView {
            reservation_id: reservation_id.clone(),
            kind,
            source_branch_id: source_branch.clone(),
            source_head: branch_state.head.clone(),
            target_event_id: target_event.clone(),
            new_branch_id: new_branch,
            mutation_digest: format_digest(&digest),
        };
        ledger
            .branch_reservations
            .insert(reservation_id, BranchReservationRow { view: view.clone() });
        Ok(SessionPull::Complete(SessionResult::ReservedBranch(view)))
    }

    fn action_branch_commit(
        &mut self,
        session: &SessionId,
        kind: BranchMutationKind,
        source_branch: &BranchId,
        target_event: &SessionEventId,
        reservation: &super::dto::BranchReservationView,
    ) -> Result<SessionPull, OpFail> {
        let Some(activity) = self.fence.enter() else {
            return Err(OpFail::Domain(SessionError::Unavailable));
        };
        let paths = SessionPaths::new(&self.home, session);
        let ledger = self
            .sessions
            .get_mut(session)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        // Consume the single-use reservation regardless of the result.
        let Some(row) = ledger
            .branch_reservations
            .remove(&reservation.reservation_id)
        else {
            return Err(OpFail::Domain(SessionError::NotFound));
        };
        if row.view != *reservation
            || row.view.kind != kind
            || &row.view.source_branch_id != source_branch
            || &row.view.target_event_id != target_event
        {
            return Err(OpFail::Domain(SessionError::InvalidArgument));
        }
        let recomputed = branch_mutation_digest(&BranchMutationDigestInput {
            session_id: session.as_str(),
            reservation_id: row.view.reservation_id.as_str(),
            kind,
            source_branch_id: row.view.source_branch_id.as_str(),
            source_head: &row.view.source_head,
            target_event_id: &row.view.target_event_id,
            new_branch_id: row.view.new_branch_id.as_str(),
        })
        .ok_or(OpFail::Domain(SessionError::InvalidArgument))?;
        if format_digest(&recomputed) != row.view.mutation_digest {
            return Err(OpFail::Domain(SessionError::InvalidArgument));
        }
        let branch_state = ledger
            .branches
            .get(source_branch)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        if branch_state.head != row.view.source_head {
            return Err(OpFail::Domain(SessionError::Conflict(ConflictResult {
                actual: branch_state.head.clone(),
            })));
        }
        if ledger.branches.len() >= MAX_BRANCHES {
            return Err(OpFail::Domain(SessionError::Limit));
        }
        let new_branch = row.view.new_branch_id.clone();
        if ledger.branches.contains_key(&new_branch) {
            return Err(OpFail::Domain(SessionError::Corrupt));
        }
        let position = branch_state
            .events
            .iter()
            .position(|row| &row.event_id == target_event)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        let boundary = &branch_state.events[position];
        let prefix_end = boundary.offset + boundary.record_len;
        let total = ledger
            .total_bytes
            .checked_add(prefix_end)
            .ok_or(OpFail::Domain(SessionError::Limit))?;
        if total > MAX_SESSION_TOTAL_BYTES {
            return Err(OpFail::Domain(SessionError::Limit));
        }
        let source_log = paths
            .absolute(&paths.branch_events(source_branch))
            .map_err(|_| OpFail::Storage)?;
        let new_log = paths
            .absolute(&paths.branch_events(&new_branch))
            .map_err(|_| OpFail::Storage)?;
        ensure_owned_directory(&self.home, paths.branches_dir()).map_err(|_| OpFail::Storage)?;
        fs::copy_prefix(&source_log, &new_log, prefix_end).map_err(|_| OpFail::Storage)?;

        let commit = activity
            .begin_commit()
            .map_err(|_| OpFail::Domain(SessionError::Unavailable))?;
        let expected_head = store::encode_head(&row.view.source_head);
        let expected_count = branch_state.events.len() as u64;
        let expected_committed = branch_state.committed_bytes;
        let mut conflict = false;
        let parentage = match kind {
            BranchMutationKind::Fork => ParentageFile {
                kind: "fork".to_owned(),
                source_branch_id: Some(source_branch.as_str().to_owned()),
                at_event_id: Some(target_event.as_str().to_owned()),
                to_event_id: None,
            },
            BranchMutationKind::Rewind => ParentageFile {
                kind: "rewind".to_owned(),
                source_branch_id: Some(source_branch.as_str().to_owned()),
                at_event_id: None,
                to_event_id: Some(target_event.as_str().to_owned()),
            },
        };
        let new_row = ManifestBranchFile {
            branch_id: new_branch.as_str().to_owned(),
            parentage,
            head: target_event.as_str().to_owned(),
            event_count: (position + 1) as u64,
            committed_bytes: prefix_end,
        };
        let update = locked_update_owned_file(
            &self.home,
            paths.manifest(),
            MAX_MANIFEST_BYTES,
            |current| {
                let mut manifest: ManifestFile =
                    serde_json::from_slice(current.ok_or_else(ConfigError::authority_rejection)?)
                        .map_err(|_| ConfigError::authority_rejection())?;
                let source = manifest
                    .branches
                    .iter_mut()
                    .find(|row| row.branch_id == source_branch.as_str())
                    .ok_or_else(ConfigError::authority_rejection)?;
                if source.head != expected_head
                    || source.event_count != expected_count
                    || source.committed_bytes != expected_committed
                {
                    conflict = true;
                    return Err(ConfigError::authority_rejection());
                }
                if manifest
                    .branches
                    .iter()
                    .any(|row| row.branch_id == new_row.branch_id)
                {
                    return Err(ConfigError::authority_rejection());
                }
                manifest.branches.push(new_row.clone());
                store::encode_manifest(&manifest).map_err(|_| ConfigError::authority_rejection())
            },
        );
        drop(commit);
        match update {
            Ok(()) => {}
            Err(_) if conflict => {
                // The copied file is an uncommitted attempt; remove it.
                fs::remove(&new_log);
                return Err(OpFail::Domain(SessionError::Conflict(ConflictResult {
                    actual: ledger
                        .branches
                        .get(source_branch)
                        .map_or(HeadStamp::Empty, |branch| branch.head.clone()),
                })));
            }
            Err(_) => {
                fs::remove(&new_log);
                return Err(OpFail::Domain(SessionError::Corrupt));
            }
        }

        let cloned = branch_state.events[..=position].to_vec();
        let mut open_calls = std::collections::HashSet::new();
        for row in &cloned {
            match row.kind {
                EventKind::ToolCall => {
                    open_calls.insert(row.call_id.clone().expect("tool call carries an id"));
                }
                EventKind::ToolResult => {
                    open_calls.remove(&row.call_id.clone().expect("tool result carries an id"));
                }
                EventKind::Message | EventKind::Usage | EventKind::Task => {}
            }
        }
        let new_state = BranchLedger {
            head: HeadStamp::Event(target_event.clone()),
            committed_bytes: prefix_end,
            open_calls,
            events: cloned,
        };
        ledger.branches.insert(new_branch.clone(), new_state);
        ledger.total_bytes = total;
        Ok(SessionPull::Complete(SessionResult::Branched(
            BranchedResult {
                branch_id: new_branch,
                head: HeadStamp::Event(target_event.clone()),
            },
        )))
    }

    fn action_load_event(
        &mut self,
        session: &SessionId,
        branch: &BranchId,
        event: &SessionEventId,
    ) -> Result<SessionPull, OpFail> {
        let Some(ledger) = self.sessions.get(session) else {
            return Err(OpFail::Domain(SessionError::NotFound));
        };
        let branch_state = ledger
            .branches
            .get(branch)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        let meta = branch_state
            .events
            .iter()
            .find(|row| &row.event_id == event)
            .ok_or(OpFail::Domain(SessionError::NotFound))?;
        let paths = SessionPaths::new(&self.home, session);
        let absolute = paths
            .absolute(&paths.branch_events(branch))
            .map_err(|_| OpFail::Storage)?;
        let bytes =
            fs::read_range(&absolute, meta.offset, meta.record_len).map_err(|_| OpFail::Storage)?;
        let decoded =
            store::decode_record(&bytes).map_err(|_| OpFail::Domain(SessionError::Corrupt))?;
        if decoded.event_id != event.as_str() || decoded.record_len != meta.record_len {
            return Err(OpFail::Domain(SessionError::Corrupt));
        }
        let start = decoded.payload_offset;
        let end = start + decoded.payload_len as usize;
        let payload = bytes
            .get(start..end)
            .ok_or(OpFail::Domain(SessionError::Corrupt))?
            .to_vec();
        Ok(SessionPull::Complete(SessionResult::Loaded(LoadedEvent {
            event: meta.to_session_event(),
            payload,
        })))
    }
}

fn admission_error(error: AdmissionError) -> SessionTaskError {
    match error {
        AdmissionError::ResourceCapacity | AdmissionError::OperationCapacity => {
            SessionTaskError::Admission
        }
    }
}

impl PackTaskActor for SessionActor {
    type Request = SessionRequest;
    type Operation = SessionOperation;
    type Pull = SessionPull;
    type Error = SessionTaskError;

    fn is_available(&self) -> bool {
        true
    }

    fn is_fatal(error: Self::Error) -> bool {
        matches!(error, SessionTaskError::Storage)
    }

    async fn invoke(&mut self, request: &Self::Request) -> Result<Self::Operation, Self::Error> {
        self.invoke_sync(request)
    }

    async fn pull(&mut self, operation: &mut Self::Operation) -> Result<Self::Pull, Self::Error> {
        self.pull_sync(operation)
    }

    async fn drop_operation(&mut self, _operation: Self::Operation) -> Result<(), Self::Error> {
        Ok(())
    }

    fn take_admission(operation: &mut Self::Operation) -> Option<TaskOperationAdmission> {
        operation.admission.take()
    }
}
