//! Windows rooted native bounded staging writer.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Write};
use std::mem::size_of;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::{
    ERROR_MORE_DATA, ERROR_NO_MORE_FILES, GENERIC_READ, GENERIC_WRITE,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ID_INFO,
    FILE_READ_ATTRIBUTES, FILE_STANDARD_INFO, FileFullDirectoryInfo, FileFullDirectoryRestartInfo,
    FileIdInfo, FileStandardInfo, GetFileInformationByHandleEx, READ_CONTROL, SYNCHRONIZE,
    WRITE_DAC,
};

#[path = "windows_staging_journal.rs"]
mod journal;
#[path = "windows_staging_recovery.rs"]
mod recovery;

pub(crate) use recovery::recover_abandoned;
#[cfg(test)]
pub(crate) use recovery::{
    after_final_recovery_snapshot_for_test, after_recovery_preflight_for_test,
    fail_next_recovery_staging_barrier_for_test, fail_recovery_disposition_for_test,
    fail_recovery_unlock_for_test,
};

#[cfg(test)]
pub(crate) use self::journal::fail_next_journal_temp_prepare_for_test;
use self::journal::{publish_journal, verify_journal};
use super::{windows_acl, windows_file, windows_open};
use crate::staging::{
    MAX_STAGING_DIRECTORIES, MAX_STAGING_ENTRIES, MAX_STAGING_FILES, MAX_STAGING_JOURNAL_BYTES,
    MAX_STAGING_ROOT_ENTRIES, WriteFailure,
};
use crate::{BundlePath, ConfigError, ConfigErrorKind, HomeLayout, TransactionId};

const GLOBAL_LOCK: &str = ".staging.lock";
const STAGING_ROOT: &str = ".staging";
const TRANSACTION_LOCK: &str = "transaction.lock";
const PAYLOAD: &str = "payload";
const JOURNAL: &str = "journal.json";
const MAX_ID_ATTEMPTS: usize = 16;
const READ_FILE_ACCESS: u32 = GENERIC_READ | READ_CONTROL | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
const WRITE_FILE_ACCESS: u32 =
    GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC | DELETE | SYNCHRONIZE;
const LOCK_FILE_ACCESS: u32 = GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC | SYNCHRONIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    volume: u64,
    id: [u8; 16],
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
    volume: u64,
    _lock: File,
    id: String,
}

impl Transaction {
    pub(crate) fn begin(
        home: &HomeLayout,
        journal: impl Fn(&TransactionId) -> Result<Vec<u8>, ConfigError>,
    ) -> Result<(TransactionId, Self), ConfigError> {
        let opened_root = windows_open::open_existing_owned_root(home.root())?;
        let root_parent = opened_root.parent;
        let root = opened_root.root;
        windows_acl::verify_fixed_descriptor(&root)?;
        let root_identity = identity(&root)?;
        let root_name = home
            .root()
            .file_name()
            .ok_or_else(|| ConfigError::new(ConfigErrorKind::InvalidHome))?
            .to_os_string();
        let plugins = open_private_directory(&root, OsStr::new("plugins"), root_identity.volume)?;
        let plugins_identity = identity(&plugins)?;
        let volume = plugins_identity.volume;
        let (global_lock, new_lock) = create_or_open_lock(&plugins, volume)?;
        if new_lock {
            windows_file::flush_file(&global_lock)?;
            super::flush_directory(&plugins)?;
        }
        File::lock(&global_lock).map_err(lock_error)?;
        let result = (|| {
            let staging =
                create_or_open_private_directory(&plugins, OsStr::new(STAGING_ROOT), volume)?;
            enforce_root_capacity(&staging)?;
            let staging_identity = identity(&staging)?;
            for _ in 0..MAX_ID_ATTEMPTS {
                let id = TransactionId::generate()?;
                let transaction = match create_private_directory_exclusive(
                    &staging,
                    OsStr::new(id.as_str()),
                    volume,
                ) {
                    Ok(directory) => directory,
                    Err(error) if error.io_kind() == Some(io::ErrorKind::AlreadyExists) => continue,
                    Err(error) => return Err(error),
                };
                let transaction_identity = identity(&transaction)?;
                let lock = create_private_file_exclusive(
                    &transaction,
                    OsStr::new(TRANSACTION_LOCK),
                    LOCK_FILE_ACCESS,
                    volume,
                )?;
                windows_file::flush_file(&lock)?;
                super::flush_directory(&transaction)?;
                File::try_lock(&lock).map_err(try_lock_error)?;
                let payload =
                    create_private_directory_exclusive(&transaction, OsStr::new(PAYLOAD), volume)?;
                let payload_identity = identity(&payload)?;
                let writing_journal = journal(&id)?;
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
                    volume,
                    _lock: lock,
                    id: id_spelling,
                };
                native.revalidate_roots()?;
                verify_named_file(
                    &native.plugins,
                    OsStr::new(GLOBAL_LOCK),
                    &global_lock,
                    volume,
                    0,
                )?;
                publish_journal(&native.transaction, volume, None, &writing_journal)?;
                native.revalidate_roots()?;
                verify_named_file(
                    &native.plugins,
                    OsStr::new(GLOBAL_LOCK),
                    &global_lock,
                    volume,
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
            let child = create_payload_directory_exclusive(&parent, OsStr::new(name), self.volume)
                .map_err(|created| WriteFailure {
                    error: created.error,
                    mutation_started: mutation_started || created.mutation_started,
                })?;
            mutation_started = true;
            verify_named_directory(&parent, OsStr::new(name), &child, self.volume)
                .map_err(|error| failure(error, true))?;
            self.payload_directories.insert(
                directory.clone(),
                identity(&child).map_err(|error| failure(error, true))?,
            );
        }

        let (parent_path, name) = split_parent(path);
        let parent = self
            .open_payload_directory(parent_path)
            .map_err(|error| failure(error, mutation_started))?;
        let mut file = create_payload_file_exclusive(
            &parent,
            OsStr::new(name),
            WRITE_FILE_ACCESS,
            self.volume,
        )
        .map_err(|created| WriteFailure {
            error: created.error,
            mutation_started: mutation_started || created.mutation_started,
        })?;
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
            .and_then(|()| windows_file::flush_file(&file))
            .and_then(|()| verify_regular(&file, self.volume, Some(expected_size)))
            .and_then(|()| {
                verify_named_file(&parent, OsStr::new(name), &file, self.volume, expected_size)
            })
            .and_then(|()| {
                #[cfg(test)]
                if FAIL_NEXT_PAYLOAD_BARRIER.with(|fail| fail.replace(false)) {
                    return Err(
                        ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::Other)
                    );
                }
                super::flush_directory(&parent)
            })
            .map_err(|error| failure(error, mutation_started))?;
        self.payload_files.insert(
            path.to_owned(),
            identity(&file).map_err(|error| failure(error, true))?,
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
            self.volume,
            Some(writing_journal),
            staged_journal,
        )?;
        self.revalidate_roots()?;
        self.verify_transaction_shape()?;
        verify_journal(
            &self.transaction,
            self.volume,
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
        let payload = open_private_directory(&self.transaction, OsStr::new(PAYLOAD), self.volume)?;
        if identity(&payload)? != self.payload_identity {
            return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
        }
        let mut observed = ObservedPayload::default();
        self.inspect_payload_directory(&payload, "", files, directories, &mut observed)?;
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
                let child = open_private_directory(directory, OsStr::new(name), self.volume)
                    .map_err(authority_mismatch)?;
                if self.payload_directories.get(&path).copied() != Some(identity(&child)?) {
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
                let file = open_private_regular(directory, OsStr::new(name), self.volume)
                    .map_err(authority_mismatch)?;
                let actual_size = regular_size(&file, self.volume)?;
                if self.payload_files.get(&path).copied() != Some(identity(&file)?) {
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
        let transaction = open_private_directory(&self.staging, OsStr::new(&self.id), self.volume)?;
        if identity(&transaction)? != self.transaction_identity {
            return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
        }
        let names = directory_names(&transaction, 4)?;
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
        let journal = open_private_regular(&transaction, OsStr::new(JOURNAL), self.volume)?;
        if regular_size(&journal, self.volume)? > MAX_STAGING_JOURNAL_BYTES as u64 {
            return Err(ConfigError::new(ConfigErrorKind::Oversized));
        }
        Ok(())
    }

    fn open_payload_directory(&self, path: &str) -> Result<File, ConfigError> {
        let mut current = self.payload.try_clone().map_err(io_error)?;
        for component in path.split('/').filter(|component| !component.is_empty()) {
            current = open_private_directory(&current, OsStr::new(component), self.volume)?;
        }
        Ok(current)
    }

    fn revalidate_roots(&self) -> Result<(), ConfigError> {
        windows_open::verify_exact_root_spelling(&self.root_parent, &self.root_name)
            .map_err(authority_mismatch)?;
        verify_named_root_directory(&self.root_parent, &self.root_name, &self.root, self.volume)?;
        verify_directory(&self.root, self.volume)?;
        verify_directory(&self.plugins, self.volume)?;
        verify_directory(&self.staging, self.volume)?;
        verify_directory(&self.transaction, self.volume)?;
        verify_directory(&self.payload, self.volume)?;
        if identity(&self.root)? != self.root_identity
            || identity(&self.plugins)? != self.plugins_identity
            || identity(&self.staging)? != self.staging_identity
            || identity(&self.transaction)? != self.transaction_identity
            || identity(&self.payload)? != self.payload_identity
        {
            return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
        }
        verify_named_directory(
            &self.root,
            OsStr::new("plugins"),
            &self.plugins,
            self.volume,
        )?;
        verify_named_directory(
            &self.plugins,
            OsStr::new(STAGING_ROOT),
            &self.staging,
            self.volume,
        )?;
        verify_named_directory(
            &self.staging,
            OsStr::new(&self.id),
            &self.transaction,
            self.volume,
        )?;
        verify_named_directory(
            &self.transaction,
            OsStr::new(PAYLOAD),
            &self.payload,
            self.volume,
        )?;
        verify_regular(&self._lock, self.volume, Some(0))?;
        verify_named_file(
            &self.transaction,
            OsStr::new(TRANSACTION_LOCK),
            &self._lock,
            self.volume,
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

fn create_or_open_lock(parent: &File, volume: u64) -> Result<(File, bool), ConfigError> {
    reject_wrong_case(parent, Some(GLOBAL_LOCK))?;
    let descriptor = windows_acl::protected_descriptor()?;
    let observed_missing =
        windows_open::child_attributes(parent, OsStr::new(GLOBAL_LOCK))?.is_none();
    let opened = windows_open::open_relative_file(
        parent,
        OsStr::new(GLOBAL_LOCK),
        LOCK_FILE_ACCESS,
        windows_open::OPEN_OR_CREATE_FILE_DISPOSITION,
        Some(&descriptor),
    )?
    .ok_or_else(|| ConfigError::new(ConfigErrorKind::Lock))?;
    let publication_required = observed_missing || opened.created;
    verify_regular(&opened.file, volume, Some(0))?;
    Ok((opened.file, publication_required))
}

fn create_or_open_private_directory(
    parent: &File,
    name: &OsStr,
    volume: u64,
) -> Result<File, ConfigError> {
    reject_wrong_case(parent, name.to_str())?;
    let descriptor = windows_acl::protected_descriptor()?;
    let observed_missing = windows_open::child_attributes(parent, name)?.is_none();
    let opened = windows_open::open_relative_directory(
        parent,
        name,
        windows_open::OWNED_DIRECTORY_ACCESS,
        windows_open::OPEN_OR_CREATE_FILE_DISPOSITION,
        Some(&descriptor),
    )?;
    verify_directory(&opened.file, volume).map_err(|error| {
        if observed_missing {
            error
        } else {
            authority_mismatch(error)
        }
    })?;
    if observed_missing || opened.publication_required() {
        super::flush_directory(&opened.file)?;
        super::flush_directory(parent)?;
    }
    Ok(opened.file)
}

fn create_private_directory_exclusive(
    parent: &File,
    name: &OsStr,
    volume: u64,
) -> Result<File, ConfigError> {
    reject_wrong_case(parent, name.to_str())?;
    let descriptor = windows_acl::protected_descriptor()?;
    let opened = windows_open::open_relative_directory(
        parent,
        name,
        windows_open::OWNED_DIRECTORY_ACCESS,
        windows_open::CREATE_FILE_DISPOSITION,
        Some(&descriptor),
    )?;
    verify_directory(&opened.file, volume)?;
    super::flush_directory(&opened.file)?;
    super::flush_directory(parent)?;
    Ok(opened.file)
}

fn create_payload_directory_exclusive(
    parent: &File,
    name: &OsStr,
    volume: u64,
) -> Result<File, WriteFailure> {
    let descriptor = windows_acl::protected_descriptor().map_err(|error| failure(error, false))?;
    reject_wrong_case(parent, name.to_str()).map_err(|error| failure(error, false))?;
    let opened = windows_open::open_relative_directory(
        parent,
        name,
        windows_open::OWNED_DIRECTORY_ACCESS,
        windows_open::CREATE_FILE_DISPOSITION,
        Some(&descriptor),
    )
    .map_err(|error| failure(error, true))?;
    #[cfg(test)]
    if FAIL_NEXT_PAYLOAD_DIRECTORY_PREPARE.with(|fail| fail.replace(false)) {
        return Err(failure(
            ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::Other),
            true,
        ));
    }
    verify_directory(&opened.file, volume)
        .and_then(|()| super::flush_directory(&opened.file))
        .and_then(|()| super::flush_directory(parent))
        .map_err(|error| failure(error, true))?;
    Ok(opened.file)
}

fn open_private_directory(parent: &File, name: &OsStr, volume: u64) -> Result<File, ConfigError> {
    reject_wrong_case(parent, name.to_str())?;
    let file = windows_open::open_relative_directory_exact(
        parent,
        name,
        windows_open::OWNED_DIRECTORY_ACCESS,
    )?;
    verify_directory(&file, volume)?;
    Ok(file)
}

fn create_private_file_exclusive(
    parent: &File,
    name: &OsStr,
    access: u32,
    volume: u64,
) -> Result<File, ConfigError> {
    reject_wrong_case(parent, name.to_str())?;
    let descriptor = windows_acl::protected_descriptor()?;
    let opened = windows_open::open_relative_file(
        parent,
        name,
        access,
        windows_open::CREATE_FILE_DISPOSITION,
        Some(&descriptor),
    )?
    .ok_or_else(|| ConfigError::new(ConfigErrorKind::Io))?;
    if !opened.created {
        return Err(
            ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::AlreadyExists)
        );
    }
    verify_regular(&opened.file, volume, Some(0))?;
    Ok(opened.file)
}

fn create_payload_file_exclusive(
    parent: &File,
    name: &OsStr,
    access: u32,
    volume: u64,
) -> Result<File, WriteFailure> {
    reject_wrong_case(parent, name.to_str()).map_err(|error| failure(error, false))?;
    let descriptor = windows_acl::protected_descriptor().map_err(|error| failure(error, false))?;
    let opened = windows_open::open_relative_file(
        parent,
        name,
        access,
        windows_open::CREATE_FILE_DISPOSITION,
        Some(&descriptor),
    )
    .map_err(|error| failure(error, false))?
    .ok_or_else(|| failure(ConfigError::new(ConfigErrorKind::Io), false))?;
    if !opened.created {
        return Err(failure(
            ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::AlreadyExists),
            false,
        ));
    }
    verify_regular(&opened.file, volume, Some(0)).map_err(|error| failure(error, true))?;
    Ok(opened.file)
}

fn open_private_regular(parent: &File, name: &OsStr, volume: u64) -> Result<File, ConfigError> {
    reject_wrong_case(parent, name.to_str())?;
    let file = windows_open::open_relative_file_exact(parent, name, READ_FILE_ACCESS)?;
    verify_regular(&file, volume, None)?;
    Ok(file)
}

fn verify_directory(file: &File, volume: u64) -> Result<(), ConfigError> {
    windows_acl::verify_fixed_descriptor(file)?;
    let attributes = windows_open::file_attributes(file)?;
    let standard = standard_info(file)?;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || attributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || !standard.Directory
        || standard.NumberOfLinks == 0
        || identity(file)?.volume != volume
    {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(())
}

fn verify_regular(file: &File, volume: u64, size: Option<u64>) -> Result<(), ConfigError> {
    windows_acl::verify_fixed_descriptor(file)?;
    let attributes = windows_open::file_attributes(file)?;
    let standard = standard_info(file)?;
    let actual_size = u64::try_from(standard.EndOfFile)
        .map_err(|_| ConfigError::new(ConfigErrorKind::Oversized))?;
    if attributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0
        || standard.Directory
        || standard.NumberOfLinks != 1
        || identity(file)?.volume != volume
        || size.is_some_and(|expected| actual_size != expected)
    {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(())
}

fn regular_size(file: &File, volume: u64) -> Result<u64, ConfigError> {
    verify_regular(file, volume, None)?;
    u64::try_from(standard_info(file)?.EndOfFile)
        .map_err(|_| ConfigError::new(ConfigErrorKind::Oversized))
}

fn identity(file: &File) -> Result<Identity, ConfigError> {
    let mut information = FILE_ID_INFO::default();
    // SAFETY: `file` is live and `information` is correctly sized writable output.
    let queried = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            std::ptr::addr_of_mut!(information).cast(),
            u32::try_from(size_of::<FILE_ID_INFO>()).expect("FILE_ID_INFO fits u32"),
        )
    };
    if queried == 0 {
        let error = io::Error::last_os_error();
        return Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(error.kind()));
    }
    identity_from_parts(
        information.VolumeSerialNumber,
        information.FileId.Identifier,
    )
}

fn identity_from_parts(volume: u64, id: [u8; 16]) -> Result<Identity, ConfigError> {
    if id == [0; 16] {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(Identity { volume, id })
}

fn standard_info(file: &File) -> Result<FILE_STANDARD_INFO, ConfigError> {
    let mut information = FILE_STANDARD_INFO::default();
    // SAFETY: `file` is live and `information` is correctly sized writable output.
    let queried = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStandardInfo,
            std::ptr::addr_of_mut!(information).cast(),
            u32::try_from(size_of::<FILE_STANDARD_INFO>()).expect("FILE_STANDARD_INFO fits u32"),
        )
    };
    if queried == 0 {
        let error = io::Error::last_os_error();
        return Err(ConfigError::new(ConfigErrorKind::Io).with_io_kind(error.kind()));
    }
    Ok(information)
}

fn verify_named_root_directory(
    parent: &File,
    name: &OsStr,
    retained: &File,
    volume: u64,
) -> Result<(), ConfigError> {
    let reopened =
        windows_open::open_owned_relative_exact(parent, name).map_err(authority_mismatch)?;
    verify_directory(&reopened, volume).map_err(authority_mismatch)?;
    if identity(&reopened)? != identity(retained)? {
        return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
    }
    Ok(())
}

fn verify_named_directory(
    parent: &File,
    name: &OsStr,
    retained: &File,
    volume: u64,
) -> Result<(), ConfigError> {
    let reopened = open_private_directory(parent, name, volume).map_err(authority_mismatch)?;
    if identity(&reopened)? != identity(retained)? {
        return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
    }
    Ok(())
}

fn verify_named_file(
    parent: &File,
    name: &OsStr,
    retained: &File,
    volume: u64,
    size: u64,
) -> Result<(), ConfigError> {
    let reopened = open_private_regular(parent, name, volume).map_err(authority_mismatch)?;
    verify_regular(&reopened, volume, Some(size))?;
    if identity(&reopened)? != identity(retained)? {
        return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
    }
    Ok(())
}

fn directory_names(directory: &File, maximum: usize) -> Result<Vec<OsString>, ConfigError> {
    let mut storage = vec![0u64; 2 * 1024];
    let mut information_class = FileFullDirectoryRestartInfo;
    let mut started = false;
    let mut names = Vec::new();
    loop {
        let byte_length = storage.len().saturating_mul(size_of::<u64>());
        // SAFETY: `directory` is live and storage is aligned writable output.
        let queried = unsafe {
            GetFileInformationByHandleEx(
                directory.as_raw_handle(),
                information_class,
                storage.as_mut_ptr().cast(),
                u32::try_from(byte_length).expect("directory buffer fits u32"),
            )
        };
        if queried == 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(code) if code == ERROR_NO_MORE_FILES as i32 => break,
                Some(code)
                    if code == ERROR_MORE_DATA as i32 && !started && storage.len() < 128 * 1024 =>
                {
                    storage.resize(storage.len().saturating_mul(2), 0);
                    continue;
                }
                _ => return Err(io_error(error)),
            }
        }
        windows_open::visit_directory_names(
            // SAFETY: the successful call initialized records in this buffer.
            unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), byte_length) },
            &mut |name| {
                if name != "." && name != ".." {
                    if names.len() >= maximum {
                        return Err(ConfigError::new(ConfigErrorKind::Oversized));
                    }
                    names.push(name);
                }
                Ok(None::<()>)
            },
        )?;
        started = true;
        information_class = FileFullDirectoryInfo;
    }
    Ok(names)
}

fn reject_wrong_case(parent: &File, expected: Option<&str>) -> Result<(), ConfigError> {
    if let Some(expected) = expected
        && windows_open::find_wrong_case_child(parent, expected)?.is_some()
    {
        return Err(ConfigError::new(ConfigErrorKind::AuthorityValidation));
    }
    Ok(())
}

fn enforce_root_capacity(directory: &File) -> Result<(), ConfigError> {
    if directory_names(directory, MAX_STAGING_ROOT_ENTRIES)?.len() >= MAX_STAGING_ROOT_ENTRIES {
        return Err(ConfigError::new(ConfigErrorKind::Oversized));
    }
    Ok(())
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
    static FAIL_NEXT_PAYLOAD_DIRECTORY_PREPARE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
pub(crate) fn fail_next_payload_directory_prepare_for_test() {
    FAIL_NEXT_PAYLOAD_DIRECTORY_PREPARE.with(|fail| fail.set(true));
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
#[path = "windows_staging_tests.rs"]
mod tests;
