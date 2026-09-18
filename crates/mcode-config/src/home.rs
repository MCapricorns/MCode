//! Defines the relocatable, lexical MCode home layout.
//!
//! [`HomeLayout`] resolves an absolute owned root from explicit or process
//! environment values and constructs the top-level Plugin and nested Pack
//! hierarchy. Resolution and path construction perform no filesystem I/O,
//! never canonicalize or follow links, and do not depend on the current
//! directory.

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use crate::{ConfigError, ConfigErrorKind, TransactionId};

/// Names the environment variable that relocates the entire owned home tree.
pub const MCODE_HOME_ENV: &str = "MCODE_HOME";

/// Names the lowercase product directory under a user home.
pub const MCODE_DIR_NAME: &str = ".mcode";

/// Contains caller-supplied values used to resolve the owned home.
#[derive(Debug, Clone, Default)]
pub struct HomeEnv {
    /// Overrides the entire owned home when nonempty.
    pub mcode_home: Option<OsString>,
    /// Supplies the user home when the override is empty or absent.
    pub home: Option<OsString>,
    /// Supplies the Windows-only fallback when `HOME` is empty or absent.
    ///
    /// Non-Windows resolution never consumes this value.
    pub user_profile: Option<OsString>,
}

impl HomeEnv {
    /// Reads home-resolution values from the current process.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            mcode_home: std::env::var_os(MCODE_HOME_ENV),
            home: std::env::var_os("HOME"),
            user_profile: {
                #[cfg(windows)]
                {
                    std::env::var_os("USERPROFILE")
                }
                #[cfg(not(windows))]
                {
                    None
                }
            },
        }
    }
}

/// Identifies one external top-level Plugin family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginFamily {
    /// Provider capability Pack family.
    Providers,
    /// Web capability Pack family.
    Web,
    /// MCP capability Pack family.
    Mcp,
    /// Usage capability Pack family.
    Usage,
    /// Theme asset family. The UI runtime itself is first-party built-in;
    /// this family only carries Theme Pack installation paths.
    Ui,
}

impl PluginFamily {
    /// Lists every external Plugin family in stable order.
    pub const ALL: [Self; 5] = [Self::Providers, Self::Web, Self::Mcp, Self::Usage, Self::Ui];

    /// Lists the families selected through singleton composition slots.
    pub const SINGLETONS: [Self; 2] = [Self::Web, Self::Mcp];

    /// Returns the canonical top-level Plugin ID.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Providers => "com.mcode.providers",
            Self::Web => "com.mcode.web",
            Self::Mcp => "com.mcode.mcp",
            Self::Usage => "com.mcode.usage",
            Self::Ui => "com.mcode.ui",
        }
    }

    /// Returns the short directory name for this family.
    #[must_use]
    pub const fn directory_name(self) -> &'static str {
        match self {
            Self::Providers => "providers",
            Self::Web => "web",
            Self::Mcp => "mcp",
            Self::Usage => "usage",
            Self::Ui => "ui",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootOrigin {
    Explicit,
    UserHome,
}

/// Constructs paths in one relocatable MCode home.
///
/// Every returned path is rooted below [`Self::root`]. The lowercase `.mcode`
/// directory is appended only when resolution uses a user home; `MCODE_HOME`
/// replaces the root entirely. Path accessor methods construct paths without
/// creating or opening any filesystem object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeLayout {
    root: PathBuf,
    origin: RootOrigin,
}

impl HomeLayout {
    /// Resolves the owned home from process environment values.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::InvalidHome`] when no valid absolute home can
    /// be resolved.
    pub fn from_process() -> Result<Self, ConfigError> {
        Self::from_env(HomeEnv::from_process())
    }

    /// Resolves the owned home from explicit environment values.
    ///
    /// Empty values are ignored. A nonempty `MCODE_HOME` completely replaces
    /// all fallback values. Otherwise `HOME` is used, with `USERPROFILE` as a
    /// Windows-only fallback. An invalid higher-priority value fails closed.
    /// Resolution remains lexical even when the selected path is absent,
    /// inaccessible, or names a link. Bootstrap performs native validation.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::InvalidHome`] when the selected value is not
    /// an absolute, normalized, well-formed owned home.
    pub fn from_env(env: HomeEnv) -> Result<Self, ConfigError> {
        if let Some(root) = nonempty(env.mcode_home) {
            return Self::from_root(root);
        }

        let user_home = nonempty(env.home);
        #[cfg(windows)]
        let user_home = user_home.or_else(|| nonempty(env.user_profile));

        let Some(user_home) = user_home else {
            return Err(ConfigError::new(ConfigErrorKind::InvalidHome));
        };
        let user_home = match normalize_absolute_root(PathBuf::from(user_home)) {
            Ok(path) => path,
            Err(path) => return Err(invalid_home_path(&path)),
        };
        let root = user_home.join(MCODE_DIR_NAME);
        Ok(Self {
            root,
            origin: RootOrigin::UserHome,
        })
    }

    /// Creates a layout from an already-resolved owned root.
    ///
    /// This operation is purely lexical and does not canonicalize or follow
    /// links. The stored path is absolute and separator-normalized.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::InvalidHome`] when `root` is relative, is a
    /// filesystem, drive, or share root, has no normal component, contains a
    /// parent component, or has an unsafe platform root or component.
    pub fn from_root(root: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        match normalize_absolute_root(root.into()) {
            Ok(root)
                if root
                    .components()
                    .any(|component| matches!(component, Component::Normal(_))) =>
            {
                Ok(Self {
                    root,
                    origin: RootOrigin::Explicit,
                })
            }
            Ok(root) | Err(root) => Err(invalid_home_path(&root)),
        }
    }

    /// Returns the owned home root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the root `config.json` path.
    #[must_use]
    pub fn config_json(&self) -> PathBuf {
        self.root.join("config.json")
    }

    /// Returns the top-level Plugin container root.
    #[must_use]
    pub fn plugins_dir(&self) -> PathBuf {
        self.root.join("plugins")
    }

    /// Returns one external top-level Plugin container.
    #[must_use]
    pub fn plugin_dir(&self, family: PluginFamily) -> PathBuf {
        self.plugins_dir().join(family.directory_name())
    }

    /// Returns one Plugin's nested Pack root.
    pub fn packs_dir(&self, family: PluginFamily) -> PathBuf {
        self.plugin_dir(family).join("packs")
    }

    /// Returns one Pack directory nested in an MCode-owned Plugin.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::PathEscape`] when `pack_id` is not a
    /// portable lowercase ASCII identifier.
    pub fn pack_dir(&self, family: PluginFamily, pack_id: &str) -> Result<PathBuf, ConfigError> {
        validate_portable_id(pack_id)?;
        Ok(self.packs_dir(family).join(pack_id))
    }

    /// Returns one Pack `installation.json` path.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::PathEscape`] when `pack_id` is invalid.
    pub fn pack_installation_json(
        &self,
        family: PluginFamily,
        pack_id: &str,
    ) -> Result<PathBuf, ConfigError> {
        Ok(self.pack_dir(family, pack_id)?.join("installation.json"))
    }

    /// Returns one Pack data directory.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::PathEscape`] when `pack_id` is invalid.
    pub fn pack_data_dir(
        &self,
        family: PluginFamily,
        pack_id: &str,
    ) -> Result<PathBuf, ConfigError> {
        Ok(self.pack_dir(family, pack_id)?.join("data"))
    }

    /// Returns one Pack versions directory.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::PathEscape`] when `pack_id` is invalid.
    pub fn pack_versions_dir(
        &self,
        family: PluginFamily,
        pack_id: &str,
    ) -> Result<PathBuf, ConfigError> {
        Ok(self.pack_dir(family, pack_id)?.join("versions"))
    }

    /// Returns the reserved Host-only directory.
    #[must_use]
    pub fn host_dir(&self) -> PathBuf {
        self.plugins_dir().join(".host")
    }

    /// Returns the reserved Host-only credential store path.
    #[must_use]
    pub fn host_auth_json(&self) -> PathBuf {
        self.host_dir().join("auth.json")
    }

    /// Returns the global Host-only staging lock path.
    #[must_use]
    pub fn host_staging_lock(&self) -> PathBuf {
        self.plugins_dir().join(".staging.lock")
    }

    /// Returns the global Host-only staging root.
    #[must_use]
    pub fn host_staging_dir(&self) -> PathBuf {
        self.plugins_dir().join(".staging")
    }

    /// Returns one Host-only transaction staging directory.
    #[must_use]
    pub fn transaction_staging_dir(&self, transaction_id: &TransactionId) -> PathBuf {
        self.host_staging_dir().join(transaction_id.as_str())
    }

    /// Joins a controlled relative path below the owned root.
    ///
    /// Every component must be portable across supported platforms. Empty
    /// components, absolute paths, prefixes, traversal, and unsafe Windows
    /// aliases are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::PathEscape`] when any component is unsafe.
    pub fn owned_join(&self, relative: impl AsRef<Path>) -> Result<PathBuf, ConfigError> {
        let Some(relative) = relative.as_ref().to_str() else {
            return Err(ConfigError::new(ConfigErrorKind::PathEscape));
        };
        if relative.is_empty() {
            return Err(ConfigError::new(ConfigErrorKind::PathEscape));
        }

        let mut joined = self.root.clone();
        for component in relative.split(['/', '\\']) {
            validate_path_component(OsStr::new(component))?;
            joined.push(component);
        }
        Ok(joined)
    }

    pub(crate) fn expected_root_name(&self) -> Option<&'static str> {
        (self.origin == RootOrigin::UserHome).then_some(MCODE_DIR_NAME)
    }
}

fn nonempty(value: Option<OsString>) -> Option<OsString> {
    value.filter(|value| !value.is_empty())
}

fn invalid_home_path(path: &Path) -> ConfigError {
    ConfigError::for_path(ConfigErrorKind::InvalidHome, path)
}

fn normalize_absolute_root(root: PathBuf) -> Result<PathBuf, PathBuf> {
    if !root.is_absolute() || has_parent(&root) || !has_well_formed_platform_root(&root) {
        return Err(root);
    }
    Ok(root.components().collect())
}

fn has_parent(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn has_well_formed_platform_root(path: &Path) -> bool {
    #[cfg(windows)]
    {
        windows_drive_unc_or_verbatim_root(path)
    }
    #[cfg(not(windows))]
    {
        matches!(path.components().next(), Some(Component::RootDir))
    }
}

#[cfg(windows)]
fn windows_drive_unc_or_verbatim_root(path: &Path) -> bool {
    use std::path::Prefix;

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return false;
    };
    if !matches!(components.next(), Some(Component::RootDir)) {
        return false;
    }

    let prefix_is_safe = match prefix.kind() {
        Prefix::Disk(_) | Prefix::VerbatimDisk(_) => true,
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
            is_safe_path_component(server) && is_safe_path_component(share)
        }
        Prefix::Verbatim(_) | Prefix::DeviceNS(_) => false,
    };
    prefix_is_safe
        && components.all(|component| match component {
            Component::Normal(name) => is_safe_path_component(name),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => false,
        })
}

fn validate_portable_id(value: &str) -> Result<(), ConfigError> {
    if !is_valid_portable_id(value) {
        return Err(ConfigError::new(ConfigErrorKind::PathEscape));
    }
    Ok(())
}

pub(crate) fn is_valid_portable_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    let Some((&first, rest)) = bytes.split_first() else {
        return false;
    };
    bytes.len() <= 128
        && first.is_ascii_lowercase()
        && rest
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(byte))
        && bytes
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && is_safe_path_component(OsStr::new(value))
}

pub(crate) fn validate_path_component(name: &OsStr) -> Result<(), ConfigError> {
    if !is_safe_path_component(name) {
        return Err(ConfigError::new(ConfigErrorKind::PathEscape));
    }
    Ok(())
}

fn is_safe_path_component(name: &OsStr) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    let Some(text) = name.to_str() else {
        return false;
    };
    !text.contains(['/', '\\', '\0', ':', '*', '?', '"', '<', '>', '|'])
        && !text.chars().any(char::is_control)
        && !text.ends_with('.')
        && !text.ends_with(' ')
        && !is_windows_device_name(text)
}

pub(crate) fn is_windows_device_name(name: &str) -> bool {
    // Windows strips trailing dots and spaces before reserved-device matching.
    let stripped = name.trim_end_matches([' ', '.']);
    if stripped.is_empty() {
        return false;
    }
    let basename = stripped.split('.').next().unwrap_or(stripped);
    ["con", "prn", "aux", "nul", "conin$", "conout$", "clock$"]
        .iter()
        .any(|reserved| basename.eq_ignore_ascii_case(reserved))
        || is_com_or_lpt_device(basename)
}

fn is_com_or_lpt_device(basename: &str) -> bool {
    let Some((prefix, unit)) = split_ascii_prefix(basename, 3) else {
        return false;
    };
    (prefix.eq_ignore_ascii_case("com") || prefix.eq_ignore_ascii_case("lpt"))
        && matches_dos_device_unit(unit)
}

fn split_ascii_prefix(value: &str, prefix_len: usize) -> Option<(&str, &str)> {
    (value.len() >= prefix_len && value.is_char_boundary(prefix_len))
        .then(|| value.split_at(prefix_len))
}

fn matches_dos_device_unit(unit: &str) -> bool {
    matches!(unit, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        || unit == "\u{00B9}"
        || unit == "\u{00B2}"
        || unit == "\u{00B3}"
}
