//! Strict typed authority for the root Plugin composition.
//!
//! This standalone document does not participate in layered configuration,
//! merge patch, project configuration, activation, or migration.

use std::fmt::{self, Display, Formatter};

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

use crate::home::is_valid_portable_id;
use crate::parse::{ParseLimits, parse_strict_value};
use crate::secure_fs::owned_file::{locked_update_owned_file, read_owned_file};
use crate::{AuthorityRevision, ConfigError, ConfigErrorKind, HomeLayout, PluginFamily};

/// Maximum encoded size of root `config.json`.
pub const MAX_ROOT_COMPOSITION_BYTES: usize = 64 * 1024;
/// Exact root composition document kind.
pub const ROOT_COMPOSITION_KIND: &str = "mycode-root-composition";
/// Exact root composition format version.
pub const ROOT_COMPOSITION_FORMAT_VERSION: u32 = 1;

const ROOT_COMPOSITION_PATH: &str = "config.json";
const MAX_SELECTIONS: usize = 256;
const MAX_MODEL_ID_BYTES: usize = 256;
// Valid composition has at most three 256-entry lists. This bound leaves room
// for every member node while rejecting unrelated node-heavy documents.
const COMPOSITION_MAX_NODES: usize = 2_048;
const COMPOSITION_MAX_DEPTH: usize = 8;

/// Maximum encoded length of one canonical provider ID.
pub const MAX_PROVIDER_ID_BYTES: usize = 64;

/// Identifies one product provider in configuration and Host routing.
///
/// Values contain 1 through 64 lowercase ASCII bytes. They start with a
/// letter, end with an alphanumeric byte, use only lowercase letters, digits,
/// and single hyphens, and never contain adjacent hyphens.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderId(String);

impl ProviderId {
    /// Parses one provider identifier in the frozen lowercase grammar.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] when `value` violates
    /// the provider identifier grammar or bound.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ConfigError> {
        let value = value.as_ref();
        let bytes = value.as_bytes();
        let valid = (1..=MAX_PROVIDER_ID_BYTES).contains(&bytes.len())
            && bytes.first().is_some_and(u8::is_ascii_lowercase)
            && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
            && !bytes.windows(2).any(|pair| pair == b"--");
        if !valid {
            return Err(authority_error());
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the exact provider identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ProviderId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("ProviderId").field(&self.0).finish()
    }
}

impl Display for ProviderId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ProviderId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Identifies one Pack using the frozen portable owned-home grammar.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackId(String);

impl PackId {
    /// Parses one exact portable Pack identifier.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] unless `value` is 1
    /// through 128 bytes in the frozen lowercase portable Pack grammar.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ConfigError> {
        let value = value.as_ref();
        if !is_valid_portable_id(value) {
            return Err(authority_error());
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the exact Pack identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PackId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("PackId").field(&self.0).finish()
    }
}

impl Display for PackId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for PackId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Selects one canonical provider and exact model for the default route.
#[derive(Clone, PartialEq, Eq)]
pub struct DefaultRoute {
    provider_id: ProviderId,
    model_id: String,
}

impl DefaultRoute {
    /// Creates a route from a canonical provider and visible ASCII model ID.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] unless `model_id` is 1
    /// through 256 visible non-whitespace ASCII bytes.
    pub fn new(provider_id: ProviderId, model_id: impl AsRef<str>) -> Result<Self, ConfigError> {
        let model_id = model_id.as_ref();
        validate_model_id(model_id)?;
        Ok(Self {
            provider_id,
            model_id: model_id.to_owned(),
        })
    }

    /// Returns the canonical provider identifier.
    #[must_use]
    pub fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    /// Returns the exact opaque model identifier.
    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }
}

impl fmt::Debug for DefaultRoute {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DefaultRoute")
            .field("provider_id", &self.provider_id)
            .field("model_id", &self.model_id)
            .finish()
    }
}

impl Serialize for DefaultRoute {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("DefaultRoute", 2)?;
        state.serialize_field("providerId", &self.provider_id)?;
        state.serialize_field("modelId", &self.model_id)?;
        state.end()
    }
}

/// Contains the strictly sorted theme selections.
///
/// The UI runtime is first-party built-in and is not configurable; only
/// Theme Pack selections are recorded here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiSelection {
    themes: Vec<PackId>,
}

impl UiSelection {
    /// Creates a UI selection with strictly byte-sorted unique themes.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] above 256 themes or
    /// when themes are not strictly sorted and unique.
    pub fn new(themes: Vec<PackId>) -> Result<Self, ConfigError> {
        validate_themes(&themes)?;
        Ok(Self { themes })
    }

    /// Creates a selection with no themes.
    #[must_use]
    pub fn empty() -> Self {
        Self { themes: Vec::new() }
    }

    /// Returns the strictly byte-sorted theme Packs.
    #[must_use]
    pub fn themes(&self) -> &[PackId] {
        &self.themes
    }
}

impl Default for UiSelection {
    fn default() -> Self {
        Self::empty()
    }
}

impl Serialize for UiSelection {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("UiSelection", 1)?;
        state.serialize_field("themes", &self.themes)?;
        state.end()
    }
}

/// Contains one valid root Plugin composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootComposition {
    default_route: Option<DefaultRoute>,
    providers: Vec<PackId>,
    usage: Vec<PackId>,
    ui: UiSelection,
    singletons: [Option<PackId>; 2],
}

impl RootComposition {
    /// Creates a composition with empty singleton selections.
    ///
    /// Provider and usage order is retained exactly. Usage order is the widget
    /// order. Empty lists and an empty UI selection are valid.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] when providers or usage
    /// exceed 256 entries or contain duplicate Pack IDs.
    pub fn new(
        default_route: Option<DefaultRoute>,
        providers: Vec<PackId>,
        usage: Vec<PackId>,
        ui: UiSelection,
    ) -> Result<Self, ConfigError> {
        validate_ordered_unique(&providers)?;
        validate_ordered_unique(&usage)?;
        Ok(Self {
            default_route,
            providers,
            usage,
            ui,
            singletons: std::array::from_fn(|_| None),
        })
    }

    /// Creates a valid empty composition.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            default_route: None,
            providers: Vec::new(),
            usage: Vec::new(),
            ui: UiSelection::empty(),
            singletons: std::array::from_fn(|_| None),
        }
    }

    /// Returns the explicitly selected default route.
    #[must_use]
    pub fn default_route(&self) -> Option<&DefaultRoute> {
        self.default_route.as_ref()
    }

    /// Replaces the optional default route without inferring a value.
    pub fn set_default_route(&mut self, route: Option<DefaultRoute>) {
        self.default_route = route;
    }

    /// Returns provider Packs in preserved selection order.
    #[must_use]
    pub fn providers(&self) -> &[PackId] {
        &self.providers
    }

    /// Replaces provider Packs while retaining exact input order.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] above 256 entries or
    /// for duplicate Pack IDs. The existing list remains unchanged on error.
    pub fn set_providers(&mut self, providers: Vec<PackId>) -> Result<(), ConfigError> {
        validate_ordered_unique(&providers)?;
        self.providers = providers;
        Ok(())
    }

    /// Returns usage Packs in widget order.
    #[must_use]
    pub fn usage(&self) -> &[PackId] {
        &self.usage
    }

    /// Replaces usage Packs while retaining exact widget order.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] above 256 entries or
    /// for duplicate Pack IDs. The existing list remains unchanged on error.
    pub fn set_usage(&mut self, usage: Vec<PackId>) -> Result<(), ConfigError> {
        validate_ordered_unique(&usage)?;
        self.usage = usage;
        Ok(())
    }

    /// Returns the validated UI selection.
    #[must_use]
    pub fn ui(&self) -> &UiSelection {
        &self.ui
    }

    /// Replaces the UI selection.
    pub fn set_ui(&mut self, ui: UiSelection) {
        self.ui = ui;
    }

    /// Returns one singleton-family Pack selection.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] for providers, usage,
    /// or UI because those families do not have singleton slots.
    pub fn singleton(&self, family: PluginFamily) -> Result<Option<&PackId>, ConfigError> {
        let index = singleton_index(family)?;
        Ok(self.singletons[index].as_ref())
    }

    /// Replaces one singleton-family Pack selection.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] for providers, usage,
    /// or UI. No slot changes when the family is rejected.
    pub fn set_singleton(
        &mut self,
        family: PluginFamily,
        pack: Option<PackId>,
    ) -> Result<(), ConfigError> {
        let index = singleton_index(family)?;
        self.singletons[index] = pack;
        Ok(())
    }
}

impl Default for RootComposition {
    fn default() -> Self {
        Self::empty()
    }
}

impl Serialize for RootComposition {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("RootComposition", 5)?;
        state.serialize_field("defaultRoute", &self.default_route)?;
        state.serialize_field("providers", &self.providers)?;
        state.serialize_field("usage", &self.usage)?;
        state.serialize_field("ui", &self.ui)?;
        state.serialize_field("singletons", &SingletonSelections(self))?;
        state.end()
    }
}

struct SingletonSelections<'a>(&'a RootComposition);

impl Serialize for SingletonSelections<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("SingletonSelections", 2)?;
        for family in PluginFamily::SINGLETONS {
            let index = singleton_index(family).map_err(serde::ser::Error::custom)?;
            state.serialize_field(family.directory_name(), &self.0.singletons[index])?;
        }
        state.end()
    }
}

/// Contains one validated persisted root composition revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootCompositionDocument {
    revision: AuthorityRevision,
    composition: RootComposition,
}

impl RootCompositionDocument {
    /// Returns the positive persisted revision.
    #[must_use]
    pub fn revision(&self) -> AuthorityRevision {
        self.revision
    }

    /// Returns the validated root composition.
    #[must_use]
    pub fn composition(&self) -> &RootComposition {
        &self.composition
    }
}

/// Reads and validates root `config.json` without creating filesystem objects.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security, bounded strict JSON, or
/// typed authority validation failures.
pub fn read_root_composition(
    home: &HomeLayout,
) -> Result<Option<RootCompositionDocument>, ConfigError> {
    read_owned_file(home, ROOT_COMPOSITION_PATH, MAX_ROOT_COMPOSITION_BYTES)?
        .as_deref()
        .map(|bytes| parse_document(home, bytes))
        .transpose()
}

/// Replaces root `config.json` using one lock-held revision compare-and-swap.
///
/// A missing document has logical revision zero. The current document is fully
/// read and validated before comparing `expected_revision`.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::RevisionConflict`] for a stale expectation,
/// [`ConfigErrorKind::RevisionExhausted`] at `i64::MAX`, and [`ConfigError`] for
/// strict authority, serialization, or owned-file transaction failures.
pub fn replace_root_composition(
    home: &HomeLayout,
    expected_revision: AuthorityRevision,
    composition: &RootComposition,
) -> Result<RootCompositionDocument, ConfigError> {
    let mut written = None;
    locked_update_owned_file(
        home,
        ROOT_COMPOSITION_PATH,
        MAX_ROOT_COMPOSITION_BYTES,
        |current| {
            let current_revision = match current {
                Some(bytes) => parse_document(home, bytes)?.revision,
                None => AuthorityRevision::ABSENT,
            };
            if current_revision != expected_revision {
                return Err(ConfigError::for_path(
                    ConfigErrorKind::RevisionConflict,
                    &home.config_json(),
                ));
            }
            let revision = current_revision
                .checked_next()
                .map_err(|error| error.with_path(&home.config_json()))?;
            let document = RootCompositionDocument {
                revision,
                composition: composition.clone(),
            };
            let bytes = serialize_document(&document)?;
            written = Some(document);
            Ok(bytes)
        },
    )?;
    written.ok_or_else(|| ConfigError::new(ConfigErrorKind::Serialization))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SerializedDocument<'a> {
    format_version: u32,
    kind: &'static str,
    revision: u64,
    #[serde(flatten)]
    composition: &'a RootComposition,
}

fn serialize_document(document: &RootCompositionDocument) -> Result<Vec<u8>, ConfigError> {
    let serialized = SerializedDocument {
        format_version: ROOT_COMPOSITION_FORMAT_VERSION,
        kind: ROOT_COMPOSITION_KIND,
        revision: document.revision.get(),
        composition: &document.composition,
    };
    let mut bytes = serde_json::to_vec(&serialized)
        .map_err(|_| ConfigError::new(ConfigErrorKind::Serialization))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_ROOT_COMPOSITION_BYTES {
        return Err(ConfigError::new(ConfigErrorKind::Serialization));
    }
    Ok(bytes)
}

fn parse_document(home: &HomeLayout, bytes: &[u8]) -> Result<RootCompositionDocument, ConfigError> {
    let value = parse_strict_value(bytes, composition_limits())
        .map_err(|error| error.with_path(&home.config_json()))?;
    parse_document_value(value).map_err(|error| error.with_path(&home.config_json()))
}

fn parse_document_value(value: Value) -> Result<RootCompositionDocument, ConfigError> {
    let mut root = exact_object(
        value,
        &[
            "formatVersion",
            "kind",
            "revision",
            "defaultRoute",
            "providers",
            "usage",
            "ui",
            "singletons",
        ],
    )?;
    if take_u32(&mut root, "formatVersion")? != ROOT_COMPOSITION_FORMAT_VERSION
        || take_string(&mut root, "kind")? != ROOT_COMPOSITION_KIND
    {
        return Err(authority_error());
    }
    let revision = AuthorityRevision::new(take_positive_u64(&mut root, "revision")?)?;
    let default_route = take_nullable(&mut root, "defaultRoute", parse_default_route)?;
    let providers = parse_ordered_list(take_value(&mut root, "providers")?)?;
    let usage = parse_ordered_list(take_value(&mut root, "usage")?)?;
    let ui = parse_ui(take_value(&mut root, "ui")?)?;
    let singletons = parse_singletons(take_value(&mut root, "singletons")?)?;
    let mut composition = RootComposition::new(default_route, providers, usage, ui)?;
    composition.singletons = singletons;
    Ok(RootCompositionDocument {
        revision,
        composition,
    })
}

fn parse_default_route(value: Value) -> Result<DefaultRoute, ConfigError> {
    let mut route = exact_object(value, &["providerId", "modelId"])?;
    let provider_id = ProviderId::parse(take_string(&mut route, "providerId")?)?;
    DefaultRoute::new(provider_id, take_string(&mut route, "modelId")?)
}

fn parse_ui(value: Value) -> Result<UiSelection, ConfigError> {
    let mut ui = exact_object(value, &["themes"])?;
    let themes = parse_pack_list(take_value(&mut ui, "themes")?)?;
    UiSelection::new(themes)
}

fn parse_singletons(value: Value) -> Result<[Option<PackId>; 2], ConfigError> {
    let fields = PluginFamily::SINGLETONS.map(PluginFamily::directory_name);
    let mut object = exact_object(value, &fields)?;
    let mut selections: [Option<PackId>; 2] = std::array::from_fn(|_| None);
    for (index, family) in PluginFamily::SINGLETONS.into_iter().enumerate() {
        selections[index] = take_nullable(&mut object, family.directory_name(), parse_pack_id)?;
    }
    Ok(selections)
}

fn parse_ordered_list(value: Value) -> Result<Vec<PackId>, ConfigError> {
    let values = parse_pack_list(value)?;
    validate_ordered_unique(&values)?;
    Ok(values)
}

fn parse_pack_list(value: Value) -> Result<Vec<PackId>, ConfigError> {
    let Value::Array(values) = value else {
        return Err(authority_error());
    };
    if values.len() > MAX_SELECTIONS {
        return Err(authority_error());
    }
    values.into_iter().map(parse_pack_id).collect()
}

fn parse_pack_id(value: Value) -> Result<PackId, ConfigError> {
    PackId::parse(value.as_str().ok_or_else(authority_error)?)
}

fn validate_model_id(value: &str) -> Result<(), ConfigError> {
    if value.is_empty()
        || value.len() > MAX_MODEL_ID_BYTES
        || !value.bytes().all(|byte| (b'!'..=b'~').contains(&byte))
    {
        return Err(authority_error());
    }
    Ok(())
}

fn validate_ordered_unique(values: &[PackId]) -> Result<(), ConfigError> {
    if values.len() > MAX_SELECTIONS {
        return Err(authority_error());
    }
    for (index, value) in values.iter().enumerate() {
        if values[..index].contains(value) {
            return Err(authority_error());
        }
    }
    Ok(())
}

fn validate_themes(themes: &[PackId]) -> Result<(), ConfigError> {
    if themes.len() > MAX_SELECTIONS || themes.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(authority_error());
    }
    Ok(())
}

fn singleton_index(family: PluginFamily) -> Result<usize, ConfigError> {
    PluginFamily::SINGLETONS
        .iter()
        .position(|candidate| *candidate == family)
        .ok_or_else(authority_error)
}

fn exact_object(value: Value, fields: &[&str]) -> Result<Map<String, Value>, ConfigError> {
    let Value::Object(object) = value else {
        return Err(authority_error());
    };
    if object.len() != fields.len() || !fields.iter().all(|field| object.contains_key(*field)) {
        return Err(authority_error());
    }
    Ok(object)
}

fn take_value(object: &mut Map<String, Value>, field: &str) -> Result<Value, ConfigError> {
    object.remove(field).ok_or_else(authority_error)
}

fn take_nullable<T>(
    object: &mut Map<String, Value>,
    field: &str,
    parse: impl FnOnce(Value) -> Result<T, ConfigError>,
) -> Result<Option<T>, ConfigError> {
    match take_value(object, field)? {
        Value::Null => Ok(None),
        value => parse(value).map(Some),
    }
}

fn take_string(object: &mut Map<String, Value>, field: &str) -> Result<String, ConfigError> {
    take_value(object, field)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(authority_error)
}

fn take_u32(object: &mut Map<String, Value>, field: &str) -> Result<u32, ConfigError> {
    take_value(object, field)?
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(authority_error)
}

fn take_positive_u64(object: &mut Map<String, Value>, field: &str) -> Result<u64, ConfigError> {
    let value = take_value(object, field)?
        .as_u64()
        .ok_or_else(authority_error)?;
    if value == 0 || value > i64::MAX as u64 {
        return Err(authority_error());
    }
    Ok(value)
}

fn composition_limits() -> ParseLimits {
    ParseLimits {
        max_depth: COMPOSITION_MAX_DEPTH,
        max_nodes: COMPOSITION_MAX_NODES,
    }
}

fn authority_error() -> ConfigError {
    ConfigError::new(ConfigErrorKind::AuthorityValidation)
}
