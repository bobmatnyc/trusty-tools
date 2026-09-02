//! The table inventory a `kg.redb` copy-then-swap walks, and the copy itself
//! (#6652).
//!
//! Why: redb cannot iterate a table it does not know the key and value types
//! of — `open_untyped_table` yields row counts and byte stats, never rows — so
//! a rewrite has to name every table explicitly. That makes the inventory a
//! correctness surface: a table present in the file but missing from this list
//! would be silently dropped by the rewrite. It is therefore also the
//! unknown-table guard, and it FAILS CLOSED — an unrecognised table aborts the
//! compaction with the live file untouched rather than rewriting without it.
//! What: [`BYTE_TABLES`] and [`STR_TABLES`] name the eleven tables `kg.redb`
//! carries, [`DROPPED_TABLES`] names the one #6652 deliberately does not copy,
//! and [`copy_all`] streams rows from a read transaction on the source into
//! write transactions on the destination, applying the history prune as a skip.
//! Test: `unknown_table_aborts_the_compaction`,
//! `compaction_preserves_every_live_row`.

use crate::memory_core::store::kg_store::{
    ACTIVE_SUBJECT_COUNTS, DRAWERS, DRAWERS_BY_FACT_KEY, KG_SCHEMA, ROOM_KEYS, ROOMS, TRIPLES,
    TRIPLES_BY_OBJECT, TripleValue, WING_KEYS, WINGS, decode_value,
};
use anyhow::{Context, Result};
use redb::{Database, ReadTransaction, ReadableTable, TableDefinition, TableHandle};

use super::stats::history_close_ms;

/// Rows written per destination write transaction.
///
/// Why: one transaction for a 342 MB rewrite would hold every dirty page in
/// memory until the single commit. Batching bounds that without making the
/// copy non-atomic overall — the destination is a throw-away file until the
/// rename, so a partially-written one is discarded, never observed.
const COPY_BATCH_ROWS: usize = 20_000;

/// Tables keyed and valued by raw bytes.
pub(super) const BYTE_TABLES: &[TableDefinition<'static, &'static [u8], &'static [u8]>] = &[
    TRIPLES,
    TRIPLES_BY_OBJECT,
    ACTIVE_SUBJECT_COUNTS,
    DRAWERS,
    DRAWERS_BY_FACT_KEY,
    ROOMS,
    WINGS,
];

/// Tables keyed by `&str` with byte values.
pub(super) const STR_TABLES: &[TableDefinition<'static, &'static str, &'static [u8]>] =
    &[ROOM_KEYS, WING_KEYS, KG_SCHEMA];

/// Tables that exist on disk and are deliberately NOT copied.
///
/// Why (#6652): `triples_by_predicate` has no reader anywhere in the workspace.
/// Leaving it out of the rewrite is how its bytes are reclaimed; listing it
/// here is how the unknown-table guard tells "dead index" apart from "a table
/// this code has never heard of". Spelled as a literal because
/// `TableHandle::name` is not `const`; [`dropped_names_match_the_definitions`]
/// pins it to `kg_store::TRIPLES_BY_PREDICATE`.
pub(super) const DROPPED_TABLES: &[&str] = &["triples_by_predicate"];

/// Every table name the copy recognises, dropped ones included.
///
/// Test: `unknown_table_aborts_the_compaction`.
pub(super) fn known_table_names() -> Vec<&'static str> {
    BYTE_TABLES
        .iter()
        .map(|t| t.name())
        .chain(STR_TABLES.iter().map(|t| t.name()))
        .chain(DROPPED_TABLES.iter().copied())
        .collect()
}

/// What one [`copy_all`] run moved and skipped.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct CopyCounts {
    pub rows_copied: u64,
    pub history_rows_pruned: u64,
}

/// Stream every live row from `src` into `dest`, pruning as it goes.
///
/// Why: this is the whole compaction. redb never returns freed pages to the
/// filesystem — `forget` and `retract` hand them to its internal free list, so
/// a palace's file only ever grows — and `Database::compact` needs `&mut
/// Database` plus zero live read transactions, neither of which a serving
/// daemon can offer. Rewriting into a fresh file is the only mechanism left,
/// and doing the prune as a SKIP during that rewrite (rather than as deletes
/// against the live file first) means the deletions cost nothing: an in-place
/// delete of a million history rows would grow `kg.redb` before the rewrite
/// shrank it.
/// What: reads through ONE caller-supplied read transaction, so the copy is a
/// single consistent snapshot rather than eleven independently-timed ones.
/// Writes in [`COPY_BATCH_ROWS`]-sized transactions. A `TRIPLES` row is skipped
/// when [`history_close_ms`] says it is a closed history row older than
/// `history_cutoff_ms`; every other row, including any row whose value will not
/// decode, is copied verbatim.
/// Test: `compaction_preserves_every_live_row`,
/// `compaction_prunes_only_stale_history`.
pub(super) fn copy_all(
    src: &ReadTransaction,
    dest: &Database,
    history_cutoff_ms: Option<i64>,
) -> Result<CopyCounts> {
    let mut counts = CopyCounts::default();
    for def in BYTE_TABLES {
        let prune = def.name() == TRIPLES.name();
        copy_byte_table(
            src,
            dest,
            *def,
            prune.then_some(history_cutoff_ms).flatten(),
            &mut counts,
        )?;
    }
    for def in STR_TABLES {
        copy_str_table(src, dest, *def, &mut counts)?;
    }
    Ok(counts)
}

/// Copy one byte-keyed table, optionally skipping stale history rows.
///
/// Test: `compaction_prunes_only_stale_history`.
fn copy_byte_table(
    src: &ReadTransaction,
    dest: &Database,
    def: TableDefinition<'static, &'static [u8], &'static [u8]>,
    history_cutoff_ms: Option<i64>,
    counts: &mut CopyCounts,
) -> Result<()> {
    let Some(table) = open_source(src, def)? else {
        // The source has never written this table. Create it empty in the
        // destination so the schema stays whole, exactly as `open_with_intent`
        // does for a fresh palace.
        return touch_byte_table(dest, def);
    };
    let mut batch: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(COPY_BATCH_ROWS);
    touch_byte_table(dest, def)?;
    for entry in table
        .iter()
        .with_context(|| format!("iterate {} for compaction", def.name()))?
    {
        let (k, v) = entry.with_context(|| format!("read a {} row for compaction", def.name()))?;
        let key = k.value();
        let raw = v.value();
        if let Some(cutoff) = history_cutoff_ms
            && is_stale_history(key, raw, cutoff)
        {
            counts.history_rows_pruned += 1;
            continue;
        }
        batch.push((key.to_vec(), raw.to_vec()));
        counts.rows_copied += 1;
        if batch.len() >= COPY_BATCH_ROWS {
            flush_byte_batch(dest, def, &mut batch)?;
        }
    }
    flush_byte_batch(dest, def, &mut batch)
}

/// Copy one `&str`-keyed table verbatim.
///
/// Test: `compaction_preserves_every_live_row`.
fn copy_str_table(
    src: &ReadTransaction,
    dest: &Database,
    def: TableDefinition<'static, &'static str, &'static [u8]>,
    counts: &mut CopyCounts,
) -> Result<()> {
    touch_str_table(dest, def)?;
    let table = match src.open_table(def) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(()),
        Err(e) => {
            return Err(
                anyhow::Error::new(e).context(format!("open {} for compaction", def.name()))
            );
        }
    };
    let mut batch: Vec<(String, Vec<u8>)> = Vec::with_capacity(COPY_BATCH_ROWS);
    for entry in table
        .iter()
        .with_context(|| format!("iterate {} for compaction", def.name()))?
    {
        let (k, v) = entry.with_context(|| format!("read a {} row for compaction", def.name()))?;
        batch.push((k.value().to_string(), v.value().to_vec()));
        counts.rows_copied += 1;
        if batch.len() >= COPY_BATCH_ROWS {
            flush_str_batch(dest, def, &mut batch)?;
        }
    }
    flush_str_batch(dest, def, &mut batch)
}

/// Whether this `TRIPLES` row is a closed history row older than `cutoff_ms`.
///
/// Why: the prune predicate and the measurement that justified it must agree
/// exactly, so both go through [`history_close_ms`]. A value that will not
/// decode returns `false` — the compaction copies what it cannot read rather
/// than dropping it.
/// Test: `compaction_prunes_only_stale_history`,
/// `an_undecodable_row_survives_the_compaction`.
fn is_stale_history(key: &[u8], raw: &[u8], cutoff_ms: i64) -> bool {
    let Ok(value) = decode_value::<TripleValue>(raw) else {
        return false;
    };
    history_close_ms(key, &value).is_some_and(|closed| closed < cutoff_ms)
}

/// Open a byte table on the source, treating "never written" as absent.
fn open_source(
    src: &ReadTransaction,
    def: TableDefinition<'static, &'static [u8], &'static [u8]>,
) -> Result<Option<redb::ReadOnlyTable<&'static [u8], &'static [u8]>>> {
    match src.open_table(def) {
        Ok(t) => Ok(Some(t)),
        Err(redb::TableError::TableDoesNotExist(_)) => Ok(None),
        Err(e) => Err(anyhow::Error::new(e).context(format!("open {} for compaction", def.name()))),
    }
}

/// Create an empty byte table in the destination so the schema stays whole.
fn touch_byte_table(
    dest: &Database,
    def: TableDefinition<'static, &'static [u8], &'static [u8]>,
) -> Result<()> {
    let wtx = dest.begin_write().context("begin table-touch txn")?;
    {
        let _ = wtx
            .open_table(def)
            .with_context(|| format!("touch {} in compacted file", def.name()))?;
    }
    wtx.commit().context("commit table-touch txn")
}

/// Create an empty `&str` table in the destination.
fn touch_str_table(
    dest: &Database,
    def: TableDefinition<'static, &'static str, &'static [u8]>,
) -> Result<()> {
    let wtx = dest.begin_write().context("begin table-touch txn")?;
    {
        let _ = wtx
            .open_table(def)
            .with_context(|| format!("touch {} in compacted file", def.name()))?;
    }
    wtx.commit().context("commit table-touch txn")
}

/// Commit one batch of byte rows and clear it.
fn flush_byte_batch(
    dest: &Database,
    def: TableDefinition<'static, &'static [u8], &'static [u8]>,
    batch: &mut Vec<(Vec<u8>, Vec<u8>)>,
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let wtx = dest.begin_write().context("begin copy batch txn")?;
    {
        let mut table = wtx
            .open_table(def)
            .with_context(|| format!("open {} in compacted file", def.name()))?;
        for (k, v) in batch.iter() {
            table
                .insert(k.as_slice(), v.as_slice())
                .with_context(|| format!("insert into {} during compaction", def.name()))?;
        }
    }
    wtx.commit().context("commit copy batch txn")?;
    batch.clear();
    Ok(())
}

/// Commit one batch of `&str` rows and clear it.
fn flush_str_batch(
    dest: &Database,
    def: TableDefinition<'static, &'static str, &'static [u8]>,
    batch: &mut Vec<(String, Vec<u8>)>,
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let wtx = dest.begin_write().context("begin copy batch txn")?;
    {
        let mut table = wtx
            .open_table(def)
            .with_context(|| format!("open {} in compacted file", def.name()))?;
        for (k, v) in batch.iter() {
            table
                .insert(k.as_str(), v.as_slice())
                .with_context(|| format!("insert into {} during compaction", def.name()))?;
        }
    }
    wtx.commit().context("commit copy batch txn")?;
    batch.clear();
    Ok(())
}
