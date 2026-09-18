use super::*;

fn token() -> CancellationToken {
    CancellationToken::new()
}

#[test]
fn empty_program_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let err = pin_program(dir.path(), "", &[], &token()).unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)));
    assert!(err.to_string().contains("empty"), "{err}");
}

#[test]
fn interior_nul_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let err = pin_program(dir.path(), "b\0ad", &[], &token()).unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)));
    assert!(err.to_string().contains("NUL"), "{err}");
}

#[test]
fn aggregate_argument_limit_rejects_individually_valid_arguments() {
    let args = vec!["x".repeat(MAX_ARG_BYTES); MAX_TOTAL_ARG_BYTES / MAX_ARG_BYTES + 1];
    let error = validate_request("program", &args).unwrap_err();
    assert!(matches!(error, ToolError::InvalidArgs(_)));
    assert!(
        error.to_string().contains("argument data exceeds"),
        "{error}"
    );
}

#[test]
fn missing_basename_reports_searched_count_without_dumping_path() {
    let dir = tempfile::tempdir().unwrap();
    let err = pin_program(dir.path(), "mcode-exec-missing-binary-xyz", &[], &token()).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("not found on PATH"), "{text}");
    assert!(text.contains("directories searched)"), "{text}");
    assert!(text.len() < 400, "error echoed the PATH: {text}");
}

#[test]
fn absent_path_candidate_is_typed_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let error = resolve_basename(
        "mcode-exec-missing-binary-xyz",
        Some(dir.path().as_os_str()),
    )
    .unwrap_err();
    assert!(error.is_not_found(), "{error}");
}

#[test]
fn non_regular_path_candidate_fails_closed_before_later_match() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    let name = "mcode-path-candidate";
    std::fs::create_dir(first.join(name)).unwrap();
    std::fs::write(second.join(name), b"later regular candidate").unwrap();
    let path = std::env::join_paths([first, second]).unwrap();

    let error = resolve_basename(name, Some(&path)).unwrap_err();
    assert!(!error.is_not_found(), "{error}");
    assert!(error.to_string().contains("not a regular file"), "{error}");
}

#[test]
fn permission_denied_candidate_probe_fails_closed() {
    let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
    let error = candidate_metadata_error(denied).unwrap_err();
    assert!(!error.is_not_found(), "{error}");
    assert!(error.to_string().contains("access denied"), "{error}");
}

#[test]
fn relative_path_program_must_become_absolute() {
    let dir = tempfile::tempdir().unwrap();
    let err = pin_program(dir.path(), "nested/tool", &[], &token()).unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
}

#[cfg(windows)]
#[test]
fn drive_relative_program_never_enters_path_search() {
    let dir = tempfile::tempdir().unwrap();
    let error = pin_program(dir.path(), r"C:tool.exe", &[], &token()).unwrap_err();
    assert!(matches!(error, ToolError::InvalidArgs(_)));
    assert!(error.to_string().contains("must be absolute"), "{error}");
}

fn assert_same_pinned_identity(left: &PinnedImage, right: &PinnedImage) {
    assert_eq!(left.identity, right.identity);
    assert_eq!(left.digest, right.digest);
    assert_eq!(left.canonical_path, right.canonical_path);
}

#[cfg(unix)]
#[test]
fn unix_basename_and_explicit_symlink_share_target_identity() {
    let target = Path::new("/bin/true");
    if !target.is_file() {
        eprintln!("skipping: /bin/true is not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("true");
    std::os::unix::fs::symlink(target, &link).unwrap();

    let via_target = pin_candidate(target, &token()).unwrap();
    let via_explicit = pin_candidate(&link, &token()).unwrap();
    assert_same_pinned_identity(&via_explicit, &via_target);

    let found = resolve_basename("true", Some(dir.path().as_os_str())).unwrap();
    let via_path = pin_candidate(&found, &token()).unwrap();
    assert_same_pinned_identity(&via_path, &via_target);
}

#[cfg(unix)]
#[test]
fn unix_backslash_basename_searches_path_never_cwd() {
    let root = tempfile::tempdir().unwrap();
    let path_dir = root.path().join("bin");
    let cwd = root.path().join("cwd");
    std::fs::create_dir(&path_dir).unwrap();
    std::fs::create_dir(&cwd).unwrap();

    let name = r"foo\bar";
    let path_file = path_dir.join(name);
    let cwd_spoof = cwd.join(name);
    std::fs::write(&path_file, b"path-image").unwrap();
    std::fs::write(&cwd_spoof, b"cwd-spoof").unwrap();

    let found = resolve_program(name, &cwd, Some(path_dir.as_os_str())).unwrap();
    assert_eq!(found, path_file);
    assert_ne!(found, cwd_spoof);

    let error = resolve_program(name, &cwd, None).unwrap_err();
    assert!(error.to_string().contains("not found on PATH"), "{error}");
    assert!(
        !is_path_program(name),
        "Unix backslash basename must not be treated as a path"
    );
}

// APFS rejects invalid UTF-8 byte names with EILSEQ; Linux provides this fixture.
#[cfg(target_os = "linux")]
#[test]
fn unicode_alias_to_non_utf8_target_is_rejected_before_spawn() {
    use std::os::unix::ffi::OsStringExt as _;
    use std::os::unix::fs::symlink;

    let source = Path::new("/usr/bin/true");
    if !source.is_file() {
        eprintln!("skipping: /usr/bin/true is not present");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let target = directory
        .path()
        .join(std::ffi::OsString::from_vec(b"target-\xff".to_vec()));
    std::fs::copy(source, &target).unwrap();
    let alias = directory.path().join("unicode-alias-λ");
    symlink(&target, &alias).unwrap();

    let error = pin_program(directory.path(), alias.to_str().unwrap(), &[], &token()).unwrap_err();
    assert!(matches!(error, ToolError::InvalidArgs(_)));
    assert!(error.to_string().contains("not valid Unicode"), "{error}");
}

#[cfg(target_os = "macos")]
#[test]
fn unicode_alias_to_unicode_target_shares_identity() {
    use std::os::unix::fs::symlink;

    let source = Path::new("/usr/bin/true");
    if !source.is_file() {
        eprintln!("skipping: /usr/bin/true is not present");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("target-λ");
    std::fs::copy(source, &target).unwrap();
    let alias = directory.path().join("unicode-alias-λ");
    symlink(&target, &alias).unwrap();
    assert_same_pinned_identity(
        &pin_candidate(&alias, &token()).unwrap(),
        &pin_candidate(&target, &token()).unwrap(),
    );
}

#[cfg(windows)]
#[test]
fn windows_basename_and_explicit_symlink_share_target_identity() {
    use std::os::windows::fs::symlink_file;

    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let target = PathBuf::from(root).join("System32").join("whoami.exe");
    if !target.is_file() {
        eprintln!("skipping: {} is not present", target.display());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("whoami.exe");
    if let Err(err) = symlink_file(&target, &link) {
        eprintln!("skipping: file symlink creation is unavailable: {err}");
        return;
    }

    let via_target = pin_candidate(&target, &token()).unwrap();
    let via_explicit = pin_candidate(&link, &token()).unwrap();
    assert_same_pinned_identity(&via_explicit, &via_target);

    let found = resolve_basename("whoami", Some(dir.path().as_os_str())).unwrap();
    let via_path = pin_candidate(&found, &token()).unwrap();
    assert_same_pinned_identity(&via_path, &via_target);
}

#[cfg(windows)]
#[test]
fn batch_extension_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("evil.cmd"), b"@echo off").unwrap();
    let program = dir.path().join("evil.cmd").to_string_lossy().into_owned();
    let err = pin_program(dir.path(), &program, &[], &token()).unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)));
    assert!(err.to_string().contains("cmd.exe"), "{err}");
}

#[test]
fn shebang_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("script.bin");
    std::fs::write(&program, b"#!/bin/sh\necho hi\n").unwrap();
    let err = pin_program(dir.path(), program.to_str().unwrap(), &[], &token()).unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)));
    assert!(
        err.to_string().contains("shebang") || err.to_string().contains("kernel-loadable"),
        "{err}"
    );
}

#[test]
fn cancelled_prepare_is_execution_error() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = token();
    cancel.cancel();
    let err = pin_program(dir.path(), "true", &[], &cancel).unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)));
    assert!(err.to_string().contains("cancelled"), "{err}");
}

#[test]
fn hex_encode_is_lowercase() {
    assert_eq!(encode_hex(&[0x0a, 0xff]), "0aff");
}
