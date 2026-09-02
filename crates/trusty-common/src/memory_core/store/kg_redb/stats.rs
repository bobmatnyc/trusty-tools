//! Read-only measurement of a palace's `kg.redb` (#6652).
//!
//! Why: the owner ruling that opened #6652 — "most of the un-processed data in
//! memory is noise" — is a hypothesis, and nothing in the pre-#6652 surface
//! could confirm or refute it. `palace_info` reported drawer / room / wing
//! counts and stopped; no path anywhere reported the file's size, its per-table
//! row counts, or the split between live triples and the permanent `hist:` rows
//! every retraction leaves behind. Deleting rows before measuring them is how a
//! compaction turns into data loss, so the measurement ships first and the
//! pruning reads its numbers.
//! What: [`KgRedbStats::measure`] opens the file through
//! [`ReadOnlyRedb`] — never a write transaction, never a migration — walks
//! every table for redb's own row counts and byte stats, and makes one O(n)
//! pass over `TRIPLES` to split active rows from history. It ends with a
//! reclaimable-bytes estimate: the file's unaccounted slack plus the dead
//! predicate index plus the over-age history rows.
//! Test: `stats_counts_match_a_hand_built_palace`,
//! `stats_history_split_tracks_the_cutoff`,
//! `measure_writes_nothing_to_the_live_file`.

use crate::memory_core::store::concurrent_open::ReadOnlyRedb;
use crate::memory_core::store::kg_store::{
    TRIPLES, TRIPLES_BY_PREDICATE, TripleValue, decode_triple_key, decode_value,
};
use anyhow::{Context, Result};
use redb::{ReadableTable, ReadableTableMetadata, TableHandle};
use std::path::{Path, PathBuf};

/// The `hist:` key prefix every closed triple interval is filed under.
///
/// Why: three modules now test for it (`write_ops` writes it, `read_ops` skips
/// it, this module and [`super::copy_swap`] prune on it). A shared constant is
/// what keeps the prune predicate and the measurement that justifies it from
/// diverging by one byte.
pub const HISTORY_KEY_PREFIX: &[u8] = b"hist:";

/// Predicate whose active object marks a drawer as superseded by a canonical
/// one (`share::supersede`).
const SUPERSEDED_BY_PREDICATE: &str = "superseded_by";

/// Milliseconds in one day, for the history age gate.
const MS_PER_DAY: i64 = 86_400_000;

/// One table's row count and redb-reported byte usage.
///
/// Why: "how big is this table" has three different answers in redb — the bytes
/// of the keys and values themselves, the b-tree metadata that indexes them,
/// and the fragmentation inside allocated pages. A compaction reclaims the last
/// one and, when a table is dropped outright, the first two. Reporting them
/// separately is what lets an operator see which.
/// What: a plain record; every field comes straight from
/// `redb::TableStats` plus `len()`.
/// Test: `stats_counts_match_a_hand_built_palace`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct KgTableStats {
    pub name: String,
    pub rows: u64,
    /// Bytes of keys and values actually stored, excluding index overhead.
    pub stored_bytes: u64,
    /// Bytes of b-tree branch keys and other per-table metadata.
    pub metadata_bytes: u64,
    /// Bytes lost to fragmentation inside this table's pages.
    pub fragmented_bytes: u64,
    /// Leaf + branch pages the table occupies.
    pub pages: u64,
}

impl KgTableStats {
    /// Bytes this table would still need after a perfect rewrite.
    ///
    /// Why/What: `stored + metadata`, the floor a compaction cannot go below
    /// while the table's rows survive. Excludes fragmentation, which is exactly
    /// what the rewrite removes.
    /// Test: `stats_counts_match_a_hand_built_palace`.
    pub fn live_bytes(&self) -> u64 {
        self.stored_bytes.saturating_add(self.metadata_bytes)
    }
}

/// Everything a read-only pass can say about one `kg.redb` file.
///
/// Why: this is the report behind every decision the compaction phase makes —
/// whether to run at all, how much it expects to reclaim, and how much of that
/// is history rather than slack. It is also the operator-facing answer to
/// #6652's opening question.
/// What: file size, one [`KgTableStats`] per table present in the file, the
/// `TRIPLES` active/history split against a day cutoff, the superseded-drawer
/// count, and a reclaimable estimate.
/// Test: `stats_counts_match_a_hand_built_palace`,
/// `stats_history_split_tracks_the_cutoff`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct KgRedbStats {
    pub path: PathBuf,
    /// `metadata(path).len()` — the number #6652 opened with.
    pub file_bytes: u64,
    /// `true` when a writer held the live file and the numbers came from a
    /// process-local copy taken at that moment.
    pub from_snapshot: bool,
    /// Every table the file actually holds, in `list_tables` order.
    pub tables: Vec<KgTableStats>,
    /// Rows under a primary `(subject, predicate, object)` key with no
    /// `valid_to` — the triples every live query can see.
    pub triples_active: u64,
    /// Rows under a primary key that already carry a `valid_to`. The import
    /// path can write these; ordinary asserts cannot.
    pub triples_closed_in_place: u64,
    /// `hist:`-prefixed rows — one per retraction or functional-predicate
    /// overwrite, ever. Read only by `dump_all_triples` and the export paths
    /// built on it.
    pub triples_history: u64,
    /// History rows closed longer ago than [`Self::history_cutoff_days`].
    pub triples_history_stale: u64,
    /// Stored bytes those stale rows occupy (exact key + value byte sum).
    pub triples_history_stale_bytes: u64,
    /// The cutoff the two `stale` fields were measured against.
    pub history_cutoff_days: i64,
    /// Active `superseded_by` triples — drawers a semantic-consolidation run
    /// folded into a canonical drawer.
    pub superseded_drawers: u64,
    /// The `triples_by_predicate` index, when the file still carries it.
    /// `None` on a palace created after #6652, and on one whose compaction has
    /// run — the rewrite reclaims it by not copying it.
    pub dead_predicate_index: Option<KgTableStats>,
    /// Bytes a compaction would reclaim: file slack, plus the dead index, plus
    /// the over-age history rows. An estimate, deliberately conservative.
    pub reclaimable_bytes: u64,
}

impl KgRedbStats {
    /// Measure the `kg.redb` file at `path` without writing to it.
    ///
    /// Why: see the module doc. Read-only is a hard requirement, not a
    /// preference — this is the function a `--dry-run` calls, and a dry run
    /// that migrated the palace would make the whole gate a lie.
    /// What: opens through [`ReadOnlyRedb`] (live file `O_RDONLY`, or a
    /// throw-away snapshot when a writer holds it), reads `len()`/`stats()` for
    /// every table `list_tables` reports, then scans `TRIPLES` once to split
    /// active from history and to count active `superseded_by` rows. The scan
    /// is the only O(n) step; every other number is a b-tree metadata read.
    /// `history_cutoff_days` selects which history rows count as stale; pass
    /// the same value the prune will use or the estimate will not match what it
    /// frees.
    /// Test: `stats_counts_match_a_hand_built_palace`,
    /// `measure_writes_nothing_to_the_live_file`.
    pub fn measure(path: &Path, history_cutoff_days: i64) -> Result<Self> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        Self::measure_at(path, history_cutoff_days, now_ms)
    }

    /// [`Self::measure`] with the clock injected, so age tests are not flaky.
    ///
    /// Test: `stats_history_split_tracks_the_cutoff`.
    pub fn measure_at(path: &Path, history_cutoff_days: i64, now_ms: i64) -> Result<Self> {
        let file_bytes = std::fs::metadata(path)
            .with_context(|| format!("stat {}", path.display()))?
            .len();
        let db = ReadOnlyRedb::open(path)?;
        let from_snapshot = db.is_snapshot();
        let rtx = db.begin_read()?;

        let handles: Vec<redb::UntypedTableHandle> =
            rtx.list_tables().context("list kg.redb tables")?.collect();
        let mut tables = Vec::with_capacity(handles.len());
        for handle in handles {
            let name = handle.name().to_string();
            let table = rtx
                .open_untyped_table(handle)
                .with_context(|| format!("open table {name} for measurement"))?;
            let st = table
                .stats()
                .with_context(|| format!("read stats for table {name}"))?;
            tables.push(KgTableStats {
                name,
                rows: table.len().context("read table row count")?,
                stored_bytes: st.stored_bytes(),
                metadata_bytes: st.metadata_bytes(),
                fragmented_bytes: st.fragmented_bytes(),
                pages: st.leaf_pages().saturating_add(st.branch_pages()),
            });
        }

        let cutoff_ms = now_ms.saturating_sub(history_cutoff_days.saturating_mul(MS_PER_DAY));
        let scan = scan_triples(&rtx, cutoff_ms)?;

        let dead_predicate_index = tables
            .iter()
            .find(|t| t.name == TRIPLES_BY_PREDICATE.name())
            .cloned();

        // Slack is everything the file occupies that no table's live bytes
        // account for: redb's free-page list, its region headers, and the
        // fragmentation inside allocated pages. A rewrite drops all of it.
        let accounted: u64 = tables.iter().map(KgTableStats::live_bytes).sum();
        let slack = file_bytes.saturating_sub(accounted);
        let reclaimable_bytes = slack
            .saturating_add(
                dead_predicate_index
                    .as_ref()
                    .map_or(0, KgTableStats::live_bytes),
            )
            .saturating_add(scan.history_stale_bytes);

        Ok(Self {
            path: path.to_path_buf(),
            file_bytes,
            from_snapshot,
            tables,
            triples_active: scan.active,
            triples_closed_in_place: scan.closed_in_place,
            triples_history: scan.history,
            triples_history_stale: scan.history_stale,
            triples_history_stale_bytes: scan.history_stale_bytes,
            history_cutoff_days,
            superseded_drawers: scan.superseded,
            dead_predicate_index,
            reclaimable_bytes,
        })
    }

    /// Row count for one table by name, or `0` when the file has no such table.
    ///
    /// Test: `stats_counts_match_a_hand_built_palace`.
    pub fn rows(&self, table: &str) -> u64 {
        self.tables
            .iter()
            .find(|t| t.name == table)
            .map_or(0, |t| t.rows)
    }
}

/// What one pass over `TRIPLES` found.
#[derive(Debug, Default)]
struct TripleScan {
    active: u64,
    closed_in_place: u64,
    history: u64,
    history_stale: u64,
    history_stale_bytes: u64,
    superseded: u64,
}

/// Walk every `TRIPLES` row once and bucket it.
///
/// Why: there is no incremental counter for any of these — `ACTIVE_SUBJECT_COUNTS`
/// tracks active rows per subject, never a global total, and nothing tracks
/// history at all. One pass answers every question at once, which is cheaper
/// than four scans and guarantees the four numbers describe the same snapshot.
/// What: for each row, [`history_close_ms`] decides whether it is a prunable
/// history row and how old it is; rows under a primary key are split by
/// `valid_to_ms` and, when active, checked for the `superseded_by` predicate.
/// An undecodable value is counted nowhere and logged — the compaction copies
/// such a row verbatim rather than dropping what it cannot read.
/// Test: `stats_history_split_tracks_the_cutoff`.
fn scan_triples(rtx: &redb::ReadTransaction, cutoff_ms: i64) -> Result<TripleScan> {
    let mut out = TripleScan::default();
    let table = match rtx.open_table(TRIPLES) {
        Ok(t) => t,
        // A palace whose TRIPLES table has never been written has no table to
        // open; that is zero rows, not a failure to measure.
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(out),
        Err(e) => return Err(anyhow::Error::new(e).context("open TRIPLES for measurement")),
    };
    for entry in table.iter().context("iterate TRIPLES for measurement")? {
        let (k, v) = entry.context("read a TRIPLES row for measurement")?;
        let key = k.value();
        let raw = v.value();
        let value: TripleValue = match decode_value(raw) {
            Ok(val) => val,
            Err(e) => {
                tracing::warn!("#6652: skipping undecodable TRIPLES value while measuring: {e}");
                continue;
            }
        };
        if key.starts_with(HISTORY_KEY_PREFIX) {
            out.history += 1;
            if let Some(closed_ms) = history_close_ms(key, &value)
                && closed_ms < cutoff_ms
            {
                out.history_stale += 1;
                out.history_stale_bytes = out
                    .history_stale_bytes
                    .saturating_add((key.len() + raw.len()) as u64);
            }
            continue;
        }
        if value.valid_to_ms.is_some() {
            out.closed_in_place += 1;
            continue;
        }
        out.active += 1;
        if let Some((_s, p, _o)) = decode_triple_key(key)
            && p == SUPERSEDED_BY_PREDICATE
        {
            out.superseded += 1;
        }
    }
    Ok(out)
}

/// When this row was closed, if it is a prunable history row.
///
/// Why (#6652): this is THE prune predicate, and it is deliberately narrow.
/// Both halves must hold — the `hist:` key prefix AND a populated `valid_to_ms`
/// — because either one alone would put live data in range. A primary-key row
/// carrying a `valid_to` is still the row a `(subject, predicate, object)`
/// lookup lands on, and a `hist:` row with no `valid_to` is a shape this
/// codebase never writes, so it is something unrecognised rather than something
/// dead. Both fall through to `None`, and `None` means "copy it, keep it".
/// What: returns the close timestamp in epoch milliseconds, or `None` for
/// anything the prune must not touch.
/// Test: `history_close_ms_requires_both_the_prefix_and_a_valid_to`,
/// `prune_never_touches_an_active_row_however_old`.
pub fn history_close_ms(key: &[u8], value: &TripleValue) -> Option<i64> {
    if !key.starts_with(HISTORY_KEY_PREFIX) {
        return None;
    }
    value.valid_to_ms
}

/// The epoch-millisecond cutoff a `days`-old history prune uses.
///
/// Why: the CLI, the dream phase, and the measurement all have to compute the
/// same boundary from the same `days` value, or the dry run reports a count the
/// real run does not free.
/// What: `now_ms - days * 86_400_000`, saturating.
/// Test: `stats_history_split_tracks_the_cutoff`.
pub fn history_cutoff_ms(now_ms: i64, days: i64) -> i64 {
    now_ms.saturating_sub(days.saturating_mul(MS_PER_DAY))
}
