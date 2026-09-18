//! Durable session ledger storage: layout, manifest, and record codec.
//!
//! One session owns `plugins/session/data/sessions/<ses1-id>/`. The strict
//! `manifest.json` (published through the hardened owned-file transaction) is
//! the commit authority; `branches/<br1-id>.events` are append-only logs whose
//! committed prefix length the manifest pins; `pending/<evt1-id>.payload`
//! holds durably staged event payloads until their append commit consumes
//! them. A torn log tail beyond the committed length is discarded during
//! recovery; any structural or digest failure inside the committed prefix is
//! corruption and fails closed.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use mcode_config::HomeLayout;

use super::digest::payload_digest;
use super::dto::{EventKind, HeadStamp, MAX_BRANCHES, MAX_EVENT_PAYLOAD_BYTES};
use super::ids::{BranchId, CALL_ID_LEN, EVENT_ID_LEN, SessionCallId, SessionEventId, SessionId};

/// Session family data root below the owned home.
pub const SESSIONS_RELATIVE_DIR: &str = "plugins/session/data/sessions";
/// Manifest file name inside one session directory.
pub const MANIFEST_FILE: &str = "manifest.json";
/// Branch log directory name inside one session directory.
pub const BRANCHES_DIR: &str = "branches";
/// Staged payload directory name inside one session directory.
pub const PENDING_DIR: &str = "pending";
/// Branch log file suffix.
pub const BRANCH_FILE_SUFFIX: &str = ".events";
/// Staged payload file suffix.
pub const PENDING_FILE_SUFFIX: &str = ".payload";
/// Maximum encoded manifest size: 64 KiB.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Maximum committed bytes across one session's branch logs: 512 MiB.
pub const MAX_SESSION_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
/// Manifest format version.
pub const MANIFEST_FORMAT_VERSION: u32 = 1;
/// Manifest kind tag.
pub const MANIFEST_KIND: &str = "mcode-session-ledger";
/// Durable record codec version byte.
pub const RECORD_VERSION: u8 = 1;
/// Fixed record body bytes before the optional call ID and payload:
/// version, event ID, kind, call flag, payload length.
const RECORD_BODY_FIXED: usize = 1 + EVENT_ID_LEN + 1 + 1 + 8;
/// Raw SHA-256 digest length carried by every record.
const RECORD_DIGEST_BYTES: usize = 32;

/// One branch row inside the manifest file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ManifestBranchFile {
    /// Branch identity spelling.
    pub(crate) branch_id: String,
    /// Parentage row.
    pub(crate) parentage: ParentageFile,
    /// `empty` or one event identity.
    pub(crate) head: String,
    /// Committed event count.
    pub(crate) event_count: u64,
    /// Committed prefix length of the branch log.
    pub(crate) committed_bytes: u64,
}

/// Parentage row: one discriminated shape with exact field presence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ParentageFile {
    /// `root`, `fork`, or `rewind`.
    pub(crate) kind: String,
    /// Present exactly for `fork`/`rewind`.
    pub(crate) source_branch_id: Option<String>,
    /// Present exactly for `fork`.
    pub(crate) at_event_id: Option<String>,
    /// Present exactly for `rewind`.
    pub(crate) to_event_id: Option<String>,
}

/// The manifest document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ManifestFile {
    /// Format version.
    pub(crate) format_version: u32,
    /// Kind tag.
    pub(crate) kind: String,
    /// Session identity spelling.
    pub(crate) session_id: String,
    /// Branch rows.
    pub(crate) branches: Vec<ManifestBranchFile>,
}

/// Reports why manifest bytes failed strict validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ManifestError {
    /// Bytes exceeded the fixed manifest bound.
    Oversized,
    /// Structure, grammar, or cross-field validation failed.
    Invalid,
}

/// Encodes one manifest document canonically.
///
/// # Errors
///
/// Returns [`ManifestError::Oversized`] when the canonical encoding exceeds
/// [`MAX_MANIFEST_BYTES`] and [`ManifestError::Invalid`] on serialization
/// failure.
pub(crate) fn encode_manifest(manifest: &ManifestFile) -> Result<Vec<u8>, ManifestError> {
    let mut bytes = serde_json::to_vec(manifest).map_err(|_| ManifestError::Invalid)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::Oversized);
    }
    Ok(bytes)
}

/// Decodes and fully validates one manifest document.
///
/// # Errors
///
/// Returns [`ManifestError::Oversized`] for oversized input and
/// [`ManifestError::Invalid`] for any structural, grammar, or cross-field
/// failure.
pub(crate) fn decode_manifest(bytes: &[u8]) -> Result<ManifestFile, ManifestError> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::Oversized);
    }
    let manifest: ManifestFile =
        serde_json::from_slice(bytes).map_err(|_| ManifestError::Invalid)?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &ManifestFile) -> Result<(), ManifestError> {
    if manifest.format_version != MANIFEST_FORMAT_VERSION || manifest.kind != MANIFEST_KIND {
        return Err(ManifestError::Invalid);
    }
    if SessionId::parse(&manifest.session_id).is_none()
        || manifest.branches.is_empty()
        || manifest.branches.len() > MAX_BRANCHES
    {
        return Err(ManifestError::Invalid);
    }
    let mut total = 0_u64;
    for (index, branch) in manifest.branches.iter().enumerate() {
        if BranchId::parse(&branch.branch_id).is_none()
            || manifest.branches[..index]
                .iter()
                .any(|other| other.branch_id == branch.branch_id)
        {
            return Err(ManifestError::Invalid);
        }
        validate_parentage(&branch.parentage)?;
        decode_head(&branch.head).ok_or(ManifestError::Invalid)?;
        total = total
            .checked_add(branch.committed_bytes)
            .ok_or(ManifestError::Invalid)?;
        if total > MAX_SESSION_TOTAL_BYTES {
            return Err(ManifestError::Invalid);
        }
    }
    Ok(())
}

fn validate_parentage(parentage: &ParentageFile) -> Result<(), ManifestError> {
    let valid_source =
        |value: &Option<String>| value.as_deref().and_then(BranchId::parse).is_some();
    let valid_event =
        |value: &Option<String>| value.as_deref().and_then(SessionEventId::parse).is_some();
    match parentage.kind.as_str() {
        "root" => {
            if parentage.source_branch_id.is_some()
                || parentage.at_event_id.is_some()
                || parentage.to_event_id.is_some()
            {
                return Err(ManifestError::Invalid);
            }
        }
        "fork" => {
            if !valid_source(&parentage.source_branch_id)
                || !valid_event(&parentage.at_event_id)
                || parentage.to_event_id.is_some()
            {
                return Err(ManifestError::Invalid);
            }
        }
        "rewind" => {
            if !valid_source(&parentage.source_branch_id)
                || parentage.at_event_id.is_some()
                || !valid_event(&parentage.to_event_id)
            {
                return Err(ManifestError::Invalid);
            }
        }
        _ => return Err(ManifestError::Invalid),
    }
    Ok(())
}

/// Encodes a head stamp as its manifest spelling.
#[must_use]
pub(crate) fn encode_head(head: &HeadStamp) -> String {
    match head {
        HeadStamp::Empty => "empty".to_owned(),
        HeadStamp::Event(event) => event.as_str().to_owned(),
    }
}

/// Decodes a head stamp from its manifest spelling.
#[must_use]
pub(crate) fn decode_head(value: &str) -> Option<HeadStamp> {
    if value == "empty" {
        return Some(HeadStamp::Empty);
    }
    SessionEventId::parse(value).map(HeadStamp::Event)
}

/// One decoded durable record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DecodedRecord {
    /// Event identity spelling.
    pub(crate) event_id: String,
    /// Wire kind tag.
    pub(crate) kind: u8,
    /// Call identity spelling, present only for tool kinds.
    pub(crate) call_id: Option<String>,
    /// Payload byte length.
    pub(crate) payload_len: u64,
    /// Byte offset of the payload inside the decoded buffer.
    pub(crate) payload_offset: usize,
    /// Raw SHA-256 digest of the payload.
    pub(crate) payload_digest: [u8; 32],
    /// Total encoded record length including the frame length prefix.
    pub(crate) record_len: u64,
}

/// Reports why record bytes failed decoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecordError {
    /// The frame extends beyond the provided buffer (a torn tail).
    Incomplete,
    /// Structure, grammar, bounds, or payload digest failed.
    Corrupt,
}

/// Encodes one durable event record.
///
/// # Panics
///
/// Panics only when a caller bypasses the DTO bounds and supplies a payload
/// whose length does not fit the checked frame header; every actor path
/// validates bounds before encoding.
#[must_use]
pub(crate) fn encode_record(
    event_id: &SessionEventId,
    kind: EventKind,
    call_id: Option<&SessionCallId>,
    payload: &[u8],
) -> Vec<u8> {
    let body_len = RECORD_BODY_FIXED
        + call_id.map_or(0, |_| CALL_ID_LEN)
        + payload.len()
        + RECORD_DIGEST_BYTES;
    let frame_len = u32::try_from(body_len).expect("payload bound keeps the frame length in u32");
    let mut record = Vec::with_capacity(4 + body_len);
    record.extend_from_slice(&frame_len.to_be_bytes());
    record.push(RECORD_VERSION);
    record.extend_from_slice(event_id.as_str().as_bytes());
    record.push(kind.tag());
    match call_id {
        Some(call) => {
            record.push(1);
            record.extend_from_slice(call.as_str().as_bytes());
        }
        None => record.push(0),
    }
    record.extend_from_slice(
        &u64::try_from(payload.len())
            .expect("payload bound keeps the payload length in u64")
            .to_be_bytes(),
    );
    record.extend_from_slice(payload);
    record.extend_from_slice(&payload_digest(payload));
    record
}

/// Decodes one durable event record from the front of `buffer`.
///
/// # Errors
///
/// Returns [`RecordError::Incomplete`] when the frame extends beyond the
/// buffer and [`RecordError::Corrupt`] for structural, grammar, bounds, or
/// digest failures.
pub(crate) fn decode_record(buffer: &[u8]) -> Result<DecodedRecord, RecordError> {
    let Some(header) = buffer.get(..4) else {
        return Err(RecordError::Incomplete);
    };
    let body_len = u32::from_be_bytes(header.try_into().expect("four header bytes")) as usize;
    let Some(body) = buffer.get(4..4_usize.checked_add(body_len).ok_or(RecordError::Corrupt)?)
    else {
        return Err(RecordError::Incomplete);
    };

    let mut cursor = 0_usize;

    if slice_at(body, &mut cursor, 1)?[0] != RECORD_VERSION {
        return Err(RecordError::Corrupt);
    }
    let event_id = std::str::from_utf8(slice_at(body, &mut cursor, EVENT_ID_LEN)?)
        .map_err(|_| RecordError::Corrupt)?
        .to_owned();
    if SessionEventId::parse(&event_id).is_none() {
        return Err(RecordError::Corrupt);
    }
    let kind = slice_at(body, &mut cursor, 1)?[0];
    if EventKind::from_tag(kind).is_none() {
        return Err(RecordError::Corrupt);
    }
    let call_id = match slice_at(body, &mut cursor, 1)?[0] {
        0 => None,
        1 => {
            let call = std::str::from_utf8(slice_at(body, &mut cursor, CALL_ID_LEN)?)
                .map_err(|_| RecordError::Corrupt)?
                .to_owned();
            if SessionCallId::parse(&call).is_none() {
                return Err(RecordError::Corrupt);
            }
            Some(call)
        }
        _ => return Err(RecordError::Corrupt),
    };
    let payload_len = u64::from_be_bytes(
        slice_at(body, &mut cursor, 8)?
            .try_into()
            .expect("eight length bytes"),
    );
    if payload_len == 0 || payload_len as usize > MAX_EVENT_PAYLOAD_BYTES {
        return Err(RecordError::Corrupt);
    }
    let payload_len = usize::try_from(payload_len).map_err(|_| RecordError::Corrupt)?;
    let payload_offset = cursor;
    slice_at(body, &mut cursor, payload_len)?;
    let mut digest = [0_u8; RECORD_DIGEST_BYTES];
    digest.copy_from_slice(slice_at(body, &mut cursor, RECORD_DIGEST_BYTES)?);
    if payload_digest(&body[payload_offset..payload_offset + payload_len]) != digest {
        return Err(RecordError::Corrupt);
    }
    if cursor != body_len {
        return Err(RecordError::Corrupt);
    }
    let record_len = u64::try_from(4 + body_len).map_err(|_| RecordError::Corrupt)?;
    Ok(DecodedRecord {
        event_id,
        kind,
        call_id,
        payload_len: u64::try_from(payload_len).map_err(|_| RecordError::Corrupt)?,
        payload_offset: payload_offset + 4,
        payload_digest: digest,
        record_len,
    })
}

/// Slices `count` bytes at `cursor`, advancing it on success.
fn slice_at<'b>(body: &'b [u8], cursor: &mut usize, count: usize) -> Result<&'b [u8], RecordError> {
    let end = (*cursor).checked_add(count).ok_or(RecordError::Corrupt)?;
    let slice = body.get(*cursor..end).ok_or(RecordError::Corrupt)?;
    *cursor = end;
    Ok(slice)
}

/// Constructs validated owned paths below one session directory.
pub(crate) struct SessionPaths<'a> {
    home: &'a HomeLayout,
    session: &'a SessionId,
}

impl<'a> SessionPaths<'a> {
    /// Binds one session's path namespace.
    pub(crate) const fn new(home: &'a HomeLayout, session: &'a SessionId) -> Self {
        Self { home, session }
    }

    fn relative(&self, tail: &str) -> String {
        format!("{SESSIONS_RELATIVE_DIR}/{}{tail}", self.session.as_str())
    }

    /// Relative owned path of the session manifest.
    pub(crate) fn manifest(&self) -> String {
        self.relative(&format!("/{MANIFEST_FILE}"))
    }

    /// Relative owned path of the branch log directory.
    pub(crate) fn branches_dir(&self) -> String {
        self.relative(&format!("/{BRANCHES_DIR}"))
    }

    /// Relative owned path of one branch log.
    pub(crate) fn branch_events(&self, branch: &BranchId) -> String {
        format!(
            "{}/{}{BRANCH_FILE_SUFFIX}",
            self.branches_dir(),
            branch.as_str()
        )
    }

    /// Relative owned path of the staged payload directory.
    pub(crate) fn pending_dir(&self) -> String {
        self.relative(&format!("/{PENDING_DIR}"))
    }

    /// Relative owned path of one staged payload.
    pub(crate) fn pending_payload(&self, event: &SessionEventId) -> String {
        format!(
            "{}/{}{PENDING_FILE_SUFFIX}",
            self.pending_dir(),
            event.as_str()
        )
    }

    /// Absolute path for one relative owned path.
    ///
    /// # Errors
    ///
    /// Returns the mcode-config path-escape error when a component is unsafe.
    pub(crate) fn absolute(&self, relative: &str) -> Result<PathBuf, mcode_config::ConfigError> {
        self.home.owned_join(relative)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::super::dto::{EventKind, HeadStamp};
    use super::super::ids::SessionCallId;
    use super::super::ids::SessionEventId;
    use super::{
        BranchId, MAX_MANIFEST_BYTES, ManifestBranchFile, ManifestError, ManifestFile,
        ParentageFile, RecordError, decode_head, decode_manifest, decode_record, encode_head,
        encode_manifest, encode_record,
    };

    fn event(suffix: char) -> SessionEventId {
        SessionEventId::parse(&format!("evt1-0123456789abcdef0123456789abcde{suffix}"))
            .expect("event id")
    }

    fn root_manifest() -> ManifestFile {
        ManifestFile {
            format_version: 1,
            kind: "mcode-session-ledger".to_owned(),
            session_id: "ses1-0123456789abcdef0123456789abcdef".to_owned(),
            branches: vec![ManifestBranchFile {
                branch_id: "br1-0123456789abcdef0123456789abcdef".to_owned(),
                parentage: ParentageFile {
                    kind: "root".to_owned(),
                    source_branch_id: None,
                    at_event_id: None,
                    to_event_id: None,
                },
                head: "empty".to_owned(),
                event_count: 0,
                committed_bytes: 0,
            }],
        }
    }

    #[test]
    fn manifest_roundtrip_and_strict_rejection() {
        let manifest = root_manifest();
        let bytes = encode_manifest(&manifest).expect("encode");
        assert_eq!(decode_manifest(&bytes).expect("decode"), manifest);

        for (label, json, extra_tail) in [
            (
                "future version",
                set_root(&manifest, "formatVersion", "2"),
                None,
            ),
            ("wrong kind", set_root(&manifest, "kind", "other"), None),
            (
                "bad session",
                set_root(&manifest, "sessionId", "ses1-nothex"),
                None,
            ),
            ("bad head", set_branch(&manifest, "head", "evt1-bad"), None),
            (
                "bad branch",
                set_branch(&manifest, "branchId", "br1-bad"),
                None,
            ),
            ("unknown extra", extra_root(&manifest), None),
            ("trailing garbage", to_value(&manifest), Some(b"extra")),
        ] {
            let mut bytes = serde_json::to_vec(&json).expect("bytes");
            if let Some(tail) = extra_tail {
                bytes.extend_from_slice(tail);
            }
            bytes.push(b'\n');
            assert_eq!(
                decode_manifest(&bytes).err(),
                Some(ManifestError::Invalid),
                "{label}"
            );
        }
        for label in ["formatVersion", "kind", "sessionId", "branches"] {
            let mut json = to_value(&manifest);
            json.as_object_mut().expect("root").remove(label);
            let bytes = serde_json::to_vec(&json).expect("bytes");
            assert_eq!(
                decode_manifest(&bytes).err(),
                Some(ManifestError::Invalid),
                "missing {label}"
            );
        }
        assert_eq!(
            decode_manifest(&vec![b'x'; MAX_MANIFEST_BYTES + 1]).err(),
            Some(ManifestError::Oversized)
        );
    }

    fn to_value(manifest: &ManifestFile) -> Value {
        serde_json::to_value(manifest).expect("value")
    }

    fn set_root(manifest: &ManifestFile, key: &str, value: &str) -> Value {
        let mut json = to_value(manifest);
        json[key] = Value::String(value.to_owned());
        json
    }

    fn set_branch(manifest: &ManifestFile, key: &str, value: &str) -> Value {
        let mut json = to_value(manifest);
        json["branches"][0][key] = Value::String(value.to_owned());
        json
    }

    fn extra_root(manifest: &ManifestFile) -> Value {
        let mut json = to_value(manifest);
        json.as_object_mut()
            .expect("root")
            .insert("extra".into(), Value::Bool(true));
        json
    }

    #[test]
    fn parentage_kinds_require_exact_field_presence() {
        let source = "br1-0123456789abcdef0123456789abcdef";
        let mut manifest = root_manifest();
        manifest.branches[0].parentage = ParentageFile {
            kind: "fork".to_owned(),
            source_branch_id: Some(source.to_owned()),
            at_event_id: Some(event('a').as_str().to_owned()),
            to_event_id: None,
        };
        assert!(decode_manifest(&encode_manifest(&manifest).expect("encode")).is_ok());

        manifest.branches[0].parentage.to_event_id = Some(event('b').as_str().to_owned());
        assert_eq!(
            decode_manifest(&encode_manifest(&manifest).expect("encode")).err(),
            Some(ManifestError::Invalid),
            "fork must not carry to-event"
        );

        manifest.branches[0].parentage = ParentageFile {
            kind: "rewind".to_owned(),
            source_branch_id: Some(source.to_owned()),
            at_event_id: None,
            to_event_id: Some(event('b').as_str().to_owned()),
        };
        assert!(decode_manifest(&encode_manifest(&manifest).expect("encode")).is_ok());

        manifest.branches[0].parentage.kind = "unknown".to_owned();
        assert_eq!(
            decode_manifest(&encode_manifest(&manifest).expect("encode")).err(),
            Some(ManifestError::Invalid)
        );
        BranchId::parse(source).expect("source grammar");
    }

    #[test]
    fn head_spelling_is_exact() {
        assert_eq!(encode_head(&HeadStamp::Empty), "empty");
        assert_eq!(decode_head("empty"), Some(HeadStamp::Empty));
        let event = event('a');
        assert_eq!(
            encode_head(&HeadStamp::Event(event.clone())),
            event.as_str()
        );
        assert_eq!(
            decode_head(event.as_str()),
            Some(HeadStamp::Event(event.clone()))
        );
        assert_eq!(decode_head("Empty"), None);
        assert_eq!(decode_head("evt1-short"), None);
    }

    #[test]
    fn record_roundtrip_and_torn_tail_classification() {
        let call = SessionCallId::parse("call1-0123456789abcdef0123456789abcde1").expect("call id");
        for kind in [
            EventKind::Message,
            EventKind::ToolCall,
            EventKind::ToolResult,
            EventKind::Usage,
        ] {
            let call_id =
                matches!(kind, EventKind::ToolCall | EventKind::ToolResult).then(|| call.clone());
            let record = encode_record(&event('f'), kind, call_id.as_ref(), b"payload bytes");
            let decoded = decode_record(&record).expect("decode");
            assert_eq!(decoded.event_id, event('f').as_str());
            assert_eq!(decoded.kind, kind.tag());
            assert_eq!(
                decoded.call_id.as_deref(),
                call_id.as_ref().map(|call| call.as_str())
            );
            assert_eq!(decoded.payload_len as usize, b"payload bytes".len());
            assert_eq!(decoded.record_len as usize, record.len());

            assert_eq!(
                decode_record(&record[..record.len() - 1]).err(),
                Some(RecordError::Incomplete),
                "truncated frame is tearing"
            );

            let mut corrupt = record.clone();
            let last = corrupt.len() - 1;
            corrupt[last] ^= 0xff;
            assert_eq!(
                decode_record(&corrupt).err(),
                Some(RecordError::Corrupt),
                "digest flip is corruption, not tearing"
            );
        }
    }

    #[test]
    fn record_rejects_structural_mutation() {
        let record = encode_record(&event('1'), EventKind::Message, None, b"payload");

        let mut bad_version = record.clone();
        bad_version[4] = 2;
        assert_eq!(
            decode_record(&bad_version).err(),
            Some(RecordError::Corrupt)
        );

        let mut bad_kind = record.clone();
        bad_kind[4 + 1 + super::super::ids::EVENT_ID_LEN] = 9;
        assert_eq!(decode_record(&bad_kind).err(), Some(RecordError::Corrupt));

        let mut bad_flag = record.clone();
        let flag_index = 4 + 1 + super::super::ids::EVENT_ID_LEN + 1;
        bad_flag[flag_index] = 7;
        assert_eq!(decode_record(&bad_flag).err(), Some(RecordError::Corrupt));

        let mut bad_event = record.clone();
        bad_event[5] = b'X';
        assert_eq!(decode_record(&bad_event).err(), Some(RecordError::Corrupt));

        assert_eq!(
            decode_record(&record[..3]).err(),
            Some(RecordError::Incomplete)
        );
    }
}
