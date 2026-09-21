//! Tests private Host-vault credential and grant reducers.

use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use tempfile::TempDir;
use zeroize::Zeroizing;

use super::test_support::*;
use super::*;
use crate::host_vault::model::{ConsumerFamily, MAX_SECRET_BYTES, parse_vault};
use crate::host_vault::{VaultRevision, initialize_empty_host_vault};
use crate::{ConfigErrorKind, ensure_home_layout};
#[test]
fn absent_and_initialized_zero_are_distinct_and_first_insert_is_exact() {
    let absent_temp = TempDir::new().expect("temp");
    let absent_layout = layout(&absent_temp);
    ensure_home_layout(&absent_layout).expect("bootstrap");
    let error = persist_command(
        &absent_layout,
        insert_command(
            ExpectedVaultState::Present(VaultRevision::EMPTY),
            "alpha",
            "one",
            b"sentinel-secret",
            Vec::new(),
        ),
    )
    .expect_err("missing is not revision zero");
    assert_eq!(error.kind(), ConfigErrorKind::RevisionConflict);
    assert!(!absent_layout.host_auth_json().exists());

    let approvals = vec![
        approval(ConsumerFamily::Web, "web-pack", "write"),
        approval(ConsumerFamily::Providers, "provider-pack", "read"),
    ];
    let result = persist_command(
        &absent_layout,
        insert_command(
            ExpectedVaultState::Absent,
            "alpha",
            "one",
            b"sentinel-secret",
            approvals,
        ),
    )
    .expect("first insert");
    assert_eq!(result.revision, revision(1));
    assert_eq!(result.credential_version.expect("version").get(), 1);
    let expected = format!(
        "{{\"formatVersion\":1,\"kind\":\"mycode-host-auth\",\"revision\":1,\"credentials\":[{{\"serviceId\":\"alpha\",\"accountId\":\"one\",\"issuerId\":\"issuer\",\"authSchemaId\":\"schema\",\"credentialVersion\":1,\"state\":\"active\",\"secretBase64Url\":\"c2VudGluZWwtc2VjcmV0\"}}],\"grants\":[{},{}]}}\n",
        grant(
            "providers",
            "com.mycode.providers",
            "provider-pack",
            "read",
            "alpha",
            "one",
            DIGEST_A,
        ),
        grant(
            "web",
            "com.mycode.web",
            "web-pack",
            "write",
            "alpha",
            "one",
            DIGEST_A,
        )
    );
    assert_eq!(
        fs::read(absent_layout.host_auth_json()).expect("vault"),
        expected.as_bytes()
    );

    let zero_temp = TempDir::new().expect("temp");
    let zero_layout = layout(&zero_temp);
    ensure_home_layout(&zero_layout).expect("bootstrap");
    initialize_empty_host_vault(&zero_layout).expect("initialize");
    assert_preserved(
        &zero_layout,
        crate::host_vault::EMPTY_VAULT_BYTES,
        insert_command(
            ExpectedVaultState::Absent,
            "alpha",
            "one",
            b"secret",
            Vec::new(),
        ),
        ConfigErrorKind::RevisionConflict,
    );
}

#[test]
fn inserts_sort_at_every_position_and_tombstones_prevent_aba() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    ensure_home_layout(&layout).expect("bootstrap");
    for (expected, account) in [
        (ExpectedVaultState::Absent, "middle"),
        (ExpectedVaultState::Present(revision(1)), "alpha"),
        (ExpectedVaultState::Present(revision(2)), "zulu"),
    ] {
        persist_command(
            &layout,
            insert_command(expected, "service", account, b"secret", Vec::new()),
        )
        .expect("insert");
    }
    let bytes = fs::read(layout.host_auth_json()).expect("vault");
    let text = std::str::from_utf8(&bytes).expect("utf8");
    assert!(text.find("\"accountId\":\"alpha\"") < text.find("\"accountId\":\"middle\""));
    assert!(text.find("\"accountId\":\"middle\"") < text.find("\"accountId\":\"zulu\""));
    assert_preserved(
        &layout,
        &bytes,
        insert_command(
            ExpectedVaultState::Present(revision(3)),
            "service",
            "middle",
            b"other",
            Vec::new(),
        ),
        ConfigErrorKind::RevisionConflict,
    );

    let tombstone = document(8, &[credential("dead", "one", 4, "revoked")], &[]);
    install(&layout, &tombstone);
    assert_preserved(
        &layout,
        tombstone.as_bytes(),
        insert_command(
            ExpectedVaultState::Present(revision(8)),
            "dead",
            "one",
            b"resurrect",
            Vec::new(),
        ),
        ConfigErrorKind::RevisionConflict,
    );
}

#[test]
fn rotate_obeys_both_cas_levels_and_exhaustion() {
    let fixtures = [
        (
            document(5, &[credential("alpha", "one", 3, "active")], &[]),
            ExpectedVaultState::Present(revision(4)),
            "alpha",
            "one",
            3,
            ConfigErrorKind::RevisionConflict,
        ),
        (
            document(5, &[credential("alpha", "one", 3, "active")], &[]),
            ExpectedVaultState::Present(revision(5)),
            "alpha",
            "one",
            2,
            ConfigErrorKind::RevisionConflict,
        ),
        (
            document(5, &[credential("alpha", "one", 3, "active")], &[]),
            ExpectedVaultState::Present(revision(5)),
            "missing",
            "one",
            3,
            ConfigErrorKind::RevisionConflict,
        ),
        (
            document(5, &[credential("alpha", "one", 3, "revoked")], &[]),
            ExpectedVaultState::Present(revision(5)),
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
            ExpectedVaultState::Present(revision(i64::MAX as u64)),
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
            ExpectedVaultState::Present(revision(5)),
            "alpha",
            "one",
            i64::MAX as u64,
            ConfigErrorKind::RevisionExhausted,
        ),
    ];
    for (fixture, expected, service, account, expected_version, kind) in fixtures {
        let temp = TempDir::new().expect("temp");
        let layout = layout(&temp);
        install(&layout, &fixture);
        assert_preserved(
            &layout,
            fixture.as_bytes(),
            VaultCommand::Rotate {
                expected,
                service_id: service.into(),
                account_id: account.into(),
                expected_version: version(expected_version),
                secret: secret(b"replacement"),
            },
            kind,
        );
    }

    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    let fixture = document(5, &[credential("alpha", "one", 3, "active")], &[]);
    install(&layout, &fixture);
    let result = persist_command(
        &layout,
        VaultCommand::Rotate {
            expected: ExpectedVaultState::Present(revision(5)),
            service_id: "alpha".into(),
            account_id: "one".into(),
            expected_version: version(3),
            secret: secret(b"replacement"),
        },
    )
    .expect("rotate");
    assert_eq!(result.revision, revision(6));
    assert_eq!(result.credential_version.expect("version").get(), 4);
    let text = fs::read_to_string(layout.host_auth_json()).expect("vault");
    assert!(text.contains("\"credentialVersion\":4"));
    assert!(text.contains("\"secretBase64Url\":\"cmVwbGFjZW1lbnQ\""));
    assert!(text.contains("\"issuerId\":\"issuer\""));
}

#[test]
fn revoke_is_account_global_and_succeeds_at_total_capacity() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    let mut credentials = vec![credential("alpha", "active", 9, "active")];
    credentials
        .extend((0..255).map(|index| credential("zeta", &format!("r{index:03}"), 1, "revoked")));
    let grants = vec![
        grant(
            "providers",
            "com.mycode.providers",
            "pack-a",
            "read",
            "alpha",
            "active",
            DIGEST_A,
        ),
        grant(
            "web",
            "com.mycode.web",
            "pack-b",
            "write",
            "alpha",
            "active",
            DIGEST_B,
        ),
    ];
    let fixture = document(10, &credentials, &grants);
    install(&layout, &fixture);
    let result = persist_command(
        &layout,
        VaultCommand::Revoke {
            expected: ExpectedVaultState::Present(revision(10)),
            service_id: "alpha".into(),
            account_id: "active".into(),
            expected_version: version(9),
        },
    )
    .expect("revoke at capacity");
    assert_eq!(result.revision, revision(11));
    assert_eq!(result.credential_version.expect("version").get(), 10);
    let bytes = fs::read(layout.host_auth_json()).expect("vault");
    let parsed = parse_vault(&bytes).expect("valid result");
    assert_eq!(parsed.credentials.len(), 256);
    assert!(parsed.grants.is_empty());
    let text = std::str::from_utf8(&bytes).expect("utf8");
    assert!(text.contains("\"accountId\":\"active\",\"issuerId\":\"issuer\",\"authSchemaId\":\"schema\",\"credentialVersion\":10,\"state\":\"revoked\",\"secretBase64Url\":null"));
}

#[test]
fn bind_batch_is_atomic_sorted_and_descriptor_version_bound() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    let fixture = document(2, &[credential("alpha", "one", 2, "active")], &[]);
    install(&layout, &fixture);
    let result = persist_command(
        &layout,
        VaultCommand::Bind {
            expected: ExpectedVaultState::Present(revision(2)),
            approvals: vec![
                bind_approval(
                    ConsumerFamily::Usage,
                    "usage-pack",
                    "read",
                    "alpha",
                    "one",
                    2,
                ),
                bind_approval(
                    ConsumerFamily::Providers,
                    "provider-pack",
                    "read",
                    "alpha",
                    "one",
                    2,
                ),
            ],
        },
    )
    .expect("batch bind");
    assert_eq!(result.revision, revision(3));
    assert!(result.credential_version.is_none());
    let bytes = fs::read(layout.host_auth_json()).expect("vault");
    let text = std::str::from_utf8(&bytes).expect("utf8");
    assert!(
        text.find("\"consumerFamily\":\"providers\"") < text.find("\"consumerFamily\":\"usage\"")
    );

    for approvals in [
        Vec::new(),
        vec![
            bind_approval(ConsumerFamily::Web, "pack", "read", "alpha", "one", 2),
            bind_approval(ConsumerFamily::Web, "pack", "read", "alpha", "one", 2),
        ],
        vec![bind_approval(
            ConsumerFamily::Providers,
            "provider-pack",
            "read",
            "alpha",
            "one",
            2,
        )],
        vec![bind_approval(
            ConsumerFamily::Web,
            "pack",
            "read",
            "alpha",
            "one",
            1,
        )],
        vec![BindApproval {
            key: key(ConsumerFamily::Web, "pack", "read"),
            target: CredentialTarget {
                descriptor: CredentialDescriptor {
                    issuer_id: "different".into(),
                    ..descriptor("alpha", "one")
                },
                expected_version: version(2),
            },
            authority_digest: digest(DIGEST_A),
        }],
    ] {
        let kind = if approvals.is_empty() {
            ConfigErrorKind::AuthorityValidation
        } else {
            ConfigErrorKind::RevisionConflict
        };
        assert_preserved(
            &layout,
            &bytes,
            VaultCommand::Bind {
                expected: ExpectedVaultState::Present(revision(3)),
                approvals,
            },
            kind,
        );
    }
}

#[test]
fn bind_grant_cap_failure_preserves_the_document() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    let credentials = vec![credential("alpha", "one", 1, "active")];
    let grants = (0..1024)
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
        .collect::<Vec<_>>();
    let fixture = document(7, &credentials, &grants);
    install(&layout, &fixture);
    assert_preserved(
        &layout,
        fixture.as_bytes(),
        VaultCommand::Bind {
            expected: ExpectedVaultState::Present(revision(7)),
            approvals: vec![bind_approval(
                ConsumerFamily::Web,
                "pack",
                "extra",
                "alpha",
                "one",
                1,
            )],
        },
        ConfigErrorKind::AuthorityValidation,
    );
}

#[test]
fn rebind_and_unbind_require_complete_old_binding_and_keep_key_order() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    let credentials = vec![
        credential("alpha", "one", 1, "active"),
        credential("beta", "two", 4, "active"),
    ];
    let grants = vec![
        grant(
            "providers",
            "com.mycode.providers",
            "pack-a",
            "read",
            "alpha",
            "one",
            DIGEST_A,
        ),
        grant(
            "web",
            "com.mycode.web",
            "pack-b",
            "write",
            "alpha",
            "one",
            DIGEST_A,
        ),
    ];
    let fixture = document(4, &credentials, &grants);
    install(&layout, &fixture);
    let stale = VaultCommand::Rebind {
        expected: ExpectedVaultState::Present(revision(4)),
        key: key(ConsumerFamily::Providers, "pack-a", "read"),
        old: binding("alpha", "one", DIGEST_B),
        replacement: target("beta", "two", 4),
        authority_digest: digest(DIGEST_B),
    };
    assert_preserved(
        &layout,
        fixture.as_bytes(),
        stale,
        ConfigErrorKind::RevisionConflict,
    );
    for replacement in [
        target("beta", "two", 3),
        CredentialTarget {
            descriptor: CredentialDescriptor {
                issuer_id: "different".into(),
                ..descriptor("beta", "two")
            },
            expected_version: version(4),
        },
    ] {
        assert_preserved(
            &layout,
            fixture.as_bytes(),
            VaultCommand::Rebind {
                expected: ExpectedVaultState::Present(revision(4)),
                key: key(ConsumerFamily::Providers, "pack-a", "read"),
                old: binding("alpha", "one", DIGEST_A),
                replacement,
                authority_digest: digest(DIGEST_B),
            },
            ConfigErrorKind::RevisionConflict,
        );
    }

    persist_command(
        &layout,
        VaultCommand::Rebind {
            expected: ExpectedVaultState::Present(revision(4)),
            key: key(ConsumerFamily::Providers, "pack-a", "read"),
            old: binding("alpha", "one", DIGEST_A),
            replacement: target("beta", "two", 4),
            authority_digest: digest(DIGEST_B),
        },
    )
    .expect("rebind");
    let rebound = fs::read(layout.host_auth_json()).expect("rebound");
    let text = std::str::from_utf8(&rebound).expect("utf8");
    assert!(
        text.find("\"consumerFamily\":\"providers\"") < text.find("\"consumerFamily\":\"web\"")
    );
    assert!(text.contains("\"packId\":\"pack-a\",\"operationId\":\"read\",\"serviceId\":\"beta\",\"accountId\":\"two\""));

    assert_preserved(
        &layout,
        &rebound,
        VaultCommand::Unbind {
            expected: ExpectedVaultState::Present(revision(5)),
            key: key(ConsumerFamily::Providers, "pack-a", "read"),
            old: binding("alpha", "one", DIGEST_B),
        },
        ConfigErrorKind::RevisionConflict,
    );
    persist_command(
        &layout,
        VaultCommand::Unbind {
            expected: ExpectedVaultState::Present(revision(5)),
            key: key(ConsumerFamily::Providers, "pack-a", "read"),
            old: binding("beta", "two", DIGEST_B),
        },
    )
    .expect("unbind");
    let text = fs::read_to_string(layout.host_auth_json()).expect("vault");
    assert!(!text.contains("\"packId\":\"pack-a\""));
    assert!(text.contains("\"packId\":\"pack-b\""));
}

#[test]
fn malformed_and_stale_outer_fail_before_target_or_secret_inspection() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    let malformed = b"{malformed ATTACKER_SECRET}";
    install(&layout, std::str::from_utf8(malformed).expect("fixture"));
    let error = persist_command(
        &layout,
        VaultCommand::Rotate {
            expected: ExpectedVaultState::Absent,
            service_id: "missing".into(),
            account_id: "missing".into(),
            expected_version: version(1),
            secret: secret(b"RAW_SECRET_SENTINEL"),
        },
    )
    .expect_err("strict parse first");
    assert_eq!(error.kind(), ConfigErrorKind::InvalidJson);
    assert_eq!(
        fs::read(layout.host_auth_json()).expect("preserved"),
        malformed
    );
    for rendered in [format!("{error}"), format!("{error:?}")] {
        assert!(!rendered.contains("ATTACKER_SECRET"));
        assert!(!rendered.contains("RAW_SECRET_SENTINEL"));
    }

    let fixture = document(9, &[credential("alpha", "one", 1, "active")], &[]);
    install(&layout, &fixture);
    assert_preserved(
        &layout,
        fixture.as_bytes(),
        VaultCommand::Rotate {
            expected: ExpectedVaultState::Present(revision(8)),
            service_id: "missing".into(),
            account_id: "missing".into(),
            expected_version: version(1),
            secret: SecretInput::new(Zeroizing::new(Vec::new())),
        },
        ConfigErrorKind::RevisionConflict,
    );
}

#[test]
fn missing_noninsert_conflicts_without_creating_a_target_or_temp() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    ensure_home_layout(&layout).expect("bootstrap");
    let error = persist_command(
        &layout,
        VaultCommand::Rotate {
            expected: ExpectedVaultState::Absent,
            service_id: "alpha".into(),
            account_id: "one".into(),
            expected_version: version(1),
            secret: secret(b"secret"),
        },
    )
    .expect_err("missing rotate");
    assert_eq!(error.kind(), ConfigErrorKind::RevisionConflict);
    assert!(!layout.host_auth_json().exists());
    let names = fs::read_dir(layout.host_dir())
        .expect("host")
        .map(|entry| entry.expect("entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(names, ["auth.json.lock"]);
}

#[test]
fn concurrent_same_outer_and_target_has_one_winner() {
    let temp = TempDir::new().expect("temp");
    let layout = Arc::new(layout(&temp));
    let fixture = document(3, &[credential("alpha", "one", 2, "active")], &[]);
    install(&layout, &fixture);
    let barrier = Arc::new(Barrier::new(8));
    let handles = (0..8)
        .map(|index| {
            let layout = Arc::clone(&layout);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                persist_command(
                    &layout,
                    VaultCommand::Rotate {
                        expected: ExpectedVaultState::Present(revision(3)),
                        service_id: "alpha".into(),
                        account_id: "one".into(),
                        expected_version: version(2),
                        secret: secret(format!("secret-{index}").as_bytes()),
                    },
                )
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread"))
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| result
                .as_ref()
                .is_err_and(|error| error.kind() == ConfigErrorKind::RevisionConflict))
            .count(),
        7
    );
}

#[test]
fn maximum_secret_uses_bounded_serializer_without_diagnostic_leaks() {
    let temp = TempDir::new().expect("temp");
    let layout = layout(&temp);
    ensure_home_layout(&layout).expect("bootstrap");
    let raw = vec![0xa5_u8; MAX_SECRET_BYTES];
    persist_command(
        &layout,
        insert_command(ExpectedVaultState::Absent, "alpha", "one", &raw, Vec::new()),
    )
    .expect("maximum secret");
    let bytes = fs::read(layout.host_auth_json()).expect("vault");
    assert!(bytes.len() < crate::host_vault::MAX_HOST_VAULT_BYTES);
    parse_vault(&bytes).expect("strict result");

    let before = bytes.clone();
    let sentinel = b"RAW_SECRET_SENTINEL_77";
    let encoded_sentinel = URL_SAFE_NO_PAD.encode(sentinel);
    let mut oversized = sentinel.to_vec();
    oversized.resize(MAX_SECRET_BYTES + 1, b'x');
    let error = persist_command(
        &layout,
        VaultCommand::Rotate {
            expected: ExpectedVaultState::Present(revision(1)),
            service_id: "alpha".into(),
            account_id: "one".into(),
            expected_version: version(1),
            secret: SecretInput::new(Zeroizing::new(oversized)),
        },
    )
    .expect_err("oversized secret");
    assert_eq!(error.kind(), ConfigErrorKind::AuthorityValidation);
    assert_eq!(
        fs::read(layout.host_auth_json()).expect("preserved"),
        before
    );
    for rendered in [format!("{error}"), format!("{error:?}")] {
        assert!(
            !rendered
                .as_bytes()
                .windows(sentinel.len())
                .any(|part| part == sentinel)
        );
        assert!(!rendered.contains(&encoded_sentinel));
    }
}
