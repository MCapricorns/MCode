//! Sole-current typed contracts for FeaturePack and ProviderPack components.
//!
//! Feature packs share [`FEATURE_PACK_WIT_PACKAGE`] while keeping the
//! externally extensible Web, MCP, and Usage worlds and family-local DTO
//! namespaces. Provider packs use the zero-import [`PROVIDER_WORLD_ID`] and
//! export [`PROVIDER_INTERFACE_ID`]. Pack DTOs stay typed in WIT. This crate
//! contains ABI artifacts only and no component runtime.
//!
//! # Examples
//!
//! ```
//! use mcode_plugin_api::{FEATURE_PACK_WIT_PACKAGE, PROVIDER_WORLD_ID};
//!
//! assert_eq!(FEATURE_PACK_WIT_PACKAGE, "mcode:feature-pack@0.0.1");
//! assert_eq!(PROVIDER_WORLD_ID, "mcode:provider-pack/provider@0.0.1");
//! ```
#![warn(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]
#![forbid(unsafe_code)]

/// Fully qualified current FeaturePack WIT package identifier.
pub const FEATURE_PACK_WIT_PACKAGE: &str = "mcode:feature-pack@0.0.1";

/// Fully qualified current ProviderPack WIT package identifier.
pub const PROVIDER_WIT_PACKAGE: &str = "mcode:provider-pack@0.0.1";

/// Current ProviderPack world name.
pub const PROVIDER_WORLD: &str = "provider";

/// Current ProviderPack package and world version.
pub const PROVIDER_WORLD_VERSION: &str = "0.0.1";

/// Fully qualified current ProviderPack world identifier.
pub const PROVIDER_WORLD_ID: &str = "mcode:provider-pack/provider@0.0.1";

/// Sole ProviderPack guest export interface name.
pub const PROVIDER_INTERFACE: &str = "provider-api";

/// Fully qualified sole ProviderPack guest export interface identifier.
pub const PROVIDER_INTERFACE_ID: &str = "mcode:provider-pack/provider-api@0.0.1";

/// Canonical current ProviderPack WIT source.
pub const PROVIDER_WIT: &str = include_str!("../wit/provider/provider.wit");
