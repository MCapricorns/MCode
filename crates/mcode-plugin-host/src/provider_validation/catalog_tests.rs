//! Catalog validator and digest tests.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    AuthInstructions, AuthInteractionRequest, AuthInteractionResponse, CatalogMetadataEntry,
    CatalogMetadataView, CatalogPage, CatalogRequest, CatalogRevision, CatalogSourceView,
    DescriptorRequest, InputModality, ModelSelection, ProviderDescriptor,
};

use super::ValidationError;
use super::catalog::{
    catalog_digest, validate_auth_interaction, validate_catalog_entries, validate_catalog_page,
    validate_catalog_source, validate_descriptor,
};
use super::test_support::{DIGEST, OTHER_DIGEST, catalog_entry};

fn descriptor_request(source: CatalogSourceView) -> DescriptorRequest {
    DescriptorRequest {
        provider_id: "provider".to_owned(),
        route_id: "route".to_owned(),
        catalog_source: source,
    }
}

fn descriptor(count: u32) -> ProviderDescriptor {
    ProviderDescriptor {
        provider_id: "provider".to_owned(),
        route_id: "route".to_owned(),
        source_revision: None,
        catalog_digest: DIGEST.to_owned(),
        model_count: count,
    }
}

fn request(offset: u32, limit: u16) -> CatalogRequest {
    CatalogRequest {
        provider_id: "provider".to_owned(),
        route_id: "route".to_owned(),
        catalog_source: CatalogSourceView::Embedded,
        catalog_digest: DIGEST.to_owned(),
        offset,
        limit,
    }
}

#[test]
fn catalog_digest_matches_fixed_golden_and_mutates_with_every_entry() {
    let entries = vec![catalog_entry("model-a"), catalog_entry("model-b")];
    let digest = catalog_digest("provider", "route", &entries).expect("valid catalog");
    assert_eq!(
        digest.as_str(),
        "sha256:68125c84afdbe7ca4c1ceb943506431541dc3c9df6cf4dd60fd27c992d76c3ab"
    );

    let mut mutated = entries.clone();
    mutated[1].max_output_tokens = Some(1_025);
    let changed = catalog_digest("provider", "route", &mutated).expect("mutated catalog");
    assert_ne!(digest.as_str(), changed.as_str());
}

#[test]
fn catalog_selection_order_payload_uniqueness_and_exact_mapping_are_enforced() {
    let ordered = vec![catalog_entry("a"), catalog_entry("b")];
    assert!(validate_catalog_entries(&ordered).is_ok());

    let mut reversed = ordered.clone();
    reversed.reverse();
    assert!(validate_catalog_entries(&reversed).is_err());

    let mut cross_tag = catalog_entry("a");
    cross_tag.selection = ModelSelection::Alias("a".to_owned());
    let crossed = vec![catalog_entry("a"), cross_tag];
    assert!(validate_catalog_entries(&crossed).is_err());

    let mut wrong_exact = catalog_entry("a");
    wrong_exact.current_model = "b".to_owned();
    assert!(validate_catalog_entries(&[wrong_exact]).is_err());

    let mut alias = catalog_entry("current-model");
    alias.selection = ModelSelection::Alias("stable-alias".to_owned());
    assert!(validate_catalog_entries(&[alias]).is_ok());
}

#[test]
fn catalog_scalar_and_list_boundaries_cover_zero_one_n_and_n_plus_one() {
    let mut entry = catalog_entry("model");
    entry.input_modalities.clear();
    assert_eq!(
        validate_catalog_entries(&[entry.clone()]),
        Err(ValidationError::Limit)
    );
    entry.input_modalities = vec![InputModality::Unknown];
    assert!(validate_catalog_entries(&[entry.clone()]).is_ok());
    entry.input_modalities = vec![
        InputModality::Unknown,
        InputModality::Text,
        InputModality::Image,
    ];
    assert!(validate_catalog_entries(&[entry.clone()]).is_err());
    entry.input_modalities = vec![
        InputModality::Text,
        InputModality::Image,
        InputModality::Image,
        InputModality::Image,
    ];
    assert_eq!(
        validate_catalog_entries(&[entry]),
        Err(ValidationError::Limit)
    );

    let many = (0..=4_096)
        .map(|index| catalog_entry(&format!("m{index:04}")))
        .collect::<Vec<_>>();
    assert!(validate_catalog_entries(&many[..4_096]).is_ok());
    assert_eq!(validate_catalog_entries(&many), Err(ValidationError::Limit));
}

#[test]
fn verified_source_revision_and_metadata_are_self_contained() {
    let revision = CatalogRevision {
        last_modified: 1,
        canonical_content_digest: DIGEST.to_owned(),
    };
    let metadata = |model: &str| CatalogMetadataEntry {
        selection: ModelSelection::Exact(model.to_owned()),
        display_name: Some("Name".to_owned()),
        input_modalities: vec![InputModality::Text],
        tool_capability: super::test_support::supported_tools(),
        reasoning_capability: super::test_support::supported_reasoning(),
        context_tokens: Some(1),
        max_output_tokens: Some(1),
    };
    let source = CatalogSourceView::Verified(CatalogMetadataView {
        revision: revision.clone(),
        entries: vec![metadata("a"), metadata("b")],
    });
    assert!(validate_catalog_source(&source).is_ok());

    let mut response = descriptor(2);
    response.source_revision = Some(revision);
    assert!(validate_descriptor(&descriptor_request(source.clone()), &response).is_ok());

    let mut wrong = response.clone();
    wrong
        .source_revision
        .as_mut()
        .expect("revision")
        .last_modified = 2;
    assert!(validate_descriptor(&descriptor_request(source), &wrong).is_err());

    let zero_revision = CatalogSourceView::Verified(CatalogMetadataView {
        revision: CatalogRevision {
            last_modified: 0,
            canonical_content_digest: DIGEST.to_owned(),
        },
        entries: vec![],
    });
    assert!(validate_catalog_source(&zero_revision).is_err());
}

#[test]
fn catalog_page_requires_exact_local_progress_relation() {
    let descriptor_request = descriptor_request(CatalogSourceView::Embedded);
    let descriptor = descriptor(3);
    let first_request = request(0, 2);
    let mut page = CatalogPage {
        provider_id: "provider".to_owned(),
        route_id: "route".to_owned(),
        source_revision: None,
        catalog_digest: DIGEST.to_owned(),
        declared_count: 3,
        offset: 0,
        entries: vec![catalog_entry("a"), catalog_entry("b")],
        next_offset: Some(2),
    };
    assert!(validate_catalog_page(&descriptor_request, &descriptor, &first_request, &page).is_ok());

    page.next_offset = Some(0);
    assert!(
        validate_catalog_page(&descriptor_request, &descriptor, &first_request, &page).is_err()
    );
    page.next_offset = Some(3);
    assert!(
        validate_catalog_page(&descriptor_request, &descriptor, &first_request, &page).is_err()
    );

    let final_request = request(2, 1);
    page.offset = 2;
    page.entries = vec![catalog_entry("c")];
    page.next_offset = None;
    assert!(validate_catalog_page(&descriptor_request, &descriptor, &final_request, &page).is_ok());

    let empty_final_request = request(3, 1);
    page.offset = 3;
    page.entries.clear();
    assert!(
        validate_catalog_page(
            &descriptor_request,
            &descriptor,
            &empty_final_request,
            &page
        )
        .is_ok()
    );
}

#[test]
fn page_limit_and_comparison_fields_fail_closed() {
    let descriptor_request = descriptor_request(CatalogSourceView::Embedded);
    let maximum_descriptor = descriptor(256);
    let maximum_request = request(0, 256);
    let maximum_page = CatalogPage {
        provider_id: "provider".to_owned(),
        route_id: "route".to_owned(),
        source_revision: None,
        catalog_digest: DIGEST.to_owned(),
        declared_count: 256,
        offset: 0,
        entries: (0..256)
            .map(|index| catalog_entry(&format!("m{index:03}")))
            .collect(),
        next_offset: None,
    };
    assert!(
        validate_catalog_page(
            &descriptor_request,
            &maximum_descriptor,
            &maximum_request,
            &maximum_page
        )
        .is_ok()
    );

    let full_descriptor = descriptor(257);
    let full_request = request(0, 256);
    let entries = (0..257)
        .map(|index| catalog_entry(&format!("m{index:03}")))
        .collect();
    let page = CatalogPage {
        provider_id: "provider".to_owned(),
        route_id: "route".to_owned(),
        source_revision: None,
        catalog_digest: DIGEST.to_owned(),
        declared_count: 257,
        offset: 0,
        entries,
        next_offset: Some(257),
    };
    assert_eq!(
        validate_catalog_page(&descriptor_request, &full_descriptor, &full_request, &page),
        Err(ValidationError::Limit)
    );

    let mut crossed = descriptor(0);
    crossed.catalog_digest = OTHER_DIGEST.to_owned();
    assert!(validate_descriptor(&descriptor_request, &crossed).is_ok());
    let request = request(0, 1);
    assert!(
        super::catalog::validate_catalog_request(&descriptor_request, &crossed, &request).is_err()
    );
}

#[test]
fn auth_presentation_is_bounded_and_has_no_answer_channel() {
    let request = AuthInteractionRequest {
        provider_id: "provider".to_owned(),
        route_id: "route".to_owned(),
    };
    let response = AuthInteractionResponse::Instructions(AuthInstructions {
        title: "Sign in".to_owned(),
        steps: vec!["Open settings".to_owned()],
    });
    assert!(validate_auth_interaction(&request, &response, "provider", "route").is_ok());

    let empty = AuthInteractionResponse::Instructions(AuthInstructions {
        title: "Sign in".to_owned(),
        steps: vec![],
    });
    assert!(validate_auth_interaction(&request, &empty, "provider", "route").is_err());

    let maximum = AuthInteractionResponse::Instructions(AuthInstructions {
        title: "Sign in".to_owned(),
        steps: vec!["step".to_owned(); 32],
    });
    assert!(validate_auth_interaction(&request, &maximum, "provider", "route").is_ok());
    let too_many = AuthInteractionResponse::Instructions(AuthInstructions {
        title: "Sign in".to_owned(),
        steps: vec!["step".to_owned(); 33],
    });
    assert_eq!(
        validate_auth_interaction(&request, &too_many, "provider", "route"),
        Err(ValidationError::Limit)
    );
}
