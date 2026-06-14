//! `PayloadStore` struct and all public CRUD methods.
//!
//! Why: Houses the store implementation separately from the error/type
//! definitions in `types.rs` so both files stay under the 500-SLOC cap.
//! What: `PayloadStore::open` + `upsert` + `get` + `exists` +
//! `lookup_id_for_uuid` + `delete` + `list_segment` + `load_all`.
//! Test: All methods are covered by the CRUD round-trip tests in `mod.rs`.

use redb::{Database, ReadableDatabase, ReadableTable};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

use crate::memory_core::store::kg_store::{PAYLOADS, encode_payload_key, segment_prefix};

use super::types::{
    PayloadRecord, PayloadRow, PayloadStoreError, Result, RowBytes, decode_row, resolve_redb_path,
    split_payload_key,
};

/// redb-backed sidecar for external string-id ↔ uuid ↔ JSON mappings.
///
/// Why: Provides the durable half of `TrustyBackedMemoryStore`'s in-memory
/// hashmap so adapter restarts don't lose payload data. As of #46 this is the
/// redb backend; the legacy SQLite migration path was removed in issue #989
/// (all palaces confirmed migrated).
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
    pub(super) db: Arc<Database>,
    pub(super) path: PathBuf,
}

impl PayloadStore {
    /// Open or create the redb-backed payload store at `path`.
    ///
    /// Why: Single entry point so callers don't have to know about the redb
    /// schema. Callers historically passed `<data_root>/payloads.db`; we
    /// rewrite that to a `payloads.redb` sibling transparently. The one-shot
    /// SQLite → redb migration was removed in issue #989 (all palaces
    /// confirmed migrated).
    /// What:
    /// 1. Resolves the redb path. Callers that still pass `payloads.db` get
    ///    `payloads.redb` next to it; other extensions are kept as-is.
    /// 2. Creates parent directories if missing.
    /// 3. Opens (or creates) the redb database and touches the PAYLOADS table
    ///    in a write transaction so range scans on a fresh file succeed.
    ///
    /// Test: `roundtrip_persists_across_reopen` opens the same path twice.
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

        let db =
            super::super::open_or_recreate(&redb_path).map_err(|e| PayloadStoreError::Database {
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
    ///
    /// Why: Both `lookup_id_for_uuid` and `list_segment` scan the same range;
    /// extracting the range scan prevents duplication.
    /// What: Collects range results into an owned `Vec<Result<RowBytes>>` so
    /// the redb transaction lifetime does not escape this function.
    /// Test: Covered indirectly by `lookup_id_for_uuid_round_trips` and
    /// `list_segment_returns_rows`.
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
