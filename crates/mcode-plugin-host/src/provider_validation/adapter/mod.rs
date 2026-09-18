//! Closed `AdapterContractV1` validation and interpretation.
pub(super) mod digest;
pub(super) mod evaluate;
mod header;
pub(super) mod json;
mod policy;
mod semantics;
pub(super) mod source;
mod structure;
pub(super) mod types;

#[cfg(test)]
pub(in crate::provider_validation) use structure::validate_transform;

use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    OrdinaryHeader, PrepareInput, WireJsonDocument,
};

pub(in crate::provider_validation) use structure::validate_contract;
use types::{
    AdapterContractV1, AdapterValidationError, ValidatedAdapter, ValidatedCatalogEntryView,
};

pub(in crate::provider_validation) fn validate_adapter(
    contract: &AdapterContractV1,
    selected: ValidatedCatalogEntryView<'_>,
    original: &PrepareInput,
    prepared: &WireJsonDocument,
    headers: &[OrdinaryHeader],
) -> Result<ValidatedAdapter, AdapterValidationError> {
    validate_contract(contract)?;
    let expected = evaluate::evaluate_contract(contract, selected, original)?;
    let body = json::compare_and_serialize(&expected, prepared)?;
    header::validate_headers(&contract.ordinary_header_rules, headers)?;
    Ok(ValidatedAdapter {
        wire_id: contract.wire_id,
        decoder_kind: contract.decoder_kind,
        contract_digest: digest::contract_digest(contract)?,
        body_digest: digest::body_digest(&body)?,
        ordinary_header_digest: digest::ordinary_header_digest(headers)?,
    })
}
