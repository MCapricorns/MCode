//! Applies private credential and grant commands under one Host-vault transaction.

use std::borrow::Cow;

use zeroize::Zeroizing;

use super::model::{
    ConsumerFamily, Credential, CredentialState, CredentialVersion, Grant, MAX_ACTIVE_CREDENTIALS,
    MAX_CREDENTIALS, MAX_GRANTS, Secret, VaultDocument, validate_document,
    validate_grant_coordinates, validate_local_id,
};
use super::{
    HOST_VAULT_FORMAT_VERSION, HOST_VAULT_KIND, HOST_VAULT_PATH, MAX_HOST_VAULT_BYTES,
    VaultRevision,
};
use crate::authority::Sha256Digest;
use crate::secure_fs::owned_file::locked_update_secret_owned_file;
use crate::{ConfigError, ConfigErrorKind, HomeLayout};

#[derive(Clone, Copy)]
enum ExpectedVaultState {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "awaits the future signed-identity mutation API")
    )]
    Absent,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "awaits the future signed-identity mutation API")
    )]
    Present(VaultRevision),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MutationResult {
    revision: VaultRevision,
    credential_version: Option<CredentialVersion>,
}

struct SecretInput(Zeroizing<Vec<u8>>);

impl SecretInput {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "awaits the future signed-identity mutation API")
    )]
    fn new(bytes: Zeroizing<Vec<u8>>) -> Self {
        Self(bytes)
    }
}

struct CredentialDescriptor {
    service_id: String,
    account_id: String,
    issuer_id: String,
    auth_schema_id: String,
}

struct CredentialTarget {
    descriptor: CredentialDescriptor,
    expected_version: CredentialVersion,
}

struct GrantKey {
    consumer_family: ConsumerFamily,
    pack_id: String,
    operation_id: String,
}

struct GrantApproval {
    key: GrantKey,
    authority_digest: Sha256Digest,
}

struct BindApproval {
    key: GrantKey,
    target: CredentialTarget,
    authority_digest: Sha256Digest,
}

struct PersistedBinding {
    service_id: String,
    account_id: String,
    authority_digest: Sha256Digest,
}

enum VaultCommand {
    InitializeEmpty,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "awaits the future signed-identity mutation API")
    )]
    Insert {
        expected: ExpectedVaultState,
        descriptor: CredentialDescriptor,
        secret: SecretInput,
        approvals: Vec<GrantApproval>,
    },
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "awaits the future signed-identity mutation API")
    )]
    Rotate {
        expected: ExpectedVaultState,
        service_id: String,
        account_id: String,
        expected_version: CredentialVersion,
        secret: SecretInput,
    },
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "awaits the future signed-identity mutation API")
    )]
    Revoke {
        expected: ExpectedVaultState,
        service_id: String,
        account_id: String,
        expected_version: CredentialVersion,
    },
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "awaits the future signed-identity mutation API")
    )]
    Bind {
        expected: ExpectedVaultState,
        approvals: Vec<BindApproval>,
    },
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "awaits the future signed-identity mutation API")
    )]
    Rebind {
        expected: ExpectedVaultState,
        key: GrantKey,
        old: PersistedBinding,
        replacement: CredentialTarget,
        authority_digest: Sha256Digest,
    },
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "awaits the future signed-identity mutation API")
    )]
    Unbind {
        expected: ExpectedVaultState,
        key: GrantKey,
        old: PersistedBinding,
    },
}

impl VaultCommand {
    fn expected(&self) -> Option<ExpectedVaultState> {
        match self {
            Self::InitializeEmpty => None,
            Self::Insert { expected, .. }
            | Self::Rotate { expected, .. }
            | Self::Revoke { expected, .. }
            | Self::Bind { expected, .. }
            | Self::Rebind { expected, .. }
            | Self::Unbind { expected, .. } => Some(*expected),
        }
    }

    fn can_create(&self) -> bool {
        matches!(self, Self::InitializeEmpty | Self::Insert { .. })
    }
}

pub(super) fn initialize_empty(home: &HomeLayout) -> Result<VaultRevision, ConfigError> {
    persist_command(home, VaultCommand::InitializeEmpty).map(|result| result.revision)
}

fn persist_command(
    home: &HomeLayout,
    command: VaultCommand,
) -> Result<MutationResult, ConfigError> {
    let target = home.host_auth_json();
    let mut result = None;
    locked_update_secret_owned_file(home, HOST_VAULT_PATH, MAX_HOST_VAULT_BYTES, |current| {
        let mut document = match current {
            Some(bytes) => super::model::parse_vault(bytes)?,
            None => empty_document(),
        };
        if let Some(expected) = command.expected() {
            compare_outer(current.is_some(), document.revision(), expected)?;
        } else if current.is_some() {
            return Err(conflict());
        }
        if current.is_none() && !command.can_create() {
            return Err(conflict());
        }
        let applied = apply_command(&mut document, command)?;
        validate_document(&document)?;
        let replacement = super::model::serialize_document(&document)?;
        result = Some(applied);
        Ok(replacement)
    })
    .map_err(|error| error.with_path(&target))?;
    result.ok_or_else(|| ConfigError::new(ConfigErrorKind::Serialization))
}

fn empty_document() -> VaultDocument<'static> {
    VaultDocument {
        format_version: HOST_VAULT_FORMAT_VERSION,
        kind: Cow::Borrowed(HOST_VAULT_KIND),
        revision: 0,
        credentials: Vec::new(),
        grants: Vec::new(),
    }
}

fn compare_outer(
    present: bool,
    actual: VaultRevision,
    expected: ExpectedVaultState,
) -> Result<(), ConfigError> {
    let matches = match expected {
        ExpectedVaultState::Absent => !present,
        ExpectedVaultState::Present(expected) => present && actual == expected,
    };
    if matches { Ok(()) } else { Err(conflict()) }
}

fn apply_command(
    document: &mut VaultDocument<'_>,
    command: VaultCommand,
) -> Result<MutationResult, ConfigError> {
    match command {
        VaultCommand::InitializeEmpty => Ok(MutationResult {
            revision: VaultRevision::EMPTY,
            credential_version: None,
        }),
        VaultCommand::Insert {
            descriptor,
            secret,
            approvals,
            ..
        } => insert(document, descriptor, secret, approvals),
        VaultCommand::Rotate {
            service_id,
            account_id,
            expected_version,
            secret,
            ..
        } => rotate(document, &service_id, &account_id, expected_version, secret),
        VaultCommand::Revoke {
            service_id,
            account_id,
            expected_version,
            ..
        } => revoke(document, &service_id, &account_id, expected_version),
        VaultCommand::Bind { approvals, .. } => bind(document, approvals),
        VaultCommand::Rebind {
            key,
            old,
            replacement,
            authority_digest,
            ..
        } => rebind(document, key, old, replacement, authority_digest),
        VaultCommand::Unbind { key, old, .. } => unbind(document, key, old),
    }
}

fn insert(
    document: &mut VaultDocument<'_>,
    descriptor: CredentialDescriptor,
    secret: SecretInput,
    approvals: Vec<GrantApproval>,
) -> Result<MutationResult, ConfigError> {
    if approvals.len() > MAX_GRANTS {
        return Err(authority_error());
    }
    if find_credential(document, &descriptor.service_id, &descriptor.account_id).is_ok() {
        return Err(conflict());
    }
    ensure_new_grant_keys(document, approvals.iter().map(|approval| &approval.key))?;
    validate_descriptor(&descriptor)?;
    let secret = Secret::from_input(secret.0)?;
    for approval in &approvals {
        validate_grant_input(&approval.key)?;
    }
    if approvals.len() > MAX_GRANTS - document.grants.len()
        || active_count(document) >= MAX_ACTIVE_CREDENTIALS
        || document.credentials.len() >= MAX_CREDENTIALS
    {
        return Err(authority_error());
    }
    let revision = next_revision(document.revision)?;
    let service_id = descriptor.service_id;
    let account_id = descriptor.account_id;
    document.credentials.push(Credential {
        service_id: Cow::Owned(service_id.clone()),
        account_id: Cow::Owned(account_id.clone()),
        issuer_id: Cow::Owned(descriptor.issuer_id),
        auth_schema_id: Cow::Owned(descriptor.auth_schema_id),
        credential_version: CredentialVersion::FIRST,
        state: CredentialState::Active,
        secret_base64_url: Some(secret),
    });
    for approval in approvals {
        document.grants.push(owned_grant(
            approval.key,
            service_id.clone(),
            account_id.clone(),
            approval.authority_digest,
        ));
    }
    finish_mutation(document, revision, Some(CredentialVersion::FIRST))
}

fn rotate(
    document: &mut VaultDocument<'_>,
    service_id: &str,
    account_id: &str,
    expected_version: CredentialVersion,
    secret: SecretInput,
) -> Result<MutationResult, ConfigError> {
    let index = active_credential_index(document, service_id, account_id, expected_version)?;
    let secret = Secret::from_input(secret.0)?;
    let credential_version = document.credentials[index]
        .credential_version
        .checked_increment()?;
    let revision = next_revision(document.revision)?;
    document.credentials[index].credential_version = credential_version;
    document.credentials[index].secret_base64_url = Some(secret);
    finish_mutation(document, revision, Some(credential_version))
}

fn revoke(
    document: &mut VaultDocument<'_>,
    service_id: &str,
    account_id: &str,
    expected_version: CredentialVersion,
) -> Result<MutationResult, ConfigError> {
    let index = active_credential_index(document, service_id, account_id, expected_version)?;
    let credential_version = document.credentials[index]
        .credential_version
        .checked_increment()?;
    let revision = next_revision(document.revision)?;
    document.credentials[index].credential_version = credential_version;
    document.credentials[index].state = CredentialState::Revoked;
    document.credentials[index].secret_base64_url = None;
    document
        .grants
        .retain(|grant| grant.service_id != service_id || grant.account_id != account_id);
    finish_mutation(document, revision, Some(credential_version))
}

fn bind(
    document: &mut VaultDocument<'_>,
    approvals: Vec<BindApproval>,
) -> Result<MutationResult, ConfigError> {
    if approvals.is_empty() || approvals.len() > MAX_GRANTS {
        return Err(authority_error());
    }
    ensure_new_grant_keys(document, approvals.iter().map(|approval| &approval.key))?;
    for approval in &approvals {
        exact_active_credential(document, &approval.target)?;
    }
    for approval in &approvals {
        validate_grant_input(&approval.key)?;
        validate_descriptor(&approval.target.descriptor)?;
    }
    if approvals.len() > MAX_GRANTS - document.grants.len() {
        return Err(authority_error());
    }
    let revision = next_revision(document.revision)?;
    for approval in approvals {
        document.grants.push(owned_grant(
            approval.key,
            approval.target.descriptor.service_id,
            approval.target.descriptor.account_id,
            approval.authority_digest,
        ));
    }
    finish_mutation(document, revision, None)
}

fn rebind(
    document: &mut VaultDocument<'_>,
    key: GrantKey,
    old: PersistedBinding,
    replacement: CredentialTarget,
    authority_digest: Sha256Digest,
) -> Result<MutationResult, ConfigError> {
    let grant_index = exact_grant_index(document, &key, &old)?;
    exact_active_credential(document, &replacement)?;
    validate_grant_input(&key)?;
    validate_binding(&old)?;
    validate_descriptor(&replacement.descriptor)?;
    let revision = next_revision(document.revision)?;
    let grant = &mut document.grants[grant_index];
    grant.service_id = Cow::Owned(replacement.descriptor.service_id);
    grant.account_id = Cow::Owned(replacement.descriptor.account_id);
    grant.authority_digest = Cow::Owned(authority_digest.into_string());
    finish_mutation(document, revision, None)
}

fn unbind(
    document: &mut VaultDocument<'_>,
    key: GrantKey,
    old: PersistedBinding,
) -> Result<MutationResult, ConfigError> {
    let index = exact_grant_index(document, &key, &old)?;
    validate_grant_input(&key)?;
    validate_binding(&old)?;
    let revision = next_revision(document.revision)?;
    document.grants.remove(index);
    finish_mutation(document, revision, None)
}

fn finish_mutation(
    document: &mut VaultDocument<'_>,
    revision: VaultRevision,
    credential_version: Option<CredentialVersion>,
) -> Result<MutationResult, ConfigError> {
    document.revision = revision.get();
    document.credentials.sort_by(|left, right| {
        (&left.service_id, &left.account_id).cmp(&(&right.service_id, &right.account_id))
    });
    document
        .grants
        .sort_by(|left, right| grant_tuple(left).cmp(&grant_tuple(right)));
    Ok(MutationResult {
        revision,
        credential_version,
    })
}

fn next_revision(current: u64) -> Result<VaultRevision, ConfigError> {
    current
        .checked_add(1)
        .filter(|value| *value <= i64::MAX as u64)
        .map(VaultRevision)
        .ok_or_else(|| ConfigError::new(ConfigErrorKind::RevisionExhausted))
}

fn find_credential(
    document: &VaultDocument<'_>,
    service_id: &str,
    account_id: &str,
) -> Result<usize, usize> {
    document.credentials.binary_search_by(|credential| {
        (
            credential.service_id.as_ref(),
            credential.account_id.as_ref(),
        )
            .cmp(&(service_id, account_id))
    })
}

fn active_credential_index(
    document: &VaultDocument<'_>,
    service_id: &str,
    account_id: &str,
    expected_version: CredentialVersion,
) -> Result<usize, ConfigError> {
    let index = find_credential(document, service_id, account_id).map_err(|_| conflict())?;
    let credential = &document.credentials[index];
    if credential.state != CredentialState::Active
        || credential.credential_version != expected_version
    {
        return Err(conflict());
    }
    Ok(index)
}

fn exact_active_credential(
    document: &VaultDocument<'_>,
    target: &CredentialTarget,
) -> Result<usize, ConfigError> {
    let descriptor = &target.descriptor;
    let index = active_credential_index(
        document,
        &descriptor.service_id,
        &descriptor.account_id,
        target.expected_version,
    )?;
    let credential = &document.credentials[index];
    if credential.issuer_id != descriptor.issuer_id
        || credential.auth_schema_id != descriptor.auth_schema_id
    {
        return Err(conflict());
    }
    Ok(index)
}

fn ensure_new_grant_keys<'a>(
    document: &VaultDocument<'_>,
    keys: impl Iterator<Item = &'a GrantKey>,
) -> Result<(), ConfigError> {
    let mut pending = keys.collect::<Vec<_>>();
    pending.sort_by(|left, right| input_grant_tuple(left).cmp(&input_grant_tuple(right)));
    if pending
        .windows(2)
        .any(|pair| input_grant_tuple(pair[0]) == input_grant_tuple(pair[1]))
        || pending.iter().any(|key| find_grant(document, key).is_ok())
    {
        return Err(conflict());
    }
    Ok(())
}

fn find_grant(document: &VaultDocument<'_>, key: &GrantKey) -> Result<usize, usize> {
    document
        .grants
        .binary_search_by(|grant| grant_tuple(grant).cmp(&input_grant_tuple(key)))
}

fn exact_grant_index(
    document: &VaultDocument<'_>,
    key: &GrantKey,
    old: &PersistedBinding,
) -> Result<usize, ConfigError> {
    let index = find_grant(document, key).map_err(|_| conflict())?;
    let grant = &document.grants[index];
    if grant.service_id != old.service_id
        || grant.account_id != old.account_id
        || grant.authority_digest != old.authority_digest.as_str()
    {
        return Err(conflict());
    }
    Ok(index)
}

fn validate_descriptor(descriptor: &CredentialDescriptor) -> Result<(), ConfigError> {
    validate_local_id(&descriptor.service_id)?;
    validate_local_id(&descriptor.account_id)?;
    validate_local_id(&descriptor.issuer_id)?;
    validate_local_id(&descriptor.auth_schema_id)
}

fn validate_grant_input(key: &GrantKey) -> Result<(), ConfigError> {
    validate_grant_coordinates(
        key.consumer_family,
        key.consumer_family.plugin_family().id(),
        &key.pack_id,
        &key.operation_id,
    )
}

fn validate_binding(binding: &PersistedBinding) -> Result<(), ConfigError> {
    validate_local_id(&binding.service_id)?;
    validate_local_id(&binding.account_id)
}

fn owned_grant(
    key: GrantKey,
    service_id: String,
    account_id: String,
    authority_digest: Sha256Digest,
) -> Grant<'static> {
    Grant {
        consumer_family: key.consumer_family,
        manager_id: Cow::Owned(key.consumer_family.plugin_family().id().to_owned()),
        pack_id: Cow::Owned(key.pack_id),
        operation_id: Cow::Owned(key.operation_id),
        service_id: Cow::Owned(service_id),
        account_id: Cow::Owned(account_id),
        authority_digest: Cow::Owned(authority_digest.into_string()),
    }
}

fn active_count(document: &VaultDocument<'_>) -> usize {
    document
        .credentials
        .iter()
        .filter(|credential| credential.state == CredentialState::Active)
        .count()
}

fn grant_tuple<'a>(grant: &'a Grant<'a>) -> (&'a str, &'a str, &'a str, &'a str) {
    (
        grant.consumer_family.sort_key(),
        &grant.manager_id,
        &grant.pack_id,
        &grant.operation_id,
    )
}

fn input_grant_tuple(key: &GrantKey) -> (&str, &str, &str, &str) {
    (
        key.consumer_family.sort_key(),
        key.consumer_family.plugin_family().id(),
        &key.pack_id,
        &key.operation_id,
    )
}

fn conflict() -> ConfigError {
    ConfigError::new(ConfigErrorKind::RevisionConflict)
}

fn authority_error() -> ConfigError {
    ConfigError::new(ConfigErrorKind::AuthorityValidation)
}

#[cfg(test)]
#[path = "reducer_boundary_tests.rs"]
mod boundary_tests;
#[cfg(test)]
#[path = "reducer_test_support.rs"]
mod test_support;
#[cfg(test)]
#[path = "reducer_tests.rs"]
mod tests;
