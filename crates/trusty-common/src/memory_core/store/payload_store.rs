//! redb-backed payload sidecar for external integrations.
//!
//! Why: `TrustyBackedMemoryStore` in open-mpm maps caller-supplied string ids
//! onto trusty's `Uuid` keyspace and attaches an arbitrary JSON payload to each
//! entry. The vector data already persists to the usearch index on disk, but
//! the string-id ↔ uuid ↔ JSON mapping was process-local — losing it on
//! restart blocked switching `TrustyBackedMemoryStore` to the production
//! default (issue #52). This module provides the missing durable sidecar so
//! payloads survive a process restart without forcing every embedding adapter
//! to roll its own storage layer.
//!
//! Issue #46 migrates this store from rusqlite to redb so the payload sidecar
//! drops the heavy native dependency chain (rusqlite + r2d2 + r2d2_sqlite) and
//! lines up with the rest of the Memory Palace (`kg_redb.rs`, palace_store).
//! The public `PayloadStore` API is unchanged so `TrustyBackedMemoryStore`
//! continues to work as a drop-in.
//!
//! What: `PayloadStore` opens a single redb database at a caller-supplied path
//! and exposes `upsert` / `get` / `delete` / `exists` / `list_segment` /
//! `lookup_id_for_uuid` / `load_all` over the `PAYLOADS` table defined in
//! `kg_store.rs`. The composite key is `[segment_len][segment][id]` (see
//! `encode_payload_key`); the value is a postcard-encoded `PayloadRecord`
//! that bundles the 16-byte uuid with the JSON payload string.
//!
//! Rows are partitioned by `segment` so a single store can back multiple
//! namespaces (open-mpm's `Segment::AgentMemory`, `CodeIndex`, etc.). Errors
//! flow through the typed `PayloadStoreError` so callers can distinguish I/O
//! from JSON from schema problems.
//!
//! Test: This module's `tests` exercise the full CRUD path plus a reopen
//! round-trip (the load-all method must return every row written by a prior
//! process), and — when the `sqlite-kg` feature is enabled — the one-shot
//! migration from the legacy `payloads.db` sidecar.

use redb::{Database, ReadableDatabase, ReadableTable};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

use crate::memory_core::store::kg_store::{PAYLOADS, encode_payload_key, segment_prefix};

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
    #[error("payload store migration error at {path}: {message}")]
    Migration { path: PathBuf, message: String },
}

type Result<T> = std::result::Result<T, PayloadStoreError>;

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
struct PayloadRecord {
    uuid: [u8; 16],
    payload: String,
}

/// redb-backed sidecar for external string-id ↔ uuid ↔ JSON mappings.
///
/// Why: Provides the durable half of `TrustyBackedMemoryStore`'s in-memory
/// hashmap so adapter restarts don't lose payload data. As of #46 this is the
/// redb backend; the SQLite implementation it replaces is now only consulted
/// when the `sqlite-kg` feature is enabled, exclusively for the one-shot
/// migration step on first open.
/// What: Owns an `Arc<redb::Database>` over a single `payloads.redb` file. All
/// reads run in `begin_read` transactions; writes serialize through
/// `begin_write` since the PAYLOADS table is the only thing this store
/// touches. Methods are synchronous — call sites are already off the
/// request-critical path (they wrap their own async vector ops).
/// Test: `roundtrip_persists_across_reopen`, `get_missing_returns_none`,
/// `delete_drops_row`, `exists_reports_membership`,
/// `lookup_id_for_uuid_round_trips`, `load_all_filters_by_segment`,
/// `list_segment_returns_rows`.
pub struct PayloadStore {
    db: Arc<Database>,
    path: PathBuf,
}

impl PayloadStore {
    /// Open or create the redb-backed payload store at `path`.
    ///
    /// Why: Single entry point so callers don't have to know about the redb
    /// schema or the one-shot migration from the legacy `payloads.db` SQLite
    /// sidecar. Callers historically passed `<data_root>/payloads.db`; we
    /// rewrite that to a `payloads.redb` sibling and (when the `sqlite-kg`
    /// feature is enabled) copy any legacy rows over before returning.
    /// What:
    /// 1. Resolves the redb path. Callers that still pass `payloads.db` get
    ///    `payloads.redb` next to it; other extensions are kept as-is.
    /// 2. Creates parent directories if missing.
    /// 3. Opens (or creates) the redb database and touches the PAYLOADS table
    ///    in a write transaction so range scans on a fresh file succeed.
    /// 4. Runs the one-shot SQLite → redb migration when the `sqlite-kg`
    ///    feature is enabled and a `payloads.db` is present.
    ///
    /// Test: `roundtrip_persists_across_reopen` opens the same path twice;
    /// `migrates_legacy_sqlite_rows` (gated on `sqlite-kg`) exercises the
    /// one-shot copy.
    pub fn open(path: &Path) -> Result<Self> {
        let redb_path = resolve_redb_path(path);

        if let Some(parent) = redb_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| PayloadStoreError::Io {
                path: redb_path.clone(),
                source: e,
            })?;
        }

        // One-shot migration must run *before* we open the redb db, because the
        // migrator opens redb itself to write rows; running it on an already-
        // open handle would deadlock on the file lock.
        #[cfg(feature = "sqlite-kg")]
        migrate_from_sqlite_if_present(path, &redb_path)?;

        let db = super::open_or_recreate(&redb_path).map_err(|e| PayloadStoreError::Database {
            path: redb_path.clone(),
            source: Box::new(e),
        })?;

        // Touch the PAYLOADS table so it exists on disk before the first read
        // transaction. redb only persists a table once it is opened in a write
        // transaction; doing it here keeps later read transactions on a brand-
        // new file from failing.
        {
            let wtx = db
                .begin_write()
                .map_err(|e| PayloadStoreError::Transaction {
                    path: redb_path.clone(),
                    source: Box::new(e),
                })?;
            {
                let _ = wtx
                    .open_table(PAYLOADS)
                    .map_err(|e| PayloadStoreError::Table {
                        path: redb_path.clone(),
                        source: Box::new(e),
                    })?;
            }
            wtx.commit().map_err(|e| PayloadStoreError::Commit {
                path: redb_path.clone(),
                source: Box::new(e),
            })?;
        }

        Ok(Self {
            db: Arc::new(db),
            path: redb_path,
        })
    }

    /// Insert or replace the row at `(segment, id)`.
    ///
    /// Why: Adapters write payloads on every `insert` call; idempotent upsert
    /// matches the trait semantics and lets retries be safe.
    /// What: Encodes the composite key, postcard-encodes the `(uuid, payload)`
    /// pair, and writes through a single write transaction. The payload is
    /// stored as a JSON string so the in-disk format is independent of
    /// `serde_json::Value`'s internal representation.
    /// Test: `roundtrip_persists_across_reopen`.
    pub fn upsert(&self, segment: &str, id: &str, uuid: Uuid, payload: &Value) -> Result<()> {
        let payload_json = serde_json::to_string(payload).map_err(|e| PayloadStoreError::Json {
            source: Box::new(e),
        })?;
        let record = PayloadRecord {
            uuid: *uuid.as_bytes(),
            payload: payload_json,
        };
        let value_bytes = postcard::to_allocvec(&record)
            .map_err(|e| PayloadStoreError::Postcard { source: e })?;
        let key = encode_payload_key(segment, id.as_bytes());

        let wtx = self
            .db
            .begin_write()
            .map_err(|e| PayloadStoreError::Transaction {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        {
            let mut table = wtx
                .open_table(PAYLOADS)
                .map_err(|e| PayloadStoreError::Table {
                    path: self.path.clone(),
                    source: Box::new(e),
                })?;
            table
                .insert(key.as_slice(), value_bytes.as_slice())
                .map_err(|e| PayloadStoreError::Storage {
                    path: self.path.clone(),
                    source: Box::new(e),
                })?;
        }
        wtx.commit().map_err(|e| PayloadStoreError::Commit {
            path: self.path.clone(),
            source: Box::new(e),
        })?;
        Ok(())
    }

    /// Fetch the payload for `(segment, id)`, if any.
    ///
    /// Why: `MemoryStore::get` expects `Ok(None)` on missing rows; a typed
    /// `Option` keeps callers from having to inspect error variants.
    /// What: Decodes the postcard record and reparses the JSON payload.
    /// Returns `Ok(None)` on miss.
    /// Test: `get_missing_returns_none`, `roundtrip_persists_across_reopen`.
    pub fn get(&self, segment: &str, id: &str) -> Result<Option<(Uuid, Value)>> {
        let key = encode_payload_key(segment, id.as_bytes());
        let rtx = self
            .db
            .begin_read()
            .map_err(|e| PayloadStoreError::Transaction {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        let table = rtx
            .open_table(PAYLOADS)
            .map_err(|e| PayloadStoreError::Table {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        let raw = table
            .get(key.as_slice())
            .map_err(|e| PayloadStoreError::Storage {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        match raw {
            Some(g) => {
                let record: PayloadRecord = postcard::from_bytes(g.value())
                    .map_err(|e| PayloadStoreError::Postcard { source: e })?;
                let uuid = Uuid::from_bytes(record.uuid);
                let value: Value =
                    serde_json::from_str(&record.payload).map_err(|e| PayloadStoreError::Json {
                        source: Box::new(e),
                    })?;
                Ok(Some((uuid, value)))
            }
            None => Ok(None),
        }
    }

    /// Check whether `(segment, id)` exists, without decoding the payload.
    ///
    /// Why: Callers that only need a presence test (e.g. dedup guards) should
    /// not pay the postcard + JSON decode cost.
    /// What: Reads the PAYLOADS row by key and returns whether it is present.
    /// Test: `exists_reports_membership`.
    pub fn exists(&self, segment: &str, id: &str) -> Result<bool> {
        let key = encode_payload_key(segment, id.as_bytes());
        let rtx = self
            .db
            .begin_read()
            .map_err(|e| PayloadStoreError::Transaction {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        let table = rtx
            .open_table(PAYLOADS)
            .map_err(|e| PayloadStoreError::Table {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        let got = table
            .get(key.as_slice())
            .map_err(|e| PayloadStoreError::Storage {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        Ok(got.is_some())
    }

    /// Reverse-lookup the caller id for a uuid (used to translate vector hits
    /// back to the application-visible id).
    ///
    /// Why: `search` returns uuids from the vector store; the adapter needs to
    /// map each hit back to the original string id without scanning the whole
    /// table.
    /// What: Range-scans the segment, decodes each row, and returns the id of
    /// the first row whose uuid matches. The PAYLOADS table is keyed by
    /// `(segment, id)` — there is no secondary index — but the scan is
    /// bounded by the segment prefix so it stays O(rows in segment) rather
    /// than O(total rows).
    /// Test: `lookup_id_for_uuid_round_trips`.
    pub fn lookup_id_for_uuid(&self, segment: &str, uuid: Uuid) -> Result<Option<String>> {
        let target = *uuid.as_bytes();
        for row in self.iter_segment(segment)? {
            let row = row?;
            let record_bytes = row.raw_value;
            let record: PayloadRecord = postcard::from_bytes(&record_bytes)
                .map_err(|e| PayloadStoreError::Postcard { source: e })?;
            if record.uuid == target {
                return Ok(Some(row.id));
            }
        }
        Ok(None)
    }

    /// Delete the row at `(segment, id)`. No-op if the row does not exist.
    ///
    /// Why: Mirrors `MemoryStore::delete` which is also idempotent.
    /// What: Removes the key from the PAYLOADS table in a write transaction.
    /// Test: `delete_drops_row`.
    pub fn delete(&self, segment: &str, id: &str) -> Result<()> {
        let key = encode_payload_key(segment, id.as_bytes());
        let wtx = self
            .db
            .begin_write()
            .map_err(|e| PayloadStoreError::Transaction {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        {
            let mut table = wtx
                .open_table(PAYLOADS)
                .map_err(|e| PayloadStoreError::Table {
                    path: self.path.clone(),
                    source: Box::new(e),
                })?;
            table
                .remove(key.as_slice())
                .map_err(|e| PayloadStoreError::Storage {
                    path: self.path.clone(),
                    source: Box::new(e),
                })?;
        }
        wtx.commit().map_err(|e| PayloadStoreError::Commit {
            path: self.path.clone(),
            source: Box::new(e),
        })?;
        Ok(())
    }

    /// List every row in `segment` as `(id, uuid, payload_json_string)`.
    ///
    /// Why: New callers (and the #46 issue spec) want a lighter shape than
    /// `PayloadRow` when they only need the id/uuid/json-string triple — for
    /// example, the data-dump utilities that don't want to reparse JSON.
    /// What: Range-scans the segment prefix and decodes each row into
    /// `(id, uuid, payload)`. Rows whose key prefix doesn't survive the
    /// length-prefix bounds check are skipped defensively.
    /// Test: `list_segment_returns_rows`.
    pub fn list_segment(&self, segment: &str) -> Result<Vec<(String, Uuid, String)>> {
        let mut out = Vec::new();
        for row in self.iter_segment(segment)? {
            let row = row?;
            let record: PayloadRecord = postcard::from_bytes(&row.raw_value)
                .map_err(|e| PayloadStoreError::Postcard { source: e })?;
            out.push((row.id, Uuid::from_bytes(record.uuid), record.payload));
        }
        Ok(out)
    }

    /// Load every row, optionally restricted to `segment_filter`.
    ///
    /// Why: On startup, adapters rebuild their in-memory sidecar in one pass;
    /// `load_all` lets them do that without iterating per-id.
    /// What: Returns all rows when `segment_filter` is `None`, or just rows
    /// matching the filter otherwise. The decoded `payload` field is a
    /// `serde_json::Value` so it slots straight into callers'
    /// `HashMap<String, Value>` sidecars.
    /// Test: `load_all_filters_by_segment` and `roundtrip_persists_across_reopen`.
    pub fn load_all(&self, segment_filter: Option<&str>) -> Result<Vec<PayloadRow>> {
        let rtx = self
            .db
            .begin_read()
            .map_err(|e| PayloadStoreError::Transaction {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        let table = rtx
            .open_table(PAYLOADS)
            .map_err(|e| PayloadStoreError::Table {
                path: self.path.clone(),
                source: Box::new(e),
            })?;

        let mut out = Vec::new();
        let iter = if let Some(seg) = segment_filter {
            let prefix = segment_prefix(seg);
            let mut end = prefix.clone();
            end.push(0xFF);
            Some(
                table
                    .range::<&[u8]>(prefix.as_slice()..end.as_slice())
                    .map_err(|e| PayloadStoreError::Storage {
                        path: self.path.clone(),
                        source: Box::new(e),
                    })?,
            )
        } else {
            None
        };

        // We need to walk either a bounded range (when filtered) or the whole
        // table (when not). redb's range and iter return different concrete
        // types, so handle each branch independently rather than trying to
        // unify them through a trait object.
        match iter {
            Some(range) => {
                for entry in range {
                    let (k, v) = entry.map_err(|e| PayloadStoreError::Storage {
                        path: self.path.clone(),
                        source: Box::new(e),
                    })?;
                    if let Some(row) = decode_row(k.value(), v.value())? {
                        out.push(row);
                    }
                }
            }
            None => {
                for entry in table.iter().map_err(|e| PayloadStoreError::Storage {
                    path: self.path.clone(),
                    source: Box::new(e),
                })? {
                    let (k, v) = entry.map_err(|e| PayloadStoreError::Storage {
                        path: self.path.clone(),
                        source: Box::new(e),
                    })?;
                    if let Some(row) = decode_row(k.value(), v.value())? {
                        out.push(row);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Internal: walk every row in `segment`, yielding `RowBytes` so callers
    /// can decode value bytes once per row in whichever shape they need.
    fn iter_segment(&self, segment: &str) -> Result<impl Iterator<Item = Result<RowBytes>> + '_> {
        let rtx = self
            .db
            .begin_read()
            .map_err(|e| PayloadStoreError::Transaction {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        let table = rtx
            .open_table(PAYLOADS)
            .map_err(|e| PayloadStoreError::Table {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        let prefix = segment_prefix(segment);
        let mut end = prefix.clone();
        end.push(0xFF);

        // Collect into an owned Vec so we don't have to thread the redb
        // transaction lifetime through the iterator type. The PAYLOADS table
        // is small (per-segment row counts are bounded by application sizing)
        // so this is acceptable.
        let mut rows: Vec<Result<RowBytes>> = Vec::new();
        let seg_owned = segment.to_string();
        let range = table
            .range::<&[u8]>(prefix.as_slice()..end.as_slice())
            .map_err(|e| PayloadStoreError::Storage {
                path: self.path.clone(),
                source: Box::new(e),
            })?;
        for entry in range {
            match entry {
                Ok((k, v)) => match split_payload_key(k.value(), &seg_owned) {
                    Some(id) => rows.push(Ok(RowBytes {
                        id,
                        raw_value: v.value().to_vec(),
                    })),
                    None => continue,
                },
                Err(e) => rows.push(Err(PayloadStoreError::Storage {
                    path: self.path.clone(),
                    source: Box::new(e),
                })),
            }
        }
        Ok(rows.into_iter())
    }
}

/// Internal helper: decoded key/value pair for a single PAYLOADS row.
struct RowBytes {
    id: String,
    raw_value: Vec<u8>,
}

/// Internal helper: turn a raw `(key, value)` redb pair into a `PayloadRow`.
fn decode_row(key: &[u8], value: &[u8]) -> Result<Option<PayloadRow>> {
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
fn split_payload_key(key: &[u8], segment: &str) -> Option<String> {
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
fn split_payload_key_any(key: &[u8]) -> Option<(String, String)> {
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

/// Internal: callers historically passed `<data_root>/payloads.db` for the
/// SQLite sidecar. Now that the store is redb-backed, accept that same path
/// and silently rewrite it to `payloads.redb` so existing call sites continue
/// to work. Paths with any other extension (or no extension) are kept as-is.
fn resolve_redb_path(path: &Path) -> PathBuf {
    if path.extension().is_some_and(|e| e == "db") {
        path.with_extension("redb")
    } else {
        path.to_path_buf()
    }
}

/// One-shot migration from a legacy SQLite `payloads.db` sidecar.
///
/// Why: Issue #46 — existing deployments have a `payloads.db` populated by the
/// pre-redb store. We copy every row across on the first redb open, then
/// rename the legacy file so the next start is a no-op.
/// What: Opens the SQLite file read-only, dumps `(segment, id, uuid, payload)`
/// rows, writes them into the redb PAYLOADS table inside a single write txn,
/// then renames `payloads.db` → `payloads.db.migrated`. No-op if the SQLite
/// file is absent.
/// Test: `migrates_legacy_sqlite_rows` (gated on the `sqlite-kg` feature).
#[cfg(feature = "sqlite-kg")]
fn migrate_from_sqlite_if_present(orig_path: &Path, redb_path: &Path) -> Result<()> {
    // The legacy SQLite file is whatever the caller originally pointed at
    // (typically `<data_root>/payloads.db`). If that file is missing, nothing
    // to do.
    let sqlite_path = if orig_path.extension().is_some_and(|e| e == "db") {
        orig_path.to_path_buf()
    } else {
        // Caller passed the redb path directly — look for a sibling
        // `payloads.db` to migrate.
        let parent = redb_path.parent().unwrap_or(Path::new("."));
        parent.join("payloads.db")
    };

    if !sqlite_path.exists() {
        return Ok(());
    }

    // If we already migrated previously the marker file is what's on disk;
    // skip silently.
    let migrated_marker = sqlite_path.with_extension("db.migrated");
    if migrated_marker.exists() && !sqlite_path.exists() {
        return Ok(());
    }

    use rusqlite::Connection;

    let conn = Connection::open_with_flags(
        &sqlite_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| PayloadStoreError::Migration {
        path: sqlite_path.clone(),
        message: format!("open legacy sqlite db read-only: {e}"),
    })?;

    // Schema check: if the `payloads` table is missing assume an empty/legacy
    // file and skip without touching it.
    let table_exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='payloads'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !table_exists {
        // Nothing to copy — rename so we don't retry every open.
        let _ = std::fs::rename(&sqlite_path, &migrated_marker);
        return Ok(());
    }

    let mut stmt = conn
        .prepare("SELECT segment, id, uuid, payload FROM payloads")
        .map_err(|e| PayloadStoreError::Migration {
            path: sqlite_path.clone(),
            message: format!("prepare legacy select: {e}"),
        })?;
    let rows_iter = stmt
        .query_map([], |row| {
            let segment: String = row.get(0)?;
            let id: String = row.get(1)?;
            let uuid_str: String = row.get(2)?;
            let payload: String = row.get(3)?;
            Ok((segment, id, uuid_str, payload))
        })
        .map_err(|e| PayloadStoreError::Migration {
            path: sqlite_path.clone(),
            message: format!("query legacy rows: {e}"),
        })?;

    // Stage rows in memory so we can open the redb db once and write them all
    // in a single transaction.
    let mut staged: Vec<(String, String, [u8; 16], String)> = Vec::new();
    for row in rows_iter {
        let (segment, id, uuid_str, payload) = row.map_err(|e| PayloadStoreError::Migration {
            path: sqlite_path.clone(),
            message: format!("read legacy row: {e}"),
        })?;
        let uuid = Uuid::parse_str(&uuid_str).map_err(|e| PayloadStoreError::Migration {
            path: sqlite_path.clone(),
            message: format!("invalid uuid in legacy row id={id}: {e}"),
        })?;
        staged.push((segment, id, *uuid.as_bytes(), payload));
    }

    // Open redb separately so the write happens before we register the long-
    // lived `Database` handle in `open`. We close it again by dropping `db` at
    // the end of this scope.
    let db = Database::create(redb_path).map_err(|e| PayloadStoreError::Database {
        path: redb_path.to_path_buf(),
        source: Box::new(e),
    })?;
    let wtx = db
        .begin_write()
        .map_err(|e| PayloadStoreError::Transaction {
            path: redb_path.to_path_buf(),
            source: Box::new(e),
        })?;
    {
        let mut table = wtx
            .open_table(PAYLOADS)
            .map_err(|e| PayloadStoreError::Table {
                path: redb_path.to_path_buf(),
                source: Box::new(e),
            })?;
        for (segment, id, uuid_bytes, payload_json) in staged {
            let record = PayloadRecord {
                uuid: uuid_bytes,
                payload: payload_json,
            };
            let value_bytes = postcard::to_allocvec(&record)
                .map_err(|e| PayloadStoreError::Postcard { source: e })?;
            let key = encode_payload_key(&segment, id.as_bytes());
            table
                .insert(key.as_slice(), value_bytes.as_slice())
                .map_err(|e| PayloadStoreError::Storage {
                    path: redb_path.to_path_buf(),
                    source: Box::new(e),
                })?;
        }
    }
    wtx.commit().map_err(|e| PayloadStoreError::Commit {
        path: redb_path.to_path_buf(),
        source: Box::new(e),
    })?;
    drop(db);

    // Drop the prepared statement / connection before renaming so SQLite
    // releases the file handle.
    drop(stmt);
    drop(conn);

    std::fs::rename(&sqlite_path, &migrated_marker).map_err(|e| PayloadStoreError::Io {
        path: sqlite_path,
        source: e,
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn fixture_uuid(b: u8) -> Uuid {
        let mut bytes = [0u8; 16];
        bytes[0] = b;
        Uuid::from_bytes(bytes)
    }

    #[test]
    fn roundtrip_persists_across_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("payloads.db");
        let u = fixture_uuid(1);

        {
            let store = PayloadStore::open(&path).unwrap();
            store
                .upsert("seg-a", "rec-1", u, &json!({"hello": "world"}))
                .unwrap();
        }

        // Reopen — payload must survive.
        let store2 = PayloadStore::open(&path).unwrap();
        let got = store2.get("seg-a", "rec-1").unwrap();
        assert_eq!(got, Some((u, json!({"hello": "world"}))));

        let rows = store2.load_all(None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "rec-1");
        assert_eq!(rows[0].uuid, u);
        assert_eq!(rows[0].segment, "seg-a");
    }

    #[test]
    fn get_missing_returns_none() {
        let dir = tempdir().unwrap();
        let store = PayloadStore::open(&dir.path().join("p.redb")).unwrap();
        assert!(store.get("seg-a", "nope").unwrap().is_none());
    }

    #[test]
    fn delete_drops_row() {
        let dir = tempdir().unwrap();
        let store = PayloadStore::open(&dir.path().join("p.redb")).unwrap();
        let u = fixture_uuid(2);
        store.upsert("seg-a", "k", u, &json!(42)).unwrap();
        store.delete("seg-a", "k").unwrap();
        assert!(store.get("seg-a", "k").unwrap().is_none());
        // Idempotent — second delete is fine.
        store.delete("seg-a", "k").unwrap();
    }

    #[test]
    fn exists_reports_membership() {
        let dir = tempdir().unwrap();
        let store = PayloadStore::open(&dir.path().join("p.redb")).unwrap();
        assert!(!store.exists("seg-a", "k").unwrap());
        store
            .upsert("seg-a", "k", fixture_uuid(5), &json!("v"))
            .unwrap();
        assert!(store.exists("seg-a", "k").unwrap());
        assert!(!store.exists("seg-b", "k").unwrap());
        store.delete("seg-a", "k").unwrap();
        assert!(!store.exists("seg-a", "k").unwrap());
    }

    #[test]
    fn lookup_id_for_uuid_round_trips() {
        let dir = tempdir().unwrap();
        let store = PayloadStore::open(&dir.path().join("p.redb")).unwrap();
        let u = fixture_uuid(7);
        store.upsert("seg-a", "rec-7", u, &json!({"x": 1})).unwrap();
        let got = store.lookup_id_for_uuid("seg-a", u).unwrap();
        assert_eq!(got, Some("rec-7".to_string()));
        // Wrong segment must miss.
        assert!(store.lookup_id_for_uuid("seg-b", u).unwrap().is_none());
    }

    #[test]
    fn load_all_filters_by_segment() {
        let dir = tempdir().unwrap();
        let store = PayloadStore::open(&dir.path().join("p.redb")).unwrap();
        store.upsert("a", "1", fixture_uuid(1), &json!(1)).unwrap();
        store.upsert("a", "2", fixture_uuid(2), &json!(2)).unwrap();
        store.upsert("b", "3", fixture_uuid(3), &json!(3)).unwrap();

        let only_a = store.load_all(Some("a")).unwrap();
        assert_eq!(only_a.len(), 2);
        assert!(only_a.iter().all(|r| r.segment == "a"));

        let all = store.load_all(None).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn list_segment_returns_rows() {
        let dir = tempdir().unwrap();
        let store = PayloadStore::open(&dir.path().join("p.redb")).unwrap();
        store
            .upsert("seg-a", "x", fixture_uuid(1), &json!({"k": "v"}))
            .unwrap();
        store
            .upsert("seg-a", "y", fixture_uuid(2), &json!({"k": "w"}))
            .unwrap();
        store
            .upsert("seg-b", "z", fixture_uuid(3), &json!({"k": "u"}))
            .unwrap();
        let mut rows = store.list_segment("seg-a").unwrap();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "x");
        assert_eq!(rows[0].1, fixture_uuid(1));
        assert!(rows[0].2.contains("\"v\""));
        assert_eq!(rows[1].0, "y");
        assert_eq!(rows[1].1, fixture_uuid(2));

        let other = store.list_segment("seg-b").unwrap();
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].0, "z");

        let empty = store.list_segment("seg-c").unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn callers_passing_payloads_db_get_redb_sibling() {
        // Existing callers (`TrustyBackedMemoryStore`) pass `payloads.db`. Make
        // sure the resolver redirects them to `payloads.redb` so the on-disk
        // store actually uses redb regardless of caller hygiene.
        let dir = tempdir().unwrap();
        let legacy_path = dir.path().join("payloads.db");
        let store = PayloadStore::open(&legacy_path).unwrap();
        store
            .upsert("s", "i", fixture_uuid(9), &json!({"ok": true}))
            .unwrap();
        drop(store);
        let redb_path = dir.path().join("payloads.redb");
        assert!(
            redb_path.exists(),
            "expected redb sibling to be created at {}",
            redb_path.display()
        );
    }

    #[cfg(feature = "sqlite-kg")]
    #[test]
    fn migrates_legacy_sqlite_rows() {
        use rusqlite::params;

        let dir = tempdir().unwrap();
        let legacy = dir.path().join("payloads.db");

        // Build a legacy SQLite payloads file with two rows.
        {
            let conn = rusqlite::Connection::open(&legacy).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE payloads (
                    segment TEXT NOT NULL,
                    id TEXT NOT NULL,
                    uuid TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    PRIMARY KEY (segment, id)
                );
                "#,
            )
            .unwrap();
            conn.execute(
                "INSERT INTO payloads (segment, id, uuid, payload) VALUES (?1, ?2, ?3, ?4)",
                params![
                    "seg-a",
                    "rec-1",
                    fixture_uuid(1).to_string(),
                    serde_json::to_string(&json!({"hello": "world"})).unwrap(),
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO payloads (segment, id, uuid, payload) VALUES (?1, ?2, ?3, ?4)",
                params![
                    "seg-b",
                    "rec-2",
                    fixture_uuid(2).to_string(),
                    serde_json::to_string(&json!({"n": 42})).unwrap(),
                ],
            )
            .unwrap();
        }

        // Open the new redb store at the same legacy path — migration must run
        // automatically.
        let store = PayloadStore::open(&legacy).unwrap();

        let a = store.get("seg-a", "rec-1").unwrap();
        assert_eq!(a, Some((fixture_uuid(1), json!({"hello": "world"}))));
        let b = store.get("seg-b", "rec-2").unwrap();
        assert_eq!(b, Some((fixture_uuid(2), json!({"n": 42}))));

        // Legacy file should be renamed.
        assert!(!legacy.exists(), "legacy payloads.db should be renamed");
        assert!(
            dir.path().join("payloads.db.migrated").exists(),
            "expected marker file"
        );

        // Reopen — must be a no-op (no panic, no duplicate rows).
        drop(store);
        let store2 = PayloadStore::open(&legacy).unwrap();
        let all = store2.load_all(None).unwrap();
        assert_eq!(all.len(), 2);
    }
}
