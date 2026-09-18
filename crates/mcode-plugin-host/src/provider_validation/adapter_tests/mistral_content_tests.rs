//! Literal Mistral tool-result content chunk fixtures.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    AssistantBlock, AssistantMessage, ImageMediaType, ImageMetadata, ImageView, Message, TextBlock,
    ToolCallBlock, ToolDefinition, ToolResultBlock, ToolResultMessage,
};

use super::super::adapter::json::AdapterJson;
use super::super::adapter::types::AdapterWireId;
use super::super::adapter::validate_adapter;
use super::fixtures::DummyFixture;
use super::test_json::wire_document;
use super::wire_tests::wire_contract;

#[test]
fn mistral_nonempty_text_chunks_distinguish_success_and_error() {
    for (is_error, text) in [(false, "result"), (true, "[tool error] result")] {
        assert_mistral(
            vec![text_block(" result ")],
            is_error,
            vec![text_chunk(text)],
        );
    }
}

#[test]
fn mistral_empty_text_and_pure_image_use_exact_placeholders() {
    assert_mistral(
        vec![text_block(" \t\n")],
        false,
        vec![text_chunk("(no tool output)")],
    );
    assert_mistral(
        vec![image_block(ImageMediaType::Png, 1)],
        false,
        vec![
            text_chunk("(see attached image)"),
            image_chunk("data:image/png;base64,AQ=="),
        ],
    );
}

#[test]
fn mistral_mixed_content_keeps_text_and_image_declaration_order() {
    assert_mistral(
        vec![
            text_block(" first "),
            image_block(ImageMediaType::Png, 1),
            text_block("second "),
            image_block(ImageMediaType::Jpeg, 2),
        ],
        false,
        vec![
            text_chunk("first \nsecond"),
            image_chunk("data:image/png;base64,AQ=="),
            image_chunk("data:image/jpeg;base64,Ag=="),
        ],
    );
}

fn assert_mistral(blocks: Vec<ToolResultBlock>, is_error: bool, content: Vec<AdapterJson>) {
    let fixture = mistral_fixture(blocks, is_error, content);
    assert!(
        validate_adapter(
            &fixture.contract,
            fixture.selected(),
            &fixture.original,
            &fixture.body,
            &fixture.headers,
        )
        .is_ok()
    );
}

fn mistral_fixture(
    blocks: Vec<ToolResultBlock>,
    is_error: bool,
    content: Vec<AdapterJson>,
) -> DummyFixture {
    let mut original = super::super::test_support::prepare_input();
    original.tools = vec![ToolDefinition {
        name: "tool".to_owned(),
        description: "Use tool".to_owned(),
        input_schema: super::super::test_support::empty_object(),
    }];
    original.messages = vec![
        Message::Assistant(AssistantMessage {
            blocks: vec![AssistantBlock::ToolCall(ToolCallBlock {
                call_id: "call-1".to_owned(),
                name: "tool".to_owned(),
                arguments: super::super::test_support::empty_object(),
            })],
        }),
        Message::ToolResult(ToolResultMessage {
            call_id: "call-1".to_owned(),
            blocks,
            is_error,
        }),
    ];
    let expected = object(vec![
        (
            "messages",
            AdapterJson::Array(vec![
                object(vec![(
                    "blocks",
                    AdapterJson::Array(vec![object(vec![
                        ("arguments", object(vec![])),
                        ("callId", string("call-1")),
                        ("name", string("tool")),
                    ])]),
                )]),
                object(vec![
                    ("callId", string("call-1")),
                    ("content", AdapterJson::Array(content)),
                    ("name", string("tool")),
                ]),
            ]),
        ),
        ("model", string("model")),
        ("system", AdapterJson::Array(vec![])),
        (
            "tools",
            AdapterJson::Array(vec![object(vec![
                ("description", string("Use tool")),
                ("name", string("tool")),
                ("schema", object(vec![])),
            ])]),
        ),
    ]);
    DummyFixture {
        contract: wire_contract(AdapterWireId::MistralConversations),
        entry: super::super::test_support::catalog_entry("model"),
        original,
        body: wire_document(&expected),
        headers: vec![],
    }
}

fn text_block(value: &str) -> ToolResultBlock {
    ToolResultBlock::Text(TextBlock {
        text: value.to_owned(),
    })
}

fn image_block(media_type: ImageMediaType, byte: u8) -> ToolResultBlock {
    ToolResultBlock::Image(ImageView {
        stamp: "img1-0123456789abcdef0123456789abcdef".to_owned(),
        media_type,
        bytes: vec![byte],
        metadata: ImageMetadata {
            width: 1,
            height: 1,
            frames: 1,
        },
    })
}

fn text_chunk(value: &str) -> AdapterJson {
    object(vec![("text", string(value)), ("type", string("text"))])
}

fn image_chunk(value: &str) -> AdapterJson {
    object(vec![
        ("image_url", string(value)),
        ("type", string("image_url")),
    ])
}

fn string(value: &str) -> AdapterJson {
    AdapterJson::ordinary_string(value)
}

fn object(fields: Vec<(&str, AdapterJson)>) -> AdapterJson {
    AdapterJson::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}
