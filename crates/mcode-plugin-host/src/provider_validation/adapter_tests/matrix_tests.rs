//! Exhaustive closed source, transform, enum, and wire matrix tests.
use super::super::adapter::types::{
    AdapterEnumSource, AdapterScalarSource, AdapterTransform, AdapterWireId,
};
use super::super::adapter::validate_transform;
use super::fixtures::single_value_contract;

const SOURCES: [AdapterScalarSource; 33] = [
    AdapterScalarSource::SelectedModel,
    AdapterScalarSource::SelectionKind,
    AdapterScalarSource::SystemItem,
    AdapterScalarSource::SystemJoined,
    AdapterScalarSource::MessageRole,
    AdapterScalarSource::BlockKind,
    AdapterScalarSource::BlockText,
    AdapterScalarSource::ToolResultCallId,
    AdapterScalarSource::ToolResultIsError,
    AdapterScalarSource::ToolResultStatus,
    AdapterScalarSource::ToolResultName,
    AdapterScalarSource::MistralToolResultContent,
    AdapterScalarSource::ToolCallId,
    AdapterScalarSource::ToolCallName,
    AdapterScalarSource::ToolCallArguments,
    AdapterScalarSource::ToolName,
    AdapterScalarSource::ToolDescription,
    AdapterScalarSource::ToolSchema,
    AdapterScalarSource::ReasoningKind,
    AdapterScalarSource::Proof,
    AdapterScalarSource::ImageBytes,
    AdapterScalarSource::ImageMediaType,
    AdapterScalarSource::ImageWidth,
    AdapterScalarSource::ImageHeight,
    AdapterScalarSource::ImageFrames,
    AdapterScalarSource::ImageDataUri,
    AdapterScalarSource::ToolChoiceKind,
    AdapterScalarSource::ToolChoiceName,
    AdapterScalarSource::ReasoningMode,
    AdapterScalarSource::ReasoningEffort,
    AdapterScalarSource::ReasoningBudget,
    AdapterScalarSource::CacheRetention,
    AdapterScalarSource::MaxOutput,
];

const WIRES: [AdapterWireId; 10] = [
    AdapterWireId::AnthropicMessages,
    AdapterWireId::OpenAiCompletions,
    AdapterWireId::OpenAiResponses,
    AdapterWireId::OpenAiCodexResponses,
    AdapterWireId::AzureOpenAiResponses,
    AdapterWireId::GoogleGenerativeAi,
    AdapterWireId::GoogleVertex,
    AdapterWireId::MistralConversations,
    AdapterWireId::BedrockConverseStream,
    AdapterWireId::PiMessages,
];

#[test]
fn every_source_transform_wire_cell_matches_the_closed_matrix() {
    for wire in WIRES {
        for source in SOURCES {
            for transform in transforms() {
                let table = table_for(source, &transform);
                let contract = single_value_contract(wire, source, transform.clone(), table);
                assert_eq!(
                    validate_transform(&contract, source, &transform).is_ok(),
                    legal(wire, source, &transform),
                    "{wire:?} {source:?} {transform:?}"
                );
            }
        }
    }
}

#[test]
fn every_enum_source_rejects_a_crossed_table() {
    for source in SOURCES {
        let Some(table) = enum_table(source) else {
            continue;
        };
        let crossed = if table == AdapterEnumSource::SelectionKind {
            AdapterEnumSource::ReasoningKind
        } else {
            AdapterEnumSource::SelectionKind
        };
        let transform = AdapterTransform::EnumToken(0);
        let contract = single_value_contract(
            AdapterWireId::AnthropicMessages,
            source,
            transform.clone(),
            Some(crossed),
        );
        assert!(
            validate_transform(&contract, source, &transform).is_err(),
            "{source:?}"
        );
    }
}

fn transforms() -> [AdapterTransform; 11] {
    [
        AdapterTransform::Identity,
        AdapterTransform::CheckedU32,
        AdapterTransform::CheckedU64,
        AdapterTransform::JsonSubtree,
        AdapterTransform::CanonicalJsonString,
        AdapterTransform::MistralToolResultContent,
        AdapterTransform::JoinLf,
        AdapterTransform::Base64StandardPadded,
        AdapterTransform::Base64StandardUnpadded,
        AdapterTransform::DataUri,
        AdapterTransform::EnumToken(0),
    ]
}

fn legal(wire: AdapterWireId, source: AdapterScalarSource, transform: &AdapterTransform) -> bool {
    use AdapterScalarSource as Source;
    use AdapterTransform as Transform;

    match source {
        Source::SelectedModel
        | Source::SystemItem
        | Source::BlockText
        | Source::ToolResultCallId
        | Source::ToolResultIsError
        | Source::ToolResultName
        | Source::ToolCallId
        | Source::ToolCallName
        | Source::ToolName
        | Source::ToolDescription
        | Source::ToolChoiceName => matches!(transform, Transform::Identity),
        Source::SelectionKind
        | Source::MessageRole
        | Source::BlockKind
        | Source::ToolResultStatus
        | Source::ReasoningKind
        | Source::ImageMediaType
        | Source::ToolChoiceKind
        | Source::ReasoningMode
        | Source::ReasoningEffort
        | Source::CacheRetention => matches!(transform, Transform::EnumToken(0)),
        Source::ImageWidth | Source::ImageHeight | Source::ImageFrames => {
            matches!(transform, Transform::CheckedU32)
        }
        Source::ReasoningBudget | Source::MaxOutput => {
            matches!(transform, Transform::CheckedU64)
        }
        Source::ToolCallArguments => {
            matches!(transform, Transform::JsonSubtree)
                || (matches!(transform, Transform::CanonicalJsonString)
                    && matches!(
                        wire,
                        AdapterWireId::OpenAiCompletions
                            | AdapterWireId::OpenAiResponses
                            | AdapterWireId::OpenAiCodexResponses
                            | AdapterWireId::AzureOpenAiResponses
                            | AdapterWireId::MistralConversations
                    ))
        }
        Source::ToolSchema => matches!(transform, Transform::JsonSubtree),
        Source::Proof | Source::ImageBytes => matches!(
            transform,
            Transform::Base64StandardPadded | Transform::Base64StandardUnpadded
        ),
        Source::SystemJoined => matches!(transform, Transform::JoinLf),
        Source::ImageDataUri => matches!(transform, Transform::DataUri),
        Source::MistralToolResultContent => {
            wire == AdapterWireId::MistralConversations
                && matches!(transform, Transform::MistralToolResultContent)
        }
    }
}

fn table_for(
    source: AdapterScalarSource,
    transform: &AdapterTransform,
) -> Option<AdapterEnumSource> {
    matches!(transform, AdapterTransform::EnumToken(_))
        .then(|| enum_table(source))
        .flatten()
}

fn enum_table(source: AdapterScalarSource) -> Option<AdapterEnumSource> {
    match source {
        AdapterScalarSource::SelectionKind => Some(AdapterEnumSource::SelectionKind),
        AdapterScalarSource::MessageRole => Some(AdapterEnumSource::MessageKind),
        AdapterScalarSource::BlockKind => Some(AdapterEnumSource::UserBlockKind),
        AdapterScalarSource::ToolResultStatus => Some(AdapterEnumSource::ToolResultStatus),
        AdapterScalarSource::ReasoningKind => Some(AdapterEnumSource::ReasoningKind),
        AdapterScalarSource::ImageMediaType => Some(AdapterEnumSource::ImageMediaType),
        AdapterScalarSource::ToolChoiceKind => Some(AdapterEnumSource::ToolChoice),
        AdapterScalarSource::ReasoningMode => Some(AdapterEnumSource::ReasoningMode),
        AdapterScalarSource::ReasoningEffort => Some(AdapterEnumSource::ReasoningEffort),
        AdapterScalarSource::CacheRetention => Some(AdapterEnumSource::CacheRetention),
        _ => None,
    }
}
