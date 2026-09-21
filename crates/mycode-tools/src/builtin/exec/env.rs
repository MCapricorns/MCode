//! Minimal child environment for structured exec.
//!
//! The block is built from an explicit allowlist of OS runtime, locale, temp,
//! and reconstructed `PATH` values. Ambient credential-like variables and
//! loader/interpreter injection variables are never copied. Dropping those
//! names is not isolation: a same-account process can still observe the
//! child.
use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::builtin::fs_search::lexical_normalize;
use crate::tool::ToolError;

/// Maximum bytes accepted for one copied environment value.
const MAX_ENV_VALUE_BYTES: usize = 32 * 1024;

#[cfg(unix)]
const ALLOWED_NAMES: &[&str] = &[
    "HOME",
    "LANG",
    "LANGUAGE",
    "LC_ADDRESS",
    "LC_ALL",
    "LC_COLLATE",
    "LC_CTYPE",
    "LC_IDENTIFICATION",
    "LC_MEASUREMENT",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NAME",
    "LC_NUMERIC",
    "LC_PAPER",
    "LC_TELEPHONE",
    "LC_TIME",
    "LOGNAME",
    "TERM",
    "TMP",
    "TMPDIR",
    "TZ",
    "USER",
];

#[cfg(windows)]
const ALLOWED_NAMES: &[&str] = &[
    "ALLUSERSPROFILE",
    "COMPUTERNAME",
    "HOMEDRIVE",
    "HOMEPATH",
    "NUMBER_OF_PROCESSORS",
    "OS",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "PROGRAMDATA",
    "PUBLIC",
    "SystemDrive",
    "SYSTEMDRIVE",
    "SystemRoot",
    "SYSTEMROOT",
    "TEMP",
    "TMP",
    "TMPDIR",
    "USERNAME",
    "USERPROFILE",
    "windir",
    "WINDIR",
];

#[cfg(not(any(unix, windows)))]
const ALLOWED_NAMES: &[&str] = &[];

/// Implied aggregate of one PATH plus every allowlisted value at the per-value cap.
const MAX_TOTAL_ENV_BYTES: usize =
    MAX_ENV_VALUE_BYTES.saturating_mul(ALLOWED_NAMES.len().saturating_add(1));

/// Snapshots the allowlisted child environment, including a reconstructed PATH.
///
/// `PATH` is rebuilt from absolute, non-empty host entries only. Relative and
/// empty entries are dropped so the child cannot inherit a cwd hijack. Entries
/// are sorted once so spawn and the invocation digest observe the same block.
/// This is the only structured-exec read of the process environment.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArgs`] when the reconstructed block would exceed
/// the existing per-value and aggregate environment budgets.
pub(super) fn snapshot_child_environment() -> Result<Vec<(OsString, OsString)>, ToolError> {
    let mut env = Vec::with_capacity(ALLOWED_NAMES.len() + 1);
    let mut total_bytes = 0_usize;
    let path = reconstructed_path()?;
    if !path.is_empty() {
        push_env_entry(&mut env, &mut total_bytes, OsString::from("PATH"), path)?;
    }
    for name in ALLOWED_NAMES {
        if env.iter().any(|(key, _)| os_eq_ignore_env(key, name)) {
            continue;
        }
        let Some(value) = std::env::var_os(name) else {
            continue;
        };
        if !env_value_allowed(&value) {
            continue;
        }
        push_env_entry(&mut env, &mut total_bytes, OsString::from(*name), value)?;
    }
    sort_env(&mut env);
    Ok(env)
}

fn push_env_entry(
    env: &mut Vec<(OsString, OsString)>,
    total_bytes: &mut usize,
    key: OsString,
    value: OsString,
) -> Result<(), ToolError> {
    let add = native_os_len(&key).saturating_add(native_os_len(&value));
    let next = total_bytes
        .checked_add(add)
        .ok_or_else(|| ToolError::InvalidArgs("aggregate environment length overflowed".into()))?;
    if next > MAX_TOTAL_ENV_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "environment data exceeds {MAX_TOTAL_ENV_BYTES} bytes"
        )));
    }
    *total_bytes = next;
    env.push((key, value));
    Ok(())
}

/// Native encoded length used by the invocation digest and redacted summaries.
#[must_use]
pub(super) fn native_os_len(value: &OsStr) -> usize {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        value.as_bytes().len()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        value.encode_wide().count().saturating_mul(2)
    }
    #[cfg(not(any(unix, windows)))]
    {
        value.as_encoded_bytes().len()
    }
}

/// Returns the reconstructed PATH value from a prepared environment block.
#[must_use]
pub(super) fn env_path(env: &[(OsString, OsString)]) -> Option<&OsStr> {
    env.iter()
        .find(|(key, _)| os_eq_ignore_env(key, "PATH"))
        .map(|(_, value)| value.as_os_str())
}

/// Sorts allowlisted environment entries into the spawn/digest order.
pub(super) fn sort_env(env: &mut [(OsString, OsString)]) {
    env.sort_by(|left, right| compare_env_keys(&left.0, &right.0));
}

fn compare_env_keys(left: &OsStr, right: &OsStr) -> Ordering {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .to_ascii_uppercase()
            .cmp(&right.to_string_lossy().to_ascii_uppercase())
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        left.as_bytes().cmp(right.as_bytes())
    }
    #[cfg(not(any(unix, windows)))]
    {
        left.cmp(right)
    }
}

fn reconstructed_path() -> Result<OsString, ToolError> {
    let host = std::env::var_os("PATH").unwrap_or_default();
    reconstructed_path_from(&host)
}

fn reconstructed_path_from(host: &OsStr) -> Result<OsString, ToolError> {
    let mut entries: Vec<PathBuf> = Vec::new();
    let mut total_bytes = 0_usize;
    for entry in std::env::split_paths(host).filter(|entry| is_searchable_path_entry(entry)) {
        let separator_bytes = if entries.is_empty() {
            0
        } else {
            native_path_separator_len()
        };
        let next = total_bytes
            .checked_add(separator_bytes)
            .and_then(|length| length.checked_add(native_os_len(entry.as_os_str())))
            .ok_or_else(|| ToolError::InvalidArgs("reconstructed PATH length overflowed".into()))?;
        if next > MAX_ENV_VALUE_BYTES {
            return Err(ToolError::InvalidArgs(format!(
                "reconstructed PATH exceeds {MAX_ENV_VALUE_BYTES} bytes"
            )));
        }
        total_bytes = next;
        entries.push(entry);
    }
    let path = std::env::join_paths(entries).unwrap_or_default();
    // Windows may quote entries while joining, so validate serialized length too.
    if native_os_len(&path) > MAX_ENV_VALUE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "reconstructed PATH exceeds {MAX_ENV_VALUE_BYTES} bytes"
        )));
    }
    Ok(path)
}

/// Native encoded length of one PATH-list separator.
const fn native_path_separator_len() -> usize {
    #[cfg(windows)]
    {
        2
    }
    #[cfg(not(windows))]
    {
        1
    }
}

/// Only absolute, non-empty PATH entries are searched or forwarded.
#[must_use]
pub(super) fn is_searchable_path_entry(entry: &Path) -> bool {
    !entry.as_os_str().is_empty() && lexical_normalize(entry).is_absolute()
}

fn env_value_allowed(value: &OsStr) -> bool {
    let len = native_os_len(value);
    if len == 0 || len > MAX_ENV_VALUE_BYTES {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        !value.as_bytes().contains(&0)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        !value.encode_wide().any(|unit| unit == 0)
    }
    #[cfg(not(any(unix, windows)))]
    {
        !value.to_string_lossy().contains('\0')
    }
}

fn os_eq_ignore_env(left: &OsStr, right: &str) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy().eq_ignore_ascii_case(right)
    }
    #[cfg(not(windows))]
    {
        left == OsStr::new(right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_and_empty_path_entries_are_not_searchable() {
        assert!(!is_searchable_path_entry(Path::new("")));
        assert!(!is_searchable_path_entry(Path::new(".")));
        assert!(!is_searchable_path_entry(Path::new("bin")));
        assert!(!is_searchable_path_entry(Path::new("./bin")));
        #[cfg(unix)]
        assert!(is_searchable_path_entry(Path::new("/usr/bin")));
        #[cfg(windows)]
        assert!(is_searchable_path_entry(Path::new(r"C:\Windows\System32")));
    }

    #[cfg(any(unix, windows))]
    fn synthetic_absolute_path(native_bytes: usize) -> OsString {
        #[cfg(unix)]
        {
            assert!(native_bytes >= 1);
            OsString::from(format!("/{}", "a".repeat(native_bytes - 1)))
        }
        #[cfg(windows)]
        {
            let characters = native_bytes / 2;
            assert!(characters >= 3);
            OsString::from(format!(r"C:\{}", "a".repeat(characters - 3)))
        }
    }

    #[cfg(any(unix, windows))]
    fn synthetic_path_list(native_bytes: usize) -> OsString {
        let first = synthetic_absolute_path(native_path_separator_len().saturating_mul(3));
        let second_bytes = native_bytes
            .checked_sub(native_os_len(&first))
            .and_then(|length| length.checked_sub(native_path_separator_len()))
            .expect("synthetic PATH has room for two entries");
        let second = synthetic_absolute_path(second_bytes);
        std::env::join_paths([PathBuf::from(first), PathBuf::from(second)])
            .expect("synthetic PATH is joinable")
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn reconstructed_path_accepts_the_per_value_limit() {
        let host = synthetic_path_list(MAX_ENV_VALUE_BYTES);
        let path = reconstructed_path_from(&host).expect("PATH at the limit");
        assert_eq!(native_os_len(&path), MAX_ENV_VALUE_BYTES);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn reconstructed_path_rejects_over_the_per_value_limit() {
        let host =
            synthetic_path_list(MAX_ENV_VALUE_BYTES.saturating_add(native_path_separator_len()));
        let error = reconstructed_path_from(&host).expect_err("PATH over the limit");
        assert!(matches!(error, ToolError::InvalidArgs(_)));
        assert!(error.to_string().contains("reconstructed PATH exceeds"));
    }

    #[test]
    fn allowlist_excludes_secrets_and_loader_injection() {
        let env = snapshot_child_environment().expect("environment snapshot");
        let keys: Vec<String> = env
            .iter()
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect();
        for forbidden in [
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
            "SSH_AUTH_SOCK",
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "DYLD_INSERT_LIBRARIES",
            "DYLD_LIBRARY_PATH",
            "PYTHONPATH",
            "NODE_OPTIONS",
            "BASH_ENV",
            "ENV",
            "IFS",
        ] {
            assert!(
                !keys.iter().any(|key| key.eq_ignore_ascii_case(forbidden)),
                "allowlist leaked {forbidden}: {keys:?}"
            );
        }
        assert!(
            keys.iter().any(|key| key.eq_ignore_ascii_case("PATH")),
            "PATH missing from {keys:?}"
        );
        for window in keys.windows(2) {
            assert_ne!(
                compare_env_keys(OsStr::new(&window[0]), OsStr::new(&window[1])),
                Ordering::Greater,
                "environment snapshot is unsorted: {keys:?}"
            );
        }
    }
}
