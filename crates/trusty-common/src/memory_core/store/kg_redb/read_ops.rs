//! Read methods for `KgStoreRedb`.
//!
//! Why: Separating read operations from write operations and struct definition
//! keeps each file under the 500-SLOC cap while remaining a coherent unit.
//! What: `impl KgStoreRedb` for `query_active`, `list_subjects`,
//! `list_subjects_with_counts`, `list_active`, `count_active_triples`,
//! `checkpoint`, `load_drawers`, `load_drawer_ids`, and `dump_all_triples`.
//! Test: `assert_then_query_returns_triple`, `list_subjects_returns_distinct_active_subjects`,
//! `count_active_triples_returns_live_only`, `upsert_drawer_then_load_drawers_round_trips`.

use crate::memory_core::palace::Drawer;
use crate::memory_core::store::kg_store::{
    ACTIVE_SUBJECT_COUNTS, DRAWERS, DrawerRecord, TRIPLES, TripleValue, decode_triple_key,
    decode_u64, decode_value, subject_prefix,
};
use anyhow::{Context, Result};
use chrono::DateTime;
use redb::{ReadableDatabase, ReadableTable};
use std::collections::HashSet;
use std::path::PathBuf;
use uuid::Uuid;

use super::super::kg::Triple;
use super::store::KgStoreRedb;
use super::types::{LegacyDrawerRecord, PreTaskDrawerRecord, parse_drawer_type, triple_from_parts};

impl KgStoreRedb {
    /// Return all currently active triples for `subject`.
    ///
    /// Why: Most queries want "what is true *now*". The primary TRIPLES table
    /// holds at most one active row per (subject, predicate), so a prefix scan
    /// on `subject_prefix(subject)` returns at most one row per predicate.
    /// What: Range scan over `[subject_prefix..end_of_prefix]`, filter rows
    /// whose `valid_to_ms.is_none()`, and map to `Triple`.
    /// Test: `assert_then_query_returns_triple`.
    pub fn query_active(&self, subject: &str) -> Result<Vec<Triple>> {
        let prefix = subject_prefix(subject);
        let rtx = self.db().begin_read().context("begin query_active txn")?;
        let triples = rtx
            .open_table(TRIPLES)
            .context("open triples table for query_active")?;
        let mut out = Vec::new();
        let mut end = prefix.clone();
        // Build exclusive end key by appending 0xFF — every valid key with this
        // subject prefix sorts before it.
        end.push(0xFF);
        let range = triples
            .range::<&[u8]>(prefix.as_slice()..end.as_slice())
            .context("range scan for query_active")?;
        for entry in range {
            let (k, v) = entry.context("read row in query_active")?;
            // Skip history rows (which we never put under the active prefix
            // anyway, but defensive against future encoders).
            if k.value().starts_with(b"hist:") {
                continue;
            }
            let value: TripleValue =
                decode_value(v.value()).context("decode TripleValue in query_active")?;
            if value.valid_to_ms.is_some() {
                continue;
            }
            let (s, p) = match decode_triple_key(k.value()) {
                Some(parts) => parts,
                None => continue,
            };
            if s != subject {
                continue;
            }
            out.push(triple_from_parts(s, p, value)?);
        }
        Ok(out)
    }

    /// List up to `limit` distinct subjects that have at least one active
    /// triple, ordered alphabetically.
    ///
    /// Why: KG Explorer UI browses subjects without knowing one upfront.
    /// What: Iterate ACTIVE_SUBJECT_COUNTS (keyed by subject bytes, sorted
    /// alphabetically), collect subjects whose count is > 0, take `limit`.
    /// Test: `list_subjects_returns_distinct_active_subjects`.
    pub fn list_subjects(&self, limit: usize) -> Result<Vec<String>> {
        let rtx = self.db().begin_read().context("begin list_subjects txn")?;
        let counts = rtx
            .open_table(ACTIVE_SUBJECT_COUNTS)
            .context("open active_subject_counts")?;
        let mut out = Vec::new();
        for entry in counts.iter().context("iter counts")? {
            if out.len() >= limit {
                break;
            }
            let (k, v) = entry.context("read counts row")?;
            if decode_u64(v.value()) == 0 {
                continue;
            }
            let s = std::str::from_utf8(k.value())
                .context("invalid utf8 in subject counts key")?
                .to_string();
            out.push(s);
        }
        Ok(out)
    }

    /// List up to `limit` `(subject, count)` rows for subjects with at least
    /// one active triple, ordered alphabetically by subject.
    ///
    /// Why: KG Explorer UI shows a count badge next to each subject; computing
    /// the count server-side in one pass avoids one query per subject.
    /// What: Iterate ACTIVE_SUBJECT_COUNTS in key order, take rows with
    /// non-zero counts up to `limit`.
    /// Test: `list_subjects_with_counts_returns_grouped_counts`.
    pub fn list_subjects_with_counts(&self, limit: usize) -> Result<Vec<(String, u64)>> {
        let rtx = self
            .db()
            .begin_read()
            .context("begin list_subjects_with_counts txn")?;
        let counts = rtx
            .open_table(ACTIVE_SUBJECT_COUNTS)
            .context("open active_subject_counts")?;
        let mut out = Vec::new();
        for entry in counts.iter().context("iter counts")? {
            if out.len() >= limit {
                break;
            }
            let (k, v) = entry.context("read counts row")?;
            let c = decode_u64(v.value());
            if c == 0 {
                continue;
            }
            let s = std::str::from_utf8(k.value())
                .context("invalid utf8 in subject counts key")?
                .to_string();
            out.push((s, c));
        }
        Ok(out)
    }

    /// List up to `limit` active triples ordered by `valid_from` descending,
    /// skipping the first `offset` rows.
    ///
    /// Why: KG Explorer's "All" mode pages through every active triple.
    /// What: Full scan of TRIPLES, filter active rows, sort by valid_from desc,
    /// take the requested window. We do a full scan because redb has no
    /// secondary index on valid_from — acceptable since the active set is
    /// bounded by application sizing.
    /// Test: `list_active_returns_ordered_window`.
    pub fn list_active(&self, limit: usize, offset: usize) -> Result<Vec<Triple>> {
        let rtx = self.db().begin_read().context("begin list_active txn")?;
        let triples = rtx
            .open_table(TRIPLES)
            .context("open triples table for list_active")?;
        let mut rows = Vec::new();
        for entry in triples.iter().context("iter triples")? {
            let (k, v) = entry.context("read triples row")?;
            if k.value().starts_with(b"hist:") {
                continue;
            }
            let value: TripleValue =
                decode_value(v.value()).context("decode TripleValue in list_active")?;
            if value.valid_to_ms.is_some() {
                continue;
            }
            let (s, p) = match decode_triple_key(k.value()) {
                Some(parts) => parts,
                None => continue,
            };
            rows.push((value.valid_from_ms, s, p, value));
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.0));
        let mut out = Vec::new();
        for (_, s, p, value) in rows.into_iter().skip(offset).take(limit) {
            out.push(triple_from_parts(s, p, value)?);
        }
        Ok(out)
    }

    /// Count currently active triples (sum of ACTIVE_SUBJECT_COUNTS).
    ///
    /// Why: Dashboard tally of live facts. Maintained incrementally so it is
    /// O(distinct subjects) rather than O(history).
    /// What: Iterate ACTIVE_SUBJECT_COUNTS, sum values.
    /// Test: `count_active_triples_returns_live_only`.
    pub fn count_active_triples(&self) -> u64 {
        let rtx = match self.db().begin_read() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("count_active_triples: begin_read failed: {e:#}");
                return 0;
            }
        };
        let counts = match rtx.open_table(ACTIVE_SUBJECT_COUNTS) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("count_active_triples: open table failed: {e:#}");
                return 0;
            }
        };
        let mut total: u64 = 0;
        let iter = match counts.iter() {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!("count_active_triples: iter failed: {e:#}");
                return 0;
            }
        };
        for entry in iter {
            match entry {
                Ok((_, v)) => total = total.saturating_add(decode_u64(v.value())),
                Err(e) => {
                    tracing::warn!("count_active_triples: row read failed: {e:#}");
                    continue;
                }
            }
        }
        total
    }

    /// Collect the distinct subjects of every ACTIVE triple with `predicate`.
    ///
    /// Why: The dream-consolidation tombstone design (epic #2866, spec §4.3)
    /// needs "all drawers with an active `superseded_by` edge" as ONE bulk
    /// scan per recall/pass — not one KG query per drawer. No existing read
    /// method returns subjects filtered by predicate.
    /// What: Full scan of the TRIPLES table (same acceptable-cost rationale
    /// as `list_active` — redb has no secondary index on predicate), keeping
    /// subjects of rows that are active (`valid_to_ms.is_none()`) and whose
    /// predicate matches exactly. Returns a de-duplicated `HashSet`.
    /// Test: `subjects_for_predicate_returns_active_matches` in kg tests.
    pub fn subjects_for_predicate(&self, predicate: &str) -> Result<HashSet<String>> {
        let rtx = self
            .db()
            .begin_read()
            .context("begin subjects_for_predicate txn")?;
        let triples = rtx
            .open_table(TRIPLES)
            .context("open triples table for subjects_for_predicate")?;
        let mut out = HashSet::new();
        for entry in triples.iter().context("iter triples")? {
            let (k, v) = entry.context("read triples row")?;
            if k.value().starts_with(b"hist:") {
                continue;
            }
            let value: TripleValue =
                decode_value(v.value()).context("decode TripleValue in subjects_for_predicate")?;
            if value.valid_to_ms.is_some() {
                continue;
            }
            let (s, p) = match decode_triple_key(k.value()) {
                Some(parts) => parts,
                None => continue,
            };
            if p == predicate {
                out.insert(s);
            }
        }
        Ok(out)
    }

    /// No-op checkpoint hook.
    ///
    /// Why: SQLite needed `PRAGMA wal_checkpoint(PASSIVE)` to bound the WAL.
    /// redb manages its own write-ahead log internally and does not require a
    /// manual checkpoint; the call is kept for API compatibility.
    /// What: Returns immediately.
    /// Test: Implicit via `checkpoint_is_noop`.
    pub fn checkpoint(&self) -> Result<()> {
        // redb manages its commit log internally — nothing to do.
        Ok(())
    }

    /// Load all drawers from the table.
    ///
    /// Why: Cold-start retrieval needs the full drawer table to map every HNSW
    /// vector hit back to metadata.
    /// What: Iterate DRAWERS, decode each `DrawerRecord` back into a `Drawer`.
    /// Rows with malformed UUID/timestamp are skipped with a warning.
    /// Test: `upsert_drawer_then_load_drawers_round_trips`.
    pub fn load_drawers(&self) -> Result<Vec<Drawer>> {
        let rtx = self.db().begin_read().context("begin load_drawers txn")?;
        let drawers = rtx.open_table(DRAWERS).context("open drawers table")?;
        let mut out = Vec::new();
        for entry in drawers.iter().context("iter drawers")? {
            let (k, v) = entry.context("read drawer row")?;
            let id_bytes = k.value();
            if id_bytes.len() != 16 {
                tracing::warn!(len = id_bytes.len(), "skip drawer with non-16-byte id key");
                continue;
            }
            let mut id_arr = [0u8; 16];
            id_arr.copy_from_slice(id_bytes);
            let id = Uuid::from_bytes(id_arr);
            let record: DrawerRecord = match decode_value::<DrawerRecord>(v.value()) {
                Ok(r) => r,
                Err(_) => {
                    // Postcard is positional, so older rows lack the current
                    // shape's trailing fields and refuse to decode. Walk the
                    // migration chain newest→oldest, lifting each forward:
                    //   1. PreTaskDrawerRecord — #61-era (drawer_type +
                    //      expires_at_ms, no spec-001 completed_at_ms).
                    //   2. LegacyDrawerRecord — pre-#61 (none of those fields).
                    match decode_value::<PreTaskDrawerRecord>(v.value()) {
                        Ok(pre) => pre.into(),
                        Err(_) => match decode_value::<LegacyDrawerRecord>(v.value()) {
                            Ok(legacy) => legacy.into(),
                            Err(e) => {
                                tracing::warn!(id = %id, "skip drawer with malformed value: {e}");
                                continue;
                            }
                        },
                    }
                }
            };
            let room_id = match Uuid::parse_str(&record.room_id) {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!(id = %id, "skip drawer with invalid room_id: {e}");
                    continue;
                }
            };
            let created_at = match DateTime::from_timestamp_millis(record.created_at_ms) {
                Some(dt) => dt,
                None => {
                    tracing::warn!(id = %id, "skip drawer with invalid created_at_ms");
                    continue;
                }
            };
            let drawer_type = parse_drawer_type(record.drawer_type.as_deref());
            let expires_at = record
                .expires_at_ms
                .and_then(DateTime::from_timestamp_millis);
            let completed_at = record
                .completed_at_ms
                .and_then(DateTime::from_timestamp_millis);
            out.push(Drawer {
                id,
                room_id,
                content: record.content,
                importance: record.importance,
                source_file: record.source_file.map(PathBuf::from),
                created_at,
                tags: record.tags,
                last_accessed_at: None,
                access_count: 0,
                drawer_type,
                expires_at,
                completed_at,
            });
        }
        Ok(out)
    }

    /// Load just the set of drawer IDs.
    ///
    /// Why: Compaction only needs "is this UUID a live drawer?"; this avoids
    /// the cost of materializing `Drawer` rows.
    /// What: Iterate DRAWERS keys, parse each 16-byte slice into a `Uuid`,
    /// collect into a `HashSet`.
    /// Test: `load_drawer_ids_matches_load_drawers`.
    pub fn load_drawer_ids(&self) -> Result<HashSet<Uuid>> {
        let rtx = self
            .db()
            .begin_read()
            .context("begin load_drawer_ids txn")?;
        let drawers = rtx.open_table(DRAWERS).context("open drawers table")?;
        let mut out = HashSet::new();
        for entry in drawers.iter().context("iter drawers")? {
            let (k, _) = entry.context("read drawer row")?;
            let id_bytes = k.value();
            if id_bytes.len() != 16 {
                continue;
            }
            let mut id_arr = [0u8; 16];
            id_arr.copy_from_slice(id_bytes);
            out.insert(Uuid::from_bytes(id_arr));
        }
        Ok(out)
    }

    /// Dump every triple, including closed history rows.
    ///
    /// Why: The #45 migration path needs to walk the entire table to export
    /// data. Also useful for diagnostics.
    /// What: Scan the TRIPLES table end-to-end, returning both active rows and
    /// `hist:` rows decoded as `Triple` (so `valid_to.is_some()` for history).
    /// Test: `assert_supersedes_prior` checks history is preserved.
    pub fn dump_all_triples(&self) -> Result<Vec<Triple>> {
        let rtx = self
            .db()
            .begin_read()
            .context("begin dump_all_triples txn")?;
        let triples = rtx
            .open_table(TRIPLES)
            .context("open triples table for dump_all_triples")?;
        let mut out = Vec::new();
        for entry in triples.iter().context("iter triples for dump")? {
            let (k, v) = entry.context("read triples row for dump")?;
            let key_bytes = k.value();
            let value: TripleValue = match decode_value(v.value()) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("skip undecodable triple value in dump: {e}");
                    continue;
                }
            };
            let (s, p) = if let Some(stripped) = key_bytes.strip_prefix(b"hist:") {
                // History key = `hist:` + original encoded key + 8 byte suffix.
                if stripped.len() < 8 {
                    continue;
                }
                let core = &stripped[..stripped.len() - 8];
                match decode_triple_key(core) {
                    Some(parts) => parts,
                    None => continue,
                }
            } else {
                match decode_triple_key(key_bytes) {
                    Some(parts) => parts,
                    None => continue,
                }
            };
            out.push(triple_from_parts(s, p, value)?);
        }
        Ok(out)
    }
}
