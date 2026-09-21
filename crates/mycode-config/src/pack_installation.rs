//! Strict mechanical authority for one nested Pack installation.
//!
//! This document records caller-supplied installation mechanics only. It does
//! not inspect payloads, establish inventory completeness, verify signatures or
//! relationships, or install or activate artifacts.

use std::fmt::{self, Display, Formatter};
use std::path::PathBuf;

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::authority::{
    exact_object, parse_active, parse_trust_high_water, take_positive_revision, take_string,
    take_u32,
};
use crate::home::is_windows_device_name;
use crate::pack_component::PACK_COMPONENT_BUNDLE_PATH;
use crate::parse::{ParseLimits, parse_strict_value};
use crate::secure_fs::owned_file::{locked_update_owned_file, read_owned_file};
use crate::{
    ArtifactRef, AuthorityRevision, ConfigError, ConfigErrorKind, HomeLayout, PackId, PluginFamily,
    Sha256Digest, SourceBindingId, TrustHighWater,
};

/// Maximum encoded size of one Pack installation authority.
pub const MAX_PACK_INSTALLATION_BYTES: usize = 4 * 1024 * 1024;
/// Exact Pack installation authority kind.
pub const PACK_INSTALLATION_KIND: &str = "mycode-pack-installation";
/// Exact Pack installation authority format version.
pub const PACK_INSTALLATION_FORMAT_VERSION: u32 = 1;
/// Maximum number of entries in one Pack inventory.
pub const MAX_PACK_INVENTORY_ENTRIES: usize = 4_096;

const MAX_BUNDLE_PATH_BYTES: usize = 512;
const MAX_BUNDLE_COMPONENTS: usize = 128;
const MAX_BUNDLE_COMPONENT_BYTES: usize = 128;
// Five parser nodes per maximum inventory entry plus the fixed envelope.
const PACK_INSTALLATION_MAX_NODES: usize = 5 * MAX_PACK_INVENTORY_ENTRIES + 64;
const PACK_INSTALLATION_MAX_DEPTH: usize = 8;
const INSTALLATION_FILE_NAME: &str = "installation.json";

/// Identifies one canonical relative path inside a Pack bundle.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BundlePath(String);

impl BundlePath {
    /// Parses an exact portable, forward-slash relative bundle path.
    ///
    /// The top-level `data` component and every `installation.json` component
    /// are reserved for Host-owned state and authority.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] when the path violates
    /// the bounded lowercase portable grammar.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ConfigError> {
        let value = value.as_ref();
        let bytes = value.as_bytes();
        if !(1..=MAX_BUNDLE_PATH_BYTES).contains(&bytes.len()) || !value.is_ascii() {
            return Err(authority_error());
        }
        let components = value.split('/').collect::<Vec<_>>();
        if components.is_empty()
            || components.len() > MAX_BUNDLE_COMPONENTS
            || components.first() == Some(&"data")
            || components.iter().any(|component| {
                !valid_bundle_component(component) || *component == INSTALLATION_FILE_NAME
            })
        {
            return Err(authority_error());
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the canonical forward-slash relative path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BundlePath {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("BundlePath").field(&self.0).finish()
    }
}

impl Display for BundlePath {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for BundlePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Binds one canonical bundle path to its exact content digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryEntry {
    path: BundlePath,
    digest: Sha256Digest,
}

impl InventoryEntry {
    /// Creates one validated inventory entry.
    #[must_use]
    pub fn new(path: BundlePath, digest: Sha256Digest) -> Self {
        Self { path, digest }
    }

    /// Returns the canonical bundle path.
    #[must_use]
    pub fn path(&self) -> &BundlePath {
        &self.path
    }

    /// Returns the expected content digest.
    #[must_use]
    pub fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

impl Serialize for InventoryEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("InventoryEntry", 2)?;
        state.serialize_field("path", &self.path)?;
        state.serialize_field("digest", &self.digest)?;
        state.end()
    }
}

/// Contains one complete validated mechanical Pack installation value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackInstallation {
    family: PluginFamily,
    pack_id: PackId,
    source: SourceBindingId,
    selected: ArtifactRef,
    trust_high_water: TrustHighWater,
    inventory: Vec<InventoryEntry>,
}

impl PackInstallation {
    /// Creates one complete mechanical Pack installation value.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] unless `inventory` has
    /// 1 through 4096 entries strictly increasing by raw path bytes.
    pub fn new(
        family: PluginFamily,
        pack_id: PackId,
        source: SourceBindingId,
        selected: ArtifactRef,
        trust_high_water: TrustHighWater,
        inventory: Vec<InventoryEntry>,
    ) -> Result<Self, ConfigError> {
        validate_inventory(&inventory)?;
        Ok(Self {
            family,
            pack_id,
            source,
            selected,
            trust_high_water,
            inventory,
        })
    }

    /// Returns the exact short family bound into this value.
    #[must_use]
    pub fn family(&self) -> PluginFamily {
        self.family
    }

    /// Returns the Pack identifier bound into this value.
    #[must_use]
    pub fn pack_id(&self) -> &PackId {
        &self.pack_id
    }

    /// Returns the opaque source binding.
    #[must_use]
    pub fn source(&self) -> &SourceBindingId {
        &self.source
    }

    /// Returns the mechanically selected artifact.
    #[must_use]
    pub fn selected(&self) -> &ArtifactRef {
        &self.selected
    }

    /// Returns the recorded trust high-water value.
    #[must_use]
    pub fn trust_high_water(&self) -> &TrustHighWater {
        &self.trust_high_water
    }

    /// Returns the nonempty, strictly path-sorted inventory.
    #[must_use]
    pub fn inventory(&self) -> &[InventoryEntry] {
        &self.inventory
    }

    /// Returns the executable component digest declared by the inventory.
    ///
    /// Declarative Packs need not contain the canonical `component.wasm` row.
    /// The selected artifact digest identifies the bundle and is not used as
    /// the component content digest.
    #[must_use]
    pub fn component_digest(&self) -> Option<&Sha256Digest> {
        match self.inventory.binary_search_by(|entry| {
            entry
                .path
                .as_str()
                .as_bytes()
                .cmp(PACK_COMPONENT_BUNDLE_PATH.as_bytes())
        }) {
            Ok(index) => Some(&self.inventory[index].digest),
            Err(_) => None,
        }
    }
}

/// Contains one validated persisted Pack installation revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackInstallationDocument {
    revision: AuthorityRevision,
    installation: PackInstallation,
}

impl PackInstallationDocument {
    /// Returns the positive persisted revision.
    #[must_use]
    pub fn revision(&self) -> AuthorityRevision {
        self.revision
    }

    /// Returns the complete mechanical installation value.
    #[must_use]
    pub fn installation(&self) -> &PackInstallation {
        &self.installation
    }
}

/// Reads one Pack installation authority without creating filesystem objects.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security, bounded strict JSON,
/// path-identity mismatch, or authority validation failures.
pub fn read_pack_installation(
    home: &HomeLayout,
    family: PluginFamily,
    pack_id: &PackId,
) -> Result<Option<PackInstallationDocument>, ConfigError> {
    let relative = installation_relative_path(family, pack_id);
    let target = home.pack_installation_json(family, pack_id.as_str())?;
    let bytes = read_owned_file(home, relative, MAX_PACK_INSTALLATION_BYTES)
        .map_err(|error| error.with_path(&target))?;
    bytes
        .as_deref()
        .map(|bytes| parse_document(home, family, pack_id, bytes))
        .transpose()
}

/// Replaces one Pack installation using a lock-held revision compare-and-swap.
///
/// Missing authority has logical revision zero. The supplied installation and
/// any current document must exactly match the family and Pack path arguments.
/// This operation never reads or updates any other authority document.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::RevisionConflict`] for a stale expectation,
/// [`ConfigErrorKind::RevisionExhausted`] at `i64::MAX`, and [`ConfigError`] for
/// identity, strict authority, serialization, or owned-file transaction
/// failures.
pub fn replace_pack_installation(
    home: &HomeLayout,
    family: PluginFamily,
    pack_id: &PackId,
    expected_revision: AuthorityRevision,
    installation: &PackInstallation,
) -> Result<PackInstallationDocument, ConfigError> {
    let target = home.pack_installation_json(family, pack_id.as_str())?;
    if installation.family != family || installation.pack_id != *pack_id {
        return Err(ConfigError::for_path(
            ConfigErrorKind::AuthorityValidation,
            &target,
        ));
    }
    let relative = installation_relative_path(family, pack_id);
    let mut written = None;
    locked_update_owned_file(home, relative, MAX_PACK_INSTALLATION_BYTES, |current| {
        let current_revision = match current {
            Some(bytes) => parse_document(home, family, pack_id, bytes)?.revision,
            None => AuthorityRevision::ABSENT,
        };
        if current_revision != expected_revision {
            return Err(ConfigError::for_path(
                ConfigErrorKind::RevisionConflict,
                &target,
            ));
        }
        let revision = current_revision
            .checked_next()
            .map_err(|error| error.with_path(&target))?;
        let document = PackInstallationDocument {
            revision,
            installation: installation.clone(),
        };
        let bytes = serialize_document(&document)?;
        written = Some(document);
        Ok(bytes)
    })
    .map_err(|error| error.with_path(&target))?;
    written.ok_or_else(|| ConfigError::new(ConfigErrorKind::Serialization))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SerializedDocument<'a> {
    format_version: u32,
    kind: &'static str,
    revision: u64,
    family: &'static str,
    pack_id: &'a PackId,
    source: &'a SourceBindingId,
    selected: &'a ArtifactRef,
    trust_high_water: &'a TrustHighWater,
    inventory: &'a [InventoryEntry],
}

fn serialize_document(document: &PackInstallationDocument) -> Result<Vec<u8>, ConfigError> {
    let installation = &document.installation;
    let serialized = SerializedDocument {
        format_version: PACK_INSTALLATION_FORMAT_VERSION,
        kind: PACK_INSTALLATION_KIND,
        revision: document.revision.get(),
        family: installation.family.directory_name(),
        pack_id: &installation.pack_id,
        source: &installation.source,
        selected: &installation.selected,
        trust_high_water: &installation.trust_high_water,
        inventory: &installation.inventory,
    };
    let mut bytes = serde_json::to_vec(&serialized)
        .map_err(|_| ConfigError::new(ConfigErrorKind::Serialization))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_PACK_INSTALLATION_BYTES {
        return Err(ConfigError::new(ConfigErrorKind::Serialization));
    }
    Ok(bytes)
}

fn parse_document(
    home: &HomeLayout,
    family: PluginFamily,
    pack_id: &PackId,
    bytes: &[u8],
) -> Result<PackInstallationDocument, ConfigError> {
    let target = home.pack_installation_json(family, pack_id.as_str())?;
    let value = parse_strict_value(bytes, installation_limits())
        .map_err(|error| error.with_path(&target))?;
    parse_document_value(value, family, pack_id).map_err(|error| error.with_path(&target))
}

fn parse_document_value(
    value: Value,
    expected_family: PluginFamily,
    expected_pack_id: &PackId,
) -> Result<PackInstallationDocument, ConfigError> {
    let mut root = exact_object(
        value,
        &[
            "formatVersion",
            "kind",
            "revision",
            "family",
            "packId",
            "source",
            "selected",
            "trustHighWater",
            "inventory",
        ],
    )?;
    if take_u32(&mut root, "formatVersion")? != PACK_INSTALLATION_FORMAT_VERSION
        || take_string(&mut root, "kind")? != PACK_INSTALLATION_KIND
        || take_string(&mut root, "family")? != expected_family.directory_name()
        || PackId::parse(take_string(&mut root, "packId")?)? != *expected_pack_id
    {
        return Err(authority_error());
    }
    let revision = take_positive_revision(&mut root, "revision")?;
    let source = SourceBindingId::parse(take_string(&mut root, "source")?)?;
    let selected = parse_active(root.remove("selected").ok_or_else(authority_error)?)?;
    let trust_high_water =
        parse_trust_high_water(root.remove("trustHighWater").ok_or_else(authority_error)?)?;
    let inventory = parse_inventory(root.remove("inventory").ok_or_else(authority_error)?)?;
    let installation = PackInstallation::new(
        expected_family,
        expected_pack_id.clone(),
        source,
        selected,
        trust_high_water,
        inventory,
    )?;
    Ok(PackInstallationDocument {
        revision,
        installation,
    })
}

fn parse_inventory(value: Value) -> Result<Vec<InventoryEntry>, ConfigError> {
    let values = value.as_array().cloned().ok_or_else(authority_error)?;
    let entries = values
        .into_iter()
        .map(|value| {
            let mut entry = exact_object(value, &["path", "digest"])?;
            let path = BundlePath::parse(take_string(&mut entry, "path")?)?;
            let digest = Sha256Digest::parse(take_string(&mut entry, "digest")?)?;
            Ok(InventoryEntry::new(path, digest))
        })
        .collect::<Result<Vec<_>, ConfigError>>()?;
    validate_inventory(&entries)?;
    Ok(entries)
}

fn validate_inventory(inventory: &[InventoryEntry]) -> Result<(), ConfigError> {
    if inventory.is_empty()
        || inventory.len() > MAX_PACK_INVENTORY_ENTRIES
        || inventory
            .windows(2)
            .any(|pair| pair[0].path.as_str().as_bytes() >= pair[1].path.as_str().as_bytes())
    {
        return Err(authority_error());
    }
    Ok(())
}

fn valid_bundle_component(component: &str) -> bool {
    let bytes = component.as_bytes();
    (1..=MAX_BUNDLE_COMPONENT_BYTES).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(byte))
        && !is_windows_device_name(component)
}

fn installation_relative_path(family: PluginFamily, pack_id: &PackId) -> PathBuf {
    PathBuf::from("plugins")
        .join(family.directory_name())
        .join("packs")
        .join(pack_id.as_str())
        .join(INSTALLATION_FILE_NAME)
}

fn installation_limits() -> ParseLimits {
    ParseLimits {
        max_depth: PACK_INSTALLATION_MAX_DEPTH,
        max_nodes: PACK_INSTALLATION_MAX_NODES,
    }
}

fn authority_error() -> ConfigError {
    ConfigError::new(ConfigErrorKind::AuthorityValidation)
}
