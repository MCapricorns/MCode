//! Windows staging journal publication and temporary-file lifecycle.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

use super::{
    JOURNAL, WRITE_FILE_ACCESS, authority_mismatch, io_error, open_private_regular,
    reject_wrong_case, verify_named_file, verify_regular, windows_acl, windows_file, windows_open,
};
use crate::staging::MAX_STAGING_JOURNAL_BYTES;
use crate::{ConfigError, ConfigErrorKind};

const JOURNAL_TEMP: &str = ".journal.json.tmp";

pub(super) fn publish_journal(
    parent: &File,
    volume: u64,
    expected_current: Option<&[u8]>,
    bytes: &[u8],
) -> Result<(), ConfigError> {
    if bytes.len() > MAX_STAGING_JOURNAL_BYTES {
        return Err(ConfigError::new(ConfigErrorKind::Oversized));
    }
    if let Some(expected) = expected_current {
        verify_journal(
            parent,
            volume,
            expected,
            ConfigErrorKind::AuthorityValidation,
        )?;
    }
    reject_wrong_case(parent, Some(JOURNAL_TEMP))?;
    let descriptor = windows_acl::protected_descriptor()?;
    let opened = windows_open::open_relative_file(
        parent,
        OsStr::new(JOURNAL_TEMP),
        WRITE_FILE_ACCESS,
        windows_open::CREATE_FILE_DISPOSITION,
        Some(&descriptor),
    )?
    .ok_or_else(|| ConfigError::new(ConfigErrorKind::Io))?;
    let mut temporary = JournalTemporary::new(opened.file);
    if !opened.created {
        temporary.disarm();
        return Err(
            ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::AlreadyExists)
        );
    }
    #[cfg(test)]
    if FAIL_NEXT_JOURNAL_TEMP_PREPARE.with(|fail| fail.replace(false)) {
        return temporary
            .fail(ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::Other));
    }
    let prepared = temporary
        .file_mut()
        .write_all(bytes)
        .map_err(io_error)
        .and_then(|()| temporary.file_mut().flush().map_err(io_error))
        .and_then(|()| windows_file::flush_file(temporary.file()))
        .and_then(|()| verify_regular(temporary.file(), volume, Some(bytes.len() as u64)));
    if let Err(error) = prepared {
        return temporary.fail(error);
    }
    if let Err(error) = windows_file::rename_relative(parent, temporary.file(), OsStr::new(JOURNAL))
    {
        return temporary.fail(error);
    }
    temporary.disarm();
    verify_named_file(
        parent,
        OsStr::new(JOURNAL),
        temporary.file(),
        volume,
        bytes.len() as u64,
    )
    .map_err(|_| ConfigError::new(ConfigErrorKind::AtomicReplace))?;
    super::super::flush_directory(parent)
}

pub(super) fn verify_journal(
    parent: &File,
    volume: u64,
    expected: &[u8],
    mismatch_kind: ConfigErrorKind,
) -> Result<(), ConfigError> {
    let mut file =
        open_private_regular(parent, OsStr::new(JOURNAL), volume).map_err(authority_mismatch)?;
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

struct JournalTemporary {
    file: File,
    armed: bool,
}

impl JournalTemporary {
    fn new(file: File) -> Self {
        Self { file, armed: true }
    }

    fn file(&self) -> &File {
        &self.file
    }

    fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn fail(mut self, error: ConfigError) -> Result<(), ConfigError> {
        windows_file::set_delete(&self.file)?;
        self.armed = false;
        Err(error)
    }
}

impl Drop for JournalTemporary {
    fn drop(&mut self) {
        if self.armed {
            let _ = windows_file::set_delete(&self.file);
        }
    }
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_JOURNAL_TEMP_PREPARE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn fail_next_journal_temp_prepare_for_test() {
    FAIL_NEXT_JOURNAL_TEMP_PREPARE.with(|fail| fail.set(true));
}
