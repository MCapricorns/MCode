//! Strict owned configuration authorities for MYCode.
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
//! Product documents (`settings.json`, `secrets.json`, `ui.json`) recover
//! trailing commas from earlier writers, fill missing fields, and rewrite the
//! canonical document. Unrelated obsolete layouts are still not inputs: there
//! is no alias, layered merge, or fallback for Plugin-lock, session, or
//! sibling-Pack paths.

#![warn(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

mod authority;
mod checkpoints;
mod compaction;
mod error;
mod home;
mod host_vault;
mod json_recover;
mod mcp_import;
mod pack_component;
mod pack_installation;
mod parse;
mod resources;
mod root_composition;
mod secrets;
mod secure_fs;
mod settings;
mod staging;
mod subagents;
mod todos;
mod transaction_id;
mod ui_state;

#[doc(inline)]
pub use authority::{
    ArtifactRef, AuthorityRevision, CanonicalVersion, Sha256Digest, SourceBindingId, TrustHighWater,
};
pub use checkpoints::{checkpoint_file, rollback_session};
#[doc(inline)]
pub use compaction::{
    COMPACTION_FORMAT_VERSION, COMPACTION_KIND, CompactionCheckpoint, MAX_SUMMARY_CHARS,
    estimate_tokens, read_compaction, write_compaction,
};
pub use error::{ConfigError, ConfigErrorKind};
pub use home::{
    HomeEnv, HomeLayout, MYCODE_DIR_NAME, MYCODE_HOME_ENV, PluginFamily, SCRATCH_DIR, SESSIONS_DIR,
    project_folder_name, session_relative,
};
#[doc(inline)]
pub use host_vault::{
    HOST_VAULT_FORMAT_VERSION, HOST_VAULT_KIND, HostVaultState, MAX_HOST_VAULT_BYTES,
    VaultRevision, initialize_empty_host_vault, read_host_vault_state,
};
pub use mcp_import::{normalize_api_key, parse_mcp_import};
#[doc(inline)]
pub use pack_component::{MAX_PACK_COMPONENT_BYTES, read_pack_component};
#[doc(inline)]
pub use pack_installation::{
    BundlePath, InventoryEntry, MAX_PACK_INSTALLATION_BYTES, MAX_PACK_INVENTORY_ENTRIES,
    PACK_INSTALLATION_FORMAT_VERSION, PACK_INSTALLATION_KIND, PackInstallation,
    PackInstallationDocument, read_pack_installation, replace_pack_installation,
};
#[doc(inline)]
pub use resources::{
    ResourceFile, SkillFile, discover_resources, discover_skills, render_resource_prompt,
    render_skill_catalog,
};
#[doc(inline)]
pub use root_composition::{
    DefaultRoute, MAX_PROVIDER_ID_BYTES, MAX_ROOT_COMPOSITION_BYTES, PackId, ProviderId,
    ROOT_COMPOSITION_FORMAT_VERSION, ROOT_COMPOSITION_KIND, RootComposition,
    RootCompositionDocument, UiSelection, read_root_composition, replace_root_composition,
};
pub use secrets::{
    MAX_SECRETS_BYTES, ProviderSecrets, SECRETS_FORMAT_VERSION, SECRETS_KIND, SECRETS_PATH,
    read_provider_secrets, replace_provider_secrets,
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
    AppSettings, AppearanceSettings, MAX_AUTHORITY_DOCUMENT_BYTES, MAX_MCP_ENV_VARS,
    MAX_MCP_SERVERS, MAX_MODELS_PER_PROVIDER, MAX_PROVIDERS, MAX_SETTINGS_BYTES,
    MAX_SUBAGENT_CONCURRENCY, MAX_SUBAGENT_ROLES, MAX_WEB_BACKENDS, McpServerSettings,
    ProviderSettings, SETTINGS_FORMAT_VERSION, SETTINGS_KIND, SETTINGS_PATH, ShellSettings,
    SubagentRoleSettings, SubagentSettings, ToolsSettings, UsageSettings, VALID_PROVIDER_KINDS,
    VALID_SHELL_KINDS, VALID_WEB_KINDS, WebBackendSettings, WebSettings, builtin_mcp_servers,
    builtin_web_backends, default_user_agent, read_app_settings, replace_app_settings,
    split_command_line,
};
#[doc(inline)]
pub use staging::{
    StagedTransaction, StagingTransaction, begin_staging, recover_abandoned_staging,
};
#[doc(inline)]
pub use subagents::{
    MAX_ROLE_BYTES, MAX_ROLES, ROLE_DIR_NAME, RoleCatalog, RoleIsolation, RoleOrigin, RoleProblem,
    RoleThinking, SubagentRole, builtin_roles, discover_roles,
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
