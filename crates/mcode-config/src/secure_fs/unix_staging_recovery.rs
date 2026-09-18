//! Unix native abandoned staging-transaction recovery.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::AsFd;

use rustix::fs::{self as rfs, AtFlags, OFlags};
use rustix::io::Errno;

use super::{
    GLOBAL_LOCK, Identity, JOURNAL, PAYLOAD, STAGING_ROOT, TRANSACTION_LOCK, directory_identity,
    directory_names, open_owned_root, open_private_directory, open_private_regular, regular_size,
    stat_device, verify_directory, verify_named_directory, verify_named_file,
    verify_named_root_directory, verify_regular,
};
use crate::staging::{
    JournalState, MAX_STAGING_DIRECTORIES, MAX_STAGING_ENTRIES, MAX_STAGING_FILE_BYTES,
    MAX_STAGING_FILES, MAX_STAGING_JOURNAL_BYTES, MAX_STAGING_ROOT_ENTRIES,
    MAX_STAGING_TOTAL_BYTES, parse_journal,
};
use crate::{BundlePath, ConfigError, ConfigErrorKind, HomeLayout, TransactionId};

#[cfg(any(target_os = "linux", target_os = "android"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MountIdentity(u64);

#[cfg(target_vendor = "apple")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MountIdentity([std::ffi::c_char; 1024]);

#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MountIdentity;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    transaction: Identity,
    transaction_lock: Identity,
    journal: Identity,
    payload: Identity,
    state: JournalState,
    journal_bytes: Vec<u8>,
    entries: BTreeMap<String, Entry>,
    total_bytes: u64,
}

#[derive(Clone, Copy)]
struct RecoveryBoundary<'a> {
    device: u64,
    mount: &'a MountIdentity,
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
    mount: MountIdentity,
    device: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    Directory(Identity),
    File { identity: Identity, size: u64 },
}

pub(crate) fn recover_abandoned(home: &HomeLayout) -> Result<usize, ConfigError> {
    let (root_parent, root_name, root) = open_owned_root(home)?;
    let root_identity = checked_identity(&root)?;
    let plugins = open_private_directory(&root, OsStr::new("plugins"), root_identity.device)?;
    let plugins_identity = checked_identity(&plugins)?;
    let device = plugins_identity.device;

    if !entry_exists(&plugins, OsStr::new(STAGING_ROOT))? {
        return Ok(0);
    }
    let mount = mount_identity(&plugins)?;
    let staging = open_private_directory(&plugins, OsStr::new(STAGING_ROOT), device)?;
    verify_mount(&staging, &mount)?;
    let staging_identity = checked_identity(&staging)?;
    let global_lock = open_existing_lock(&plugins, OsStr::new(GLOBAL_LOCK), device)?;
    verify_mount(&global_lock, &mount)?;
    let global_lock_identity = checked_identity(&global_lock)?;
    #[cfg(test)]
    signal_global_lock_wait_for_test();
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
        mount,
        device,
    };

    let result = (|| {
        revalidate_anchors(&anchors)?;

        let mut names = directory_names(&staging, MAX_STAGING_ROOT_ENTRIES + 1)?;
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
            let Some(counted) = recover_candidate(&anchors, &name, &id)? else {
                continue;
            };
            deleted = deleted
                .checked_add(counted)
                .ok_or_else(|| ConfigError::new(ConfigErrorKind::Oversized))?;
        }
        Ok(deleted)
    })();
    let unlock = File::unlock(&global_lock).map_err(lock_error);
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

fn recover_candidate(
    anchors: &Anchors<'_>,
    name: &OsStr,
    id: &TransactionId,
) -> Result<Option<usize>, ConfigError> {
    let staging = anchors.staging;
    let device = anchors.device;
    let transaction = match open_private_directory(staging, name, device) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let lock = match open_existing_lock(&transaction, OsStr::new(TRANSACTION_LOCK), device) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    match File::try_lock(&lock) {
        Ok(()) => {}
        Err(_) => return Ok(None),
    }

    let preflight = snapshot(&transaction, &lock, device, &anchors.mount, id).and_then(|first| {
        snapshot(&transaction, &lock, device, &anchors.mount, id).map(|second| (first, second))
    });
    let (first, second) = match preflight {
        Ok(pair) if pair.0 == pair.1 => pair,
        _ => {
            let _ = File::unlock(&lock);
            return Ok(None);
        }
    };
    if !matches!(second.state, JournalState::Writing | JournalState::Staged) {
        let _ = File::unlock(&lock);
        return Ok(None);
    }

    #[cfg(test)]
    if RENAME_NEXT_CANDIDATE.with(|rename| rename.replace(false)) {
        rfs::renameat(
            staging.as_fd(),
            name,
            staging.as_fd(),
            OsStr::new(RACED_TRANSACTION_NAME),
        )
        .map_err(|error| super::super::map_errno(error, ConfigErrorKind::Io))?;
    }

    let current_transaction = match open_private_directory(staging, name, device) {
        Ok(value) => value,
        Err(_) => {
            let _ = File::unlock(&lock);
            return Ok(None);
        }
    };
    match checked_identity_on_mount(&current_transaction, &anchors.mount) {
        Ok(identity) if identity == first.transaction => {}
        _ => {
            let _ = File::unlock(&lock);
            return Ok(None);
        }
    }

    match delete_transaction(anchors, name, id, &current_transaction, &lock, &first) {
        Ok(()) => {
            File::unlock(&lock).map_err(lock_error)?;
            Ok(Some(1))
        }
        Err(error) if error.kind() == ConfigErrorKind::RecoveryIndeterminate => Err(error),
        Err(_) => {
            let _ = File::unlock(&lock);
            Ok(None)
        }
    }
}

fn snapshot(
    transaction: &File,
    lock: &File,
    device: u64,
    mount: &MountIdentity,
    id: &TransactionId,
) -> Result<Snapshot, ConfigError> {
    verify_directory(transaction, device)?;
    let transaction_identity = checked_identity_on_mount(transaction, mount)?;
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

    verify_regular(lock, device, Some(0))?;
    verify_named_file(transaction, OsStr::new(TRANSACTION_LOCK), lock, device, 0)?;
    let transaction_lock = checked_identity_on_mount(lock, mount)?;

    let mut journal = open_private_regular(transaction, OsStr::new(JOURNAL), device)?;
    let journal_size = regular_size(&journal, device)?;
    if journal_size > MAX_STAGING_JOURNAL_BYTES as u64 {
        return Err(oversized());
    }
    let journal_identity = checked_identity_on_mount(&journal, mount)?;
    let mut bytes = Vec::with_capacity(journal_size as usize);
    journal
        .by_ref()
        .take((MAX_STAGING_JOURNAL_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    let state = parse_journal(&bytes, id).ok_or_else(validation)?;

    let payload = open_private_directory(transaction, OsStr::new(PAYLOAD), device)?;
    let payload_identity = checked_identity_on_mount(&payload, mount)?;
    let mut entries = BTreeMap::new();
    let mut counts = Counts::default();
    inspect_payload(&payload, "", device, mount, &mut entries, &mut counts)?;
    if state == JournalState::Staged && counts.files == 0 {
        return Err(validation());
    }

    Ok(Snapshot {
        transaction: transaction_identity,
        transaction_lock,
        journal: journal_identity,
        payload: payload_identity,
        state,
        journal_bytes: bytes,
        entries,
        total_bytes: counts.total_bytes,
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
    device: u64,
    mount: &MountIdentity,
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
        let stat = rfs::statat(directory.as_fd(), &name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|error| super::super::map_errno(error, ConfigErrorKind::Io))?;
        match rfs::FileType::from_raw_mode(stat.st_mode) {
            rfs::FileType::Directory => {
                let child = open_private_directory(directory, &name, device)?;
                let identity = checked_identity_on_mount(&child, mount)?;
                counts.directories = counts.directories.checked_add(1).ok_or_else(oversized)?;
                check_counts(counts)?;
                if entries
                    .insert(path.clone(), Entry::Directory(identity))
                    .is_some()
                {
                    return Err(validation());
                }
                inspect_payload(&child, &path, device, mount, entries, counts)?;
            }
            rfs::FileType::RegularFile => {
                let file = open_private_regular(directory, &name, device)?;
                let size = regular_size(&file, device)?;
                check_file_size(size)?;
                counts.files = counts.files.checked_add(1).ok_or_else(oversized)?;
                counts.total_bytes = counts.total_bytes.checked_add(size).ok_or_else(oversized)?;
                check_counts(counts)?;
                if entries
                    .insert(
                        path,
                        Entry::File {
                            identity: checked_identity_on_mount(&file, mount)?,
                            size,
                        },
                    )
                    .is_some()
                {
                    return Err(validation());
                }
            }
            _ => return Err(validation()),
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

fn delete_transaction(
    anchors: &Anchors<'_>,
    name: &OsStr,
    id: &TransactionId,
    transaction: &File,
    lock: &File,
    expected: &Snapshot,
) -> Result<(), ConfigError> {
    let mut mutation_started = false;
    let boundary = RecoveryBoundary {
        device: anchors.device,
        mount: &anchors.mount,
    };
    revalidate_anchors(anchors)?;
    verify_named_directory(anchors.staging, name, transaction, anchors.device)?;
    let current = snapshot(transaction, lock, anchors.device, &anchors.mount, id)?;
    if &current != expected {
        return Err(validation());
    }
    #[cfg(test)]
    run_after_final_snapshot_for_test();

    let payload = open_private_directory(transaction, OsStr::new(PAYLOAD), anchors.device)?;
    if checked_identity_on_mount(&payload, &anchors.mount)? != expected.payload {
        return Err(validation());
    }
    delete_payload_directory(
        &payload,
        "",
        anchors.device,
        &anchors.mount,
        &expected.entries,
        &mut mutation_started,
    )?;
    unlink_verified(
        transaction,
        OsStr::new(PAYLOAD),
        &payload,
        expected.payload,
        true,
        boundary,
        &mut mutation_started,
    )?;

    let journal = verify_journal(
        transaction,
        anchors.device,
        &anchors.mount,
        expected,
        mutation_started,
    )?;
    unlink_verified(
        transaction,
        OsStr::new(JOURNAL),
        &journal,
        expected.journal,
        false,
        boundary,
        &mut mutation_started,
    )?;
    unlink_verified(
        transaction,
        OsStr::new(TRANSACTION_LOCK),
        lock,
        expected.transaction_lock,
        false,
        boundary,
        &mut mutation_started,
    )?;
    super::super::sync_directory(transaction).map_err(indeterminate)?;

    unlink_verified(
        anchors.staging,
        name,
        transaction,
        expected.transaction,
        true,
        boundary,
        &mut mutation_started,
    )?;
    prove_absent(anchors.staging, name, mutation_started)?;
    #[cfg(test)]
    if FAIL_NEXT_STAGING_BARRIER.with(|fail| fail.replace(false)) {
        return Err(indeterminate(
            ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::Other),
        ));
    }
    super::super::sync_directory(anchors.staging).map_err(indeterminate)?;
    prove_absent(anchors.staging, name, mutation_started)?;
    revalidate_anchors(anchors).map_err(indeterminate)
}

fn delete_payload_directory(
    directory: &File,
    prefix: &str,
    device: u64,
    mount: &MountIdentity,
    entries: &BTreeMap<String, Entry>,
    mutation_started: &mut bool,
) -> Result<(), ConfigError> {
    let mut names = directory_names(directory, MAX_STAGING_ENTRIES + 1)
        .map_err(|error| mutation_error(error, *mutation_started))?;
    names.sort();
    let actual =
        string_set(names.clone()).map_err(|error| mutation_error(error, *mutation_started))?;
    if actual != immediate_child_names(entries, prefix) {
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
                let child = open_private_directory(directory, &name, device)
                    .map_err(|error| mutation_error(error, *mutation_started))?;
                if checked_identity_on_mount(&child, mount)
                    .map_err(|error| mutation_error(error, *mutation_started))?
                    != expected
                {
                    return Err(mutation_error(validation(), *mutation_started));
                }
                delete_payload_directory(&child, &path, device, mount, entries, mutation_started)?;
                unlink_verified(
                    directory,
                    &name,
                    &child,
                    expected,
                    true,
                    RecoveryBoundary { device, mount },
                    mutation_started,
                )?;
            }
            Some(Entry::File { identity, size }) => {
                let file = open_private_regular(directory, &name, device)
                    .map_err(|error| mutation_error(error, *mutation_started))?;
                if checked_identity_on_mount(&file, mount)
                    .map_err(|error| mutation_error(error, *mutation_started))?
                    != identity
                    || regular_size(&file, device)
                        .map_err(|error| mutation_error(error, *mutation_started))?
                        != size
                {
                    return Err(mutation_error(validation(), *mutation_started));
                }
                unlink_verified(
                    directory,
                    &name,
                    &file,
                    identity,
                    false,
                    RecoveryBoundary { device, mount },
                    mutation_started,
                )?;
            }
            None => return Err(mutation_error(validation(), *mutation_started)),
        }
    }
    super::super::sync_directory(directory)
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
    transaction: &File,
    device: u64,
    mount: &MountIdentity,
    expected: &Snapshot,
    mutation_started: bool,
) -> Result<File, ConfigError> {
    let mut journal = open_private_regular(transaction, OsStr::new(JOURNAL), device)
        .map_err(|error| mutation_error(error, mutation_started))?;
    if checked_identity_on_mount(&journal, mount)
        .map_err(|error| mutation_error(error, mutation_started))?
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
    Ok(journal)
}

fn unlink_verified(
    parent: &File,
    name: &OsStr,
    retained: &File,
    expected: Identity,
    directory: bool,
    boundary: RecoveryBoundary<'_>,
    mutation_started: &mut bool,
) -> Result<(), ConfigError> {
    let binding = if directory {
        verify_named_directory(parent, name, retained, boundary.device)
    } else {
        let size = regular_size(retained, boundary.device)
            .map_err(|error| mutation_error(error, *mutation_started))?;
        verify_named_file(parent, name, retained, boundary.device, size)
    };
    binding.map_err(|error| mutation_error(error, *mutation_started))?;
    if checked_identity_on_mount(retained, boundary.mount)
        .map_err(|error| mutation_error(error, *mutation_started))?
        != expected
    {
        return Err(mutation_error(validation(), *mutation_started));
    }
    let flags = if directory {
        AtFlags::REMOVEDIR
    } else {
        AtFlags::empty()
    };
    #[cfg(test)]
    if fail_unlink_for_test() {
        return Err(mutation_error(
            ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::Other),
            *mutation_started,
        ));
    }
    rfs::unlinkat(parent.as_fd(), name, flags).map_err(|error| {
        mutation_error(
            super::super::map_errno(error, ConfigErrorKind::Io),
            *mutation_started,
        )
    })?;
    *mutation_started = true;
    prove_unlinked(retained, expected, directory)
}

fn prove_unlinked(retained: &File, expected: Identity, directory: bool) -> Result<(), ConfigError> {
    let stat = rfs::fstat(retained.as_fd())
        .map_err(|error| indeterminate(super::super::map_errno(error, ConfigErrorKind::Io)))?;
    let expected_type = if directory {
        rfs::FileType::Directory
    } else {
        rfs::FileType::RegularFile
    };
    let current = Identity {
        device: stat_device(&stat).map_err(indeterminate)?,
        inode: stat.st_ino,
    };
    if current != expected
        || rfs::FileType::from_raw_mode(stat.st_mode) != expected_type
        || stat.st_nlink != 0
    {
        return Err(indeterminate(validation()));
    }
    Ok(())
}

fn prove_absent(parent: &File, name: &OsStr, mutation_started: bool) -> Result<(), ConfigError> {
    match entry_exists(parent, name) {
        Ok(false) => Ok(()),
        Ok(true) => Err(mutation_error(validation(), mutation_started)),
        Err(error) => Err(mutation_error(error, mutation_started)),
    }
}

fn revalidate_anchors(anchors: &Anchors<'_>) -> Result<(), ConfigError> {
    super::verify_exact_spelling(anchors.root_parent, anchors.root_name)?;
    verify_named_root_directory(
        anchors.root_parent,
        anchors.root_name,
        anchors.root,
        anchors.device,
    )?;
    verify_directory(anchors.root, anchors.device)?;
    verify_directory(anchors.plugins, anchors.device)?;
    verify_directory(anchors.staging, anchors.device)?;
    if checked_identity(anchors.root)? != anchors.root_identity
        || checked_identity_on_mount(anchors.plugins, &anchors.mount)? != anchors.plugins_identity
        || checked_identity_on_mount(anchors.staging, &anchors.mount)? != anchors.staging_identity
        || checked_identity_on_mount(anchors.global_lock, &anchors.mount)?
            != anchors.global_lock_identity
    {
        return Err(validation());
    }
    verify_named_directory(
        anchors.root,
        OsStr::new("plugins"),
        anchors.plugins,
        anchors.device,
    )?;
    verify_named_directory(
        anchors.plugins,
        OsStr::new(STAGING_ROOT),
        anchors.staging,
        anchors.device,
    )?;
    verify_named_file(
        anchors.plugins,
        OsStr::new(GLOBAL_LOCK),
        anchors.global_lock,
        anchors.device,
        0,
    )
}

fn open_existing_lock(parent: &File, name: &OsStr, device: u64) -> Result<File, ConfigError> {
    super::reject_wrong_case(parent, name.to_str())?;
    let flags = OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    let file = File::from(super::super::open_staging_component(parent, name, flags)?);
    verify_regular(&file, device, Some(0))?;
    Ok(file)
}

fn entry_exists(parent: &File, name: &OsStr) -> Result<bool, ConfigError> {
    super::reject_wrong_case(parent, name.to_str())?;
    match rfs::statat(parent.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(Errno::NOENT) => Ok(false),
        Err(error) => Err(super::super::map_errno(error, ConfigErrorKind::Io)),
    }
}

fn checked_identity(file: &File) -> Result<Identity, ConfigError> {
    let identity = directory_identity(file)?;
    if identity.device == 0 || identity.inode == 0 {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(identity)
}

fn checked_identity_on_mount(
    file: &File,
    expected_mount: &MountIdentity,
) -> Result<Identity, ConfigError> {
    verify_mount(file, expected_mount)?;
    checked_identity(file)
}

fn verify_mount(file: &File, expected: &MountIdentity) -> Result<(), ConfigError> {
    let actual = mount_identity(file)?;
    check_mount_identity(&actual, expected)
}

fn check_mount_identity(
    actual: &MountIdentity,
    expected: &MountIdentity,
) -> Result<(), ConfigError> {
    if actual != expected {
        return Err(validation());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn mount_identity(file: &File) -> Result<MountIdentity, ConfigError> {
    use rustix::fs::StatxFlags;

    let information = rfs::statx(
        file.as_fd(),
        "",
        AtFlags::EMPTY_PATH | AtFlags::NO_AUTOMOUNT,
        StatxFlags::MNT_ID,
    )
    .map_err(|error| super::super::map_errno(error, ConfigErrorKind::AccessControl))?;
    let returned = StatxFlags::from_bits_retain(information.stx_mask);
    if !returned.contains(StatxFlags::MNT_ID) || information.stx_mnt_id == 0 {
        return Err(mount_identity_unavailable());
    }
    Ok(MountIdentity(information.stx_mnt_id))
}

#[cfg(target_vendor = "apple")]
fn mount_identity(file: &File) -> Result<MountIdentity, ConfigError> {
    let information = rfs::fstatfs(file.as_fd())
        .map_err(|error| super::super::map_errno(error, ConfigErrorKind::AccessControl))?;
    if information.f_mntonname[0] == 0 {
        return Err(mount_identity_unavailable());
    }
    Ok(MountIdentity(information.f_mntonname))
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
fn mount_identity(_file: &File) -> Result<MountIdentity, ConfigError> {
    Err(mount_identity_unavailable())
}

fn mount_identity_unavailable() -> ConfigError {
    ConfigError::new(ConfigErrorKind::AccessControl).with_io_kind(io::ErrorKind::Unsupported)
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
const RACED_TRANSACTION_NAME: &str = "raced-transaction";

#[cfg(test)]
type TestCallback = Box<dyn FnOnce()>;

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_STAGING_BARRIER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static GLOBAL_LOCK_WAIT_READY: std::cell::RefCell<Option<std::sync::mpsc::Sender<()>>> =
        const { std::cell::RefCell::new(None) };
    static RENAME_NEXT_CANDIDATE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static AFTER_FINAL_SNAPSHOT: std::cell::RefCell<Option<TestCallback>> =
        const { std::cell::RefCell::new(None) };
    static FAIL_UNLINK: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
pub(crate) fn fail_next_recovery_staging_barrier_for_test() {
    FAIL_NEXT_STAGING_BARRIER.with(|fail| fail.set(true));
}

#[cfg(test)]
pub(crate) fn notify_on_recovery_global_lock_wait_for_test(ready: std::sync::mpsc::Sender<()>) {
    GLOBAL_LOCK_WAIT_READY.with(|pending| {
        assert!(pending.borrow_mut().replace(ready).is_none());
    });
}

#[cfg(test)]
fn signal_global_lock_wait_for_test() {
    GLOBAL_LOCK_WAIT_READY.with(|pending| {
        if let Some(ready) = pending.borrow_mut().take() {
            ready.send(()).expect("recovery lock-wait receiver");
        }
    });
}

#[cfg(test)]
pub(crate) fn rename_next_recovery_candidate_for_test() -> &'static str {
    RENAME_NEXT_CANDIDATE.with(|rename| rename.set(true));
    RACED_TRANSACTION_NAME
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
pub(crate) fn fail_recovery_unlink_for_test(call: usize) {
    assert!(call > 0);
    FAIL_UNLINK.with(|state| state.set((call, 0)));
}

#[cfg(test)]
fn fail_unlink_for_test() -> bool {
    FAIL_UNLINK.with(|state| {
        let (target, calls) = state.get();
        let calls = calls + 1;
        state.set((target, calls));
        target == calls
    })
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
    use super::{Counts, check_counts, check_file_size, check_root_count};
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    use super::{MountIdentity, check_mount_identity};
    use crate::ConfigErrorKind;
    use crate::staging::{
        MAX_STAGING_DIRECTORIES, MAX_STAGING_ENTRIES, MAX_STAGING_FILE_BYTES, MAX_STAGING_FILES,
        MAX_STAGING_TOTAL_BYTES,
    };

    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    #[test]
    fn mount_identity_mismatch_is_rejected() {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let identities = (MountIdentity(1), MountIdentity(2));
        #[cfg(target_vendor = "apple")]
        let identities = (MountIdentity([1; 1024]), MountIdentity([2; 1024]));

        assert_eq!(
            check_mount_identity(&identities.0, &identities.1)
                .expect_err("mount mismatch")
                .kind(),
            ConfigErrorKind::AuthorityValidation
        );
    }

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
                .expect_err("one over")
                .kind(),
            ConfigErrorKind::Oversized
        );
    }
}
