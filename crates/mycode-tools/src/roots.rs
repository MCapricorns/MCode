//! Picks which workspace root an absolute tool path belongs to.
//!
//! Relative paths stay on the session cwd. An absolute path under an extra
//! workspace root is re-anchored to that root so the existing single-root
//! walk still enforces containment. Paths outside every root stay on the
//! session cwd and fail the existing escape check.

use std::path::{Path, PathBuf};

use crate::builtin::fs_search::{lexical_normalize, strip_prefix_lexical, strip_verbatim_prefix};

/// Returns the directory a tool path should be prepared against, and the
/// path spelling to pass into that prepare call.
///
/// Extra-root hits return a relative spelling (empty when the argument is
/// the root itself). Every other argument is returned unchanged so the
/// session-cwd prepare path keeps its current errors.
#[must_use]
pub fn anchor_tool_path(primary: &Path, extras: &[PathBuf], raw: &str) -> (PathBuf, String) {
    let argument = Path::new(raw);
    if !argument.is_absolute() {
        return (primary.to_path_buf(), raw.to_owned());
    }
    let normalized = lexical_normalize(&strip_verbatim_prefix(argument));
    let mut best: Option<(usize, PathBuf, String)> = None;
    for extra in extras {
        if extra.as_os_str().is_empty() {
            continue;
        }
        let extra_norm = lexical_normalize(&strip_verbatim_prefix(extra));
        let Some(relative) = strip_prefix_lexical(&extra_norm, &normalized) else {
            continue;
        };
        let depth = extra_norm.components().count();
        if best
            .as_ref()
            .is_some_and(|(best_depth, _, _)| *best_depth >= depth)
        {
            continue;
        }
        let spelled = if relative.as_os_str().is_empty() {
            String::new()
        } else {
            relative.to_string_lossy().replace('\\', "/")
        };
        best = Some((depth, extra.clone(), spelled));
    }
    match best {
        Some((_, root, relative)) => (root, relative),
        None => (primary.to_path_buf(), raw.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(parts: &[&str]) -> PathBuf {
        let mut path = if cfg!(windows) {
            PathBuf::from(r"C:\")
        } else {
            PathBuf::from("/")
        };
        for part in parts {
            path.push(part);
        }
        path
    }

    #[test]
    fn relative_paths_stay_on_the_primary_root() {
        let primary = abs(&["work", "web"]);
        let (root, path) = anchor_tool_path(&primary, &[], "src/main.rs");
        assert_eq!(root, primary);
        assert_eq!(path, "src/main.rs");
    }

    #[test]
    fn absolute_path_under_an_extra_root_is_reanchored() {
        let extras = vec![abs(&["work", "api"]), abs(&["work", "web-other"])];
        let target = abs(&["work", "api", "src", "main.rs"]);
        let (root, path) = anchor_tool_path(
            &abs(&["work", "web"]),
            &extras,
            &target.display().to_string(),
        );
        assert_eq!(root, extras[0]);
        assert_eq!(path, "src/main.rs");
    }

    #[test]
    fn a_sibling_prefix_does_not_count_as_containment() {
        let extras = vec![abs(&["work", "web"])];
        let primary = abs(&["work", "app"]);
        let target = abs(&["work", "web-other", "src", "main.rs"]);
        let spelled = target.display().to_string();
        let (root, path) = anchor_tool_path(&primary, &extras, &spelled);
        assert_eq!(root, primary);
        assert_eq!(path, spelled);
    }

    #[test]
    fn the_longer_nested_root_wins() {
        let extras = vec![abs(&["work"]), abs(&["work", "api"])];
        let target = abs(&["work", "api", "Cargo.toml"]);
        let (root, path) = anchor_tool_path(
            &abs(&["work", "web"]),
            &extras,
            &target.display().to_string(),
        );
        assert_eq!(root, extras[1]);
        assert_eq!(path, "Cargo.toml");
    }
}
