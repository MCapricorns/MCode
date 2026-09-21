//! Windows rooted native abandoned staging-transaction recovery.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::mem::size_of;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::GENERIC_READ;
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO_EX, FILE_READ_ATTRIBUTES,
    FILE_WRITE_ATTRIBUTES, FileDispositionInfoEx, READ_CONTROL, SYNCHRONIZE,
    SetFileInformationByHandle,
};

use super::{
    GLOBAL_LOCK, Identity, JOURNAL, PAYLOAD, STAGING_ROOT, TRANSACTION_LOCK, directory_names,
    identity, open_private_directory, open_private_regular, regular_size, reject_wrong_case,
    verify_directory, verify_named_directory, verify_named_file, verify_named_root_directory,
    verify_regular,
};
use crate::secure_fs::windows::{windows_acl, windows_open};
use crate::staging::{
    JournalState, MAX_STAGING_DIRECTORIES, MAX_STAGING_ENTRIES, MAX_STAGING_FILE_BYTES,
    MAX_STAGING_FILES, MAX_STAGING_JOURNAL_BYTES, MAX_STAGING_ROOT_ENTRIES,
    MAX_STAGING_TOTAL_BYTES, parse_journal,
};
use crate::{BundlePath, ConfigError, ConfigErrorKind, HomeLayout, TransactionId};

const RECOVERY_LOCK_ACCESS: u32 = GENERIC_READ | READ_CONTROL | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
const DELETE_FILE_ACCESS: u32 = GENERIC_READ
    | READ_CONTROL
    | FILE_READ_ATTRIBUTES
    | FILE_WRITE_ATTRIBUTES
    | DELETE
    | SYNCHRONIZE;
const DELETE_DIRECTORY_ACCESS: u32 =
    windows_open::OWNED_DIRECTORY_ACCESS | FILE_WRITE_ATTRIBUTES | DELETE;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    transaction: Identity,
    transaction_lock: Identity,
    journal: Identity,
    payload: Identity,
    state: JournalState,
    journal_bytes: Vec<u8>,
    entries: BTreeMap<String, Entry>,
}

struct Anchors<'a> {
    root_parent: &'a File,
    root_name: &'a OsStr,
    root: &'a File,
    root_identity: Identity,
    plugins: &'a File,
    plugins_identity: Identity,
    staging: &'a File,
    staging_identity: Identity,
    global_lock: &'a File,
    global_lock_identity: Identity,
    volume: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    Directory(Identity),
    File { identity: Identity, size: u64 },
}

pub(crate) fn recover_abandoned(home: &HomeLayout) -> Result<usize, ConfigError> {
    let opened_root = windows_open::open_existing_owned_root(home.root())?;
    let root_parent = opened_root.parent;
    let root = opened_root.root;
    windows_acl::verify_fixed_descriptor(&root)?;
    let root_identity = checked_identity(&root)?;
    let root_name = home
        .root()
        .file_name()
        .ok_or_else(|| ConfigError::new(ConfigErrorKind::InvalidHome))?
        .to_os_string();
    let plugins = open_private_directory(&root, OsStr::new("plugins"), root_identity.volume)?;
    let plugins_identity = checked_identity(&plugins)?;
    let volume = plugins_identity.volume;

    reject_wrong_case(&plugins, Some(STAGING_ROOT))?;
    if windows_open::child_attributes(&plugins, OsStr::new(STAGING_ROOT))?.is_none() {
        return Ok(0);
    }
    let staging = open_private_directory(&plugins, OsStr::new(STAGING_ROOT), volume)?;
    let staging_identity = checked_identity(&staging)?;
    let global_lock = open_existing_lock(&plugins, OsStr::new(GLOBAL_LOCK), volume)?;
    let global_lock_identity = checked_identity(&global_lock)?;
    File::lock(&global_lock).map_err(lock_error)?;
    let anchors = Anchors {
        root_parent: &root_parent,
        root_name: &root_name,
        root: &root,
        root_identity,
        plugins: &plugins,
        plugins_identity,
        staging: &staging,
        staging_identity,
        global_lock: &global_lock,
        global_lock_identity,
        volume,
    };

    let result = (|| {
        revalidate_anchors(&anchors)?;
        let scan = open_private_directory(&plugins, OsStr::new(STAGING_ROOT), volume)?;
        if checked_identity(&scan)? != staging_identity {
            return Err(validation());
        }
        let mut names = directory_names(&scan, MAX_STAGING_ROOT_ENTRIES + 1)?;
        check_root_count(names.len())?;
        names.sort();
        let mut deleted = 0usize;
        for name in names {
            let Some(text) = name.to_str() else {
                continue;
            };
            let Some(id) = TransactionId::parse_persistent(text) else {
                continue;
            };
            if recover_candidate(&anchors, &name, &id)? {
                deleted = deleted
                    .checked_add(1)
                    .ok_or_else(|| ConfigError::new(ConfigErrorKind::Oversized))?;
            }
        }
        Ok(deleted)
    })();
    let unlock = unlock_after_recovery(&global_lock, false);
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

fn recover_candidate(
    anchors: &Anchors<'_>,
    name: &OsStr,
    id: &TransactionId,
) -> Result<bool, ConfigError> {
    let staging = anchors.staging;
    let volume = anchors.volume;
    let transaction = match open_private_directory(staging, name, volume) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    let lock = match open_existing_lock(&transaction, OsStr::new(TRANSACTION_LOCK), volume) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    if File::try_lock(&lock).is_err() {
        return Ok(false);
    }
    let pair = open_private_directory(staging, name, volume)
        .and_then(|first_handle| snapshot(&first_handle, &lock, volume, id))
        .and_then(|first| {
            open_private_directory(staging, name, volume)
                .and_then(|second_handle| snapshot(&second_handle, &lock, volume, id))
                .map(|second| (first, second))
        });
    let (first, second) = match pair {
        Ok(value) if value.0 == value.1 => value,
        _ => {
            let _ = File::unlock(&lock);
            return Ok(false);
        }
    };
    if !matches!(second.state, JournalState::Writing | JournalState::Staged) {
        let _ = File::unlock(&lock);
        return Ok(false);
    }
    #[cfg(test)]
    run_after_recovery_preflight_for_test();

    let transaction = match open_private_directory(staging, name, volume) {
        Ok(value) => value,
        Err(_) => {
            let _ = File::unlock(&lock);
            return Ok(false);
        }
    };
    if let Err(error) = delete_transaction(anchors, name, id, &transaction, &lock, &first) {
        if error.kind() == ConfigErrorKind::RecoveryIndeterminate {
            return Err(error);
        }
        let _ = File::unlock(&lock);
        return Ok(false);
    }
    unlock_after_recovery(&lock, true)?;
    Ok(true)
}

fn snapshot(
    transaction: &File,
    lock: &File,
    volume: u64,
    id: &TransactionId,
) -> Result<Snapshot, ConfigError> {
    verify_directory(transaction, volume)?;
    let names = string_set(directory_names(transaction, 4)?)?;
    if names
        != BTreeSet::from([
            TRANSACTION_LOCK.to_owned(),
            JOURNAL.to_owned(),
            PAYLOAD.to_owned(),
        ])
    {
        return Err(validation());
    }
    verify_regular(lock, volume, Some(0))?;
    verify_named_file(transaction, OsStr::new(TRANSACTION_LOCK), lock, volume, 0)?;

    let mut journal = open_private_regular(transaction, OsStr::new(JOURNAL), volume)?;
    let journal_size = regular_size(&journal, volume)?;
    if journal_size > MAX_STAGING_JOURNAL_BYTES as u64 {
        return Err(oversized());
    }
    let journal_identity = checked_identity(&journal)?;
    let mut bytes = Vec::with_capacity(journal_size as usize);
    journal
        .by_ref()
        .take((MAX_STAGING_JOURNAL_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    let state = parse_journal(&bytes, id).ok_or_else(validation)?;

    let payload = open_private_directory(transaction, OsStr::new(PAYLOAD), volume)?;
    let mut entries = BTreeMap::new();
    let mut counts = Counts::default();
    inspect_payload(&payload, "", volume, &mut entries, &mut counts)?;
    if state == JournalState::Staged && counts.files == 0 {
        return Err(validation());
    }
    Ok(Snapshot {
        transaction: checked_identity(transaction)?,
        transaction_lock: checked_identity(lock)?,
        journal: journal_identity,
        payload: checked_identity(&payload)?,
        state,
        journal_bytes: bytes,
        entries,
    })
}

#[derive(Default)]
struct Counts {
    files: usize,
    directories: usize,
    total_bytes: u64,
}

fn inspect_payload(
    directory: &File,
    prefix: &str,
    volume: u64,
    entries: &mut BTreeMap<String, Entry>,
    counts: &mut Counts,
) -> Result<(), ConfigError> {
    let mut names = directory_names(directory, MAX_STAGING_ENTRIES + 1)?;
    names.sort();
    for name in names {
        let text = name.to_str().ok_or_else(validation)?;
        let path = if prefix.is_empty() {
            text.to_owned()
        } else {
            format!("{prefix}/{text}")
        };
        BundlePath::parse(&path)?;
        let attributes =
            windows_open::child_attributes(directory, &name)?.ok_or_else(validation)?;
        if attributes & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY != 0 {
            let child = open_private_directory(directory, &name, volume)?;
            let child_identity = checked_identity(&child)?;
            counts.directories = counts.directories.checked_add(1).ok_or_else(oversized)?;
            check_counts(counts)?;
            if entries
                .insert(path.clone(), Entry::Directory(child_identity))
                .is_some()
            {
                return Err(validation());
            }
            inspect_payload(&child, &path, volume, entries, counts)?;
        } else {
            let file = open_private_regular(directory, &name, volume)?;
            let size = regular_size(&file, volume)?;
            check_file_size(size)?;
            counts.files = counts.files.checked_add(1).ok_or_else(oversized)?;
            counts.total_bytes = counts.total_bytes.checked_add(size).ok_or_else(oversized)?;
            check_counts(counts)?;
            if entries
                .insert(
                    path,
                    Entry::File {
                        identity: checked_identity(&file)?,
                        size,
                    },
                )
                .is_some()
            {
                return Err(validation());
            }
        }
    }
    Ok(())
}

fn check_root_count(count: usize) -> Result<(), ConfigError> {
    if count > MAX_STAGING_ROOT_ENTRIES {
        return Err(oversized());
    }
    Ok(())
}

fn check_file_size(size: u64) -> Result<(), ConfigError> {
    if size > MAX_STAGING_FILE_BYTES {
        return Err(oversized());
    }
    Ok(())
}

fn check_counts(counts: &Counts) -> Result<(), ConfigError> {
    let entries = counts
        .files
        .checked_add(counts.directories)
        .ok_or_else(oversized)?;
    if counts.files > MAX_STAGING_FILES
        || counts.directories > MAX_STAGING_DIRECTORIES
        || entries > MAX_STAGING_ENTRIES
        || counts.total_bytes > MAX_STAGING_TOTAL_BYTES
    {
        return Err(oversized());
    }
    Ok(())
}

fn revalidate_anchors(anchors: &Anchors<'_>) -> Result<(), ConfigError> {
    windows_open::verify_exact_root_spelling(anchors.root_parent, anchors.root_name)?;
    verify_named_root_directory(
        anchors.root_parent,
        anchors.root_name,
        anchors.root,
        anchors.volume,
    )?;
    verify_directory(anchors.root, anchors.volume)?;
    verify_directory(anchors.plugins, anchors.volume)?;
    verify_directory(anchors.staging, anchors.volume)?;
    if checked_identity(anchors.root)? != anchors.root_identity
        || checked_identity(anchors.plugins)? != anchors.plugins_identity
        || checked_identity(anchors.staging)? != anchors.staging_identity
        || checked_identity(anchors.global_lock)? != anchors.global_lock_identity
    {
        return Err(validation());
    }
    verify_named_directory(
        anchors.root,
        OsStr::new("plugins"),
        anchors.plugins,
        anchors.volume,
    )?;
    verify_named_directory(
        anchors.plugins,
        OsStr::new(STAGING_ROOT),
        anchors.staging,
        anchors.volume,
    )?;
    verify_named_file(
        anchors.plugins,
        OsStr::new(GLOBAL_LOCK),
        anchors.global_lock,
        anchors.volume,
        0,
    )
}

fn delete_transaction(
    anchors: &Anchors<'_>,
    name: &OsStr,
    id: &TransactionId,
    transaction: &File,
    lock: &File,
    expected: &Snapshot,
) -> Result<(), ConfigError> {
    let mut mutation_started = false;
    revalidate_anchors(anchors)?;
    verify_named_directory(anchors.staging, name, transaction, anchors.volume)?;
    let current = snapshot(transaction, lock, anchors.volume, id)?;
    if &current != expected {
        return Err(validation());
    }
    #[cfg(test)]
    run_after_final_snapshot_for_test();

    let payload = open_private_directory(transaction, OsStr::new(PAYLOAD), anchors.volume)?;
    if checked_identity(&payload)? != expected.payload {
        return Err(validation());
    }
    delete_payload_directory(
        &payload,
        "",
        anchors.volume,
        &expected.entries,
        &mut mutation_started,
    )?;
    let payload_delete = open_delete_directory(transaction, OsStr::new(PAYLOAD), anchors.volume)
        .map_err(|error| mutation_error(error, mutation_started))?;
    if checked_identity(&payload_delete).map_err(|error| mutation_error(error, mutation_started))?
        != expected.payload
    {
        return Err(mutation_error(validation(), mutation_started));
    }
    verify_named_directory(
        transaction,
        OsStr::new(PAYLOAD),
        &payload_delete,
        anchors.volume,
    )
    .map_err(|error| mutation_error(error, mutation_started))?;
    mark_delete(&payload_delete, &mut mutation_started)?;
    drop(payload_delete);
    prove_absent(transaction, OsStr::new(PAYLOAD), mutation_started)?;

    let mut journal = open_delete_file(transaction, OsStr::new(JOURNAL), anchors.volume)
        .map_err(|error| mutation_error(error, mutation_started))?;
    verify_journal(&mut journal, expected, mutation_started)?;
    verify_named_file(
        transaction,
        OsStr::new(JOURNAL),
        &journal,
        anchors.volume,
        u64::try_from(expected.journal_bytes.len()).expect("journal length fits u64"),
    )
    .map_err(|error| mutation_error(error, mutation_started))?;
    mark_delete(&journal, &mut mutation_started)?;
    drop(journal);
    prove_absent(transaction, OsStr::new(JOURNAL), mutation_started)?;

    let lock_delete = open_delete_file(transaction, OsStr::new(TRANSACTION_LOCK), anchors.volume)
        .map_err(|error| mutation_error(error, mutation_started))?;
    if checked_identity(&lock_delete).map_err(indeterminate)? != expected.transaction_lock {
        return Err(indeterminate(validation()));
    }
    verify_named_file(
        transaction,
        OsStr::new(TRANSACTION_LOCK),
        &lock_delete,
        anchors.volume,
        0,
    )
    .map_err(indeterminate)?;
    mark_delete(&lock_delete, &mut mutation_started)?;
    drop(lock_delete);
    prove_absent(transaction, OsStr::new(TRANSACTION_LOCK), mutation_started)?;
    super::super::flush_directory(transaction).map_err(indeterminate)?;

    let transaction_delete =
        open_delete_directory(anchors.staging, name, anchors.volume).map_err(indeterminate)?;
    if checked_identity(&transaction_delete).map_err(indeterminate)? != expected.transaction {
        return Err(indeterminate(validation()));
    }
    verify_named_directory(anchors.staging, name, &transaction_delete, anchors.volume)
        .map_err(indeterminate)?;
    mark_delete(&transaction_delete, &mut mutation_started)?;
    drop(transaction_delete);
    prove_absent(anchors.staging, name, mutation_started)?;
    #[cfg(test)]
    if FAIL_NEXT_STAGING_BARRIER.with(|fail| fail.replace(false)) {
        return Err(indeterminate(
            ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::Other),
        ));
    }
    super::super::flush_directory(anchors.staging).map_err(indeterminate)?;
    prove_absent(anchors.staging, name, mutation_started)?;
    revalidate_anchors(anchors).map_err(indeterminate)
}

fn delete_payload_directory(
    directory: &File,
    prefix: &str,
    volume: u64,
    entries: &BTreeMap<String, Entry>,
    mutation_started: &mut bool,
) -> Result<(), ConfigError> {
    let mut names = directory_names(directory, MAX_STAGING_ENTRIES + 1)
        .map_err(|error| mutation_error(error, *mutation_started))?;
    names.sort();
    let actual =
        string_set(names.clone()).map_err(|error| mutation_error(error, *mutation_started))?;
    let expected_names = immediate_child_names(entries, prefix);
    if actual != expected_names {
        return Err(mutation_error(validation(), *mutation_started));
    }
    for name in names {
        let text = name
            .to_str()
            .ok_or_else(|| mutation_error(validation(), *mutation_started))?;
        let path = if prefix.is_empty() {
            text.to_owned()
        } else {
            format!("{prefix}/{text}")
        };
        match entries.get(&path).copied() {
            Some(Entry::Directory(expected)) => {
                let child = open_private_directory(directory, &name, volume)
                    .map_err(|error| mutation_error(error, *mutation_started))?;
                if checked_identity(&child)
                    .map_err(|error| mutation_error(error, *mutation_started))?
                    != expected
                {
                    return Err(mutation_error(validation(), *mutation_started));
                }
                delete_payload_directory(&child, &path, volume, entries, mutation_started)?;
                let deletion = open_delete_directory(directory, &name, volume)
                    .map_err(|error| mutation_error(error, *mutation_started))?;
                if checked_identity(&deletion)
                    .map_err(|error| mutation_error(error, *mutation_started))?
                    != expected
                {
                    return Err(mutation_error(validation(), *mutation_started));
                }
                verify_named_directory(directory, &name, &deletion, volume)
                    .map_err(|error| mutation_error(error, *mutation_started))?;
                mark_delete(&deletion, mutation_started)?;
                drop(deletion);
                prove_absent(directory, &name, *mutation_started)?;
            }
            Some(Entry::File {
                identity: expected,
                size,
            }) => {
                let deletion = open_delete_file(directory, &name, volume)
                    .map_err(|error| mutation_error(error, *mutation_started))?;
                if checked_identity(&deletion)
                    .map_err(|error| mutation_error(error, *mutation_started))?
                    != expected
                    || regular_size(&deletion, volume)
                        .map_err(|error| mutation_error(error, *mutation_started))?
                        != size
                {
                    return Err(mutation_error(validation(), *mutation_started));
                }
                verify_named_file(directory, &name, &deletion, volume, size)
                    .map_err(|error| mutation_error(error, *mutation_started))?;
                mark_delete(&deletion, mutation_started)?;
                drop(deletion);
                prove_absent(directory, &name, *mutation_started)?;
            }
            None => return Err(mutation_error(validation(), *mutation_started)),
        }
    }
    super::super::flush_directory(directory)
        .map_err(|error| mutation_error(error, *mutation_started))
}

fn immediate_child_names(entries: &BTreeMap<String, Entry>, prefix: &str) -> BTreeSet<String> {
    let prefix = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    };
    entries
        .keys()
        .filter_map(|path| {
            let remainder = path.strip_prefix(&prefix)?;
            (!remainder.contains('/')).then(|| remainder.to_owned())
        })
        .collect()
}

fn verify_journal(
    journal: &mut File,
    expected: &Snapshot,
    mutation_started: bool,
) -> Result<(), ConfigError> {
    if checked_identity(journal).map_err(|error| mutation_error(error, mutation_started))?
        != expected.journal
    {
        return Err(mutation_error(validation(), mutation_started));
    }
    let mut bytes = Vec::new();
    journal
        .by_ref()
        .take((MAX_STAGING_JOURNAL_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| mutation_error(io_error(error), mutation_started))?;
    if bytes != expected.journal_bytes {
        return Err(mutation_error(validation(), mutation_started));
    }
    Ok(())
}

fn prove_absent(parent: &File, name: &OsStr, mutation_started: bool) -> Result<(), ConfigError> {
    match windows_open::exact_child_exists(parent, name) {
        Ok(false) => Ok(()),
        Ok(true) => Err(mutation_error(validation(), mutation_started)),
        Err(error) => Err(mutation_error(error, mutation_started)),
    }
}

fn open_existing_lock(parent: &File, name: &OsStr, volume: u64) -> Result<File, ConfigError> {
    let file = windows_open::open_relative_file_exact(parent, name, RECOVERY_LOCK_ACCESS).map_err(
        |error| {
            if error.io_kind() == Some(io::ErrorKind::NotFound) {
                ConfigError::new(ConfigErrorKind::Lock).with_io_kind(io::ErrorKind::NotFound)
            } else {
                error
            }
        },
    )?;
    verify_regular(&file, volume, Some(0))?;
    Ok(file)
}

fn open_delete_file(parent: &File, name: &OsStr, volume: u64) -> Result<File, ConfigError> {
    let file = windows_open::open_relative_file_exact(parent, name, DELETE_FILE_ACCESS)?;
    verify_regular(&file, volume, None)?;
    Ok(file)
}

fn open_delete_directory(parent: &File, name: &OsStr, volume: u64) -> Result<File, ConfigError> {
    let file = windows_open::open_relative_directory_exact(parent, name, DELETE_DIRECTORY_ACCESS)?;
    verify_directory(&file, volume)?;
    Ok(file)
}

fn mark_delete(file: &File, mutation_started: &mut bool) -> Result<(), ConfigError> {
    #[cfg(test)]
    if fail_disposition_for_test() {
        return Err(mutation_error(
            ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::Other),
            *mutation_started,
        ));
    }
    let disposition = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    // SAFETY: `file` is a live owned handle opened with DELETE and
    // FILE_WRITE_ATTRIBUTES access, and the fixed disposition structure remains
    // readable for the synchronous call.
    let deleted = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfoEx,
            std::ptr::addr_of!(disposition).cast(),
            u32::try_from(size_of::<FILE_DISPOSITION_INFO_EX>())
                .expect("FILE_DISPOSITION_INFO_EX fits u32"),
        )
    };
    if deleted == 0 {
        let error = io::Error::last_os_error();
        return Err(mutation_error(
            ConfigError::new(ConfigErrorKind::Io).with_io_kind(error.kind()),
            *mutation_started,
        ));
    }
    *mutation_started = true;
    Ok(())
}

fn checked_identity(file: &File) -> Result<Identity, ConfigError> {
    let value = identity(file)?;
    if value.volume == 0 || value.id == [0; 16] {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(value)
}

fn string_set(names: Vec<OsString>) -> Result<BTreeSet<String>, ConfigError> {
    names
        .into_iter()
        .map(|name| name.into_string().map_err(|_| validation()))
        .collect()
}

fn validation() -> ConfigError {
    ConfigError::new(ConfigErrorKind::AuthorityValidation)
}

fn oversized() -> ConfigError {
    ConfigError::new(ConfigErrorKind::Oversized)
}

fn io_error(error: io::Error) -> ConfigError {
    ConfigError::new(ConfigErrorKind::Io).with_io_kind(error.kind())
}

fn lock_error(error: io::Error) -> ConfigError {
    ConfigError::new(ConfigErrorKind::Lock).with_io_kind(error.kind())
}

#[cfg(test)]
type TestCallback = Box<dyn FnOnce()>;

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_STAGING_BARRIER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static AFTER_PREFLIGHT: std::cell::RefCell<Option<TestCallback>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_FINAL_SNAPSHOT: std::cell::RefCell<Option<TestCallback>> =
        const { std::cell::RefCell::new(None) };
    static FAIL_DISPOSITION: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
    static FAIL_TRANSACTION_UNLOCK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_GLOBAL_UNLOCK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn fail_next_recovery_staging_barrier_for_test() {
    FAIL_NEXT_STAGING_BARRIER.with(|fail| fail.set(true));
}

#[cfg(test)]
pub(crate) fn after_recovery_preflight_for_test(callback: impl FnOnce() + 'static) {
    AFTER_PREFLIGHT.with(|pending| {
        assert!(pending.borrow_mut().replace(Box::new(callback)).is_none());
    });
}

#[cfg(test)]
fn run_after_recovery_preflight_for_test() {
    AFTER_PREFLIGHT.with(|pending| {
        if let Some(callback) = pending.borrow_mut().take() {
            callback();
        }
    });
}

#[cfg(test)]
pub(crate) fn after_final_recovery_snapshot_for_test(callback: impl FnOnce() + 'static) {
    AFTER_FINAL_SNAPSHOT.with(|pending| {
        assert!(pending.borrow_mut().replace(Box::new(callback)).is_none());
    });
}

#[cfg(test)]
fn run_after_final_snapshot_for_test() {
    AFTER_FINAL_SNAPSHOT.with(|pending| {
        if let Some(callback) = pending.borrow_mut().take() {
            callback();
        }
    });
}

#[cfg(test)]
pub(crate) fn fail_recovery_disposition_for_test(call: usize) {
    assert!(call > 0);
    FAIL_DISPOSITION.with(|state| state.set((call, 0)));
}

#[cfg(test)]
fn fail_disposition_for_test() -> bool {
    FAIL_DISPOSITION.with(|state| {
        let (target, calls) = state.get();
        let calls = calls + 1;
        state.set((target, calls));
        target == calls
    })
}

#[cfg(test)]
pub(crate) fn fail_recovery_unlock_for_test(transaction: bool) {
    if transaction {
        FAIL_TRANSACTION_UNLOCK.with(|fail| fail.set(true));
    } else {
        FAIL_GLOBAL_UNLOCK.with(|fail| fail.set(true));
    }
}

fn unlock_after_recovery(file: &File, _transaction: bool) -> Result<(), ConfigError> {
    #[cfg(test)]
    {
        let fail = if _transaction {
            FAIL_TRANSACTION_UNLOCK.with(|fail| fail.replace(false))
        } else {
            FAIL_GLOBAL_UNLOCK.with(|fail| fail.replace(false))
        };
        if fail {
            File::unlock(file).map_err(lock_error)?;
            return Err(ConfigError::new(ConfigErrorKind::Lock).with_io_kind(io::ErrorKind::Other));
        }
    }
    File::unlock(file).map_err(lock_error)
}

fn mutation_error(error: ConfigError, mutation_started: bool) -> ConfigError {
    if mutation_started {
        indeterminate(error)
    } else {
        error
    }
}

fn indeterminate(error: ConfigError) -> ConfigError {
    let mut mapped = ConfigError::new(ConfigErrorKind::RecoveryIndeterminate);
    if let Some(kind) = error.io_kind() {
        mapped = mapped.with_io_kind(kind);
    }
    mapped
}

#[cfg(test)]
mod tests {
    use super::{Counts, check_counts, check_file_size, check_root_count, immediate_child_names};
    use crate::staging::{
        MAX_STAGING_DIRECTORIES, MAX_STAGING_ENTRIES, MAX_STAGING_FILE_BYTES, MAX_STAGING_FILES,
        MAX_STAGING_TOTAL_BYTES,
    };
    use crate::{
        AccessControlEvidence, ConfigErrorKind, HomeLayout, begin_staging, ensure_home_layout,
        probe_access_control,
    };

    #[test]
    fn recovery_bounds_accept_exact_and_reject_one_over() {
        for (exact, over) in [
            (
                Counts {
                    files: MAX_STAGING_FILES,
                    directories: 0,
                    total_bytes: 0,
                },
                Counts {
                    files: MAX_STAGING_FILES + 1,
                    directories: 0,
                    total_bytes: 0,
                },
            ),
            (
                Counts {
                    files: 0,
                    directories: MAX_STAGING_DIRECTORIES,
                    total_bytes: 0,
                },
                Counts {
                    files: 0,
                    directories: MAX_STAGING_DIRECTORIES + 1,
                    total_bytes: 0,
                },
            ),
            (
                Counts {
                    files: MAX_STAGING_ENTRIES / 2,
                    directories: MAX_STAGING_ENTRIES / 2,
                    total_bytes: MAX_STAGING_TOTAL_BYTES,
                },
                Counts {
                    files: 0,
                    directories: 0,
                    total_bytes: MAX_STAGING_TOTAL_BYTES + 1,
                },
            ),
        ] {
            check_counts(&exact).expect("exact bound");
            assert_eq!(
                check_counts(&over).expect_err("one over").kind(),
                ConfigErrorKind::Oversized
            );
        }
        check_root_count(super::MAX_STAGING_ROOT_ENTRIES).expect("exact root count");
        assert_eq!(
            check_root_count(super::MAX_STAGING_ROOT_ENTRIES + 1)
                .expect_err("one-over root count")
                .kind(),
            ConfigErrorKind::Oversized
        );
        check_file_size(MAX_STAGING_FILE_BYTES).expect("exact file size");
        assert_eq!(
            check_file_size(MAX_STAGING_FILE_BYTES + 1)
                .expect_err("one-over file size")
                .kind(),
            ConfigErrorKind::Oversized
        );
    }

    #[test]
    fn permissive_payload_dacl_is_preserved_without_repair() {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        ensure_home_layout(&layout).expect("bootstrap");
        let writer = begin_staging(&layout).expect("begin");
        let transaction = layout.transaction_staging_dir(writer.id());
        drop(writer);
        let payload = transaction.join("payload");
        crate::secure_fs::windows::windows_file::make_permissive_for_test(&payload);

        assert_eq!(super::recover_abandoned(&layout).expect("recovery"), 0);
        assert!(transaction.exists());
        assert!(matches!(
            probe_access_control(&payload),
            AccessControlEvidence::WindowsProtectedDacl {
                extra_aces: 1..,
                ..
            }
        ));
    }

    #[test]
    fn immediate_children_require_exact_names() {
        let entries = [
            (
                "a".to_owned(),
                super::Entry::Directory(super::Identity {
                    volume: 1,
                    id: [1; 16],
                }),
            ),
            (
                "a/b".to_owned(),
                super::Entry::File {
                    identity: super::Identity {
                        volume: 1,
                        id: [2; 16],
                    },
                    size: 1,
                },
            ),
        ]
        .into_iter()
        .collect();
        assert_eq!(immediate_child_names(&entries, ""), ["a".to_owned()].into());
        assert_eq!(
            immediate_child_names(&entries, "a"),
            ["b".to_owned()].into()
        );
    }
}
