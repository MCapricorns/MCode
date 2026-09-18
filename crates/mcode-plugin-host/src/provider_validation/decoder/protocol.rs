//! Pure decoder protocol, cumulative-limit, and backpressure reducer.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    DecoderPull, FrameAcceptance, NormalizedEvent, ProviderError, ResponseFrame,
};

use super::events::{EventBatch, EventReducer};
use super::{DecoderPolicy, validate_response_frame};
use crate::provider_validation::{ValidationError, ValidationResult};

const QUEUE_CAPACITY: usize = 16;
const MAX_PULLS: u64 = 65_536;
const MAX_DATA_FRAMES: u64 = 1_024;
const MAX_SUCCESS_BYTES: u64 = 16 * 1_024 * 1_024;
const MAX_ERROR_BYTES: u64 = 64 * 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecoderState {
    InitialPull,
    NeedHead,
    DrainingBody,
    NeedBody,
    DrainingAfterEnd,
    Closed,
}

#[derive(Debug, Clone)]
pub(crate) enum DecoderAction {
    Pull { limit: u8 },
    ReadFrame,
    PushFrame(ResponseFrame),
    AwaitReceipt,
    WaitForConsumer,
    PublishTerminal(NormalizedEvent),
    Closed,
}

#[derive(Debug, Clone)]
pub(crate) enum TransportReceipt {
    Frame(ResponseFrame),
    Eof,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalClose {
    Cancelled,
    Deadline,
    Trap,
    Unavailable,
    Failed,
}

#[derive(Debug, Clone)]
pub(crate) enum CloseCause {
    GuestMethod(ProviderError),
    GuestTerminal,
    Protocol,
    Limit,
    Eof,
    Cancelled,
    Deadline,
    Trap,
    Unavailable,
    Failed,
}

#[derive(Debug, Clone)]
pub(crate) struct CloseReceipt {
    pub(crate) cause: CloseCause,
    pub(crate) terminal: Option<NormalizedEvent>,
}

#[derive(Debug)]
pub(crate) struct DecoderReceipt {
    pub(crate) accepted_events: Vec<NormalizedEvent>,
    pub(crate) close: Option<CloseReceipt>,
    pub(crate) transitioned: bool,
}

#[derive(Debug, Clone)]
enum InFlight {
    Pull { limit: u8 },
    ReadFrame,
    PushFrame,
    PublishTerminal,
}

#[derive(Debug)]
pub(crate) struct DecoderReducer {
    state: DecoderState,
    events: EventReducer,
    in_flight: Option<InFlight>,
    pending_frame: Option<ResponseFrame>,
    close: Option<CloseReceipt>,
    queue_len: usize,
    awaiting_consumer: bool,
    terminal_published: bool,
    pulls: u64,
    data_frames: u64,
    data_bytes: u64,
    status: Option<u16>,
}

impl DecoderReducer {
    pub(crate) fn new(policy: DecoderPolicy) -> Self {
        Self {
            state: DecoderState::InitialPull,
            events: EventReducer::new(&policy),
            in_flight: None,
            pending_frame: None,
            close: None,
            queue_len: 0,
            awaiting_consumer: false,
            terminal_published: false,
            pulls: 0,
            data_frames: 0,
            data_bytes: 0,
            status: None,
        }
    }

    pub(crate) const fn state(&self) -> DecoderState {
        self.state
    }

    pub(crate) const fn queue_len(&self) -> usize {
        self.queue_len
    }

    pub(crate) const fn pull_count(&self) -> u64 {
        self.pulls
    }

    pub(crate) fn next_action(&mut self) -> DecoderAction {
        if self.state == DecoderState::Closed {
            return self.closed_action();
        }
        if self.awaiting_consumer && self.queue_len != 0 {
            return DecoderAction::WaitForConsumer;
        }
        if self.in_flight.is_some() {
            return DecoderAction::AwaitReceipt;
        }
        if let Some(frame) = &self.pending_frame {
            if self.free_capacity() == 0 {
                return DecoderAction::WaitForConsumer;
            }
            self.in_flight = Some(InFlight::PushFrame);
            return DecoderAction::PushFrame(frame.clone());
        }

        match self.state {
            DecoderState::InitialPull
            | DecoderState::DrainingBody
            | DecoderState::DrainingAfterEnd => self.begin_pull(),
            DecoderState::NeedHead | DecoderState::NeedBody => {
                if self.queue_len != 0 || self.free_capacity() == 0 {
                    DecoderAction::WaitForConsumer
                } else {
                    self.in_flight = Some(InFlight::ReadFrame);
                    DecoderAction::ReadFrame
                }
            }
            DecoderState::Closed => self.closed_action(),
        }
    }

    pub(crate) fn pull_receipt(
        &mut self,
        result: Result<DecoderPull, ProviderError>,
    ) -> DecoderReceipt {
        if self.state == DecoderState::Closed {
            return self.stable_closed_receipt();
        }
        let Some(InFlight::Pull { limit }) = self.in_flight.take() else {
            return self.protocol_close();
        };
        let pull = match result {
            Ok(pull) => pull,
            Err(error) => return self.guest_error_close(error),
        };

        match (self.state, pull) {
            (DecoderState::InitialPull, DecoderPull::NeedFrame) => {
                self.state = DecoderState::NeedHead;
                DecoderReceipt::transitioned()
            }
            (DecoderState::DrainingBody, DecoderPull::NeedFrame) => {
                self.state = DecoderState::NeedBody;
                DecoderReceipt::transitioned()
            }
            (DecoderState::DrainingAfterEnd, DecoderPull::NeedFrame)
            | (DecoderState::InitialPull, DecoderPull::Events(_)) => self.protocol_close(),
            (DecoderState::DrainingBody, DecoderPull::Events(events)) => {
                self.accept_events(events, limit, false)
            }
            (DecoderState::DrainingAfterEnd, DecoderPull::Events(events)) => {
                self.accept_events(events, limit, true)
            }
            (DecoderState::NeedHead | DecoderState::NeedBody | DecoderState::Closed, _) => {
                self.protocol_close()
            }
        }
    }

    pub(crate) fn transport_receipt(&mut self, receipt: TransportReceipt) -> DecoderReceipt {
        if self.state == DecoderState::Closed {
            return self.stable_closed_receipt();
        }
        if !matches!(self.in_flight.take(), Some(InFlight::ReadFrame)) {
            return self.protocol_close();
        }
        match receipt {
            TransportReceipt::Eof => {
                self.close_synthesized(CloseCause::Eof, ProviderError::Unavailable)
            }
            TransportReceipt::Cancelled => {
                self.close_synthesized(CloseCause::Cancelled, ProviderError::Cancelled)
            }
            TransportReceipt::Frame(frame) => {
                if let Err(error) = self.validate_pending_frame(&frame) {
                    return self.validation_close(error);
                }
                self.pending_frame = Some(frame);
                DecoderReceipt::transitioned()
            }
        }
    }

    pub(crate) fn push_receipt(
        &mut self,
        result: Result<FrameAcceptance, ProviderError>,
    ) -> DecoderReceipt {
        if self.state == DecoderState::Closed {
            return self.stable_closed_receipt();
        }
        if !matches!(self.in_flight.take(), Some(InFlight::PushFrame)) {
            return self.protocol_close();
        }
        let frame = match self.pending_frame.take() {
            Some(frame) => frame,
            None => return self.protocol_close(),
        };
        if let Err(error) = result {
            return self.guest_error_close(error);
        }

        match (self.state, frame) {
            (DecoderState::NeedHead, ResponseFrame::Head(head)) => {
                self.status = Some(head.status);
                self.state = DecoderState::DrainingBody;
            }
            (DecoderState::NeedBody, ResponseFrame::Data(data)) => {
                let bytes = u64::try_from(data.len()).expect("validated data length fits u64");
                self.data_frames += 1;
                self.data_bytes += bytes;
                self.state = DecoderState::DrainingBody;
            }
            (DecoderState::NeedBody, ResponseFrame::End) => {
                self.state = DecoderState::DrainingAfterEnd;
            }
            _ => return self.protocol_close(),
        }
        DecoderReceipt::transitioned()
    }

    pub(crate) fn consumer_receipt(&mut self, count: usize) -> ValidationResult {
        if count == 0 || count > self.queue_len {
            return Err(ValidationError::InvalidArgument);
        }
        self.queue_len -= count;
        self.awaiting_consumer = false;
        Ok(())
    }

    pub(crate) fn terminal_published(&mut self) -> ValidationResult {
        if self.state != DecoderState::Closed
            || self.queue_len != 0
            || self.terminal_published
            || self
                .close
                .as_ref()
                .is_none_or(|receipt| receipt.terminal.is_none())
            || !matches!(self.in_flight.take(), Some(InFlight::PublishTerminal))
        {
            return Err(ValidationError::InvalidArgument);
        }
        self.terminal_published = true;
        Ok(())
    }

    pub(crate) fn close_external(&mut self, cause: ExternalClose) -> DecoderReceipt {
        if self.state == DecoderState::Closed {
            return self.stable_closed_receipt();
        }
        let (cause, error) = match cause {
            ExternalClose::Cancelled => (CloseCause::Cancelled, ProviderError::Cancelled),
            ExternalClose::Deadline => (CloseCause::Deadline, ProviderError::Cancelled),
            ExternalClose::Trap => (CloseCause::Trap, ProviderError::Failed),
            ExternalClose::Unavailable => (CloseCause::Unavailable, ProviderError::Unavailable),
            ExternalClose::Failed => (CloseCause::Failed, ProviderError::Failed),
        };
        self.close_synthesized(cause, error)
    }

    fn begin_pull(&mut self) -> DecoderAction {
        let free = self.free_capacity();
        if free == 0 {
            return DecoderAction::WaitForConsumer;
        }
        if self.pulls >= MAX_PULLS {
            self.close_synthesized(CloseCause::Limit, ProviderError::Limit);
            return self.closed_action();
        }
        let limit = u8::try_from(free).expect("queue capacity fits u8");
        self.pulls += 1;
        self.in_flight = Some(InFlight::Pull { limit });
        DecoderAction::Pull { limit }
    }

    fn accept_events(
        &mut self,
        events: Vec<NormalizedEvent>,
        limit: u8,
        after_end: bool,
    ) -> DecoderReceipt {
        let successful_status = self
            .status
            .is_some_and(|status| (200..300).contains(&status));
        // Reduction errors irreversibly close the decoder before partial state can be
        // observed; only a fully returned batch is enqueued.
        let batch = match self
            .events
            .reduce_batch(events, limit, after_end, successful_status)
        {
            Ok(batch) => batch,
            Err(error) => return self.validation_close(error),
        };
        self.enqueue_and_maybe_close(batch)
    }

    fn enqueue_and_maybe_close(&mut self, batch: EventBatch) -> DecoderReceipt {
        self.queue_len += batch.accepted.len();
        self.awaiting_consumer = !batch.accepted.is_empty();
        if let Some(terminal) = batch.terminal {
            let receipt = CloseReceipt {
                cause: CloseCause::GuestTerminal,
                terminal: Some(terminal),
            };
            self.finish_close(receipt, batch.accepted)
        } else {
            DecoderReceipt {
                accepted_events: batch.accepted,
                close: None,
                transitioned: true,
            }
        }
    }

    fn validate_pending_frame(&self, frame: &ResponseFrame) -> ValidationResult {
        validate_response_frame(frame)?;
        match (self.state, frame) {
            (DecoderState::NeedHead, ResponseFrame::Head(_)) => Ok(()),
            (DecoderState::NeedBody, ResponseFrame::End) => Ok(()),
            (DecoderState::NeedBody, ResponseFrame::Data(data)) => {
                let next_frames = self
                    .data_frames
                    .checked_add(1)
                    .ok_or(ValidationError::Limit)?;
                if next_frames > MAX_DATA_FRAMES {
                    return Err(ValidationError::Limit);
                }
                let bytes = u64::try_from(data.len()).map_err(|_| ValidationError::Limit)?;
                let next_bytes = self
                    .data_bytes
                    .checked_add(bytes)
                    .ok_or(ValidationError::Limit)?;
                let maximum = if self
                    .status
                    .is_some_and(|status| (200..300).contains(&status))
                {
                    MAX_SUCCESS_BYTES
                } else {
                    MAX_ERROR_BYTES
                };
                if next_bytes > maximum {
                    return Err(ValidationError::Limit);
                }
                Ok(())
            }
            _ => Err(ValidationError::InvalidArgument),
        }
    }

    fn validation_close(&mut self, error: ValidationError) -> DecoderReceipt {
        match error {
            ValidationError::InvalidArgument => {
                self.close_synthesized(CloseCause::Protocol, ProviderError::InvalidArgument)
            }
            ValidationError::Limit => {
                self.close_synthesized(CloseCause::Limit, ProviderError::Limit)
            }
        }
    }

    fn protocol_close(&mut self) -> DecoderReceipt {
        self.close_synthesized(CloseCause::Protocol, ProviderError::InvalidArgument)
    }

    fn guest_error_close(&mut self, error: ProviderError) -> DecoderReceipt {
        let receipt = CloseReceipt {
            cause: CloseCause::GuestMethod(error),
            terminal: None,
        };
        self.finish_close(receipt, Vec::new())
    }

    fn close_synthesized(&mut self, cause: CloseCause, error: ProviderError) -> DecoderReceipt {
        let receipt = CloseReceipt {
            cause,
            terminal: Some(NormalizedEvent::Failed(error)),
        };
        self.finish_close(receipt, Vec::new())
    }

    fn finish_close(
        &mut self,
        receipt: CloseReceipt,
        accepted_events: Vec<NormalizedEvent>,
    ) -> DecoderReceipt {
        self.state = DecoderState::Closed;
        self.in_flight = None;
        self.pending_frame = None;
        self.close = Some(receipt.clone());
        DecoderReceipt {
            accepted_events,
            close: Some(receipt),
            transitioned: true,
        }
    }

    fn stable_closed_receipt(&self) -> DecoderReceipt {
        DecoderReceipt {
            accepted_events: Vec::new(),
            close: self.close.clone(),
            transitioned: false,
        }
    }

    fn closed_action(&mut self) -> DecoderAction {
        if self.queue_len != 0 {
            return DecoderAction::WaitForConsumer;
        }
        if self.in_flight.is_some() {
            return DecoderAction::AwaitReceipt;
        }
        if !self.terminal_published
            && let Some(terminal) = self
                .close
                .as_ref()
                .and_then(|receipt| receipt.terminal.as_ref())
        {
            self.in_flight = Some(InFlight::PublishTerminal);
            return DecoderAction::PublishTerminal(terminal.clone());
        }
        DecoderAction::Closed
    }

    const fn free_capacity(&self) -> usize {
        QUEUE_CAPACITY - self.queue_len
    }
}

impl DecoderReceipt {
    const fn transitioned() -> Self {
        Self {
            accepted_events: Vec::new(),
            close: None,
            transitioned: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
        ResponseHead, ResponseMedia,
    };

    #[test]
    fn external_close_clears_a_frame_pending_at_guest_push() {
        let mut reducer = DecoderReducer::new(DecoderPolicy::empty());
        assert!(matches!(
            reducer.next_action(),
            DecoderAction::Pull { limit: 16 }
        ));
        reducer.pull_receipt(Ok(DecoderPull::NeedFrame));
        assert!(matches!(reducer.next_action(), DecoderAction::ReadFrame));
        reducer.transport_receipt(TransportReceipt::Frame(ResponseFrame::Head(ResponseHead {
            status: 200,
            media: ResponseMedia::Json,
        })));
        assert!(reducer.pending_frame.is_some());
        assert!(matches!(
            reducer.next_action(),
            DecoderAction::PushFrame(ResponseFrame::Head(_))
        ));
        assert!(matches!(reducer.in_flight, Some(InFlight::PushFrame)));

        let receipt = reducer.close_external(ExternalClose::Cancelled);
        assert!(receipt.transitioned);
        assert!(reducer.pending_frame.is_none());
        assert!(reducer.in_flight.is_none());
        assert!(matches!(
            reducer.next_action(),
            DecoderAction::PublishTerminal(NormalizedEvent::Failed(ProviderError::Cancelled))
        ));

        let late_guest_receipt = reducer.push_receipt(Ok(FrameAcceptance::Accepted));
        assert!(!late_guest_receipt.transitioned);
        assert!(late_guest_receipt.accepted_events.is_empty());
        assert!(matches!(reducer.next_action(), DecoderAction::AwaitReceipt));
        reducer
            .terminal_published()
            .expect("external terminal published");
        assert!(matches!(reducer.next_action(), DecoderAction::Closed));
    }
}
