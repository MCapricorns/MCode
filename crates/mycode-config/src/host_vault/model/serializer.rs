//! Serializes Host-vault documents into one bounded zeroizing allocation.

#[cfg(test)]
use std::cell::Cell;
use std::io::{self, Write};

use serde::Serialize;
use zeroize::Zeroizing;

use super::VaultDocument;
use crate::{ConfigError, ConfigErrorKind};

use crate::host_vault::MAX_HOST_VAULT_BYTES;

#[cfg(test)]
thread_local! {
    static SERIALIZATION_COUNT: Cell<usize> = const { Cell::new(0) };
}

pub(in crate::host_vault) fn serialize_document(
    document: &VaultDocument<'_>,
) -> Result<Zeroizing<Vec<u8>>, ConfigError> {
    #[cfg(test)]
    SERIALIZATION_COUNT.with(|count| count.set(count.get() + 1));

    let mut output = BoundedZeroizingWriter::new()?;
    {
        let mut serializer = serde_json::Serializer::new(&mut output);
        document
            .serialize(&mut serializer)
            .map_err(|_| ConfigError::new(ConfigErrorKind::Serialization))?;
    }
    output.finish()
}

struct BoundedZeroizingWriter {
    bytes: Zeroizing<Vec<u8>>,
    initial_capacity: usize,
}

impl BoundedZeroizingWriter {
    fn new() -> Result<Self, ConfigError> {
        let bytes = Zeroizing::new(Vec::with_capacity(MAX_HOST_VAULT_BYTES));
        let initial_capacity = bytes.capacity();
        if initial_capacity < MAX_HOST_VAULT_BYTES {
            return Err(ConfigError::new(ConfigErrorKind::Serialization));
        }
        Ok(Self {
            bytes,
            initial_capacity,
        })
    }

    fn finish(mut self) -> Result<Zeroizing<Vec<u8>>, ConfigError> {
        if self.bytes.len() > MAX_HOST_VAULT_BYTES.saturating_sub(1)
            || self.bytes.capacity() != self.initial_capacity
        {
            return Err(ConfigError::new(ConfigErrorKind::Serialization));
        }
        self.bytes.push(b'\n');
        if self.bytes.len() > MAX_HOST_VAULT_BYTES || self.bytes.capacity() != self.initial_capacity
        {
            return Err(ConfigError::new(ConfigErrorKind::Serialization));
        }
        Ok(std::mem::take(&mut self.bytes))
    }
}

impl Write for BoundedZeroizingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let next = self
            .bytes
            .len()
            .checked_add(buffer.len())
            .ok_or_else(|| io::Error::other("Host-vault serialization overflow"))?;
        if next > MAX_HOST_VAULT_BYTES - 1 || self.bytes.capacity() != self.initial_capacity {
            return Err(io::Error::other("Host-vault serialization limit exceeded"));
        }
        self.bytes.extend_from_slice(buffer);
        if self.bytes.capacity() != self.initial_capacity {
            return Err(io::Error::other("Host-vault serialization reallocated"));
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
pub(in crate::host_vault) fn serialization_count_for_test() -> usize {
    SERIALIZATION_COUNT.with(Cell::get)
}

#[cfg(test)]
pub(in crate::host_vault) fn serialize_for_test(
    document: &VaultDocument<'_>,
) -> Result<Zeroizing<Vec<u8>>, ConfigError> {
    serialize_document(document)
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::io::Write;

    use super::{BoundedZeroizingWriter, MAX_HOST_VAULT_BYTES, VaultDocument, serialize_document};
    use crate::ConfigErrorKind;

    #[test]
    fn bounded_writer_accepts_exact_json_limit_without_reallocation() {
        let mut output = BoundedZeroizingWriter::new().expect("writer");
        let capacity = output.bytes.capacity();
        output
            .write_all(&vec![b'x'; MAX_HOST_VAULT_BYTES - 1])
            .expect("exact JSON limit");
        assert_eq!(output.bytes.capacity(), capacity);
        let bytes = output.finish().expect("newline");
        assert_eq!(bytes.len(), MAX_HOST_VAULT_BYTES);
        assert_eq!(bytes.capacity(), capacity);
        assert_eq!(bytes.last(), Some(&b'\n'));
    }

    #[test]
    fn bounded_writer_rejects_overflow_without_reallocation() {
        let mut output = BoundedZeroizingWriter::new().expect("writer");
        let capacity = output.bytes.capacity();
        output
            .write_all(&vec![b'x'; MAX_HOST_VAULT_BYTES - 1])
            .expect("exact JSON limit");
        assert!(output.write_all(b"x").is_err());
        assert_eq!(output.bytes.len(), MAX_HOST_VAULT_BYTES - 1);
        assert_eq!(output.bytes.capacity(), capacity);
    }

    #[test]
    fn bounded_writer_accepts_allocator_overcapacity_without_reallocation() {
        let bytes = zeroize::Zeroizing::new(Vec::with_capacity(MAX_HOST_VAULT_BYTES + 64));
        let initial_capacity = bytes.capacity();
        assert!(initial_capacity > MAX_HOST_VAULT_BYTES);
        let mut output = BoundedZeroizingWriter {
            bytes,
            initial_capacity,
        };
        output.write_all(b"{}").expect("JSON");
        let bytes = output.finish().expect("newline");
        assert_eq!(bytes.as_slice(), b"{}\n");
        assert_eq!(bytes.capacity(), initial_capacity);
    }

    #[test]
    fn serialization_overflow_has_the_serialization_error_kind() {
        let oversized_kind = "x".repeat(MAX_HOST_VAULT_BYTES);
        let document = VaultDocument {
            format_version: 1,
            kind: Cow::Borrowed(&oversized_kind),
            revision: 0,
            credentials: Vec::new(),
            grants: Vec::new(),
        };
        let error = match serialize_document(&document) {
            Ok(_) => panic!("oversized serialization succeeded"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), ConfigErrorKind::Serialization);
    }
}
