//! Decoder protocol, cumulative-limit, and backpressure tests.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    CompletionReason, CompletionTerminal, DecoderPull, FrameAcceptance, NormalizedEvent,
    ProviderError, ReasoningDelta, ReasoningKind, ResponseFrame, ResponseHead, ResponseMedia,
    TextDelta, UnsupportedFlow, Usage,
};

use super::ValidationError;
use super::decoder::DecoderPolicy;
use super::decoder::protocol::{
    CloseCause, DecoderAction, DecoderReducer, DecoderState, ExternalClose, TransportReceipt,
};
use super::decoder_test_support::{
    accept_data, accept_end, accept_events, accept_head, consume_all, reducer, request_frame,
};

fn text(index: u8, value: &str) -> NormalizedEvent {
    NormalizedEvent::TextDelta(TextDelta {
        content_index: index,
        text: value.to_owned(),
    })
}

fn usage() -> Usage {
    Usage {
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
    }
}

fn completed(reason: CompletionReason) -> NormalizedEvent {
    NormalizedEvent::Completed(CompletionTerminal {
        reason,
        reported_model: None,
        usage: usage(),
    })
}

fn assert_failed(receipt: &super::decoder::protocol::DecoderReceipt, expected: ProviderError) {
    let close = receipt.close.as_ref().expect("close receipt");
    assert!(matches!(
        (close.terminal.as_ref(), expected),
        (
            Some(NormalizedEvent::Failed(ProviderError::InvalidArgument)),
            ProviderError::InvalidArgument
        ) | (
            Some(NormalizedEvent::Failed(ProviderError::Limit)),
            ProviderError::Limit
        ) | (
            Some(NormalizedEvent::Failed(ProviderError::Unavailable)),
            ProviderError::Unavailable
        ) | (
            Some(NormalizedEvent::Failed(ProviderError::Cancelled)),
            ProviderError::Cancelled
        ) | (
            Some(NormalizedEvent::Failed(ProviderError::Failed)),
            ProviderError::Failed
        )
    ));
}

#[test]
fn legal_receipts_visit_the_six_closed_states_without_finished() {
    let mut reducer = reducer();
    assert_eq!(reducer.state(), DecoderState::InitialPull);

    accept_head(&mut reducer, 200);
    accept_data(&mut reducer, vec![1]);
    accept_end(&mut reducer);
    let receipt = accept_events(&mut reducer, vec![completed(CompletionReason::Stop)]);

    assert!(receipt.transitioned);
    assert!(receipt.accepted_events.is_empty());
    assert_eq!(reducer.state(), DecoderState::Closed);
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::PublishTerminal(NormalizedEvent::Completed(_))
    ));
    assert!(matches!(reducer.next_action(), DecoderAction::AwaitReceipt));
    reducer.terminal_published().expect("publish receipt");
    assert!(matches!(reducer.next_action(), DecoderAction::Closed));
}

#[test]
fn terminal_publication_rejects_a_nonempty_queue_without_losing_the_receipt() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    accept_end(&mut reducer);
    accept_events(
        &mut reducer,
        vec![text(0, "queued"), completed(CompletionReason::Stop)],
    );

    assert_eq!(
        reducer.terminal_published(),
        Err(ValidationError::InvalidArgument)
    );
    assert_eq!(reducer.queue_len(), 1);
    reducer.consumer_receipt(1).expect("queued item consumed");
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::PublishTerminal(NormalizedEvent::Completed(_))
    ));
    reducer
        .terminal_published()
        .expect("terminal publication receipt");
}

#[test]
fn terminal_publication_rejects_a_repeated_receipt() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    accept_end(&mut reducer);
    accept_events(&mut reducer, vec![completed(CompletionReason::Stop)]);
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::PublishTerminal(_)
    ));
    reducer
        .terminal_published()
        .expect("first publication receipt");

    assert_eq!(
        reducer.terminal_published(),
        Err(ValidationError::InvalidArgument)
    );
    assert!(matches!(reducer.next_action(), DecoderAction::Closed));
}

#[test]
fn terminal_publication_rejects_a_guest_method_close_without_a_terminal() {
    let mut reducer = reducer();
    reducer.next_action();
    reducer.pull_receipt(Err(ProviderError::Failed));

    assert_eq!(
        reducer.terminal_published(),
        Err(ValidationError::InvalidArgument)
    );
    assert!(matches!(reducer.next_action(), DecoderAction::Closed));
}

#[test]
fn terminal_publication_rejects_the_wrong_in_flight_action_without_consuming_it() {
    let mut reducer = reducer();
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::Pull { limit: 16 }
    ));

    assert_eq!(
        reducer.terminal_published(),
        Err(ValidationError::InvalidArgument)
    );
    assert!(matches!(reducer.next_action(), DecoderAction::AwaitReceipt));
}

#[test]
fn terminal_publication_rejects_a_missing_in_flight_receipt() {
    let mut reducer = reducer();
    reducer.close_external(ExternalClose::Failed);

    assert_eq!(
        reducer.terminal_published(),
        Err(ValidationError::InvalidArgument)
    );
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::PublishTerminal(NormalizedEvent::Failed(ProviderError::Failed))
    ));
}

#[test]
fn an_issued_guest_or_network_action_waits_for_its_receipt() {
    let mut reducer = reducer();
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::Pull { limit: 16 }
    ));
    assert!(matches!(reducer.next_action(), DecoderAction::AwaitReceipt));
    assert_eq!(reducer.pull_count(), 1);
    reducer.pull_receipt(Ok(DecoderPull::NeedFrame));
    assert!(matches!(reducer.next_action(), DecoderAction::ReadFrame));
    assert!(matches!(reducer.next_action(), DecoderAction::AwaitReceipt));
}

#[test]
fn initial_pull_accepts_only_need_frame() {
    let mut reducer = reducer();
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::Pull { limit: 16 }
    ));
    let receipt = reducer.pull_receipt(Ok(DecoderPull::Events(vec![text(0, "x")])));
    assert_failed(&receipt, ProviderError::InvalidArgument);
    assert_eq!(reducer.state(), DecoderState::Closed);
}

#[test]
fn pull_and_push_without_the_table_action_protocol_close() {
    let mut pull = reducer();
    let receipt = pull.pull_receipt(Ok(DecoderPull::NeedFrame));
    assert_failed(&receipt, ProviderError::InvalidArgument);

    let mut push = reducer();
    let receipt = push.push_receipt(Ok(FrameAcceptance::Accepted));
    assert_failed(&receipt, ProviderError::InvalidArgument);

    let mut frame = reducer();
    let receipt = frame.transport_receipt(TransportReceipt::Frame(ResponseFrame::End));
    assert_failed(&receipt, ProviderError::InvalidArgument);
}

#[test]
fn frame_order_seals_one_head_and_one_end() {
    for frame in [ResponseFrame::Data(vec![1]), ResponseFrame::End] {
        let mut before_head = reducer();
        assert!(matches!(
            before_head.next_action(),
            DecoderAction::Pull { .. }
        ));
        before_head.pull_receipt(Ok(DecoderPull::NeedFrame));
        assert!(matches!(
            before_head.next_action(),
            DecoderAction::ReadFrame
        ));
        let receipt = before_head.transport_receipt(TransportReceipt::Frame(frame));
        assert_failed(&receipt, ProviderError::InvalidArgument);
    }

    let mut repeated_head = reducer();
    accept_head(&mut repeated_head, 200);
    request_frame(&mut repeated_head);
    assert!(matches!(
        repeated_head.next_action(),
        DecoderAction::ReadFrame
    ));
    let receipt = repeated_head.transport_receipt(TransportReceipt::Frame(ResponseFrame::Head(
        ResponseHead {
            status: 200,
            media: ResponseMedia::Json,
        },
    )));
    assert_failed(&receipt, ProviderError::InvalidArgument);

    let mut repeated_end = reducer();
    accept_head(&mut repeated_end, 200);
    accept_end(&mut repeated_end);
    let receipt = repeated_end.transport_receipt(TransportReceipt::Frame(ResponseFrame::End));
    assert_failed(&receipt, ProviderError::InvalidArgument);
}

#[test]
fn invalid_head_and_frame_boundaries_close_before_guest_push() {
    for status in [199, 600] {
        let mut reducer = reducer();
        reducer.next_action();
        reducer.pull_receipt(Ok(DecoderPull::NeedFrame));
        reducer.next_action();
        let receipt =
            reducer.transport_receipt(TransportReceipt::Frame(ResponseFrame::Head(ResponseHead {
                status,
                media: ResponseMedia::EventStream,
            })));
        assert_failed(&receipt, ProviderError::InvalidArgument);
        assert!(matches!(
            reducer.next_action(),
            DecoderAction::PublishTerminal(_)
        ));
    }

    let mut empty = reducer();
    accept_head(&mut empty, 200);
    request_frame(&mut empty);
    empty.next_action();
    let receipt = empty.transport_receipt(TransportReceipt::Frame(ResponseFrame::Data(Vec::new())));
    assert_failed(&receipt, ProviderError::InvalidArgument);

    let mut oversized = reducer();
    accept_head(&mut oversized, 200);
    request_frame(&mut oversized);
    oversized.next_action();
    let receipt = oversized.transport_receipt(TransportReceipt::Frame(ResponseFrame::Data(vec![
        0;
        65_537
    ])));
    assert_failed(&receipt, ProviderError::Limit);
}

#[test]
fn data_frame_count_accepts_1024_and_rejects_1025() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    for _ in 0..1_024 {
        accept_data(&mut reducer, vec![0]);
    }

    request_frame(&mut reducer);
    reducer.next_action();
    let receipt = reducer.transport_receipt(TransportReceipt::Frame(ResponseFrame::Data(vec![0])));
    assert_failed(&receipt, ProviderError::Limit);
}

#[test]
fn success_and_error_cumulative_byte_caps_reject_n_plus_one() {
    let mut success = reducer();
    accept_head(&mut success, 200);
    for _ in 0..256 {
        accept_data(&mut success, vec![0; 65_536]);
    }
    request_frame(&mut success);
    success.next_action();
    let receipt = success.transport_receipt(TransportReceipt::Frame(ResponseFrame::Data(vec![0])));
    assert_failed(&receipt, ProviderError::Limit);

    let mut error = reducer();
    accept_head(&mut error, 400);
    accept_data(&mut error, vec![0; 65_536]);
    request_frame(&mut error);
    error.next_action();
    let receipt = error.transport_receipt(TransportReceipt::Frame(ResponseFrame::Data(vec![0])));
    assert_failed(&receipt, ProviderError::Limit);
}

#[test]
fn pull_65537_closes_without_issuing_the_guest_action() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    for _ in 1..65_536 {
        let receipt = accept_events(&mut reducer, vec![text(0, "x")]);
        assert!(receipt.close.is_none());
        consume_all(&mut reducer);
    }
    assert_eq!(reducer.pull_count(), 65_536);

    assert!(matches!(
        reducer.next_action(),
        DecoderAction::PublishTerminal(NormalizedEvent::Failed(ProviderError::Limit))
    ));
    assert_eq!(reducer.pull_count(), 65_536);
    assert_eq!(reducer.state(), DecoderState::Closed);
}

#[test]
fn terminal_requires_end_and_batch_last() {
    let mut early = reducer();
    accept_head(&mut early, 200);
    let receipt = accept_events(&mut early, vec![completed(CompletionReason::Stop)]);
    assert_failed(&receipt, ProviderError::InvalidArgument);

    let mut nonfinal = reducer();
    accept_head(&mut nonfinal, 200);
    accept_end(&mut nonfinal);
    let receipt = accept_events(
        &mut nonfinal,
        vec![completed(CompletionReason::Stop), text(0, "late")],
    );
    assert_failed(&receipt, ProviderError::InvalidArgument);
}

#[test]
fn final_batch_enqueues_preceding_items_before_publishing_its_terminal() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    accept_end(&mut reducer);
    let receipt = accept_events(
        &mut reducer,
        vec![text(0, "last text"), completed(CompletionReason::Stop)],
    );

    assert_eq!(receipt.accepted_events.len(), 1);
    assert_eq!(reducer.queue_len(), 1);
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::WaitForConsumer
    ));
    reducer
        .consumer_receipt(1)
        .expect("preceding item consumed");
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::PublishTerminal(NormalizedEvent::Completed(_))
    ));
}

#[test]
fn two_xx_failed_terminal_without_an_open_tool_drains_before_publication() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    accept_end(&mut reducer);
    let receipt = accept_events(
        &mut reducer,
        vec![
            text(0, "accepted prefix"),
            NormalizedEvent::Failed(ProviderError::Failed),
        ],
    );

    assert_eq!(receipt.accepted_events.len(), 1);
    assert!(matches!(
        receipt
            .close
            .as_ref()
            .and_then(|close| close.terminal.as_ref()),
        Some(NormalizedEvent::Failed(ProviderError::Failed))
    ));
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::WaitForConsumer
    ));
    reducer
        .consumer_receipt(1)
        .expect("accepted prefix drained");
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::PublishTerminal(NormalizedEvent::Failed(ProviderError::Failed))
    ));
    reducer
        .terminal_published()
        .expect("guest failed terminal published");
    assert!(matches!(reducer.next_action(), DecoderAction::Closed));
}

#[test]
fn successful_terminal_rejects_repeated_terminal_late_event_data_and_end_without_new_effects() {
    for late in [ResponseFrame::Data(vec![1]), ResponseFrame::End] {
        let mut reducer = reducer();
        accept_head(&mut reducer, 200);
        accept_end(&mut reducer);
        let first = accept_events(&mut reducer, vec![completed(CompletionReason::Stop)]);
        assert!(first.transitioned);

        let frame = reducer.transport_receipt(TransportReceipt::Frame(late));
        assert!(!frame.transitioned);
        assert!(matches!(
            frame
                .close
                .as_ref()
                .and_then(|close| close.terminal.as_ref()),
            Some(NormalizedEvent::Completed(_))
        ));
        for event in [completed(CompletionReason::Stop), text(0, "late content")] {
            let receipt = reducer.pull_receipt(Ok(DecoderPull::Events(vec![event])));
            assert!(!receipt.transitioned);
            assert!(receipt.accepted_events.is_empty());
            assert!(matches!(
                receipt
                    .close
                    .as_ref()
                    .and_then(|close| close.terminal.as_ref()),
                Some(NormalizedEvent::Completed(_))
            ));
        }
        let late_method_error = reducer.pull_receipt(Err(ProviderError::Failed));
        assert!(!late_method_error.transitioned);
        assert!(matches!(
            late_method_error
                .close
                .as_ref()
                .and_then(|close| close.terminal.as_ref()),
            Some(NormalizedEvent::Completed(_))
        ));
    }
}

#[test]
fn need_frame_after_end_and_late_receipts_are_protocol_failures_without_second_terminal() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    accept_end(&mut reducer);
    assert!(matches!(reducer.next_action(), DecoderAction::Pull { .. }));
    let first = reducer.pull_receipt(Ok(DecoderPull::NeedFrame));
    assert_failed(&first, ProviderError::InvalidArgument);

    let late = reducer.pull_receipt(Ok(DecoderPull::Events(vec![completed(
        CompletionReason::Stop,
    )])));
    assert!(!late.transitioned);
    assert!(late.accepted_events.is_empty());
    assert_failed(&late, ProviderError::InvalidArgument);
}

#[test]
fn unsupported_flow_method_error_preserves_its_closed_payload() {
    let mut reducer = reducer();
    reducer.next_action();
    let receipt = reducer.pull_receipt(Err(ProviderError::UnsupportedFlow(
        UnsupportedFlow::ResponseMedia,
    )));

    assert!(matches!(
        receipt.close.as_ref().map(|close| &close.cause),
        Some(CloseCause::GuestMethod(ProviderError::UnsupportedFlow(
            UnsupportedFlow::ResponseMedia
        )))
    ));
    assert!(
        receipt
            .close
            .as_ref()
            .is_some_and(|close| close.terminal.is_none())
    );
    assert!(matches!(reducer.next_action(), DecoderAction::Closed));
}

#[test]
fn pull_method_error_is_the_sole_stable_guest_failure_receipt() {
    let mut reducer = reducer();
    reducer.next_action();
    let first = reducer.pull_receipt(Err(ProviderError::Unavailable));
    assert!(first.transitioned);
    assert!(matches!(
        first.close.as_ref().map(|close| &close.cause),
        Some(CloseCause::GuestMethod(ProviderError::Unavailable))
    ));
    assert!(
        first
            .close
            .as_ref()
            .is_some_and(|close| close.terminal.is_none())
    );
    assert!(matches!(reducer.next_action(), DecoderAction::Closed));

    let repeated = reducer.pull_receipt(Err(ProviderError::Failed));
    assert!(!repeated.transitioned);
    assert!(repeated.accepted_events.is_empty());
    assert!(matches!(
        repeated.close.as_ref().map(|close| &close.cause),
        Some(CloseCause::GuestMethod(ProviderError::Unavailable))
    ));
    assert!(
        repeated
            .close
            .as_ref()
            .is_some_and(|close| close.terminal.is_none())
    );
    let late_host_close = reducer.close_external(ExternalClose::Cancelled);
    assert!(!late_host_close.transitioned);
    assert!(matches!(
        late_host_close.close.as_ref().map(|close| &close.cause),
        Some(CloseCause::GuestMethod(ProviderError::Unavailable))
    ));
    assert!(
        late_host_close
            .close
            .as_ref()
            .is_some_and(|close| close.terminal.is_none())
    );
    assert!(matches!(reducer.next_action(), DecoderAction::Closed));
}

#[test]
fn queued_accepted_items_drain_before_method_error_closes_without_terminal() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    let receipt = accept_events(&mut reducer, vec![text(0, "first"), text(0, "second")]);
    assert_eq!(receipt.accepted_events.len(), 2);
    reducer.consumer_receipt(1).expect("first item consumed");
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::Pull { limit: 15 }
    ));

    let close = reducer.pull_receipt(Err(ProviderError::Failed));
    assert!(close.transitioned);
    assert!(
        close
            .close
            .as_ref()
            .is_some_and(|receipt| receipt.terminal.is_none())
    );
    assert_eq!(reducer.queue_len(), 1);
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::WaitForConsumer
    ));
    reducer.consumer_receipt(1).expect("second item consumed");
    assert!(matches!(reducer.next_action(), DecoderAction::Closed));
}

#[test]
fn transport_eof_and_cancel_close_at_required_frame() {
    let mut head_eof = reducer();
    head_eof.next_action();
    head_eof.pull_receipt(Ok(DecoderPull::NeedFrame));
    head_eof.next_action();
    let receipt = head_eof.transport_receipt(TransportReceipt::Eof);
    assert_failed(&receipt, ProviderError::Unavailable);

    let mut body_cancel = reducer();
    accept_head(&mut body_cancel, 200);
    request_frame(&mut body_cancel);
    body_cancel.next_action();
    let receipt = body_cancel.transport_receipt(TransportReceipt::Cancelled);
    assert_failed(&receipt, ProviderError::Cancelled);
}

#[test]
fn external_close_first_winner_is_stable_and_has_no_guest_followup() {
    let mut deadline = reducer();
    let first = deadline.close_external(ExternalClose::Deadline);
    assert_failed(&first, ProviderError::Cancelled);
    let loser = deadline.close_external(ExternalClose::Trap);
    assert!(!loser.transitioned);
    assert_failed(&loser, ProviderError::Cancelled);
    let late_method_error = deadline.pull_receipt(Err(ProviderError::Failed));
    assert!(!late_method_error.transitioned);
    assert_failed(&late_method_error, ProviderError::Cancelled);
    assert!(matches!(
        late_method_error.close.as_ref().map(|close| &close.cause),
        Some(CloseCause::Deadline)
    ));
    assert!(matches!(
        deadline.next_action(),
        DecoderAction::PublishTerminal(_)
    ));

    for (cause, expected) in [
        (ExternalClose::Cancelled, ProviderError::Cancelled),
        (ExternalClose::Trap, ProviderError::Failed),
        (ExternalClose::Unavailable, ProviderError::Unavailable),
        (ExternalClose::Failed, ProviderError::Failed),
    ] {
        let mut reducer = reducer();
        let receipt = reducer.close_external(cause);
        assert!(receipt.transitioned);
        assert_failed(&receipt, expected);
    }
}

#[test]
fn queue_capacity_drives_free_zero_one_and_sixteen_actions() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::Pull { limit: 16 }
    ));
    let events = (0..16).map(|_| text(0, "x")).collect();
    let receipt = reducer.pull_receipt(Ok(DecoderPull::Events(events)));
    assert_eq!(receipt.accepted_events.len(), 16);
    assert_eq!(reducer.queue_len(), 16);
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::WaitForConsumer
    ));

    reducer.consumer_receipt(1).expect("one consumed");
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::Pull { limit: 1 }
    ));
    reducer.pull_receipt(Ok(DecoderPull::NeedFrame));
    assert_eq!(reducer.state(), DecoderState::NeedBody);
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::WaitForConsumer
    ));
    reducer.consumer_receipt(15).expect("queue emptied");
    assert!(matches!(reducer.next_action(), DecoderAction::ReadFrame));
}

#[test]
fn consumer_receipt_rejects_zero_without_consuming_the_queue() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    accept_events(&mut reducer, vec![text(0, "queued")]);

    assert_eq!(
        reducer.consumer_receipt(0),
        Err(ValidationError::InvalidArgument)
    );
    assert_eq!(reducer.queue_len(), 1);
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::WaitForConsumer
    ));
}

#[test]
fn consumer_receipt_rejects_a_count_above_the_queue() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    accept_events(&mut reducer, vec![text(0, "queued")]);

    assert_eq!(
        reducer.consumer_receipt(2),
        Err(ValidationError::InvalidArgument)
    );
    assert_eq!(reducer.queue_len(), 1);
    reducer.consumer_receipt(1).expect("queued item consumed");
}

#[test]
fn every_enqueued_batch_waits_before_another_pull() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    let receipt = accept_events(&mut reducer, vec![text(0, "x")]);
    assert_eq!(receipt.accepted_events.len(), 1);
    assert_eq!(reducer.queue_len(), 1);
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::WaitForConsumer
    ));
    reducer.consumer_receipt(1).expect("consumer capacity");
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::Pull { limit: 16 }
    ));
}

#[test]
fn returned_queue_17_is_a_protocol_shape_failure() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    reducer.next_action();
    let events = (0..17).map(|_| text(0, "x")).collect();
    let receipt = reducer.pull_receipt(Ok(DecoderPull::Events(events)));
    assert_failed(&receipt, ProviderError::InvalidArgument);
    assert!(matches!(
        receipt.close.as_ref().map(|close| &close.cause),
        Some(CloseCause::Protocol)
    ));
    assert!(receipt.accepted_events.is_empty());
    assert_eq!(reducer.queue_len(), 0);
}

#[test]
fn events_over_a_free_capacity_of_one_are_a_protocol_shape_failure() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    let receipt = accept_events(&mut reducer, (0..16).map(|_| text(0, "x")).collect());
    assert_eq!(receipt.accepted_events.len(), 16);
    reducer.consumer_receipt(1).expect("one free queue slot");
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::Pull { limit: 1 }
    ));

    let receipt = reducer.pull_receipt(Ok(DecoderPull::Events(vec![
        text(0, "first"),
        text(0, "second"),
    ])));
    assert_failed(&receipt, ProviderError::InvalidArgument);
    assert!(matches!(
        receipt.close.as_ref().map(|close| &close.cause),
        Some(CloseCause::Protocol)
    ));
    assert!(receipt.accepted_events.is_empty());
    assert_eq!(reducer.queue_len(), 15);
}

#[test]
fn invalid_batch_does_not_enqueue_its_valid_prefix() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    let receipt = accept_events(
        &mut reducer,
        vec![
            text(0, "valid prefix"),
            NormalizedEvent::ReasoningDelta(ReasoningDelta {
                content_index: 0,
                kind: ReasoningKind::Thinking,
                text: "wrong kind".to_owned(),
            }),
        ],
    );
    assert_failed(&receipt, ProviderError::InvalidArgument);
    assert!(receipt.accepted_events.is_empty());
    assert_eq!(reducer.queue_len(), 0);
}

#[test]
fn accepted_items_drain_before_a_synthesized_terminal_is_published() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    let receipt = accept_events(&mut reducer, vec![text(0, "queued")]);
    assert_eq!(receipt.accepted_events.len(), 1);
    let close = reducer.close_external(ExternalClose::Cancelled);
    assert_failed(&close, ProviderError::Cancelled);
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::WaitForConsumer
    ));
    reducer.consumer_receipt(1).expect("queued item consumed");
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::PublishTerminal(NormalizedEvent::Failed(ProviderError::Cancelled))
    ));
    assert!(matches!(reducer.next_action(), DecoderAction::AwaitReceipt));
    reducer
        .terminal_published()
        .expect("host terminal published");
    assert!(matches!(reducer.next_action(), DecoderAction::Closed));
}

#[test]
fn byte_at_a_time_data_is_accepted_and_head_disconnect_closes() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    accept_data(&mut reducer, vec![1]);
    accept_data(&mut reducer, vec![2]);

    let mut disconnected = DecoderReducer::new(DecoderPolicy::empty());
    accept_head(&mut disconnected, 200);
    request_frame(&mut disconnected);
    disconnected.next_action();
    let receipt = disconnected.transport_receipt(TransportReceipt::Eof);
    assert_failed(&receipt, ProviderError::Unavailable);
}

#[test]
fn data_push_method_error_does_not_accept_the_pending_frame() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    request_frame(&mut reducer);
    assert!(matches!(reducer.next_action(), DecoderAction::ReadFrame));
    reducer.transport_receipt(TransportReceipt::Frame(ResponseFrame::Data(vec![1, 2, 3])));
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::PushFrame(ResponseFrame::Data(data)) if data == vec![1, 2, 3]
    ));

    let receipt = reducer.push_receipt(Err(ProviderError::Unavailable));
    assert!(matches!(
        receipt.close.as_ref().map(|close| &close.cause),
        Some(CloseCause::GuestMethod(ProviderError::Unavailable))
    ));
    assert!(receipt.accepted_events.is_empty());
    assert!(
        receipt
            .close
            .as_ref()
            .is_some_and(|close| close.terminal.is_none())
    );
    assert_eq!(reducer.state(), DecoderState::Closed);
    assert!(matches!(reducer.next_action(), DecoderAction::Closed));
}

#[test]
fn end_push_method_error_does_not_accept_the_pending_frame() {
    let mut reducer = reducer();
    accept_head(&mut reducer, 200);
    request_frame(&mut reducer);
    assert!(matches!(reducer.next_action(), DecoderAction::ReadFrame));
    reducer.transport_receipt(TransportReceipt::Frame(ResponseFrame::End));
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::PushFrame(ResponseFrame::End)
    ));

    let receipt = reducer.push_receipt(Err(ProviderError::Cancelled));
    assert!(matches!(
        receipt.close.as_ref().map(|close| &close.cause),
        Some(CloseCause::GuestMethod(ProviderError::Cancelled))
    ));
    assert!(receipt.accepted_events.is_empty());
    assert!(
        receipt
            .close
            .as_ref()
            .is_some_and(|close| close.terminal.is_none())
    );
    assert_eq!(reducer.state(), DecoderState::Closed);
    assert!(matches!(reducer.next_action(), DecoderAction::Closed));
}

#[test]
fn head_push_method_error_does_not_accept_the_pending_frame() {
    let mut reducer = reducer();
    reducer.next_action();
    reducer.pull_receipt(Ok(DecoderPull::NeedFrame));
    reducer.next_action();
    reducer.transport_receipt(TransportReceipt::Frame(ResponseFrame::Head(ResponseHead {
        status: 200,
        media: ResponseMedia::Json,
    })));
    reducer.next_action();
    let receipt = reducer.push_receipt(Err(ProviderError::Failed));
    assert!(receipt.transitioned);
    assert!(matches!(
        receipt.close.as_ref().map(|close| &close.cause),
        Some(CloseCause::GuestMethod(ProviderError::Failed))
    ));
    assert!(
        receipt
            .close
            .as_ref()
            .is_some_and(|close| close.terminal.is_none())
    );
    assert_eq!(reducer.state(), DecoderState::Closed);
    assert!(matches!(reducer.next_action(), DecoderAction::Closed));
}
