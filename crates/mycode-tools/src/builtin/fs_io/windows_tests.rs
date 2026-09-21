use super::*;

fn injected_failure(stage: &str) -> io::Error {
    io::Error::other(format!("injected {stage} failure"))
}

fn assert_name_absent(dir: &tempfile::TempDir, name: &str) {
    assert!(
        !dir.path().join(name).exists(),
        "rejected temp name must be deleted"
    );
}

#[test]
fn temp_volume_failure_deletes_created_name() {
    let dir = tempfile::tempdir().unwrap();
    let parent = open_allowed_root(dir.path()).unwrap();
    let name = OsStr::new("mycode-write-volume.tmp");
    let error = create_temp_with(
        &parent,
        name,
        |_| Err(injected_failure("volume")),
        duplicate_delete_handle,
    )
    .err()
    .expect("injected volume failure must be returned");
    assert!(error.to_string().contains("injected volume failure"));
    assert_name_absent(&dir, "mycode-write-volume.tmp");
}

#[test]
fn temp_stat_failure_deletes_created_name() {
    let dir = tempfile::tempdir().unwrap();
    let parent = open_allowed_root(dir.path()).unwrap();
    let name = OsStr::new("mycode-write-stat.tmp");
    let error = create_temp_with(
        &parent,
        name,
        |_| Err(injected_failure("stat")),
        duplicate_delete_handle,
    )
    .err()
    .expect("injected stat failure must be returned");
    assert!(error.to_string().contains("injected stat failure"));
    assert_name_absent(&dir, "mycode-write-stat.tmp");
}

#[test]
fn temp_type_failure_deletes_created_name() {
    let dir = tempfile::tempdir().unwrap();
    let parent = open_allowed_root(dir.path()).unwrap();
    let name = OsStr::new("mycode-write-type.tmp");
    let error = create_temp_with(
        &parent,
        name,
        |file| {
            let mut meta = meta_from_file(file, true)?;
            meta.kind = FileKind::Directory;
            Ok(meta)
        },
        duplicate_delete_handle,
    )
    .err()
    .expect("a non-file temp must be rejected");
    assert!(
        error
            .to_string()
            .contains("temporary file is not a regular file"),
        "{error}"
    );
    assert_name_absent(&dir, "mycode-write-type.tmp");
}

#[test]
fn temp_duplicate_failure_deletes_created_name() {
    let dir = tempfile::tempdir().unwrap();
    let parent = open_allowed_root(dir.path()).unwrap();
    let name = OsStr::new("mycode-write-dup.tmp");
    let error = create_temp_with(
        &parent,
        name,
        |file| meta_from_file(file, true),
        |_| Err(injected_failure("duplicate")),
    )
    .err()
    .expect("injected duplicate failure must be returned");
    assert!(error.to_string().contains("injected duplicate failure"));
    // Cleanup must run on the creation handle (never a by-name reopen)
    // and must actually remove the temp.
    assert_name_absent(&dir, "mycode-write-dup.tmp");
}

#[test]
fn temp_stat_failure_reports_cleanup_error() {
    let dir = tempfile::tempdir().unwrap();
    let parent = open_allowed_root(dir.path()).unwrap();
    let name = OsStr::new("mycode-write-stat-cleanup.tmp");
    let fault = crate::builtin::fs_io::install_delete_fault_under(dir.path())
        .expect("delete fault fixture must install");
    let error = create_temp_with(
        &parent,
        name,
        |_| Err(injected_failure("stat")),
        duplicate_delete_handle,
    )
    .err()
    .expect("injected stat failure must be returned");
    assert!(
        error.to_string().contains("injected stat failure"),
        "{error}"
    );
    assert!(
        error.to_string().contains("injected mycode delete failure"),
        "cleanup failure must be folded into the returned error: {error}"
    );
    assert!(
        dir.path().join("mycode-write-stat-cleanup.tmp").exists(),
        "faulted cleanup must leave documented residue"
    );
    drop(fault);
    std::fs::remove_file(dir.path().join("mycode-write-stat-cleanup.tmp")).ok();
}
