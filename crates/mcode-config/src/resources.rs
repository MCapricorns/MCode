//! Workspace and global resource files feeding the system prompt.
//!
//! Resources are the built-in replacement for the old resources Pack world:
//! bounded markdown files discovered at fixed, safe locations — the session
//! workspace (AGENTS.md, MCODE.md) and the MCode home (AGENTS.md). Each file
//! becomes one system prompt contribution; nothing enters the prompt
//! unbounded or from arbitrary paths.

use std::path::{Path, PathBuf};

use crate::{ConfigError, HomeLayout};

/// Maximum bytes read per resource file.
pub const MAX_RESOURCE_BYTES: usize = 64 * 1024;
/// Maximum resources in one catalog.
pub const MAX_RESOURCES: usize = 16;
/// Maximum total prompt characters across all resources.
pub const MAX_TOTAL_PROMPT_CHARS: usize = 96 * 1024;

/// One discovered resource file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceFile {
    /// Stable contribution title shown in the prompt header.
    pub name: String,
    /// Absolute file path.
    pub path: PathBuf,
    /// Whether this file is the global home-level resource.
    pub global: bool,
}

/// Discovers resource files for one session workspace.
///
/// Order is stable: the workspace files first, then the global home file.
/// Duplicates (the workspace root equal to the home root) are collapsed.
#[must_use]
pub fn discover_resources(home: &HomeLayout, workspace_root: &Path) -> Vec<ResourceFile> {
    let mut files = Vec::new();
    let mut push = |name: &str, path: PathBuf, global: bool| {
        if files.len() < MAX_RESOURCES
            && path.is_file()
            && !files.iter().any(|file: &ResourceFile| file.path == path)
        {
            files.push(ResourceFile {
                name: name.to_owned(),
                path,
                global,
            });
        }
    };
    push("AGENTS.md", workspace_root.join("AGENTS.md"), false);
    push("MCODE.md", workspace_root.join("MCODE.md"), false);
    push("AGENTS.md", home.root().join("AGENTS.md"), true);
    files
}

/// Reads one resource file into bounded UTF-8 text.
///
/// # Errors
///
/// Returns [`ConfigError`] for IO failures, oversized files, or non-UTF-8
/// content.
pub fn read_resource(path: &Path) -> Result<String, ConfigError> {
    let bytes = std::fs::read(path).map_err(|_| ConfigError::authority_rejection())?;
    if bytes.len() > MAX_RESOURCE_BYTES {
        return Err(ConfigError::new(crate::ConfigErrorKind::Oversized));
    }
    String::from_utf8(bytes).map_err(|_| ConfigError::authority_rejection())
}

/// Renders discovered resources into ordered system prompt parts.
///
/// Each contribution carries a header naming its source. Oversized or
/// unreadable files are skipped; the total stays within
/// [`MAX_TOTAL_PROMPT_CHARS`].
#[must_use]
pub fn render_resource_prompt(files: &[ResourceFile]) -> Vec<String> {
    let mut parts = Vec::new();
    let mut total = 0usize;
    for file in files {
        let Ok(text) = read_resource(&file.path) else {
            continue;
        };
        let scope = if file.global { "global" } else { "workspace" };
        let rendered = format!("# {name} ({scope})\n\n{text}", name = file.name);
        let chars = rendered.chars().count();
        if total + chars > MAX_TOTAL_PROMPT_CHARS {
            break;
        }
        total += chars;
        parts.push(rendered);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> (tempfile::TempDir, HomeLayout) {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        (parent, layout)
    }

    #[test]
    fn discovers_workspace_then_global_resources() {
        let (parent, home) = layout();
        let workspace = parent.path().join("proj");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(home.root()).expect("home");
        std::fs::write(workspace.join("AGENTS.md"), "workspace rules").expect("seed");
        std::fs::write(home.root().join("AGENTS.md"), "global rules").expect("seed");

        let files = discover_resources(&home, &workspace);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "AGENTS.md");
        assert!(!files[0].global);
        assert!(files[1].global);
    }

    #[test]
    fn rendering_bounds_and_orders_contributions() {
        let (parent, home) = layout();
        let workspace = parent.path().join("proj");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(home.root()).expect("home");
        std::fs::write(workspace.join("AGENTS.md"), "be terse").expect("seed");
        std::fs::write(home.root().join("AGENTS.md"), "be safe").expect("seed");
        std::fs::write(workspace.join("MCODE.md"), vec![0xff_u8; 16]).expect("non-utf8");

        let files = discover_resources(&home, &workspace);
        let parts = render_resource_prompt(&files);
        assert_eq!(parts.len(), 2, "unreadable files are skipped");
        assert!(parts[0].starts_with("# AGENTS.md (workspace)"));
        assert!(parts[0].contains("be terse"));
        assert!(parts[1].starts_with("# AGENTS.md (global)"));

        let oversized = workspace.join("AGENTS.md");
        std::fs::write(&oversized, vec![b'a'; MAX_RESOURCE_BYTES + 1]).expect("big");
        let files = discover_resources(&home, &workspace);
        let parts = render_resource_prompt(&files);
        assert_eq!(
            parts.len(),
            1,
            "oversized workspace file skipped; global file still renders"
        );
        assert!(parts[0].starts_with("# AGENTS.md (global)"));
    }

    #[test]
    fn missing_home_resource_is_absent() {
        let (_parent, home) = layout();
        let files = discover_resources(&home, Path::new("/nowhere"));
        assert!(files.is_empty());
    }
}
