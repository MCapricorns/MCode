//! Freezes Host-vault reducer capacity, exhaustion, and failure-order boundaries.

use std::fs;

use tempfile::TempDir;

use super::test_support::*;
use super::*;
use crate::host_vault::model::{ConsumerFamily, MAX_GRANTS, parse_vault};
use crate::{ConfigErrorKind, ensure_home_layout};

fn credentials(active: usize, revoked: usize) -> Vec<String> {
    (0..active)
        .map(|index| credential("alpha", &format!("a{index:03}"), 1, "active"))
        .chain((0..revoked).map(|index| credential("zeta", &format!("r{index:03}"), 1, "revoked")))
        .collect()
}

fn grants(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| {
            grant(
                "providers",
                "com.mycode.providers",
                "pack",
                &format!("op{index:04}"),
                "alpha",
                "one",
                DIGEST_A,
            )
        })
        .collect()
}

fn oversized_insert_approvals() -> Vec<GrantApproval> {
    (0..=MAX_GRANTS)
        .map(|_| approval(ConsumerFamily::Web, "invalid/pack", "bad+operation"))
        .collect()
}

fn oversized_bind_approvals() -> Vec<BindApproval> {
    (0..=MAX_GRANTS)
        .map(|_| {
            bind_approval(
                ConsumerFamily::Web,
                "invalid/pack",
                "bad+operation",
                "missing",
                "missing",
                1,
            )
        })
        .collect()
}

#[test]
fn initialized_revision_zero_accepts_present_insert() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    ensure_home_layout(&layout).expect("bootstrap");
    crate::host_vault::initialize_empty_host_vault(&layout).expect("initialize");

    let result = persist_command(
        &layout,
        insert_command(
            ExpectedVaultState::Present(crate::host_vault::VaultRevision::EMPTY),
            "alpha",
            "one",
            b"secret",
            Vec::new(),
        ),
    )
    .expect("insert at revision zero");

    assert_eq!(result.revision, revision(1));
    assert_eq!(result.credential_version, Some(version(1)));
}

#[test]
fn overlimit_insert_rejects_before_duplicate_or_invalid_member_scans() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    let fixture = document(4, &[credential("alpha", "one", 1, "active")], &[]);
    install(&layout, &fixture);

    assert_preserved(
        &layout,
        fixture.as_bytes(),
        insert_command(
            ExpectedVaultState::Present(revision(4)),
            "beta",
            "two",
            b"secret",
            oversized_insert_approvals(),
        ),
        ConfigErrorKind::AuthorityValidation,
    );
    assert_preserved(
        &layout,
        fixture.as_bytes(),
        insert_command(
            ExpectedVaultState::Present(revision(3)),
            "beta",
            "two",
            b"secret",
            oversized_insert_approvals(),
        ),
        ConfigErrorKind::RevisionConflict,
    );
}

#[test]
fn overlimit_bind_rejects_before_duplicate_invalid_or_target_scans() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    let fixture = document(4, &[credential("alpha", "one", 1, "active")], &[]);
    install(&layout, &fixture);

    assert_preserved(
        &layout,
        fixture.as_bytes(),
        VaultCommand::Bind {
            expected: ExpectedVaultState::Present(revision(4)),
            approvals: oversized_bind_approvals(),
        },
        ConfigErrorKind::AuthorityValidation,
    );
    assert_preserved(
        &layout,
        fixture.as_bytes(),
        VaultCommand::Bind {
            expected: ExpectedVaultState::Present(revision(3)),
            approvals: oversized_bind_approvals(),
        },
        ConfigErrorKind::RevisionConflict,
    );
}

#[test]
fn grant_coordinate_pack_and_operation_boundaries_are_canonical() {
    let valid_pack = format!("a{}z", "b".repeat(126));
    let valid_operation = format!("a{}z", "b".repeat(126));
    assert_eq!(valid_pack.len(), 128);
    assert_eq!(valid_operation.len(), 128);
    validate_grant_input(&key(
        ConsumerFamily::Providers,
        &valid_pack,
        &valid_operation,
    ))
    .expect("128-byte coordinates");

    for invalid in [
        key(
            ConsumerFamily::Providers,
            &format!("a{}z", "b".repeat(127)),
            "operation",
        ),
        key(
            ConsumerFamily::Providers,
            "pack",
            &format!("a{}z", "b".repeat(127)),
        ),
        key(ConsumerFamily::Providers, "Pack", "operation"),
        key(ConsumerFamily::Providers, "pack", "operation-"),
    ] {
        assert_eq!(
            validate_grant_input(&invalid)
                .expect_err("invalid coordinate")
                .kind(),
            ConfigErrorKind::AuthorityValidation
        );
    }
}

#[test]
fn revoke_failures_preserve_exact_bytes() {
    let cases = [
        (
            document(5, &[credential("alpha", "one", 3, "active")], &[]),
            4,
            "alpha",
            "one",
            3,
            ConfigErrorKind::RevisionConflict,
        ),
        (
            document(5, &[credential("alpha", "one", 3, "active")], &[]),
            5,
            "alpha",
            "one",
            2,
            ConfigErrorKind::RevisionConflict,
        ),
        (
            document(5, &[credential("alpha", "one", 3, "active")], &[]),
            5,
            "missing",
            "one",
            3,
            ConfigErrorKind::RevisionConflict,
        ),
        (
            document(5, &[credential("alpha", "one", 3, "revoked")], &[]),
            5,
            "alpha",
            "one",
            3,
            ConfigErrorKind::RevisionConflict,
        ),
        (
            document(
                i64::MAX as u64,
                &[credential("alpha", "one", 3, "active")],
                &[],
            ),
            i64::MAX as u64,
            "alpha",
            "one",
            3,
            ConfigErrorKind::RevisionExhausted,
        ),
        (
            document(
                5,
                &[credential("alpha", "one", i64::MAX as u64, "active")],
                &[],
            ),
            5,
            "alpha",
            "one",
            i64::MAX as u64,
            ConfigErrorKind::RevisionExhausted,
        ),
    ];

    for (fixture, outer, service, account, credential_version, kind) in cases {
        let temp = TempDir::new().expect("temp");
        let layout = layout(&temp);
        install(&layout, &fixture);
        assert_preserved(
            &layout,
            fixture.as_bytes(),
            VaultCommand::Revoke {
                expected: ExpectedVaultState::Present(revision(outer)),
                service_id: service.into(),
                account_id: account.into(),
                expected_version: version(credential_version),
            },
            kind,
        );
    }
}

#[test]
fn insert_accepts_full_revoked_capacity_and_enforces_active_capacity() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    let fixture = document(8, &credentials(63, 256), &[]);
    install(&layout, &fixture);
    persist_command(
        &layout,
        insert_command(
            ExpectedVaultState::Present(revision(8)),
            "beta",
            "new",
            b"secret",
            Vec::new(),
        ),
    )
    .expect("reach active 64 with 256 revoked credentials");
    let bytes = fs::read(layout.host_auth_json()).expect("vault");
    let parsed = parse_vault(&bytes).expect("valid result");
    assert_eq!(parsed.credentials.len(), 320);

    let full_active = document(3, &credentials(64, 0), &[]);
    install(&layout, &full_active);
    assert_preserved(
        &layout,
        full_active.as_bytes(),
        insert_command(
            ExpectedVaultState::Present(revision(3)),
            "beta",
            "new",
            b"secret",
            Vec::new(),
        ),
        ConfigErrorKind::AuthorityValidation,
    );
}

#[test]
fn all_active_credentials_can_be_revoked_at_the_compatibility_envelope() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    let fixture = document(3, &credentials(64, 256), &[]);
    install(&layout, &fixture);

    for index in 0..64 {
        persist_command(
            &layout,
            VaultCommand::Revoke {
                expected: ExpectedVaultState::Present(revision(3 + index)),
                service_id: "alpha".into(),
                account_id: format!("a{index:03}"),
                expected_version: version(1),
            },
        )
        .expect("capacity-neutral revoke");
    }

    let tombstones = fs::read(layout.host_auth_json()).expect("vault");
    let parsed = parse_vault(&tombstones).expect("320 tombstones remain valid");
    assert_eq!(parsed.credentials.len(), 320);
    assert!(
        parsed
            .credentials
            .iter()
            .all(|credential| credential.state == CredentialState::Revoked)
    );
    assert_preserved(
        &layout,
        &tombstones,
        insert_command(
            ExpectedVaultState::Present(revision(67)),
            "beta",
            "new",
            b"secret",
            Vec::new(),
        ),
        ConfigErrorKind::AuthorityValidation,
    );
}

#[test]
fn full_grant_capacity_duplicate_bind_reports_conflict() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    let fixture = document(
        6,
        &[credential("alpha", "one", 1, "active")],
        &grants(MAX_GRANTS),
    );
    install(&layout, &fixture);

    assert_preserved(
        &layout,
        fixture.as_bytes(),
        VaultCommand::Bind {
            expected: ExpectedVaultState::Present(revision(6)),
            approvals: vec![bind_approval(
                ConsumerFamily::Providers,
                "pack",
                "op0000",
                "alpha",
                "one",
                1,
            )],
        },
        ConfigErrorKind::RevisionConflict,
    );
}

#[test]
fn full_grant_capacity_existing_credential_insert_reports_conflict() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    let fixture = document(
        6,
        &[credential("alpha", "one", 1, "active")],
        &grants(MAX_GRANTS),
    );
    install(&layout, &fixture);

    assert_preserved(
        &layout,
        fixture.as_bytes(),
        insert_command(
            ExpectedVaultState::Present(revision(6)),
            "alpha",
            "one",
            b"replacement",
            vec![approval(ConsumerFamily::Web, "new-pack", "new-operation")],
        ),
        ConfigErrorKind::RevisionConflict,
    );
}

#[test]
fn bind_from_1023_to_1024_succeeds() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    let fixture = document(
        6,
        &[credential("alpha", "one", 1, "active")],
        &grants(MAX_GRANTS - 1),
    );
    install(&layout, &fixture);

    persist_command(
        &layout,
        VaultCommand::Bind {
            expected: ExpectedVaultState::Present(revision(6)),
            approvals: vec![bind_approval(
                ConsumerFamily::Web,
                "pack",
                "last",
                "alpha",
                "one",
                1,
            )],
        },
    )
    .expect("1024th grant");
    let bytes = fs::read(layout.host_auth_json()).expect("vault");
    assert_eq!(
        parse_vault(&bytes).expect("valid result").grants.len(),
        MAX_GRANTS
    );
}

#[test]
fn failed_insert_batch_is_all_or_none() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    let fixture = document(2, &[credential("alpha", "one", 1, "active")], &[]);
    install(&layout, &fixture);
    assert_preserved(
        &layout,
        fixture.as_bytes(),
        insert_command(
            ExpectedVaultState::Present(revision(2)),
            "beta",
            "two",
            b"secret",
            vec![
                approval(ConsumerFamily::Providers, "valid-pack", "read"),
                approval(ConsumerFamily::Web, "invalid/pack", "write"),
            ],
        ),
        ConfigErrorKind::AuthorityValidation,
    );
}

#[test]
fn rebind_and_unbind_work_at_full_grant_capacity() {
    for unbind in [false, true] {
        let temp = TempDir::new().expect("temp");
        let layout = layout(&temp);
        let fixture = document(
            7,
            &[
                credential("alpha", "one", 1, "active"),
                credential("beta", "two", 2, "active"),
            ],
            &grants(MAX_GRANTS),
        );
        install(&layout, &fixture);
        let command = if unbind {
            VaultCommand::Unbind {
                expected: ExpectedVaultState::Present(revision(7)),
                key: key(ConsumerFamily::Providers, "pack", "op0000"),
                old: binding("alpha", "one", DIGEST_A),
            }
        } else {
            VaultCommand::Rebind {
                expected: ExpectedVaultState::Present(revision(7)),
                key: key(ConsumerFamily::Providers, "pack", "op0000"),
                old: binding("alpha", "one", DIGEST_A),
                replacement: target("beta", "two", 2),
                authority_digest: digest(DIGEST_B),
            }
        };
        persist_command(&layout, command).expect("capacity-neutral mutation");
        let bytes = fs::read(layout.host_auth_json()).expect("vault");
        let parsed = parse_vault(&bytes).expect("valid result");
        assert_eq!(
            parsed.grants.len(),
            if unbind { MAX_GRANTS - 1 } else { MAX_GRANTS }
        );
    }
}

#[test]
fn rebind_and_unbind_revision_exhaustion_preserve_full_vault() {
    for unbind in [false, true] {
        let temp = TempDir::new().expect("temp");
        let layout = layout(&temp);
        let fixture = document(
            i64::MAX as u64,
            &[
                credential("alpha", "one", 1, "active"),
                credential("beta", "two", 2, "active"),
            ],
            &grants(MAX_GRANTS),
        );
        install(&layout, &fixture);
        let command = if unbind {
            VaultCommand::Unbind {
                expected: ExpectedVaultState::Present(revision(i64::MAX as u64)),
                key: key(ConsumerFamily::Providers, "pack", "op0000"),
                old: binding("alpha", "one", DIGEST_A),
            }
        } else {
            VaultCommand::Rebind {
                expected: ExpectedVaultState::Present(revision(i64::MAX as u64)),
                key: key(ConsumerFamily::Providers, "pack", "op0000"),
                old: binding("alpha", "one", DIGEST_A),
                replacement: target("beta", "two", 2),
                authority_digest: digest(DIGEST_B),
            }
        };
        assert_preserved(
            &layout,
            fixture.as_bytes(),
            command,
            ConfigErrorKind::RevisionExhausted,
        );
    }
}
