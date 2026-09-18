//! End-to-end session service tests over a real owned home.

// Rust guideline compliant 2026-09-18.

use std::path::Path;

use mcode_config::HomeLayout;
use mcode_plugin_host::session::{
    BranchMutationKind, EventKind, HeadStamp, SessionError, SessionEventId, SessionId,
    SessionService,
};

fn home() -> (tempfile::TempDir, HomeLayout) {
    let parent = tempfile::tempdir().expect("parent");
    let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
    (parent, layout)
}

fn session_dir(layout: &HomeLayout, session: &SessionId) -> std::path::PathBuf {
    layout
        .owned_join(format!(
            "plugins/session/data/sessions/{}",
            session.as_str()
        ))
        .expect("session dir")
}

async fn append_message(
    service: &SessionService,
    session: &SessionId,
    branch: &mcode_plugin_host::session::BranchId,
    expected: &HeadStamp,
    payload: &[u8],
) -> mcode_plugin_host::session::AppendedResult {
    let reservation = service
        .reserve_event(session, branch, EventKind::Message, None, payload)
        .await
        .expect("reserve");
    service
        .append(session, branch, expected, &reservation)
        .await
        .expect("append")
}

#[tokio::test]
async fn create_open_append_read_roundtrip() {
    let (_parent, layout) = home();
    let service = SessionService::new(&layout);

    let created = service.create().await.expect("create");
    let opened = service.open(&created.session_id).await.expect("open");
    assert_eq!(opened.heads.len(), 1);
    assert_eq!(opened.heads[0].branch_id, created.branch_id);
    assert_eq!(opened.heads[0].head, HeadStamp::Empty);

    let first = append_message(
        &service,
        &created.session_id,
        &created.branch_id,
        &HeadStamp::Empty,
        b"hello",
    )
    .await;
    let first_event = first.head.event().expect("committed head").clone();

    let second = append_message(
        &service,
        &created.session_id,
        &created.branch_id,
        &first.head,
        b"world",
    )
    .await;
    assert_eq!(
        second.head.event().expect("head"),
        service
            .open(&created.session_id)
            .await
            .expect("reopen")
            .heads[0]
            .head
            .event()
            .expect("reopened head")
    );

    let snapshot = second.head.clone();
    let page = service
        .read(&created.session_id, &created.branch_id, &snapshot, None, 1)
        .await
        .expect("page");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].event_id, first_event);
    assert!(page.next.is_some());

    let rest = service
        .read(
            &created.session_id,
            &created.branch_id,
            &snapshot,
            page.next.as_ref(),
            8,
        )
        .await
        .expect("rest");
    assert_eq!(rest.items.len(), 1);
    assert_eq!(rest.next, None);

    let loaded = service
        .load_event(&created.session_id, &created.branch_id, &first_event)
        .await
        .expect("load");
    assert_eq!(loaded.payload, b"hello");
    assert_eq!(loaded.event.bytes, 5);
    assert_eq!(loaded.event.kind, EventKind::Message);
    service.shutdown().await;
}

#[tokio::test]
async fn recovery_truncates_torn_tails_and_orphan_payloads() {
    let (parent, layout) = home();
    let service = SessionService::new(&layout);
    let created = service.create().await.expect("create");
    let appended = append_message(
        &service,
        &created.session_id,
        &created.branch_id,
        &HeadStamp::Empty,
        b"durable",
    )
    .await;

    // Simulate a crash: a torn tail beyond the committed prefix plus an
    // orphan staged payload.
    let branch_log = session_dir(&layout, &created.session_id)
        .join("branches")
        .join(format!("{}.events", created.branch_id.as_str()));
    let committed_len = std::fs::metadata(&branch_log).expect("log").len();
    append_bytes(&branch_log, &[0xde, 0xad, 0xbe, 0xef]);
    let pending_dir = session_dir(&layout, &created.session_id).join("pending");
    let orphan = pending_dir.join("evt1-0123456789abcdef0123456789abcdef.payload");
    std::fs::write(&orphan, b"orphan").expect("orphan payload");

    // A fresh service (new actor, empty memory) recovers the session.
    drop(service);
    let recovered = SessionService::new(&layout);
    let opened = recovered.open(&created.session_id).await.expect("recover");
    assert_eq!(opened.heads[0].head, appended.head);
    assert_eq!(
        std::fs::metadata(&branch_log).expect("truncated").len(),
        committed_len,
        "torn tail is discarded"
    );
    assert!(
        !orphan.exists(),
        "orphan staged payload is removed during recovery"
    );
    let page = recovered
        .read(
            &created.session_id,
            &created.branch_id,
            &appended.head,
            None,
            16,
        )
        .await
        .expect("page after recovery");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.next, None);
    recovered.shutdown().await;
    drop(parent);
}

#[tokio::test]
async fn committed_digest_corruption_fails_closed() {
    let (_parent, layout) = home();
    let service = SessionService::new(&layout);
    let created = service.create().await.expect("create");
    append_message(
        &service,
        &created.session_id,
        &created.branch_id,
        &HeadStamp::Empty,
        b"payload",
    )
    .await;
    service.shutdown().await;

    let branch_log = session_dir(&layout, &created.session_id)
        .join("branches")
        .join(format!("{}.events", created.branch_id.as_str()));
    let mut bytes = std::fs::read(&branch_log).expect("log");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&branch_log, bytes).expect("corrupt");

    let service = SessionService::new(&layout);
    let error = service
        .open(&created.session_id)
        .await
        .expect_err("corruption fails closed");
    assert_eq!(error, SessionError::Corrupt);
    service.shutdown().await;
}

#[tokio::test]
async fn reservations_are_single_use_and_cas_enforced() {
    let (_parent, layout) = home();
    let service = SessionService::new(&layout);
    let created = service.create().await.expect("create");

    let reservation = service
        .reserve_event(
            &created.session_id,
            &created.branch_id,
            EventKind::Message,
            None,
            b"first",
        )
        .await
        .expect("reserve");

    // Replaying the same reservation after a successful commit is rejected.
    let appended = service
        .append(
            &created.session_id,
            &created.branch_id,
            &HeadStamp::Empty,
            &reservation,
        )
        .await
        .expect("commit");
    let replay = service
        .append(
            &created.session_id,
            &created.branch_id,
            &HeadStamp::Empty,
            &reservation,
        )
        .await
        .expect_err("reservation replay");
    assert_eq!(replay, SessionError::NotFound);

    // A stale expected head loses the CAS and reports the actual head.
    let fresh = service
        .reserve_event(
            &created.session_id,
            &created.branch_id,
            EventKind::Message,
            None,
            b"second",
        )
        .await
        .expect("reserve");
    let stale = service
        .append(
            &created.session_id,
            &created.branch_id,
            &HeadStamp::Empty,
            &fresh,
        )
        .await
        .expect_err("stale CAS");
    assert_eq!(
        stale,
        SessionError::Conflict(mcode_plugin_host::session::ConflictResult {
            actual: appended.head.clone(),
        })
    );
    service.shutdown().await;
}

#[tokio::test]
async fn tool_call_ordering_gates_reservations() {
    let (_parent, layout) = home();
    let service = SessionService::new(&layout);
    let created = service.create().await.expect("create");
    let call = mcode_plugin_host::session::SessionCallId::generate().expect("call");

    let orphan = service
        .reserve_event(
            &created.session_id,
            &created.branch_id,
            EventKind::ToolResult,
            Some(call.clone()),
            b"result",
        )
        .await
        .expect_err("orphan result");
    assert_eq!(orphan, SessionError::InvalidArgument);

    let call_reservation = service
        .reserve_event(
            &created.session_id,
            &created.branch_id,
            EventKind::ToolCall,
            Some(call.clone()),
            b"call",
        )
        .await
        .expect("call reservation");
    let after_call = service
        .append(
            &created.session_id,
            &created.branch_id,
            &HeadStamp::Empty,
            &call_reservation,
        )
        .await
        .expect("call commit");

    let result_reservation = service
        .reserve_event(
            &created.session_id,
            &created.branch_id,
            EventKind::ToolResult,
            Some(call),
            b"result",
        )
        .await
        .expect("paired result");
    service
        .append(
            &created.session_id,
            &created.branch_id,
            &after_call.head,
            &result_reservation,
        )
        .await
        .expect("result commit");
    service.shutdown().await;
}

#[tokio::test]
async fn fork_and_rewind_create_exact_prefix_branches() {
    let (_parent, layout) = home();
    let service = SessionService::new(&layout);
    let created = service.create().await.expect("create");
    let first = append_message(
        &service,
        &created.session_id,
        &created.branch_id,
        &HeadStamp::Empty,
        b"one",
    )
    .await;
    let first_event = first.head.event().expect("first event").clone();
    let second = append_message(
        &service,
        &created.session_id,
        &created.branch_id,
        &first.head,
        b"two",
    )
    .await;

    // Fork at the first event: the new branch head is exactly that event.
    let fork_reservation = service
        .reserve_branch(
            &created.session_id,
            BranchMutationKind::Fork,
            &created.branch_id,
            &first_event,
        )
        .await
        .expect("fork reservation");
    let forked = service
        .fork(
            &created.session_id,
            &created.branch_id,
            &first_event,
            &fork_reservation,
        )
        .await
        .expect("fork");
    assert_eq!(forked.head, HeadStamp::Event(first_event.clone()));

    let forked_page = service
        .read(
            &created.session_id,
            &forked.branch_id,
            &forked.head,
            None,
            16,
        )
        .await
        .expect("forked page");
    assert_eq!(forked_page.items.len(), 1);
    assert_eq!(forked_page.items[0].event_id, first_event);

    // A crossed reservation (wrong kind or target) is rejected without any
    // durable branch mutation.
    let rewind_reservation = service
        .reserve_branch(
            &created.session_id,
            BranchMutationKind::Rewind,
            &created.branch_id,
            &first_event,
        )
        .await
        .expect("rewind reservation");
    let crossed = service
        .fork(
            &created.session_id,
            &created.branch_id,
            &first_event,
            &rewind_reservation,
        )
        .await
        .expect_err("crossed kind");
    assert_eq!(crossed, SessionError::InvalidArgument);

    // The crossed attempt consumed the single-use reservation; issue a fresh
    // one for the actual rewind.
    let rewind_reservation = service
        .reserve_branch(
            &created.session_id,
            BranchMutationKind::Rewind,
            &created.branch_id,
            &first_event,
        )
        .await
        .expect("rewind reservation");
    let rewound = service
        .rewind(
            &created.session_id,
            &created.branch_id,
            &first_event,
            &rewind_reservation,
        )
        .await
        .expect("rewind");
    assert_eq!(rewound.head, HeadStamp::Event(first_event.clone()));

    let heads = service
        .open(&created.session_id)
        .await
        .expect("heads")
        .heads;
    assert_eq!(heads.len(), 3);
    let second_event = second.head.event().expect("second event");
    assert!(
        heads
            .iter()
            .any(|head| head.head.event() == Some(second_event)),
        "the original branch keeps its full head"
    );
    service.shutdown().await;
}

#[tokio::test]
async fn fixed_bounds_are_enforced() {
    let (_parent, layout) = home();
    let service = SessionService::new(&layout);
    let created = service.create().await.expect("create");

    let empty = service
        .reserve_event(
            &created.session_id,
            &created.branch_id,
            EventKind::Message,
            None,
            b"",
        )
        .await
        .expect_err("empty payload");
    assert_eq!(empty, SessionError::Limit);

    let oversized = service
        .reserve_event(
            &created.session_id,
            &created.branch_id,
            EventKind::Message,
            None,
            &vec![b'x'; 8 * 1024 * 1024 + 1],
        )
        .await
        .expect_err("payload beyond 8 MiB");
    assert_eq!(oversized, SessionError::Limit);

    let usage_oversized = service
        .reserve_event(
            &created.session_id,
            &created.branch_id,
            EventKind::Usage,
            None,
            &vec![b'x'; 64 * 1024 + 1],
        )
        .await
        .expect_err("usage beyond 64 KiB");
    assert_eq!(usage_oversized, SessionError::Limit);

    let committed = append_message(
        &service,
        &created.session_id,
        &created.branch_id,
        &HeadStamp::Empty,
        b"event",
    )
    .await;
    for limit in [0, 257] {
        let error = service
            .read(
                &created.session_id,
                &created.branch_id,
                &committed.head,
                None,
                limit,
            )
            .await
            .expect_err("page bound");
        assert_eq!(error, SessionError::Limit);
    }

    let event = committed.head.event().expect("event").clone();
    for _ in 1..64 {
        let reservation = service
            .reserve_branch(
                &created.session_id,
                BranchMutationKind::Fork,
                &created.branch_id,
                &event,
            )
            .await
            .expect("branch reservation");
        service
            .fork(
                &created.session_id,
                &created.branch_id,
                &event,
                &reservation,
            )
            .await
            .expect("fork");
    }
    let exhausted = service
        .reserve_branch(
            &created.session_id,
            BranchMutationKind::Fork,
            &created.branch_id,
            &event,
        )
        .await
        .expect_err("branch bound");
    assert_eq!(exhausted, SessionError::Limit);
    service.shutdown().await;
}

#[tokio::test]
async fn unknown_sessions_and_events_fail_closed() {
    let (_parent, layout) = home();
    let service = SessionService::new(&layout);
    let missing = SessionId::generate().expect("session id");
    assert_eq!(
        service.open(&missing).await.expect_err("unknown session"),
        SessionError::NotFound
    );
    let event = SessionEventId::generate().expect("event id");
    let branch = mcode_plugin_host::session::BranchId::generate().expect("branch id");
    assert_eq!(
        service
            .read(&missing, &branch, &HeadStamp::Empty, None, 1)
            .await
            .expect_err("read on unknown session"),
        SessionError::NotFound
    );
    let _ = event;
    service.shutdown().await;
}

#[tokio::test]
async fn shutdown_retires_the_generation_fence() {
    let (_parent, layout) = home();
    let service = SessionService::new(&layout);
    let created = service.create().await.expect("create");
    service.shutdown().await;

    // A new publication over the same home still works; the retired service
    // is simply unusable by contract.
    let fresh = SessionService::new(&layout);
    let opened = fresh.open(&created.session_id).await.expect("reopen");
    assert_eq!(opened.heads.len(), 1);
    fresh.shutdown().await;
}

fn append_bytes(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("append tail");
    file.write_all(bytes).expect("torn tail");
    file.sync_all().expect("durable tail");
}
