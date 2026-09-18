//! Lexical scalar sources and checked derived adapter transforms.
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};

use crate::provider_validation::scalar::MAX_LOGICAL_CHARGE;
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    AssistantBlock, ImageMediaType, ImageView, Message, Reasoning, ReasoningEffort, ToolChoice,
    ToolResultBlock, UserBlock,
};

use super::evaluate::{Evaluator, Scope};
use super::json::{AdapterJson, canonical_wire_text, from_wire};
use super::types::{
    AdapterEnumSource, AdapterScalarSource, AdapterTransform, AdapterValidationError,
    AdapterValidationResult,
};

impl<'a> Evaluator<'a> {
    pub(super) fn block_kind(
        &mut self,
        transform: &AdapterTransform,
        scope: Scope<'a>,
    ) -> AdapterValidationResult<AdapterJson> {
        let (message, block, source, ordinal) = match scope {
            Scope::UserBlock(message, block, value) => (
                message,
                block,
                AdapterEnumSource::UserBlockKind,
                u8::from(matches!(value, UserBlock::Image(_))),
            ),
            Scope::AssistantBlock(message, block, value) => (
                message,
                block,
                AdapterEnumSource::AssistantBlockKind,
                match value {
                    AssistantBlock::Text(_) => 0,
                    AssistantBlock::Reasoning(_) => 1,
                    AssistantBlock::ToolCall(_) => 2,
                },
            ),
            Scope::ToolResultBlock(message, block, value) => (
                message,
                block,
                AdapterEnumSource::ToolResultBlockKind,
                u8::from(matches!(value, ToolResultBlock::Image(_))),
            ),
            _ => return Err(AdapterValidationError::InvalidContract),
        };
        self.consume_optional(format!("m{message}.b{block}.kind"))?;
        self.enum_token(transform, source, ordinal)
    }

    pub(super) fn block_text(
        &mut self,
        transform: &AdapterTransform,
        scope: Scope<'a>,
    ) -> AdapterValidationResult<AdapterJson> {
        let (message, block, value) = match scope {
            Scope::UserBlock(message, block, UserBlock::Text(value)) => {
                (message, block, value.text.as_str())
            }
            Scope::AssistantBlock(message, block, AssistantBlock::Text(value)) => {
                (message, block, value.text.as_str())
            }
            Scope::AssistantBlock(message, block, AssistantBlock::Reasoning(value)) => {
                (message, block, value.text.as_str())
            }
            Scope::ToolResultBlock(message, block, ToolResultBlock::Text(value)) => {
                (message, block, value.text.as_str())
            }
            _ => return Err(AdapterValidationError::InvalidContract),
        };
        if !matches!(transform, AdapterTransform::Identity) {
            return Err(AdapterValidationError::InvalidContract);
        }
        self.consume(format!("m{message}.b{block}.text"))?;
        Ok(AdapterJson::ordinary_string(value))
    }

    pub(super) fn tool_call_scalar(
        &mut self,
        source: AdapterScalarSource,
        transform: &AdapterTransform,
        scope: Scope<'a>,
    ) -> AdapterValidationResult<AdapterJson> {
        let Scope::AssistantBlock(message, block, AssistantBlock::ToolCall(call)) = scope else {
            return Err(AdapterValidationError::InvalidContract);
        };
        match source {
            AdapterScalarSource::ToolCallId => {
                require_identity(transform)?;
                self.consume(format!("m{message}.b{block}.call-id"))?;
                Ok(AdapterJson::ordinary_string(&call.call_id))
            }
            AdapterScalarSource::ToolCallName => {
                require_identity(transform)?;
                self.consume(format!("m{message}.b{block}.call-name"))?;
                Ok(AdapterJson::ordinary_string(&call.name))
            }
            AdapterScalarSource::ToolCallArguments => {
                self.consume(format!("m{message}.b{block}.arguments"))?;
                match transform {
                    AdapterTransform::JsonSubtree => from_wire(&call.arguments),
                    AdapterTransform::CanonicalJsonString => Ok(AdapterJson::derived_string(
                        canonical_wire_text(&call.arguments)?,
                    )),
                    _ => Err(AdapterValidationError::InvalidContract),
                }
            }
            _ => Err(AdapterValidationError::InvalidContract),
        }
    }

    pub(super) fn tool_scalar(
        &mut self,
        source: AdapterScalarSource,
        transform: &AdapterTransform,
        scope: Scope<'a>,
    ) -> AdapterValidationResult<AdapterJson> {
        let Scope::Tool(index) = scope else {
            return Err(AdapterValidationError::InvalidContract);
        };
        let tool = self
            .original
            .tools
            .get(index)
            .ok_or(AdapterValidationError::SourceMismatch)?;
        match source {
            AdapterScalarSource::ToolName => {
                require_identity(transform)?;
                self.consume(format!("tool{index}.name"))?;
                Ok(AdapterJson::ordinary_string(&tool.name))
            }
            AdapterScalarSource::ToolDescription => {
                require_identity(transform)?;
                self.consume(format!("tool{index}.description"))?;
                Ok(AdapterJson::ordinary_string(&tool.description))
            }
            AdapterScalarSource::ToolSchema => {
                if !matches!(transform, AdapterTransform::JsonSubtree) {
                    return Err(AdapterValidationError::InvalidContract);
                }
                self.consume(format!("tool{index}.schema"))?;
                from_wire(&tool.input_schema)
            }
            _ => Err(AdapterValidationError::InvalidContract),
        }
    }

    pub(super) fn reasoning_scalar(
        &mut self,
        source: AdapterScalarSource,
        transform: &AdapterTransform,
        scope: Scope<'a>,
    ) -> AdapterValidationResult<AdapterJson> {
        let Scope::AssistantBlock(message, block, AssistantBlock::Reasoning(reasoning)) = scope
        else {
            return Err(AdapterValidationError::InvalidContract);
        };
        match source {
            AdapterScalarSource::ReasoningKind => {
                self.consume(format!("m{message}.b{block}.reasoning-kind"))?;
                self.enum_token(
                    transform,
                    AdapterEnumSource::ReasoningKind,
                    match reasoning.kind {
                        crate::provider_wit::exports::mcode::provider_pack::provider_api::ReasoningKind::Thinking => 0,
                        crate::provider_wit::exports::mcode::provider_pack::provider_api::ReasoningKind::Summary => 1,
                    },
                )
            }
            AdapterScalarSource::Proof => {
                let proof = reasoning
                    .proof
                    .as_ref()
                    .ok_or(AdapterValidationError::InvalidContract)?;
                self.consume(format!("m{message}.b{block}.proof"))?;
                encode_bytes(transform, &proof.proof)
            }
            _ => Err(AdapterValidationError::InvalidContract),
        }
    }

    pub(super) fn image_scalar(
        &mut self,
        source: AdapterScalarSource,
        transform: &AdapterTransform,
        scope: Scope<'a>,
    ) -> AdapterValidationResult<AdapterJson> {
        let (message, block, image) = image(scope)?;
        match source {
            AdapterScalarSource::ImageBytes => {
                self.consume(format!("m{message}.b{block}.bytes"))?;
                encode_bytes(transform, &image.bytes)
            }
            AdapterScalarSource::ImageMediaType => {
                self.consume(format!("m{message}.b{block}.media"))?;
                self.enum_token(
                    transform,
                    AdapterEnumSource::ImageMediaType,
                    media_ordinal(&image.media_type),
                )
            }
            AdapterScalarSource::ImageWidth => {
                require_u32(transform)?;
                self.consume_optional(format!("m{message}.b{block}.width"))?;
                Ok(AdapterJson::Number(image.metadata.width.to_string()))
            }
            AdapterScalarSource::ImageHeight => {
                require_u32(transform)?;
                self.consume_optional(format!("m{message}.b{block}.height"))?;
                Ok(AdapterJson::Number(image.metadata.height.to_string()))
            }
            AdapterScalarSource::ImageFrames => {
                require_u32(transform)?;
                self.consume_optional(format!("m{message}.b{block}.frames"))?;
                Ok(AdapterJson::Number(image.metadata.frames.to_string()))
            }
            AdapterScalarSource::ImageDataUri => {
                if !matches!(transform, AdapterTransform::DataUri) {
                    return Err(AdapterValidationError::InvalidContract);
                }
                self.consume(format!("m{message}.b{block}.bytes"))?;
                self.consume(format!("m{message}.b{block}.media"))?;
                Ok(AdapterJson::derived_string(data_uri(image)?))
            }
            _ => Err(AdapterValidationError::InvalidContract),
        }
    }

    pub(super) fn tool_choice_scalar(
        &mut self,
        source: AdapterScalarSource,
        transform: &AdapterTransform,
        scope: Scope<'a>,
    ) -> AdapterValidationResult<AdapterJson> {
        if !matches!(scope, Scope::Root) {
            return Err(AdapterValidationError::InvalidContract);
        }
        match source {
            AdapterScalarSource::ToolChoiceKind => {
                self.consume_optional("tool-choice.kind")?;
                self.enum_token(
                    transform,
                    AdapterEnumSource::ToolChoice,
                    match self.original.tool_choice {
                        ToolChoice::Unset => 0,
                        ToolChoice::Auto => 1,
                        ToolChoice::None => 2,
                        ToolChoice::Specific(_) => 3,
                    },
                )
            }
            AdapterScalarSource::ToolChoiceName => {
                require_identity(transform)?;
                let ToolChoice::Specific(choice) = &self.original.tool_choice else {
                    return Err(AdapterValidationError::InvalidContract);
                };
                self.consume("tool-choice.name")?;
                Ok(AdapterJson::ordinary_string(&choice.name))
            }
            _ => Err(AdapterValidationError::InvalidContract),
        }
    }

    pub(super) fn reasoning_control_scalar(
        &mut self,
        source: AdapterScalarSource,
        transform: &AdapterTransform,
        scope: Scope<'a>,
    ) -> AdapterValidationResult<AdapterJson> {
        if !matches!(scope, Scope::Root) {
            return Err(AdapterValidationError::InvalidContract);
        }
        match source {
            AdapterScalarSource::ReasoningMode => {
                self.consume_optional("reasoning.kind")?;
                self.enum_token(
                    transform,
                    AdapterEnumSource::ReasoningMode,
                    match self.original.reasoning {
                        Reasoning::Unset => 0,
                        Reasoning::Disabled => 1,
                        Reasoning::Enabled(_) => 2,
                    },
                )
            }
            AdapterScalarSource::ReasoningEffort => {
                let Reasoning::Enabled(enabled) = &self.original.reasoning else {
                    return Err(AdapterValidationError::InvalidContract);
                };
                let effort = enabled
                    .effort
                    .as_ref()
                    .ok_or(AdapterValidationError::InvalidContract)?;
                self.consume("reasoning.effort")?;
                self.enum_token(
                    transform,
                    AdapterEnumSource::ReasoningEffort,
                    effort_ordinal(effort),
                )
            }
            AdapterScalarSource::ReasoningBudget => {
                let Reasoning::Enabled(enabled) = &self.original.reasoning else {
                    return Err(AdapterValidationError::InvalidContract);
                };
                let budget = enabled
                    .budget_tokens
                    .ok_or(AdapterValidationError::InvalidContract)?;
                if !matches!(transform, AdapterTransform::CheckedU64) {
                    return Err(AdapterValidationError::InvalidContract);
                }
                self.consume("reasoning.budget")?;
                Ok(AdapterJson::Number(budget.to_string()))
            }
            _ => Err(AdapterValidationError::InvalidContract),
        }
    }

    pub(super) fn mistral_content(
        &mut self,
        transform: &AdapterTransform,
        scope: Scope<'a>,
    ) -> AdapterValidationResult<AdapterJson> {
        if !matches!(transform, AdapterTransform::MistralToolResultContent) {
            return Err(AdapterValidationError::InvalidContract);
        }
        let (message, result) = match scope {
            Scope::Message(message, Message::ToolResult(result))
            | Scope::MessageEntry(message, Message::ToolResult(result)) => (message, result),
            _ => return Err(AdapterValidationError::InvalidContract),
        };
        self.consume(format!("m{message}.blocks"))?;
        self.consume(format!("m{message}.status"))?;
        let matched = self.matched_result(&result.call_id)?;
        if matches!(
            matched.status,
            crate::provider_validation::prepare::ToolResultStatus::Error
        ) != result.is_error
        {
            return Err(AdapterValidationError::SourceMismatch);
        }

        let mut text = Vec::new();
        let mut images = Vec::new();
        for (block, value) in result.blocks.iter().enumerate() {
            self.consume(format!("m{message}.b{block}.variant"))?;
            match value {
                ToolResultBlock::Text(value) => {
                    self.consume(format!("m{message}.b{block}.text"))?;
                    text.push(value.text.as_str());
                }
                ToolResultBlock::Image(value) => {
                    self.consume(format!("m{message}.b{block}.bytes"))?;
                    self.consume(format!("m{message}.b{block}.media"))?;
                    images.push(value);
                }
            }
        }
        let text_plan = MistralTextPlan::new(&text, !images.is_empty(), result.is_error)?;
        let mut image_lengths = Vec::with_capacity(images.len());
        for image in &images {
            image_lengths.push(data_uri_length(image)?);
        }
        validate_mistral_aggregate(
            text_plan.result_length,
            text_plan.serialized_length,
            &image_lengths,
        )?;

        let derived_text = text_plan.build(&text)?;
        let mut chunks = Vec::with_capacity(
            images
                .len()
                .checked_add(1)
                .ok_or(AdapterValidationError::Limit)?,
        );
        chunks.push(AdapterJson::Object(vec![
            ("text".to_owned(), AdapterJson::derived_string(derived_text)),
            ("type".to_owned(), AdapterJson::ordinary_string("text")),
        ]));
        for image in images {
            chunks.push(AdapterJson::Object(vec![
                (
                    "image_url".to_owned(),
                    AdapterJson::derived_string(data_uri(image)?),
                ),
                ("type".to_owned(), AdapterJson::ordinary_string("image_url")),
            ]));
        }
        Ok(AdapterJson::Array(chunks))
    }
}

pub(in crate::provider_validation) fn encode_bytes(
    transform: &AdapterTransform,
    bytes: &[u8],
) -> AdapterValidationResult<AdapterJson> {
    let length = u64::try_from(bytes.len()).map_err(|_| AdapterValidationError::Limit)?;
    let groups = length / 3;
    let remainder = length % 3;
    let encoded_length = match transform {
        AdapterTransform::Base64StandardPadded => length
            .checked_add(2)
            .and_then(|value| value.checked_div(3))
            .and_then(|value| value.checked_mul(4)),
        AdapterTransform::Base64StandardUnpadded => groups.checked_mul(4).and_then(|value| {
            value.checked_add(match remainder {
                0 => 0,
                1 => 2,
                _ => 3,
            })
        }),
        _ => return Err(AdapterValidationError::InvalidContract),
    }
    .ok_or(AdapterValidationError::Limit)?;
    if encoded_length > crate::provider_validation::scalar::MAX_LOGICAL_CHARGE {
        return Err(AdapterValidationError::Limit);
    }
    let value = match transform {
        AdapterTransform::Base64StandardPadded => STANDARD.encode(bytes),
        AdapterTransform::Base64StandardUnpadded => STANDARD_NO_PAD.encode(bytes),
        _ => unreachable!("transform checked above"),
    };
    Ok(AdapterJson::derived_string(value))
}

fn image(scope: Scope<'_>) -> AdapterValidationResult<(usize, usize, &ImageView)> {
    match scope {
        Scope::UserBlock(message, block, UserBlock::Image(value))
        | Scope::ToolResultBlock(message, block, ToolResultBlock::Image(value)) => {
            Ok((message, block, value))
        }
        _ => Err(AdapterValidationError::InvalidContract),
    }
}

pub(in crate::provider_validation) fn data_uri(
    image: &ImageView,
) -> AdapterValidationResult<String> {
    let capacity =
        usize::try_from(data_uri_length(image)?).map_err(|_| AdapterValidationError::Limit)?;
    let mut output = String::with_capacity(capacity);
    output.push_str("data:");
    output.push_str(media_mime(&image.media_type));
    output.push_str(";base64,");
    STANDARD.encode_string(&image.bytes, &mut output);
    if output.len() != capacity {
        return Err(AdapterValidationError::SourceMismatch);
    }
    Ok(output)
}

fn data_uri_length(image: &ImageView) -> AdapterValidationResult<u64> {
    let source_length =
        u64::try_from(image.bytes.len()).map_err(|_| AdapterValidationError::Limit)?;
    let encoded_length = source_length
        .checked_add(2)
        .and_then(|value| value.checked_div(3))
        .and_then(|value| value.checked_mul(4))
        .ok_or(AdapterValidationError::Limit)?;
    let prefix_length = 5_u64
        .checked_add(
            u64::try_from(media_mime(&image.media_type).len())
                .map_err(|_| AdapterValidationError::Limit)?,
        )
        .and_then(|value| value.checked_add(8))
        .ok_or(AdapterValidationError::Limit)?;
    let total = prefix_length
        .checked_add(encoded_length)
        .ok_or(AdapterValidationError::Limit)?;
    if total > MAX_LOGICAL_CHARGE {
        return Err(AdapterValidationError::Limit);
    }
    Ok(total)
}

fn media_mime(value: &ImageMediaType) -> &'static str {
    match value {
        ImageMediaType::Png => "image/png",
        ImageMediaType::Jpeg => "image/jpeg",
        ImageMediaType::Gif => "image/gif",
        ImageMediaType::Webp => "image/webp",
        ImageMediaType::Tiff => "image/tiff",
    }
}

fn media_ordinal(value: &ImageMediaType) -> u8 {
    match value {
        ImageMediaType::Png => 0,
        ImageMediaType::Jpeg => 1,
        ImageMediaType::Gif => 2,
        ImageMediaType::Webp => 3,
        ImageMediaType::Tiff => 4,
    }
}

fn effort_ordinal(value: &ReasoningEffort) -> u8 {
    match value {
        ReasoningEffort::Minimal => 0,
        ReasoningEffort::Low => 1,
        ReasoningEffort::Medium => 2,
        ReasoningEffort::High => 3,
    }
}

fn require_identity(transform: &AdapterTransform) -> AdapterValidationResult<()> {
    matches!(transform, AdapterTransform::Identity)
        .then_some(())
        .ok_or(AdapterValidationError::InvalidContract)
}

fn require_u32(transform: &AdapterTransform) -> AdapterValidationResult<()> {
    matches!(transform, AdapterTransform::CheckedU32)
        .then_some(())
        .ok_or(AdapterValidationError::InvalidContract)
}

pub(in crate::provider_validation) fn mistral_text(
    text: &[&str],
    has_images: bool,
    is_error: bool,
) -> AdapterValidationResult<String> {
    MistralTextPlan::new(text, has_images, is_error)?.build(text)
}

struct MistralTextPlan {
    start: u64,
    end: u64,
    prefix: &'static str,
    placeholder: Option<&'static str>,
    result_length: u64,
    serialized_length: u64,
}

impl MistralTextPlan {
    fn new(text: &[&str], has_images: bool, is_error: bool) -> AdapterValidationResult<Self> {
        let mut total = 0_u64;
        let mut leading = 0_u64;
        let mut leading_open = true;
        for (index, value) in text.iter().enumerate() {
            if index != 0 {
                account_trim_char('\n', &mut total, &mut leading, &mut leading_open)?;
            }
            for character in value.chars() {
                account_trim_char(character, &mut total, &mut leading, &mut leading_open)?;
            }
        }
        if total > MAX_LOGICAL_CHARGE {
            return Err(AdapterValidationError::Limit);
        }

        let mut trailing = 0_u64;
        if leading < total {
            let mut trailing_open = true;
            for (index, value) in text.iter().rev().enumerate() {
                for character in value.chars().rev() {
                    if !trailing_open {
                        break;
                    }
                    account_trailing(character, &mut trailing, &mut trailing_open)?;
                }
                if index + 1 != text.len() && trailing_open {
                    account_trailing('\n', &mut trailing, &mut trailing_open)?;
                }
                if !trailing_open {
                    break;
                }
            }
        }
        let end = total
            .checked_sub(trailing)
            .ok_or(AdapterValidationError::Limit)?;
        let has_trimmed = leading < end;
        let prefix = if is_error { "[tool error] " } else { "" };
        let placeholder = (!has_trimmed).then_some(if has_images {
            "(see attached image)"
        } else {
            "(no tool output)"
        });
        let body_length = if let Some(value) = placeholder {
            checked_len(value.len())?
        } else {
            end.checked_sub(leading)
                .ok_or(AdapterValidationError::Limit)?
        };
        let result_length = checked_len(prefix.len())?
            .checked_add(body_length)
            .ok_or(AdapterValidationError::Limit)?;
        let escapes = if placeholder.is_some() {
            0
        } else {
            joined_escape_count(text, leading, end)?
        };
        let serialized_length = 2_u64
            .checked_add(result_length)
            .and_then(|value| value.checked_add(escapes))
            .ok_or(AdapterValidationError::Limit)?;
        if result_length > MAX_LOGICAL_CHARGE {
            return Err(AdapterValidationError::Limit);
        }
        Ok(Self {
            start: leading,
            end,
            prefix,
            placeholder,
            result_length,
            serialized_length,
        })
    }

    fn build(self, text: &[&str]) -> AdapterValidationResult<String> {
        let capacity =
            usize::try_from(self.result_length).map_err(|_| AdapterValidationError::Limit)?;
        let mut output = String::with_capacity(capacity);
        output.push_str(self.prefix);
        if let Some(placeholder) = self.placeholder {
            output.push_str(placeholder);
        } else {
            append_joined_range(text, self.start, self.end, &mut output)?;
        }
        if output.len() != capacity {
            return Err(AdapterValidationError::SourceMismatch);
        }
        Ok(output)
    }
}

fn account_trim_char(
    character: char,
    total: &mut u64,
    leading: &mut u64,
    leading_open: &mut bool,
) -> AdapterValidationResult<()> {
    let length = u64::from(character.len_utf8() as u32);
    *total = total
        .checked_add(length)
        .ok_or(AdapterValidationError::Limit)?;
    if *leading_open {
        if is_mistral_trim(character) {
            *leading = leading
                .checked_add(length)
                .ok_or(AdapterValidationError::Limit)?;
        } else {
            *leading_open = false;
        }
    }
    Ok(())
}

fn account_trailing(
    character: char,
    trailing: &mut u64,
    trailing_open: &mut bool,
) -> AdapterValidationResult<()> {
    if is_mistral_trim(character) {
        *trailing = trailing
            .checked_add(u64::from(character.len_utf8() as u32))
            .ok_or(AdapterValidationError::Limit)?;
    } else {
        *trailing_open = false;
    }
    Ok(())
}

fn append_joined_range(
    text: &[&str],
    start: u64,
    end: u64,
    output: &mut String,
) -> AdapterValidationResult<()> {
    let mut offset = 0_u64;
    for (index, value) in text.iter().enumerate() {
        if index != 0 {
            append_segment("\n", start, end, &mut offset, output)?;
        }
        append_segment(value, start, end, &mut offset, output)?;
    }
    Ok(())
}

fn append_segment(
    segment: &str,
    start: u64,
    end: u64,
    offset: &mut u64,
    output: &mut String,
) -> AdapterValidationResult<()> {
    let length = checked_len(segment.len())?;
    let segment_end = offset
        .checked_add(length)
        .ok_or(AdapterValidationError::Limit)?;
    let copy_start = start.max(*offset).min(segment_end);
    let copy_end = end.max(*offset).min(segment_end);
    if copy_start < copy_end {
        let local_start =
            usize::try_from(copy_start - *offset).map_err(|_| AdapterValidationError::Limit)?;
        let local_end =
            usize::try_from(copy_end - *offset).map_err(|_| AdapterValidationError::Limit)?;
        output.push_str(
            segment
                .get(local_start..local_end)
                .ok_or(AdapterValidationError::SourceMismatch)?,
        );
    }
    *offset = segment_end;
    Ok(())
}

fn joined_escape_count(text: &[&str], start: u64, end: u64) -> AdapterValidationResult<u64> {
    let mut offset = 0_u64;
    let mut escapes = 0_u64;
    for (index, value) in text.iter().enumerate() {
        if index != 0 {
            count_segment_escapes("\n", start, end, &mut offset, &mut escapes)?;
        }
        count_segment_escapes(value, start, end, &mut offset, &mut escapes)?;
    }
    Ok(escapes)
}

fn count_segment_escapes(
    segment: &str,
    start: u64,
    end: u64,
    offset: &mut u64,
    escapes: &mut u64,
) -> AdapterValidationResult<()> {
    for byte in segment.as_bytes() {
        if (start..end).contains(offset) && matches!(byte, b'"' | b'\\' | b'\t' | b'\n') {
            *escapes = escapes
                .checked_add(1)
                .ok_or(AdapterValidationError::Limit)?;
        }
        *offset = offset.checked_add(1).ok_or(AdapterValidationError::Limit)?;
    }
    Ok(())
}

pub(in crate::provider_validation) fn validate_mistral_aggregate(
    text_length: u64,
    text_serialized_length: u64,
    image_lengths: &[u64],
) -> AdapterValidationResult<()> {
    let mut retained = text_length;
    let mut wire = checked_add(25, text_serialized_length)?;
    let mut logical = checked_add(64, text_length)?;
    for image_length in image_lengths {
        retained = checked_add(retained, *image_length)?;
        wire = checked_add(wire, checked_add(36, *image_length)?)?;
        logical = checked_add(logical, checked_add(66, *image_length)?)?;
    }
    if retained > MAX_LOGICAL_CHARGE || wire > MAX_LOGICAL_CHARGE || logical > MAX_LOGICAL_CHARGE {
        return Err(AdapterValidationError::Limit);
    }
    Ok(())
}

fn checked_add(left: u64, right: u64) -> AdapterValidationResult<u64> {
    left.checked_add(right).ok_or(AdapterValidationError::Limit)
}

fn checked_len(length: usize) -> AdapterValidationResult<u64> {
    u64::try_from(length).map_err(|_| AdapterValidationError::Limit)
}

fn is_mistral_trim(value: char) -> bool {
    matches!(
        value,
        '\u{0009}' | '\u{000a}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}
