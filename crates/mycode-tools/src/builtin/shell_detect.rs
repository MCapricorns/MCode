//! Default platform-shell discovery and the process-wide runtime preference.
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::builtin::fs_search::lexical_normalize;

static RUNTIME_SHELL: RwLock<Option<DetectedShell>> = RwLock::new(None);

/// Kind of platform shell used by the `shell` tool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellKind {
    /// PowerShell 7+ (`pwsh`).
    Pwsh,
    /// Windows PowerShell 5.1 (`powershell`).
    PowerShell,
    /// `cmd.exe`.
    Cmd,
    /// Bash (`bash` / `sh`).
    Bash,
}

impl ShellKind {
    /// Stable settings / wire name for this kind.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pwsh => "pwsh",
            Self::PowerShell => "powershell",
            Self::Cmd => "cmd",
            Self::Bash => "bash",
        }
    }

    /// Parses a settings / wire name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "pwsh" => Some(Self::Pwsh),
            "powershell" => Some(Self::PowerShell),
            "cmd" => Some(Self::Cmd),
            "bash" => Some(Self::Bash),
            _ => None,
        }
    }

    /// Classifies a program path from its file stem.
    #[must_use]
    pub fn from_program(path: &Path) -> Self {
        let stem = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        match stem.to_ascii_lowercase().as_str() {
            "pwsh" => Self::Pwsh,
            "powershell" => Self::PowerShell,
            "cmd" => Self::Cmd,
            "bash" | "sh" => Self::Bash,
            _ => {
                #[cfg(windows)]
                {
                    Self::Pwsh
                }
                #[cfg(not(windows))]
                {
                    Self::Bash
                }
            }
        }
    }
}

/// A resolved shell program and its kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DetectedShell {
    /// Interpreter family used to build launch arguments.
    pub kind: ShellKind,
    /// Absolute or PATH-resolved program path.
    pub program: PathBuf,
}

/// First pinnable platform shell for the current host.
#[must_use]
pub fn detect_default_shell() -> Option<DetectedShell> {
    #[cfg(windows)]
    {
        detect_windows_shell_with(&WindowsShellEnv::from_process())
    }
    #[cfg(not(windows))]
    {
        detect_posix_shell()
    }
}

/// Replaces the process-wide shell preference used by execute.
pub fn set_runtime_shell(shell: Option<DetectedShell>) {
    *RUNTIME_SHELL
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = shell;
}

/// Current process-wide shell preference, if desktop or a test set one.
#[must_use]
pub(crate) fn runtime_shell() -> Option<DetectedShell> {
    RUNTIME_SHELL
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

#[cfg(not(windows))]
fn detect_posix_shell() -> Option<DetectedShell> {
    let path = std::env::var_os("PATH");
    let mut candidates = vec![PathBuf::from("/bin/bash")];
    candidates.extend(path_named_files(path.as_deref(), "bash"));
    candidates.extend(path_named_files(path.as_deref(), "sh"));
    candidates
        .into_iter()
        .find(|program| program.is_file())
        .map(|program| DetectedShell {
            kind: ShellKind::Bash,
            program,
        })
}

#[cfg(windows)]
#[derive(Debug, Default)]
pub(crate) struct WindowsShellEnv {
    path: Option<std::ffi::OsString>,
    program_files: Option<std::ffi::OsString>,
    program_files_x86: Option<std::ffi::OsString>,
    local_app_data: Option<std::ffi::OsString>,
    user_profile: Option<std::ffi::OsString>,
    system_root: Option<std::ffi::OsString>,
    comspec: Option<std::ffi::OsString>,
}

#[cfg(windows)]
impl WindowsShellEnv {
    fn from_process() -> Self {
        Self {
            path: std::env::var_os("PATH"),
            program_files: std::env::var_os("ProgramFiles"),
            program_files_x86: std::env::var_os("ProgramFiles(x86)"),
            local_app_data: std::env::var_os("LOCALAPPDATA")
                .or_else(|| std::env::var_os("LocalAppData")),
            user_profile: std::env::var_os("USERPROFILE"),
            system_root: std::env::var_os("SystemRoot").or_else(|| std::env::var_os("SYSTEMROOT")),
            comspec: std::env::var_os("ComSpec").or_else(|| std::env::var_os("COMSPEC")),
        }
    }
}

/// First pinnable candidate from an injected Windows environment.
#[cfg(windows)]
#[must_use]
pub(crate) fn detect_windows_shell_with(env: &WindowsShellEnv) -> Option<DetectedShell> {
    windows_shell_candidates(env)
        .into_iter()
        .find(|(_, program)| powershell_image_looks_pinnable(program))
        .map(|(kind, program)| DetectedShell { kind, program })
}

/// Candidate programs in discovery order. Existence is not required.
#[cfg(windows)]
fn windows_shell_candidates(env: &WindowsShellEnv) -> Vec<(ShellKind, PathBuf)> {
    let mut candidates = Vec::new();

    candidates.extend(
        path_named_files(env.path.as_deref(), "pwsh.exe")
            .into_iter()
            .map(|program| (ShellKind::Pwsh, program)),
    );

    if let Some(program_files) = env.program_files.as_ref() {
        let program_files = Path::new(program_files);
        candidates.push((
            ShellKind::Pwsh,
            program_files.join("PowerShell").join("7").join("pwsh.exe"),
        ));
        candidates.push((
            ShellKind::Pwsh,
            program_files
                .join("PowerShell")
                .join("7-preview")
                .join("pwsh.exe"),
        ));
        candidates.extend(
            fuzzy_windows_apps_pwsh(&program_files.join("WindowsApps"))
                .into_iter()
                .map(|program| (ShellKind::Pwsh, program)),
        );
    }

    if let Some(program_files_x86) = env.program_files_x86.as_ref() {
        candidates.push((
            ShellKind::Pwsh,
            Path::new(program_files_x86)
                .join("PowerShell")
                .join("7")
                .join("pwsh.exe"),
        ));
    }

    if let Some(local_app_data) = env.local_app_data.as_ref() {
        candidates.push((
            ShellKind::Pwsh,
            Path::new(local_app_data)
                .join("Microsoft")
                .join("WinGet")
                .join("Links")
                .join("pwsh.exe"),
        ));
    }

    if let Some(user_profile) = env.user_profile.as_ref() {
        candidates.push((
            ShellKind::Pwsh,
            Path::new(user_profile)
                .join("scoop")
                .join("shims")
                .join("pwsh.exe"),
        ));
    }

    candidates.extend(
        path_named_files(env.path.as_deref(), "powershell.exe")
            .into_iter()
            .map(|program| (ShellKind::PowerShell, program)),
    );

    if let Some(system_root) = env.system_root.as_ref() {
        candidates.push((
            ShellKind::PowerShell,
            Path::new(system_root)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe"),
        ));
    }

    if let Some(comspec) = env.comspec.as_ref() {
        candidates.push((ShellKind::Cmd, PathBuf::from(comspec)));
    }
    if let Some(system_root) = env.system_root.as_ref() {
        candidates.push((
            ShellKind::Cmd,
            Path::new(system_root).join("System32").join("cmd.exe"),
        ));
    }

    if let Some(program_files) = env.program_files.as_ref() {
        candidates.push((
            ShellKind::Bash,
            Path::new(program_files)
                .join("Git")
                .join("bin")
                .join("bash.exe"),
        ));
    }
    if let Some(local_app_data) = env.local_app_data.as_ref() {
        candidates.push((
            ShellKind::Bash,
            Path::new(local_app_data)
                .join("Programs")
                .join("Git")
                .join("bin")
                .join("bash.exe"),
        ));
    }

    candidates
}

fn path_named_files(path_var: Option<&OsStr>, file_name: &str) -> Vec<PathBuf> {
    let Some(path_var) = path_var else {
        return Vec::new();
    };
    std::env::split_paths(path_var)
        .filter(|entry| is_absolute_path_entry(entry))
        .map(|entry| entry.join(file_name))
        .collect()
}

fn is_absolute_path_entry(entry: &Path) -> bool {
    !entry.as_os_str().is_empty() && lexical_normalize(entry).is_absolute()
}

/// A regular file large enough that it is not a 0-byte Store execution alias.
#[cfg(windows)]
#[must_use]
pub(crate) fn powershell_image_looks_pinnable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 64)
}

/// Finds `pwsh.exe` inside versioned `Microsoft.PowerShell_*` package
/// directories under a WindowsApps root, newest version first.
#[cfg(windows)]
#[must_use]
pub(crate) fn fuzzy_windows_apps_pwsh(windows_apps: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(windows_apps) else {
        return Vec::new();
    };
    let mut packages: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("Microsoft.PowerShell_"))
        })
        .collect();
    // Descending lexical order puts the highest version first for the
    // stable `Major.Minor.Patch.Build` naming scheme.
    packages.sort_unstable_by(|a, b| b.cmp(a));
    packages
        .into_iter()
        .map(|package| package.join("pwsh.exe"))
        .filter(|pwsh| pwsh.exists())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_kind_parse_and_stems() {
        assert_eq!(ShellKind::Pwsh.as_str(), "pwsh");
        assert_eq!(ShellKind::PowerShell.as_str(), "powershell");
        assert_eq!(ShellKind::Cmd.as_str(), "cmd");
        assert_eq!(ShellKind::Bash.as_str(), "bash");
        assert_eq!(ShellKind::parse("PWSH"), Some(ShellKind::Pwsh));
        assert_eq!(
            ShellKind::parse(" powershell "),
            Some(ShellKind::PowerShell)
        );
        assert_eq!(ShellKind::parse("cmd"), Some(ShellKind::Cmd));
        assert_eq!(ShellKind::parse("bash"), Some(ShellKind::Bash));
        assert_eq!(ShellKind::parse("fish"), None);
        assert_eq!(
            ShellKind::from_program(Path::new("pwsh.exe")),
            ShellKind::Pwsh
        );
        // A Windows-spelled path is a single component on Unix, so its stem is
        // only meaningful where `\` separates.
        #[cfg(windows)]
        assert_eq!(
            ShellKind::from_program(Path::new(
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
            )),
            ShellKind::PowerShell
        );
        assert_eq!(
            ShellKind::from_program(Path::new("cmd.exe")),
            ShellKind::Cmd
        );
        assert_eq!(
            ShellKind::from_program(Path::new("/bin/bash")),
            ShellKind::Bash
        );
    }

    #[cfg(windows)]
    fn write_pinnable(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, vec![0_u8; 128]).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn store_pwsh_is_found_under_windows_apps_not_program_files_siblings() {
        let root = tempfile::tempdir().unwrap();
        let program_files = root.path();
        let package = "Microsoft.PowerShell_9.0.0.0_x64__8wekyb3d8bbwe";
        let store_pwsh = program_files
            .join("WindowsApps")
            .join(package)
            .join("pwsh.exe");
        let sibling_pwsh = program_files.join(package).join("pwsh.exe");
        write_pinnable(&store_pwsh);
        write_pinnable(&sibling_pwsh);

        let windows_apps = program_files.join("WindowsApps");
        assert!(
            windows_apps.ends_with("WindowsApps"),
            "{}",
            windows_apps.display()
        );
        assert_eq!(
            fuzzy_windows_apps_pwsh(&windows_apps),
            vec![store_pwsh.clone()]
        );
        assert_eq!(
            fuzzy_windows_apps_pwsh(program_files),
            vec![sibling_pwsh.clone()],
            "the scanner treats its argument as the WindowsApps root"
        );

        let detected = detect_windows_shell_with(&WindowsShellEnv {
            program_files: Some(program_files.as_os_str().to_os_string()),
            ..WindowsShellEnv::default()
        })
        .expect("store pwsh is discoverable");
        assert_eq!(detected.kind, ShellKind::Pwsh);
        assert_eq!(detected.program, store_pwsh);
        assert_ne!(detected.program, sibling_pwsh);
    }

    #[cfg(windows)]
    #[test]
    fn discovery_prefers_powershell_7_over_inbox_51() {
        let root = tempfile::tempdir().unwrap();
        let program_files = root.path().join("Program Files");
        let system_root = root.path().join("Windows");
        let pwsh7 = program_files.join("PowerShell").join("7").join("pwsh.exe");
        let inbox = system_root
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        write_pinnable(&pwsh7);
        write_pinnable(&inbox);

        let detected = detect_windows_shell_with(&WindowsShellEnv {
            program_files: Some(program_files.as_os_str().to_os_string()),
            system_root: Some(system_root.as_os_str().to_os_string()),
            ..WindowsShellEnv::default()
        })
        .expect("pwsh 7 is discoverable");
        assert_eq!(detected.kind, ShellKind::Pwsh);
        assert_eq!(detected.program, pwsh7);
    }

    #[cfg(windows)]
    #[test]
    fn path_store_alias_is_skipped_for_later_pinnable_pwsh() {
        let root = tempfile::tempdir().unwrap();
        let alias_dir = root.path().join("alias");
        let real_dir = root.path().join("real");
        let alias = alias_dir.join("pwsh.exe");
        let real = real_dir.join("pwsh.exe");
        std::fs::create_dir_all(&alias_dir).unwrap();
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::write(&alias, []).unwrap();
        write_pinnable(&real);

        let path = std::env::join_paths([&alias_dir, &real_dir]).unwrap();
        let detected = detect_windows_shell_with(&WindowsShellEnv {
            path: Some(path),
            ..WindowsShellEnv::default()
        })
        .expect("real PE after a Store alias");
        assert_eq!(detected.program, real);
    }

    #[cfg(not(windows))]
    #[test]
    fn posix_detect_finds_a_shell() {
        let detected = detect_default_shell().expect("POSIX host has bash or sh");
        assert_eq!(detected.kind, ShellKind::Bash);
        assert!(detected.program.is_file(), "{:?}", detected.program);
    }
}
