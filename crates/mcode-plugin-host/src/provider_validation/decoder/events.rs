//! Stateful validation of normalized decoder events.
use std::collections::BTreeSet;

use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    CompletionReason, NormalizedEvent, ReasoningKind, ToolArgumentsDelta, ToolCallEnd,
    ToolCallStart,
};

use super::tool_json::validate_tool_arguments;
use super::{DecoderPolicy, validate_event_batch, validate_normalized_event};
use crate::provider_validation::{ValidationError, ValidationResult};

const MAX_EVENTS: u64 = 65_536;
const MAX_CUMULATIVE_EVENT_CHARGE: u64 = 8 * 1_024 * 1_024;
const MAX_PROOF_BYTES: u64 = 256 * 1_024;
const MAX_TOOL_DELTAS: u32 = 16_384;
const MAX_TOOL_BYTES: u64 = 1_024 * 1_024;
const MAX_ALL_TOOL_BYTES: u64 = 2 * 1_024 * 1_024;

#[derive(Debug)]
pub(super) struct EventReducer {
    proof_supported: bool,
    tool_names: BTreeSet<String>,
    reported_models: BTreeSet<String>,
    call_ids: BTreeSet<String>,
    current: Option<ContentSlot>,
    event_count: u64,
    event_charge: u64,
    proof_bytes: u64,
    tool_bytes: u64,
    complete_calls: u32,
    terminal_seen: bool,
}

#[derive(Debug)]
pub(super) struct EventBatch {
    pub(super) accepted: Vec<NormalizedEvent>,
    pub(super) terminal: Option<NormalizedEvent>,
}

#[derive(Debug)]
struct ContentSlot {
    index: u8,
    kind: ContentKind,
}

#[derive(Debug)]
enum ContentKind {
    Text,
    Reasoning {
        kind: ReasoningTag,
        proof_seen: bool,
    },
    Tool(ToolState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReasoningTag {
    Thinking,
    Summary,
}

#[derive(Debug)]
enum ToolState {
    Open {
        call_id: String,
        delta_count: u32,
        bytes: u64,
        arguments: String,
    },
    Sealed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentClass {
    Text,
    Reasoning,
}

impl EventReducer {
    pub(super) fn new(policy: &DecoderPolicy) -> Self {
        Self {
            proof_supported: policy.proof_supported,
            tool_names: policy.tool_names.clone(),
            reported_models: policy.reported_models.clone(),
            call_ids: BTreeSet::new(),
            current: None,
            event_count: 0,
            event_charge: 0,
            proof_bytes: 0,
            tool_bytes: 0,
            complete_calls: 0,
            terminal_seen: false,
        }
    }

    /// Seeds bounded cumulative counters for boundary tests.
    ///
    /// # Panics
    ///
    /// Panics when either seed exceeds its production bound.
    #[cfg(test)]
    pub(super) fn set_cumulative_usage_for_test(&mut self, event_count: u64, event_charge: u64) {
        assert!(event_count <= MAX_EVENTS);
        assert!(event_charge <= MAX_CUMULATIVE_EVENT_CHARGE);
        self.event_count = event_count;
        self.event_charge = event_charge;
    }

    pub(super) fn reduce_batch(
        &mut self,
        events: Vec<NormalizedEvent>,
        limit: u8,
        after_end: bool,
        successful_status: bool,
    ) -> ValidationResult<EventBatch> {
        validate_event_batch(&events, limit)?;
        self.validate_terminal_position(&events, after_end)?;
        if !successful_status {
            return self.reduce_error_batch(events, after_end);
        }

        let event_count = u64::try_from(events.len()).map_err(|_| ValidationError::Limit)?;
        self.event_count = self
            .event_count
            .checked_add(event_count)
            .ok_or(ValidationError::Limit)?;
        if self.event_count > MAX_EVENTS {
            return Err(ValidationError::Limit);
        }

        let mut accepted = Vec::with_capacity(events.len());
        let mut terminal = None;
        for event in events {
            let charge = validate_normalized_event(&event)?;
            self.event_charge = self
                .event_charge
                .checked_add(charge)
                .ok_or(ValidationError::Limit)?;
            if self.event_charge > MAX_CUMULATIVE_EVENT_CHARGE {
                return Err(ValidationError::Limit);
            }
            match event {
                NormalizedEvent::TextDelta(delta) => {
                    self.select_content(delta.content_index, ContentClass::Text, None)?;
                    accepted.push(NormalizedEvent::TextDelta(delta));
                }
                NormalizedEvent::ReasoningDelta(delta) => {
                    self.select_content(
                        delta.content_index,
                        ContentClass::Reasoning,
                        Some(reasoning_tag(&delta.kind)),
                    )?;
                    accepted.push(NormalizedEvent::ReasoningDelta(delta));
                }
                NormalizedEvent::ReasoningProof(proof) => {
                    self.accept_proof(
                        proof.content_index,
                        reasoning_tag(&proof.kind),
                        proof.proof.len(),
                    )?;
                    accepted.push(NormalizedEvent::ReasoningProof(proof));
                }
                NormalizedEvent::ToolCallStart(start) => {
                    self.start_tool(&start)?;
                    accepted.push(NormalizedEvent::ToolCallStart(start));
                }
                NormalizedEvent::ToolArgumentsDelta(delta) => {
                    self.append_tool_delta(&delta)?;
                    accepted.push(NormalizedEvent::ToolArgumentsDelta(delta));
                }
                NormalizedEvent::ToolCallEnd(end) => {
                    self.end_tool(&end)?;
                    accepted.push(NormalizedEvent::ToolCallEnd(end));
                }
                NormalizedEvent::Completed(mut completed) => {
                    self.accept_completed(&completed.reason)?;
                    if completed
                        .reported_model
                        .as_ref()
                        .is_some_and(|model| !self.reported_models.contains(model))
                    {
                        completed.reported_model = None;
                    }
                    self.terminal_seen = true;
                    terminal = Some(NormalizedEvent::Completed(completed));
                }
                NormalizedEvent::Failed(error) => {
                    self.reject_open_tool()?;
                    self.terminal_seen = true;
                    terminal = Some(NormalizedEvent::Failed(error));
                }
            }
        }
        Ok(EventBatch { accepted, terminal })
    }

    fn validate_terminal_position(
        &self,
        events: &[NormalizedEvent],
        after_end: bool,
    ) -> ValidationResult {
        if self.terminal_seen {
            return Err(ValidationError::InvalidArgument);
        }
        let mut terminal_position = None;
        for (position, event) in events.iter().enumerate() {
            if is_terminal(event) && terminal_position.replace(position).is_some() {
                return Err(ValidationError::InvalidArgument);
            }
        }
        if terminal_position.is_some() && !after_end {
            return Err(ValidationError::InvalidArgument);
        }
        if terminal_position.is_some_and(|position| position + 1 != events.len()) {
            return Err(ValidationError::InvalidArgument);
        }
        Ok(())
    }

    fn reduce_error_batch(
        &mut self,
        mut events: Vec<NormalizedEvent>,
        after_end: bool,
    ) -> ValidationResult<EventBatch> {
        if !after_end || events.len() != 1 || !matches!(events[0], NormalizedEvent::Failed(_)) {
            return Err(ValidationError::InvalidArgument);
        }
        self.event_count = self
            .event_count
            .checked_add(1)
            .ok_or(ValidationError::Limit)?;
        if self.event_count > MAX_EVENTS {
            return Err(ValidationError::Limit);
        }
        let event = events.pop().ok_or(ValidationError::InvalidArgument)?;
        self.event_charge = self
            .event_charge
            .checked_add(validate_normalized_event(&event)?)
            .ok_or(ValidationError::Limit)?;
        if self.event_charge > MAX_CUMULATIVE_EVENT_CHARGE {
            return Err(ValidationError::Limit);
        }
        self.terminal_seen = true;
        Ok(EventBatch {
            accepted: Vec::new(),
            terminal: Some(event),
        })
    }

    fn select_content(
        &mut self,
        index: u8,
        class: ContentClass,
        reasoning: Option<ReasoningTag>,
    ) -> ValidationResult {
        match &mut self.current {
            None => {
                if index != 0 {
                    return Err(ValidationError::InvalidArgument);
                }
                self.current = Some(ContentSlot {
                    index,
                    kind: new_content_kind(class, reasoning)?,
                });
            }
            Some(current) if current.index == index => match (&current.kind, class) {
                (ContentKind::Text, ContentClass::Text) => {}
                (ContentKind::Reasoning { kind, .. }, ContentClass::Reasoning)
                    if Some(*kind) == reasoning => {}
                _ => return Err(ValidationError::InvalidArgument),
            },
            Some(current) if current.index.checked_add(1) == Some(index) => {
                ensure_slot_can_advance(current)?;
                self.current = Some(ContentSlot {
                    index,
                    kind: new_content_kind(class, reasoning)?,
                });
            }
            Some(_) => return Err(ValidationError::InvalidArgument),
        }
        Ok(())
    }

    fn accept_proof(&mut self, index: u8, kind: ReasoningTag, bytes: usize) -> ValidationResult {
        if !self.proof_supported {
            return Err(ValidationError::InvalidArgument);
        }
        let Some(ContentSlot {
            index: current_index,
            kind:
                ContentKind::Reasoning {
                    kind: current_kind,
                    proof_seen,
                },
        }) = &mut self.current
        else {
            return Err(ValidationError::InvalidArgument);
        };
        if *current_index != index || *current_kind != kind || *proof_seen {
            return Err(ValidationError::InvalidArgument);
        }
        let bytes = u64::try_from(bytes).map_err(|_| ValidationError::Limit)?;
        self.proof_bytes = self
            .proof_bytes
            .checked_add(bytes)
            .ok_or(ValidationError::Limit)?;
        if self.proof_bytes > MAX_PROOF_BYTES {
            return Err(ValidationError::Limit);
        }
        *proof_seen = true;
        Ok(())
    }

    fn start_tool(&mut self, start: &ToolCallStart) -> ValidationResult {
        if !self.tool_names.contains(&start.name) || self.call_ids.contains(&start.call_id) {
            return Err(ValidationError::InvalidArgument);
        }
        match &self.current {
            None if start.content_index == 0 => {}
            Some(current)
                if current.index.checked_add(1) == Some(start.content_index)
                    && ensure_slot_can_advance(current).is_ok() => {}
            _ => return Err(ValidationError::InvalidArgument),
        }
        self.call_ids.insert(start.call_id.clone());
        self.current = Some(ContentSlot {
            index: start.content_index,
            kind: ContentKind::Tool(ToolState::Open {
                call_id: start.call_id.clone(),
                delta_count: 0,
                bytes: 0,
                arguments: String::new(),
            }),
        });
        Ok(())
    }

    fn append_tool_delta(&mut self, delta: &ToolArgumentsDelta) -> ValidationResult {
        let Some(ContentSlot {
            index,
            kind:
                ContentKind::Tool(ToolState::Open {
                    call_id,
                    delta_count,
                    bytes,
                    arguments,
                }),
        }) = &mut self.current
        else {
            return Err(ValidationError::InvalidArgument);
        };
        if *index != delta.content_index || *call_id != delta.call_id {
            return Err(ValidationError::InvalidArgument);
        }
        let next_count = delta_count.checked_add(1).ok_or(ValidationError::Limit)?;
        if next_count > MAX_TOOL_DELTAS {
            return Err(ValidationError::Limit);
        }
        let delta_bytes = u64::try_from(delta.delta.len()).map_err(|_| ValidationError::Limit)?;
        let next_bytes = bytes
            .checked_add(delta_bytes)
            .ok_or(ValidationError::Limit)?;
        let next_total = self
            .tool_bytes
            .checked_add(delta_bytes)
            .ok_or(ValidationError::Limit)?;
        if next_bytes > MAX_TOOL_BYTES || next_total > MAX_ALL_TOOL_BYTES {
            return Err(ValidationError::Limit);
        }
        arguments.push_str(&delta.delta);
        *delta_count = next_count;
        *bytes = next_bytes;
        self.tool_bytes = next_total;
        Ok(())
    }

    fn end_tool(&mut self, end: &ToolCallEnd) -> ValidationResult {
        let Some(ContentSlot {
            index,
            kind:
                ContentKind::Tool(ToolState::Open {
                    call_id,
                    delta_count,
                    arguments,
                    ..
                }),
        }) = &self.current
        else {
            return Err(ValidationError::InvalidArgument);
        };
        if *index != end.content_index || *call_id != end.call_id || *delta_count == 0 {
            return Err(ValidationError::InvalidArgument);
        }
        validate_tool_arguments(arguments)?;
        let current = self
            .current
            .as_mut()
            .ok_or(ValidationError::InvalidArgument)?;
        current.kind = ContentKind::Tool(ToolState::Sealed);
        self.complete_calls = self
            .complete_calls
            .checked_add(1)
            .ok_or(ValidationError::Limit)?;
        Ok(())
    }

    fn accept_completed(&self, reason: &CompletionReason) -> ValidationResult {
        self.reject_open_tool()?;
        let tool_use = matches!(reason, CompletionReason::ToolUse);
        if tool_use != (self.complete_calls > 0) {
            return Err(ValidationError::InvalidArgument);
        }
        Ok(())
    }

    fn reject_open_tool(&self) -> ValidationResult {
        if self
            .current
            .as_ref()
            .is_some_and(|slot| matches!(slot.kind, ContentKind::Tool(ToolState::Open { .. })))
        {
            return Err(ValidationError::InvalidArgument);
        }
        Ok(())
    }
}

fn new_content_kind(
    class: ContentClass,
    reasoning: Option<ReasoningTag>,
) -> ValidationResult<ContentKind> {
    match (class, reasoning) {
        (ContentClass::Text, None) => Ok(ContentKind::Text),
        (ContentClass::Reasoning, Some(kind)) => Ok(ContentKind::Reasoning {
            kind,
            proof_seen: false,
        }),
        _ => Err(ValidationError::InvalidArgument),
    }
}

fn ensure_slot_can_advance(slot: &ContentSlot) -> ValidationResult {
    if matches!(slot.kind, ContentKind::Tool(ToolState::Open { .. })) {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(())
}

const fn reasoning_tag(kind: &ReasoningKind) -> ReasoningTag {
    match kind {
        ReasoningKind::Thinking => ReasoningTag::Thinking,
        ReasoningKind::Summary => ReasoningTag::Summary,
    }
}

const fn is_terminal(event: &NormalizedEvent) -> bool {
    matches!(
        event,
        NormalizedEvent::Completed(_) | NormalizedEvent::Failed(_)
    )
}
