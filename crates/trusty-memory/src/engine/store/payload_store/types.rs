//! Types, errors, and key-codec helpers for `PayloadStore`.
//!
//! Why: Isolates the error enum, row structs, and key-codec helpers from the
//! store implementation so each file stays under the 500-SLOC cap.
//! What: Defines `PayloadStoreError`, `PayloadRow`, `PayloadRecord`,
//! `Result`, `RowBytes`, and the four key-codec free functions.
//! Test: Covered indirectly by the CRUD and round-trip tests in `mod.rs`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

/// Errors raised by `PayloadStore`.
///
/// Why: Callers may want to fall back gracefully on a missing payload but
/// surface a hard I/O failure — distinguishing the two requires a typed error.
/// What: Wraps the error sources (redb storage, transaction, table, postcard,
/// JSON, migration) so each can be inspected without `downcast`. `NotFound`
/// is a value not an error path — missing rows surface as `Ok(None)` instead.
/// Test: Covered indirectly by the round-trip test and the missing-row test.
//
// Why (boxing): redb's error types (`DatabaseError`, `TransactionError`,
// `TableError`, `StorageError`, `CommitError`) are large enums (the largest
// variant pushes the parent enum past 180 bytes), which trips Clippy's
// `result_large_err` lint at every `Result<_, PayloadStoreError>` return site.
// We box each redb source so the enum stays small (≤ a couple of words per
// variant) while preserving the typed error API. `serde_json::Error` is
// similarly boxy and is boxed for the same reason.
// What: Each variant whose source is a large foreign error owns a
// `Box<Source>`; the `Display` impl deref-prints transparently.
// Test: existing CRUD tests still exercise every variant's construction path
// without behavioural change.
#[derive(Debug, Error)]
pub enum PayloadStoreError {
    #[error("payload store io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("payload store redb database error at {path}: {source}")]
    Database {
        path: PathBuf,
        #[source]
        source: Box<redb::DatabaseError>,
    },
    #[error("payload store redb transaction error at {path}: {source}")]
    Transaction {
        path: PathBuf,
        #[source]
        source: Box<redb::TransactionError>,
    },
    #[error("payload store redb table error at {path}: {source}")]
    Table {
        path: PathBuf,
        #[source]
        source: Box<redb::TableError>,
    },
    #[error("payload store redb storage error at {path}: {source}")]
    Storage {
        path: PathBuf,
        #[source]
        source: Box<redb::StorageError>,
    },
    #[error("payload store redb commit error at {path}: {source}")]
    Commit {
        path: PathBuf,
        #[source]
        source: Box<redb::CommitError>,
    },
    #[error("payload store postcard codec error: {source}")]
    Postcard {
        #[source]
        source: postcard::Error,
    },
    #[error("payload store json error: {source}")]
    Json {
        #[source]
        source: Box<serde_json::Error>,
    },
}

/// Module-local result alias.
pub(super) type Result<T> = std::result::Result<T, PayloadStoreError>;

/// One persisted payload row.
///
/// Why: `load_all` needs a single struct shape so callers can hydrate their
/// in-memory sidecar in one pass.
/// What: Pairs the original caller id, the deterministic uuid the vector store
/// keys by, and the JSON payload.
/// Test: `roundtrip_persists_across_reopen` reads the row back through this
/// type.
#[derive(Debug, Clone, PartialEq)]
pub struct PayloadRow {
    pub segment: String,
    pub id: String,
    pub uuid: Uuid,
    pub payload: Value,
}

/// Postcard-encoded value layout for one PAYLOADS row.
///
/// Why: redb table values are raw byte slices; we postcard-encode this struct
/// so the (uuid, json) pair travels as a single, fixed schema.
/// What: 16-byte uuid + JSON payload string. JSON-as-string lets us avoid the
/// `serde_json::Value` <-> postcard impedance issue while still round-tripping
/// arbitrary user payloads.
/// Test: `roundtrip_persists_across_reopen` covers the codec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PayloadRecord {
    pub(super) uuid: [u8; 16],
    pub(super) payload: String,
}

/// Internal helper: decoded key/value pair for a single PAYLOADS row.
pub(super) struct RowBytes {
    pub(super) id: String,
    pub(super) raw_value: Vec<u8>,
}

/// Internal: callers historically passed `<data_root>/payloads.db` for the
/// SQLite sidecar. Now that the store is redb-backed, accept that same path
/// and silently rewrite it to `payloads.redb` so existing call sites continue
/// to work. Paths with any other extension (or no extension) are kept as-is.
///
/// Why: Backwards compat — existing `TrustyBackedMemoryStore` callers pass
/// `payloads.db`. We rewrite to the redb sibling transparently.
/// What: Returns `path.with_extension("redb")` for `.db` inputs; identity otherwise.
/// Test: `callers_passing_payloads_db_get_redb_sibling`.
pub(super) fn resolve_redb_path(path: &std::path::Path) -> std::path::PathBuf {
    if path.extension().is_some_and(|e| e == "db") {
        path.with_extension("redb")
    } else {
        path.to_path_buf()
    }
}

/// Internal helper: turn a raw `(key, value)` redb pair into a `PayloadRow`.
///
/// Why: `load_all` walks raw redb entries; decoding is the same for both
/// the range and full-scan branches.
/// What: Splits the key into (segment, id) and postcard-decodes the value.
/// Returns `Ok(None)` for malformed keys (defensive skip).
/// Test: `roundtrip_persists_across_reopen`, `load_all_filters_by_segment`.
pub(super) fn decode_row(key: &[u8], value: &[u8]) -> Result<Option<PayloadRow>> {
    let (segment, id) = match split_payload_key_any(key) {
        Some(parts) => parts,
        None => return Ok(None),
    };
    let record: PayloadRecord =
        postcard::from_bytes(value).map_err(|e| PayloadStoreError::Postcard { source: e })?;
    let uuid = Uuid::from_bytes(record.uuid);
    let payload: Value =
        serde_json::from_str(&record.payload).map_err(|e| PayloadStoreError::Json {
            source: Box::new(e),
        })?;
    Ok(Some(PayloadRow {
        segment,
        id,
        uuid,
        payload,
    }))
}

/// Internal: extract the id half of a payload key when the segment is known.
/// Returns `None` if the key is malformed or doesn't start with the expected
/// segment prefix.
///
/// Why: `iter_segment` needs to decode only the id half (segment is already
/// known) without allocating a second String for the segment.
/// What: Validates the segment prefix in the key; returns the trailing id bytes as UTF-8.
/// Test: Covered by CRUD tests that use `iter_segment` internally.
pub(super) fn split_payload_key(key: &[u8], segment: &str) -> Option<String> {
    if key.len() < 2 {
        return None;
    }
    let seg_len = u16::from_be_bytes([key[0], key[1]]) as usize;
    if key.len() < 2 + seg_len {
        return None;
    }
    let seg_bytes = &key[2..2 + seg_len];
    if seg_bytes != segment.as_bytes() {
        return None;
    }
    let id_bytes = &key[2 + seg_len..];
    std::str::from_utf8(id_bytes).ok().map(|s| s.to_string())
}

/// Internal: split a payload key into `(segment, id)` without knowing the
/// segment in advance. Used by `load_all`.
///
/// Why: `load_all` scans all rows and needs both halves of the key.
/// What: Parses the length-prefixed segment then reads the remaining bytes as id.
/// Test: `load_all_filters_by_segment`.
pub(super) fn split_payload_key_any(key: &[u8]) -> Option<(String, String)> {
    if key.len() < 2 {
        return None;
    }
    let seg_len = u16::from_be_bytes([key[0], key[1]]) as usize;
    if key.len() < 2 + seg_len {
        return None;
    }
    let segment = std::str::from_utf8(&key[2..2 + seg_len]).ok()?.to_string();
    let id = std::str::from_utf8(&key[2 + seg_len..]).ok()?.to_string();
    Some((segment, id))
}
