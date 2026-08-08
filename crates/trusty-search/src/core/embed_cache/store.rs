//! redb-backed storage for the machine-wide embedding cache (issue #5024).
//!
//! Why: the cache has to survive daemon restarts to be worth anything — the
//! motivating case is a fresh worktree indexed hours after its base clone — so
//! it is on disk rather than in the heap. It is also strictly bounded: an
//! unbounded machine-wide vector store on a host with dozens of large indexes
//! would reach tens of gigabytes (see [`DEFAULT_MAX_MB`]).
//!
//! What: [`EmbedCacheStore`] over three redb tables — the entries, an
//! ascending-sequence LRU index, and a small meta table holding the running
//! byte total and the sequence counter. Reads are lock-free-ish (redb read
//! transactions never block the writer); every mutation of a batch happens in
//! one write transaction so the byte total can never drift from the entries.
//!
//! Test: see `super::tests`.

use std::path::Path;

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};

use super::key::CacheKey;

/// Entry table: 32-byte [`CacheKey`] → `[8-byte last-used seq LE][f32 LE ...]`.
///
/// Why: the sequence lives in the value rather than a side table so a hit
/// already knows which LRU row to retire, without a second lookup.
const ENTRIES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("embed_entries");

/// LRU index: monotonic sequence → the [`CacheKey`] last touched at that
/// sequence. Ascending key order is ascending age, so eviction is a forward
/// range scan from the front.
const LRU: TableDefinition<u64, &[u8]> = TableDefinition::new("embed_lru");

/// Meta table: `"total_bytes"` and `"next_seq"`.
const META: TableDefinition<&str, u64> = TableDefinition::new("embed_meta");

const META_TOTAL_BYTES: &str = "total_bytes";
const META_NEXT_SEQ: &str = "next_seq";

/// Per-entry bookkeeping charged on top of the vector's own bytes.
///
/// Why: the byte total is a budget for *disk*, so it has to include what the
/// key, the value header, the LRU row, and redb's B-tree nodes actually cost —
/// charging only `dim * 4` would let the file grow well past the ceiling. 96 B
/// covers the 32-byte key, the 8-byte sequence header, the ~40-byte LRU row,
/// and a slice of internal-node overhead.
/// What: added to `8 + dim * 4` in [`entry_cost`].
const ENTRY_OVERHEAD_BYTES: u64 = 96;

/// Default ceiling on the cache file, in megabytes.
///
/// Why: sized against this class of host. A 384-dim f32 vector costs ~1.6 KB
/// all-in, so 2 GiB holds ~1.28 M unique chunks. The measured live working set
/// on the development machine is ~146 K unique chunks (~240 MB) across 28
/// registered indexes, so this leaves roughly 8x headroom while still capping
/// the pathological case — 82 indexes at the `Medium` tier's 200 K
/// `TRUSTY_MAX_CHUNKS` would otherwise reach ~27 GB.
/// What: overridable via `TRUSTY_EMBED_CACHE_MAX_MB`; see
/// `super::config::resolve_max_bytes`.
pub(super) const DEFAULT_MAX_MB: u64 = 2048;

/// Errors this store can raise. All of them are non-fatal to a reindex — the
/// caller degrades to embedding everything (see `super::EmbedCache`).
#[derive(Debug, thiserror::Error)]
pub(crate) enum EmbedCacheError {
    #[error("embed cache database error: {0}")]
    Database(#[from] redb::DatabaseError),
    #[error("embed cache transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("embed cache table error: {0}")]
    Table(#[from] redb::TableError),
    #[error("embed cache storage error: {0}")]
    Storage(#[from] redb::StorageError),
    #[error("embed cache commit error: {0}")]
    Commit(#[from] redb::CommitError),
    #[error("embed cache io error: {0}")]
    Io(#[from] std::io::Error),
}

type Result<T> = std::result::Result<T, EmbedCacheError>;

/// Bytes one entry of `dim` f32 components charges against the ceiling.
fn entry_cost(dim: usize) -> u64 {
    8 + (dim as u64) * 4 + ENTRY_OVERHEAD_BYTES
}

/// A cache hit: the vector, plus the LRU sequence it currently occupies so the
/// batched touch can retire that row without re-reading the entry.
pub(crate) struct Hit {
    pub(crate) vector: Vec<f32>,
    pub(crate) seq: u64,
}

/// redb handle for the embedding cache.
///
/// Why: wrapping the `Database` lets the byte-budget invariant live in one
/// place — every insert path goes through [`Self::write_batch`], which updates
/// the total and evicts in the same transaction that added the entries, so a
/// crash can never leave the total under-counting what is on disk.
/// What: owns the database and the resolved ceiling.
/// Test: see `super::tests`.
pub(crate) struct EmbedCacheStore {
    db: Database,
    max_bytes: u64,
}

impl EmbedCacheStore {
    /// Open (or create) the cache at `path` with a `max_bytes` ceiling.
    ///
    /// Why: `redb_cache_bytes` is held deliberately small. This database is
    /// touched in short bursts during a reindex and never on the query hot
    /// path, so a large page cache would buy nothing and add to the daemon's
    /// resident set — which an entire memory-reduction workstream exists to
    /// keep down.
    /// What: creates the parent directory, opens the database with a bounded
    /// page cache, and initialises the three tables so later read transactions
    /// never fail on a missing table.
    /// Test: `open_creates_usable_store`, `reopen_preserves_entries`.
    pub(crate) fn open(path: &Path, max_bytes: u64, redb_cache_bytes: usize) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Database::builder()
            .set_cache_size(redb_cache_bytes)
            .create(path)?;
        // Materialise every table up front: `open_table` inside a read
        // transaction errors on a table that was never written, and treating
        // that as a cache failure would spam warnings on a fresh install.
        {
            let txn = db.begin_write()?;
            txn.open_table(ENTRIES)?;
            txn.open_table(LRU)?;
            txn.open_table(META)?;
            txn.commit()?;
        }
        Ok(Self { db, max_bytes })
    }

    /// Look up `keys`, returning one slot per input.
    ///
    /// Why: batched because a reindex asks about a whole parse batch at once
    /// and one read transaction over N keys is far cheaper than N transactions.
    /// What: `None` for a miss. A stored vector whose length is not
    /// `expected_dim` is also reported as a miss — that is the last line of
    /// defence if an identity ever failed to capture a model change, and
    /// re-embedding is always the safe answer.
    /// Test: `roundtrip_hits_after_write`, `wrong_dimension_entry_is_a_miss`.
    pub(crate) fn get_batch(
        &self,
        keys: &[CacheKey],
        expected_dim: usize,
    ) -> Result<Vec<Option<Hit>>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(ENTRIES)?;
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            out.push(match table.get(key.as_bytes())? {
                Some(v) => decode_entry(v.value(), expected_dim),
                None => None,
            });
        }
        Ok(out)
    }

    /// Insert `inserts` and refresh the LRU position of `touches`, then evict
    /// down to the ceiling — all in one write transaction.
    ///
    /// Why: one transaction is what makes the byte total trustworthy. If the
    /// insert committed and the accounting did not, the file would grow past
    /// the ceiling forever with no way to notice.
    /// What: assigns each affected key a fresh ascending sequence, removes the
    /// stale LRU rows, adds the new entries' cost to the running total, and
    /// then evicts oldest-first until the total is back under `max_bytes`.
    /// Returns the number of entries evicted.
    /// Test: `write_batch_inserts_and_touches`, `eviction_drops_oldest_first`,
    /// `ceiling_is_respected_across_batches`.
    pub(crate) fn write_batch(
        &self,
        inserts: &[(CacheKey, Vec<f32>)],
        touches: &[(CacheKey, u64)],
    ) -> Result<usize> {
        if inserts.is_empty() && touches.is_empty() {
            return Ok(0);
        }
        let txn = self.db.begin_write()?;
        let evicted;
        {
            let mut entries = txn.open_table(ENTRIES)?;
            let mut lru = txn.open_table(LRU)?;
            let mut meta = txn.open_table(META)?;

            let mut next_seq = meta.get(META_NEXT_SEQ)?.map(|v| v.value()).unwrap_or(0);
            let mut total = meta.get(META_TOTAL_BYTES)?.map(|v| v.value()).unwrap_or(0);

            // Refresh existing entries first — a touch never changes the total.
            for (key, old_seq) in touches {
                let Some(existing) = entries.get(key.as_bytes())?.map(|v| v.value().to_vec())
                else {
                    // Evicted between the read and this write. Nothing to
                    // refresh; the next reindex re-embeds it.
                    continue;
                };
                lru.remove(*old_seq)?;
                let seq = next_seq;
                next_seq += 1;
                let mut value = existing;
                value[..8].copy_from_slice(&seq.to_le_bytes());
                entries.insert(key.as_bytes(), value.as_slice())?;
                lru.insert(seq, key.as_bytes())?;
            }

            for (key, vector) in inserts {
                let seq = next_seq;
                next_seq += 1;
                // Replacing an existing key would double-count its cost, so
                // retire the old row's accounting first.
                if let Some(old) = entries.get(key.as_bytes())?.map(|v| v.value().to_vec()) {
                    let old_seq = u64::from_le_bytes(
                        old[..8].try_into().unwrap_or_else(|_| [0u8; 8]),
                    );
                    lru.remove(old_seq)?;
                    total = total.saturating_sub(entry_cost((old.len() - 8) / 4));
                }
                entries.insert(key.as_bytes(), encode_entry(seq, vector).as_slice())?;
                lru.insert(seq, key.as_bytes())?;
                total = total.saturating_add(entry_cost(vector.len()));
            }

            evicted = evict_to_ceiling(&mut entries, &mut lru, &mut total, self.max_bytes)?;

            meta.insert(META_NEXT_SEQ, next_seq)?;
            meta.insert(META_TOTAL_BYTES, total)?;
        }
        txn.commit()?;
        Ok(evicted)
    }

    /// Current accounted size, in bytes. Test/observability only.
    pub(crate) fn total_bytes(&self) -> Result<u64> {
        let txn = self.db.begin_read()?;
        let meta = txn.open_table(META)?;
        Ok(meta.get(META_TOTAL_BYTES)?.map(|v| v.value()).unwrap_or(0))
    }

    /// Number of live entries. Test/observability only.
    pub(crate) fn len(&self) -> Result<u64> {
        let txn = self.db.begin_read()?;
        Ok(txn.open_table(ENTRIES)?.len()?)
    }
}

/// Drop oldest-first until `total` is at or under `max_bytes`.
///
/// Why: separated from `write_batch` so the eviction loop is testable against a
/// deliberately tiny ceiling without driving a whole reindex.
/// What: walks `lru` in ascending sequence order (ascending age), removing each
/// key from both tables and decrementing the running total. A `max_bytes` of 0
/// means "unbounded" and is handled by the caller before this is reached.
/// Test: `eviction_drops_oldest_first`.
fn evict_to_ceiling(
    entries: &mut redb::Table<&[u8], &[u8]>,
    lru: &mut redb::Table<u64, &[u8]>,
    total: &mut u64,
    max_bytes: u64,
) -> Result<usize> {
    if *total <= max_bytes {
        return Ok(0);
    }
    // Collect first: redb does not allow removing from a table while an
    // iterator over it is live.
    let mut victims: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut projected = *total;
    for row in lru.iter()? {
        let (seq, key) = row?;
        let key = key.value().to_vec();
        let cost = entries
            .get(key.as_slice())?
            .map(|v| entry_cost((v.value().len().saturating_sub(8)) / 4))
            .unwrap_or(0);
        victims.push((seq.value(), key));
        projected = projected.saturating_sub(cost);
        if projected <= max_bytes {
            break;
        }
    }
    for (seq, key) in &victims {
        entries.remove(key.as_slice())?;
        lru.remove(*seq)?;
    }
    *total = projected;
    Ok(victims.len())
}

/// Encode `[seq][vector]` for the entries table.
fn encode_entry(seq: u64, vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + vector.len() * 4);
    out.extend_from_slice(&seq.to_le_bytes());
    for f in vector {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Decode an entries-table value, rejecting anything that is not exactly
/// `expected_dim` f32 components.
///
/// Why: a length mismatch means the stored vector was produced under a
/// different configuration than the caller is asking for. Returning `None`
/// converts that into a re-embed instead of a silently wrong vector.
/// What: `None` on a short or misaligned buffer, or on a dimension mismatch.
/// Test: `wrong_dimension_entry_is_a_miss`, `truncated_entry_is_a_miss`.
fn decode_entry(raw: &[u8], expected_dim: usize) -> Option<Hit> {
    if raw.len() < 8 {
        return None;
    }
    let seq = u64::from_le_bytes(raw[..8].try_into().ok()?);
    let body = &raw[8..];
    if body.len() != expected_dim * 4 {
        return None;
    }
    let vector = body
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Some(Hit { vector, seq })
}

/// Expose [`decode_entry`] to the sibling test module so the malformed-value
/// arms can be exercised without writing corrupt bytes through redb.
#[cfg(test)]
pub(super) fn decode_entry_for_test(raw: &[u8], expected_dim: usize) -> Option<Hit> {
    decode_entry(raw, expected_dim)
}
