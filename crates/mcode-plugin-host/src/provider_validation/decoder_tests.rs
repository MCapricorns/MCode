//! Decoder-local frame, event, terminal, and usage tests.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    CompletionReason, CompletionTerminal, DecoderPull, NormalizedEvent, ProviderError,
    ReasoningKind, ReasoningProof, ResponseFrame, ResponseHead, ResponseMedia, TextDelta,
    ToolArgumentsDelta, ToolCallEnd, ToolCallStart, UnsupportedFlow, Usage,
};

use super::ValidationError;
use super::decoder::{
    validate_decoder_pull, validate_normalized_event, validate_pull_limit, validate_response_frame,
};

fn text_event(text: &str) -> NormalizedEvent {
    NormalizedEvent::TextDelta(TextDelta {
        content_index: 0,
        text: text.to_owned(),
    })
}

fn terminal(usage: Usage) -> NormalizedEvent {
    NormalizedEvent::Completed(CompletionTerminal {
        reason: CompletionReason::Stop,
        reported_model: Some("model".to_owned()),
        usage,
    })
}

fn no_usage() -> Usage {
    Usage {
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
    }
}

#[test]
fn pull_limit_covers_zero_one_n_and_n_plus_one() {
    assert_eq!(validate_pull_limit(0), Err(ValidationError::Limit));
    assert!(validate_pull_limit(1).is_ok());
    assert!(validate_pull_limit(16).is_ok());
    assert_eq!(validate_pull_limit(17), Err(ValidationError::Limit));
}

#[test]
fn frame_head_and_data_local_bounds_are_exact() {
    assert!(
        validate_response_frame(&ResponseFrame::Head(ResponseHead {
            status: 200,
            media: ResponseMedia::Json,
        }))
        .is_ok()
    );
    assert!(
        validate_response_frame(&ResponseFrame::Head(ResponseHead {
            status: 599,
            media: ResponseMedia::EventStream,
        }))
        .is_ok()
    );
    assert!(
        validate_response_frame(&ResponseFrame::Head(ResponseHead {
            status: 199,
            media: ResponseMedia::Json,
        }))
        .is_err()
    );
    assert!(validate_response_frame(&ResponseFrame::Data(vec![])).is_err());
    assert!(validate_response_frame(&ResponseFrame::Data(vec![0])).is_ok());
    assert!(validate_response_frame(&ResponseFrame::Data(vec![0; 65_536])).is_ok());
    assert_eq!(
        validate_response_frame(&ResponseFrame::Data(vec![0; 65_537])),
        Err(ValidationError::Limit)
    );
    assert!(validate_response_frame(&ResponseFrame::End).is_ok());
}

#[test]
fn event_text_proof_and_content_index_boundaries_are_local() {
    assert!(validate_normalized_event(&text_event("x")).is_ok());
    assert!(validate_normalized_event(&text_event("")).is_err());
    assert!(validate_normalized_event(&text_event(&"x".repeat(65_536))).is_ok());
    assert!(validate_normalized_event(&text_event(&"x".repeat(65_537))).is_err());

    let proof = |index, bytes| {
        NormalizedEvent::ReasoningProof(ReasoningProof {
            content_index: index,
            kind: ReasoningKind::Thinking,
            proof: bytes,
        })
    };
    assert!(validate_normalized_event(&proof(63, vec![1])).is_ok());
    assert!(validate_normalized_event(&proof(64, vec![1])).is_err());
    assert!(validate_normalized_event(&proof(0, vec![])).is_err());
    assert!(validate_normalized_event(&proof(0, vec![1; 65_537])).is_err());
}

#[test]
fn tool_event_fields_apply_tracking_label_and_delta_bounds() {
    let start = NormalizedEvent::ToolCallStart(ToolCallStart {
        content_index: 63,
        call_id: format!("r{}z", "-".repeat(126)),
        name: "n".repeat(128),
    });
    assert!(validate_normalized_event(&start).is_ok());

    for invalid_start in [
        NormalizedEvent::ToolCallStart(ToolCallStart {
            content_index: 64,
            call_id: "bad/id".to_owned(),
            name: "tool\nname".to_owned(),
        }),
        NormalizedEvent::ToolCallStart(ToolCallStart {
            content_index: 0,
            call_id: "call-1".to_owned(),
            name: "n".repeat(129),
        }),
    ] {
        assert!(validate_normalized_event(&invalid_start).is_err());
    }

    let one_byte_delta = NormalizedEvent::ToolArgumentsDelta(ToolArgumentsDelta {
        content_index: 0,
        call_id: "call-1".to_owned(),
        delta: "x".to_owned(),
    });
    assert!(validate_normalized_event(&one_byte_delta).is_ok());
    let empty_delta = NormalizedEvent::ToolArgumentsDelta(ToolArgumentsDelta {
        content_index: 0,
        call_id: "call-1".to_owned(),
        delta: String::new(),
    });
    assert!(validate_normalized_event(&empty_delta).is_err());

    let delta = NormalizedEvent::ToolArgumentsDelta(ToolArgumentsDelta {
        content_index: 0,
        call_id: "call-1".to_owned(),
        delta: "x".repeat(65_536),
    });
    assert!(validate_normalized_event(&delta).is_ok());
    let oversized_delta = NormalizedEvent::ToolArgumentsDelta(ToolArgumentsDelta {
        content_index: 0,
        call_id: "call-1".to_owned(),
        delta: "x".repeat(65_537),
    });
    assert_eq!(
        validate_normalized_event(&oversized_delta),
        Err(ValidationError::Limit)
    );

    let end = NormalizedEvent::ToolCallEnd(ToolCallEnd {
        content_index: 0,
        call_id: "call-1".to_owned(),
    });
    assert!(validate_normalized_event(&end).is_ok());
}

#[test]
fn event_batches_are_nonempty_bounded_by_pull_and_logical_charge() {
    assert!(validate_decoder_pull(&DecoderPull::NeedFrame, 1).is_ok());
    assert!(validate_decoder_pull(&DecoderPull::Events(vec![]), 1).is_err());
    assert!(validate_decoder_pull(&DecoderPull::Events(vec![text_event("x")]), 1).is_ok());
    assert_eq!(
        validate_decoder_pull(
            &DecoderPull::Events(vec![text_event("x"), text_event("y")]),
            1
        ),
        Err(ValidationError::InvalidArgument)
    );

    let maximum_count = DecoderPull::Events(vec![text_event("x"); 16]);
    assert!(validate_decoder_pull(&maximum_count, 16).is_ok());
    let over_charge = DecoderPull::Events(vec![text_event(&"x".repeat(65_536)); 16]);
    assert_eq!(
        validate_decoder_pull(&over_charge, 16),
        Err(ValidationError::Limit)
    );
}

#[test]
fn terminal_usage_preserves_independent_zeroes_and_rejects_each_overflow() {
    let exact = terminal(Usage {
        input_tokens: Some(0),
        output_tokens: Some(i64::MAX as u64),
        cache_read_tokens: Some(0),
        cache_write_tokens: Some(1),
    });
    assert!(validate_normalized_event(&exact).is_ok());

    for usage in [
        Usage {
            input_tokens: Some(i64::MAX as u64 + 1),
            ..no_usage()
        },
        Usage {
            output_tokens: Some(i64::MAX as u64 + 1),
            ..no_usage()
        },
        Usage {
            cache_read_tokens: Some(i64::MAX as u64 + 1),
            ..no_usage()
        },
        Usage {
            cache_write_tokens: Some(i64::MAX as u64 + 1),
            ..no_usage()
        },
    ] {
        assert_eq!(
            validate_normalized_event(&terminal(usage)),
            Err(ValidationError::Limit)
        );
    }
}

#[test]
fn stable_failure_events_have_no_untrusted_text_payload() {
    for error in [
        ProviderError::InvalidArgument,
        ProviderError::Limit,
        ProviderError::Unavailable,
        ProviderError::Cancelled,
        ProviderError::Failed,
    ] {
        assert!(validate_normalized_event(&NormalizedEvent::Failed(error)).is_ok());
    }
}

#[test]
fn unsupported_flow_failure_payload_is_closed_and_stable() {
    for flow in [
        UnsupportedFlow::CatalogSource,
        UnsupportedFlow::Authentication,
        UnsupportedFlow::Model,
        UnsupportedFlow::Tools,
        UnsupportedFlow::ToolChoice,
        UnsupportedFlow::Reasoning,
        UnsupportedFlow::Cache,
        UnsupportedFlow::Image,
        UnsupportedFlow::Proof,
        UnsupportedFlow::ResponseMedia,
    ] {
        let event = NormalizedEvent::Failed(ProviderError::UnsupportedFlow(flow));
        assert_eq!(validate_normalized_event(&event), Ok(12));
    }
}
