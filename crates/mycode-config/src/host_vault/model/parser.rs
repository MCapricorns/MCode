//! Deserializes Host-vault JSON without retaining attacker-controlled diagnostics.

use std::borrow::Cow;
use std::fmt::{self, Formatter};
use std::marker::PhantomData;

use serde::de::{self, DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use zeroize::Zeroize;

use super::{
    ConsumerFamily, Credential, CredentialState, CredentialVersion, Grant, Secret, VaultDocument,
    classify_json_error, decode_secret, validate_document,
};
use crate::ConfigError;

mod callbacks;

use callbacks::{
    reject_map, reject_non_u64_scalars, reject_null_and_wrappers, reject_numbers, reject_sequence,
    reject_strings,
};

pub(super) fn deserialize_vault(bytes: &[u8]) -> Result<VaultDocument<'_>, ConfigError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let document = VaultDocument::deserialize(&mut deserializer).map_err(classify_json_error)?;
    deserializer.end().map_err(classify_json_error)?;
    validate_document(&document)?;
    Ok(document)
}

struct BorrowedString<'a>(&'a str);

impl<'de> Deserialize<'de> for BorrowedString<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(BorrowedStringVisitor)
    }
}

struct BorrowedStringVisitor;

impl<'de> Visitor<'de> for BorrowedStringVisitor {
    type Value = BorrowedString<'de>;

    fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("an unescaped borrowed vault string")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(BorrowedString(value))
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom("non-borrowed vault strings are rejected"))
    }

    fn visit_string<E>(self, mut value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        value.zeroize();
        Err(E::custom("owned vault strings are rejected"))
    }

    reject_numbers!();
    reject_null_and_wrappers!();
    reject_sequence!();
    reject_map!();
}

struct U64Value(u64);

impl<'de> Deserialize<'de> for U64Value {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(U64Visitor)
    }
}

struct U64Visitor;

impl<'de> Visitor<'de> for U64Visitor {
    type Value = U64Value;

    fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("a nonnegative vault integer")
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(U64Value(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        u64::try_from(value)
            .map(U64Value)
            .map_err(|_| E::custom("vault integer is out of range"))
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom("vault integer has wrong type"))
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom("non-borrowed vault strings are rejected"))
    }

    fn visit_string<E>(self, mut value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        value.zeroize();
        Err(E::custom("owned vault strings are rejected"))
    }

    reject_non_u64_scalars!();
    reject_null_and_wrappers!();
    reject_sequence!();
    reject_map!();
}

struct FieldSeed<F>(fn(&str) -> Option<F>);

enum FieldName<F> {
    Known(F),
    Unknown,
}

impl<'de, F> DeserializeSeed<'de> for FieldSeed<F> {
    type Value = FieldName<F>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(FieldVisitor(self.0))
    }
}

struct FieldVisitor<F>(fn(&str) -> Option<F>);

impl<'de, F> Visitor<'de> for FieldVisitor<F> {
    type Value = FieldName<F>;

    fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("an unescaped vault member name")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok((self.0)(value).map_or(FieldName::Unknown, FieldName::Known))
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom("non-borrowed vault member names are rejected"))
    }

    fn visit_string<E>(self, mut value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        value.zeroize();
        Err(E::custom("owned vault member names are rejected"))
    }

    reject_numbers!();
    reject_null_and_wrappers!();
    reject_sequence!();
    reject_map!();
}

struct VaultArray<T>(Vec<T>);

impl<'de, T> Deserialize<'de> for VaultArray<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ArrayVisitor(PhantomData))
    }
}

struct ArrayVisitor<T>(PhantomData<T>);

impl<'de, T> Visitor<'de> for ArrayVisitor<T>
where
    T: Deserialize<'de>,
{
    type Value = VaultArray<T>;

    fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("a vault array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element()? {
            values.push(value);
        }
        Ok(VaultArray(values))
    }

    reject_numbers!();
    reject_strings!();
    reject_null_and_wrappers!();
    reject_map!();
}

impl<'de> Deserialize<'de> for CredentialState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(CredentialStateVisitor)
    }
}

struct CredentialStateVisitor;

impl<'de> Visitor<'de> for CredentialStateVisitor {
    type Value = CredentialState;

    fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("a credential state")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match value {
            "active" => Ok(CredentialState::Active),
            "revoked" => Ok(CredentialState::Revoked),
            _ => Err(E::custom("credential state is unknown")),
        }
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom("non-borrowed vault strings are rejected"))
    }

    fn visit_string<E>(self, mut value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        value.zeroize();
        Err(E::custom("owned vault strings are rejected"))
    }

    reject_numbers!();
    reject_null_and_wrappers!();
    reject_sequence!();
    reject_map!();
}

impl<'de> Deserialize<'de> for ConsumerFamily {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ConsumerFamilyVisitor)
    }
}

struct ConsumerFamilyVisitor;

impl<'de> Visitor<'de> for ConsumerFamilyVisitor {
    type Value = ConsumerFamily;

    fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("a consumer family")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match value {
            "providers" => Ok(ConsumerFamily::Providers),
            "web" => Ok(ConsumerFamily::Web),
            "usage" => Ok(ConsumerFamily::Usage),
            _ => Err(E::custom("consumer family is unknown")),
        }
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom("non-borrowed vault strings are rejected"))
    }

    fn visit_string<E>(self, mut value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        value.zeroize();
        Err(E::custom("owned vault strings are rejected"))
    }

    reject_numbers!();
    reject_null_and_wrappers!();
    reject_sequence!();
    reject_map!();
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(SecretVisitor)
    }
}

struct SecretVisitor;

impl<'de> Visitor<'de> for SecretVisitor {
    type Value = Secret;

    fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("an unescaped canonical Base64URL secret")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        decode_secret(value).map_err(|()| E::custom("invalid secret encoding"))
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom("non-borrowed secret strings are rejected"))
    }

    fn visit_string<E>(self, mut value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        value.zeroize();
        Err(E::custom("owned secret strings are rejected"))
    }

    reject_numbers!();
    reject_null_and_wrappers!();
    reject_sequence!();
    reject_map!();
}

struct OptionalSecret(Option<Secret>);

impl<'de> Deserialize<'de> for OptionalSecret {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(OptionalSecretVisitor)
    }
}

struct OptionalSecretVisitor;

impl<'de> Visitor<'de> for OptionalSecretVisitor {
    type Value = OptionalSecret;

    fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("a secret or null")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(OptionalSecret(None))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(OptionalSecret(None))
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        decode_secret(value)
            .map(|secret| OptionalSecret(Some(secret)))
            .map_err(|()| E::custom("invalid secret encoding"))
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom("non-borrowed secret strings are rejected"))
    }

    fn visit_string<E>(self, mut value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        value.zeroize();
        Err(E::custom("owned secret strings are rejected"))
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        Secret::deserialize(deserializer).map(|secret| OptionalSecret(Some(secret)))
    }

    fn visit_newtype_struct<D>(self, _deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        Err(D::Error::custom("secret has wrong type"))
    }

    reject_numbers!();
    reject_sequence!();
    reject_map!();
}

#[derive(Clone, Copy)]
enum DocumentField {
    FormatVersion,
    Kind,
    Revision,
    Credentials,
    Grants,
}

fn document_field(value: &str) -> Option<DocumentField> {
    match value {
        "formatVersion" => Some(DocumentField::FormatVersion),
        "kind" => Some(DocumentField::Kind),
        "revision" => Some(DocumentField::Revision),
        "credentials" => Some(DocumentField::Credentials),
        "grants" => Some(DocumentField::Grants),
        _ => None,
    }
}

impl<'de> Deserialize<'de> for VaultDocument<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(DocumentVisitor)
    }
}

struct DocumentVisitor;

impl<'de> Visitor<'de> for DocumentVisitor {
    type Value = VaultDocument<'de>;

    fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Host-vault object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut format_version = None;
        let mut kind = None;
        let mut revision = None;
        let mut credentials = None;
        let mut grants = None;
        while let Some(field) = map.next_key_seed(FieldSeed(document_field))? {
            match field {
                FieldName::Known(DocumentField::FormatVersion) => {
                    read_once_with(&mut format_version, &mut map, |value: U64Value| value.0)?;
                }
                FieldName::Known(DocumentField::Kind) => {
                    read_once_with(&mut kind, &mut map, |value: BorrowedString<'de>| value.0)?;
                }
                FieldName::Known(DocumentField::Revision) => {
                    read_once_with(&mut revision, &mut map, |value: U64Value| value.0)?;
                }
                FieldName::Known(DocumentField::Credentials) => {
                    read_once_with(
                        &mut credentials,
                        &mut map,
                        |value: VaultArray<Credential<'de>>| value.0,
                    )?;
                }
                FieldName::Known(DocumentField::Grants) => {
                    read_once_with(&mut grants, &mut map, |value: VaultArray<Grant<'de>>| {
                        value.0
                    })?;
                }
                FieldName::Unknown => return Err(A::Error::custom("unknown Host-vault member")),
            }
        }
        let format_version = u32::try_from(required(format_version)?)
            .map_err(|_| A::Error::custom("vault integer is out of range"))?;
        Ok(VaultDocument {
            format_version,
            kind: Cow::Borrowed(required(kind)?),
            revision: required(revision)?,
            credentials: required(credentials)?,
            grants: required(grants)?,
        })
    }

    reject_numbers!();
    reject_strings!();
    reject_null_and_wrappers!();
    reject_sequence!();
}

#[derive(Clone, Copy)]
enum CredentialField {
    ServiceId,
    AccountId,
    IssuerId,
    AuthSchemaId,
    CredentialVersion,
    State,
    SecretBase64Url,
}

fn credential_field(value: &str) -> Option<CredentialField> {
    match value {
        "serviceId" => Some(CredentialField::ServiceId),
        "accountId" => Some(CredentialField::AccountId),
        "issuerId" => Some(CredentialField::IssuerId),
        "authSchemaId" => Some(CredentialField::AuthSchemaId),
        "credentialVersion" => Some(CredentialField::CredentialVersion),
        "state" => Some(CredentialField::State),
        "secretBase64Url" => Some(CredentialField::SecretBase64Url),
        _ => None,
    }
}

impl<'de> Deserialize<'de> for Credential<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(CredentialVisitor)
    }
}

struct CredentialVisitor;

impl<'de> Visitor<'de> for CredentialVisitor {
    type Value = Credential<'de>;

    fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("a credential object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut service_id = None;
        let mut account_id = None;
        let mut issuer_id = None;
        let mut auth_schema_id = None;
        let mut credential_version = None;
        let mut state = None;
        let mut secret_base64_url = None;
        while let Some(field) = map.next_key_seed(FieldSeed(credential_field))? {
            match field {
                FieldName::Known(CredentialField::ServiceId) => {
                    read_once_with(&mut service_id, &mut map, |value: BorrowedString<'de>| {
                        Cow::Borrowed(value.0)
                    })?
                }
                FieldName::Known(CredentialField::AccountId) => {
                    read_once_with(&mut account_id, &mut map, |value: BorrowedString<'de>| {
                        Cow::Borrowed(value.0)
                    })?
                }
                FieldName::Known(CredentialField::IssuerId) => {
                    read_once_with(&mut issuer_id, &mut map, |value: BorrowedString<'de>| {
                        Cow::Borrowed(value.0)
                    })?
                }
                FieldName::Known(CredentialField::AuthSchemaId) => read_once_with(
                    &mut auth_schema_id,
                    &mut map,
                    |value: BorrowedString<'de>| Cow::Borrowed(value.0),
                )?,
                FieldName::Known(CredentialField::CredentialVersion) => {
                    read_once_with(&mut credential_version, &mut map, |value: U64Value| value.0)?
                }
                FieldName::Known(CredentialField::State) => {
                    read_once(&mut state, &mut map)?;
                }
                FieldName::Known(CredentialField::SecretBase64Url) => {
                    read_once_with(&mut secret_base64_url, &mut map, |value: OptionalSecret| {
                        value.0
                    })?
                }
                FieldName::Unknown => return Err(A::Error::custom("unknown credential member")),
            }
        }
        Ok(Credential {
            service_id: required(service_id)?,
            account_id: required(account_id)?,
            issuer_id: required(issuer_id)?,
            auth_schema_id: required(auth_schema_id)?,
            credential_version: CredentialVersion::new(required(credential_version)?)
                .map_err(|_| A::Error::custom("credential version is out of range"))?,
            state: required(state)?,
            secret_base64_url: required(secret_base64_url)?,
        })
    }

    reject_numbers!();
    reject_strings!();
    reject_null_and_wrappers!();
    reject_sequence!();
}

#[derive(Clone, Copy)]
enum GrantField {
    ConsumerFamily,
    ManagerId,
    PackId,
    OperationId,
    ServiceId,
    AccountId,
    AuthorityDigest,
}

fn grant_field(value: &str) -> Option<GrantField> {
    match value {
        "consumerFamily" => Some(GrantField::ConsumerFamily),
        "managerId" => Some(GrantField::ManagerId),
        "packId" => Some(GrantField::PackId),
        "operationId" => Some(GrantField::OperationId),
        "serviceId" => Some(GrantField::ServiceId),
        "accountId" => Some(GrantField::AccountId),
        "authorityDigest" => Some(GrantField::AuthorityDigest),
        _ => None,
    }
}

impl<'de> Deserialize<'de> for Grant<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(GrantVisitor)
    }
}

struct GrantVisitor;

impl<'de> Visitor<'de> for GrantVisitor {
    type Value = Grant<'de>;

    fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("a grant object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut consumer_family = None;
        let mut manager_id = None;
        let mut pack_id = None;
        let mut operation_id = None;
        let mut service_id = None;
        let mut account_id = None;
        let mut authority_digest = None;
        while let Some(field) = map.next_key_seed(FieldSeed(grant_field))? {
            match field {
                FieldName::Known(GrantField::ConsumerFamily) => {
                    read_once(&mut consumer_family, &mut map)?;
                }
                FieldName::Known(GrantField::ManagerId) => {
                    read_once_with(&mut manager_id, &mut map, |value: BorrowedString<'de>| {
                        Cow::Borrowed(value.0)
                    })?
                }
                FieldName::Known(GrantField::PackId) => {
                    read_once_with(&mut pack_id, &mut map, |value: BorrowedString<'de>| {
                        Cow::Borrowed(value.0)
                    })?
                }
                FieldName::Known(GrantField::OperationId) => {
                    read_once_with(&mut operation_id, &mut map, |value: BorrowedString<'de>| {
                        Cow::Borrowed(value.0)
                    })?
                }
                FieldName::Known(GrantField::ServiceId) => {
                    read_once_with(&mut service_id, &mut map, |value: BorrowedString<'de>| {
                        Cow::Borrowed(value.0)
                    })?
                }
                FieldName::Known(GrantField::AccountId) => {
                    read_once_with(&mut account_id, &mut map, |value: BorrowedString<'de>| {
                        Cow::Borrowed(value.0)
                    })?
                }
                FieldName::Known(GrantField::AuthorityDigest) => read_once_with(
                    &mut authority_digest,
                    &mut map,
                    |value: BorrowedString<'de>| Cow::Borrowed(value.0),
                )?,
                FieldName::Unknown => return Err(A::Error::custom("unknown grant member")),
            }
        }
        Ok(Grant {
            consumer_family: required(consumer_family)?,
            manager_id: required(manager_id)?,
            pack_id: required(pack_id)?,
            operation_id: required(operation_id)?,
            service_id: required(service_id)?,
            account_id: required(account_id)?,
            authority_digest: required(authority_digest)?,
        })
    }

    reject_numbers!();
    reject_strings!();
    reject_null_and_wrappers!();
    reject_sequence!();
}

fn read_once<'de, A, T>(slot: &mut Option<T>, map: &mut A) -> Result<(), A::Error>
where
    A: MapAccess<'de>,
    T: Deserialize<'de>,
{
    if slot.is_some() {
        return Err(A::Error::custom("duplicate vault member"));
    }
    *slot = Some(map.next_value()?);
    Ok(())
}

fn read_once_with<'de, A, T, V>(
    slot: &mut Option<T>,
    map: &mut A,
    transform: impl FnOnce(V) -> T,
) -> Result<(), A::Error>
where
    A: MapAccess<'de>,
    V: Deserialize<'de>,
{
    if slot.is_some() {
        return Err(A::Error::custom("duplicate vault member"));
    }
    *slot = Some(transform(map.next_value()?));
    Ok(())
}

fn required<T, E>(value: Option<T>) -> Result<T, E>
where
    E: de::Error,
{
    value.ok_or_else(|| E::custom("required vault member is missing"))
}
