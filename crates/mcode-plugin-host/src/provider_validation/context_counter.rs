//! Closed T7 dummy context-counter registry and pure measurement contract.
//!
//! This module contains no Store, guest callback, credential, network, or
//! mutable global state. Its sole compiled entry is explicitly a dummy fixture.
use std::sync::LazyLock;

use sha2::{Digest, Sha256};

use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    CatalogEntry, ModelSelection, PrepareInput,
};

use super::prepare::{SelectedCatalogView, validate_prepare_input};
use super::{ValidationError, ValidationResult};

const COUNTER_DOMAIN: &[u8] = b"mcode-provider-context-counter-ref-v1\0";
const ALGORITHM_DOMAIN: &[u8] = b"mcode-provider-context-algorithm-v1\0";
const VOCABULARY_DOMAIN: &[u8] = b"mcode-provider-context-vocabulary-v1\0";
const WIRE_FRAMING_DOMAIN: &[u8] = b"mcode-provider-context-wire-framing-v1\0";
const OUTPUT_RESERVATION_DOMAIN: &[u8] = b"mcode-provider-context-output-reservation-v1\0";
const BODY_DOMAIN: &[u8] = b"mcode-provider-wire-body-v1\0";

const ALGORITHM_MAGIC: &[u8] = b"mcode-dummy-algorithm-v1\0";
const VOCABULARY_MAGIC: &[u8] = b"mcode-dummy-vocabulary-v1\0";
const WIRE_FRAMING_MAGIC: &[u8] = b"mcode-dummy-wire-framing-v1\0";
const OUTPUT_RESERVATION_MAGIC: &[u8] = b"mcode-dummy-output-reservation-v1\0";

const DUMMY_ALGORITHM_BYTES: &[u8] =
    b"mcode-dummy-algorithm-v1\0\x00\x00\x00\x00\x00\x00\x00\x02\x00\x00\x00\x00\x00\x00\x00\x01";
const DUMMY_VOCABULARY_BYTES: &[u8] =
    b"mcode-dummy-vocabulary-v1\0\x00\x00\x00\x00\x00\x00\x00\x04";
const DUMMY_WIRE_FRAMING_BYTES: &[u8] =
    b"mcode-dummy-wire-framing-v1\0\x00\x00\x00\x00\x00\x00\x00\x03";
const DUMMY_OUTPUT_RESERVATION_BYTES: &[u8] =
    b"mcode-dummy-output-reservation-v1\0\x00\x00\x00\x00\x00\x00\x00\x0e";

const DUMMY_ALGORITHM_DIGEST: &str =
    "sha256:bcc7a3c85a0e50b4bdb889040974603c1fdfefe1ad465768517773b3bc22ff83";
const DUMMY_VOCABULARY_DIGEST: &str =
    "sha256:bf90d47c54bbdb50a402f44abc0dbd71a7dc2cdc3a032391c1d8985f0ca9be21";
const DUMMY_WIRE_FRAMING_DIGEST: &str =
    "sha256:32a504d4d1dafe4eb3e5bbeb7364269ec913f7ba68d482a3d0e3406a16794092";
const DUMMY_OUTPUT_RESERVATION_DIGEST: &str =
    "sha256:f7f2f70c3dc81c3064f66dfc218df302234d981a960ac141730ab368ce3c01a2";
const DUMMY_COUNTER_DIGEST: &str =
    "sha256:da58bfe417ee5a4759d3a287b1af86fba7da1e6522e87c10b64a1e90232062b6";

const PROFILE_FIELDS: [&str; 12] = [
    "registry-id",
    "registry-version",
    "algorithm-id",
    "algorithm-version",
    "algorithm-digest",
    "vocabulary-digest",
    "wire-framing-id",
    "wire-framing-version",
    "wire-framing-digest",
    "output-reservation-id",
    "output-reservation-version",
    "output-reservation-digest",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ContextCounterRefV1 {
    pub(super) registry_id: String,
    pub(super) registry_version: u16,
    pub(super) algorithm_id: String,
    pub(super) algorithm_version: u16,
    pub(super) algorithm_digest: String,
    pub(super) vocabulary_digest: String,
    pub(super) wire_framing_id: String,
    pub(super) wire_framing_version: u16,
    pub(super) wire_framing_digest: String,
    pub(super) output_reservation_id: String,
    pub(super) output_reservation_version: u16,
    pub(super) output_reservation_digest: String,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ProfileValue<'a> {
    String(&'a str),
    Unsigned(u64),
    Boolean(bool),
    Null,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ProfileField<'a> {
    pub(super) name: &'a str,
    pub(super) value: ProfileValue<'a>,
}

pub(super) fn parse_context_counter_ref(
    fields: &[ProfileField<'_>],
) -> ValidationResult<ContextCounterRefV1> {
    if fields.len() != PROFILE_FIELDS.len()
        || fields
            .iter()
            .zip(PROFILE_FIELDS)
            .any(|(field, expected)| field.name != expected)
    {
        return Err(ValidationError::InvalidArgument);
    }
    let parsed = ContextCounterRefV1 {
        registry_id: profile_string(fields[0].value)?.to_owned(),
        registry_version: profile_version(fields[1].value)?,
        algorithm_id: profile_string(fields[2].value)?.to_owned(),
        algorithm_version: profile_version(fields[3].value)?,
        algorithm_digest: profile_string(fields[4].value)?.to_owned(),
        vocabulary_digest: profile_string(fields[5].value)?.to_owned(),
        wire_framing_id: profile_string(fields[6].value)?.to_owned(),
        wire_framing_version: profile_version(fields[7].value)?,
        wire_framing_digest: profile_string(fields[8].value)?.to_owned(),
        output_reservation_id: profile_string(fields[9].value)?.to_owned(),
        output_reservation_version: profile_version(fields[10].value)?,
        output_reservation_digest: profile_string(fields[11].value)?.to_owned(),
    };
    validate_counter_ref(&parsed)?;
    Ok(parsed)
}

pub(super) fn counter_digest(reference: &ContextCounterRefV1) -> ValidationResult<String> {
    validate_counter_ref(reference)?;
    let mut hash = Sha256::new();
    hash.update(COUNTER_DOMAIN);
    hash_string(&mut hash, &reference.registry_id)?;
    hash.update(reference.registry_version.to_be_bytes());
    hash_string(&mut hash, &reference.algorithm_id)?;
    hash.update(reference.algorithm_version.to_be_bytes());
    hash_string(&mut hash, &reference.algorithm_digest)?;
    hash_string(&mut hash, &reference.vocabulary_digest)?;
    hash_string(&mut hash, &reference.wire_framing_id)?;
    hash.update(reference.wire_framing_version.to_be_bytes());
    hash_string(&mut hash, &reference.wire_framing_digest)?;
    hash_string(&mut hash, &reference.output_reservation_id)?;
    hash.update(reference.output_reservation_version.to_be_bytes());
    hash_string(&mut hash, &reference.output_reservation_digest)?;
    Ok(digest_text(&hash.finalize()))
}

pub(super) fn body_digest(body: &[u8]) -> ValidationResult<String> {
    let length = u64::try_from(body.len()).map_err(|_| ValidationError::Limit)?;
    let mut hash = Sha256::new();
    hash.update(BODY_DOMAIN);
    hash.update(length.to_be_bytes());
    hash.update(body);
    Ok(digest_text(&hash.finalize()))
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ComponentBytes<'a> {
    pub(super) algorithm: &'a [u8],
    pub(super) vocabulary: &'a [u8],
    pub(super) wire_framing: &'a [u8],
    pub(super) output_reservation: &'a [u8],
}

#[derive(Debug, Clone)]
pub(super) struct RegistryEntrySpec<'a> {
    pub(super) reference: ContextCounterRefV1,
    pub(super) components: ComponentBytes<'a>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CompiledCounter {
    pub(super) body_byte_weight: u64,
    pub(super) prepare_charge_weight: u64,
    pub(super) bytes_per_token: u64,
    pub(super) framing_tokens: u64,
    pub(super) default_output_tokens: u64,
}

#[derive(Debug, Clone)]
struct ValidatedRegistryEntry<'a> {
    reference: ContextCounterRefV1,
    counter_digest: String,
    components: ComponentBytes<'a>,
    compiled: CompiledCounter,
}

#[derive(Debug, Clone)]
pub(super) struct CounterRegistry<'a> {
    entries: Vec<ValidatedRegistryEntry<'a>>,
}

impl CounterRegistry<'_> {
    fn lookup(
        &self,
        reference: &ContextCounterRefV1,
    ) -> ValidationResult<&ValidatedRegistryEntry<'_>> {
        let mut matches = self
            .entries
            .iter()
            .filter(|entry| entry.reference == *reference);
        let entry = matches.next().ok_or(ValidationError::InvalidArgument)?;
        if matches.next().is_some() {
            return Err(ValidationError::InvalidArgument);
        }
        Ok(entry)
    }
}

pub(super) fn trusted_dummy_counter_ref() -> ContextCounterRefV1 {
    ContextCounterRefV1 {
        registry_id: "t7-dummy-context".to_owned(),
        registry_version: 1,
        algorithm_id: "dummy-byte-charge".to_owned(),
        algorithm_version: 1,
        algorithm_digest: DUMMY_ALGORITHM_DIGEST.to_owned(),
        vocabulary_digest: DUMMY_VOCABULARY_DIGEST.to_owned(),
        wire_framing_id: "dummy-fixed-framing".to_owned(),
        wire_framing_version: 1,
        wire_framing_digest: DUMMY_WIRE_FRAMING_DIGEST.to_owned(),
        output_reservation_id: "dummy-output-reservation".to_owned(),
        output_reservation_version: 1,
        output_reservation_digest: DUMMY_OUTPUT_RESERVATION_DIGEST.to_owned(),
    }
}

pub(super) const fn trusted_dummy_counter_digest() -> &'static str {
    DUMMY_COUNTER_DIGEST
}

static TRUSTED_DUMMY_REGISTRY: LazyLock<ValidationResult<CounterRegistry<'static>>> =
    LazyLock::new(build_trusted_registry);

pub(super) fn trusted_registry() -> ValidationResult<&'static CounterRegistry<'static>> {
    TRUSTED_DUMMY_REGISTRY.as_ref().map_err(|error| *error)
}

fn build_trusted_registry() -> ValidationResult<CounterRegistry<'static>> {
    let spec = RegistryEntrySpec {
        reference: trusted_dummy_counter_ref(),
        components: ComponentBytes {
            algorithm: DUMMY_ALGORITHM_BYTES,
            vocabulary: DUMMY_VOCABULARY_BYTES,
            wire_framing: DUMMY_WIRE_FRAMING_BYTES,
            output_reservation: DUMMY_OUTPUT_RESERVATION_BYTES,
        },
    };
    let registry = construct_registry(std::slice::from_ref(&spec))?;
    if registry.entries.len() != 1 || registry.entries[0].counter_digest != DUMMY_COUNTER_DIGEST {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(registry)
}

#[cfg(test)]
pub(super) fn registry_for_test<'a>(
    specs: &[RegistryEntrySpec<'a>],
) -> ValidationResult<CounterRegistry<'a>> {
    construct_registry(specs)
}

fn construct_registry<'a>(
    specs: &[RegistryEntrySpec<'a>],
) -> ValidationResult<CounterRegistry<'a>> {
    if specs.is_empty() {
        return Err(ValidationError::InvalidArgument);
    }
    let mut entries = Vec::with_capacity(specs.len());
    for spec in specs {
        if entries
            .iter()
            .any(|entry: &ValidatedRegistryEntry<'_>| entry.reference == spec.reference)
        {
            return Err(ValidationError::InvalidArgument);
        }
        entries.push(validate_registry_entry(spec)?);
    }
    Ok(CounterRegistry { entries })
}

fn validate_registry_entry<'a>(
    spec: &RegistryEntrySpec<'a>,
) -> ValidationResult<ValidatedRegistryEntry<'a>> {
    validate_counter_ref(&spec.reference)?;
    let algorithm = compile_component(spec.components.algorithm, ALGORITHM_MAGIC, 2)?;
    let vocabulary = compile_component(spec.components.vocabulary, VOCABULARY_MAGIC, 1)?;
    let wire_framing = compile_component(spec.components.wire_framing, WIRE_FRAMING_MAGIC, 1)?;
    let output_reservation = compile_component(
        spec.components.output_reservation,
        OUTPUT_RESERVATION_MAGIC,
        1,
    )?;
    if algorithm.contains(&0)
        || vocabulary[0] == 0
        || component_digest(
            ALGORITHM_DOMAIN,
            spec.reference.registry_version,
            spec.reference.algorithm_version,
            spec.components.algorithm,
        )? != spec.reference.algorithm_digest
        || component_digest(
            VOCABULARY_DOMAIN,
            spec.reference.registry_version,
            spec.reference.algorithm_version,
            spec.components.vocabulary,
        )? != spec.reference.vocabulary_digest
        || component_digest(
            WIRE_FRAMING_DOMAIN,
            spec.reference.registry_version,
            spec.reference.wire_framing_version,
            spec.components.wire_framing,
        )? != spec.reference.wire_framing_digest
        || component_digest(
            OUTPUT_RESERVATION_DOMAIN,
            spec.reference.registry_version,
            spec.reference.output_reservation_version,
            spec.components.output_reservation,
        )? != spec.reference.output_reservation_digest
    {
        return Err(ValidationError::InvalidArgument);
    }
    let compiled = CompiledCounter {
        body_byte_weight: algorithm[0],
        prepare_charge_weight: algorithm[1],
        bytes_per_token: vocabulary[0],
        framing_tokens: wire_framing[0],
        default_output_tokens: output_reservation[0],
    };
    Ok(ValidatedRegistryEntry {
        reference: spec.reference.clone(),
        counter_digest: counter_digest(&spec.reference)?,
        components: spec.components,
        compiled,
    })
}

fn compile_component(bytes: &[u8], magic: &[u8], value_count: usize) -> ValidationResult<Vec<u64>> {
    let values_bytes = value_count
        .checked_mul(std::mem::size_of::<u64>())
        .ok_or(ValidationError::Limit)?;
    let expected_length = magic
        .len()
        .checked_add(values_bytes)
        .ok_or(ValidationError::Limit)?;
    if bytes.len() != expected_length || !bytes.starts_with(magic) {
        return Err(ValidationError::InvalidArgument);
    }
    let (encoded_values, remainder) = bytes[magic.len()..].as_chunks::<8>();
    if !remainder.is_empty() {
        return Err(ValidationError::InvalidArgument);
    }
    let values = encoded_values
        .iter()
        .map(|encoded| u64::from_be_bytes(*encoded))
        .collect::<Vec<_>>();
    let mut canonical = Vec::with_capacity(expected_length);
    canonical.extend_from_slice(magic);
    for value in &values {
        canonical.extend_from_slice(&value.to_be_bytes());
    }
    if canonical != bytes {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(values)
}

fn component_digest(
    domain: &[u8],
    registry_version: u16,
    component_version: u16,
    bytes: &[u8],
) -> ValidationResult<String> {
    let length = u64::try_from(bytes.len()).map_err(|_| ValidationError::Limit)?;
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(registry_version.to_be_bytes());
    hash.update(component_version.to_be_bytes());
    hash.update(length.to_be_bytes());
    hash.update(bytes);
    Ok(digest_text(&hash.finalize()))
}

#[derive(Debug, Clone, Copy)]
pub(super) struct MeasurementFacts {
    pub(super) body_bytes: u64,
    pub(super) prepare_logical_charge: u64,
    pub(super) requested_output_tokens: Option<u64>,
}

pub(super) fn measure_facts(
    compiled: CompiledCounter,
    facts: MeasurementFacts,
) -> ValidationResult<u64> {
    let body_charge = facts
        .body_bytes
        .checked_mul(compiled.body_byte_weight)
        .ok_or(ValidationError::Limit)?;
    let prepare_charge = facts
        .prepare_logical_charge
        .checked_mul(compiled.prepare_charge_weight)
        .ok_or(ValidationError::Limit)?;
    let algorithm_charge = body_charge
        .checked_add(prepare_charge)
        .ok_or(ValidationError::Limit)?;
    let rounding = compiled
        .bytes_per_token
        .checked_sub(1)
        .ok_or(ValidationError::InvalidArgument)?;
    let rounded = algorithm_charge
        .checked_add(rounding)
        .ok_or(ValidationError::Limit)?;
    let vocabulary_tokens = rounded / compiled.bytes_per_token;
    let framed = vocabulary_tokens
        .checked_add(compiled.framing_tokens)
        .ok_or(ValidationError::Limit)?;
    framed
        .checked_add(
            facts
                .requested_output_tokens
                .unwrap_or(compiled.default_output_tokens),
        )
        .ok_or(ValidationError::Limit)
}

pub(super) fn confirm_deterministic<F>(mut measure: F) -> ValidationResult<u64>
where
    F: FnMut() -> ValidationResult<u64>,
{
    let first = measure()?;
    let second = measure()?;
    if first != second {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(first)
}

#[derive(Debug, Clone)]
pub(super) struct SealedContextMeasurement {
    pub(super) provider_id: String,
    pub(super) route_id: String,
    pub(super) catalog_digest: String,
    pub(super) selection: ModelSelection,
    pub(super) current_model: String,
    pub(super) completion_operation: String,
    pub(super) selected_entry: CatalogEntry,
    pub(super) original: PrepareInput,
    pub(super) body_digest: String,
    pub(super) canonical_body: Vec<u8>,
    pub(super) counter_ref: ContextCounterRefV1,
    pub(super) counter_digest: String,
    pub(super) required_context_tokens: u64,
}

pub(super) fn measure_context(
    selected: SelectedCatalogView<'_>,
    original: &PrepareInput,
    canonical_body: &[u8],
    expected_body_digest: &str,
    reference: &ContextCounterRefV1,
    expected_counter_digest: &str,
) -> ValidationResult<SealedContextMeasurement> {
    let registry = trusted_registry()?;
    measure_context_with_registry(
        registry,
        selected,
        original,
        canonical_body,
        expected_body_digest,
        reference,
        expected_counter_digest,
    )
}

pub(super) fn measure_context_with_registry(
    registry: &CounterRegistry<'_>,
    selected: SelectedCatalogView<'_>,
    original: &PrepareInput,
    canonical_body: &[u8],
    expected_body_digest: &str,
    reference: &ContextCounterRefV1,
    expected_counter_digest: &str,
) -> ValidationResult<SealedContextMeasurement> {
    validate_digest(expected_body_digest)?;
    validate_digest(expected_counter_digest)?;
    let actual_body_digest = body_digest(canonical_body)?;
    let actual_counter_digest = counter_digest(reference)?;
    if actual_body_digest != expected_body_digest
        || actual_counter_digest != expected_counter_digest
    {
        return Err(ValidationError::InvalidArgument);
    }
    let validated = validate_prepare_input(original, selected)?;
    let entry = registry.lookup(reference)?;
    if entry.counter_digest != expected_counter_digest {
        return Err(ValidationError::InvalidArgument);
    }
    let body_bytes = u64::try_from(canonical_body.len()).map_err(|_| ValidationError::Limit)?;
    let facts = MeasurementFacts {
        body_bytes,
        prepare_logical_charge: validated.logical_charge,
        requested_output_tokens: original.max_output_tokens,
    };
    let required_context_tokens = confirm_deterministic(|| measure_facts(entry.compiled, facts))?;
    if selected
        .entry
        .context_tokens
        .is_some_and(|limit| required_context_tokens > limit)
    {
        return Err(ValidationError::Limit);
    }
    if entry.components.algorithm.is_empty()
        || entry.components.vocabulary.is_empty()
        || entry.components.wire_framing.is_empty()
        || entry.components.output_reservation.is_empty()
    {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(SealedContextMeasurement {
        provider_id: selected.provider_id.to_owned(),
        route_id: selected.route_id.to_owned(),
        catalog_digest: selected.catalog_digest.to_owned(),
        selection: selected.entry.selection.clone(),
        current_model: selected.entry.current_model.clone(),
        completion_operation: selected.entry.completion_operation.clone(),
        selected_entry: selected.entry.clone(),
        original: original.clone(),
        body_digest: actual_body_digest,
        canonical_body: canonical_body.to_vec(),
        counter_ref: reference.clone(),
        counter_digest: actual_counter_digest,
        required_context_tokens,
    })
}

fn validate_counter_ref(reference: &ContextCounterRefV1) -> ValidationResult {
    validate_local_id(&reference.registry_id)?;
    validate_local_id(&reference.algorithm_id)?;
    validate_local_id(&reference.wire_framing_id)?;
    validate_local_id(&reference.output_reservation_id)?;
    if reference.registry_version == 0
        || reference.algorithm_version == 0
        || reference.wire_framing_version == 0
        || reference.output_reservation_version == 0
    {
        return Err(ValidationError::InvalidArgument);
    }
    validate_digest(&reference.algorithm_digest)?;
    validate_digest(&reference.vocabulary_digest)?;
    validate_digest(&reference.wire_framing_digest)?;
    validate_digest(&reference.output_reservation_digest)
}

fn validate_local_id(value: &str) -> ValidationResult {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 64
        || !bytes[0].is_ascii_lowercase()
        || bytes[1..]
            .iter()
            .any(|byte| !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && *byte != b'-')
    {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(())
}

fn validate_digest(value: &str) -> ValidationResult {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(ValidationError::InvalidArgument);
    };
    if hex.len() != 64
        || hex
            .as_bytes()
            .iter()
            .any(|byte| !byte.is_ascii_digit() && !matches!(byte, b'a'..=b'f'))
    {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(())
}

fn profile_string(value: ProfileValue<'_>) -> ValidationResult<&str> {
    match value {
        ProfileValue::String(value) => Ok(value),
        ProfileValue::Unsigned(_) | ProfileValue::Boolean(_) | ProfileValue::Null => {
            Err(ValidationError::InvalidArgument)
        }
    }
}

fn profile_version(value: ProfileValue<'_>) -> ValidationResult<u16> {
    let ProfileValue::Unsigned(value) = value else {
        return Err(ValidationError::InvalidArgument);
    };
    let version = u16::try_from(value).map_err(|_| ValidationError::InvalidArgument)?;
    if version == 0 {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(version)
}

fn hash_string(hash: &mut Sha256, value: &str) -> ValidationResult {
    let length = u32::try_from(value.len()).map_err(|_| ValidationError::Limit)?;
    hash.update(length.to_be_bytes());
    hash.update(value.as_bytes());
    Ok(())
}

fn digest_text(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(71);
    result.push_str("sha256:");
    for byte in bytes {
        result.push(char::from(HEX[(byte >> 4) as usize]));
        result.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    result
}
