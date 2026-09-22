//! Workspace and global resource files feeding the system prompt.
//!
//! Resources are the built-in replacement for the old resources Pack world:
//! bounded markdown files discovered at fixed, safe locations — the session
//! workspace (AGENTS.md, MYCODE.md) and the MYCode home (AGENTS.md). Each file
//! becomes one system prompt contribution; nothing enters the prompt
//! unbounded or from arbitrary paths.

use std::path::{Path, PathBuf};

use crate::{ConfigError, HomeLayout};

/// Maximum bytes read per resource file.
pub(crate) const MAX_RESOURCE_BYTES: usize = 64 * 1024;
/// Maximum resources in one catalog.
pub(crate) const MAX_RESOURCES: usize = 16;
/// Maximum total prompt characters across all resources.
pub(crate) const MAX_TOTAL_PROMPT_CHARS: usize = 96 * 1024;

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
    push("MYCODE.md", workspace_root.join("MYCODE.md"), false);
    push(
        "AGENTS.md",
        workspace_root.join(".mycode").join("agents.md"),
        false,
    );
    push(
        "AGENTS.md",
        workspace_root.join(".agents").join("AGENTS.md"),
        false,
    );
    push("AGENTS.md", home.root().join("AGENTS.md"), true);
    if let Some(user_home) = home.root().parent() {
        push(
            "AGENTS.md",
            user_home.join(".agents").join("AGENTS.md"),
            true,
        );
    }
    files
}

/// One slash-command skill discovered under `.agents`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillFile {
    /// Command slug without the leading `/`.
    pub slug: String,
    /// One-line title from the first heading or file stem.
    pub title: String,
    /// Absolute path of the skill markdown.
    pub path: PathBuf,
    /// Whether this file came from the user-global `.agents` tree.
    pub global: bool,
}

/// Discovers `/` skills from the workspace and the user-global `.agents` tree.
#[must_use]
pub fn discover_skills(workspace_root: &Path, user_home: Option<&Path>) -> Vec<SkillFile> {
    const MAX_SKILLS: usize = 24;
    let mut skills = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut push_root = |root: &Path, global: bool| {
        collect_skills(root, &mut skills, &mut seen, MAX_SKILLS, global);
        collect_skills(
            &root.join("skills"),
            &mut skills,
            &mut seen,
            MAX_SKILLS,
            global,
        );
    };
    push_root(&workspace_root.join(".agents"), false);
    if let Some(user_home) = user_home {
        push_root(&user_home.join(".agents"), true);
    }
    skills
}

fn collect_skills(
    dir: &Path,
    skills: &mut Vec<SkillFile>,
    seen: &mut std::collections::BTreeSet<String>,
    max: usize,
    global: bool,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if skills.len() >= max {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            let skill = path.join("SKILL.md");
            if skill.is_file() {
                push_skill(&skill, path.file_name(), skills, seen, global);
            }
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md") || name.ends_with(".md"))
        {
            push_skill(&path, path.file_stem(), skills, seen, global);
        }
    }
}

fn push_skill(
    path: &Path,
    stem: Option<&std::ffi::OsStr>,
    skills: &mut Vec<SkillFile>,
    seen: &mut std::collections::BTreeSet<String>,
    global: bool,
) {
    let Some(stem) = stem.and_then(|stem| stem.to_str()) else {
        return;
    };
    let slug = stem
        .trim()
        .trim_start_matches('.')
        .replace([' ', '_'], "-")
        .to_ascii_lowercase();
    if slug.is_empty() || slug == "agents" || !seen.insert(slug.clone()) {
        return;
    }
    let title = read_resource(path)
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.trim().strip_prefix("# ").map(str::trim))
                .map(str::to_owned)
        })
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| slug.clone());
    skills.push(SkillFile {
        slug,
        title,
        path: path.to_path_buf(),
        global,
    });
}

/// Reads one resource file into bounded UTF-8 text.
///
/// # Errors
///
/// Returns [`ConfigError`] for IO failures, oversized files, or non-UTF-8
/// content.
pub(crate) fn read_resource(path: &Path) -> Result<String, ConfigError> {
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

/// Compact on-demand skill catalog for the system prompt.
///
/// Lists slug, title, and path only. Skill bodies stay on disk until the
/// model reads the named file or the user inserts a `/slug` pointer.
#[must_use]
pub fn render_skill_catalog(files: &[SkillFile]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let mut out = String::from(
        "Skills are available now. When a task matches a skill, read that SKILL.md \
and follow it without being asked. The catalog is on demand: do not paste every \
skill body into the prompt.",
    );
    for skill in files {
        out.push_str(&format!(
            "\n- /{slug} — {title} (`{path}`)",
            slug = skill.slug,
            title = skill.title,
            path = skill.path.display()
        ));
    }
    Some(out)
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
    fn discovers_dot_mycode_agents_md() {
        let (parent, home) = layout();
        let workspace = parent.path().join("proj");
        std::fs::create_dir_all(workspace.join(".mycode")).expect("workspace");
        std::fs::create_dir_all(home.root()).expect("home");
        std::fs::write(
            workspace.join(".mycode").join("agents.md"),
            "dot-folder rules",
        )
        .expect("seed");

        let files = discover_resources(&home, &workspace);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "AGENTS.md");
        assert!(
            files[0].path.ends_with(r".mycodegents.md")
                || files[0].path.ends_with(".mycode/agents.md")
        );
    }

    #[test]
    fn rendering_bounds_and_orders_contributions() {
        let (parent, home) = layout();
        let workspace = parent.path().join("proj");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(home.root()).expect("home");
        std::fs::write(workspace.join("AGENTS.md"), "be terse").expect("seed");
        std::fs::write(home.root().join("AGENTS.md"), "be safe").expect("seed");
        std::fs::write(workspace.join("MYCODE.md"), vec![0xff_u8; 16]).expect("non-utf8");

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

    #[test]
    fn discovers_agents_rules_and_skills() {
        let (parent, home) = layout();
        let workspace = parent.path().join("proj");
        let user_agents = parent.path().join(".agents").join("skills").join("review");
        std::fs::create_dir_all(workspace.join(".agents")).expect("workspace agents");
        std::fs::create_dir_all(&user_agents).expect("user skill");
        std::fs::create_dir_all(home.root()).expect("home");
        std::fs::write(
            workspace.join(".agents").join("AGENTS.md"),
            "workspace agents rules",
        )
        .expect("workspace rules");
        std::fs::write(
            user_agents.join("SKILL.md"),
            "# Review\n\nCheck the diff.\n",
        )
        .expect("skill");

        let files = discover_resources(&home, &workspace);
        assert!(
            files.iter().any(|file| file
                .path
                .ends_with(std::path::Path::new(".agents").join("AGENTS.md"))),
            "workspace .agents/AGENTS.md is a global-rules source: {files:?}"
        );
        let skills = discover_skills(&workspace, Some(parent.path()));
        assert_eq!(skills.len(), 1, "{skills:?}");
        assert_eq!(skills[0].slug, "review");
        assert_eq!(skills[0].title, "Review");
        assert!(skills[0].global, "user-home skills are marked global");
        let catalog = render_skill_catalog(&skills).expect("catalog");
        assert!(catalog.contains("/review"), "{catalog}");
        assert!(catalog.contains("on demand"), "{catalog}");
        assert!(
            !catalog.contains("Check the diff"),
            "catalog must not embed SKILL.md: {catalog}"
        );
    }
}
