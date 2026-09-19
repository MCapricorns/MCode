use std::fs::{self, File};

#[cfg(unix)]
use mcode_config::HomeEnv;
use mcode_config::{
    AccessControlEvidence, BundlePath, ConfigErrorKind, HomeLayout, OwnedKind, begin_staging,
    ensure_home_layout, probe_access_control,
};

fn layout() -> (tempfile::TempDir, HomeLayout) {
    let parent = tempfile::tempdir().expect("parent");
    let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
    ensure_home_layout(&layout).expect("bootstrap");
    (parent, layout)
}

fn assert_writing(transaction: &std::path::Path) {
    let journal: serde_json::Value = serde_json::from_slice(
        &fs::read(transaction.join("journal.json")).expect("writing journal"),
    )
    .expect("valid journal");
    assert_eq!(journal["state"], "writing");
}

#[test]
fn staging_is_lazy_until_begin_and_publishes_exact_journals() {
    let (_parent, layout) = layout();
    assert!(!layout.host_staging_lock().exists());
    assert!(!layout.host_staging_dir().exists());

    let mut writing = begin_staging(&layout).expect("begin staging");
    let transaction = layout.transaction_staging_dir(writing.id());
    let writing_bytes = format!(
        "{{\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"{}\",\"state\":\"writing\"}}\n",
        writing.id()
    );
    assert_eq!(
        fs::read(transaction.join("journal.json")).expect("journal"),
        writing_bytes.as_bytes()
    );
    assert_eq!(
        sorted_names(&transaction),
        ["journal.json", "payload", "transaction.lock"]
    );
    assert_eq!(
        fs::metadata(layout.host_staging_lock())
            .expect("global lock")
            .len(),
        0
    );
    assert_eq!(
        fs::metadata(transaction.join("transaction.lock"))
            .expect("transaction lock")
            .len(),
        0
    );

    writing
        .write_file(&BundlePath::parse("bin/main.wasm").expect("path"), b"wasm")
        .expect("write payload");
    writing
        .write_file(&BundlePath::parse("empty").expect("path"), b"")
        .expect("write empty payload");
    let id = writing.id().as_str().to_owned();
    let staged = writing.finish().expect("finish staging");
    assert_eq!(staged.id().as_str(), id);
    let staged_bytes = format!(
        "{{\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"{id}\",\"state\":\"staged\"}}\n"
    );
    assert_eq!(
        fs::read(transaction.join("journal.json")).expect("journal"),
        staged_bytes.as_bytes()
    );
    assert!(!transaction.join(".journal.json.tmp").exists());
    assert!(!transaction.join("journal.json.lock").exists());
    assert_eq!(
        fs::read(transaction.join("payload/bin/main.wasm")).expect("payload"),
        b"wasm"
    );
}

#[test]
fn validation_can_retry_and_empty_finish_is_rejected() {
    let (_parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    let path = BundlePath::parse("a/b").expect("path");
    writing.write_file(&path, b"first").expect("first");
    assert_eq!(
        writing
            .write_file(&path, b"duplicate")
            .expect_err("duplicate")
            .kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert_eq!(
        writing
            .write_file(&BundlePath::parse("a").expect("path"), b"prefix")
            .expect_err("prefix")
            .kind(),
        ConfigErrorKind::AuthorityValidation
    );
    writing
        .write_file(&BundlePath::parse("c").expect("path"), b"retry")
        .expect("retry after validation");
    drop(writing.finish().expect("finish"));

    let empty = begin_staging(&layout).expect("empty begin");
    assert_eq!(
        empty.finish().err().expect("empty finish").kind(),
        ConfigErrorKind::AuthorityValidation
    );
}

#[test]
fn journal_compare_and_swap_rejects_every_noncanonical_writing_state_unchanged() {
    for (label, replacement) in [
        ("malformed", b"not-json\n".as_slice()),
        ("extra field", b"dynamic".as_slice()),
        ("wrong id", br#"{"formatVersion":1,"kind":"mcode-staging-transaction","transactionId":"00000000000000000000000000000000","state":"writing"}
"#),
        ("staged", br#"{"state":"staged"}
"#),
        ("committing", br#"{"state":"committing"}
"#),
        ("committed", br#"{"state":"committed"}
"#),
        ("unknown", br#"{"state":"future"}
"#),
    ] {
        let (_parent, layout) = layout();
        let mut writing = begin_staging(&layout).expect("begin");
        let transaction = layout.transaction_staging_dir(writing.id());
        writing
            .write_file(&BundlePath::parse("file").expect("path"), b"known")
            .expect("write");
        let journal = transaction.join("journal.json");
        let replacement = if label == "extra field" {
            format!(
                "{{\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"{}\",\"state\":\"writing\",\"extra\":true}}\n",
                writing.id()
            )
            .into_bytes()
        } else {
            replacement.to_vec()
        };
        fs::write(&journal, &replacement).expect("replace journal bytes");
        assert_eq!(
            writing.finish().err().expect(label).kind(),
            ConfigErrorKind::AuthorityValidation,
            "{label}"
        );
        assert_eq!(
            fs::read(journal).expect("unchanged journal"),
            replacement,
            "{label}"
        );
        assert!(!transaction.join(".journal.json.tmp").exists(), "{label}");
    }
}

#[test]
fn invalid_current_journal_precedes_existing_temporary_file() {
    let (_parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    writing
        .write_file(&BundlePath::parse("file").expect("path"), b"known")
        .expect("write");
    let journal = transaction.join("journal.json");
    let temporary = transaction.join(".journal.json.tmp");
    fs::write(&journal, b"not-json\n").expect("invalid current journal");
    fs::write(&temporary, b"retained crash temporary").expect("existing temporary");

    assert_eq!(
        writing.finish().err().expect("invalid journal").kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert_eq!(
        fs::read(&journal).expect("unchanged journal"),
        b"not-json\n"
    );
    assert_eq!(
        fs::read(&temporary).expect("unchanged temporary"),
        b"retained crash temporary"
    );
}

#[test]
fn same_size_payload_replacement_is_rejected_as_authority_mismatch() {
    let (_parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    writing
        .write_file(&BundlePath::parse("file").expect("path"), b"same")
        .expect("write");
    let file = transaction.join("payload/file");
    let moved = transaction.join("payload/moved");
    fs::rename(&file, &moved).expect("move original");
    fs::copy(&moved, &file).expect("same-size replacement");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("private replacement");
    }

    assert_eq!(
        writing.finish().err().expect("replacement").kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert_writing(&transaction);
}

#[test]
fn payload_directory_replacement_is_rejected_as_authority_mismatch() {
    let (_parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    writing
        .write_file(&BundlePath::parse("dir/file").expect("path"), b"x")
        .expect("write");
    let directory = transaction.join("payload/dir");
    let moved = transaction.join("payload/moved");
    fs::rename(&directory, &moved).expect("move original directory");
    fs::create_dir(&directory).expect("replacement directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .expect("private replacement");
    }
    fs::copy(moved.join("file"), directory.join("file")).expect("replacement child");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory.join("file"), fs::Permissions::from_mode(0o600))
            .expect("private child");
    }

    assert_eq!(
        writing.finish().err().expect("replacement").kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert_writing(&transaction);
}

#[test]
fn hard_linked_payload_is_rejected_as_authority_mismatch() {
    let (parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    writing
        .write_file(&BundlePath::parse("file").expect("path"), b"x")
        .expect("write");
    fs::hard_link(
        transaction.join("payload/file"),
        parent.path().join("outside-hard-link"),
    )
    .expect("hard link");

    assert_eq!(
        writing.finish().err().expect("hard link").kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert_writing(&transaction);
}

#[cfg(unix)]
fn replace_known_payload_with_special(create: impl FnOnce(&std::path::Path, &std::path::Path)) {
    replace_known_payload_with_special_in(tempfile::tempdir().expect("parent"), create);
}

#[cfg(unix)]
fn replace_known_payload_with_special_in(
    parent: tempfile::TempDir,
    create: impl FnOnce(&std::path::Path, &std::path::Path),
) {
    let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
    ensure_home_layout(&layout).expect("bootstrap");
    let mut writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    writing
        .write_file(&BundlePath::parse("file").expect("path"), b"x")
        .expect("write");
    let file = transaction.join("payload/file");
    fs::remove_file(&file).expect("remove regular file");
    create(&file, parent.path());
    assert_eq!(
        writing.finish().err().expect("special object").kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert_writing(&transaction);
}

/// A parent directory short enough to hold a `sun_path` fixture.
///
/// Darwin caps `sun_path` at 104 bytes, and the per-user macOS temporary
/// directory already exceeds that before the staging path suffix.
#[cfg(unix)]
fn short_socket_parent() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("mcode-sock-")
        .tempdir_in("/tmp")
        .expect("short socket parent")
}

#[cfg(unix)]
#[test]
fn known_payload_fifo_is_rejected_without_opening_it() {
    replace_known_payload_with_special(|file, _outside| {
        assert!(
            std::process::Command::new("mkfifo")
                .arg(file)
                .status()
                .expect("run mkfifo")
                .success()
        );
    });
}

#[cfg(unix)]
#[test]
fn existing_global_lock_fifo_is_rejected_without_opening_it() {
    let (_parent, layout) = layout();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(layout.host_staging_lock())
            .status()
            .expect("run mkfifo")
            .success()
    );

    assert_eq!(
        begin_staging(&layout)
            .err()
            .expect("special global lock")
            .kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert!(!layout.host_staging_dir().exists());
}

#[cfg(unix)]
#[test]
fn known_payload_symlink_is_rejected_without_following_it() {
    replace_known_payload_with_special(|file, outside| {
        let target = outside.join("outside-target");
        fs::write(&target, b"outside").expect("outside target");
        std::os::unix::fs::symlink(target, file).expect("known symlink");
    });
}

#[cfg(unix)]
#[test]
fn known_payload_socket_is_rejected_without_opening_it() {
    replace_known_payload_with_special_in(short_socket_parent(), |file, _outside| {
        std::os::unix::net::UnixListener::bind(file).expect("known socket");
    });
}

#[cfg(unix)]
#[test]
fn root_rename_blocks_finish_without_repair() {
    let (parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    writing
        .write_file(&BundlePath::parse("file").expect("path"), b"x")
        .expect("write");
    let moved = parent.path().join("moved-home");
    fs::rename(layout.root(), &moved).expect("rename retained root");

    assert_eq!(
        writing.finish().err().expect("renamed root").kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert!(
        !layout.root().exists(),
        "finish must not recreate the root alias"
    );
}

#[cfg(unix)]
#[test]
fn explicit_root_wrong_case_alias_is_rejected() {
    use std::os::unix::fs::PermissionsExt;

    let parent = tempfile::tempdir().expect("parent");
    let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
    ensure_home_layout(&layout).expect("bootstrap");
    let alias = parent.path().join("HOME");
    match fs::create_dir(&alias) {
        Ok(()) => fs::set_permissions(&alias, fs::Permissions::from_mode(0o700))
            .expect("private wrong-case sibling"),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            fs::rename(layout.root(), &alias).expect("case-only root rename");
        }
        Err(error) => panic!("wrong-case fixture: {error}"),
    }

    assert_eq!(
        begin_staging(&layout)
            .err()
            .expect("wrong-case explicit root")
            .kind(),
        ConfigErrorKind::AuthorityValidation
    );
}

#[cfg(windows)]
#[test]
fn windows_case_only_root_rename_is_rejected() {
    let (parent, layout) = layout();
    fs::rename(layout.root(), parent.path().join("HOME")).expect("case-only root rename");

    assert_eq!(
        begin_staging(&layout)
            .err()
            .expect("renamed explicit root")
            .kind(),
        ConfigErrorKind::InvalidHome
    );
}

#[cfg(windows)]
#[test]
fn windows_non_ascii_case_only_root_rename_is_rejected() {
    let parent = tempfile::tempdir().expect("parent");
    let layout = HomeLayout::from_root(parent.path().join("Ångström")).expect("layout");
    ensure_home_layout(&layout).expect("bootstrap");
    fs::rename(layout.root(), parent.path().join("ångström")).expect("Unicode case-only rename");

    assert_eq!(
        begin_staging(&layout)
            .err()
            .expect("renamed Unicode root")
            .kind(),
        ConfigErrorKind::InvalidHome
    );
}

#[cfg(unix)]
#[test]
fn large_root_parent_does_not_impose_staging_capacity() {
    let user_home = tempfile::tempdir().expect("user home");
    let layout = HomeLayout::from_env(HomeEnv {
        home: Some(user_home.path().as_os_str().to_owned()),
        ..HomeEnv::default()
    })
    .expect("layout");
    ensure_home_layout(&layout).expect("bootstrap");
    for index in 0..9_225 {
        fs::write(user_home.path().join(format!("sibling-{index}")), []).expect("sibling");
    }

    drop(begin_staging(&layout).expect("unbounded root spelling scan"));
}

#[test]
fn staging_root_accepts_its_last_slot_and_rejects_one_over() {
    const FROZEN_ROOT_ENTRIES: usize = 1_024;

    let (_parent, layout) = layout();
    drop(begin_staging(&layout).expect("create staging root"));
    let staging = layout.host_staging_dir();
    let existing = sorted_names(&staging).len();
    assert_eq!(existing, 1);
    for index in existing..FROZEN_ROOT_ENTRIES - 1 {
        fs::write(staging.join(format!("capacity-{index}")), []).expect("capacity fixture");
    }
    assert_eq!(sorted_names(&staging).len(), FROZEN_ROOT_ENTRIES - 1);

    drop(begin_staging(&layout).expect("last root slot"));
    assert_eq!(sorted_names(&staging).len(), FROZEN_ROOT_ENTRIES);
    assert_eq!(
        begin_staging(&layout)
            .err()
            .expect("over root capacity")
            .kind(),
        ConfigErrorKind::Oversized
    );
    assert_eq!(sorted_names(&staging).len(), FROZEN_ROOT_ENTRIES);
}

#[test]
fn global_lock_blocks_begin_only_until_publication_lock_is_released() {
    let (_parent, layout) = layout();
    drop(begin_staging(&layout).expect("create valid global lock"));
    let lock = File::options()
        .read(true)
        .write(true)
        .open(layout.host_staging_lock())
        .expect("global lock");
    File::lock(&lock).expect("hold global lock");
    let (sent, received) = std::sync::mpsc::channel();
    let contender_layout = layout.clone();
    let contender = std::thread::spawn(move || {
        sent.send(begin_staging(&contender_layout).map(|writer| writer.id().as_str().to_owned()))
            .expect("send result");
    });
    assert!(
        received
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "begin must remain blocked while the global lock is held"
    );
    File::unlock(&lock).expect("release global lock");
    assert!(
        received
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("begin completes after unlock")
            .is_ok()
    );
    contender.join().expect("contender thread");
}

#[test]
fn wrong_case_staging_alias_is_rejected_without_canonical_creation() {
    let (_parent, layout) = layout();
    let alias = layout.plugins_dir().join(".STAGING");
    fs::create_dir(&alias).expect("wrong-case alias");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&alias, fs::Permissions::from_mode(0o700)).expect("private alias");
    }

    assert_eq!(
        begin_staging(&layout).err().expect("wrong case").kind(),
        ConfigErrorKind::AuthorityValidation
    );
    let spellings = sorted_names(&layout.plugins_dir());
    assert!(spellings.iter().any(|name| name == ".STAGING"));
    assert!(!spellings.iter().any(|name| name == ".staging"));
}

#[test]
fn wrong_case_fixed_transaction_child_blocks_finish_without_repair() {
    let (_parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    writing
        .write_file(&BundlePath::parse("file").expect("path"), b"x")
        .expect("write");
    let payload = transaction.join("payload");
    let alias = transaction.join("Payload");
    fs::rename(&payload, &alias).expect("case alias");

    assert_eq!(
        writing.finish().err().expect("wrong-case payload").kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert!(
        !sorted_names(&transaction)
            .iter()
            .any(|name| name == "payload")
    );
    assert!(
        sorted_names(&transaction)
            .iter()
            .any(|name| name == "Payload")
    );
}

#[test]
fn unknown_payload_file_blocks_staged_publication() {
    let (_parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    writing
        .write_file(&BundlePath::parse("file").expect("path"), b"known")
        .expect("write");
    fs::write(transaction.join("payload/extra"), b"unknown").expect("extra file");

    assert_eq!(
        writing.finish().err().expect("unknown payload").kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert_writing(&transaction);
}

#[test]
fn unknown_payload_directory_blocks_staged_publication() {
    let (_parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    writing
        .write_file(&BundlePath::parse("file").expect("path"), b"known")
        .expect("write");
    fs::create_dir(transaction.join("payload/extra")).expect("extra directory");

    assert_eq!(
        writing.finish().err().expect("unknown payload").kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert_writing(&transaction);
}

#[cfg(unix)]
#[test]
fn unknown_payload_symlink_blocks_staged_publication() {
    let (parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    writing
        .write_file(&BundlePath::parse("file").expect("path"), b"known")
        .expect("write");
    let outside = parent.path().join("outside");
    fs::write(&outside, b"outside").expect("outside file");
    std::os::unix::fs::symlink(&outside, transaction.join("payload/extra")).expect("extra symlink");

    assert_eq!(
        writing.finish().err().expect("unknown payload").kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert_writing(&transaction);
}

#[cfg(windows)]
#[test]
fn unknown_payload_reparse_blocks_staged_publication() {
    let (parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    writing
        .write_file(&BundlePath::parse("file").expect("path"), b"known")
        .expect("write");
    let outside = parent.path().join("outside");
    fs::create_dir(&outside).expect("outside directory");
    junction::create(&outside, transaction.join("payload/extra")).expect("extra junction");

    assert_eq!(
        writing.finish().err().expect("unknown payload").kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert_writing(&transaction);
}

#[test]
fn unknown_transaction_entry_blocks_staged_publication() {
    let (_parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    writing
        .write_file(&BundlePath::parse("file").expect("path"), b"known")
        .expect("write");
    fs::write(transaction.join("extra"), b"unknown").expect("extra root entry");

    assert_eq!(
        writing.finish().err().expect("unknown root entry").kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert_writing(&transaction);
}

#[test]
fn per_file_size_change_is_not_hidden_by_unchanged_total() {
    let (_parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    writing
        .write_file(&BundlePath::parse("first").expect("path"), b"a")
        .expect("first write");
    writing
        .write_file(&BundlePath::parse("second").expect("path"), b"bbb")
        .expect("second write");
    fs::write(transaction.join("payload/first"), b"aa").expect("grow first");
    fs::write(transaction.join("payload/second"), b"bb").expect("shrink second");

    assert_eq!(
        writing.finish().err().expect("changed file sizes").kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert_writing(&transaction);
}

#[test]
fn replaced_transaction_lock_blocks_staged_publication() {
    let (_parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    writing
        .write_file(&BundlePath::parse("file").expect("path"), b"known")
        .expect("write");
    let lock = transaction.join("transaction.lock");
    fs::rename(&lock, layout.host_staging_dir().join("moved-lock")).expect("move retained lock");
    fs::write(&lock, []).expect("replacement lock");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o600))
            .expect("private replacement lock");
    }

    assert_eq!(
        writing
            .finish()
            .err()
            .expect("replacement lock must be rejected")
            .kind(),
        ConfigErrorKind::AuthorityValidation
    );
    assert_writing(&transaction);
}

#[test]
fn transaction_lock_remains_active_through_staged_guard() {
    let (_parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    let lock_path = layout
        .transaction_staging_dir(writing.id())
        .join("transaction.lock");
    let contender = File::open(&lock_path).expect("open lock contender");
    assert!(matches!(
        File::try_lock(&contender),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    writing
        .write_file(&BundlePath::parse("file").expect("path"), b"x")
        .expect("write");
    let staged = writing.finish().expect("finish");
    assert!(matches!(
        File::try_lock(&contender),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(staged);
    File::try_lock(&contender).expect("lock released by staged guard");
    File::unlock(&contender).expect("unlock contender");
}

#[test]
fn production_staging_avoids_path_recursive_filesystem_apis() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "src/staging.rs",
        "src/secure_fs/fallback_staging.rs",
        "src/secure_fs/unix_staging.rs",
        "src/secure_fs/unix_staging_recovery.rs",
        "src/secure_fs/windows_open.rs",
        "src/secure_fs/windows_staging.rs",
        "src/secure_fs/windows_staging_journal.rs",
        "src/secure_fs/windows_staging_recovery.rs",
    ] {
        let source = fs::read_to_string(manifest.join(relative)).expect("staging source");
        for forbidden in [
            "std::fs::read_dir",
            "fs::read_dir",
            "remove_dir_all",
            ".read_dir(",
            "std::fs::remove_file",
            "std::fs::remove_dir",
            "fs::remove_file",
            "fs::remove_dir",
        ] {
            assert!(
                !source.contains(forbidden),
                "{relative} introduced forbidden {forbidden}"
            );
        }
        for line in source.lines() {
            for forbidden_call in ["remove_file(", "remove_dir("] {
                for (offset, _) in line.match_indices(forbidden_call) {
                    let prefix = &line[..offset];
                    let declaration = prefix.trim_end().ends_with("fn");
                    assert!(
                        declaration,
                        "{relative} introduced path deletion {forbidden_call}"
                    );
                }
            }
        }
    }
}

#[test]
fn staging_objects_have_exact_private_access() {
    let (_parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    writing
        .write_file(&BundlePath::parse("dir/file").expect("path"), b"x")
        .expect("write");

    for path in [
        layout.host_staging_dir(),
        transaction.clone(),
        transaction.join("payload"),
        transaction.join("payload/dir"),
    ] {
        assert_private(probe_access_control(&path), OwnedKind::Directory);
    }
    for path in [
        layout.host_staging_lock(),
        transaction.join("transaction.lock"),
        transaction.join("journal.json"),
        transaction.join("payload/dir/file"),
    ] {
        assert_private(probe_access_control(&path), OwnedKind::File);
    }
}

#[cfg(unix)]
#[test]
fn unix_staging_objects_are_same_device_private_and_link_count_one() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let (_parent, layout) = layout();
    let mut writing = begin_staging(&layout).expect("begin");
    let transaction = layout.transaction_staging_dir(writing.id());
    writing
        .write_file(&BundlePath::parse("dir/file").expect("path"), b"x")
        .expect("write");
    let device = fs::metadata(layout.plugins_dir())
        .expect("plugins metadata")
        .dev();
    for path in [
        layout.host_staging_dir(),
        transaction.clone(),
        transaction.join("payload"),
        transaction.join("payload/dir"),
    ] {
        let metadata = fs::symlink_metadata(path).expect("directory metadata");
        assert!(metadata.is_dir());
        assert_eq!(metadata.dev(), device);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    }
    for path in [
        layout.host_staging_lock(),
        transaction.join("transaction.lock"),
        transaction.join("journal.json"),
        transaction.join("payload/dir/file"),
    ] {
        let metadata = fs::symlink_metadata(path).expect("file metadata");
        assert!(metadata.is_file());
        assert_eq!(metadata.dev(), device);
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }
}

fn sorted_names(path: &std::path::Path) -> Vec<String> {
    let mut names = fs::read_dir(path)
        .expect("listing")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn assert_private(evidence: AccessControlEvidence, expected: OwnedKind) {
    match evidence {
        AccessControlEvidence::UnixMode { kind, mode } => {
            assert_eq!(kind, expected);
            assert_eq!(
                mode,
                if expected == OwnedKind::Directory {
                    0o700
                } else {
                    0o600
                }
            );
        }
        AccessControlEvidence::WindowsProtectedDacl {
            kind,
            owner_current_user,
            current_user,
            system,
            protected,
            extra_aces,
            ..
        } => {
            assert_eq!(kind, expected);
            assert!(owner_current_user && current_user && system && protected);
            assert_eq!(extra_aces, 0);
        }
        AccessControlEvidence::Unavailable { platform, reason } => {
            panic!("native access evidence unavailable on {platform}: {reason:?}")
        }
    }
}
