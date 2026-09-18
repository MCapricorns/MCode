//! Nonempty exhaustive trusted adapter fixture.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    AssistantBlock, AssistantMessage, CacheRetention, EnabledReasoning, ImageMediaType,
    ImageMetadata, ImageView, Message, Reasoning, ReasoningBlock, ReasoningEffort, ReasoningKind,
    ReasoningProofView, SpecificToolChoice, TextBlock, ToolCallBlock, ToolChoice, ToolDefinition,
    ToolResultBlock, ToolResultMessage, UserBlock, UserMessage,
};

use super::super::adapter::json::AdapterJson;
use super::super::adapter::types::{
    AdapterCollection, AdapterContractV1, AdapterEnumSource, AdapterModelSource, AdapterPresence,
    AdapterPresenceSource, AdapterScalarSource, AdapterTransform, AdapterVariantSource,
    AdapterWireId, ContractTree, EnumTokenEntry, EnumTokenTable, PathSegment,
};
use super::super::test_support::{catalog_entry, prepare_input};
use super::fixtures::{ContractBuilder, DummyFixture, decoder_for};
use super::test_json::wire_document;

pub(super) fn exhaustive_fixture(with_proof: bool) -> DummyFixture {
    let expected = exhaustive_expected(with_proof);
    let mut original = prepare_input();
    original.system = vec!["system one".to_owned(), "system two".to_owned()];
    original.tools = vec![ToolDefinition {
        name: "tool".to_owned(),
        description: "Use tool".to_owned(),
        input_schema: wire_document(&object(vec![("type", string("object"))])),
    }];
    original.messages = vec![
        Message::User(UserMessage {
            blocks: vec![
                UserBlock::Text(text("hello")),
                UserBlock::Image(image(ImageMediaType::Png, &[1, 2], 3, 4, 1)),
            ],
        }),
        Message::Assistant(AssistantMessage {
            blocks: vec![
                AssistantBlock::Text(text("answer")),
                AssistantBlock::Reasoning(ReasoningBlock {
                    kind: ReasoningKind::Thinking,
                    text: "thought".to_owned(),
                    proof: with_proof.then(|| ReasoningProofView {
                        stamp: "prf1-0123456789abcdef0123456789abcdef".to_owned(),
                        source_request_id: "request-0".to_owned(),
                        source_turn_id: "turn-0".to_owned(),
                        source_content_index: 0,
                        reasoning_kind: ReasoningKind::Thinking,
                        proof: vec![0xff],
                    }),
                }),
                tool_call("call-1"),
                tool_call("call-2"),
            ],
        }),
        Message::ToolResult(ToolResultMessage {
            call_id: "call-1".to_owned(),
            blocks: vec![
                ToolResultBlock::Text(text("first result")),
                ToolResultBlock::Image(image(ImageMediaType::Jpeg, &[3], 5, 6, 1)),
            ],
            is_error: false,
        }),
        Message::ToolResult(ToolResultMessage {
            call_id: "call-2".to_owned(),
            blocks: vec![ToolResultBlock::Text(text("second result"))],
            is_error: true,
        }),
    ];
    original.tool_choice = ToolChoice::Specific(SpecificToolChoice {
        name: "tool".to_owned(),
    });
    original.reasoning = Reasoning::Enabled(EnabledReasoning {
        effort: Some(ReasoningEffort::Low),
        budget_tokens: Some(64),
    });
    original.cache_retention = CacheRetention::Session;
    original.max_output_tokens = Some(128);

    DummyFixture {
        contract: exhaustive_contract(),
        entry: catalog_entry("model"),
        original,
        body: wire_document(&expected),
        headers: vec![],
    }
}

fn exhaustive_contract() -> AdapterContractV1 {
    let mut contract = ExhaustiveContract::default();
    let selection = contract.table(AdapterEnumSource::SelectionKind, &["exact", "alias"]);
    let messages = contract.table(
        AdapterEnumSource::MessageKind,
        &["user", "assistant", "tool"],
    );
    let users = contract.table(AdapterEnumSource::UserBlockKind, &["text", "image"]);
    let assistants = contract.table(
        AdapterEnumSource::AssistantBlockKind,
        &["text", "reasoning", "tool"],
    );
    let results = contract.table(AdapterEnumSource::ToolResultBlockKind, &["text", "image"]);
    let reasoning_kinds =
        contract.table(AdapterEnumSource::ReasoningKind, &["thinking", "summary"]);
    let choices = contract.table(
        AdapterEnumSource::ToolChoice,
        &["unset", "auto", "none", "specific"],
    );
    let reasoning_modes = contract.table(
        AdapterEnumSource::ReasoningMode,
        &["unset", "disabled", "enabled"],
    );
    let efforts = contract.table(
        AdapterEnumSource::ReasoningEffort,
        &["minimal", "low", "medium", "high"],
    );
    let cache = contract.table(
        AdapterEnumSource::CacheRetention,
        &["unset", "none", "request", "session"],
    );

    let model_cases = (0..2)
        .map(|_| {
            let kind = contract.enum_value("kind", AdapterScalarSource::SelectionKind, selection);
            let value = contract.value(
                "value",
                AdapterScalarSource::SelectedModel,
                AdapterTransform::Identity,
            );
            contract.object(None, vec![kind, value])
        })
        .collect();
    let model = contract.builder.switch(
        Some(key("model")),
        AdapterPresence::Required,
        None,
        AdapterVariantSource::ModelSelection,
        model_cases,
    );

    let system_item = contract.builder.value(
        Some(PathSegment::ArrayItem),
        AdapterPresence::Required,
        None,
        AdapterScalarSource::SystemItem,
        AdapterTransform::Identity,
    );
    let system = contract.builder.array(
        Some(key("system")),
        AdapterCollection::System,
        system_item,
        0,
        1_024,
    );

    let user_blocks = contract.user_blocks(users);
    let user_role = contract.enum_value("role", AdapterScalarSource::MessageRole, messages);
    let user = contract.object(None, vec![user_blocks, user_role]);

    let assistant_blocks = contract.assistant_blocks(assistants, reasoning_kinds);
    let assistant_role = contract.enum_value("role", AdapterScalarSource::MessageRole, messages);
    let assistant = contract.object(None, vec![assistant_blocks, assistant_role]);

    let result_blocks = contract.result_blocks(results);
    let result_call = contract.value(
        "callId",
        AdapterScalarSource::ToolResultCallId,
        AdapterTransform::Identity,
    );
    let result_error = contract.value(
        "isError",
        AdapterScalarSource::ToolResultIsError,
        AdapterTransform::Identity,
    );
    let result_role = contract.enum_value("role", AdapterScalarSource::MessageRole, messages);
    let result = contract.object(
        None,
        vec![result_blocks, result_call, result_error, result_role],
    );

    let message_item = contract.builder.switch(
        Some(PathSegment::ArrayItem),
        AdapterPresence::Required,
        None,
        AdapterVariantSource::Message,
        vec![user, assistant, result],
    );
    let message_array = contract.builder.array(
        Some(key("messages")),
        AdapterCollection::Messages,
        message_item,
        0,
        4_096,
    );

    let tool_description = contract.value(
        "description",
        AdapterScalarSource::ToolDescription,
        AdapterTransform::Identity,
    );
    let tool_name = contract.value(
        "name",
        AdapterScalarSource::ToolName,
        AdapterTransform::Identity,
    );
    let tool_schema = contract.value(
        "schema",
        AdapterScalarSource::ToolSchema,
        AdapterTransform::JsonSubtree,
    );
    let tool_item = contract.object(
        Some(PathSegment::ArrayItem),
        vec![tool_description, tool_name, tool_schema],
    );
    let tools = contract.builder.array(
        Some(key("tools")),
        AdapterCollection::Tools,
        tool_item,
        0,
        1_024,
    );

    let cache_cases = (0..4)
        .map(|_| {
            contract.builder.value(
                None,
                AdapterPresence::Required,
                None,
                AdapterScalarSource::CacheRetention,
                AdapterTransform::EnumToken(cache),
            )
        })
        .collect();
    let cache_node = contract.builder.switch(
        Some(key("cache")),
        AdapterPresence::OmitForUnset,
        Some(AdapterPresenceSource::CacheRetention),
        AdapterVariantSource::CacheRetention,
        cache_cases,
    );

    let max_output = contract.builder.value(
        Some(key("maxOutput")),
        AdapterPresence::OmitIfNone,
        Some(AdapterPresenceSource::MaxOutput),
        AdapterScalarSource::MaxOutput,
        AdapterTransform::CheckedU64,
    );

    let reasoning_cases = (0..2)
        .map(|_| {
            contract.builder.value(
                None,
                AdapterPresence::Required,
                None,
                AdapterScalarSource::ReasoningMode,
                AdapterTransform::EnumToken(reasoning_modes),
            )
        })
        .collect::<Vec<_>>();
    let budget = contract.builder.value(
        Some(key("budget")),
        AdapterPresence::OmitIfNone,
        Some(AdapterPresenceSource::ReasoningBudget),
        AdapterScalarSource::ReasoningBudget,
        AdapterTransform::CheckedU64,
    );
    let effort = contract.builder.value(
        Some(key("effort")),
        AdapterPresence::OmitIfNone,
        Some(AdapterPresenceSource::ReasoningEffort),
        AdapterScalarSource::ReasoningEffort,
        AdapterTransform::EnumToken(efforts),
    );
    let mode = contract.enum_value("mode", AdapterScalarSource::ReasoningMode, reasoning_modes);
    let enabled = contract.object(None, vec![budget, effort, mode]);
    let reasoning = contract.builder.switch(
        Some(key("reasoning")),
        AdapterPresence::OmitForUnset,
        Some(AdapterPresenceSource::Reasoning),
        AdapterVariantSource::Reasoning,
        vec![reasoning_cases[0], reasoning_cases[1], enabled],
    );

    let mut choice_cases = (0..3)
        .map(|_| {
            contract.builder.value(
                None,
                AdapterPresence::Required,
                None,
                AdapterScalarSource::ToolChoiceKind,
                AdapterTransform::EnumToken(choices),
            )
        })
        .collect::<Vec<_>>();
    let choice_kind = contract.enum_value("kind", AdapterScalarSource::ToolChoiceKind, choices);
    let choice_name = contract.value(
        "name",
        AdapterScalarSource::ToolChoiceName,
        AdapterTransform::Identity,
    );
    choice_cases.push(contract.object(None, vec![choice_kind, choice_name]));
    let tool_choice = contract.builder.switch(
        Some(key("toolChoice")),
        AdapterPresence::OmitForUnset,
        Some(AdapterPresenceSource::ToolChoice),
        AdapterVariantSource::ToolChoice,
        choice_cases,
    );

    let root = contract.object(
        None,
        vec![
            cache_node,
            max_output,
            message_array,
            model,
            reasoning,
            system,
            tool_choice,
            tools,
        ],
    );
    AdapterContractV1 {
        version: 1,
        wire_id: AdapterWireId::AnthropicMessages,
        model_source: AdapterModelSource::CurrentModel,
        tree: ContractTree {
            root,
            nodes: contract.builder.nodes,
            tables: contract.tables,
        },
        ordinary_header_rules: vec![],
        decoder_kind: decoder_for(AdapterWireId::AnthropicMessages),
    }
}

#[derive(Default)]
struct ExhaustiveContract {
    builder: ContractBuilder,
    tables: Vec<EnumTokenTable>,
}

impl ExhaustiveContract {
    fn table(&mut self, source: AdapterEnumSource, tokens: &[&str]) -> u16 {
        let index = u16::try_from(self.tables.len()).expect("test table count");
        self.tables.push(EnumTokenTable {
            source,
            entries: tokens
                .iter()
                .enumerate()
                .map(|(ordinal, token)| EnumTokenEntry {
                    variant_ordinal: ordinal as u8,
                    token: (*token).to_owned(),
                })
                .collect(),
        });
        index
    }

    fn value(
        &mut self,
        name: &str,
        source: AdapterScalarSource,
        transform: AdapterTransform,
    ) -> u32 {
        self.builder.value(
            Some(key(name)),
            AdapterPresence::Required,
            None,
            source,
            transform,
        )
    }

    fn enum_value(&mut self, name: &str, source: AdapterScalarSource, table: u16) -> u32 {
        self.value(name, source, AdapterTransform::EnumToken(table))
    }

    fn object(&mut self, segment: Option<PathSegment>, children: Vec<u32>) -> u32 {
        self.builder
            .object(segment, AdapterPresence::Required, None, children)
    }

    fn user_blocks(&mut self, kinds: u16) -> u32 {
        let text_kind = self.enum_value("kind", AdapterScalarSource::BlockKind, kinds);
        let text_value = self.value(
            "text",
            AdapterScalarSource::BlockText,
            AdapterTransform::Identity,
        );
        let text = self.object(None, vec![text_kind, text_value]);
        let data = self.value(
            "data",
            AdapterScalarSource::ImageDataUri,
            AdapterTransform::DataUri,
        );
        let frames = self.value(
            "frames",
            AdapterScalarSource::ImageFrames,
            AdapterTransform::CheckedU32,
        );
        let height = self.value(
            "height",
            AdapterScalarSource::ImageHeight,
            AdapterTransform::CheckedU32,
        );
        let image_kind = self.enum_value("kind", AdapterScalarSource::BlockKind, kinds);
        let width = self.value(
            "width",
            AdapterScalarSource::ImageWidth,
            AdapterTransform::CheckedU32,
        );
        let image = self.object(None, vec![data, frames, height, image_kind, width]);
        let item = self.builder.switch(
            Some(PathSegment::ArrayItem),
            AdapterPresence::Required,
            None,
            AdapterVariantSource::UserBlock,
            vec![text, image],
        );
        self.builder.array(
            Some(key("blocks")),
            AdapterCollection::Blocks,
            item,
            1,
            4_096,
        )
    }

    fn assistant_blocks(&mut self, kinds: u16, reasoning_kinds: u16) -> u32 {
        let text_kind = self.enum_value("kind", AdapterScalarSource::BlockKind, kinds);
        let text_value = self.value(
            "text",
            AdapterScalarSource::BlockText,
            AdapterTransform::Identity,
        );
        let text = self.object(None, vec![text_kind, text_value]);

        let reasoning_kind = self.enum_value("kind", AdapterScalarSource::BlockKind, kinds);
        let proof = self.builder.value(
            Some(key("proof")),
            AdapterPresence::OmitIfNone,
            Some(AdapterPresenceSource::ReasoningProof),
            AdapterScalarSource::Proof,
            AdapterTransform::Base64StandardPadded,
        );
        let source_kind = self.enum_value(
            "reasoningKind",
            AdapterScalarSource::ReasoningKind,
            reasoning_kinds,
        );
        let reasoning_text = self.value(
            "text",
            AdapterScalarSource::BlockText,
            AdapterTransform::Identity,
        );
        let reasoning = self.object(
            None,
            vec![reasoning_kind, proof, source_kind, reasoning_text],
        );

        let arguments = self.value(
            "arguments",
            AdapterScalarSource::ToolCallArguments,
            AdapterTransform::JsonSubtree,
        );
        let call_id = self.value(
            "callId",
            AdapterScalarSource::ToolCallId,
            AdapterTransform::Identity,
        );
        let call_kind = self.enum_value("kind", AdapterScalarSource::BlockKind, kinds);
        let call_name = self.value(
            "name",
            AdapterScalarSource::ToolCallName,
            AdapterTransform::Identity,
        );
        let call = self.object(None, vec![arguments, call_id, call_kind, call_name]);

        let item = self.builder.switch(
            Some(PathSegment::ArrayItem),
            AdapterPresence::Required,
            None,
            AdapterVariantSource::AssistantBlock,
            vec![text, reasoning, call],
        );
        self.builder.array(
            Some(key("blocks")),
            AdapterCollection::Blocks,
            item,
            1,
            4_096,
        )
    }

    fn result_blocks(&mut self, kinds: u16) -> u32 {
        let text_kind = self.enum_value("kind", AdapterScalarSource::BlockKind, kinds);
        let text_value = self.value(
            "text",
            AdapterScalarSource::BlockText,
            AdapterTransform::Identity,
        );
        let text = self.object(None, vec![text_kind, text_value]);
        let data = self.value(
            "data",
            AdapterScalarSource::ImageDataUri,
            AdapterTransform::DataUri,
        );
        let image_kind = self.enum_value("kind", AdapterScalarSource::BlockKind, kinds);
        let image = self.object(None, vec![data, image_kind]);
        let item = self.builder.switch(
            Some(PathSegment::ArrayItem),
            AdapterPresence::Required,
            None,
            AdapterVariantSource::ToolResultBlock,
            vec![text, image],
        );
        self.builder.array(
            Some(key("blocks")),
            AdapterCollection::Blocks,
            item,
            1,
            4_096,
        )
    }
}

fn exhaustive_expected(with_proof: bool) -> AdapterJson {
    let mut reasoning = vec![("kind", string("reasoning"))];
    if with_proof {
        reasoning.push(("proof", string("/w==")));
    }
    reasoning.extend([
        ("reasoningKind", string("thinking")),
        ("text", string("thought")),
    ]);
    object(vec![
        ("cache", string("session")),
        ("maxOutput", number(128)),
        (
            "messages",
            array(vec![
                object(vec![
                    (
                        "blocks",
                        array(vec![
                            object(vec![("kind", string("text")), ("text", string("hello"))]),
                            object(vec![
                                ("data", string("data:image/png;base64,AQI=")),
                                ("frames", number(1)),
                                ("height", number(4)),
                                ("kind", string("image")),
                                ("width", number(3)),
                            ]),
                        ]),
                    ),
                    ("role", string("user")),
                ]),
                object(vec![
                    (
                        "blocks",
                        array(vec![
                            object(vec![("kind", string("text")), ("text", string("answer"))]),
                            object(reasoning),
                            expected_call("call-1"),
                            expected_call("call-2"),
                        ]),
                    ),
                    ("role", string("assistant")),
                ]),
                object(vec![
                    (
                        "blocks",
                        array(vec![
                            object(vec![
                                ("kind", string("text")),
                                ("text", string("first result")),
                            ]),
                            object(vec![
                                ("data", string("data:image/jpeg;base64,Aw==")),
                                ("kind", string("image")),
                            ]),
                        ]),
                    ),
                    ("callId", string("call-1")),
                    ("isError", AdapterJson::Boolean(false)),
                    ("role", string("tool")),
                ]),
                object(vec![
                    (
                        "blocks",
                        array(vec![object(vec![
                            ("kind", string("text")),
                            ("text", string("second result")),
                        ])]),
                    ),
                    ("callId", string("call-2")),
                    ("isError", AdapterJson::Boolean(true)),
                    ("role", string("tool")),
                ]),
            ]),
        ),
        (
            "model",
            object(vec![("kind", string("exact")), ("value", string("model"))]),
        ),
        (
            "reasoning",
            object(vec![
                ("budget", number(64)),
                ("effort", string("low")),
                ("mode", string("enabled")),
            ]),
        ),
        (
            "system",
            array(vec![string("system one"), string("system two")]),
        ),
        (
            "toolChoice",
            object(vec![("kind", string("specific")), ("name", string("tool"))]),
        ),
        (
            "tools",
            array(vec![object(vec![
                ("description", string("Use tool")),
                ("name", string("tool")),
                ("schema", object(vec![("type", string("object"))])),
            ])]),
        ),
    ])
}

fn expected_call(call_id: &str) -> AdapterJson {
    object(vec![
        (
            "arguments",
            object(vec![("count", number(2)), ("label", string("x"))]),
        ),
        ("callId", string(call_id)),
        ("kind", string("tool")),
        ("name", string("tool")),
    ])
}

fn tool_call(call_id: &str) -> AssistantBlock {
    AssistantBlock::ToolCall(ToolCallBlock {
        call_id: call_id.to_owned(),
        name: "tool".to_owned(),
        arguments: wire_document(&object(vec![("count", number(2)), ("label", string("x"))])),
    })
}

fn image(
    media_type: ImageMediaType,
    bytes: &[u8],
    width: u32,
    height: u32,
    frames: u32,
) -> ImageView {
    ImageView {
        stamp: "img1-0123456789abcdef0123456789abcdef".to_owned(),
        media_type,
        bytes: bytes.to_vec(),
        metadata: ImageMetadata {
            width,
            height,
            frames,
        },
    }
}

fn text(value: &str) -> TextBlock {
    TextBlock {
        text: value.to_owned(),
    }
}

fn key(value: &str) -> PathSegment {
    PathSegment::Key(value.to_owned())
}

fn string(value: &str) -> AdapterJson {
    AdapterJson::ordinary_string(value)
}

fn number(value: u64) -> AdapterJson {
    AdapterJson::Number(value.to_string())
}

fn array(items: Vec<AdapterJson>) -> AdapterJson {
    AdapterJson::Array(items)
}

fn object(fields: Vec<(&str, AdapterJson)>) -> AdapterJson {
    AdapterJson::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}
