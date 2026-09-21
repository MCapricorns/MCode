use super::*;
use std::sync::{Mutex, MutexGuard};

pub(crate) static PROCESS_CWD_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn lock_process_cwd() -> MutexGuard<'static, ()> {
    PROCESS_CWD_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(target_os = "macos")]
#[test]
fn signed_device_identity_preserves_kernel_bits() {
    let device: libc::dev_t = -1;
    assert_eq!(unix_device_identity(device).unwrap(), device as u64);
}

#[test]
fn component_names_reject_separators_and_dots() {
    assert!(validate_component_name(OsStr::new("old_only.txt")).is_ok());
    assert!(validate_component_name(OsStr::new("nested/old_only.txt")).is_err());
    assert!(validate_component_name(OsStr::new(".")).is_err());
    assert!(validate_component_name(OsStr::new("..")).is_err());
    assert!(validate_component_name(OsStr::new("")).is_err());
    #[cfg(windows)]
    assert!(validate_component_name(OsStr::new(r"nested\old_only.txt")).is_err());
    #[cfg(windows)]
    assert!(validate_component_name(OsStr::new("file.txt:stream")).is_err());
    #[cfg(windows)]
    assert!(validate_component_name(OsStr::new("dir:stream:$DATA")).is_err());
    #[cfg(unix)]
    assert!(validate_component_name(OsStr::new(r"nested\old_only.txt")).is_ok());
    #[cfg(unix)]
    assert!(validate_component_name(OsStr::new("file.txt:stream")).is_ok());
}

#[test]
fn lexical_normalize_resolves_dots_without_fs() {
    assert_eq!(
        lexical_normalize(Path::new("a/b/../c/./d")),
        PathBuf::from("a/c/d")
    );
    assert_eq!(lexical_normalize(Path::new("a/..")), PathBuf::new());
    assert_eq!(lexical_normalize(Path::new("../x")), PathBuf::from("../x"));
    let above_root = lexical_normalize(Path::new("/../etc"));
    assert!(
        above_root
            .components()
            .any(|component| matches!(component, Component::ParentDir)),
        "{above_root:?}"
    );
}

#[test]
fn relative_cwd_and_root_aliases_have_exact_semantics() {
    let _cwd_lock = lock_process_cwd();
    let process_cwd = std::env::current_dir().unwrap();
    let base = tempfile::tempdir_in(&process_cwd).unwrap();
    std::fs::create_dir_all(base.path().join("sub/a")).unwrap();
    let relative_cwd = base.path().strip_prefix(&process_cwd).unwrap().join("sub");
    let expected = lexical_normalize(&process_cwd.join(&relative_cwd));

    let absolute_alias = expected.to_str().unwrap();
    for argument in [
        None,
        Some(""),
        Some("."),
        Some("./"),
        Some("a/.."),
        Some("missing/.."),
        Some(absolute_alias),
    ] {
        let resolved = resolve_search_root(&relative_cwd, argument).unwrap();
        assert_eq!(resolved.root, expected, "{argument:?}");
        assert_eq!(resolved.cwd, expected, "{argument:?}");
    }

    for argument in ["..", "../sub", "a/../.."] {
        let error = resolve_search_root(&relative_cwd, Some(argument)).unwrap_err();
        assert!(
            matches!(error, ToolError::InvalidArgs(_)),
            "{argument}: {error}"
        );
    }
}

/// An already-absolute session cwd must not consult the process cwd.
///
/// The deleted-cwd scenario runs in a child process so it cannot steal
/// the parent suite's working directory. libtest `--exact` matches the
/// crate-relative module path, not the bare function name; a bare filter
/// runs zero tests and still exits successfully.
#[cfg(unix)]
#[test]
fn absolute_session_cwd_does_not_require_process_cwd() {
    const CHILD_ENV: &str = "MYCODE_FS_SEARCH_INVALID_CWD_CHILD";
    // libtest prints this crate-relative path; `module_path!()` includes the crate name.
    const TEST_NAME: &str =
        "builtin::fs_search::tests::absolute_session_cwd_does_not_require_process_cwd";
    if std::env::var_os(CHILD_ENV).is_some() {
        let session = tempfile::tempdir().unwrap();
        std::fs::write(session.path().join("kept.txt"), "x").unwrap();
        let scratch = tempfile::tempdir().unwrap();
        std::env::set_current_dir(scratch.path()).unwrap();
        let scratch_path = scratch.path().to_path_buf();
        std::mem::forget(scratch);
        std::fs::remove_dir(&scratch_path).unwrap();
        let resolved = resolve_search_root(session.path(), None)
            .expect("absolute session cwd should resolve without process cwd");
        assert_eq!(resolved.root, session.path());
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", "--test-threads", "1", TEST_NAME])
        .env(CHILD_ENV, "1")
        .output()
        .expect("spawn invalid-cwd child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("running 1 test") && stdout.contains("test result: ok. 1 passed"),
        "child must execute the invalid-cwd branch, not an empty --exact filter\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
fn internal_parent_that_never_leaves_root_is_allowed() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("docs")).unwrap();
    let resolved = resolve_search_root(directory.path(), Some("docs/../docs")).unwrap();
    // The resolved root carries the handle-proven spelling; a tempdir on
    // a Windows host can hand out an 8.3 alias (`RUNNER~1`) while the
    // resolved root keeps the long on-disk name (`runneradmin`).
    #[cfg(windows)]
    let expected = strip_verbatim_prefix(&directory.path().join("docs").canonicalize().unwrap());
    #[cfg(not(windows))]
    let expected = directory.path().join("docs");
    assert_eq!(resolved.root, expected);
}

#[test]
fn containment_is_component_based() {
    assert!(is_within(Path::new("/a/b"), Path::new("/a/b/c")));
    assert!(!is_within(Path::new("/a/b"), Path::new("/a/bb")));
    assert!(!is_within(Path::new("/a/b"), Path::new("/a")));
}

#[cfg(not(windows))]
#[test]
fn unix_lexical_containment_and_relative_stay_case_sensitive() {
    let root = Path::new("/Ä/repo");
    assert!(!is_within_lexical(root, Path::new("/ä/repo")));
    assert!(strip_prefix_lexical(root, Path::new("/ä/repo")).is_none());
    assert_eq!(
        strip_prefix_lexical(root, Path::new("/Ä/repo/sub")),
        Some(PathBuf::from("sub"))
    );
    assert!(is_within_lexical(root, Path::new("/Ä/repo/sub")));
}

#[cfg(windows)]
#[test]
fn windows_prefix_conversion_preserves_unc_authority() {
    assert_eq!(
        strip_verbatim_prefix(Path::new(r"\\?\UNC\server\share\dir\file")),
        PathBuf::from(r"\\server\share\dir\file")
    );
    assert_eq!(
        strip_verbatim_prefix(Path::new(r"\\?\C:\dir\file")),
        PathBuf::from(r"C:\dir\file")
    );
    let volume = strip_verbatim_prefix(Path::new(r"\\?\Volume{abc}\dir"));
    assert!(volume.is_absolute(), "{volume:?}");
    assert_eq!(volume, PathBuf::from(r"\\.\Volume{abc}\dir"));
}

#[cfg(windows)]
#[test]
fn windows_extended_length_conversion_preserves_path_kinds() {
    assert_eq!(
        windows_extended_length_path(Path::new(r"C:\dir\file")),
        PathBuf::from(r"\\?\C:\dir\file")
    );
    assert_eq!(
        windows_extended_length_path(Path::new(r"\\server\share\dir\file")),
        PathBuf::from(r"\\?\UNC\server\share\dir\file")
    );
    for path in [
        r"\\?\C:\dir\file",
        r"\\?\UNC\server\share\dir",
        r"\\.\Device",
    ] {
        assert_eq!(
            windows_extended_length_path(Path::new(path)),
            Path::new(path)
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_long_cwd_opens_with_or_without_a_verbatim_prefix() {
    use std::os::windows::ffi::OsStrExt;

    let directory = tempfile::tempdir().unwrap();
    let mut long_cwd = directory.path().canonicalize().unwrap();
    while long_cwd.as_os_str().encode_wide().count() <= 300 {
        long_cwd.push("0123456789abcdef");
    }
    std::fs::create_dir_all(&long_cwd).unwrap();
    let plain_cwd = strip_verbatim_prefix(&long_cwd);
    let absolute_alias = plain_cwd.to_str().unwrap();

    for (cwd, argument) in [
        (long_cwd.as_path(), None),
        (plain_cwd.as_path(), None),
        (long_cwd.as_path(), Some(absolute_alias)),
    ] {
        let resolved = resolve_search_root(cwd, argument).unwrap();
        assert_eq!(resolved.root, resolved.cwd, "{argument:?}");
        assert_eq!(resolved.cwd, plain_cwd, "{argument:?}");
    }
}

#[cfg(windows)]
#[test]
fn windows_non_ascii_case_alias_resolves_like_the_retained_root() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path().join("Ä").join("repo");
    std::fs::create_dir_all(cwd.join("sub")).unwrap();
    std::fs::write(cwd.join("sub").join("note.txt"), "hi").unwrap();

    let resolved = resolve_search_root(&cwd, None).unwrap();
    let retained = resolved.cwd.clone();
    let alias = retained
        .to_str()
        .expect("retained cwd is Unicode")
        .to_lowercase();
    assert_ne!(
        alias,
        retained.to_string_lossy().as_ref(),
        "fixture must include a letter that Unicode-lowercases"
    );
    let slash_alias = alias.replace('\\', "/");
    let verbatim_alias = windows_extended_length_path(Path::new(&alias));
    let verbatim_alias = verbatim_alias.to_str().expect("verbatim alias is Unicode");
    let child_alias = Path::new(&alias).join("sub");
    let parent_alias = Path::new(&alias).join("..");
    let escaped_alias = Path::new(&alias).join("sub").join("..").join("..");

    for argument in [alias.as_str(), slash_alias.as_str(), verbatim_alias] {
        let aliased = resolve_search_root(&cwd, Some(argument)).unwrap();
        assert_eq!(aliased.root, retained, "{argument}");
        assert_eq!(aliased.cwd, retained, "{argument}");
    }

    let child = resolve_search_root(
        &cwd,
        Some(child_alias.to_str().expect("child alias is Unicode")),
    )
    .unwrap();
    assert_eq!(child.cwd, retained);
    assert_eq!(child.root, retained.join("sub"));

    for argument in [
        format!("{alias}2"),
        parent_alias
            .to_str()
            .expect("parent alias is Unicode")
            .to_owned(),
        escaped_alias
            .to_str()
            .expect("escaped alias is Unicode")
            .to_owned(),
    ] {
        let error = resolve_search_root(&cwd, Some(&argument)).unwrap_err();
        assert!(
            matches!(error, ToolError::InvalidArgs(_)),
            "{argument}: {error}"
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_ordinal_case_rejects_turkish_i_expansion_alias() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path().join("İ").join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let resolved = resolve_search_root(&cwd, None).unwrap();
    let retained = resolved.cwd.to_str().expect("retained cwd is Unicode");
    let dotted_alias = retained.replace('İ', "i\u{307}");
    assert_ne!(dotted_alias, retained);
    let error = resolve_search_root(&cwd, Some(&dotted_alias)).unwrap_err();
    assert!(
        matches!(error, ToolError::InvalidArgs(_)),
        "{dotted_alias}: {error}"
    );
}

#[cfg(windows)]
#[test]
fn windows_ordinal_case_accepts_small_sigma_alias() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path().join("Σ").join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let resolved = resolve_search_root(&cwd, None).unwrap();
    let retained = resolved.cwd.clone();
    let alias = retained
        .to_str()
        .expect("retained cwd is Unicode")
        .replace('Σ', "σ");
    assert_ne!(alias, retained.to_string_lossy().as_ref());
    let aliased = resolve_search_root(&cwd, Some(&alias)).unwrap();
    assert_eq!(aliased.root, retained);
    assert_eq!(aliased.cwd, retained);
}

#[cfg(windows)]
#[test]
fn windows_drive_verbatim_device_and_unc_output_never_leaks_question_prefix() {
    let cases = [
        (r"C:\Users\name", "C:/Users/name"),
        (r"\\?\C:\Users\name", "C:/Users/name"),
        (r"\\server\share\dir", "//server/share/dir"),
        (r"\\?\UNC\server\share\dir", "//server/share/dir"),
        (r"\\.\PhysicalDrive0", "//./PhysicalDrive0"),
        (r"\\?\Volume{abc}\dir", "//./Volume{abc}/dir"),
    ];
    for (input, expected) in cases {
        let rendered = to_posix(Path::new(input));
        assert_eq!(rendered, expected, "{input}");
        assert!(!rendered.contains("//?/"), "{input}: {rendered}");
    }
}

#[cfg(windows)]
#[test]
fn windows_prefix_containment_is_separate_and_exact() {
    assert!(is_within(
        Path::new(r"C:\root"),
        Path::new(r"C:\root\child")
    ));
    assert!(!is_within(
        Path::new(r"C:\root"),
        Path::new(r"D:\root\child")
    ));
    let verbatim_drive_root = strip_verbatim_prefix(Path::new(r"\\?\C:\root"));
    let verbatim_drive_child = strip_verbatim_prefix(Path::new(r"\\?\C:\root\child"));
    assert!(is_within(&verbatim_drive_root, &verbatim_drive_child));
    let verbatim_root = strip_verbatim_prefix(Path::new(r"\\?\Volume{abc}\root"));
    let verbatim_child = strip_verbatim_prefix(Path::new(r"\\?\Volume{abc}\root\child"));
    assert!(is_within(&verbatim_root, &verbatim_child));
    let unc_root = strip_verbatim_prefix(Path::new(r"\\?\UNC\server\share\root"));
    let unc_child = strip_verbatim_prefix(Path::new(r"\\?\UNC\server\share\root\child"));
    let other_share = strip_verbatim_prefix(Path::new(r"\\?\UNC\server\other\root\child"));
    assert!(is_within(&unc_root, &unc_child));
    assert!(!is_within(&unc_root, &other_share));
    assert!(is_within(
        Path::new(r"\\.\Device\root"),
        Path::new(r"\\.\Device\root\child")
    ));
    assert!(!is_within(
        Path::new(r"\\.\Device\root"),
        Path::new(r"\\.\Other\root\child")
    ));
}

#[cfg(windows)]
#[test]
fn windows_volume_verbatim_argument_is_not_cwd_relative() {
    let directory = tempfile::tempdir().unwrap();
    let decoy = directory.path().join(r"Volume{abc}").join("dir");
    std::fs::create_dir_all(&decoy).unwrap();
    std::fs::write(decoy.join("secret.txt"), "x").unwrap();
    let error = resolve_search_root(directory.path(), Some(r"\\?\Volume{abc}\dir")).unwrap_err();
    assert!(matches!(error, ToolError::InvalidArgs(_)), "{error}");
}

#[cfg(windows)]
#[test]
fn windows_lexical_containment_and_relative_share_unicode_case() {
    let root = Path::new(r"C:\Ä\repo");
    let alias = Path::new(r"c:\ä\repo");
    let child = Path::new(r"c:\ä\repo\sub\file.rs");
    let mixed_sep = Path::new(r"c:/ä/repo/sub");
    let lookalike = Path::new(r"C:\Ärepo");
    let other_drive = Path::new(r"D:\ä\repo");
    let current_drive_abs = Path::new(r"\ä\repo");
    let drive_relative = Path::new(r"C:ä\repo");
    let drive_only = Path::new(r"C:");
    let verbatim_root = strip_verbatim_prefix(Path::new(r"\\?\C:\Ä\repo"));
    let verbatim_alias = strip_verbatim_prefix(Path::new(r"\\?\c:\ä\repo\sub"));

    assert!(is_within_lexical(root, alias));
    assert_eq!(strip_prefix_lexical(root, alias), Some(PathBuf::new()));
    assert!(
        !is_within(root, alias),
        "handle-proven containment stays exact"
    );

    assert!(is_within_lexical(root, child));
    assert_eq!(
        strip_prefix_lexical(root, child),
        Some(PathBuf::from("sub").join("file.rs"))
    );
    assert!(is_within_lexical(root, mixed_sep));
    assert_eq!(
        strip_prefix_lexical(root, mixed_sep),
        Some(PathBuf::from("sub"))
    );

    assert_eq!(verbatim_root, PathBuf::from(r"C:\Ä\repo"));
    assert!(is_within_lexical(&verbatim_root, &verbatim_alias));
    assert_eq!(
        strip_prefix_lexical(&verbatim_root, &verbatim_alias),
        Some(PathBuf::from("sub"))
    );

    let unc_root = Path::new(r"\\Server\Share\Ä");
    assert_eq!(
        strip_prefix_lexical(unc_root, Path::new(r"\\server\share\ä\child")),
        Some(PathBuf::from("child"))
    );
    assert!(strip_prefix_lexical(unc_root, Path::new(r"\\server\other\ä")).is_none());
    assert!(os_str_eq_lexical(OsStr::new("Σ"), OsStr::new("σ")));
    assert!(!os_str_eq_lexical(OsStr::new("Σ"), OsStr::new("ς")));
    assert_eq!(
        strip_prefix_lexical(Path::new(r"C:\Σ"), Path::new(r"C:\σ\child")),
        Some(PathBuf::from("child"))
    );
    assert!(strip_prefix_lexical(Path::new(r"C:\Σ"), Path::new(r"C:\ς\child")).is_none());
    assert!(!os_str_eq_lexical(OsStr::new("İ"), OsStr::new("i\u{307}")));
    assert!(strip_prefix_lexical(Path::new("İ"), Path::new("i\u{307}")).is_none());

    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    let unpaired_a = OsString::from_wide(&[0xD800]);
    let unpaired_b = OsString::from_wide(&[0xD801]);
    assert!(!os_str_eq_lexical(&unpaired_a, &unpaired_b));
    assert!(os_str_eq_lexical(
        &unpaired_a,
        &OsString::from_wide(&[0xD800])
    ));

    assert!(is_within_lexical(
        Path::new(r"\\.\Device\Ä"),
        Path::new(r"\\.\device\ä\child")
    ));
    assert_eq!(
        strip_prefix_lexical(Path::new(r"C:\"), Path::new(r"c:\ä")),
        Some(PathBuf::from("ä"))
    );

    for outsider in [lookalike, other_drive, current_drive_abs, drive_relative] {
        assert!(!is_within_lexical(root, outsider), "{outsider:?}");
        assert!(
            strip_prefix_lexical(root, outsider).is_none(),
            "{outsider:?}"
        );
    }
    assert!(
        strip_prefix_lexical(drive_only, Path::new(r"C:\foo")).is_none(),
        "drive-relative C: does not contain drive-root C:\\foo"
    );
}

#[test]
fn rel_posix_and_absolute_output_are_normalized() {
    let root = Path::new("r");
    assert_eq!(rel_posix(root, &root.join("a/b.rs")), "a/b.rs");
    assert_eq!(rel_posix(&root.join("x.rs"), &root.join("x.rs")), "r/x.rs");
    assert_eq!(to_posix(Path::new("/a/b")), "/a/b");
    assert_eq!(to_posix(Path::new("/")), "/");
}

#[test]
fn display_line_truncates_on_char_boundaries() {
    let mut truncated = false;
    assert_eq!(display_line(b"hello", 10, &mut truncated), "hello");
    assert!(!truncated);

    let multibyte = "é".repeat(50);
    let mut truncated = false;
    let output = display_line(multibyte.as_bytes(), 51, &mut truncated);
    assert!(truncated);
    assert_eq!(output.len(), 50);
}

#[test]
fn scan_reservations_are_atomic_and_settled() {
    let limiter = WalkLimiter::new(&Limits::default());
    assert_eq!(limiter.reserve_scan(8, 10), ScanReservation::Granted(8));
    assert_eq!(limiter.reserve_scan(8, 10), ScanReservation::Granted(2));
    assert_eq!(limiter.reserve_scan(1, 10), ScanReservation::Pending);
    limiter.settle_scan(8, 3);
    assert_eq!(limiter.reserve_scan(5, 10), ScanReservation::Granted(5));
    limiter.settle_scan(2, 2);
    limiter.settle_scan(5, 5);
    assert_eq!(limiter.claimed_scan_bytes(), 10);
    assert_eq!(limiter.reserve_scan(1, 10), ScanReservation::Exhausted);
}

#[test]
fn concurrent_short_read_releases_capacity_before_exhaustion() {
    use std::sync::Arc;
    use std::sync::mpsc;

    let limiter = Arc::new(WalkLimiter::new(&Limits::default()));
    assert_eq!(limiter.reserve_scan(10, 10), ScanReservation::Granted(10));
    let (checked_tx, checked_rx) = mpsc::channel();
    let (settled_tx, settled_rx) = mpsc::channel();
    let contender = {
        let limiter = Arc::clone(&limiter);
        std::thread::spawn(move || {
            checked_tx.send(limiter.reserve_scan(1, 10)).unwrap();
            settled_rx.recv().unwrap();
            limiter.reserve_scan(7, 10)
        })
    };

    assert_eq!(checked_rx.recv().unwrap(), ScanReservation::Pending);
    limiter.settle_scan(10, 3);
    settled_tx.send(()).unwrap();
    assert_eq!(contender.join().unwrap(), ScanReservation::Granted(7));
    limiter.settle_scan(7, 7);
    assert_eq!(limiter.reserve_scan(1, 10), ScanReservation::Exhausted);
}

#[test]
fn target_access_denied_is_an_execution_error() {
    let error = map_target_open_error(
        "private",
        io::Error::new(io::ErrorKind::PermissionDenied, "access denied"),
    );
    assert!(matches!(error, ToolError::Execution(_)), "{error}");
}

#[cfg(unix)]
#[test]
fn fifo_target_resolution_does_not_wait_for_a_writer() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::sync::mpsc;

    let directory = tempfile::tempdir().unwrap();
    let fifo = directory.path().join("input.pipe");
    let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: `fifo_name` is a live NUL-terminated path and the mode is
    // valid. The created FIFO remains owned by the temporary directory.
    let status = unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) };
    assert_eq!(status, 0, "{}", io::Error::last_os_error());

    let cwd = directory.path().to_path_buf();
    let (result_tx, result_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        result_tx
            .send(resolve_search_root(&cwd, Some("input.pipe")))
            .unwrap();
    });
    let result = match result_rx.recv_timeout(Duration::from_millis(500)) {
        Ok(result) => result,
        Err(error) => {
            // Opening both ends never waits and releases an implementation
            // that accidentally used blocking `O_RDONLY`.
            let _release = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&fifo)
                .unwrap();
            let _ = result_rx.recv_timeout(Duration::from_secs(2));
            worker.join().unwrap();
            panic!("FIFO target resolution blocked: {error}");
        }
    };
    worker.join().unwrap();
    let error = result.unwrap_err();
    assert!(matches!(error, ToolError::InvalidArgs(_)), "{error}");
}

#[test]
fn limiter_records_first_stop_reason() {
    let limiter = WalkLimiter::new(&Limits::default());
    limiter.stop("time limit reached");
    limiter.stop("cancelled");
    assert_eq!(limiter.stopped_reason(), Some("time limit reached"));
}

#[tokio::test]
async fn run_blocking_returns_value_and_maps_panics() {
    let cancel = CancellationToken::new();
    assert_eq!(
        run_blocking("search", &cancel, SEARCH_TIME_LIMIT, |_| Ok(7usize))
            .await
            .unwrap(),
        7
    );
    let error = run_blocking(
        "search",
        &cancel,
        SEARCH_TIME_LIMIT,
        |_| -> Result<(), ToolError> {
            panic!("boom");
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ToolError::Execution(_)));
}

#[tokio::test]
async fn run_blocking_cancellation_is_an_error() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = run_blocking("find", &cancel, SEARCH_TIME_LIMIT, |_| Ok(()))
        .await
        .unwrap_err();
    assert!(matches!(error, ToolError::Execution(_)));
    assert!(error.to_string().contains("cancelled"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_blocking_midflight_cancellation_never_returns_partial_output() {
    use std::sync::mpsc;

    let cancel = CancellationToken::new();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        run_blocking("search", &task_cancel, SEARCH_TIME_LIMIT, move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok("partial")
        })
        .await
    });
    started_rx.recv().unwrap();
    cancel.cancel();
    // The worker must be allowed to unwind; run_blocking joins it.
    release_tx.send(()).unwrap();
    let error = task.await.unwrap().unwrap_err();
    assert!(matches!(error, ToolError::Execution(_)));
    assert!(error.to_string().contains("cancelled"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_blocking_joins_worker_before_returning_on_cancel() {
    use std::sync::mpsc;

    struct WorkerDrop {
        tx: mpsc::Sender<()>,
    }
    impl Drop for WorkerDrop {
        fn drop(&mut self) {
            let _ = self.tx.send(());
        }
    }

    let cancel = CancellationToken::new();
    let (started_tx, started_rx) = mpsc::channel();
    let (dropped_tx, dropped_rx) = mpsc::channel();
    let (mut reader, _writer) = blocking_pipe().unwrap();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        run_blocking("search", &task_cancel, SEARCH_TIME_LIMIT, move |token| {
            let _guard = WorkerDrop { tx: dropped_tx };
            started_tx.send(()).unwrap();
            // Block in a syscall so the oneshot cannot complete before
            // the outer cancel branch is chosen and joins this thread.
            interruptible_read(&mut reader);
            let _ = token;
            Ok(())
        })
        .await
    });
    started_rx.recv().unwrap();
    cancel.cancel();
    let error = task.await.unwrap().unwrap_err();
    assert!(dropped_rx.try_recv().is_ok(), "worker was detached");
    assert!(error.to_string().contains("cancelled"));
}

// Shared helpers used across the split test modules.

pub(crate) enum InterruptKind {
    Timeout,
    Cancel,
}
pub(crate) fn counting_limits() -> (Limits, Arc<AtomicU64>) {
    let count = Arc::new(AtomicU64::new(0));
    let limits = Limits {
        resolve_count: Some(Arc::clone(&count)),
        ..Limits::default()
    };
    (limits, count)
}
pub(crate) async fn assert_interrupt_after_final_check(kind: InterruptKind) {
    let past_check = Arc::new(AtomicBool::new(false));
    let enter_read = Arc::new(AtomicBool::new(false));
    let (reader, _writer) = blocking_pipe().unwrap();
    let cancel = CancellationToken::new();
    let work_cancel = cancel.clone();
    let started = Instant::now();
    let work = tokio::spawn({
        let past_check = Arc::clone(&past_check);
        let enter_read = Arc::clone(&enter_read);
        async move {
            run_blocking(
                "search",
                &work_cancel,
                Duration::from_millis(80),
                move |worker_cancel| {
                    let mut reader = reader;
                    interruptible_block_after_final_check(
                        &mut reader,
                        &worker_cancel,
                        &past_check,
                        &enter_read,
                    );
                    Ok(())
                },
            )
            .await
        }
    });
    wait_flag(&past_check, "worker past last token check").await;
    match kind {
        InterruptKind::Timeout => {
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        InterruptKind::Cancel => {
            cancel.cancel();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    // First interrupt was published after the last token check and may
    // already have been consumed. Enter the blocking read now.
    enter_read.store(true, Ordering::Release);
    let error = work.await.expect("join run_blocking").unwrap_err();
    match kind {
        InterruptKind::Timeout => {
            assert!(error.to_string().contains("time limit reached"), "{error}");
        }
        InterruptKind::Cancel => {
            assert!(error.to_string().contains("cancelled"), "{error}");
        }
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}
pub(crate) async fn wait_flag(flag: &AtomicBool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !flag.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}
#[cfg(windows)]
pub(crate) fn exclusive_open(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_FLAG_BACKUP_SEMANTICS: required to open a directory handle.
    // Without it, exclusive directory locks fail and the sharing barrier
    // would be skipped.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}
pub(crate) fn interruptible_block_after_final_check(
    reader: &mut File,
    cancel: &CancellationToken,
    past_check: &AtomicBool,
    enter_read: &AtomicBool,
) {
    if cancel.is_cancelled() {
        return;
    }
    past_check.store(true, Ordering::Release);
    while !enter_read.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    interruptible_read(reader);
}
pub(crate) fn blocking_pipe() -> io::Result<(File, File)> {
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;
        let mut fds = [0; 2];
        // SAFETY: `fds` is two writable integers; a successful `pipe` fills
        // both with owned descriptors transferred into `File` below.
        let status = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if status != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: exclusive ownership of the new pipe ends.
        Ok(unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Pipes::CreatePipe;
        let mut read = INVALID_HANDLE_VALUE;
        let mut write = INVALID_HANDLE_VALUE;
        // SAFETY: output handles are writable; a successful call yields
        // owned pipe ends transferred into `File`.
        let ok = unsafe { CreatePipe(&mut read, &mut write, std::ptr::null_mut(), 0) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: exclusive ownership of the new pipe ends.
        Ok(unsafe { (File::from_raw_handle(read), File::from_raw_handle(write)) })
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}
pub(crate) fn interruptible_block(reader: &mut File, cancel: &CancellationToken) {
    if !cancel.is_cancelled() {
        interruptible_read(reader);
    }
}
pub(crate) fn interruptible_read(reader: &mut File) {
    let mut byte = [0u8; 1];
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;

        if wait_for_worker_readable(reader).is_err() {
            return;
        }
        // SAFETY: `reader` is a live pipe fd; a one-byte buffer is valid.
        let read = unsafe { libc::read(reader.as_raw_fd(), byte.as_mut_ptr().cast(), 1) };
        if read < 0 {
            // After cancel, SIGURG makes read return EINTR. Do not retry:
            // the writer is still open, so a retry would block forever.
            let _ = io::Error::last_os_error();
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        let mut read = 0u32;
        // SAFETY: blocking read of one byte from a live pipe handle.
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::ReadFile(
                reader.as_raw_handle(),
                byte.as_mut_ptr().cast(),
                1,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            // Capture immediately. CancelSynchronousIo fails the
            // read with ERROR_OPERATION_ABORTED; return so the
            // worker can observe the published cancel token.
            let _ = io::Error::last_os_error();
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = reader.read(&mut byte);
    }
}
#[cfg(windows)]
pub(crate) fn open_shared_directory(path: &Path) -> File {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .expect("open directory with delete sharing")
}
pub(crate) fn pathdiff_from_to(from: &Path, to: &Path) -> PathBuf {
    let mut prefix = PathBuf::new();
    let mut current = from.to_path_buf();
    loop {
        if let Ok(suffix) = to.strip_prefix(&current) {
            prefix.push(suffix);
            return prefix;
        }
        prefix.push("..");
        if !current.pop() {
            return to.to_path_buf();
        }
    }
}
