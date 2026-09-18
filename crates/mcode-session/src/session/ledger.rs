//! In-memory session ledger state and pure validation logic.
//!
//! One [`SessionLedger`] is the actor-owned projection of a recovered session:
//! branch heads, ordered event indexes with log offsets, open tool-call
//! tracking for the call/result ordering check, and the single-use
//! reservation tables. Every mutation is applied by the actor only after the
//! matching durable effect succeeded.
use std::collections::{BTreeMap, HashMap, HashSet};

use super::dto::{BranchHead, EventKind, EventsResult, HeadStamp, SessionError, SessionEvent};
use super::ids::{BranchId, SessionCallId, SessionEventId};

/// One committed event's index row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EventMeta {
    /// Event identity.
    pub(crate) event_id: SessionEventId,
    /// Canonical payload digest spelling.
    pub(crate) digest: String,
    /// Payload byte length.
    pub(crate) bytes: u64,
    /// Event classification.
    pub(crate) kind: EventKind,
    /// Present only for tool kinds.
    pub(crate) call_id: Option<SessionCallId>,
    /// Byte offset of the record inside the branch log.
    pub(crate) offset: u64,
    /// Total encoded record length.
    pub(crate) record_len: u64,
}

impl EventMeta {
    /// Projects one index row to its DTO metadata.
    pub(crate) fn to_session_event(&self) -> SessionEvent {
        SessionEvent {
            event_id: self.event_id.clone(),
            digest: self.digest.clone(),
            bytes: self.bytes,
            kind: self.kind,
            call_id: self.call_id.clone(),
        }
    }
}

/// One branch's in-memory state.
#[derive(Clone, Debug)]
pub(crate) struct BranchLedger {
    /// Committed head.
    pub(crate) head: HeadStamp,
    /// Ordered committed event rows.
    pub(crate) events: Vec<EventMeta>,
    /// Committed prefix length of the branch log.
    pub(crate) committed_bytes: u64,
    /// Tool calls issued without their result yet.
    pub(crate) open_calls: HashSet<SessionCallId>,
}

impl BranchLedger {
    /// Builds a fresh root branch state.
    pub(crate) fn root() -> Self {
        Self {
            head: HeadStamp::Empty,
            events: Vec::new(),
            committed_bytes: 0,
            open_calls: HashSet::new(),
        }
    }
}

/// One actor session projection.
#[derive(Debug, Default)]
pub(crate) struct SessionLedger {
    /// Branches keyed by identity (byte order).
    pub(crate) branches: BTreeMap<BranchId, BranchLedger>,
    /// Committed bytes across every branch log.
    pub(crate) total_bytes: u64,
    /// Single-use event reservations keyed by event identity.
    pub(crate) event_reservations: HashMap<SessionEventId, EventReservationRow>,
    /// Single-use branch reservations keyed by reservation identity.
    pub(crate) branch_reservations: HashMap<super::ids::BranchReservationId, BranchReservationRow>,
}

/// A live single-use event reservation.
#[derive(Clone, Debug)]
pub(crate) struct EventReservationRow {
    /// The issued view.
    pub(crate) view: super::dto::EventReservationView,
    /// The event classification bound at issuance.
    pub(crate) kind: EventKind,
    /// The call identity bound at issuance.
    pub(crate) call_id: Option<SessionCallId>,
}

/// A live single-use branch reservation.
#[derive(Clone, Debug)]
pub(crate) struct BranchReservationRow {
    /// The issued view.
    pub(crate) view: super::dto::BranchReservationView,
}

impl SessionLedger {
    /// Creates one empty ledger skeleton.
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// Returns every branch head in branch-ID byte order.
    pub(crate) fn heads(&self) -> Vec<BranchHead> {
        self.branches
            .iter()
            .map(|(branch_id, branch)| BranchHead {
                branch_id: branch_id.clone(),
                head: branch.head.clone(),
            })
            .collect()
    }

    /// Enforces the full call/result ordering check for one candidate event.
    ///
    /// A tool call must not repeat any call identity already present on the
    /// branch; a tool result must resolve one open tool call; and a call
    /// identity is legal only on tool kinds.
    pub(crate) fn check_ordering(
        branch: &BranchLedger,
        kind: EventKind,
        call_id: Option<&SessionCallId>,
    ) -> Result<(), SessionError> {
        let tool_kind = matches!(kind, EventKind::ToolCall | EventKind::ToolResult);
        match (tool_kind, call_id) {
            (false, None) => Ok(()),
            (false, Some(_)) => Err(SessionError::InvalidArgument),
            (true, None) => Err(SessionError::InvalidArgument),
            (true, Some(call)) => match kind {
                EventKind::ToolCall => {
                    let repeated = branch.events.iter().any(|event| {
                        event
                            .call_id
                            .as_ref()
                            .is_some_and(|existing| existing == call)
                    });
                    if repeated {
                        Err(SessionError::InvalidArgument)
                    } else {
                        Ok(())
                    }
                }
                EventKind::ToolResult => {
                    if branch.open_calls.contains(call) {
                        Ok(())
                    } else {
                        Err(SessionError::InvalidArgument)
                    }
                }
                other => unreachable!("tool kind {other:?} matched above"),
            },
        }
    }

    /// Computes one read page against an immutable snapshot boundary.
    ///
    /// The snapshot must name the current-empty head or one committed event
    /// of the branch; `after` must be `None` or a committed event at or
    /// before the snapshot boundary. The empty snapshot is valid only while
    /// the branch is still empty, matching the stale-cursor rejection.
    pub(crate) fn page(
        &self,
        branch: &BranchId,
        snapshot_head: &HeadStamp,
        after: Option<&SessionEventId>,
        limit: u16,
    ) -> Result<EventsResult, SessionError> {
        let branch_state = self.branches.get(branch).ok_or(SessionError::NotFound)?;
        let end = match snapshot_head {
            HeadStamp::Empty if branch_state.events.is_empty() => 0,
            HeadStamp::Empty => return Err(SessionError::InvalidArgument),
            HeadStamp::Event(event) => {
                Self::index_of(branch_state, event).ok_or(SessionError::InvalidArgument)? + 1
            }
        };
        let start = match after {
            None => 0,
            Some(event) => Self::index_of(branch_state, event).ok_or(SessionError::NotFound)? + 1,
        };
        if start > end {
            return Err(SessionError::InvalidArgument);
        }
        let items = branch_state.events[start..end]
            .iter()
            .take(usize::from(limit))
            .map(EventMeta::to_session_event)
            .collect::<Vec<_>>();
        let returned_end = start + items.len();
        let next = if items.is_empty() || returned_end >= end {
            None
        } else {
            Some(branch_state.events[returned_end - 1].event_id.clone())
        };
        Ok(EventsResult { items, next })
    }

    fn index_of(branch: &BranchLedger, event: &SessionEventId) -> Option<usize> {
        branch.events.iter().position(|row| &row.event_id == event)
    }
}

#[cfg(test)]
mod tests {
    use super::super::dto::{BranchHead, EventKind, HeadStamp, SessionError};
    use super::super::ids::{BranchId, SessionCallId, SessionEventId};
    use super::{BranchLedger, EventMeta, SessionLedger};

    fn id_tail(seed: u8) -> String {
        format!("{seed:02x}23456789abcdef0123456789abcdef")
    }

    fn event_id(seed: u8) -> SessionEventId {
        SessionEventId::parse(&format!("evt1-{}", id_tail(seed))).expect("event id")
    }

    fn call_id(seed: u8) -> SessionCallId {
        SessionCallId::parse(&format!("call1-{}", id_tail(seed))).expect("call id")
    }

    fn branch_id(seed: u8) -> BranchId {
        BranchId::parse(&format!("br1-{}", id_tail(seed))).expect("branch id")
    }

    fn row(seed: u8, kind: EventKind, call: Option<SessionCallId>) -> EventMeta {
        EventMeta {
            event_id: event_id(seed),
            digest: super::super::digest::format_digest(&[seed; 32]),
            bytes: u64::from(seed),
            kind,
            call_id: call,
            offset: 0,
            record_len: 48 + u64::from(seed),
        }
    }

    fn loaded(rows: Vec<EventMeta>) -> (SessionLedger, BranchId) {
        let root = branch_id(0);
        let mut ledger = SessionLedger::empty();
        ledger.branches.insert(root.clone(), BranchLedger::root());
        let branch = ledger.branches.get_mut(&root).expect("root branch");
        for (index, meta) in rows.iter().enumerate() {
            if meta.kind == EventKind::ToolCall {
                branch
                    .open_calls
                    .insert(meta.call_id.clone().expect("call"));
            }
            if meta.kind == EventKind::ToolResult {
                branch
                    .open_calls
                    .remove(&meta.call_id.clone().expect("call"));
            }
            let head = HeadStamp::Event(meta.event_id.clone());
            branch.head = head;
            branch.committed_bytes += meta.record_len;
            branch.events.insert(index, meta.clone());
        }
        (ledger, root)
    }

    #[test]
    fn ordering_check_accepts_paired_tool_flow_only() {
        let root = BranchLedger::root();
        let call = call_id(7);
        assert!(SessionLedger::check_ordering(&root, EventKind::Message, None).is_ok());
        assert_eq!(
            SessionLedger::check_ordering(&root, EventKind::Message, Some(&call)).err(),
            Some(SessionError::InvalidArgument)
        );
        assert_eq!(
            SessionLedger::check_ordering(&root, EventKind::ToolCall, None).err(),
            Some(SessionError::InvalidArgument)
        );
        assert_eq!(
            SessionLedger::check_ordering(&root, EventKind::ToolResult, Some(&call)).err(),
            Some(SessionError::InvalidArgument),
            "orphan result"
        );

        let (ledger, root_id) = loaded(vec![
            row(1, EventKind::Message, None),
            row(2, EventKind::ToolCall, Some(call.clone())),
        ]);
        let branch = ledger.branches.get(&root_id).expect("branch");
        assert!(SessionLedger::check_ordering(branch, EventKind::ToolResult, Some(&call)).is_ok());
        assert_eq!(
            SessionLedger::check_ordering(branch, EventKind::ToolCall, Some(&call)).err(),
            Some(SessionError::InvalidArgument),
            "duplicate call"
        );
    }

    #[test]
    fn heads_are_sorted_by_branch_bytes() {
        let (ledger, _) = loaded(vec![row(1, EventKind::Message, None)]);
        assert_eq!(
            ledger.heads(),
            vec![BranchHead {
                branch_id: branch_id(0),
                head: HeadStamp::Event(event_id(1)),
            }]
        );
    }

    #[test]
    fn paging_walks_an_immutable_snapshot_exactly() {
        let rows: Vec<EventMeta> = (1..=3)
            .map(|seed| row(seed, EventKind::Message, None))
            .collect();
        let (ledger, root) = loaded(rows);
        let snapshot = HeadStamp::Event(event_id(3));

        let first = ledger.page(&root, &snapshot, None, 2).expect("page");
        assert_eq!(first.items.len(), 2);
        assert_eq!(first.next.as_ref(), Some(&event_id(2)));

        let second = ledger
            .page(&root, &snapshot, first.next.as_ref(), 2)
            .expect("page");
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.next, None, "EOF page has no cursor");

        let empty = ledger
            .page(&root, &HeadStamp::Empty, None, 2)
            .expect_err("empty snapshot is stale on a branch with events");
        assert_eq!(empty, SessionError::InvalidArgument);

        let same_spot = ledger
            .page(&root, &snapshot, Some(&event_id(3)), 2)
            .expect("after equal to the snapshot is an empty exclusive window");
        assert!(same_spot.items.is_empty());
        assert_eq!(same_spot.next, None);
        assert_eq!(
            ledger
                .page(&root, &HeadStamp::Event(event_id(1)), Some(&event_id(3)), 2)
                .err(),
            Some(SessionError::InvalidArgument),
            "cursor after the snapshot boundary is stale"
        );
        let zero = ledger.page(&root, &snapshot, None, 0).expect("degrades");
        assert!(zero.items.is_empty());
        assert_eq!(zero.next, None);
    }

    #[test]
    fn paging_rejects_foreign_snapshots_and_cursors() {
        let (ledger, root) = loaded(vec![row(1, EventKind::Message, None)]);
        let foreign_event = event_id(9);
        assert_eq!(
            ledger
                .page(&root, &HeadStamp::Event(foreign_event.clone()), None, 1)
                .err(),
            Some(SessionError::InvalidArgument)
        );
        assert_eq!(
            ledger
                .page(
                    &root,
                    &HeadStamp::Event(event_id(1)),
                    Some(&foreign_event),
                    1
                )
                .err(),
            Some(SessionError::NotFound)
        );
        let missing_branch = branch_id(5);
        assert_eq!(
            ledger
                .page(&missing_branch, &HeadStamp::Empty, None, 1)
                .err(),
            Some(SessionError::NotFound)
        );

        let (fresh, fresh_root) = loaded(vec![]);
        let still_empty = fresh
            .page(&fresh_root, &HeadStamp::Empty, None, 1)
            .expect("empty snapshot on an empty branch");
        assert!(still_empty.items.is_empty());
        assert_eq!(still_empty.next, None);
    }
}
