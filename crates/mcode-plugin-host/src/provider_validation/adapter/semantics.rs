//! Lexical and wire-policy proofs for closed adapter contracts.
use super::types::{
    AdapterCollection, AdapterContractV1, AdapterEnumSource, AdapterPresence,
    AdapterPresenceSource, AdapterScalarSource, AdapterTransform, AdapterValidationError,
    AdapterValidationResult, AdapterVariantSource, AdapterWireId, ContractNodeBody,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LexicalScope {
    Root,
    System,
    SystemMessageEntry,
    Message,
    MessageEntry,
    UserBlock,
    AssistantBlock,
    ToolResultBlock,
    Tool,
}

#[derive(Debug, Clone, Copy)]
struct ActiveCase {
    source: AdapterVariantSource,
    ordinal: u8,
}

struct Context {
    scope: LexicalScope,
    cases: Vec<ActiveCase>,
    wrappers: Vec<AdapterPresenceSource>,
    root_collections: RootCollectionAccounting,
}

#[derive(Default)]
struct RootCollectionAccounting {
    system_messages: bool,
    system: bool,
    messages: bool,
    system_joined: bool,
}

#[derive(Default)]
struct MistralAccounting {
    normal_blocks: u8,
    composites: u8,
}

pub(super) fn validate_semantics(contract: &AdapterContractV1) -> AdapterValidationResult<()> {
    let mut context = Context {
        scope: LexicalScope::Root,
        cases: Vec::new(),
        wrappers: Vec::new(),
        root_collections: RootCollectionAccounting::default(),
    };
    let mut mistral = Vec::new();
    walk_node(
        contract,
        contract.tree.root as usize,
        &mut context,
        &mut mistral,
    )
}

fn walk_node(
    contract: &AdapterContractV1,
    index: usize,
    context: &mut Context,
    mistral: &mut Vec<MistralAccounting>,
) -> AdapterValidationResult<()> {
    let node = &contract.tree.nodes[index];
    validate_wrapper(node.presence, node.presence_source, context)?;
    let pushed_wrapper = if node.presence == AdapterPresence::Required {
        false
    } else {
        context.wrappers.push(
            node.presence_source
                .ok_or(AdapterValidationError::InvalidContract)?,
        );
        true
    };

    let result = match &node.body {
        ContractNodeBody::Object(object) => {
            for child in &object.children {
                walk_node(contract, *child as usize, context, mistral)?;
            }
            Ok(())
        }
        ContractNodeBody::Array(array) => {
            validate_root_collection(array.collection, context)?;
            if array.collection == AdapterCollection::SystemMessages
                && !matches!(
                    contract.wire_id,
                    AdapterWireId::OpenAiCompletions | AdapterWireId::MistralConversations
                )
            {
                return Err(AdapterValidationError::InvalidContract);
            }
            if array.collection == AdapterCollection::Blocks
                && contract.wire_id == AdapterWireId::MistralConversations
                && is_tool_result_message(context)
            {
                let accounting = mistral
                    .last_mut()
                    .ok_or(AdapterValidationError::InvalidContract)?;
                accounting.normal_blocks = accounting
                    .normal_blocks
                    .checked_add(1)
                    .ok_or(AdapterValidationError::InvalidContract)?;
            }
            let previous = context.scope;
            context.scope = collection_item_scope(array.collection, context)?;
            let result = walk_node(contract, array.item as usize, context, mistral);
            context.scope = previous;
            result
        }
        ContractNodeBody::Switch(value) => walk_switch(contract, value, context, mistral),
        ContractNodeBody::Value(value) => {
            if value.source == AdapterScalarSource::SystemJoined {
                validate_system_joined(context)?;
            }
            validate_scalar(contract, value.source, &value.transform, context)?;
            if value.source == AdapterScalarSource::MistralToolResultContent {
                let accounting = mistral
                    .last_mut()
                    .ok_or(AdapterValidationError::InvalidContract)?;
                accounting.composites = accounting
                    .composites
                    .checked_add(1)
                    .ok_or(AdapterValidationError::InvalidContract)?;
            }
            Ok(())
        }
        ContractNodeBody::Constant(_) => Ok(()),
    };

    if pushed_wrapper {
        context.wrappers.pop();
    }
    result
}

fn walk_switch(
    contract: &AdapterContractV1,
    value: &super::types::ContractSwitch,
    context: &mut Context,
    mistral: &mut Vec<MistralAccounting>,
) -> AdapterValidationResult<()> {
    validate_switch(contract.wire_id, value.source, context)?;
    for case in &value.cases {
        let previous_scope = context.scope;
        if value.source == AdapterVariantSource::SystemMessageEntry {
            context.scope = if case.variant_ordinal == 0 {
                LexicalScope::System
            } else {
                LexicalScope::MessageEntry
            };
        }
        context.cases.push(ActiveCase {
            source: value.source,
            ordinal: case.variant_ordinal,
        });
        let track_mistral = contract.wire_id == AdapterWireId::MistralConversations
            && value.source == AdapterVariantSource::Message
            && case.variant_ordinal == 2;
        if track_mistral {
            mistral.push(MistralAccounting::default());
        }
        walk_node(contract, case.node as usize, context, mistral)?;
        if track_mistral {
            let accounting = mistral
                .pop()
                .ok_or(AdapterValidationError::InvalidContract)?;
            if accounting.normal_blocks != 0 || accounting.composites != 1 {
                return Err(AdapterValidationError::InvalidContract);
            }
        }
        context.cases.pop();
        context.scope = previous_scope;
    }
    Ok(())
}

fn validate_wrapper(
    presence: AdapterPresence,
    source: Option<AdapterPresenceSource>,
    context: &Context,
) -> AdapterValidationResult<()> {
    if source.is_some_and(|source| has_wrapper(context, source)) {
        return Err(AdapterValidationError::InvalidContract);
    }
    let valid = match (presence, source) {
        (AdapterPresence::Required, None) => true,
        (AdapterPresence::OmitIfNone, Some(AdapterPresenceSource::ReasoningProof)) => {
            context.scope == LexicalScope::AssistantBlock
                && active_ordinal(context, AdapterVariantSource::AssistantBlock) == Some(1)
        }
        (
            AdapterPresence::OmitIfNone,
            Some(AdapterPresenceSource::ReasoningEffort | AdapterPresenceSource::ReasoningBudget),
        ) => {
            context.scope == LexicalScope::Root
                && active_ordinal(context, AdapterVariantSource::Reasoning) == Some(2)
        }
        (AdapterPresence::OmitIfNone, Some(AdapterPresenceSource::MaxOutput))
        | (
            AdapterPresence::OmitForUnset,
            Some(
                AdapterPresenceSource::ToolChoice
                | AdapterPresenceSource::Reasoning
                | AdapterPresenceSource::CacheRetention,
            ),
        ) => context.scope == LexicalScope::Root,
        _ => false,
    };
    valid
        .then_some(())
        .ok_or(AdapterValidationError::InvalidContract)
}

fn validate_switch(
    wire: AdapterWireId,
    source: AdapterVariantSource,
    context: &Context,
) -> AdapterValidationResult<()> {
    let valid = match source {
        AdapterVariantSource::ModelSelection => context.scope == LexicalScope::Root,
        AdapterVariantSource::SystemMessageEntry => {
            context.scope == LexicalScope::SystemMessageEntry
        }
        AdapterVariantSource::Message => matches!(
            context.scope,
            LexicalScope::Message | LexicalScope::MessageEntry
        ),
        AdapterVariantSource::UserBlock => context.scope == LexicalScope::UserBlock,
        AdapterVariantSource::AssistantBlock => context.scope == LexicalScope::AssistantBlock,
        AdapterVariantSource::ToolResultBlock => context.scope == LexicalScope::ToolResultBlock,
        AdapterVariantSource::ToolResultStatus => {
            matches!(
                wire,
                AdapterWireId::GoogleGenerativeAi | AdapterWireId::GoogleVertex
            ) && is_tool_result_message(context)
        }
        AdapterVariantSource::ToolChoice => {
            context.scope == LexicalScope::Root
                && has_wrapper(context, AdapterPresenceSource::ToolChoice)
        }
        AdapterVariantSource::Reasoning => {
            context.scope == LexicalScope::Root
                && has_wrapper(context, AdapterPresenceSource::Reasoning)
        }
        AdapterVariantSource::CacheRetention => {
            context.scope == LexicalScope::Root
                && has_wrapper(context, AdapterPresenceSource::CacheRetention)
        }
    };
    valid
        .then_some(())
        .ok_or(AdapterValidationError::InvalidContract)
}

fn validate_root_collection(
    collection: AdapterCollection,
    context: &mut Context,
) -> AdapterValidationResult<()> {
    if context.scope != LexicalScope::Root {
        return Ok(());
    }
    let accounting = &mut context.root_collections;
    let valid = match collection {
        AdapterCollection::SystemMessages => {
            let valid = !accounting.system_messages
                && !accounting.system
                && !accounting.messages
                && !accounting.system_joined;
            accounting.system_messages = true;
            valid
        }
        AdapterCollection::System => {
            accounting.system = true;
            !accounting.system_messages
        }
        AdapterCollection::Messages => {
            accounting.messages = true;
            !accounting.system_messages
        }
        AdapterCollection::Blocks | AdapterCollection::Tools => true,
    };
    valid
        .then_some(())
        .ok_or(AdapterValidationError::InvalidContract)
}

fn validate_system_joined(context: &mut Context) -> AdapterValidationResult<()> {
    if context.scope != LexicalScope::Root || context.root_collections.system_messages {
        return Err(AdapterValidationError::InvalidContract);
    }
    context.root_collections.system_joined = true;
    Ok(())
}

fn collection_item_scope(
    collection: AdapterCollection,
    context: &Context,
) -> AdapterValidationResult<LexicalScope> {
    match collection {
        AdapterCollection::System if context.scope == LexicalScope::Root => {
            Ok(LexicalScope::System)
        }
        AdapterCollection::Messages if context.scope == LexicalScope::Root => {
            Ok(LexicalScope::Message)
        }
        AdapterCollection::SystemMessages if context.scope == LexicalScope::Root => {
            Ok(LexicalScope::SystemMessageEntry)
        }
        AdapterCollection::Tools if context.scope == LexicalScope::Root => Ok(LexicalScope::Tool),
        AdapterCollection::Blocks => match active_ordinal(context, AdapterVariantSource::Message) {
            Some(0)
                if matches!(
                    context.scope,
                    LexicalScope::Message | LexicalScope::MessageEntry
                ) =>
            {
                Ok(LexicalScope::UserBlock)
            }
            Some(1)
                if matches!(
                    context.scope,
                    LexicalScope::Message | LexicalScope::MessageEntry
                ) =>
            {
                Ok(LexicalScope::AssistantBlock)
            }
            Some(2)
                if matches!(
                    context.scope,
                    LexicalScope::Message | LexicalScope::MessageEntry
                ) =>
            {
                Ok(LexicalScope::ToolResultBlock)
            }
            _ => Err(AdapterValidationError::InvalidContract),
        },
        _ => Err(AdapterValidationError::InvalidContract),
    }
}

fn validate_scalar(
    contract: &AdapterContractV1,
    source: AdapterScalarSource,
    transform: &AdapterTransform,
    context: &Context,
) -> AdapterValidationResult<()> {
    use AdapterScalarSource as Source;
    let valid = match source {
        Source::SelectedModel | Source::SystemJoined => context.scope == LexicalScope::Root,
        Source::SelectionKind => in_switch(context, AdapterVariantSource::ModelSelection),
        Source::SystemItem => matches!(context.scope, LexicalScope::System),
        Source::MessageRole => {
            matches!(
                context.scope,
                LexicalScope::Message | LexicalScope::MessageEntry
            ) && in_switch(context, AdapterVariantSource::Message)
        }
        Source::BlockKind => validate_block_kind(contract, transform, context),
        Source::BlockText => is_text_block(context),
        Source::ToolResultCallId => is_tool_result_message(context),
        Source::ToolResultIsError => {
            matches!(
                contract.wire_id,
                AdapterWireId::AnthropicMessages | AdapterWireId::PiMessages
            ) && is_tool_result_message(context)
        }
        Source::ToolResultStatus => {
            contract.wire_id == AdapterWireId::BedrockConverseStream
                && is_tool_result_message(context)
                && is_exact_bedrock_status_table(contract, transform)
        }
        Source::ToolResultName => {
            matches!(
                contract.wire_id,
                AdapterWireId::PiMessages
                    | AdapterWireId::GoogleGenerativeAi
                    | AdapterWireId::GoogleVertex
                    | AdapterWireId::MistralConversations
            ) && is_tool_result_message(context)
        }
        Source::MistralToolResultContent => {
            contract.wire_id == AdapterWireId::MistralConversations
                && is_tool_result_message(context)
        }
        Source::ToolCallId | Source::ToolCallName | Source::ToolCallArguments => {
            context.scope == LexicalScope::AssistantBlock
                && active_ordinal(context, AdapterVariantSource::AssistantBlock) == Some(2)
        }
        Source::ToolName | Source::ToolDescription | Source::ToolSchema => {
            context.scope == LexicalScope::Tool
        }
        Source::ReasoningKind => {
            context.scope == LexicalScope::AssistantBlock
                && active_ordinal(context, AdapterVariantSource::AssistantBlock) == Some(1)
        }
        Source::Proof => {
            context.scope == LexicalScope::AssistantBlock
                && active_ordinal(context, AdapterVariantSource::AssistantBlock) == Some(1)
                && has_wrapper(context, AdapterPresenceSource::ReasoningProof)
        }
        Source::ImageBytes
        | Source::ImageMediaType
        | Source::ImageWidth
        | Source::ImageHeight
        | Source::ImageFrames
        | Source::ImageDataUri => is_image_block(context),
        Source::ToolChoiceKind => {
            in_switch(context, AdapterVariantSource::ToolChoice)
                && has_wrapper(context, AdapterPresenceSource::ToolChoice)
        }
        Source::ToolChoiceName => {
            active_ordinal(context, AdapterVariantSource::ToolChoice) == Some(3)
                && has_wrapper(context, AdapterPresenceSource::ToolChoice)
        }
        Source::ReasoningMode => {
            in_switch(context, AdapterVariantSource::Reasoning)
                && has_wrapper(context, AdapterPresenceSource::Reasoning)
        }
        Source::ReasoningEffort => {
            context.scope == LexicalScope::Root
                && active_ordinal(context, AdapterVariantSource::Reasoning) == Some(2)
                && has_wrapper(context, AdapterPresenceSource::ReasoningEffort)
        }
        Source::ReasoningBudget => {
            context.scope == LexicalScope::Root
                && active_ordinal(context, AdapterVariantSource::Reasoning) == Some(2)
                && has_wrapper(context, AdapterPresenceSource::ReasoningBudget)
        }
        Source::CacheRetention => {
            in_switch(context, AdapterVariantSource::CacheRetention)
                && has_wrapper(context, AdapterPresenceSource::CacheRetention)
        }
        Source::MaxOutput => {
            context.scope == LexicalScope::Root
                && has_wrapper(context, AdapterPresenceSource::MaxOutput)
        }
    };
    valid
        .then_some(())
        .ok_or(AdapterValidationError::InvalidContract)
}

fn is_exact_bedrock_status_table(
    contract: &AdapterContractV1,
    transform: &AdapterTransform,
) -> bool {
    let AdapterTransform::EnumToken(index) = transform else {
        return false;
    };
    let Some(table) = contract.tree.tables.get(usize::from(*index)) else {
        return false;
    };
    table.source == AdapterEnumSource::ToolResultStatus
        && table.entries.len() == 2
        && table.entries[0].variant_ordinal == 0
        && table.entries[0].token == "success"
        && table.entries[1].variant_ordinal == 1
        && table.entries[1].token == "error"
}

fn validate_block_kind(
    contract: &AdapterContractV1,
    transform: &AdapterTransform,
    context: &Context,
) -> bool {
    let (variant, table_source) = match context.scope {
        LexicalScope::UserBlock => (
            AdapterVariantSource::UserBlock,
            AdapterEnumSource::UserBlockKind,
        ),
        LexicalScope::AssistantBlock => (
            AdapterVariantSource::AssistantBlock,
            AdapterEnumSource::AssistantBlockKind,
        ),
        LexicalScope::ToolResultBlock => (
            AdapterVariantSource::ToolResultBlock,
            AdapterEnumSource::ToolResultBlockKind,
        ),
        _ => return false,
    };
    let AdapterTransform::EnumToken(table) = transform else {
        return false;
    };
    in_switch(context, variant)
        && contract
            .tree
            .tables
            .get(usize::from(*table))
            .is_some_and(|table| table.source == table_source)
}

fn is_text_block(context: &Context) -> bool {
    match context.scope {
        LexicalScope::UserBlock => {
            active_ordinal(context, AdapterVariantSource::UserBlock) == Some(0)
        }
        LexicalScope::AssistantBlock => matches!(
            active_ordinal(context, AdapterVariantSource::AssistantBlock),
            Some(0 | 1)
        ),
        LexicalScope::ToolResultBlock => {
            active_ordinal(context, AdapterVariantSource::ToolResultBlock) == Some(0)
        }
        _ => false,
    }
}

fn is_image_block(context: &Context) -> bool {
    matches!(
        context.scope,
        LexicalScope::UserBlock | LexicalScope::ToolResultBlock
    ) && (active_ordinal(context, AdapterVariantSource::UserBlock) == Some(1)
        || active_ordinal(context, AdapterVariantSource::ToolResultBlock) == Some(1))
}

fn is_tool_result_message(context: &Context) -> bool {
    matches!(
        context.scope,
        LexicalScope::Message | LexicalScope::MessageEntry
    ) && active_ordinal(context, AdapterVariantSource::Message) == Some(2)
}

fn in_switch(context: &Context, source: AdapterVariantSource) -> bool {
    active_ordinal(context, source).is_some()
}

fn active_ordinal(context: &Context, source: AdapterVariantSource) -> Option<u8> {
    context
        .cases
        .iter()
        .rev()
        .find(|case| case.source == source)
        .map(|case| case.ordinal)
}

fn has_wrapper(context: &Context, source: AdapterPresenceSource) -> bool {
    context.wrappers.contains(&source)
}
