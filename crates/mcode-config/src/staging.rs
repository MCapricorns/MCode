//! Bounded native writer for Host staging transactions.
//!
//! The staging substrate establishes durable, private, same-volume mechanical
//! state and recovers fully validated abandoned transactions. It does not verify
//! bundle trust, signatures, digests, inventory completeness, or activation.

use std::collections::{BTreeMap, BTreeSet};

use crate::{BundlePath, ConfigError, ConfigErrorKind, HomeLayout, TransactionId};

use crate::secure_fs::staging_platform as platform;

/// Maximum encoded staging journal size: 1 KiB.
pub const MAX_STAGING_JOURNAL_BYTES: usize = 1024;
/// Maximum number of direct entries in `.staging/`.
pub const MAX_STAGING_ROOT_ENTRIES: usize = 1024;
/// Maximum number of payload regular files.
pub const MAX_STAGING_FILES: usize = 4096;
/// Maximum number of structural payload directories.
pub const MAX_STAGING_DIRECTORIES: usize = 4096;
/// Maximum combined number of payload files and directories.
pub const MAX_STAGING_ENTRIES: usize = 8192;
/// Maximum byte length of one payload file: 256 MiB.
pub const MAX_STAGING_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum logical byte length of one payload: 512 MiB.
pub const MAX_STAGING_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

const STAGING_JOURNAL_KIND: &str = "mcode-staging-transaction";

#[cfg(any(unix, windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JournalState {
    Writing,
    Staged,
    Committing,
    Committed,
}

pub(crate) struct WriteFailure {
    pub(crate) error: ConfigError,
    pub(crate) mutation_started: bool,
}

#[derive(Clone, Copy)]
struct LedgerLimits {
    files: usize,
    directories: usize,
    entries: usize,
    file_bytes: u64,
    total_bytes: u64,
}

const PUBLIC_LIMITS: LedgerLimits = LedgerLimits {
    files: MAX_STAGING_FILES,
    directories: MAX_STAGING_DIRECTORIES,
    entries: MAX_STAGING_ENTRIES,
    file_bytes: MAX_STAGING_FILE_BYTES,
    total_bytes: MAX_STAGING_TOTAL_BYTES,
};

#[derive(Default)]
struct PayloadLedger {
    files: BTreeMap<String, u64>,
    directories: BTreeSet<String>,
    total_bytes: u64,
}

#[derive(Debug)]
struct PlannedWrite {
    path: String,
    new_directories: Vec<String>,
    size: u64,
}

impl PayloadLedger {
    fn plan(
        &self,
        path: &BundlePath,
        size: usize,
        limits: LedgerLimits,
    ) -> Result<PlannedWrite, ConfigError> {
        let size = u64::try_from(size).map_err(|_| oversized())?;
        if size > limits.file_bytes {
            return Err(oversized());
        }
        let path = path.as_str();
        if self.files.contains_key(path) || self.directories.contains(path) {
            return Err(validation_error());
        }

        let components = path.split('/').collect::<Vec<_>>();
        let mut new_directories = Vec::new();
        let mut prefix = String::new();
        for component in &components[..components.len().saturating_sub(1)] {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            if self.files.contains_key(&prefix) {
                return Err(validation_error());
            }
            if !self.directories.contains(&prefix) {
                new_directories.push(prefix.clone());
            }
        }
        let descendant_prefix = format!("{path}/");
        if self
            .files
            .range(descendant_prefix.clone()..)
            .next()
            .is_some_and(|(entry, _)| entry.starts_with(&descendant_prefix))
            || self
                .directories
                .range(descendant_prefix.clone()..)
                .next()
                .is_some_and(|entry| entry.starts_with(&descendant_prefix))
        {
            return Err(validation_error());
        }

        let file_count = self.files.len().checked_add(1).ok_or_else(oversized)?;
        let directory_count = self
            .directories
            .len()
            .checked_add(new_directories.len())
            .ok_or_else(oversized)?;
        let entry_count = file_count
            .checked_add(directory_count)
            .ok_or_else(oversized)?;
        let total_bytes = self.total_bytes.checked_add(size).ok_or_else(oversized)?;
        if file_count > limits.files
            || directory_count > limits.directories
            || entry_count > limits.entries
            || total_bytes > limits.total_bytes
        {
            return Err(oversized());
        }
        Ok(PlannedWrite {
            path: path.to_owned(),
            new_directories,
            size,
        })
    }

    fn apply(&mut self, plan: PlannedWrite) {
        self.files.insert(plan.path, plan.size);
        self.directories.extend(plan.new_directories);
        self.total_bytes += plan.size;
    }
}

/// Holds an exclusively locked transaction while payload files are written.
#[must_use = "dropping the writer abandons its transaction and releases the transaction lock"]
pub struct StagingTransaction {
    id: TransactionId,
    native: platform::Transaction,
    ledger: PayloadLedger,
    poisoned: bool,
}

impl StagingTransaction {
    /// Returns the Host-generated transaction identifier.
    #[must_use]
    pub fn id(&self) -> &TransactionId {
        &self.id
    }

    /// Exclusively creates and durably writes one canonical payload file.
    ///
    /// Lexical and ledger failures happen before native mutation and may be
    /// retried. A failure after native payload mutation poisons this writer.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::Oversized`] for fixed count or byte bounds,
    /// [`ConfigErrorKind::AuthorityValidation`] for duplicate and prefix
    /// conflicts, and native security, lock, or I/O failures otherwise.
    pub fn write_file(&mut self, path: &BundlePath, bytes: &[u8]) -> Result<(), ConfigError> {
        if self.poisoned {
            return Err(validation_error());
        }
        let plan = self.ledger.plan(path, bytes.len(), PUBLIC_LIMITS)?;
        match self
            .native
            .write_file(&plan.path, &plan.new_directories, bytes, plan.size)
        {
            Ok(()) => {
                self.ledger.apply(plan);
                Ok(())
            }
            Err(failure) => {
                self.poisoned |= failure.mutation_started;
                Err(failure.error)
            }
        }
    }

    /// Publishes `staged` after revalidating the complete written payload.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] for an empty or
    /// poisoned writer and [`ConfigError`] for failed identity, access,
    /// journal, or durability validation.
    pub fn finish(mut self) -> Result<StagedTransaction, ConfigError> {
        if self.poisoned || self.ledger.files.is_empty() {
            return Err(validation_error());
        }
        let writing = journal_bytes(&self.id, "writing")?;
        let staged = journal_bytes(&self.id, "staged")?;
        if let Err(error) = self.native.finish(
            &self.ledger.files,
            &self.ledger.directories,
            self.ledger.total_bytes,
            &writing,
            &staged,
        ) {
            self.poisoned = true;
            return Err(error);
        }
        Ok(StagedTransaction {
            id: self.id,
            _native: self.native,
        })
    }
}

/// Retains the exclusive transaction lock for a proven staged payload.
#[must_use = "dropping the staged guard releases the transaction lock before a durable claim"]
pub struct StagedTransaction {
    id: TransactionId,
    _native: platform::Transaction,
}

impl StagedTransaction {
    /// Returns the Host-generated transaction identifier.
    #[must_use]
    pub fn id(&self) -> &TransactionId {
        &self.id
    }
}

/// Begins one lazy native staging transaction under the global lock.
///
/// The operation verifies the retained home and plugins anchors, creates only
/// staging protocol objects, enforces the bounded direct-root capacity, and
/// publishes `writing` before releasing the global lock.
///
/// # Errors
///
/// Returns [`ConfigError`] for missing or invalid owned anchors, staging-root
/// capacity, random generation, lock, access, identity, or durability failure.
pub fn begin_staging(home: &HomeLayout) -> Result<StagingTransaction, ConfigError> {
    let (id, native) = platform::Transaction::begin(home, |id| journal_bytes(id, "writing"))?;
    Ok(StagingTransaction {
        id,
        native,
        ledger: PayloadLedger::default(),
        poisoned: false,
    })
}

/// Durably removes fully validated abandoned staging transactions.
///
/// The operation creates nothing. It preserves busy, claimed, malformed, raced,
/// or otherwise unprovable candidates and counts only transaction roots whose
/// deletion and final staging-directory barrier both completed.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::Oversized`] before mutation when the staging root
/// exceeds its fixed bound. Returns
/// [`ConfigErrorKind::RecoveryIndeterminate`] when deletion, durability, or
/// identity proof fails after the first deletion succeeds, and native security,
/// lock, or I/O failures otherwise.
pub fn recover_abandoned_staging(home: &HomeLayout) -> Result<usize, ConfigError> {
    platform::recover_abandoned(home)
}

#[cfg(any(unix, windows, test))]
pub(crate) fn parse_journal(bytes: &[u8], id: &TransactionId) -> Option<JournalState> {
    if bytes.len() > MAX_STAGING_JOURNAL_BYTES {
        return None;
    }
    for (state, name) in [
        (JournalState::Writing, "writing"),
        (JournalState::Staged, "staged"),
        (JournalState::Committing, "committing"),
        (JournalState::Committed, "committed"),
    ] {
        if journal_bytes(id, name).ok().as_deref() == Some(bytes) {
            return Some(state);
        }
    }
    None
}

fn journal_bytes(id: &TransactionId, state: &str) -> Result<Vec<u8>, ConfigError> {
    let bytes = format!(
        "{{\"formatVersion\":1,\"kind\":\"{STAGING_JOURNAL_KIND}\",\"transactionId\":\"{}\",\"state\":\"{state}\"}}\n",
        id.as_str()
    )
    .into_bytes();
    if bytes.len() > MAX_STAGING_JOURNAL_BYTES {
        return Err(ConfigError::new(ConfigErrorKind::Serialization));
    }
    Ok(bytes)
}

fn validation_error() -> ConfigError {
    ConfigError::new(ConfigErrorKind::AuthorityValidation)
}

fn oversized() -> ConfigError {
    ConfigError::new(ConfigErrorKind::Oversized)
}

#[cfg(test)]
mod tests {
    use super::{JournalState, LedgerLimits, PayloadLedger};
    use crate::{BundlePath, ConfigErrorKind, HomeLayout, TransactionId, ensure_home_layout};

    const SMALL: LedgerLimits = LedgerLimits {
        files: 2,
        directories: 2,
        entries: 4,
        file_bytes: 4,
        total_bytes: 6,
    };

    #[test]
    fn journal_parser_accepts_only_fixed_canonical_bytes() {
        let id = TransactionId::parse_persistent("tx1-0123456789abcdef0123456789abcdef")
            .expect("transaction ID");
        for (bytes, state) in [
            (b"{\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"tx1-0123456789abcdef0123456789abcdef\",\"state\":\"writing\"}\n".as_slice(), JournalState::Writing),
            (b"{\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"tx1-0123456789abcdef0123456789abcdef\",\"state\":\"staged\"}\n".as_slice(), JournalState::Staged),
            (b"{\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"tx1-0123456789abcdef0123456789abcdef\",\"state\":\"committing\"}\n".as_slice(), JournalState::Committing),
            (b"{\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"tx1-0123456789abcdef0123456789abcdef\",\"state\":\"committed\"}\n".as_slice(), JournalState::Committed),
        ] {
            assert_eq!(super::parse_journal(bytes, &id), Some(state));
        }

        for (label, bytes) in [
            ("reordered", b"{\"kind\":\"mcode-staging-transaction\",\"formatVersion\":1,\"transactionId\":\"tx1-0123456789abcdef0123456789abcdef\",\"state\":\"writing\"}\n".as_slice()),
            ("duplicate", b"{\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"tx1-0123456789abcdef0123456789abcdef\",\"state\":\"writing\",\"state\":\"writing\"}\n".as_slice()),
            ("unknown", b"{\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"tx1-0123456789abcdef0123456789abcdef\",\"state\":\"writing\",\"extra\":0}\n".as_slice()),
            ("missing", b"{\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"tx1-0123456789abcdef0123456789abcdef\"}\n".as_slice()),
            ("wrong type", b"{\"formatVersion\":\"1\",\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"tx1-0123456789abcdef0123456789abcdef\",\"state\":\"writing\"}\n".as_slice()),
            ("wrong kind", b"{\"formatVersion\":1,\"kind\":\"other\",\"transactionId\":\"tx1-0123456789abcdef0123456789abcdef\",\"state\":\"writing\"}\n".as_slice()),
            ("wrong id", b"{\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"tx1-fedcba9876543210fedcba9876543210\",\"state\":\"writing\"}\n".as_slice()),
            ("future version", b"{\"formatVersion\":2,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"tx1-0123456789abcdef0123456789abcdef\",\"state\":\"writing\"}\n".as_slice()),
            ("future state", b"{\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"tx1-0123456789abcdef0123456789abcdef\",\"state\":\"future\"}\n".as_slice()),
            ("state type", b"{\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"tx1-0123456789abcdef0123456789abcdef\",\"state\":0}\n".as_slice()),
            ("CRLF", b"{\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"tx1-0123456789abcdef0123456789abcdef\",\"state\":\"writing\"}\r\n".as_slice()),
            ("trailing", b"{\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"tx1-0123456789abcdef0123456789abcdef\",\"state\":\"writing\"}\n\n".as_slice()),
            ("whitespace", b" {\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"tx1-0123456789abcdef0123456789abcdef\",\"state\":\"writing\"}\n".as_slice()),
            ("non UTF-8", b"\xff\xfe\n".as_slice()),
        ] {
            assert_eq!(super::parse_journal(bytes, &id), None, "{label}");
        }
        assert_eq!(
            super::parse_journal(&vec![b'x'; super::MAX_STAGING_JOURNAL_BYTES + 1], &id),
            None
        );
    }

    fn path(value: &str) -> BundlePath {
        BundlePath::parse(value).expect("bundle path")
    }

    fn add(ledger: &mut PayloadLedger, value: &str, size: usize) {
        let plan = ledger.plan(&path(value), size, SMALL).expect("plan");
        ledger.apply(plan);
    }

    #[test]
    fn public_limits_are_frozen_and_wired_to_the_ledger() {
        assert_eq!(super::MAX_STAGING_JOURNAL_BYTES, 1_024);
        assert_eq!(super::MAX_STAGING_ROOT_ENTRIES, 1_024);
        assert_eq!(super::MAX_STAGING_FILES, 4_096);
        assert_eq!(super::MAX_STAGING_DIRECTORIES, 4_096);
        assert_eq!(super::MAX_STAGING_ENTRIES, 8_192);
        assert_eq!(super::MAX_STAGING_FILE_BYTES, 256 * 1_024 * 1_024);
        assert_eq!(super::MAX_STAGING_TOTAL_BYTES, 512 * 1_024 * 1_024);
        assert_eq!(super::PUBLIC_LIMITS.files, 4_096);
        assert_eq!(super::PUBLIC_LIMITS.directories, 4_096);
        assert_eq!(super::PUBLIC_LIMITS.entries, 8_192);
        assert_eq!(super::PUBLIC_LIMITS.file_bytes, 256 * 1_024 * 1_024);
        assert_eq!(super::PUBLIC_LIMITS.total_bytes, 512 * 1_024 * 1_024);
    }

    #[test]
    fn ledger_rejects_duplicates_and_both_prefix_conflict_directions() {
        let mut ledger = PayloadLedger::default();
        add(&mut ledger, "a/b", 1);
        for value in ["a/b", "a", "a/b/c"] {
            assert_eq!(
                ledger
                    .plan(&path(value), 1, SMALL)
                    .expect_err("conflict")
                    .kind(),
                ConfigErrorKind::AuthorityValidation
            );
        }
    }

    #[test]
    fn ledger_checks_each_independent_bound_without_large_allocations() {
        for (limits, existing, candidate, size, reason) in [
            (
                LedgerLimits {
                    file_bytes: 0,
                    ..SMALL
                },
                vec![],
                "a",
                1,
                "file bytes",
            ),
            (
                LedgerLimits {
                    total_bytes: 1,
                    ..SMALL
                },
                vec![("a", 1)],
                "b",
                1,
                "total bytes",
            ),
            (
                LedgerLimits { files: 1, ..SMALL },
                vec![("a", 0)],
                "b",
                0,
                "files",
            ),
            (
                LedgerLimits {
                    directories: 1,
                    ..SMALL
                },
                vec![],
                "a/b/c",
                0,
                "directories",
            ),
            (
                LedgerLimits {
                    entries: 1,
                    ..SMALL
                },
                vec![],
                "a/b",
                0,
                "combined entries",
            ),
        ] {
            let mut ledger = PayloadLedger::default();
            for (path, size) in existing {
                let plan = ledger
                    .plan(&self::path(path), size, SMALL)
                    .expect("fixture plan");
                ledger.apply(plan);
            }
            assert_eq!(
                ledger
                    .plan(&path(candidate), size, limits)
                    .expect_err(reason)
                    .kind(),
                ConfigErrorKind::Oversized,
                "{reason} bound"
            );
        }
    }

    #[test]
    fn ledger_accepts_exact_bounds_and_rejects_one_over() {
        for (label, exact_limits, exact_existing, exact_path, over_limits) in [
            (
                "file bytes",
                LedgerLimits {
                    file_bytes: 1,
                    ..SMALL
                },
                vec![],
                ("a", 1),
                LedgerLimits {
                    file_bytes: 0,
                    ..SMALL
                },
            ),
            (
                "total bytes",
                LedgerLimits {
                    total_bytes: 2,
                    ..SMALL
                },
                vec![("a", 1)],
                ("b", 1),
                LedgerLimits {
                    total_bytes: 1,
                    ..SMALL
                },
            ),
            (
                "file count",
                LedgerLimits { files: 2, ..SMALL },
                vec![("a", 0)],
                ("b", 0),
                LedgerLimits { files: 1, ..SMALL },
            ),
            (
                "directory count",
                LedgerLimits {
                    directories: 2,
                    ..SMALL
                },
                vec![],
                ("a/b/c", 0),
                LedgerLimits {
                    directories: 1,
                    ..SMALL
                },
            ),
            (
                "combined entries",
                LedgerLimits {
                    entries: 2,
                    ..SMALL
                },
                vec![],
                ("a/b", 0),
                LedgerLimits {
                    entries: 1,
                    ..SMALL
                },
            ),
        ] {
            let mut exact = PayloadLedger::default();
            let mut over = PayloadLedger::default();
            for (path, size) in exact_existing {
                let exact_plan = exact
                    .plan(&self::path(path), size, SMALL)
                    .expect("exact fixture");
                exact.apply(exact_plan);
                let over_plan = over
                    .plan(&self::path(path), size, SMALL)
                    .expect("over fixture");
                over.apply(over_plan);
            }
            assert!(
                exact
                    .plan(&path(exact_path.0), exact_path.1, exact_limits)
                    .is_ok(),
                "{label} exact"
            );
            assert_eq!(
                over.plan(&path(exact_path.0), exact_path.1, over_limits)
                    .expect_err(label)
                    .kind(),
                ConfigErrorKind::Oversized,
                "{label} one over"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn finish_journal_prepare_failure_keeps_writing_and_cleans_temp() {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        ensure_home_layout(&layout).expect("bootstrap");
        let mut writer = super::begin_staging(&layout).expect("begin");
        let transaction = layout.transaction_staging_dir(writer.id());
        writer.write_file(&path("file"), b"x").expect("write");
        super::platform::fail_next_journal_temp_prepare_for_test();
        assert_eq!(
            writer
                .finish()
                .err()
                .expect("injected finish failure")
                .kind(),
            ConfigErrorKind::Io
        );
        assert!(!transaction.join(".journal.json.tmp").exists());
        let bytes = std::fs::read(transaction.join("journal.json")).expect("writing journal");
        assert!(bytes.ends_with(b"\"state\":\"writing\"}\n"));
    }

    #[cfg(unix)]
    #[test]
    fn unix_journal_prepare_failure_retains_the_owned_temporary() {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        ensure_home_layout(&layout).expect("bootstrap");
        let mut writer = super::begin_staging(&layout).expect("begin");
        let transaction = layout.transaction_staging_dir(writer.id());
        writer.write_file(&path("file"), b"x").expect("write");
        super::platform::fail_next_journal_temp_prepare_for_test();
        assert_eq!(
            writer
                .finish()
                .err()
                .expect("injected finish failure")
                .kind(),
            ConfigErrorKind::Io
        );
        assert!(transaction.join(".journal.json.tmp").exists());
        let bytes = std::fs::read(transaction.join("journal.json")).expect("writing journal");
        assert!(bytes.ends_with(b"\"state\":\"writing\"}\n"));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn recovery_barrier_failure_is_indeterminate_after_root_deletion() {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        ensure_home_layout(&layout).expect("bootstrap");
        let writer = super::begin_staging(&layout).expect("begin");
        let transaction = layout.transaction_staging_dir(writer.id());
        drop(writer);
        super::platform::fail_next_recovery_staging_barrier_for_test();

        let error = super::recover_abandoned_staging(&layout).expect_err("barrier failure");
        assert_eq!(error.kind(), ConfigErrorKind::RecoveryIndeterminate);
        assert!(!transaction.exists());
    }

    #[cfg(unix)]
    #[test]
    fn home_root_replacement_while_waiting_for_global_lock_preserves_old_transaction() {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        ensure_home_layout(&layout).expect("bootstrap");
        let writer = super::begin_staging(&layout).expect("begin");
        let id = writer.id().as_str().to_owned();
        drop(writer);

        let lock = std::fs::File::options()
            .read(true)
            .write(true)
            .open(layout.host_staging_lock())
            .expect("global lock");
        std::fs::File::lock(&lock).expect("hold global lock");
        let (ready_sender, ready_receiver) = std::sync::mpsc::channel();
        let (result_sender, result_receiver) = std::sync::mpsc::channel();
        let recovery_layout = layout.clone();
        let recovery = std::thread::spawn(move || {
            super::platform::notify_on_recovery_global_lock_wait_for_test(ready_sender);
            result_sender
                .send(
                    super::recover_abandoned_staging(&recovery_layout)
                        .map_err(|error| error.kind()),
                )
                .expect("send recovery result");
        });
        ready_receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("recovery reached global lock wait");

        let moved_root = parent.path().join("moved-home");
        std::fs::rename(layout.root(), &moved_root).expect("move retained root");
        ensure_home_layout(&layout).expect("create replacement root");
        std::fs::File::unlock(&lock).expect("release global lock");

        let error = result_receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("recovery completes after unlock")
            .expect_err("root replacement must fail recovery");
        assert_eq!(error, ConfigErrorKind::AuthorityValidation);
        assert!(
            moved_root
                .join("plugins")
                .join(".staging")
                .join(id)
                .exists()
        );
        recovery.join().expect("recovery thread");
    }

    #[cfg(unix)]
    #[test]
    fn renamed_candidate_after_preflight_is_preserved_without_deletion() {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        ensure_home_layout(&layout).expect("bootstrap");
        let writer = super::begin_staging(&layout).expect("begin");
        let canonical = layout.transaction_staging_dir(writer.id());
        drop(writer);
        let raced_name = super::platform::rename_next_recovery_candidate_for_test();
        let raced = layout.host_staging_dir().join(raced_name);

        assert_eq!(
            super::recover_abandoned_staging(&layout).expect("recovery"),
            0
        );
        assert!(!canonical.exists());
        assert!(raced.join("transaction.lock").exists());
        assert!(raced.join("journal.json").exists());
        assert!(raced.join("payload").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn unix_payload_name_set_race_preserves_before_deletion() {
        for mutation in ["file", "directory"] {
            let parent = tempfile::tempdir().expect("parent");
            let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
            ensure_home_layout(&layout).expect("bootstrap");
            let mut writer = super::begin_staging(&layout).expect("begin");
            writer
                .write_file(&BundlePath::parse("dir/file").expect("path"), b"payload")
                .expect("write");
            let transaction = layout.transaction_staging_dir(writer.id());
            drop(writer);
            let raced_transaction = transaction.clone();
            let moved = parent.path().join(format!("moved-{mutation}"));
            super::platform::after_final_recovery_snapshot_for_test(move || {
                let source = if mutation == "file" {
                    raced_transaction.join("payload/dir/file")
                } else {
                    raced_transaction.join("payload/dir")
                };
                std::fs::rename(source, moved).expect("move expected payload entry");
            });

            assert_eq!(
                super::recover_abandoned_staging(&layout).expect("recovery"),
                0
            );
            assert!(transaction.exists(), "{mutation}");
            assert!(transaction.join("journal.json").is_file(), "{mutation}");
            assert!(transaction.join("transaction.lock").is_file(), "{mutation}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_unlink_failure_classifies_after_first_successful_deletion() {
        for (call, expected) in [(1, None), (2, Some(ConfigErrorKind::RecoveryIndeterminate))] {
            let parent = tempfile::tempdir().expect("parent");
            let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
            ensure_home_layout(&layout).expect("bootstrap");
            let writer = super::begin_staging(&layout).expect("begin");
            let transaction = layout.transaction_staging_dir(writer.id());
            drop(writer);
            super::platform::fail_recovery_unlink_for_test(call);

            let result = super::recover_abandoned_staging(&layout);
            match expected {
                None => assert_eq!(result.expect("preserved candidate"), 0),
                Some(kind) => assert_eq!(result.expect_err("indeterminate").kind(), kind),
            }
            assert!(transaction.exists());
            if call == 1 {
                assert!(transaction.join("payload").is_dir());
                assert!(transaction.join("journal.json").is_file());
                assert!(transaction.join("transaction.lock").is_file());
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_third_snapshot_preserves_claimed_or_extra_candidate() {
        for mutation in ["claimed", "extra"] {
            let parent = tempfile::tempdir().expect("parent");
            let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
            ensure_home_layout(&layout).expect("bootstrap");
            let writer = super::begin_staging(&layout).expect("begin");
            let transaction = layout.transaction_staging_dir(writer.id());
            let id = writer.id().as_str().to_owned();
            drop(writer);
            let raced_transaction = transaction.clone();
            super::platform::after_recovery_preflight_for_test(move || {
                match mutation {
                "claimed" => std::fs::write(
                    raced_transaction.join("journal.json"),
                    format!(
                        "{{\"formatVersion\":1,\"kind\":\"mcode-staging-transaction\",\"transactionId\":\"{id}\",\"state\":\"committing\"}}\n"
                    ),
                )
                .expect("claim journal"),
                "extra" => std::fs::write(raced_transaction.join("extra"), b"x")
                    .expect("add extra entry"),
                _ => unreachable!(),
            }
            });

            assert_eq!(
                super::recover_abandoned_staging(&layout).expect("recovery"),
                0
            );
            assert!(transaction.exists(), "{mutation}");
            assert!(transaction.join("payload").is_dir(), "{mutation}");
            assert!(transaction.join("transaction.lock").is_file(), "{mutation}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_payload_name_set_race_preserves_before_deletion() {
        for mutation in ["missing", "extra"] {
            let parent = tempfile::tempdir().expect("parent");
            let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
            ensure_home_layout(&layout).expect("bootstrap");
            let mut writer = super::begin_staging(&layout).expect("begin");
            writer
                .write_file(&BundlePath::parse("file").expect("path"), b"payload")
                .expect("write");
            let transaction = layout.transaction_staging_dir(writer.id());
            drop(writer);
            let raced_transaction = transaction.clone();
            let moved = parent.path().join("moved-payload");
            super::platform::after_final_recovery_snapshot_for_test(move || match mutation {
                "missing" => std::fs::rename(raced_transaction.join("payload/file"), moved)
                    .expect("move expected payload"),
                "extra" => std::fs::write(raced_transaction.join("payload/extra"), b"x")
                    .expect("add payload entry"),
                _ => unreachable!(),
            });

            assert_eq!(
                super::recover_abandoned_staging(&layout).expect("recovery"),
                0
            );
            assert!(transaction.exists(), "{mutation}");
            assert!(transaction.join("journal.json").is_file(), "{mutation}");
            assert!(transaction.join("transaction.lock").is_file(), "{mutation}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_disposition_failure_classifies_by_first_successful_delete() {
        for (call, expected) in [(1, None), (2, Some(ConfigErrorKind::RecoveryIndeterminate))] {
            let parent = tempfile::tempdir().expect("parent");
            let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
            ensure_home_layout(&layout).expect("bootstrap");
            let writer = super::begin_staging(&layout).expect("begin");
            let transaction = layout.transaction_staging_dir(writer.id());
            drop(writer);
            super::platform::fail_recovery_disposition_for_test(call);

            let result = super::recover_abandoned_staging(&layout);
            match expected {
                None => assert_eq!(result.expect("preserved candidate"), 0),
                Some(kind) => assert_eq!(result.expect_err("indeterminate").kind(), kind),
            }
            assert!(transaction.exists());
            if call == 1 {
                assert!(transaction.join("payload").is_dir());
                assert!(transaction.join("journal.json").is_file());
                assert!(transaction.join("transaction.lock").is_file());
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn successful_recovery_unlock_failures_remain_lock_errors() {
        for transaction_unlock in [true, false] {
            let parent = tempfile::tempdir().expect("parent");
            let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
            ensure_home_layout(&layout).expect("bootstrap");
            let writer = super::begin_staging(&layout).expect("begin");
            let transaction = layout.transaction_staging_dir(writer.id());
            drop(writer);
            super::platform::fail_recovery_unlock_for_test(transaction_unlock);

            let error = super::recover_abandoned_staging(&layout).expect_err("unlock failure");
            assert_eq!(error.kind(), ConfigErrorKind::Lock);
            assert!(!transaction.exists());
        }
    }

    #[test]
    fn native_failure_after_creation_poisons_writer() {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        ensure_home_layout(&layout).expect("bootstrap");
        let mut writer = super::begin_staging(&layout).expect("begin");
        super::platform::fail_next_payload_write_for_test();
        assert_eq!(
            writer
                .write_file(&path("file"), b"bytes")
                .expect_err("injected write failure")
                .kind(),
            ConfigErrorKind::Io
        );
        assert_eq!(
            writer
                .write_file(&path("retry"), b"bytes")
                .expect_err("poisoned retry")
                .kind(),
            ConfigErrorKind::AuthorityValidation
        );
        assert_eq!(
            writer.finish().err().expect("poisoned finish").kind(),
            ConfigErrorKind::AuthorityValidation
        );
    }

    #[test]
    fn payload_barrier_failure_is_propagated_and_poisons_writer() {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        ensure_home_layout(&layout).expect("bootstrap");
        let mut writer = super::begin_staging(&layout).expect("begin");
        super::platform::fail_next_payload_barrier_for_test();
        assert_eq!(
            writer
                .write_file(&path("file"), b"bytes")
                .expect_err("injected barrier failure")
                .kind(),
            ConfigErrorKind::Io
        );
        assert_eq!(
            writer
                .write_file(&path("retry"), b"bytes")
                .expect_err("poisoned retry")
                .kind(),
            ConfigErrorKind::AuthorityValidation
        );
    }

    #[test]
    fn failed_validation_does_not_change_ledger() {
        let ledger = PayloadLedger::default();
        assert_eq!(
            ledger
                .plan(
                    &path("a/b/c"),
                    1,
                    LedgerLimits {
                        directories: 1,
                        ..SMALL
                    },
                )
                .expect_err("directory limit")
                .kind(),
            ConfigErrorKind::Oversized
        );
        assert!(ledger.files.is_empty());
        assert!(ledger.directories.is_empty());
        assert_eq!(ledger.total_bytes, 0);
    }
}
