//! Provides shared fixtures for private Host-vault reducer tests.

use std::fs;

use tempfile::TempDir;
use zeroize::Zeroizing;

use super::{
    BindApproval, CredentialDescriptor, CredentialTarget, ExpectedVaultState, GrantApproval,
    GrantKey, PersistedBinding, SecretInput, VaultCommand, persist_command,
};
use crate::authority::Sha256Digest;
use crate::host_vault::model::{ConsumerFamily, CredentialVersion};
use crate::host_vault::{VaultRevision, relative_path};
use crate::secure_fs::owned_file::replace_owned_file;
use crate::{ConfigErrorKind, HomeLayout};

pub(super) const DIGEST_A: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(super) const DIGEST_B: &str =
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

pub(super) fn layout(temp: &TempDir) -> HomeLayout {
    HomeLayout::from_root(temp.path().join("home")).expect("layout")
}

pub(super) fn revision(value: u64) -> VaultRevision {
    VaultRevision::new(value).expect("vault revision")
}

pub(super) fn version(value: u64) -> CredentialVersion {
    CredentialVersion::new(value).expect("credential version")
}

pub(super) fn descriptor(service: &str, account: &str) -> CredentialDescriptor {
    CredentialDescriptor {
        service_id: service.into(),
        account_id: account.into(),
        issuer_id: "issuer".into(),
        auth_schema_id: "schema".into(),
    }
}

pub(super) fn target(service: &str, account: &str, expected_version: u64) -> CredentialTarget {
    CredentialTarget {
        descriptor: descriptor(service, account),
        expected_version: version(expected_version),
    }
}

pub(super) fn key(family: ConsumerFamily, pack: &str, operation: &str) -> GrantKey {
    GrantKey {
        consumer_family: family,
        pack_id: pack.into(),
        operation_id: operation.into(),
    }
}

pub(super) fn digest(value: &str) -> Sha256Digest {
    Sha256Digest::parse(value).expect("digest")
}

pub(super) fn secret(value: &[u8]) -> SecretInput {
    SecretInput::new(Zeroizing::new(value.to_vec()))
}

pub(super) fn approval(family: ConsumerFamily, pack: &str, operation: &str) -> GrantApproval {
    GrantApproval {
        key: key(family, pack, operation),
        authority_digest: digest(DIGEST_A),
    }
}

pub(super) fn bind_approval(
    family: ConsumerFamily,
    pack: &str,
    operation: &str,
    service: &str,
    account: &str,
    expected_version: u64,
) -> BindApproval {
    BindApproval {
        key: key(family, pack, operation),
        target: target(service, account, expected_version),
        authority_digest: digest(DIGEST_A),
    }
}

pub(super) fn binding(service: &str, account: &str, digest_value: &str) -> PersistedBinding {
    PersistedBinding {
        service_id: service.into(),
        account_id: account.into(),
        authority_digest: digest(digest_value),
    }
}

pub(super) fn credential(
    service: &str,
    account: &str,
    credential_version: u64,
    state: &str,
) -> String {
    let secret = if state == "active" { "\"YQ\"" } else { "null" };
    format!(
        "{{\"serviceId\":\"{service}\",\"accountId\":\"{account}\",\"issuerId\":\"issuer\",\"authSchemaId\":\"schema\",\"credentialVersion\":{credential_version},\"state\":\"{state}\",\"secretBase64Url\":{secret}}}"
    )
}

pub(super) fn grant(
    family: &str,
    manager: &str,
    pack: &str,
    operation: &str,
    service: &str,
    account: &str,
    digest_value: &str,
) -> String {
    format!(
        "{{\"consumerFamily\":\"{family}\",\"managerId\":\"{manager}\",\"packId\":\"{pack}\",\"operationId\":\"{operation}\",\"serviceId\":\"{service}\",\"accountId\":\"{account}\",\"authorityDigest\":\"{digest_value}\"}}"
    )
}

pub(super) fn document(revision: u64, credentials: &[String], grants: &[String]) -> String {
    format!(
        "{{\"formatVersion\":1,\"kind\":\"mcode-host-auth\",\"revision\":{revision},\"credentials\":[{}],\"grants\":[{}]}}\n",
        credentials.join(","),
        grants.join(",")
    )
}

pub(super) fn install(layout: &HomeLayout, bytes: &str) {
    replace_owned_file(layout, relative_path(), bytes.as_bytes()).expect("fixture");
}

pub(super) fn insert_command(
    expected: ExpectedVaultState,
    service: &str,
    account: &str,
    secret_bytes: &[u8],
    approvals: Vec<GrantApproval>,
) -> VaultCommand {
    VaultCommand::Insert {
        expected,
        descriptor: descriptor(service, account),
        secret: secret(secret_bytes),
        approvals,
    }
}

pub(super) fn assert_preserved(
    layout: &HomeLayout,
    expected: &[u8],
    command: VaultCommand,
    kind: ConfigErrorKind,
) {
    let error = persist_command(layout, command).expect_err("command must fail");
    assert_eq!(error.kind(), kind);
    assert_eq!(
        fs::read(layout.host_auth_json()).expect("preserved"),
        expected
    );
    let names = fs::read_dir(layout.host_dir())
        .expect("host directory")
        .map(|entry| entry.expect("entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2, "failure created a temporary file");
    assert!(names.contains(&"auth.json".into()));
    assert!(names.contains(&"auth.json.lock".into()));
}
