//! Closed context-counter registry and measurement contract tests.
use std::cell::Cell;

use crate::provider_wit::exports::mcode::provider_pack::provider_api::ModelSelection;

use super::ValidationError;
use super::context_counter::{
    CompiledCounter, ComponentBytes, MeasurementFacts, ProfileField, ProfileValue,
    RegistryEntrySpec, body_digest, confirm_deterministic, counter_digest, measure_context,
    measure_context_with_registry, measure_facts, parse_context_counter_ref, registry_for_test,
    trusted_dummy_counter_digest, trusted_dummy_counter_ref, trusted_registry,
};
use super::prepare::SelectedCatalogView;
use super::test_support::{DIGEST, OTHER_DIGEST, catalog_entry, prepare_input, selected};

const ALGORITHM_BYTES: &[u8] =
    b"mcode-dummy-algorithm-v1\0\x00\x00\x00\x00\x00\x00\x00\x02\x00\x00\x00\x00\x00\x00\x00\x01";
const VOCABULARY_BYTES: &[u8] = b"mcode-dummy-vocabulary-v1\0\x00\x00\x00\x00\x00\x00\x00\x04";
const WIRE_FRAMING_BYTES: &[u8] = b"mcode-dummy-wire-framing-v1\0\x00\x00\x00\x00\x00\x00\x00\x03";
const OUTPUT_RESERVATION_BYTES: &[u8] =
    b"mcode-dummy-output-reservation-v1\0\x00\x00\x00\x00\x00\x00\x00\x0e";
const BODY_DIGEST: &str = "sha256:9bf48f1700bc188ea7faf2c1749289fdef9e748917d486505fdbd0a331fb83cb";

fn profile_fields() -> Vec<ProfileField<'static>> {
    vec![
        ProfileField {
            name: "registry-id",
            value: ProfileValue::String("t7-dummy-context"),
        },
        ProfileField {
            name: "registry-version",
            value: ProfileValue::Unsigned(1),
        },
        ProfileField {
            name: "algorithm-id",
            value: ProfileValue::String("dummy-byte-charge"),
        },
        ProfileField {
            name: "algorithm-version",
            value: ProfileValue::Unsigned(1),
        },
        ProfileField {
            name: "algorithm-digest",
            value: ProfileValue::String(
                "sha256:bcc7a3c85a0e50b4bdb889040974603c1fdfefe1ad465768517773b3bc22ff83",
            ),
        },
        ProfileField {
            name: "vocabulary-digest",
            value: ProfileValue::String(
                "sha256:bf90d47c54bbdb50a402f44abc0dbd71a7dc2cdc3a032391c1d8985f0ca9be21",
            ),
        },
        ProfileField {
            name: "wire-framing-id",
            value: ProfileValue::String("dummy-fixed-framing"),
        },
        ProfileField {
            name: "wire-framing-version",
            value: ProfileValue::Unsigned(1),
        },
        ProfileField {
            name: "wire-framing-digest",
            value: ProfileValue::String(
                "sha256:32a504d4d1dafe4eb3e5bbeb7364269ec913f7ba68d482a3d0e3406a16794092",
            ),
        },
        ProfileField {
            name: "output-reservation-id",
            value: ProfileValue::String("dummy-output-reservation"),
        },
        ProfileField {
            name: "output-reservation-version",
            value: ProfileValue::Unsigned(1),
        },
        ProfileField {
            name: "output-reservation-digest",
            value: ProfileValue::String(
                "sha256:f7f2f70c3dc81c3064f66dfc218df302234d981a960ac141730ab368ce3c01a2",
            ),
        },
    ]
}

fn entry_spec() -> RegistryEntrySpec<'static> {
    RegistryEntrySpec {
        reference: trusted_dummy_counter_ref(),
        components: ComponentBytes {
            algorithm: ALGORITHM_BYTES,
            vocabulary: VOCABULARY_BYTES,
            wire_framing: WIRE_FRAMING_BYTES,
            output_reservation: OUTPUT_RESERVATION_BYTES,
        },
    }
}

fn base_compiled() -> CompiledCounter {
    CompiledCounter {
        body_byte_weight: 1,
        prepare_charge_weight: 1,
        bytes_per_token: 1,
        framing_tokens: 0,
        default_output_tokens: 0,
    }
}

#[test]
fn profile_parses_exact_order_and_counter_digest_matches_golden() {
    let reference = parse_context_counter_ref(&profile_fields()).expect("valid exact profile");
    assert_eq!(reference, trusted_dummy_counter_ref());
    assert_eq!(
        counter_digest(&reference).expect("counter digest"),
        "sha256:da58bfe417ee5a4759d3a287b1af86fba7da1e6522e87c10b64a1e90232062b6"
    );
    assert_eq!(
        trusted_dummy_counter_digest(),
        counter_digest(&reference).expect("valid counter digest")
    );
}

#[test]
fn profile_rejects_missing_extra_unknown_reordered_type_and_coercion() {
    let exact = profile_fields();

    assert!(parse_context_counter_ref(&exact[..11]).is_err());

    let mut extra = exact.clone();
    extra.push(ProfileField {
        name: "extension",
        value: ProfileValue::Null,
    });
    assert!(parse_context_counter_ref(&extra).is_err());

    let mut unknown = exact.clone();
    unknown[0].name = "registry";
    assert!(parse_context_counter_ref(&unknown).is_err());

    let mut reordered = exact.clone();
    reordered.swap(0, 2);
    assert!(parse_context_counter_ref(&reordered).is_err());

    let mut wrong_type = exact.clone();
    wrong_type[4].value = ProfileValue::Boolean(false);
    assert!(parse_context_counter_ref(&wrong_type).is_err());

    let mut coerced = exact.clone();
    coerced[1].value = ProfileValue::String("1");
    assert!(parse_context_counter_ref(&coerced).is_err());

    let mut too_large = exact;
    too_large[1].value = ProfileValue::Unsigned(u64::from(u16::MAX) + 1);
    assert!(parse_context_counter_ref(&too_large).is_err());
}

#[test]
fn local_id_version_and_digest_grammars_are_exact() {
    let mut fields = profile_fields();
    let maximum_id = format!("a{}", "0".repeat(63));
    fields[0].value = ProfileValue::String(&maximum_id);
    assert!(parse_context_counter_ref(&fields).is_ok());

    let oversized_id = format!("a{}", "0".repeat(64));
    fields[0].value = ProfileValue::String(&oversized_id);
    assert!(parse_context_counter_ref(&fields).is_err());

    for invalid in ["", "0counter", "Counter", "counter_1", "counter.1"] {
        fields[0].value = ProfileValue::String(invalid);
        assert!(
            parse_context_counter_ref(&fields).is_err(),
            "accepted {invalid}"
        );
    }

    fields = profile_fields();
    fields[3].value = ProfileValue::Unsigned(0);
    assert!(parse_context_counter_ref(&fields).is_err());

    fields = profile_fields();
    fields[4].value = ProfileValue::String(
        "sha256:BCC7a3c85a0e50b4bdb889040974603c1fdfefe1ad465768517773b3bc22ff83",
    );
    assert!(parse_context_counter_ref(&fields).is_err());

    fields[4].value = ProfileValue::String(
        "sha256:bcc7a3c85a0e50b4bdb889040974603c1fdfefe1ad465768517773b3bc22ff8",
    );
    assert!(parse_context_counter_ref(&fields).is_err());
}

#[test]
fn registry_has_one_static_self_validated_dummy_entry_only() {
    let first = trusted_registry().expect("trusted registry");
    let second = trusted_registry().expect("same trusted registry");
    assert!(std::ptr::eq(first, second));
    assert_eq!(
        registry_for_test(&[]).err(),
        Some(ValidationError::InvalidArgument)
    );

    let spec = entry_spec();
    assert_eq!(
        registry_for_test(&[spec.clone(), spec]).err(),
        Some(ValidationError::InvalidArgument)
    );
}

#[test]
fn each_component_mutation_with_unchanged_tuple_rejects_construction() {
    for component in 0..4 {
        let mut algorithm = ALGORITHM_BYTES.to_vec();
        let mut vocabulary = VOCABULARY_BYTES.to_vec();
        let mut wire_framing = WIRE_FRAMING_BYTES.to_vec();
        let mut output_reservation = OUTPUT_RESERVATION_BYTES.to_vec();
        let bytes = match component {
            0 => &mut algorithm,
            1 => &mut vocabulary,
            2 => &mut wire_framing,
            3 => &mut output_reservation,
            _ => unreachable!(),
        };
        let last = bytes.last_mut().expect("component byte");
        *last = last.checked_add(1).expect("fixture byte");

        let spec = RegistryEntrySpec {
            reference: trusted_dummy_counter_ref(),
            components: ComponentBytes {
                algorithm: &algorithm,
                vocabulary: &vocabulary,
                wire_framing: &wire_framing,
                output_reservation: &output_reservation,
            },
        };
        assert_eq!(
            registry_for_test(&[spec]).err(),
            Some(ValidationError::InvalidArgument),
            "component {component} was not bound to its digest"
        );
    }
}

#[test]
fn lookup_rejects_every_tuple_field_mutation_and_unknown_tuple() {
    let registry = trusted_registry().expect("trusted registry");
    let mut mutations = Vec::new();

    let mut value = trusted_dummy_counter_ref();
    value.registry_id.push('x');
    mutations.push(value);
    let mut value = trusted_dummy_counter_ref();
    value.registry_version = 2;
    mutations.push(value);
    let mut value = trusted_dummy_counter_ref();
    value.algorithm_id.push('x');
    mutations.push(value);
    let mut value = trusted_dummy_counter_ref();
    value.algorithm_version = 2;
    mutations.push(value);
    let mut value = trusted_dummy_counter_ref();
    value.algorithm_digest = OTHER_DIGEST.to_owned();
    mutations.push(value);
    let mut value = trusted_dummy_counter_ref();
    value.vocabulary_digest = OTHER_DIGEST.to_owned();
    mutations.push(value);
    let mut value = trusted_dummy_counter_ref();
    value.wire_framing_id.push('x');
    mutations.push(value);
    let mut value = trusted_dummy_counter_ref();
    value.wire_framing_version = 2;
    mutations.push(value);
    let mut value = trusted_dummy_counter_ref();
    value.wire_framing_digest = OTHER_DIGEST.to_owned();
    mutations.push(value);
    let mut value = trusted_dummy_counter_ref();
    value.output_reservation_id.push('x');
    mutations.push(value);
    let mut value = trusted_dummy_counter_ref();
    value.output_reservation_version = 2;
    mutations.push(value);
    let mut value = trusted_dummy_counter_ref();
    value.output_reservation_digest = OTHER_DIGEST.to_owned();
    mutations.push(value);

    for (index, reference) in mutations.iter().enumerate() {
        let entry = catalog_entry("model");
        let input = prepare_input();
        let digest = counter_digest(reference).expect("valid mutated tuple");
        assert_eq!(
            measure_context_with_registry(
                registry,
                selected(&entry),
                &input,
                b"{}",
                BODY_DIGEST,
                reference,
                &digest,
            )
            .err(),
            Some(ValidationError::InvalidArgument),
            "tuple field {index} unexpectedly matched"
        );
    }
}

#[test]
fn dummy_measurement_accepts_exact_limit_and_rejects_limit_plus_one() {
    let reference = trusted_dummy_counter_ref();
    let input = prepare_input();
    let mut entry = catalog_entry("model");
    entry.context_tokens = Some(64);

    let sealed = measure_context(
        selected(&entry),
        &input,
        b"{}",
        BODY_DIGEST,
        &reference,
        trusted_dummy_counter_digest(),
    )
    .expect("exact context limit");
    assert_eq!(sealed.required_context_tokens, 64);

    entry.context_tokens = Some(63);
    assert_eq!(
        measure_context(
            selected(&entry),
            &input,
            b"{}",
            BODY_DIGEST,
            &reference,
            trusted_dummy_counter_digest(),
        )
        .err(),
        Some(ValidationError::Limit)
    );
}

#[test]
fn absent_catalog_limit_still_measures_and_requested_reservation_is_exact() {
    let reference = trusted_dummy_counter_ref();
    let mut entry = catalog_entry("model");
    entry.context_tokens = None;
    entry.max_output_tokens = Some(20);
    let mut input = prepare_input();
    input.max_output_tokens = Some(20);

    let sealed = measure_context(
        selected(&entry),
        &input,
        b"{}",
        BODY_DIGEST,
        &reference,
        trusted_dummy_counter_digest(),
    )
    .expect("unbounded catalog context");
    assert_eq!(sealed.required_context_tokens, 72);
}

#[test]
fn sealed_result_retains_full_binding_and_isolated_bytes() {
    let reference = trusted_dummy_counter_ref();
    let mut entry = catalog_entry("model");
    entry.context_tokens = Some(64);
    let mut input = prepare_input();
    let mut body = b"{}".to_vec();

    let sealed = measure_context(
        selected(&entry),
        &input,
        &body,
        BODY_DIGEST,
        &reference,
        trusted_dummy_counter_digest(),
    )
    .expect("sealed measurement");
    input.system.push("mutated".to_owned());
    body[0] = b'[';
    entry.current_model = "mutated-model".to_owned();

    assert_eq!(sealed.provider_id, "provider");
    assert_eq!(sealed.route_id, "route");
    assert_eq!(sealed.catalog_digest, DIGEST);
    assert!(matches!(sealed.selection, ModelSelection::Exact(ref model) if model == "model"));
    assert_eq!(sealed.current_model, "model");
    assert_eq!(sealed.completion_operation, "complete");
    assert_eq!(sealed.selected_entry.current_model, "model");
    assert!(sealed.original.system.is_empty());
    assert_eq!(sealed.body_digest, BODY_DIGEST);
    assert_eq!(sealed.canonical_body, b"{}");
    assert_eq!(sealed.counter_ref, reference);
    assert_eq!(sealed.counter_digest, trusted_dummy_counter_digest());
    assert_eq!(sealed.required_context_tokens, 64);
}

#[test]
fn crossed_original_snapshot_body_and_ref_digests_are_rejected() {
    let reference = trusted_dummy_counter_ref();
    let mut entry = catalog_entry("model");
    entry.context_tokens = Some(64);
    let input = prepare_input();

    let mut crossed = input.clone();
    crossed.provider_id = "other-provider".to_owned();
    assert!(
        measure_context(
            selected(&entry),
            &crossed,
            b"{}",
            BODY_DIGEST,
            &reference,
            trusted_dummy_counter_digest(),
        )
        .is_err()
    );

    crossed = input.clone();
    crossed.route_id = "other-route".to_owned();
    assert!(
        measure_context(
            selected(&entry),
            &crossed,
            b"{}",
            BODY_DIGEST,
            &reference,
            trusted_dummy_counter_digest(),
        )
        .is_err()
    );

    crossed = input.clone();
    crossed.catalog_digest = OTHER_DIGEST.to_owned();
    assert!(
        measure_context(
            selected(&entry),
            &crossed,
            b"{}",
            BODY_DIGEST,
            &reference,
            trusted_dummy_counter_digest(),
        )
        .is_err()
    );

    crossed = input.clone();
    crossed.selection = ModelSelection::Exact("other-model".to_owned());
    assert!(
        measure_context(
            selected(&entry),
            &crossed,
            b"{}",
            BODY_DIGEST,
            &reference,
            trusted_dummy_counter_digest(),
        )
        .is_err()
    );

    crossed = input.clone();
    crossed.current_model = "other-model".to_owned();
    assert!(
        measure_context(
            selected(&entry),
            &crossed,
            b"{}",
            BODY_DIGEST,
            &reference,
            trusted_dummy_counter_digest(),
        )
        .is_err()
    );

    crossed = input.clone();
    crossed.operation_id = "other-operation".to_owned();
    assert!(
        measure_context(
            selected(&entry),
            &crossed,
            b"{}",
            BODY_DIGEST,
            &reference,
            trusted_dummy_counter_digest(),
        )
        .is_err()
    );

    assert_eq!(
        measure_context(
            selected(&entry),
            &input,
            b"[]",
            BODY_DIGEST,
            &reference,
            trusted_dummy_counter_digest(),
        )
        .err(),
        Some(ValidationError::InvalidArgument)
    );
    assert_eq!(
        measure_context(
            selected(&entry),
            &input,
            b"{}",
            OTHER_DIGEST,
            &reference,
            trusted_dummy_counter_digest(),
        )
        .err(),
        Some(ValidationError::InvalidArgument)
    );
    assert_eq!(
        measure_context(
            selected(&entry),
            &input,
            b"{}",
            BODY_DIGEST,
            &reference,
            OTHER_DIGEST,
        )
        .err(),
        Some(ValidationError::InvalidArgument)
    );

    let mut unknown = reference;
    unknown.registry_version = 2;
    let unknown_digest = counter_digest(&unknown).expect("valid unknown tuple");
    assert_eq!(
        measure_context(
            selected(&entry),
            &input,
            b"{}",
            BODY_DIGEST,
            &unknown,
            &unknown_digest,
        )
        .err(),
        Some(ValidationError::InvalidArgument)
    );
}

#[test]
fn selected_snapshot_crossing_is_rejected_before_measurement() {
    let reference = trusted_dummy_counter_ref();
    let input = prepare_input();
    let entry = catalog_entry("model");
    for view in [
        SelectedCatalogView {
            provider_id: "other-provider",
            route_id: "route",
            catalog_digest: DIGEST,
            entry: &entry,
        },
        SelectedCatalogView {
            provider_id: "provider",
            route_id: "other-route",
            catalog_digest: DIGEST,
            entry: &entry,
        },
        SelectedCatalogView {
            provider_id: "provider",
            route_id: "route",
            catalog_digest: OTHER_DIGEST,
            entry: &entry,
        },
    ] {
        assert!(
            measure_context(
                view,
                &input,
                b"{}",
                BODY_DIGEST,
                &reference,
                trusted_dummy_counter_digest(),
            )
            .is_err()
        );
    }
}

#[test]
fn every_checked_measurement_stage_rejects_overflow() {
    let mut compiled = base_compiled();
    compiled.body_byte_weight = 2;
    assert_eq!(
        measure_facts(
            compiled,
            MeasurementFacts {
                body_bytes: u64::MAX,
                prepare_logical_charge: 0,
                requested_output_tokens: Some(0),
            },
        ),
        Err(ValidationError::Limit)
    );

    compiled = base_compiled();
    compiled.prepare_charge_weight = 2;
    assert_eq!(
        measure_facts(
            compiled,
            MeasurementFacts {
                body_bytes: 0,
                prepare_logical_charge: u64::MAX,
                requested_output_tokens: Some(0),
            },
        ),
        Err(ValidationError::Limit)
    );

    compiled = base_compiled();
    assert_eq!(
        measure_facts(
            compiled,
            MeasurementFacts {
                body_bytes: u64::MAX,
                prepare_logical_charge: 1,
                requested_output_tokens: Some(0),
            },
        ),
        Err(ValidationError::Limit)
    );

    compiled = base_compiled();
    compiled.bytes_per_token = 2;
    assert_eq!(
        measure_facts(
            compiled,
            MeasurementFacts {
                body_bytes: u64::MAX,
                prepare_logical_charge: 0,
                requested_output_tokens: Some(0),
            },
        ),
        Err(ValidationError::Limit)
    );

    compiled = base_compiled();
    compiled.framing_tokens = 1;
    assert_eq!(
        measure_facts(
            compiled,
            MeasurementFacts {
                body_bytes: u64::MAX,
                prepare_logical_charge: 0,
                requested_output_tokens: Some(0),
            },
        ),
        Err(ValidationError::Limit)
    );

    compiled = base_compiled();
    assert_eq!(
        measure_facts(
            compiled,
            MeasurementFacts {
                body_bytes: u64::MAX,
                prepare_logical_charge: 0,
                requested_output_tokens: Some(1),
            },
        ),
        Err(ValidationError::Limit)
    );

    compiled = base_compiled();
    compiled.bytes_per_token = 0;
    assert_eq!(
        measure_facts(
            compiled,
            MeasurementFacts {
                body_bytes: 1,
                prepare_logical_charge: 0,
                requested_output_tokens: Some(0),
            },
        ),
        Err(ValidationError::InvalidArgument)
    );
}

#[test]
fn repeat_is_deterministic_and_injected_nondeterminism_is_rejected() {
    assert_eq!(confirm_deterministic(|| Ok(64)), Ok(64));

    let calls = Cell::new(0_u64);
    assert_eq!(
        confirm_deterministic(|| {
            let value = calls.get();
            calls.set(value + 1);
            Ok(value)
        }),
        Err(ValidationError::InvalidArgument)
    );
}

#[test]
fn body_digest_has_exact_domain_length_and_mutation_binding() {
    assert_eq!(body_digest(b"{}").expect("body digest"), BODY_DIGEST);
    assert_ne!(
        body_digest(b"[]").expect("mutated body digest"),
        BODY_DIGEST
    );
}
