//! Strict owned configuration authorities for MCode.
//!
//! [`ensure_home_layout`] bootstraps only the owned home root and `plugins/`.
//! Authority files are lazy, bounded, strict JSON documents published through
//! anchored no-follow transactions with revision compare-and-swap.
//!
//! The root authority is [`RootComposition`] at `config.json`; it composes
//! external Packs across the provider, web, MCP, usage, and theme families.
//! Each nested Pack records its mechanical installation in a
//! [`PackInstallation`] document at its canonical family path. The Host vault
//! is exclusively `plugins/.host/auth.json`.
//!
//! Obsolete product artifacts are not configuration inputs. This crate has no
//! migration, compatibility read, layered merge, alias, or fallback for old
//! settings, model, credential, Plugin-lock, session, or sibling-Pack layouts.

#![warn(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

mod authority;
mod checkpoints;
mod compaction;
mod error;
mod home;
mod host_vault;
mod pack_component;
mod pack_installation;
mod parse;
mod resources;
mod root_composition;
mod secrets;
mod secure_fs;
mod settings;
mod staging;
mod todos;
mod transaction_id;
mod ui_state;

#[doc(inline)]
pub use authority::{
    ArtifactRef, AuthorityRevision, CanonicalVersion, Sha256Digest, SourceBindingId, TrustHighWater,
};
pub use checkpoints::{
    CheckpointEntry, MAX_SNAPSHOT_FILE_BYTES, MAX_SNAPSHOTS_PER_SESSION, checkpoint_file,
    list_checkpoints, rollback_session,
};
#[doc(inline)]
pub use compaction::{
    COMPACTION_FORMAT_VERSION, COMPACTION_KIND, CompactionCheckpoint, MAX_SUMMARY_CHARS,
    estimate_tokens, read_compaction, write_compaction,
};
pub use error::{ConfigError, ConfigErrorKind};
pub use home::{HomeEnv, HomeLayout, MCODE_DIR_NAME, MCODE_HOME_ENV, PluginFamily};
#[doc(inline)]
pub use host_vault::{
    HOST_VAULT_FORMAT_VERSION, HOST_VAULT_KIND, HostVaultState, MAX_HOST_VAULT_BYTES,
    VaultRevision, initialize_empty_host_vault, read_host_vault_state,
};
#[doc(inline)]
pub use pack_component::{
    MAX_PACK_COMPONENT_BYTES, PACK_COMPONENT_BUNDLE_PATH, read_pack_component,
};
#[doc(inline)]
pub use pack_installation::{
    BundlePath, InventoryEntry, MAX_PACK_INSTALLATION_BYTES, MAX_PACK_INVENTORY_ENTRIES,
    PACK_INSTALLATION_FORMAT_VERSION, PACK_INSTALLATION_KIND, PackInstallation,
    PackInstallationDocument, read_pack_installation, replace_pack_installation,
};
#[doc(inline)]
pub use resources::{
    MAX_RESOURCE_BYTES, MAX_RESOURCES, MAX_TOTAL_PROMPT_CHARS, ResourceFile, discover_resources,
    read_resource, render_resource_prompt,
};
#[doc(inline)]
pub use root_composition::{
    DefaultRoute, MAX_PROVIDER_ID_BYTES, MAX_ROOT_COMPOSITION_BYTES, PackId, ProviderId,
    ROOT_COMPOSITION_FORMAT_VERSION, ROOT_COMPOSITION_KIND, RootComposition,
    RootCompositionDocument, UiSelection, read_root_composition, replace_root_composition,
};
pub use secrets::{
    MAX_KEY_BYTES, MAX_SECRET_PROVIDERS, MAX_SECRETS_BYTES, ProviderSecrets,
    SECRETS_FORMAT_VERSION, SECRETS_KIND, SECRETS_PATH, read_provider_secrets,
    replace_provider_secrets,
};
#[doc(inline)]
pub use secure_fs::owned_file::{
    ensure_owned_directory, locked_update_owned_file, read_owned_file, replace_owned_file,
};
#[doc(inline)]
pub use secure_fs::{
    AccessControlEvidence, NativeUnavailableReason, OwnedKind, ensure_home_layout,
    probe_access_control,
};
#[doc(inline)]
pub use settings::{
    AppSettings, AppearanceSettings, MAX_MCP_SERVERS, MAX_MODELS_PER_PROVIDER, MAX_PROVIDERS,
    MAX_SETTINGS_BYTES, MAX_WEB_BACKENDS, McpServerSettings, ProviderSettings,
    SETTINGS_FORMAT_VERSION, SETTINGS_KIND, SETTINGS_PATH, UsageSettings, VALID_PROVIDER_KINDS,
    WebBackendSettings, WebSettings, builtin_mcp_servers, default_user_agent, read_app_settings,
    replace_app_settings,
};
#[doc(inline)]
pub use staging::{
    MAX_STAGING_DIRECTORIES, MAX_STAGING_ENTRIES, MAX_STAGING_FILE_BYTES, MAX_STAGING_FILES,
    MAX_STAGING_JOURNAL_BYTES, MAX_STAGING_ROOT_ENTRIES, MAX_STAGING_TOTAL_BYTES,
    StagedTransaction, StagingTransaction, begin_staging, recover_abandoned_staging,
};
pub use todos::{
    MAX_TODO_CONTENT_CHARS, MAX_TODO_DEPS, MAX_TODO_TASKS, TODO_FORMAT_VERSION, TODO_KIND,
    TodoDocument, TodoStatus, TodoTask, new_todo_id, read_todo_document, read_todo_revision,
    replace_todo_document,
};
#[doc(inline)]
pub use transaction_id::TransactionId;
#[doc(inline)]
pub use ui_state::{
    MAX_RECENT_PROJECTS, MAX_SESSION_PROJECTS, UI_STATE_FORMAT_VERSION, UI_STATE_KIND,
    UI_STATE_PATH, UiState, read_ui_state, replace_ui_state,
};
