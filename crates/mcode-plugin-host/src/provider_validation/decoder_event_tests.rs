//! Stateful normalized-event reducer tests.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    CompletionReason, CompletionTerminal, NormalizedEvent, ProviderError, ReasoningDelta,
    ReasoningKind, ReasoningProof, TextDelta, ToolArgumentsDelta, ToolCallEnd, ToolCallStart,
    Usage,
};

use super::ValidationError;
use super::decoder::protocol::{DecoderAction, DecoderReducer, DecoderState};
use super::decoder::reduce_failed_batch_with_cumulative_usage_for_test;
use super::decoder_test_support::{
    accept_end, accept_events, accept_head, consume_all, policy, reducer,
};

fn text(index: u8, value: impl Into<String>) -> NormalizedEvent {
    NormalizedEvent::TextDelta(TextDelta {
        content_index: index,
        text: value.into(),
    })
}

fn reasoning(index: u8, kind: ReasoningKind, value: &str) -> NormalizedEvent {
    NormalizedEvent::ReasoningDelta(ReasoningDelta {
        content_index: index,
        kind,
        text: value.to_owned(),
    })
}

fn proof(index: u8, kind: ReasoningKind, bytes: Vec<u8>) -> NormalizedEvent {
    NormalizedEvent::ReasoningProof(ReasoningProof {
        content_index: index,
        kind,
        proof: bytes,
    })
}

fn start(index: u8, call_id: &str, name: &str) -> NormalizedEvent {
    NormalizedEvent::ToolCallStart(ToolCallStart {
        content_index: index,
        call_id: call_id.to_owned(),
        name: name.to_owned(),
    })
}

fn delta(index: u8, call_id: &str, value: impl Into<String>) -> NormalizedEvent {
    NormalizedEvent::ToolArgumentsDelta(ToolArgumentsDelta {
        content_index: index,
        call_id: call_id.to_owned(),
        delta: value.into(),
    })
}

fn end(index: u8, call_id: &str) -> NormalizedEvent {
    NormalizedEvent::ToolCallEnd(ToolCallEnd {
        content_index: index,
        call_id: call_id.to_owned(),
    })
}

fn terminal(reason: CompletionReason, model: Option<&str>) -> NormalizedEvent {
    NormalizedEvent::Completed(CompletionTerminal {
        reason,
        reported_model: model.map(str::to_owned),
        usage: Usage {
            input_tokens: Some(0),
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: Some(i64::MAX as u64),
        },
    })
}

fn configured(proof_supported: bool) -> DecoderReducer {
    DecoderReducer::new(policy(
        proof_supported,
        &["tool", "other"],
        &["known-model"],
    ))
}

fn emit(reducer: &mut DecoderReducer, events: Vec<NormalizedEvent>) {
    let receipt = accept_events(reducer, events);
    assert!(receipt.close.is_none());
    consume_all(reducer);
}

fn assert_invalid(reducer: &mut DecoderReducer) {
    assert_eq!(reducer.state(), DecoderState::Closed);
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::PublishTerminal(NormalizedEvent::Failed(ProviderError::InvalidArgument))
    ));
}

fn split_chunks(value: &str) -> Vec<String> {
    value
        .as_bytes()
        .chunks(65_536)
        .map(|chunk| String::from_utf8(chunk.to_vec()).expect("ASCII fixture"))
        .collect()
}

fn canonical_object(total_bytes: usize) -> String {
    assert!(total_bytes >= 8);
    format!(r#"{{"x":"{}"}}"#, "a".repeat(total_bytes - 8))
}

fn canonical_array_object(payload_bytes: usize) -> String {
    let values = (0..payload_bytes)
        .step_by(60_000)
        .map(|start| {
            let length = (payload_bytes - start).min(60_000);
            format!(r#""{}""#, "a".repeat(length))
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"x":[{values}]}}"#)
}

fn emit_fragments(reducer: &mut DecoderReducer, index: u8, call_id: &str, value: &str) {
    for chunks in split_chunks(value).chunks(15) {
        emit(
            reducer,
            chunks
                .iter()
                .map(|chunk| delta(index, call_id, chunk.clone()))
                .collect(),
        );
    }
}

#[test]
fn content_indices_are_contiguous_nondecreasing_and_bind_one_kind() {
    let mut reducer = configured(true);
    accept_head(&mut reducer, 200);
    emit(&mut reducer, vec![text(0, "a"), text(0, "b")]);
    emit(
        &mut reducer,
        vec![reasoning(1, ReasoningKind::Thinking, "r")],
    );
    let receipt = accept_events(&mut reducer, vec![text(1, "wrong-kind")]);
    assert!(receipt.close.is_some());
    assert_invalid(&mut reducer);

    let mut gap = configured(true);
    accept_head(&mut gap, 200);
    let receipt = accept_events(&mut gap, vec![text(1, "gap")]);
    assert!(receipt.close.is_some());

    let mut maximum = configured(true);
    accept_head(&mut maximum, 200);
    for index in 0..=63 {
        emit(&mut maximum, vec![text(index, "x")]);
    }
    let receipt = accept_events(&mut maximum, vec![text(64, "x")]);
    assert!(receipt.close.is_some());
}

#[test]
fn advanced_content_index_cannot_reappear() {
    let mut reducer = configured(true);
    accept_head(&mut reducer, 200);
    emit(&mut reducer, vec![text(0, "first")]);
    emit(&mut reducer, vec![text(1, "second")]);
    let receipt = accept_events(&mut reducer, vec![text(0, "late")]);
    assert!(receipt.close.is_some());
}

#[test]
fn reasoning_kind_is_stable_and_deltas_are_nonempty_safe_text() {
    let mut reducer = configured(true);
    accept_head(&mut reducer, 200);
    emit(
        &mut reducer,
        vec![reasoning(0, ReasoningKind::Summary, "summary")],
    );
    let receipt = accept_events(
        &mut reducer,
        vec![reasoning(0, ReasoningKind::Thinking, "crossed")],
    );
    assert!(receipt.close.is_some());

    for value in ["", "bad\rtext"] {
        let mut reducer = configured(true);
        accept_head(&mut reducer, 200);
        let receipt = accept_events(
            &mut reducer,
            vec![reasoning(0, ReasoningKind::Thinking, value)],
        );
        assert!(receipt.close.is_some());
    }
}

#[test]
fn proof_requires_capability_current_reasoning_kind_and_single_occurrence() {
    let mut unsupported = configured(false);
    accept_head(&mut unsupported, 200);
    emit(
        &mut unsupported,
        vec![reasoning(0, ReasoningKind::Thinking, "r")],
    );
    let receipt = accept_events(
        &mut unsupported,
        vec![proof(0, ReasoningKind::Thinking, vec![1])],
    );
    assert!(receipt.close.is_some());

    let mut orphan = configured(true);
    accept_head(&mut orphan, 200);
    let receipt = accept_events(
        &mut orphan,
        vec![proof(0, ReasoningKind::Thinking, vec![1])],
    );
    assert!(receipt.close.is_some());

    let mut duplicate = configured(true);
    accept_head(&mut duplicate, 200);
    emit(
        &mut duplicate,
        vec![
            reasoning(0, ReasoningKind::Thinking, "r"),
            proof(0, ReasoningKind::Thinking, vec![1]),
        ],
    );
    let receipt = accept_events(
        &mut duplicate,
        vec![proof(0, ReasoningKind::Thinking, vec![2])],
    );
    assert!(receipt.close.is_some());

    let mut crossed = configured(true);
    accept_head(&mut crossed, 200);
    emit(
        &mut crossed,
        vec![reasoning(0, ReasoningKind::Thinking, "r")],
    );
    let receipt = accept_events(
        &mut crossed,
        vec![proof(0, ReasoningKind::Summary, vec![1])],
    );
    assert!(receipt.close.is_some());
}

#[test]
fn proof_at_content_index_63_is_accepted_through_the_state_machine() {
    let mut reducer = configured(true);
    accept_head(&mut reducer, 200);
    for index in 0..63 {
        emit(&mut reducer, vec![text(index, "x")]);
    }
    let receipt = accept_events(
        &mut reducer,
        vec![
            reasoning(63, ReasoningKind::Summary, "summary"),
            proof(63, ReasoningKind::Summary, vec![1, 2, 3]),
        ],
    );
    assert_eq!(receipt.accepted_events.len(), 2);
    assert!(receipt.close.is_none());
}

#[test]
fn proof_per_item_and_all_proof_caps_are_exact() {
    let mut reducer = configured(true);
    accept_head(&mut reducer, 200);
    for index in 0..4 {
        emit(
            &mut reducer,
            vec![
                reasoning(index, ReasoningKind::Thinking, "r"),
                proof(index, ReasoningKind::Thinking, vec![1; 65_536]),
            ],
        );
    }
    emit(
        &mut reducer,
        vec![reasoning(4, ReasoningKind::Thinking, "r")],
    );
    let receipt = accept_events(
        &mut reducer,
        vec![proof(4, ReasoningKind::Thinking, vec![1])],
    );
    assert!(receipt.close.is_some());

    let mut oversized = configured(true);
    accept_head(&mut oversized, 200);
    emit(
        &mut oversized,
        vec![reasoning(0, ReasoningKind::Thinking, "r")],
    );
    let receipt = accept_events(
        &mut oversized,
        vec![proof(0, ReasoningKind::Thinking, vec![1; 65_537])],
    );
    assert!(receipt.close.is_some());
}

#[test]
fn non_2xx_failed_batch_counts_toward_cumulative_event_count() {
    assert_eq!(
        reduce_failed_batch_with_cumulative_usage_for_test(65_535, 0),
        Ok(())
    );
    assert_eq!(
        reduce_failed_batch_with_cumulative_usage_for_test(65_536, 0),
        Err(ValidationError::Limit)
    );
}

#[test]
fn non_2xx_failed_batch_counts_toward_cumulative_event_charge() {
    assert_eq!(
        reduce_failed_batch_with_cumulative_usage_for_test(0, 8 * 1_024 * 1_024 - 8),
        Ok(())
    );
    assert_eq!(
        reduce_failed_batch_with_cumulative_usage_for_test(0, 8 * 1_024 * 1_024 - 7),
        Err(ValidationError::Limit)
    );
}

#[test]
fn event_count_accepts_65536_and_rejects_65537() {
    let mut reducer = configured(false);
    accept_head(&mut reducer, 200);
    for _ in 0..4_096 {
        emit(&mut reducer, (0..16).map(|_| text(0, "x")).collect());
    }
    let receipt = accept_events(&mut reducer, vec![text(0, "x")]);
    assert!(matches!(
        receipt
            .close
            .as_ref()
            .and_then(|close| close.terminal.as_ref()),
        Some(NormalizedEvent::Failed(ProviderError::Limit))
    ));
}

#[test]
fn cumulative_event_charge_rejects_the_first_event_over_eight_mib() {
    let mut reducer = configured(false);
    accept_head(&mut reducer, 200);
    for _ in 0..127 {
        emit(&mut reducer, vec![text(0, "x".repeat(65_536))]);
    }
    let receipt = accept_events(&mut reducer, vec![text(0, "x".repeat(65_536))]);
    assert!(matches!(
        receipt
            .close
            .as_ref()
            .and_then(|close| close.terminal.as_ref()),
        Some(NormalizedEvent::Failed(ProviderError::Limit))
    ));
}

#[test]
fn tool_registry_and_request_wide_call_ids_are_sealed() {
    let mut unknown = configured(false);
    accept_head(&mut unknown, 200);
    assert!(
        accept_events(&mut unknown, vec![start(0, "call-1", "missing")])
            .close
            .is_some()
    );

    let mut duplicate = configured(false);
    accept_head(&mut duplicate, 200);
    emit(
        &mut duplicate,
        vec![
            start(0, "call-1", "tool"),
            delta(0, "call-1", "{}"),
            end(0, "call-1"),
        ],
    );
    assert!(
        accept_events(&mut duplicate, vec![start(1, "call-1", "other")])
            .close
            .is_some()
    );
}

#[test]
fn tool_open_state_rejects_second_start_interleave_and_index_advance() {
    for event in [
        start(0, "call-2", "other"),
        start(1, "call-2", "other"),
        text(1, "advance"),
        reasoning(0, ReasoningKind::Thinking, "interleave"),
    ] {
        let mut reducer = configured(false);
        accept_head(&mut reducer, 200);
        emit(&mut reducer, vec![start(0, "call-1", "tool")]);
        assert!(accept_events(&mut reducer, vec![event]).close.is_some());
    }
}

#[test]
fn tool_open_accepts_only_same_index_and_call_id_delta() {
    for event in [
        delta(0, "other-id", "{}"),
        delta(1, "call-1", "{}"),
        end(0, "other-id"),
        end(0, "call-1"),
    ] {
        let mut reducer = configured(false);
        accept_head(&mut reducer, 200);
        emit(&mut reducer, vec![start(0, "call-1", "tool")]);
        assert!(accept_events(&mut reducer, vec![event]).close.is_some());
    }
}

#[test]
fn sealed_tool_index_rejects_late_delta_end_and_second_call() {
    for event in [
        delta(0, "call-1", "{}"),
        end(0, "call-1"),
        start(0, "call-2", "other"),
    ] {
        let mut reducer = configured(false);
        accept_head(&mut reducer, 200);
        emit(
            &mut reducer,
            vec![
                start(0, "call-1", "tool"),
                delta(0, "call-1", "{}"),
                end(0, "call-1"),
            ],
        );
        assert!(accept_events(&mut reducer, vec![event]).close.is_some());
    }
}

#[test]
fn tool_delta_count_accepts_16384_and_rejects_16385() {
    let object = canonical_object(16_384);
    let mut exact = configured(false);
    accept_head(&mut exact, 200);
    emit(&mut exact, vec![start(0, "call-1", "tool")]);
    for chunk in object.as_bytes().chunks(16) {
        emit(
            &mut exact,
            chunk
                .iter()
                .map(|byte| delta(0, "call-1", char::from(*byte).to_string()))
                .collect(),
        );
    }
    emit(&mut exact, vec![end(0, "call-1")]);

    let mut over = configured(false);
    accept_head(&mut over, 200);
    emit(&mut over, vec![start(0, "call-1", "tool")]);
    for _ in 0..1_024 {
        emit(
            &mut over,
            (0..16).map(|_| delta(0, "call-1", "a")).collect(),
        );
    }
    let receipt = accept_events(&mut over, vec![delta(0, "call-1", "a")]);
    assert!(matches!(
        receipt
            .close
            .as_ref()
            .and_then(|close| close.terminal.as_ref()),
        Some(NormalizedEvent::Failed(ProviderError::Limit))
    ));
}

#[test]
fn per_call_and_all_call_argument_byte_caps_are_exact() {
    let object = canonical_array_object(900 * 1_024);
    let mut all_calls = configured(false);
    accept_head(&mut all_calls, 200);
    for (index, call_id) in [(0, "call-1"), (1, "call-2")] {
        emit(&mut all_calls, vec![start(index, call_id, "tool")]);
        emit_fragments(&mut all_calls, index, call_id, &object);
        emit(&mut all_calls, vec![end(index, call_id)]);
    }
    let remaining = 2 * 1_024 * 1_024 - 2 * object.len();
    emit(&mut all_calls, vec![start(2, "call-3", "tool")]);
    emit_fragments(&mut all_calls, 2, "call-3", &"a".repeat(remaining));
    let receipt = accept_events(&mut all_calls, vec![delta(2, "call-3", "x")]);
    assert!(matches!(
        receipt
            .close
            .as_ref()
            .and_then(|close| close.terminal.as_ref()),
        Some(NormalizedEvent::Failed(ProviderError::Limit))
    ));

    let mut per_call = configured(false);
    accept_head(&mut per_call, 200);
    emit(&mut per_call, vec![start(0, "call-1", "tool")]);
    emit_fragments(&mut per_call, 0, "call-1", &"a".repeat(1_024 * 1_024));
    let receipt = accept_events(&mut per_call, vec![delta(0, "call-1", "x")]);
    assert!(receipt.close.is_some());
}

#[test]
fn guest_failed_terminal_rejects_an_open_tool_without_publishing_its_valid_prefix() {
    let mut reducer = configured(false);
    accept_head(&mut reducer, 200);
    emit(&mut reducer, vec![start(0, "call-1", "tool")]);
    accept_end(&mut reducer);

    let receipt = accept_events(
        &mut reducer,
        vec![
            delta(0, "call-1", "{}"),
            NormalizedEvent::Failed(ProviderError::Failed),
        ],
    );
    assert!(matches!(
        receipt
            .close
            .as_ref()
            .and_then(|close| close.terminal.as_ref()),
        Some(NormalizedEvent::Failed(ProviderError::InvalidArgument))
    ));
    assert!(receipt.accepted_events.is_empty());
    assert_eq!(reducer.queue_len(), 0);
}

#[test]
fn terminal_tool_reason_matches_complete_calls_and_rejects_open_calls() {
    let mut no_call = configured(false);
    accept_head(&mut no_call, 200);
    accept_end(&mut no_call);
    assert!(
        accept_events(
            &mut no_call,
            vec![terminal(CompletionReason::ToolUse, None)]
        )
        .close
        .is_some()
    );

    let mut complete = configured(false);
    accept_head(&mut complete, 200);
    emit(
        &mut complete,
        vec![
            start(0, "call-1", "tool"),
            delta(0, "call-1", "{}"),
            end(0, "call-1"),
        ],
    );
    accept_end(&mut complete);
    let receipt = accept_events(
        &mut complete,
        vec![terminal(CompletionReason::ToolUse, None)],
    );
    assert!(matches!(
        receipt
            .close
            .as_ref()
            .and_then(|close| close.terminal.as_ref()),
        Some(NormalizedEvent::Completed(_))
    ));

    let mut wrong_reason = configured(false);
    accept_head(&mut wrong_reason, 200);
    emit(
        &mut wrong_reason,
        vec![
            start(0, "call-1", "tool"),
            delta(0, "call-1", "{}"),
            end(0, "call-1"),
        ],
    );
    accept_end(&mut wrong_reason);
    assert!(
        accept_events(
            &mut wrong_reason,
            vec![terminal(CompletionReason::Stop, None)]
        )
        .close
        .is_some()
    );

    let mut open = configured(false);
    accept_head(&mut open, 200);
    emit(&mut open, vec![start(0, "call-1", "tool")]);
    accept_end(&mut open);
    assert!(
        accept_events(&mut open, vec![terminal(CompletionReason::ToolUse, None)])
            .close
            .is_some()
    );
}

#[test]
fn reported_model_is_retained_only_when_in_the_sealed_catalog() {
    for (reported, expected) in [
        (Some("known-model"), Some("known-model")),
        (Some("unknown-model"), None),
        (None, None),
    ] {
        let mut reducer = configured(false);
        accept_head(&mut reducer, 200);
        accept_end(&mut reducer);
        let receipt = accept_events(
            &mut reducer,
            vec![terminal(CompletionReason::Stop, reported)],
        );
        let Some(NormalizedEvent::Completed(completed)) = receipt
            .close
            .as_ref()
            .and_then(|close| close.terminal.as_ref())
        else {
            panic!("completed terminal");
        };
        assert_eq!(completed.reported_model.as_deref(), expected);
        assert_eq!(completed.usage.input_tokens, Some(0));
    }
}

#[test]
fn non_2xx_emits_no_content_and_only_one_failed_terminal_after_end() {
    let mut content = reducer();
    accept_head(&mut content, 400);
    assert!(
        accept_events(&mut content, vec![text(0, "secret body")])
            .close
            .is_some()
    );

    let mut completed = reducer();
    accept_head(&mut completed, 500);
    accept_end(&mut completed);
    assert!(
        accept_events(&mut completed, vec![terminal(CompletionReason::Stop, None)])
            .close
            .is_some()
    );

    let mut failed = reducer();
    accept_head(&mut failed, 599);
    accept_end(&mut failed);
    let receipt = accept_events(
        &mut failed,
        vec![NormalizedEvent::Failed(ProviderError::Unavailable)],
    );
    assert!(receipt.accepted_events.is_empty());
    assert!(matches!(
        receipt
            .close
            .as_ref()
            .and_then(|close| close.terminal.as_ref()),
        Some(NormalizedEvent::Failed(ProviderError::Unavailable))
    ));
    assert!(matches!(
        failed.next_action(),
        DecoderAction::PublishTerminal(NormalizedEvent::Failed(ProviderError::Unavailable))
    ));
    assert!(matches!(failed.next_action(), DecoderAction::AwaitReceipt));
    failed
        .terminal_published()
        .expect("guest terminal published");
    assert!(matches!(failed.next_action(), DecoderAction::Closed));
}
