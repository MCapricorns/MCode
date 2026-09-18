//! Shared fixtures for pure decoder reducer tests.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    DecoderPull, FrameAcceptance, NormalizedEvent, ResponseFrame, ResponseHead, ResponseMedia,
};

use super::decoder::DecoderPolicy;
use super::decoder::protocol::{
    DecoderAction, DecoderReceipt, DecoderReducer, DecoderState, TransportReceipt,
};

pub(super) fn policy(proof_supported: bool, tools: &[&str], models: &[&str]) -> DecoderPolicy {
    DecoderPolicy::new(
        proof_supported,
        tools.iter().map(|value| (*value).to_owned()),
        models.iter().map(|value| (*value).to_owned()),
    )
}

pub(super) fn reducer() -> DecoderReducer {
    DecoderReducer::new(DecoderPolicy::empty())
}

pub(super) fn accept_head(reducer: &mut DecoderReducer, status: u16) {
    assert!(matches!(
        reducer.next_action(),
        DecoderAction::Pull { limit: 16 }
    ));
    let receipt = reducer.pull_receipt(Ok(DecoderPull::NeedFrame));
    assert!(receipt.transitioned && receipt.close.is_none());
    assert_eq!(reducer.state(), DecoderState::NeedHead);
    accept_frame(
        reducer,
        ResponseFrame::Head(ResponseHead {
            status,
            media: ResponseMedia::Json,
        }),
    );
    assert_eq!(reducer.state(), DecoderState::DrainingBody);
}

pub(super) fn request_frame(reducer: &mut DecoderReducer) {
    assert!(matches!(reducer.next_action(), DecoderAction::Pull { .. }));
    let receipt = reducer.pull_receipt(Ok(DecoderPull::NeedFrame));
    assert!(receipt.transitioned && receipt.close.is_none());
}

pub(super) fn accept_frame(reducer: &mut DecoderReducer, frame: ResponseFrame) {
    assert!(matches!(reducer.next_action(), DecoderAction::ReadFrame));
    let expected = frame.clone();
    let receipt = reducer.transport_receipt(TransportReceipt::Frame(frame));
    assert!(receipt.transitioned && receipt.close.is_none());
    let action = reducer.next_action();
    match (action, expected) {
        (DecoderAction::PushFrame(ResponseFrame::Head(actual)), ResponseFrame::Head(expected)) => {
            assert_eq!(actual.status, expected.status);
            assert!(matches!(
                (actual.media, expected.media),
                (ResponseMedia::Json, ResponseMedia::Json)
                    | (ResponseMedia::EventStream, ResponseMedia::EventStream)
            ));
        }
        (DecoderAction::PushFrame(ResponseFrame::Data(actual)), ResponseFrame::Data(expected)) => {
            assert_eq!(actual, expected);
        }
        (DecoderAction::PushFrame(ResponseFrame::End), ResponseFrame::End) => {}
        _ => panic!("push action must preserve the accepted transport frame"),
    }
    let receipt = reducer.push_receipt(Ok(FrameAcceptance::Accepted));
    assert!(receipt.transitioned && receipt.close.is_none());
}

pub(super) fn accept_data(reducer: &mut DecoderReducer, data: Vec<u8>) {
    request_frame(reducer);
    assert_eq!(reducer.state(), DecoderState::NeedBody);
    accept_frame(reducer, ResponseFrame::Data(data));
}

pub(super) fn accept_end(reducer: &mut DecoderReducer) {
    request_frame(reducer);
    assert_eq!(reducer.state(), DecoderState::NeedBody);
    accept_frame(reducer, ResponseFrame::End);
    assert_eq!(reducer.state(), DecoderState::DrainingAfterEnd);
}

pub(super) fn accept_events(
    reducer: &mut DecoderReducer,
    events: Vec<NormalizedEvent>,
) -> DecoderReceipt {
    assert!(matches!(reducer.next_action(), DecoderAction::Pull { .. }));
    reducer.pull_receipt(Ok(DecoderPull::Events(events)))
}

pub(super) fn consume_all(reducer: &mut DecoderReducer) {
    let count = reducer.queue_len();
    if count != 0 {
        reducer
            .consumer_receipt(count)
            .expect("accepted queue count");
    }
}
