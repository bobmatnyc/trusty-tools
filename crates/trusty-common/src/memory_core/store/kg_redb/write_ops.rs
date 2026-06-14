//! Write methods for `KgStoreRedb` plus in-transaction batch helpers.
//!
//! Why: Separating write operations from the struct definition and read
//! operations keeps each file under the 500-SLOC cap.
//! What: `impl KgStoreRedb` for `assert`, `retract`, `upsert_drawer`,
//! `delete_drawer`, `delete_by_subject`, plus free-function helpers used
//! by `apply_batch` in `import.rs`.
//! Test: `assert_then_query_returns_triple`, `retract_closes_active_interval`,
//! `delete_drawer_removes_row`, `cascade_delete_removes_triples_for_subject`.

use crate::memory_core::palace::Drawer;
use crate::memory_core::store::kg_store::{
    ACTIVE_SUBJECT_COUNTS, DRAWERS, TRIPLES, TRIPLES_BY_OBJECT, TRIPLES_BY_PREDICATE, TripleValue,
    decode_triple_key, decode_u64, decode_value, encode_object_index_key,
    encode_predicate_index_key, encode_triple_key, encode_u64, encode_value, subject_prefix,
};
use anyhow::{Context, Result};
use redb::{ReadableDatabase, ReadableTable};
use uuid::Uuid;

use super::super::kg::Triple;
use super::store::KgStoreRedb;
use super::types::{Tbl, drawer_to_record, now_ms};

impl KgStoreRedb {
    /// Assert a triple. If an active row exists for `(subject, predicate)` it
    /// is closed (valid_to = now) and removed from secondary indexes; then the
    /// new triple is inserted and indexed.
    ///
    /// Why: Temporal model — facts have intervals. New assertion supersedes
    /// the prior active row instead of overwriting it, preserving history.
    /// What: Single write transaction over TRIPLES + secondary indexes +
    /// ACTIVE_SUBJECT_COUNTS so the invariant "at most one active row per
    /// (subject, predicate)" can never be observed broken.
    /// Test: `assert_then_query_returns_triple`, `assert_supersedes_prior`.
    pub fn assert(&self, triple: &Triple) -> Result<()> {
        self.check_writable()?;
        let close_ms = triple.valid_from.timestamp_millis();
        let new_value = TripleValue {
            object: triple.object.clone(),
            valid_from_ms: triple.valid_from.timestamp_millis(),
            valid_to_ms: triple.valid_to.map(|dt| dt.timestamp_millis()),
            confidence: triple.confidence,
            provenance: triple.provenance.clone(),
        };

        let wtx = self.db().begin_write().context("begin assert txn")?;
        {
            let mut triples = wtx.open_table(TRIPLES).context("open triples table")?;
            let mut by_object = wtx
                .open_table(TRIPLES_BY_OBJECT)
                .context("open triples_by_object table")?;
            let mut by_predicate = wtx
                .open_table(TRIPLES_BY_PREDICATE)
                .context("open triples_by_predicate table")?;
            let mut counts = wtx
                .open_table(ACTIVE_SUBJECT_COUNTS)
                .context("open active_subject_counts table")?;

            let key = encode_triple_key(&triple.subject, &triple.predicate);

            // Look up existing active row at this (subject, predicate). Because
            // we only ever store one row per (subject, predicate) key (the most
            // recent), checking by direct key is sufficient.
            let mut closed_any = false;
            let prior_opt: Option<TripleValue> = {
                let existing = triples
                    .get(key.as_slice())
                    .context("read existing triple")?;
                match existing {
                    Some(g) => Some(decode_value(g.value()).context("decode prior triple")?),
                    None => None,
                }
            };
            if let Some(prior) = prior_opt {
                if prior.valid_to_ms.is_none() {
                    // Active — close it by setting valid_to and writing back.
                    // But since we're about to overwrite with the new row, we
                    // only need to drop the secondary index entries and
                    // decrement the active counter.
                    let obj_key =
                        encode_object_index_key(&prior.object, &triple.subject, &triple.predicate);
                    by_object
                        .remove(obj_key.as_slice())
                        .context("remove prior object index")?;
                    let pred_key = encode_predicate_index_key(&triple.predicate, &triple.subject);
                    by_predicate
                        .remove(pred_key.as_slice())
                        .context("remove prior predicate index")?;
                    closed_any = true;
                }
                // History preservation: write the closed prior row into a
                // history key. We use a synthetic key suffix so it does not
                // collide with the active row. Format: `[hist:][orig key]
                // [valid_from_ms BE]`. This keeps dump_all_triples honest.
                if prior.valid_to_ms.is_none() {
                    let mut hist_key = Vec::with_capacity(5 + key.len() + 8);
                    hist_key.extend_from_slice(b"hist:");
                    hist_key.extend_from_slice(&key);
                    hist_key.extend_from_slice(&prior.valid_from_ms.to_be_bytes());
                    let closed = TripleValue {
                        valid_to_ms: Some(close_ms),
                        ..prior
                    };
                    let closed_bytes = encode_value(&closed).context("encode closed prior")?;
                    triples
                        .insert(hist_key.as_slice(), closed_bytes.as_slice())
                        .context("insert closed history row")?;
                }
            }

            // Insert / overwrite active row.
            let new_bytes = encode_value(&new_value).context("encode new triple")?;
            triples
                .insert(key.as_slice(), new_bytes.as_slice())
                .context("insert new triple")?;

            // Insert secondary indexes for the new active row (only when it
            // is itself active — `assert` with `valid_to = Some(_)` would be
            // a closed-on-arrival row that should not appear in indexes).
            if new_value.valid_to_ms.is_none() {
                let obj_key =
                    encode_object_index_key(&new_value.object, &triple.subject, &triple.predicate);
                by_object
                    .insert(obj_key.as_slice(), [].as_slice())
                    .context("insert new object index")?;
                let pred_key = encode_predicate_index_key(&triple.predicate, &triple.subject);
                by_predicate
                    .insert(pred_key.as_slice(), [].as_slice())
                    .context("insert new predicate index")?;

                // Maintain count: net change is 0 if we just closed one and
                // opened one; +1 if there was no prior active row.
                if !closed_any {
                    let subj_key = triple.subject.as_bytes();
                    let prev = counts
                        .get(subj_key)
                        .context("read prior count")?
                        .map(|v| decode_u64(v.value()))
                        .unwrap_or(0);
                    let next = prev.saturating_add(1);
                    counts
                        .insert(subj_key, encode_u64(next).as_slice())
                        .context("update active count")?;
                }
            } else if closed_any {
                // Closed-on-arrival row replacing an active one — decrement.
                let subj_key = triple.subject.as_bytes();
                let prev = counts
                    .get(subj_key)
                    .context("read prior count")?
                    .map(|v| decode_u64(v.value()))
                    .unwrap_or(0);
                let next = prev.saturating_sub(1);
                if next == 0 {
                    counts.remove(subj_key).context("remove zero count")?;
                } else {
                    counts
                        .insert(subj_key, encode_u64(next).as_slice())
                        .context("update active count")?;
                }
            }
        }
        wtx.commit().context("commit assert txn")?;
        Ok(())
    }

    /// Close the active triple for `(subject, predicate)` without inserting a
    /// replacement. Returns the number of rows closed (0 or 1).
    ///
    /// Why: `assert` always closes-and-replaces; retract is the way to say
    /// "this fact is no longer true and has no successor" — used by
    /// `remove_prompt_fact`.
    /// What: Reads the row at `(subject, predicate)`. If active, writes a
    /// history copy with `valid_to = now`, drops the active row from the
    /// primary table, removes secondary indexes, and decrements the count.
    /// Test: `retract_closes_active_interval`.
    pub fn retract(&self, subject: &str, predicate: &str) -> Result<usize> {
        self.check_writable()?;
        let key = encode_triple_key(subject, predicate);
        let close_ms = now_ms();
        let wtx = self.db().begin_write().context("begin retract txn")?;
        let closed;
        {
            let mut triples = wtx.open_table(TRIPLES).context("open triples table")?;
            let mut by_object = wtx
                .open_table(TRIPLES_BY_OBJECT)
                .context("open triples_by_object table")?;
            let mut by_predicate = wtx
                .open_table(TRIPLES_BY_PREDICATE)
                .context("open triples_by_predicate table")?;
            let mut counts = wtx
                .open_table(ACTIVE_SUBJECT_COUNTS)
                .context("open active_subject_counts table")?;

            let prior_opt: Option<TripleValue> = {
                let existing = triples
                    .get(key.as_slice())
                    .context("lookup active triple for retract")?;
                match existing {
                    Some(g) => Some(decode_value(g.value()).context("decode prior for retract")?),
                    None => None,
                }
            };
            match prior_opt {
                Some(prior) => {
                    if prior.valid_to_ms.is_none() {
                        // Move to history.
                        let mut hist_key = Vec::with_capacity(5 + key.len() + 8);
                        hist_key.extend_from_slice(b"hist:");
                        hist_key.extend_from_slice(&key);
                        hist_key.extend_from_slice(&prior.valid_from_ms.to_be_bytes());
                        let closed_v = TripleValue {
                            valid_to_ms: Some(close_ms),
                            ..prior.clone()
                        };
                        let bytes = encode_value(&closed_v).context("encode retract history")?;
                        triples
                            .insert(hist_key.as_slice(), bytes.as_slice())
                            .context("insert retract history row")?;
                        // Remove active row + indexes.
                        triples
                            .remove(key.as_slice())
                            .context("remove active row for retract")?;
                        let obj_key = encode_object_index_key(&prior.object, subject, predicate);
                        by_object
                            .remove(obj_key.as_slice())
                            .context("remove object index for retract")?;
                        let pred_key = encode_predicate_index_key(predicate, subject);
                        by_predicate
                            .remove(pred_key.as_slice())
                            .context("remove predicate index for retract")?;
                        // Decrement count.
                        let subj_key = subject.as_bytes();
                        let prev = counts
                            .get(subj_key)
                            .context("read prior count for retract")?
                            .map(|v| decode_u64(v.value()))
                            .unwrap_or(0);
                        let next = prev.saturating_sub(1);
                        if next == 0 {
                            counts.remove(subj_key).context("remove zero count")?;
                        } else {
                            counts
                                .insert(subj_key, encode_u64(next).as_slice())
                                .context("update count after retract")?;
                        }
                        closed = 1;
                    } else {
                        // Row exists but is already closed — nothing to do.
                        closed = 0;
                    }
                }
                None => {
                    closed = 0;
                }
            }
        }
        wtx.commit().context("commit retract txn")?;
        Ok(closed)
    }

    /// Persist a drawer's metadata.
    ///
    /// Why: HNSW only stores vectors keyed by UUID; without drawer metadata
    /// persisted alongside, vector hits map to nothing after a cold restart.
    /// What: Serialize the drawer to `DrawerRecord` and write under its UUID
    /// bytes in DRAWERS.
    /// Test: `upsert_drawer_then_load_drawers_round_trips`.
    pub fn upsert_drawer(&self, drawer: &Drawer) -> Result<()> {
        self.check_writable()?;
        let record = drawer_to_record(drawer);
        let bytes = encode_value(&record).context("encode drawer record")?;
        let id_bytes = *drawer.id.as_bytes();
        let wtx = self.db().begin_write().context("begin upsert_drawer txn")?;
        {
            let mut drawers = wtx.open_table(DRAWERS).context("open drawers table")?;
            drawers
                .insert(id_bytes.as_slice(), bytes.as_slice())
                .context("insert drawer record")?;
        }
        wtx.commit().context("commit upsert_drawer txn")?;
        Ok(())
    }

    /// Remove a drawer by UUID.
    ///
    /// Why: Forgetting must clear both the vector index and the persistent
    /// metadata row — otherwise restart resurrects the drawer.
    /// What: Remove the row keyed by UUID bytes from DRAWERS. No-op on unknown id.
    /// Test: `delete_drawer_removes_row`.
    pub fn delete_drawer(&self, id: Uuid) -> Result<()> {
        self.check_writable()?;
        let id_bytes = *id.as_bytes();
        let wtx = self.db().begin_write().context("begin delete_drawer txn")?;
        {
            let mut drawers = wtx.open_table(DRAWERS).context("open drawers table")?;
            drawers
                .remove(id_bytes.as_slice())
                .context("remove drawer record")?;
        }
        wtx.commit().context("commit delete_drawer txn")?;
        Ok(())
    }

    /// Delete all active triples whose subject matches `subject`.
    ///
    /// Why: Cascade-delete on drawer removal (issue #278) — when a drawer is
    /// forgotten, every triple extracted from it (identified by the
    /// `drawer:<uuid>` subject prefix) must be removed so the KG does not
    /// accumulate orphaned edges.
    /// What: Performs a prefix scan over TRIPLES using `subject_prefix(subject)`,
    /// collects every active (non-history, non-closed) `(subject, predicate)`
    /// pair, and retracts each via the existing `retract` path so secondary
    /// indexes and the active count table are kept consistent. Returns the
    /// number of active rows closed.
    /// Test: `cascade_delete_removes_triples_for_subject` in this module's
    /// test section.
    pub fn delete_by_subject(&self, subject: &str) -> Result<usize> {
        self.check_writable()?;
        let prefix = subject_prefix(subject);
        let mut to_retract: Vec<(String, String)> = Vec::new();
        {
            let rtx = self
                .db()
                .begin_read()
                .context("begin delete_by_subject read")?;
            let triples = rtx
                .open_table(TRIPLES)
                .context("open triples for delete_by_subject scan")?;
            let mut end = prefix.clone();
            end.push(0xFF);
            let range = triples
                .range::<&[u8]>(prefix.as_slice()..end.as_slice())
                .context("range scan for delete_by_subject")?;
            for entry in range {
                let (k, v) = entry.context("read row in delete_by_subject")?;
                if k.value().starts_with(b"hist:") {
                    continue;
                }
                let value: TripleValue =
                    decode_value(v.value()).context("decode value in delete_by_subject")?;
                if value.valid_to_ms.is_some() {
                    // Already closed — skip.
                    continue;
                }
                if let Some((s, p)) = decode_triple_key(k.value()) {
                    to_retract.push((s, p));
                }
            }
        }
        let mut closed = 0usize;
        for (s, p) in &to_retract {
            match self.retract(s, p) {
                Ok(n) => closed += n,
                Err(e) => {
                    tracing::warn!(subject = %s, predicate = %p, "delete_by_subject: retract failed: {e:#}");
                }
            }
        }
        Ok(closed)
    }
}

// ----- in-transaction helpers shared by the single-op and batch paths -----
//
// Why: The single-op `assert` / `retract` / drawer methods already
// implement the correct semantics inside their own `begin_write` block.
// To share that logic with `apply_batch` without duplicating it, we lift
// the per-op body into a free function that takes already-opened tables.
// This keeps the txn boundary explicit (one `begin_write` per batch) and
// avoids logic drift between the two paths. The single-op methods could
// be migrated to call these helpers in a follow-up; for now we accept
// the duplication to keep the diff minimal.

/// In-transaction assert helper; mirrors `KgStoreRedb::assert`.
///
/// Why: Lets `apply_batch` perform N asserts inside one write txn.
/// What: Same close-prior + insert-new + index-maintenance logic that
/// the single-op `assert` runs, but takes already-opened tables.
/// Test: `apply_batch_groups_asserts_into_single_commit`.
pub(super) fn batch_assert(
    triples: &mut Tbl<'_>,
    by_object: &mut Tbl<'_>,
    by_predicate: &mut Tbl<'_>,
    counts: &mut Tbl<'_>,
    triple: &Triple,
) -> Result<()> {
    let close_ms = triple.valid_from.timestamp_millis();
    let new_value = TripleValue {
        object: triple.object.clone(),
        valid_from_ms: triple.valid_from.timestamp_millis(),
        valid_to_ms: triple.valid_to.map(|dt| dt.timestamp_millis()),
        confidence: triple.confidence,
        provenance: triple.provenance.clone(),
    };
    let key = encode_triple_key(&triple.subject, &triple.predicate);

    let mut closed_any = false;
    let prior_opt: Option<TripleValue> = {
        let existing = triples
            .get(key.as_slice())
            .context("read existing triple (batch)")?;
        match existing {
            Some(g) => Some(decode_value(g.value()).context("decode prior triple (batch)")?),
            None => None,
        }
    };
    if let Some(prior) = prior_opt
        && prior.valid_to_ms.is_none()
    {
        let obj_key = encode_object_index_key(&prior.object, &triple.subject, &triple.predicate);
        by_object
            .remove(obj_key.as_slice())
            .context("remove prior object index (batch)")?;
        let pred_key = encode_predicate_index_key(&triple.predicate, &triple.subject);
        by_predicate
            .remove(pred_key.as_slice())
            .context("remove prior predicate index (batch)")?;
        closed_any = true;

        let mut hist_key = Vec::with_capacity(5 + key.len() + 8);
        hist_key.extend_from_slice(b"hist:");
        hist_key.extend_from_slice(&key);
        hist_key.extend_from_slice(&prior.valid_from_ms.to_be_bytes());
        let closed = TripleValue {
            valid_to_ms: Some(close_ms),
            ..prior
        };
        let closed_bytes = encode_value(&closed).context("encode closed prior (batch)")?;
        triples
            .insert(hist_key.as_slice(), closed_bytes.as_slice())
            .context("insert closed history row (batch)")?;
    }

    let new_bytes = encode_value(&new_value).context("encode new triple (batch)")?;
    triples
        .insert(key.as_slice(), new_bytes.as_slice())
        .context("insert new triple (batch)")?;

    if new_value.valid_to_ms.is_none() {
        let obj_key =
            encode_object_index_key(&new_value.object, &triple.subject, &triple.predicate);
        by_object
            .insert(obj_key.as_slice(), [].as_slice())
            .context("insert new object index (batch)")?;
        let pred_key = encode_predicate_index_key(&triple.predicate, &triple.subject);
        by_predicate
            .insert(pred_key.as_slice(), [].as_slice())
            .context("insert new predicate index (batch)")?;
        if !closed_any {
            let subj_key = triple.subject.as_bytes();
            let prev = counts
                .get(subj_key)
                .context("read prior count (batch)")?
                .map(|v| decode_u64(v.value()))
                .unwrap_or(0);
            let next = prev.saturating_add(1);
            counts
                .insert(subj_key, encode_u64(next).as_slice())
                .context("update active count (batch)")?;
        }
    } else if closed_any {
        let subj_key = triple.subject.as_bytes();
        let prev = counts
            .get(subj_key)
            .context("read prior count for closed-on-arrival (batch)")?
            .map(|v| decode_u64(v.value()))
            .unwrap_or(0);
        let next = prev.saturating_sub(1);
        if next == 0 {
            counts
                .remove(subj_key)
                .context("remove zero count (batch)")?;
        } else {
            counts
                .insert(subj_key, encode_u64(next).as_slice())
                .context("update active count (batch)")?;
        }
    }
    Ok(())
}

/// In-transaction retract helper; mirrors `KgStoreRedb::retract`.
///
/// Why: Lets `apply_batch` perform a retract inside one write txn.
/// What: Same move-to-history + index-removal logic as the single-op
/// `retract`, but takes already-opened tables.
/// Test: `apply_batch_groups_asserts_into_single_commit` (Retract variant).
pub(super) fn batch_retract(
    triples: &mut Tbl<'_>,
    by_object: &mut Tbl<'_>,
    by_predicate: &mut Tbl<'_>,
    counts: &mut Tbl<'_>,
    subject: &str,
    predicate: &str,
) -> Result<usize> {
    let key = encode_triple_key(subject, predicate);
    let close_ms = now_ms();
    let prior_opt: Option<TripleValue> = {
        let existing = triples
            .get(key.as_slice())
            .context("lookup active triple for retract (batch)")?;
        match existing {
            Some(g) => Some(decode_value(g.value()).context("decode prior for retract (batch)")?),
            None => None,
        }
    };
    let Some(prior) = prior_opt else {
        return Ok(0);
    };
    if prior.valid_to_ms.is_some() {
        return Ok(0);
    }

    let mut hist_key = Vec::with_capacity(5 + key.len() + 8);
    hist_key.extend_from_slice(b"hist:");
    hist_key.extend_from_slice(&key);
    hist_key.extend_from_slice(&prior.valid_from_ms.to_be_bytes());
    let closed_v = TripleValue {
        valid_to_ms: Some(close_ms),
        ..prior.clone()
    };
    let bytes = encode_value(&closed_v).context("encode retract history (batch)")?;
    triples
        .insert(hist_key.as_slice(), bytes.as_slice())
        .context("insert retract history row (batch)")?;
    triples
        .remove(key.as_slice())
        .context("remove active row for retract (batch)")?;
    let obj_key = encode_object_index_key(&prior.object, subject, predicate);
    by_object
        .remove(obj_key.as_slice())
        .context("remove object index for retract (batch)")?;
    let pred_key = encode_predicate_index_key(predicate, subject);
    by_predicate
        .remove(pred_key.as_slice())
        .context("remove predicate index for retract (batch)")?;
    let subj_key = subject.as_bytes();
    let prev = counts
        .get(subj_key)
        .context("read prior count for retract (batch)")?
        .map(|v| decode_u64(v.value()))
        .unwrap_or(0);
    let next = prev.saturating_sub(1);
    if next == 0 {
        counts
            .remove(subj_key)
            .context("remove zero count (batch)")?;
    } else {
        counts
            .insert(subj_key, encode_u64(next).as_slice())
            .context("update count after retract (batch)")?;
    }
    Ok(1)
}

/// In-transaction drawer upsert helper.
///
/// Why: Lets `apply_batch` upsert a drawer inside one write txn.
/// What: Serializes `drawer` and inserts under its UUID key in DRAWERS.
/// Test: `apply_batch_mixes_drawer_and_triple_ops`.
pub(super) fn batch_upsert_drawer(drawers: &mut Tbl<'_>, drawer: &Drawer) -> Result<()> {
    let record = drawer_to_record(drawer);
    let bytes = encode_value(&record).context("encode drawer record (batch)")?;
    let id_bytes = *drawer.id.as_bytes();
    drawers
        .insert(id_bytes.as_slice(), bytes.as_slice())
        .context("insert drawer record (batch)")?;
    Ok(())
}

/// In-transaction drawer delete helper.
///
/// Why: Lets `apply_batch` delete a drawer inside one write txn.
/// What: Removes the row keyed by UUID bytes from DRAWERS.
/// Test: Indirect via `apply_batch` with `DeleteDrawer` variant.
pub(super) fn batch_delete_drawer(drawers: &mut Tbl<'_>, id: Uuid) -> Result<()> {
    let id_bytes = *id.as_bytes();
    drawers
        .remove(id_bytes.as_slice())
        .context("remove drawer record (batch)")?;
    Ok(())
}
