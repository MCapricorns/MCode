//! Unix handle-relative bounded staging writer.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;

use rustix::fs::{self as rfs, AtFlags, Mode, OFlags};
use rustix::io::Errno;

use super as unix;
use crate::staging::{
    MAX_STAGING_DIRECTORIES, MAX_STAGING_ENTRIES, MAX_STAGING_FILES, MAX_STAGING_JOURNAL_BYTES,
    MAX_STAGING_ROOT_ENTRIES, WriteFailure,
};
use crate::{BundlePath, ConfigError, ConfigErrorKind, HomeLayout, TransactionId};

#[path = "unix_staging_recovery.rs"]
mod recovery;

pub(crate) use recovery::recover_abandoned;
#[cfg(test)]
pub(crate) use recovery::{
    after_final_recovery_snapshot_for_test, fail_next_recovery_staging_barrier_for_test,
    fail_recovery_unlink_for_test, notify_on_recovery_global_lock_wait_for_test,
    rename_next_recovery_candidate_for_test,
};

const DIRECTORY_MODE: rfs::RawMode = 0o700;
const FILE_MODE: rfs::RawMode = 0o600;
const GLOBAL_LOCK: &str = ".staging.lock";
const STAGING_ROOT: &str = ".staging";
const TRANSACTION_LOCK: &str = "transaction.lock";
const PAYLOAD: &str = "payload";
const JOURNAL: &str = "journal.json";
const JOURNAL_TEMP: &str = ".journal.json.tmp";
const MAX_ID_ATTEMPTS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

pub(crate) struct Transaction {
    root_parent: File,
    root_name: OsString,
    root: File,
    plugins: File,
    staging: File,
    transaction: File,
    payload: File,
    root_identity: Identity,
    plugins_identity: Identity,
    staging_identity: Identity,
    transaction_identity: Identity,
    payload_identity: Identity,
    payload_directories: BTreeMap<String, Identity>,
    payload_files: BTreeMap<String, Identity>,
    plugins_device: u64,
    _lock: File,
    id: String,
}

impl Transaction {
    pub(crate) fn begin(
        home: &HomeLayout,
        journal: impl Fn(&TransactionId) -> Result<Vec<u8>, ConfigError>,
    ) -> Result<(TransactionId, Self), ConfigError> {
        let (root_parent, root_name, root) = open_owned_root(home)?;
        let root_identity = directory_identity(&root)?;
        let root_device = root_identity.device;
        let plugins = open_private_directory(&root, OsStr::new("plugins"), root_device)?;
        let plugins_identity = directory_identity(&plugins)?;
        let plugins_device = plugins_identity.device;

        let global_lock = create_or_open_lock(&plugins, OsStr::new(GLOBAL_LOCK), plugins_device)?;
        File::lock(&global_lock).map_err(lock_error)?;
        let result = (|| {
            let staging = create_or_open_private_directory(
                &plugins,
                OsStr::new(STAGING_ROOT),
                plugins_device,
            )?;
            enforce_root_capacity(&staging)?;
            let staging_identity = directory_identity(&staging)?;
            for _ in 0..MAX_ID_ATTEMPTS {
                let id = TransactionId::generate()?;
                let transaction = match create_private_directory_exclusive(
                    &staging,
                    OsStr::new(id.as_str()),
                    plugins_device,
                ) {
                    Ok(directory) => directory,
                    Err(error) if error.io_kind() == Some(io::ErrorKind::AlreadyExists) => continue,
                    Err(error) => return Err(error),
                };
                let transaction_identity = directory_identity(&transaction)?;
                let lock = create_private_file_exclusive(
                    &transaction,
                    OsStr::new(TRANSACTION_LOCK),
                    plugins_device,
                )?;
                sync_file(&lock)?;
                unix::sync_directory(&transaction)?;
                File::try_lock(&lock).map_err(try_lock_error)?;
                let payload = create_private_directory_exclusive(
                    &transaction,
                    OsStr::new(PAYLOAD),
                    plugins_device,
                )?;
                let payload_identity = directory_identity(&payload)?;
                let bytes = journal(&id)?;
                let id_spelling = id.as_str().to_owned();
                let native = Self {
                    root_parent,
                    root_name,
                    root,
                    plugins,
                    staging,
                    transaction,
                    payload,
                    root_identity,
                    plugins_identity,
                    staging_identity,
                    transaction_identity,
                    payload_identity,
                    payload_directories: BTreeMap::new(),
                    payload_files: BTreeMap::new(),
                    plugins_device,
                    _lock: lock,
                    id: id_spelling,
                };
                native.revalidate_roots()?;
                verify_named_file(
                    &native.plugins,
                    OsStr::new(GLOBAL_LOCK),
                    &global_lock,
                    plugins_device,
                    0,
                )?;
                publish_journal(&native.transaction, plugins_device, None, &bytes)?;
                native.revalidate_roots()?;
                verify_named_file(
                    &native.plugins,
                    OsStr::new(GLOBAL_LOCK),
                    &global_lock,
                    plugins_device,
                    0,
                )?;
                return Ok((id, native));
            }
            Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::AlreadyExists))
        })();
        let unlock = File::unlock(&global_lock).map_err(lock_error);
        match (result, unlock) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    pub(crate) fn write_file(
        &mut self,
        path: &str,
        new_directories: &[String],
        bytes: &[u8],
        expected_size: u64,
    ) -> Result<(), WriteFailure> {
        let mut mutation_started = false;
        for directory in new_directories {
            let (parent_path, name) = split_parent(directory);
            let parent = self
                .open_payload_directory(parent_path)
                .map_err(|error| failure(error, mutation_started))?;
            let child =
                create_payload_directory_exclusive(&parent, OsStr::new(name), self.plugins_device)
                    .map_err(|created| WriteFailure {
                        error: created.error,
                        mutation_started: mutation_started || created.mutation_started,
                    })?;
            mutation_started = true;
            verify_named_directory(&parent, OsStr::new(name), &child, self.plugins_device)
                .map_err(|error| failure(error, true))?;
            self.payload_directories.insert(
                directory.clone(),
                directory_identity(&child).map_err(|error| failure(error, true))?,
            );
        }

        let (parent_path, name) = split_parent(path);
        let parent = self
            .open_payload_directory(parent_path)
            .map_err(|error| failure(error, mutation_started))?;
        let mut file =
            create_payload_file_exclusive(&parent, OsStr::new(name), self.plugins_device).map_err(
                |created| WriteFailure {
                    error: created.error,
                    mutation_started: mutation_started || created.mutation_started,
                },
            )?;
        mutation_started = true;
        #[cfg(test)]
        if FAIL_NEXT_PAYLOAD_WRITE.with(|fail| fail.replace(false)) {
            return Err(failure(
                ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::Other),
                true,
            ));
        }
        file.write_all(bytes)
            .map_err(io_error)
            .and_then(|()| file.flush().map_err(io_error))
            .and_then(|()| sync_file(&file))
            .and_then(|()| verify_regular(&file, self.plugins_device, Some(expected_size)))
            .and_then(|()| {
                verify_named_file(
                    &parent,
                    OsStr::new(name),
                    &file,
                    self.plugins_device,
                    expected_size,
                )
            })
            .and_then(|()| {
                #[cfg(test)]
                if FAIL_NEXT_PAYLOAD_BARRIER.with(|fail| fail.replace(false)) {
                    return Err(
                        ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::Other)
                    );
                }
                unix::sync_directory(&parent)
            })
            .map_err(|error| failure(error, mutation_started))?;
        self.payload_files.insert(
            path.to_owned(),
            directory_identity(&file).map_err(|error| failure(error, true))?,
        );
        Ok(())
    }

    pub(crate) fn finish(
        &mut self,
        files: &BTreeMap<String, u64>,
        directories: &BTreeSet<String>,
        total_bytes: u64,
        writing_journal: &[u8],
        staged_journal: &[u8],
    ) -> Result<(), ConfigError> {
        self.revalidate_roots()?;
        self.verify_transaction_shape()?;
        self.verify_payload(files, directories, total_bytes)?;
        publish_journal(
            &self.transaction,
            self.plugins_device,
            Some(writing_journal),
            staged_journal,
        )?;
        self.revalidate_roots()?;
        self.verify_transaction_shape()?;
        verify_journal(
            &self.transaction,
            self.plugins_device,
            staged_journal,
            ConfigErrorKind::AtomicReplace,
        )
    }

    fn verify_payload(
        &self,
        files: &BTreeMap<String, u64>,
        directories: &BTreeSet<String>,
        total_bytes: u64,
    ) -> Result<(), ConfigError> {
        let mut observed = ObservedPayload::default();
        self.inspect_payload_directory(&self.payload, "", files, directories, &mut observed)?;
        if &observed.files != files
            || &observed.directories != directories
            || observed.total_bytes != total_bytes
        {
            return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
        }
        Ok(())
    }

    fn inspect_payload_directory(
        &self,
        directory: &File,
        prefix: &str,
        files: &BTreeMap<String, u64>,
        directories: &BTreeSet<String>,
        observed: &mut ObservedPayload,
    ) -> Result<(), ConfigError> {
        for name in directory_names(directory, MAX_STAGING_ENTRIES + 1)? {
            let name = name
                .to_str()
                .ok_or_else(|| ConfigError::new(ConfigErrorKind::AuthorityValidation))?;
            let path = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            BundlePath::parse(&path)?;
            if directories.contains(&path) {
                let child =
                    open_private_directory(directory, OsStr::new(name), self.plugins_device)
                        .map_err(authority_mismatch)?;
                if self.payload_directories.get(&path).copied() != Some(directory_identity(&child)?)
                {
                    return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
                }
                if !observed.directories.insert(path.clone())
                    || observed.directories.len() > MAX_STAGING_DIRECTORIES
                    || observed.entry_count()? > MAX_STAGING_ENTRIES
                {
                    return Err(ConfigError::new(ConfigErrorKind::Oversized));
                }
                self.inspect_payload_directory(&child, &path, files, directories, observed)?;
            } else if let Some(expected_size) = files.get(&path) {
                let file = open_private_regular(directory, OsStr::new(name), self.plugins_device)
                    .map_err(authority_mismatch)?;
                let actual_size = regular_size(&file, self.plugins_device)?;
                if self.payload_files.get(&path).copied() != Some(directory_identity(&file)?) {
                    return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
                }
                if actual_size != *expected_size {
                    return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
                }
                if observed.files.insert(path, actual_size).is_some()
                    || observed.files.len() > MAX_STAGING_FILES
                    || observed.entry_count()? > MAX_STAGING_ENTRIES
                {
                    return Err(ConfigError::new(ConfigErrorKind::Oversized));
                }
                observed.total_bytes = observed
                    .total_bytes
                    .checked_add(actual_size)
                    .ok_or_else(|| ConfigError::new(ConfigErrorKind::Oversized))?;
            } else {
                return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
            }
        }
        Ok(())
    }

    fn verify_transaction_shape(&self) -> Result<(), ConfigError> {
        let names = directory_names(&self.transaction, 4)?;
        let observed = names
            .into_iter()
            .map(|name| {
                name.into_string()
                    .map_err(|_| ConfigError::new(ConfigErrorKind::AuthorityValidation))
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let expected = BTreeSet::from([
            TRANSACTION_LOCK.to_owned(),
            PAYLOAD.to_owned(),
            JOURNAL.to_owned(),
        ]);
        if observed != expected {
            return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
        }
        let journal =
            open_private_regular(&self.transaction, OsStr::new(JOURNAL), self.plugins_device)?;
        if regular_size(&journal, self.plugins_device)? > MAX_STAGING_JOURNAL_BYTES as u64 {
            return Err(ConfigError::new(ConfigErrorKind::Oversized));
        }
        Ok(())
    }

    fn open_payload_directory(&self, path: &str) -> Result<File, ConfigError> {
        let mut current = self.payload.try_clone().map_err(io_error)?;
        for component in path.split('/').filter(|component| !component.is_empty()) {
            current = open_private_directory(&current, OsStr::new(component), self.plugins_device)?;
        }
        Ok(current)
    }

    fn revalidate_roots(&self) -> Result<(), ConfigError> {
        verify_exact_spelling(&self.root_parent, &self.root_name)?;
        verify_named_root_directory(
            &self.root_parent,
            &self.root_name,
            &self.root,
            self.plugins_device,
        )?;
        verify_directory(&self.root, self.plugins_device)?;
        verify_directory(&self.plugins, self.plugins_device)?;
        verify_directory(&self.staging, self.plugins_device)?;
        verify_directory(&self.transaction, self.plugins_device)?;
        verify_directory(&self.payload, self.plugins_device)?;
        if directory_identity(&self.root)? != self.root_identity
            || directory_identity(&self.plugins)? != self.plugins_identity
            || directory_identity(&self.staging)? != self.staging_identity
            || directory_identity(&self.transaction)? != self.transaction_identity
            || directory_identity(&self.payload)? != self.payload_identity
        {
            return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
        }
        verify_named_directory(
            &self.root,
            OsStr::new("plugins"),
            &self.plugins,
            self.plugins_device,
        )?;
        verify_named_directory(
            &self.plugins,
            OsStr::new(STAGING_ROOT),
            &self.staging,
            self.plugins_device,
        )?;
        verify_named_directory(
            &self.staging,
            OsStr::new(&self.id),
            &self.transaction,
            self.plugins_device,
        )?;
        verify_named_directory(
            &self.transaction,
            OsStr::new(PAYLOAD),
            &self.payload,
            self.plugins_device,
        )?;
        verify_regular(&self._lock, self.plugins_device, Some(0))?;
        verify_named_file(
            &self.transaction,
            OsStr::new(TRANSACTION_LOCK),
            &self._lock,
            self.plugins_device,
            0,
        )
    }
}

#[derive(Default)]
struct ObservedPayload {
    files: BTreeMap<String, u64>,
    directories: BTreeSet<String>,
    total_bytes: u64,
}

impl ObservedPayload {
    fn entry_count(&self) -> Result<usize, ConfigError> {
        self.files
            .len()
            .checked_add(self.directories.len())
            .ok_or_else(|| ConfigError::new(ConfigErrorKind::Oversized))
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        let _ = File::unlock(&self._lock);
    }
}

fn open_owned_root(home: &HomeLayout) -> Result<(File, OsString, File), ConfigError> {
    let root = home.root();
    let parent_path = root
        .parent()
        .ok_or_else(|| ConfigError::for_path(ConfigErrorKind::InvalidHome, root))?;
    let name = root
        .file_name()
        .ok_or_else(|| ConfigError::for_path(ConfigErrorKind::InvalidHome, root))?;
    let parent = unix::open_trailing_directory(parent_path)?;
    reject_wrong_case(&parent, name.to_str())?;
    let opened = unix::open_existing_directory(&parent, name)?;
    unix::verify_owned_directory(&opened)?;
    verify_exact_spelling(&parent, name)?;
    Ok((parent, name.to_os_string(), opened))
}

fn enforce_root_capacity(staging: &File) -> Result<(), ConfigError> {
    if directory_names(staging, MAX_STAGING_ROOT_ENTRIES)?.len() >= MAX_STAGING_ROOT_ENTRIES {
        return Err(ConfigError::new(ConfigErrorKind::Oversized));
    }
    Ok(())
}

fn directory_names(directory: &File, maximum: usize) -> Result<Vec<OsString>, ConfigError> {
    let entries = rfs::Dir::read_from(directory.as_fd())
        .map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if names.len() >= maximum {
            return Err(ConfigError::new(ConfigErrorKind::Oversized));
        }
        names.push(OsStr::from_bytes(name).to_os_string());
    }
    Ok(names)
}

fn create_or_open_lock(parent: &File, name: &OsStr, device: u64) -> Result<File, ConfigError> {
    reject_wrong_case(parent, name.to_str())?;
    let create_flags = OFlags::RDWR
        | OFlags::CREATE
        | OFlags::EXCL
        | OFlags::CLOEXEC
        | OFlags::NOFOLLOW
        | OFlags::NONBLOCK;
    let (file, created) = match unix::open_staging_component(parent, name, create_flags) {
        Ok(descriptor) => (File::from(descriptor), true),
        Err(error) if error.io_kind() == Some(io::ErrorKind::AlreadyExists) => {
            require_regular_entry(parent, name)?;
            let open_flags = OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
            (
                File::from(unix::open_staging_component(parent, name, open_flags)?),
                false,
            )
        }
        Err(error) => return Err(error),
    };
    if created {
        rfs::fchmod(file.as_fd(), Mode::from_raw_mode(FILE_MODE))
            .map_err(|error| unix::map_errno(error, ConfigErrorKind::AccessControl))?;
    }
    verify_regular(&file, device, Some(0))?;
    if created {
        sync_file(&file)?;
        unix::sync_directory(parent)?;
    }
    Ok(file)
}

fn create_or_open_private_directory(
    parent: &File,
    name: &OsStr,
    device: u64,
) -> Result<File, ConfigError> {
    reject_wrong_case(parent, name.to_str())?;
    let created = match rfs::mkdirat(parent.as_fd(), name, Mode::from_raw_mode(DIRECTORY_MODE)) {
        Ok(()) => true,
        Err(Errno::EXIST) => false,
        Err(error) => return Err(unix::map_errno(error, ConfigErrorKind::Io)),
    };
    let directory = if created {
        let directory = unix::open_staging_directory(parent, name)?;
        rfs::fchmod(directory.as_fd(), Mode::from_raw_mode(DIRECTORY_MODE))
            .map_err(|error| unix::map_errno(error, ConfigErrorKind::AccessControl))?;
        verify_directory(&directory, device)?;
        directory
    } else {
        open_private_directory(parent, name, device)?
    };
    if created {
        unix::sync_directory(&directory)?;
        unix::sync_directory(parent)?;
    }
    Ok(directory)
}

fn create_private_directory_exclusive(
    parent: &File,
    name: &OsStr,
    device: u64,
) -> Result<File, ConfigError> {
    reject_wrong_case(parent, name.to_str())?;
    rfs::mkdirat(parent.as_fd(), name, Mode::from_raw_mode(DIRECTORY_MODE))
        .map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))?;
    let directory = unix::open_staging_directory(parent, name)?;
    rfs::fchmod(directory.as_fd(), Mode::from_raw_mode(DIRECTORY_MODE))
        .map_err(|error| unix::map_errno(error, ConfigErrorKind::AccessControl))?;
    verify_directory(&directory, device)?;
    unix::sync_directory(&directory)?;
    unix::sync_directory(parent)?;
    Ok(directory)
}

fn create_payload_directory_exclusive(
    parent: &File,
    name: &OsStr,
    device: u64,
) -> Result<File, WriteFailure> {
    reject_wrong_case(parent, name.to_str()).map_err(|error| failure(error, false))?;
    rfs::mkdirat(parent.as_fd(), name, Mode::from_raw_mode(DIRECTORY_MODE))
        .map_err(|error| failure(unix::map_errno(error, ConfigErrorKind::Io), false))?;
    let prepared = unix::open_staging_directory(parent, name).and_then(|directory| {
        rfs::fchmod(directory.as_fd(), Mode::from_raw_mode(DIRECTORY_MODE))
            .map_err(|error| unix::map_errno(error, ConfigErrorKind::AccessControl))?;
        verify_directory(&directory, device)?;
        unix::sync_directory(&directory)?;
        unix::sync_directory(parent)?;
        Ok(directory)
    });
    prepared.map_err(|error| failure(error, true))
}

fn open_private_directory(parent: &File, name: &OsStr, device: u64) -> Result<File, ConfigError> {
    reject_wrong_case(parent, name.to_str())?;
    let directory = unix::open_staging_directory(parent, name)?;
    verify_directory(&directory, device)?;
    Ok(directory)
}

fn create_private_file_exclusive(
    parent: &File,
    name: &OsStr,
    device: u64,
) -> Result<File, ConfigError> {
    reject_wrong_case(parent, name.to_str())?;
    let flags = OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let file = File::from(unix::open_staging_component(parent, name, flags)?);
    rfs::fchmod(file.as_fd(), Mode::from_raw_mode(FILE_MODE))
        .map_err(|error| unix::map_errno(error, ConfigErrorKind::AccessControl))?;
    verify_regular(&file, device, Some(0))?;
    Ok(file)
}

fn create_payload_file_exclusive(
    parent: &File,
    name: &OsStr,
    device: u64,
) -> Result<File, WriteFailure> {
    reject_wrong_case(parent, name.to_str()).map_err(|error| failure(error, false))?;
    let flags = OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let file = File::from(
        unix::open_staging_component(parent, name, flags).map_err(|error| failure(error, false))?,
    );
    let prepared = rfs::fchmod(file.as_fd(), Mode::from_raw_mode(FILE_MODE))
        .map_err(|error| unix::map_errno(error, ConfigErrorKind::AccessControl))
        .and_then(|()| verify_regular(&file, device, Some(0)));
    prepared
        .map(|()| file)
        .map_err(|error| failure(error, true))
}

fn open_private_regular(parent: &File, name: &OsStr, device: u64) -> Result<File, ConfigError> {
    reject_wrong_case(parent, name.to_str())?;
    let before = require_regular_entry(parent, name)?;
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    let file = File::from(unix::open_staging_component(parent, name, flags)?);
    verify_regular(&file, device, None)?;
    if directory_identity(&file)?
        != (Identity {
            device: stat_device(&before)?,
            inode: before.st_ino,
        })
    {
        return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
    }
    Ok(file)
}

fn require_regular_entry(parent: &File, name: &OsStr) -> Result<rfs::Stat, ConfigError> {
    let stat = rfs::statat(parent.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))?;
    match rfs::FileType::from_raw_mode(stat.st_mode) {
        rfs::FileType::RegularFile => Ok(stat),
        rfs::FileType::Symlink => Err(ConfigError::new(ConfigErrorKind::LinkEscape)),
        _ => Err(ConfigError::new(ConfigErrorKind::AuthorityValidation)),
    }
}

fn verify_directory(file: &File, device: u64) -> Result<(), ConfigError> {
    let stat =
        rfs::fstat(file.as_fd()).map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))?;
    if rfs::FileType::from_raw_mode(stat.st_mode) != rfs::FileType::Directory
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || stat.st_mode & 0o777 != DIRECTORY_MODE
        || stat_device(&stat)? != device
    {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(())
}

fn verify_regular(file: &File, device: u64, size: Option<u64>) -> Result<(), ConfigError> {
    let stat =
        rfs::fstat(file.as_fd()).map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))?;
    let actual_size =
        u64::try_from(stat.st_size).map_err(|_| ConfigError::new(ConfigErrorKind::Oversized))?;
    if rfs::FileType::from_raw_mode(stat.st_mode) != rfs::FileType::RegularFile
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || stat.st_mode & 0o777 != FILE_MODE
        || stat.st_nlink != 1
        || stat_device(&stat)? != device
        || size.is_some_and(|expected| actual_size != expected)
    {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(())
}

fn regular_size(file: &File, device: u64) -> Result<u64, ConfigError> {
    verify_regular(file, device, None)?;
    let stat =
        rfs::fstat(file.as_fd()).map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))?;
    u64::try_from(stat.st_size).map_err(|_| ConfigError::new(ConfigErrorKind::Oversized))
}

fn directory_identity(file: &File) -> Result<Identity, ConfigError> {
    let stat =
        rfs::fstat(file.as_fd()).map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))?;
    Ok(Identity {
        device: stat_device(&stat)?,
        inode: stat.st_ino,
    })
}

#[cfg(target_vendor = "apple")]
fn stat_device(stat: &rfs::Stat) -> Result<u64, ConfigError> {
    u64::try_from(stat.st_dev).map_err(|_| ConfigError::new(ConfigErrorKind::AccessControl))
}

#[cfg(not(target_vendor = "apple"))]
fn stat_device(stat: &rfs::Stat) -> Result<u64, ConfigError> {
    Ok(stat.st_dev)
}

fn verify_named_root_directory(
    parent: &File,
    name: &OsStr,
    retained: &File,
    device: u64,
) -> Result<(), ConfigError> {
    let reopened = unix::open_existing_directory(parent, name).map_err(authority_mismatch)?;
    verify_directory(&reopened, device).map_err(authority_mismatch)?;
    if directory_identity(&reopened)? != directory_identity(retained)? {
        return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
    }
    Ok(())
}

fn verify_named_directory(
    parent: &File,
    name: &OsStr,
    retained: &File,
    device: u64,
) -> Result<(), ConfigError> {
    let reopened = open_private_directory(parent, name, device).map_err(authority_mismatch)?;
    if directory_identity(&reopened)? != directory_identity(retained)? {
        return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
    }
    Ok(())
}

fn verify_named_file(
    parent: &File,
    name: &OsStr,
    retained: &File,
    device: u64,
    size: u64,
) -> Result<(), ConfigError> {
    let reopened = open_private_regular(parent, name, device).map_err(authority_mismatch)?;
    verify_regular(&reopened, device, Some(size))?;
    if directory_identity(&reopened)? != directory_identity(retained)? {
        return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
    }
    Ok(())
}

fn publish_journal(
    parent: &File,
    device: u64,
    expected_current: Option<&[u8]>,
    bytes: &[u8],
) -> Result<(), ConfigError> {
    if bytes.len() > MAX_STAGING_JOURNAL_BYTES {
        return Err(ConfigError::new(ConfigErrorKind::Oversized));
    }
    if let Some(expected) = expected_current {
        verify_journal(
            parent,
            device,
            expected,
            ConfigErrorKind::AuthorityValidation,
        )?;
    }
    let mut temporary = create_private_file_exclusive(parent, OsStr::new(JOURNAL_TEMP), device)?;
    #[cfg(test)]
    if FAIL_NEXT_JOURNAL_TEMP_PREPARE.with(|fail| fail.replace(false)) {
        return Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::Other));
    }
    let prepared = temporary
        .write_all(bytes)
        .map_err(io_error)
        .and_then(|()| temporary.flush().map_err(io_error))
        .and_then(|()| sync_file(&temporary))
        .and_then(|()| verify_regular(&temporary, device, Some(bytes.len() as u64)));
    prepared?;
    if let Err(error) = rfs::renameat(parent.as_fd(), JOURNAL_TEMP, parent.as_fd(), JOURNAL) {
        return Err(unix::map_errno(error, ConfigErrorKind::AtomicReplace));
    }
    verify_named_file(
        parent,
        OsStr::new(JOURNAL),
        &temporary,
        device,
        bytes.len() as u64,
    )
    .map_err(|_| ConfigError::new(ConfigErrorKind::AtomicReplace))?;
    unix::sync_directory(parent)
}

fn verify_journal(
    parent: &File,
    device: u64,
    expected: &[u8],
    mismatch_kind: ConfigErrorKind,
) -> Result<(), ConfigError> {
    let mut file =
        open_private_regular(parent, OsStr::new(JOURNAL), device).map_err(authority_mismatch)?;
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let mut bytes = Vec::with_capacity(expected.len());
    file.take((MAX_STAGING_JOURNAL_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes != expected {
        return Err(ConfigError::new(mismatch_kind));
    }
    Ok(())
}

fn reject_wrong_case(parent: &File, expected: Option<&str>) -> Result<(), ConfigError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    inspect_spelling(parent, OsStr::new(expected)).map(|_| ())
}

fn verify_exact_spelling(parent: &File, expected: &OsStr) -> Result<(), ConfigError> {
    if !inspect_spelling(parent, expected)? {
        return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
    }
    Ok(())
}

fn inspect_spelling(parent: &File, expected: &OsStr) -> Result<bool, ConfigError> {
    let entries = rfs::Dir::read_from(parent.as_fd())
        .map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))?;
    let mut exact = false;
    for entry in entries {
        let entry = entry.map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))?;
        let name = OsStr::from_bytes(entry.file_name().to_bytes());
        if name == expected {
            exact = true;
        } else if expected.to_str().is_some_and(|expected| {
            name.to_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(expected))
        }) {
            return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
        }
    }
    Ok(exact)
}

#[cfg(target_vendor = "apple")]
fn sync_file(file: &File) -> Result<(), ConfigError> {
    rfs::fcntl_fullfsync(file.as_fd()).map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))
}

#[cfg(not(target_vendor = "apple"))]
fn sync_file(file: &File) -> Result<(), ConfigError> {
    rfs::fsync(file.as_fd()).map_err(|error| unix::map_errno(error, ConfigErrorKind::Io))
}

fn split_parent(path: &str) -> (&str, &str) {
    path.rsplit_once('/')
        .map_or(("", path), |(parent, name)| (parent, name))
}

fn failure(error: ConfigError, mutation_started: bool) -> WriteFailure {
    WriteFailure {
        error,
        mutation_started,
    }
}

fn io_error(error: io::Error) -> ConfigError {
    ConfigError::new(ConfigErrorKind::Io).with_io_kind(error.kind())
}

fn authority_mismatch(_error: ConfigError) -> ConfigError {
    ConfigError::new(ConfigErrorKind::AuthorityValidation)
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_PAYLOAD_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_NEXT_PAYLOAD_BARRIER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_NEXT_JOURNAL_TEMP_PREPARE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn fail_next_payload_write_for_test() {
    FAIL_NEXT_PAYLOAD_WRITE.with(|fail| fail.set(true));
}

#[cfg(test)]
pub(crate) fn fail_next_payload_barrier_for_test() {
    FAIL_NEXT_PAYLOAD_BARRIER.with(|fail| fail.set(true));
}

#[cfg(test)]
pub(crate) fn fail_next_journal_temp_prepare_for_test() {
    FAIL_NEXT_JOURNAL_TEMP_PREPARE.with(|fail| fail.set(true));
}

fn lock_error(error: io::Error) -> ConfigError {
    ConfigError::new(ConfigErrorKind::Lock).with_io_kind(error.kind())
}

fn try_lock_error(error: std::fs::TryLockError) -> ConfigError {
    match error {
        std::fs::TryLockError::Error(error) => lock_error(error),
        std::fs::TryLockError::WouldBlock => {
            ConfigError::new(ConfigErrorKind::Lock).with_io_kind(io::ErrorKind::WouldBlock)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use crate::{ConfigErrorKind, HomeLayout, begin_staging, ensure_home_layout};

    #[test]
    fn existing_global_lock_is_rejected_without_mode_repair() {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        ensure_home_layout(&layout).expect("bootstrap");
        fs::write(layout.host_staging_lock(), []).expect("foreign lock fixture");
        fs::set_permissions(
            layout.host_staging_lock(),
            fs::Permissions::from_mode(0o666),
        )
        .expect("permissive mode");

        assert_eq!(
            begin_staging(&layout).err().expect("invalid lock").kind(),
            ConfigErrorKind::AccessControl
        );
        assert_eq!(
            fs::metadata(layout.host_staging_lock())
                .expect("lock metadata")
                .mode()
                & 0o777,
            0o666
        );
        assert_eq!(
            fs::metadata(layout.host_staging_lock())
                .expect("lock metadata")
                .nlink(),
            1
        );
        assert!(!layout.host_staging_dir().exists());
    }
}
