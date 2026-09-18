//! Trusted in-memory dummy adapter fixtures.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    CatalogEntry, OrdinaryHeader, PrepareInput, WireJsonArray, WireJsonDocument, WireJsonField,
    WireJsonNode, WireJsonObject,
};

use super::super::adapter::types::{
    AdapterCollection, AdapterContractV1, AdapterDecoderKind, AdapterEnumSource,
    AdapterModelSource, AdapterPresence, AdapterPresenceSource, AdapterScalarSource,
    AdapterTransform, AdapterWireId, ContractArray, ContractCase, ContractConstant, ContractNode,
    ContractNodeBody, ContractObject, ContractSwitch, ContractTree, ContractValue, EnumTokenEntry,
    EnumTokenTable, FixedHeaderRule, OneOfHeaderRule, OrdinaryHeaderRule, PathSegment,
    TypedJsonConstant, ValidatedCatalogEntryView,
};
use super::super::test_support::{DIGEST, catalog_entry, prepare_input};

pub(super) struct DummyFixture {
    pub(super) contract: AdapterContractV1,
    pub(super) entry: CatalogEntry,
    pub(super) original: PrepareInput,
    pub(super) body: WireJsonDocument,
    pub(super) headers: Vec<OrdinaryHeader>,
}

impl DummyFixture {
    pub(super) fn selected(&self) -> ValidatedCatalogEntryView<'_> {
        ValidatedCatalogEntryView {
            provider_id: "provider",
            route_id: "route",
            catalog_digest: DIGEST,
            selection: &self.entry.selection,
            current_model: &self.entry.current_model,
            input_modalities: &self.entry.input_modalities,
            tool_capability: &self.entry.tool_capability,
            reasoning_capability: &self.entry.reasoning_capability,
            context_tokens: self.entry.context_tokens,
            max_output_tokens: self.entry.max_output_tokens,
            completion_operation: &self.entry.completion_operation,
        }
    }
}

pub(super) fn single_value_contract(
    wire_id: AdapterWireId,
    source: AdapterScalarSource,
    transform: AdapterTransform,
    table_source: Option<AdapterEnumSource>,
) -> AdapterContractV1 {
    let tables = table_source
        .map(|source| {
            vec![EnumTokenTable {
                source,
                entries: (0..source.variant_count())
                    .map(|variant_ordinal| EnumTokenEntry {
                        variant_ordinal,
                        token: format!("v{variant_ordinal}"),
                    })
                    .collect(),
            }]
        })
        .unwrap_or_default();
    AdapterContractV1 {
        version: 1,
        wire_id,
        model_source: AdapterModelSource::CurrentModel,
        tree: ContractTree {
            root: 1,
            nodes: vec![
                ContractNode {
                    parent: Some(1),
                    segment: Some(PathSegment::Key("value".to_owned())),
                    presence: AdapterPresence::Required,
                    presence_source: None,
                    body: ContractNodeBody::Value(ContractValue { source, transform }),
                },
                ContractNode {
                    parent: None,
                    segment: None,
                    presence: AdapterPresence::Required,
                    presence_source: None,
                    body: ContractNodeBody::Object(ContractObject { children: vec![0] }),
                },
            ],
            tables,
        },
        ordinary_header_rules: vec![],
        decoder_kind: decoder_for(wire_id),
    }
}

pub(super) fn decoder_for(wire_id: AdapterWireId) -> AdapterDecoderKind {
    match wire_id {
        AdapterWireId::AnthropicMessages => AdapterDecoderKind::AnthropicMessages,
        AdapterWireId::OpenAiCompletions => AdapterDecoderKind::OpenAiCompletions,
        AdapterWireId::OpenAiResponses => AdapterDecoderKind::OpenAiResponses,
        AdapterWireId::OpenAiCodexResponses => AdapterDecoderKind::OpenAiCodexResponses,
        AdapterWireId::AzureOpenAiResponses => AdapterDecoderKind::AzureOpenAiResponses,
        AdapterWireId::GoogleGenerativeAi => AdapterDecoderKind::GoogleGenerativeAi,
        AdapterWireId::GoogleVertex => AdapterDecoderKind::GoogleVertex,
        AdapterWireId::MistralConversations => AdapterDecoderKind::MistralConversations,
        AdapterWireId::BedrockConverseStream => AdapterDecoderKind::BedrockConverseStream,
        AdapterWireId::PiMessages => AdapterDecoderKind::PiMessages,
    }
}

pub(super) fn minimal_fixture() -> DummyFixture {
    let mut builder = ContractBuilder::default();
    let selection_table = EnumTokenTable {
        source: AdapterEnumSource::SelectionKind,
        entries: vec![
            EnumTokenEntry {
                variant_ordinal: 0,
                token: "exact".to_owned(),
            },
            EnumTokenEntry {
                variant_ordinal: 1,
                token: "alias".to_owned(),
            },
        ],
    };
    let model_cases = [0, 1].map(|_| {
        let kind = builder.value(
            Some(PathSegment::Key("kind".to_owned())),
            AdapterPresence::Required,
            None,
            AdapterScalarSource::SelectionKind,
            AdapterTransform::EnumToken(0),
        );
        let value = builder.value(
            Some(PathSegment::Key("value".to_owned())),
            AdapterPresence::Required,
            None,
            AdapterScalarSource::SelectedModel,
            AdapterTransform::Identity,
        );
        builder.object(None, AdapterPresence::Required, None, vec![kind, value])
    });
    let model = builder.switch(
        Some(PathSegment::Key("model".to_owned())),
        AdapterPresence::Required,
        None,
        super::super::adapter::types::AdapterVariantSource::ModelSelection,
        model_cases.to_vec(),
    );

    let system_item = builder.value(
        Some(PathSegment::ArrayItem),
        AdapterPresence::Required,
        None,
        AdapterScalarSource::SystemItem,
        AdapterTransform::Identity,
    );
    let system = builder.array(
        Some(PathSegment::Key("system".to_owned())),
        AdapterCollection::System,
        system_item,
        0,
        1_024,
    );
    let message_cases = (0..3)
        .map(|_| {
            builder.constant(
                None,
                AdapterPresence::Required,
                None,
                TypedJsonConstant::Null,
            )
        })
        .collect();
    let message_item = builder.switch(
        Some(PathSegment::ArrayItem),
        AdapterPresence::Required,
        None,
        super::super::adapter::types::AdapterVariantSource::Message,
        message_cases,
    );
    let messages = builder.array(
        Some(PathSegment::Key("messages".to_owned())),
        AdapterCollection::Messages,
        message_item,
        0,
        4_096,
    );
    let tool_name = builder.value(
        Some(PathSegment::Key("name".to_owned())),
        AdapterPresence::Required,
        None,
        AdapterScalarSource::ToolName,
        AdapterTransform::Identity,
    );
    let tool_description = builder.value(
        Some(PathSegment::Key("description".to_owned())),
        AdapterPresence::Required,
        None,
        AdapterScalarSource::ToolDescription,
        AdapterTransform::Identity,
    );
    let tool_schema = builder.value(
        Some(PathSegment::Key("schema".to_owned())),
        AdapterPresence::Required,
        None,
        AdapterScalarSource::ToolSchema,
        AdapterTransform::JsonSubtree,
    );
    let tool_item = builder.object(
        Some(PathSegment::ArrayItem),
        AdapterPresence::Required,
        None,
        vec![tool_description, tool_name, tool_schema],
    );
    let tools = builder.array(
        Some(PathSegment::Key("tools".to_owned())),
        AdapterCollection::Tools,
        tool_item,
        0,
        1_024,
    );
    let cache = builder.constant(
        Some(PathSegment::Key("cache".to_owned())),
        AdapterPresence::OmitForUnset,
        Some(AdapterPresenceSource::CacheRetention),
        TypedJsonConstant::Null,
    );
    let max_output = builder.constant(
        Some(PathSegment::Key("maxOutput".to_owned())),
        AdapterPresence::OmitIfNone,
        Some(AdapterPresenceSource::MaxOutput),
        TypedJsonConstant::Null,
    );
    let reasoning = builder.constant(
        Some(PathSegment::Key("reasoning".to_owned())),
        AdapterPresence::OmitForUnset,
        Some(AdapterPresenceSource::Reasoning),
        TypedJsonConstant::Null,
    );
    let tool_choice = builder.constant(
        Some(PathSegment::Key("toolChoice".to_owned())),
        AdapterPresence::OmitForUnset,
        Some(AdapterPresenceSource::ToolChoice),
        TypedJsonConstant::Null,
    );
    let root = builder.object(
        None,
        AdapterPresence::Required,
        None,
        vec![
            cache,
            max_output,
            messages,
            model,
            reasoning,
            system,
            tool_choice,
            tools,
        ],
    );
    let contract = AdapterContractV1 {
        version: 1,
        wire_id: AdapterWireId::AnthropicMessages,
        model_source: AdapterModelSource::CurrentModel,
        tree: ContractTree {
            root,
            nodes: builder.nodes,
            tables: vec![selection_table],
        },
        ordinary_header_rules: vec![
            OrdinaryHeaderRule::Fixed(FixedHeaderRule {
                name: "accept".to_owned(),
                value: "application/json".to_owned(),
            }),
            OrdinaryHeaderRule::OneOf(OneOfHeaderRule {
                name: "x-mode".to_owned(),
                values: vec!["fast".to_owned(), "safe".to_owned()],
                required: true,
            }),
        ],
        decoder_kind: AdapterDecoderKind::AnthropicMessages,
    };

    DummyFixture {
        contract,
        entry: catalog_entry("model"),
        original: prepare_input(),
        body: minimal_body(),
        headers: vec![OrdinaryHeader {
            name: "x-mode".to_owned(),
            value: "safe".to_owned(),
        }],
    }
}

fn minimal_body() -> WireJsonDocument {
    WireJsonDocument {
        root: 6,
        nodes: vec![
            WireJsonNode::StringValue("exact".to_owned()),
            WireJsonNode::StringValue("model".to_owned()),
            WireJsonNode::ObjectValue(WireJsonObject {
                fields: vec![
                    WireJsonField {
                        key: "kind".to_owned(),
                        value: 0,
                    },
                    WireJsonField {
                        key: "value".to_owned(),
                        value: 1,
                    },
                ],
            }),
            WireJsonNode::ArrayValue(WireJsonArray { items: vec![] }),
            WireJsonNode::ArrayValue(WireJsonArray { items: vec![] }),
            WireJsonNode::ArrayValue(WireJsonArray { items: vec![] }),
            WireJsonNode::ObjectValue(WireJsonObject {
                fields: vec![
                    WireJsonField {
                        key: "messages".to_owned(),
                        value: 3,
                    },
                    WireJsonField {
                        key: "model".to_owned(),
                        value: 2,
                    },
                    WireJsonField {
                        key: "system".to_owned(),
                        value: 4,
                    },
                    WireJsonField {
                        key: "tools".to_owned(),
                        value: 5,
                    },
                ],
            }),
        ],
    }
}

#[derive(Default)]
pub(super) struct ContractBuilder {
    pub(super) nodes: Vec<ContractNode>,
}

impl ContractBuilder {
    pub(super) fn push(
        &mut self,
        segment: Option<PathSegment>,
        presence: AdapterPresence,
        presence_source: Option<AdapterPresenceSource>,
        body: ContractNodeBody,
    ) -> u32 {
        let index = u32::try_from(self.nodes.len()).expect("dummy contract node count");
        self.nodes.push(ContractNode {
            parent: None,
            segment,
            presence,
            presence_source,
            body,
        });
        index
    }

    pub(super) fn attach(&mut self, parent: u32, children: &[u32]) {
        for child in children {
            self.nodes[*child as usize].parent = Some(parent);
        }
    }

    pub(super) fn object(
        &mut self,
        segment: Option<PathSegment>,
        presence: AdapterPresence,
        presence_source: Option<AdapterPresenceSource>,
        children: Vec<u32>,
    ) -> u32 {
        let node = self.push(
            segment,
            presence,
            presence_source,
            ContractNodeBody::Object(ContractObject {
                children: children.clone(),
            }),
        );
        self.attach(node, &children);
        node
    }

    pub(super) fn array(
        &mut self,
        segment: Option<PathSegment>,
        collection: AdapterCollection,
        item: u32,
        min: u32,
        max: u32,
    ) -> u32 {
        let node = self.push(
            segment,
            AdapterPresence::Required,
            None,
            ContractNodeBody::Array(ContractArray {
                collection,
                item,
                min,
                max,
            }),
        );
        self.attach(node, &[item]);
        node
    }

    pub(super) fn switch(
        &mut self,
        segment: Option<PathSegment>,
        presence: AdapterPresence,
        presence_source: Option<AdapterPresenceSource>,
        source: super::super::adapter::types::AdapterVariantSource,
        cases: Vec<u32>,
    ) -> u32 {
        let node = self.push(
            segment,
            presence,
            presence_source,
            ContractNodeBody::Switch(ContractSwitch {
                source,
                cases: cases
                    .iter()
                    .enumerate()
                    .map(|(ordinal, node)| ContractCase {
                        variant_ordinal: ordinal as u8,
                        node: *node,
                    })
                    .collect(),
            }),
        );
        self.attach(node, &cases);
        node
    }

    pub(super) fn value(
        &mut self,
        segment: Option<PathSegment>,
        presence: AdapterPresence,
        presence_source: Option<AdapterPresenceSource>,
        source: AdapterScalarSource,
        transform: AdapterTransform,
    ) -> u32 {
        self.push(
            segment,
            presence,
            presence_source,
            ContractNodeBody::Value(ContractValue { source, transform }),
        )
    }

    pub(super) fn constant(
        &mut self,
        segment: Option<PathSegment>,
        presence: AdapterPresence,
        presence_source: Option<AdapterPresenceSource>,
        value: TypedJsonConstant,
    ) -> u32 {
        self.push(
            segment,
            presence,
            presence_source,
            ContractNodeBody::Constant(ContractConstant { value }),
        )
    }
}
