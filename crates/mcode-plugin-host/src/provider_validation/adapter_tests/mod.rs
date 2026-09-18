//! Closed adapter contract validator tests.
mod derived_tests;
mod digest_tests;
mod exhaustive_fixture;
mod exhaustive_tests;
mod fixtures;
mod matrix_tests;
mod mistral_content_tests;
mod scope_tests;
mod semantic_remediation_tests;
mod structure_tests;
mod system_joined_tests;
mod system_message_tests;
mod test_json;
mod validation_tests;
mod wire_tests;

use super::adapter::types::{
    AdapterContractV1, AdapterDecoderKind, AdapterModelSource, AdapterPresence, AdapterWireId,
    ContractNode, ContractNodeBody, ContractObject, ContractTree,
};
use super::adapter::validate_contract;

fn empty_contract(wire_id: AdapterWireId, decoder_kind: AdapterDecoderKind) -> AdapterContractV1 {
    AdapterContractV1 {
        version: 1,
        wire_id,
        model_source: AdapterModelSource::CurrentModel,
        tree: ContractTree {
            root: 0,
            nodes: vec![ContractNode {
                parent: None,
                segment: None,
                presence: AdapterPresence::Required,
                presence_source: None,
                body: ContractNodeBody::Object(ContractObject { children: vec![] }),
            }],
            tables: vec![],
        },
        ordinary_header_rules: vec![],
        decoder_kind,
    }
}

#[test]
fn wire_decoder_pairs_are_closed_and_one_to_one() {
    let pairs = [
        (
            AdapterWireId::AnthropicMessages,
            AdapterDecoderKind::AnthropicMessages,
        ),
        (
            AdapterWireId::OpenAiCompletions,
            AdapterDecoderKind::OpenAiCompletions,
        ),
        (
            AdapterWireId::OpenAiResponses,
            AdapterDecoderKind::OpenAiResponses,
        ),
        (
            AdapterWireId::OpenAiCodexResponses,
            AdapterDecoderKind::OpenAiCodexResponses,
        ),
        (
            AdapterWireId::AzureOpenAiResponses,
            AdapterDecoderKind::AzureOpenAiResponses,
        ),
        (
            AdapterWireId::GoogleGenerativeAi,
            AdapterDecoderKind::GoogleGenerativeAi,
        ),
        (
            AdapterWireId::GoogleVertex,
            AdapterDecoderKind::GoogleVertex,
        ),
        (
            AdapterWireId::MistralConversations,
            AdapterDecoderKind::MistralConversations,
        ),
        (
            AdapterWireId::BedrockConverseStream,
            AdapterDecoderKind::BedrockConverseStream,
        ),
        (AdapterWireId::PiMessages, AdapterDecoderKind::PiMessages),
    ];

    for (index, (wire_id, decoder_kind)) in pairs.iter().copied().enumerate() {
        assert!(validate_contract(&empty_contract(wire_id, decoder_kind)).is_ok());
        let crossed_decoder = pairs[(index + 1) % pairs.len()].1;
        assert!(validate_contract(&empty_contract(wire_id, crossed_decoder)).is_err());
    }
}
