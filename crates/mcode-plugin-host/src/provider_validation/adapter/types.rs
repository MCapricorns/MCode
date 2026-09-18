//! Closed Host-private adapter contract vocabulary.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    InputModality, ModelSelection, ReasoningCapability, ToolCapability,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::provider_validation) enum AdapterWireId {
    AnthropicMessages,
    OpenAiCompletions,
    OpenAiResponses,
    OpenAiCodexResponses,
    AzureOpenAiResponses,
    GoogleGenerativeAi,
    GoogleVertex,
    MistralConversations,
    BedrockConverseStream,
    PiMessages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::provider_validation) enum AdapterModelSource {
    RequestedSelection,
    CurrentModel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) struct AdapterContractV1 {
    pub(in crate::provider_validation) version: u8,
    pub(in crate::provider_validation) wire_id: AdapterWireId,
    pub(in crate::provider_validation) model_source: AdapterModelSource,
    pub(in crate::provider_validation) tree: ContractTree,
    pub(in crate::provider_validation) ordinary_header_rules: Vec<OrdinaryHeaderRule>,
    pub(in crate::provider_validation) decoder_kind: AdapterDecoderKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) struct ContractTree {
    pub(in crate::provider_validation) root: u32,
    pub(in crate::provider_validation) nodes: Vec<ContractNode>,
    pub(in crate::provider_validation) tables: Vec<EnumTokenTable>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) struct ContractNode {
    pub(in crate::provider_validation) parent: Option<u32>,
    pub(in crate::provider_validation) segment: Option<PathSegment>,
    pub(in crate::provider_validation) presence: AdapterPresence,
    pub(in crate::provider_validation) presence_source: Option<AdapterPresenceSource>,
    pub(in crate::provider_validation) body: ContractNodeBody,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) enum ContractNodeBody {
    Object(ContractObject),
    Array(ContractArray),
    Switch(ContractSwitch),
    Value(ContractValue),
    Constant(ContractConstant),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) struct ContractObject {
    pub(in crate::provider_validation) children: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) struct ContractArray {
    pub(in crate::provider_validation) collection: AdapterCollection,
    pub(in crate::provider_validation) item: u32,
    pub(in crate::provider_validation) min: u32,
    pub(in crate::provider_validation) max: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) struct ContractSwitch {
    pub(in crate::provider_validation) source: AdapterVariantSource,
    pub(in crate::provider_validation) cases: Vec<ContractCase>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) struct ContractValue {
    pub(in crate::provider_validation) source: AdapterScalarSource,
    pub(in crate::provider_validation) transform: AdapterTransform,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) struct ContractConstant {
    pub(in crate::provider_validation) value: TypedJsonConstant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) struct ContractCase {
    pub(in crate::provider_validation) variant_ordinal: u8,
    pub(in crate::provider_validation) node: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) enum PathSegment {
    Key(String),
    ArrayItem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::provider_validation) enum AdapterCollection {
    System,
    Messages,
    SystemMessages,
    Blocks,
    Tools,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::provider_validation) enum AdapterVariantSource {
    ModelSelection,
    SystemMessageEntry,
    Message,
    UserBlock,
    AssistantBlock,
    ToolResultBlock,
    ToolResultStatus,
    ToolChoice,
    Reasoning,
    CacheRetention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::provider_validation) enum AdapterScalarSource {
    SelectedModel,
    SelectionKind,
    SystemItem,
    SystemJoined,
    MessageRole,
    BlockKind,
    BlockText,
    ToolResultCallId,
    ToolResultIsError,
    ToolResultStatus,
    ToolResultName,
    MistralToolResultContent,
    ToolCallId,
    ToolCallName,
    ToolCallArguments,
    ToolName,
    ToolDescription,
    ToolSchema,
    ReasoningKind,
    Proof,
    ImageBytes,
    ImageMediaType,
    ImageWidth,
    ImageHeight,
    ImageFrames,
    ImageDataUri,
    ToolChoiceKind,
    ToolChoiceName,
    ReasoningMode,
    ReasoningEffort,
    ReasoningBudget,
    CacheRetention,
    MaxOutput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) enum AdapterTransform {
    Identity,
    CheckedU32,
    CheckedU64,
    JsonSubtree,
    CanonicalJsonString,
    MistralToolResultContent,
    JoinLf,
    Base64StandardPadded,
    Base64StandardUnpadded,
    DataUri,
    EnumToken(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::provider_validation) enum AdapterPresence {
    Required,
    OmitIfNone,
    OmitForUnset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::provider_validation) enum AdapterPresenceSource {
    ReasoningProof,
    ReasoningEffort,
    ReasoningBudget,
    MaxOutput,
    ToolChoice,
    Reasoning,
    CacheRetention,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) enum TypedJsonConstant {
    Null,
    Boolean(bool),
    Number(String),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) struct EnumTokenTable {
    pub(in crate::provider_validation) source: AdapterEnumSource,
    pub(in crate::provider_validation) entries: Vec<EnumTokenEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::provider_validation) enum AdapterEnumSource {
    SelectionKind,
    MessageKind,
    UserBlockKind,
    AssistantBlockKind,
    ToolResultBlockKind,
    ToolResultStatus,
    ReasoningKind,
    ImageMediaType,
    ToolChoice,
    ReasoningMode,
    ReasoningEffort,
    CacheRetention,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) struct EnumTokenEntry {
    pub(in crate::provider_validation) variant_ordinal: u8,
    pub(in crate::provider_validation) token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) enum OrdinaryHeaderRule {
    Fixed(FixedHeaderRule),
    OneOf(OneOfHeaderRule),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) struct FixedHeaderRule {
    pub(in crate::provider_validation) name: String,
    pub(in crate::provider_validation) value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) struct OneOfHeaderRule {
    pub(in crate::provider_validation) name: String,
    pub(in crate::provider_validation) values: Vec<String>,
    pub(in crate::provider_validation) required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::provider_validation) enum AdapterDecoderKind {
    AnthropicMessages,
    OpenAiCompletions,
    OpenAiResponses,
    OpenAiCodexResponses,
    AzureOpenAiResponses,
    GoogleGenerativeAi,
    GoogleVertex,
    MistralConversations,
    BedrockConverseStream,
    PiMessages,
}

#[derive(Debug, Clone, Copy)]
pub(in crate::provider_validation) struct ValidatedCatalogEntryView<'a> {
    pub(in crate::provider_validation) provider_id: &'a str,
    pub(in crate::provider_validation) route_id: &'a str,
    pub(in crate::provider_validation) catalog_digest: &'a str,
    pub(in crate::provider_validation) selection: &'a ModelSelection,
    pub(in crate::provider_validation) current_model: &'a str,
    pub(in crate::provider_validation) input_modalities: &'a [InputModality],
    pub(in crate::provider_validation) tool_capability: &'a ToolCapability,
    pub(in crate::provider_validation) reasoning_capability: &'a ReasoningCapability,
    pub(in crate::provider_validation) context_tokens: Option<u64>,
    pub(in crate::provider_validation) max_output_tokens: Option<u64>,
    pub(in crate::provider_validation) completion_operation: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) struct ValidatedAdapter {
    pub(in crate::provider_validation) wire_id: AdapterWireId,
    pub(in crate::provider_validation) decoder_kind: AdapterDecoderKind,
    pub(in crate::provider_validation) contract_digest: String,
    pub(in crate::provider_validation) body_digest: String,
    pub(in crate::provider_validation) ordinary_header_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::provider_validation) enum AdapterValidationError {
    InvalidContract,
    SourceMismatch,
    BodyMismatch,
    HeaderMismatch,
    CapabilityMismatch,
    Limit,
}

pub(in crate::provider_validation) type AdapterValidationResult<T = ValidatedAdapter> =
    Result<T, AdapterValidationError>;

impl AdapterWireId {
    pub(in crate::provider_validation) const fn ordinal(self) -> u8 {
        match self {
            Self::AnthropicMessages => 0,
            Self::OpenAiCompletions => 1,
            Self::OpenAiResponses => 2,
            Self::OpenAiCodexResponses => 3,
            Self::AzureOpenAiResponses => 4,
            Self::GoogleGenerativeAi => 5,
            Self::GoogleVertex => 6,
            Self::MistralConversations => 7,
            Self::BedrockConverseStream => 8,
            Self::PiMessages => 9,
        }
    }
}

impl AdapterDecoderKind {
    pub(in crate::provider_validation) const fn ordinal(self) -> u8 {
        match self {
            Self::AnthropicMessages => 0,
            Self::OpenAiCompletions => 1,
            Self::OpenAiResponses => 2,
            Self::OpenAiCodexResponses => 3,
            Self::AzureOpenAiResponses => 4,
            Self::GoogleGenerativeAi => 5,
            Self::GoogleVertex => 6,
            Self::MistralConversations => 7,
            Self::BedrockConverseStream => 8,
            Self::PiMessages => 9,
        }
    }
}

impl AdapterVariantSource {
    pub(in crate::provider_validation) const fn variant_count(self) -> u8 {
        match self {
            Self::ModelSelection
            | Self::SystemMessageEntry
            | Self::UserBlock
            | Self::ToolResultBlock
            | Self::ToolResultStatus => 2,
            Self::Message | Self::AssistantBlock | Self::Reasoning => 3,
            Self::ToolChoice | Self::CacheRetention => 4,
        }
    }
}

impl AdapterEnumSource {
    pub(in crate::provider_validation) const fn variant_count(self) -> u8 {
        match self {
            Self::SelectionKind
            | Self::UserBlockKind
            | Self::ToolResultBlockKind
            | Self::ToolResultStatus
            | Self::ReasoningKind => 2,
            Self::MessageKind | Self::AssistantBlockKind => 3,
            Self::ToolChoice | Self::ReasoningEffort | Self::CacheRetention => 4,
            Self::ImageMediaType => 5,
            Self::ReasoningMode => 3,
        }
    }
}
