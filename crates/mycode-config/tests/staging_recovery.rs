use std::fs;

use mycode_config::{
    BundlePath, HomeLayout, begin_staging, ensure_home_layout, recover_abandoned_staging,
};

fn layout() -> (tempfile::TempDir, HomeLayout) {
    let parent = tempfile::tempdir().expect("parent");
    let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
    ensure_home_layout(&layout).expect("bootstrap");
    (parent, layout)
}

fn canonical_journal(id: &str, state: &str) -> Vec<u8> {
    format!(
        "{{\"formatVersion\":1,\"kind\":\"mycode-staging-transaction\",\"transactionId\":\"{id}\",\"state\":\"{state}\"}}\n"
    )
    .into_bytes()
}

#[test]
fn absent_staging_recovers_without_creation() {
    let (_parent, layout) = layout();
    assert_eq!(recover_abandoned_staging(&layout).expect("recovery"), 0);
    assert!(!layout.host_staging_dir().exists());
    assert!(!layout.host_staging_lock().exists());
}

#[test]
fn missing_global_lock_rejects_without_modification() {
    let (_parent, layout) = layout();
    let writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    drop(writing);
    fs::remove_file(layout.host_staging_lock()).expect("remove global lock fixture");

    assert!(recover_abandoned_staging(&layout).is_err());
    assert!(transaction.exists());
    assert!(!layout.host_staging_lock().exists());
}

#[test]
fn empty_writing_transaction_is_durably_removed() {
    let (_parent, layout) = layout();
    let writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    drop(writing);

    assert_eq!(recover_abandoned_staging(&layout).expect("recovery"), 1);
    assert!(!transaction.exists());
    assert!(layout.host_staging_dir().exists());
    assert!(layout.host_staging_lock().exists());
}

#[test]
fn staged_transaction_is_durably_removed() {
    let (_parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    writing
        .write_file(&BundlePath::parse("dir/file").expect("path"), b"payload")
        .expect("write");
    let transaction = layout.transaction_staging_dir(writing.id());
    drop(writing.finish().expect("finish"));

    assert_eq!(recover_abandoned_staging(&layout).expect("recovery"), 1);
    assert!(!transaction.exists());
}

#[test]
fn recovery_count_matches_exact_names_removed() {
    let (_parent, layout) = layout();
    let first = begin_staging(&layout).expect("first transaction");
    let first_path = layout.transaction_staging_dir(first.id());
    drop(first);
    let second = begin_staging(&layout).expect("second transaction");
    let second_path = layout.transaction_staging_dir(second.id());
    drop(second);
    let busy = begin_staging(&layout).expect("busy transaction");
    let busy_path = layout.transaction_staging_dir(busy.id());

    assert_eq!(recover_abandoned_staging(&layout).expect("recovery"), 2);
    assert!(!first_path.exists());
    assert!(!second_path.exists());
    assert!(busy_path.exists());
    drop(busy);
}

#[test]
fn busy_transaction_is_preserved() {
    let (_parent, layout) = layout();
    let writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());

    assert_eq!(recover_abandoned_staging(&layout).expect("recovery"), 0);
    assert!(transaction.exists());
    drop(writing);
}

#[test]
fn malformed_claimed_and_unknown_shape_transactions_are_preserved() {
    for (label, mutation) in [
        ("malformed", "malformed"),
        ("committing", "committing"),
        ("committed", "committed"),
        ("future", "future"),
        ("extra", "extra"),
        ("invalid-tree", "invalid-tree"),
    ] {
        let (_parent, layout) = layout();
        let mut writing = begin_staging(&layout).expect("begin");
        let id = writing.id().as_str().to_owned();
        let transaction = layout.transaction_staging_dir(writing.id());
        if mutation == "invalid-tree" {
            writing
                .write_file(&BundlePath::parse("valid").expect("path"), b"x")
                .expect("write");
        }
        drop(writing);
        match mutation {
            "malformed" => {
                fs::write(transaction.join("journal.json"), b"{}\n").expect("malformed fixture")
            }
            "committing" | "committed" | "future" => fs::write(
                transaction.join("journal.json"),
                canonical_journal(&id, mutation),
            )
            .expect("claimed fixture"),
            "extra" => fs::write(transaction.join("extra"), b"x").expect("extra fixture"),
            "invalid-tree" => {
                fs::rename(
                    transaction.join("payload/valid"),
                    transaction.join("payload/Invalid"),
                )
                .expect("invalid tree fixture");
            }
            _ => unreachable!(),
        }

        assert_eq!(
            recover_abandoned_staging(&layout).expect("recovery"),
            0,
            "{label}"
        );
        assert!(transaction.exists(), "{label}");
    }
}

#[test]
fn staged_empty_transaction_is_preserved() {
    let (_parent, layout) = layout();
    let writing = begin_staging(&layout).expect("begin");
    let id = writing.id().as_str().to_owned();
    let transaction = layout.transaction_staging_dir(writing.id());
    drop(writing);
    fs::write(
        transaction.join("journal.json"),
        canonical_journal(&id, "staged"),
    )
    .expect("staged journal");

    assert_eq!(recover_abandoned_staging(&layout).expect("recovery"), 0);
    assert!(transaction.exists());
}

#[test]
fn invalid_existing_global_lock_preserves_candidate() {
    let (_parent, layout) = layout();
    let writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    drop(writing);
    fs::write(layout.host_staging_lock(), b"not empty").expect("invalid lock");

    assert!(recover_abandoned_staging(&layout).is_err());
    assert!(transaction.exists());
}

#[test]
fn staging_file_instead_of_directory_is_rejected_without_creation() {
    let (_parent, layout) = layout();
    fs::write(layout.host_staging_dir(), b"invalid staging").expect("invalid staging");

    assert!(recover_abandoned_staging(&layout).is_err());
    assert!(layout.host_staging_dir().is_file());
    assert!(!layout.host_staging_lock().exists());
}

#[test]
fn representative_noncanonical_journals_preserve_candidates() {
    for variant in [
        "reordered",
        "duplicate",
        "unknown",
        "wrong-type",
        "non-utf8",
    ] {
        let (_parent, layout) = layout();
        let writing = begin_staging(&layout).expect("begin");
        let id = writing.id().as_str().to_owned();
        let transaction = layout.transaction_staging_dir(writing.id());
        drop(writing);
        let bytes = match variant {
            "reordered" => format!("{{\"kind\":\"mycode-staging-transaction\",\"formatVersion\":1,\"transactionId\":\"{id}\",\"state\":\"writing\"}}\n").into_bytes(),
            "duplicate" => format!("{{\"formatVersion\":1,\"kind\":\"mycode-staging-transaction\",\"transactionId\":\"{id}\",\"state\":\"writing\",\"state\":\"writing\"}}\n").into_bytes(),
            "unknown" => format!("{{\"formatVersion\":1,\"kind\":\"mycode-staging-transaction\",\"transactionId\":\"{id}\",\"state\":\"writing\",\"extra\":0}}\n").into_bytes(),
            "wrong-type" => format!("{{\"formatVersion\":\"1\",\"kind\":\"mycode-staging-transaction\",\"transactionId\":\"{id}\",\"state\":\"writing\"}}\n").into_bytes(),
            "non-utf8" => vec![0xff, 0xfe, b'\n'],
            _ => unreachable!(),
        };
        fs::write(transaction.join("journal.json"), bytes).expect("journal fixture");

        assert_eq!(
            recover_abandoned_staging(&layout).expect("recovery"),
            0,
            "{variant}"
        );
        assert!(transaction.exists(), "{variant}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn same_filesystem_bind_mount_is_preserved_when_available() {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::Command;

    struct MountedDirectory {
        path: PathBuf,
        mounted: bool,
    }

    impl Drop for MountedDirectory {
        fn drop(&mut self) {
            if self.mounted {
                let _ = Command::new("umount").arg(&self.path).status();
            }
        }
    }

    let (parent, layout) = layout();
    let writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    drop(writing);

    let external = parent.path().join("external-payload");
    fs::create_dir(&external).expect("external directory");
    fs::set_permissions(&external, fs::Permissions::from_mode(0o700))
        .expect("external directory mode");
    let external_file = external.join("outside");
    fs::write(&external_file, b"outside").expect("external file");
    fs::set_permissions(&external_file, fs::Permissions::from_mode(0o600))
        .expect("external file mode");

    let mountpoint = transaction.join("payload/mounted");
    fs::create_dir(&mountpoint).expect("mountpoint");
    fs::set_permissions(&mountpoint, fs::Permissions::from_mode(0o700)).expect("mountpoint mode");
    let mounted = Command::new("mount")
        .arg("--bind")
        .arg(&external)
        .arg(&mountpoint)
        .output();
    let Ok(output) = mounted else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let mut guard = MountedDirectory {
        path: mountpoint,
        mounted: true,
    };

    assert_eq!(recover_abandoned_staging(&layout).expect("recovery"), 0);
    assert!(transaction.exists());
    assert_eq!(fs::read(&external_file).expect("outside file"), b"outside");

    let unmounted = Command::new("umount")
        .arg(&guard.path)
        .status()
        .expect("unmount command");
    assert!(unmounted.success(), "unmount bind fixture");
    guard.mounted = false;
}

#[cfg(windows)]
#[test]
fn readonly_transaction_is_removed() {
    let (_parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    writing
        .write_file(&BundlePath::parse("dir/file").expect("path"), b"payload")
        .expect("write");
    let transaction = layout.transaction_staging_dir(writing.id());
    drop(writing);
    for path in [
        transaction.join("payload/dir/file"),
        transaction.join("payload/dir"),
        transaction.join("payload"),
        transaction.join("journal.json"),
        transaction.join("transaction.lock"),
        transaction.clone(),
    ] {
        let mut permissions = fs::metadata(&path).expect("metadata").permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).expect("readonly");
    }

    assert_eq!(recover_abandoned_staging(&layout).expect("recovery"), 1);
    assert!(!transaction.exists());
}

#[cfg(windows)]
#[test]
fn reparse_payload_directory_is_preserved() {
    let (parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    writing
        .write_file(&BundlePath::parse("dir/file").expect("path"), b"payload")
        .expect("write");
    let transaction = layout.transaction_staging_dir(writing.id());
    drop(writing);
    let original = parent.path().join("original-payload-directory");
    fs::rename(transaction.join("payload/dir"), &original).expect("move payload directory");
    junction::create(&original, transaction.join("payload/dir")).expect("payload junction");

    assert_eq!(recover_abandoned_staging(&layout).expect("recovery"), 0);
    assert!(transaction.exists());
    assert_eq!(
        fs::read(original.join("file")).expect("outside payload"),
        b"payload"
    );
}

#[cfg(windows)]
#[test]
fn hardlinked_payload_is_preserved() {
    let (_parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    writing
        .write_file(&BundlePath::parse("file").expect("path"), b"payload")
        .expect("write");
    let transaction = layout.transaction_staging_dir(writing.id());
    drop(writing);
    let external = layout.root().join("hardlink");
    fs::hard_link(transaction.join("payload/file"), &external).expect("hard link");

    assert_eq!(recover_abandoned_staging(&layout).expect("recovery"), 0);
    assert!(transaction.exists());
    assert_eq!(fs::read(external).expect("external hardlink"), b"payload");
}
