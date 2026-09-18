//! Fail-closed staging backend for unsupported operating systems.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use crate::staging::WriteFailure;
use crate::{ConfigError, ConfigErrorKind, HomeLayout, TransactionId};

pub(crate) fn recover_abandoned(_home: &HomeLayout) -> Result<usize, ConfigError> {
    Err(native_unavailable())
}

pub(crate) struct Transaction;

impl Transaction {
    pub(crate) fn begin(
        _home: &HomeLayout,
        _journal: impl Fn(&TransactionId) -> Result<Vec<u8>, ConfigError>,
    ) -> Result<(TransactionId, Self), ConfigError> {
        Err(native_unavailable())
    }

    pub(crate) fn write_file(
        &mut self,
        _path: &str,
        _new_directories: &[String],
        _bytes: &[u8],
        _expected_size: u64,
    ) -> Result<(), WriteFailure> {
        Err(WriteFailure {
            error: native_unavailable(),
            mutation_started: false,
        })
    }

    pub(crate) fn finish(
        &mut self,
        _files: &BTreeMap<String, u64>,
        _directories: &BTreeSet<String>,
        _total_bytes: u64,
        _writing_journal: &[u8],
        _staged_journal: &[u8],
    ) -> Result<(), ConfigError> {
        Err(native_unavailable())
    }
}

fn native_unavailable() -> ConfigError {
    ConfigError::new(ConfigErrorKind::AccessControl).with_io_kind(io::ErrorKind::Unsupported)
}
