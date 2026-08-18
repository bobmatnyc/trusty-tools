//! JSONL → palace import, idempotent by content hash (#5902).
//!
//! Why: this is the half that makes convergence real. `trusty-agents`' importer
//! keys each incoming record as `imported:{machine_id}:{id}`, so the same fact
//! from two machines stays two records forever — the exact defect this replaces.
//! Keying on the content hash instead means importing the same file twice is a
//! no-op, importing a superset adds only what is new, and two machines' exports
//! that overlap converge on one memory.
//! What: [`import_palace_records`] (the decided merge, against an open palace)
//! and [`import_palace_jsonl`] (read a file, then that).
//! Test: `import_is_idempotent`, `import_of_a_superset_adds_only_the_new`,
//! `two_machines_converge_on_one_memory`, `merge_keeps_the_earlier_created_at_in_either_order`,
//! `merge_keeps_the_earlier_created_at_in_either_order` in `share::tests`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::record::SharedMemoryRecord;
use crate::memory_core::content_hash::ContentHash;
use crate::memory_core::embed::Embedder;
use crate::memory_core::palace::{Drawer, RoomType};
use crate::memory_core::retrieval::{PalaceHandle, shared_embedder};
use crate::memory_core::room_identity::DEFAULT_WING_ID;
use crate::memory_core::store::l1_cache::L1Cache;
use crate::memory_core::store::rooms::resolve_or_create_room_in_wing;
use crate::memory_core::store::vector::VectorStore;
use crate::memory_core::timeouts;

/// What one record's import did.
///
/// Why: a caller reporting "imported 40 memories" when 38 were already present
/// has told the user nothing. The three outcomes need different words, and a
/// skipped line is a fact the operator has to see.
/// Test: `import_is_idempotent`, `import_of_a_superset_adds_only_the_new`,
/// `import_skips_a_bad_line_and_keeps_the_rest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportOutcome {
    /// The hash was absent locally; a new drawer was written.
    Inserted,
    /// The hash was already present and the merge changed something (an earlier
    /// `created_at`, or a tag the local copy lacked).
    Merged,
    /// The hash was already present and the local copy already dominated it.
    Unchanged,
    /// The record could not be trusted or could not be written; nothing changed.
    Skipped,
}

/// Tally of an import run.
///
/// Test: `import_is_idempotent`, `import_of_a_superset_adds_only_the_new`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub inserted: usize,
    pub merged: usize,
    pub unchanged: usize,
    pub skipped: usize,
}

impl ImportSummary {
    fn record(&mut self, outcome: ImportOutcome) {
        match outcome {
            ImportOutcome::Inserted => self.inserted += 1,
            ImportOutcome::Merged => self.merged += 1,
            ImportOutcome::Unchanged => self.unchanged += 1,
            ImportOutcome::Skipped => self.skipped += 1,
        }
    }

    /// Whether this run changed the palace at all.
    ///
    /// Why: the idempotency property is stated in these terms — a second import
    /// of the same file must leave the palace untouched — so the predicate the
    /// tests assert on belongs here rather than being re-derived per call site.
    /// Test: `import_is_idempotent`.
    pub fn changed_anything(&self) -> bool {
        self.inserted > 0 || self.merged > 0
    }
}

/// Read `path` as JSONL and import every record it holds.
///
/// Why line-at-a-time rather than parse-then-apply: a file pulled from git can
/// have one bad line — a conflict marker a merge left behind, a truncated last
/// record — and refusing the whole file for it would strand every good memory in
/// it. Each line is independent, so each failure is contained and counted.
/// What: skips blank lines, counts an unparseable or unverifiable line as
/// [`ImportOutcome::Skipped`] with a warning naming the line number, and applies
/// the rest through [`import_palace_records`]. Per-line containment covers the
/// FILE's failures only — an embedder that cannot be resolved aborts the whole
/// run with `Err` and no summary, per [`import_palace_records`].
/// Test: `export_then_import_preserves_metadata`, `import_skips_a_bad_line_and_keeps_the_rest`.
pub async fn import_palace_jsonl(handle: &PalaceHandle, path: &Path) -> Result<ImportSummary> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read the export at {}", path.display()))?;

    let mut records = Vec::new();
    let mut summary = ImportSummary::default();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<SharedMemoryRecord>(line) {
            Ok(r) => records.push(r),
            Err(e) => {
                tracing::warn!(
                    line = i + 1,
                    file = %path.display(),
                    "#5902 import: skipping unparseable JSONL line: {e}"
                );
                summary.skipped += 1;
            }
        }
    }

    let applied = import_palace_records(handle, &records).await?;
    summary.inserted += applied.inserted;
    summary.merged += applied.merged;
    summary.unchanged += applied.unchanged;
    summary.skipped += applied.skipped;
    Ok(summary)
}

/// Import `records` into `handle`, keyed on content hash.
///
/// Why the whole run holds the per-palace write mutex: the sequence per record is
/// read-the-table, decide, write. Without the mutex a concurrent `remember` of
/// the same body could interleave between the decision and the write, and the
/// import would insert a duplicate of a fact that arrived a microsecond earlier —
/// reintroducing, through a different door, exactly the duplication this exists to
/// remove. The mutex is the same one `remember_with_options` takes (#154).
///
/// Why merge keeps the EARLIER `created_at` (decided): a colliding hash means the
/// bodies are identical, so nothing about the fact itself is in dispute and the
/// merge discards almost nothing — the palace has no author, machine, or session
/// field for the two copies to disagree about. What must not happen is the
/// timestamp regressing to whichever import ran last: `created_at` feeds temporal
/// decay and the `drawer_listing_order` recency tie-break, so re-importing an old
/// shared file would silently promote every fact in it to "written today". Tags
/// union, and importance takes the maximum, on the same reasoning: a merge may add
/// information, never remove it.
///
/// 🔴 An embedder that cannot be RESOLVED aborts the whole run (#5902). This
/// function resolves `shared_embedder()` once, before the first record, and
/// returns `Err` if that fails — no records are examined and no summary is
/// produced. That is a behaviour change from resolving inside `insert_new`,
/// where each record resolved for itself, so a failure was caught per record,
/// counted [`ImportOutcome::Skipped`], and the next record tried again. Both
/// forms fail closed — nothing is written either way — but the failure now
/// surfaces as an error rather than a summary of N skips, and it surfaces once
/// instead of after N embed timeouts. A per-record embed that FAILS or times out
/// is unchanged: still skipped, still counted, and the run continues (see
/// [`insert_new`]).
///
/// What: for each verified record, looks its hash up in the in-memory drawer
/// table. Absent → embed, upsert the vector, persist the drawer with the record's
/// own `created_at` and metadata, push it into the table. Present → apply the
/// merge above and re-persist only if something changed. Refreshes the L1 snapshot
/// and the closet index once at the end rather than per record.
/// Test: `import_is_idempotent`, `import_of_a_superset_adds_only_the_new`,
/// `two_machines_converge_on_one_memory`, `merge_keeps_the_earlier_created_at_in_either_order`,
/// `merge_keeps_the_earlier_created_at_in_either_order`,
/// `import_preserves_the_record_created_at_on_insert`.
pub async fn import_palace_records(
    handle: &PalaceHandle,
    records: &[SharedMemoryRecord],
) -> Result<ImportSummary> {
    if records.is_empty() {
        return Ok(ImportSummary::default());
    }
    // #5902: resolved once for the whole run, not once per inserted record, and
    // resolved HERE rather than inside `insert_new` so a test can supply its own
    // (see `import_palace_records_with_embedder`). That move also changed what a
    // resolution failure does — see this function's doc.
    let embedder = shared_embedder()
        .await
        .context("acquire the shared embedder for import")?;
    import_palace_records_with_embedder(handle, records, &embedder).await
}

/// [`import_palace_records`] against a caller-supplied embedder.
///
/// Why this seam exists (#5902): [`insert_new`] embeds BEFORE it writes, and its
/// failure must abort the insert with nothing written. That claim was correct by
/// inspection but unprovable by test while the embedder came from the
/// process-wide `shared_embedder()` `OnceCell` — a cell that can only be seeded
/// once per test binary, and is seeded there with the always-succeeding
/// `MockEmbedder`. Passing the embedder in is the same seam
/// `retrieval::deferred_embed::embed_and_store` already uses for the same reason,
/// and it lets a failing double prove the ordering rather than assert it.
/// What: identical to [`import_palace_records`] except that the embedder is a
/// parameter. Not public API — the shared embedder is the only correct choice in
/// production.
/// Test: `an_embedder_failure_during_import_writes_nothing_durably`, plus every
/// test that reaches this through [`import_palace_records`].
pub(crate) async fn import_palace_records_with_embedder(
    handle: &PalaceHandle,
    records: &[SharedMemoryRecord],
    embedder: &Arc<dyn Embedder + Send + Sync>,
) -> Result<ImportSummary> {
    let mut summary = ImportSummary::default();
    if records.is_empty() {
        return Ok(summary);
    }

    handle.touch();
    if handle.is_read_only() {
        return Err(anyhow::anyhow!(
            "palace '{}' is read-only: the HTTP daemon holds the write lock, so an \
             import cannot proceed — route it through the daemon or stop it first",
            handle.id
        ));
    }

    // #154 / #5902: one guard for the whole run. See the doc comment above for
    // why the read-decide-write sequence cannot be interleaved.
    let _write_guard = timeouts::lock_with_timeout(
        &handle.write_mutex,
        timeouts::write_lock_timeout(),
        handle.id.as_str(),
    )
    .await?;

    let mut changed = false;
    for rec in records {
        match apply_one(handle, rec, embedder).await {
            Ok(outcome) => {
                if matches!(outcome, ImportOutcome::Inserted | ImportOutcome::Merged) {
                    changed = true;
                }
                summary.record(outcome);
            }
            Err(e) => {
                tracing::warn!(
                    palace = %handle.id,
                    hash = %rec.content_hash,
                    "#5902 import: skipping a record: {e:#}"
                );
                summary.record(ImportOutcome::Skipped);
            }
        }
    }

    if changed {
        if let Some(data_dir) = handle.data_dir.as_ref() {
            let snap = handle.drawers.read().clone();
            L1Cache::save_l1_cache(&snap, data_dir).context("save the L1 snapshot after import")?;
        }
        handle.rebuild_closets();
    }
    Ok(summary)
}

/// Apply one verified record. Caller holds the write guard.
///
/// Why it takes the whole decision rather than being split further: the "does
/// this hash exist locally" read and the write that depends on its answer must
/// stay adjacent under one guard, and splitting them across functions is how they
/// drift apart.
/// What: verifies, then inserts or merges as described on
/// [`import_palace_records`].
/// Test: as [`import_palace_records`].
async fn apply_one(
    handle: &PalaceHandle,
    rec: &SharedMemoryRecord,
    embedder: &Arc<dyn Embedder + Send + Sync>,
) -> Result<ImportOutcome> {
    // A record whose declared digest does not describe its body would enter the
    // palace under an identity no other machine can reproduce.
    let hash = rec.verify().context("verify the record")?;

    let existing = find_by_hash(handle, hash);
    match existing {
        Some((id, local_created_at, local_tags, local_importance)) => {
            merge_into_existing(
                handle,
                rec,
                id,
                local_created_at,
                local_tags,
                local_importance,
            )
            .await
        }
        None => insert_new(handle, rec, embedder)
            .await
            .map(|_| ImportOutcome::Inserted),
    }
}

/// The local drawer holding `hash`, if any, with the fields the merge reads.
///
/// Why it returns copies rather than the `Drawer`: the read lock must not be held
/// across the `.await` points the merge and insert paths contain.
/// What: first match wins. Duplicate hashes inside one palace are possible for
/// drawers written before this field existed; the earliest-created wins so the
/// choice is stable across runs rather than depending on table order.
/// Test: `merge_keeps_the_earlier_created_at_in_either_order`.
fn find_by_hash(
    handle: &PalaceHandle,
    hash: ContentHash,
) -> Option<(Uuid, DateTime<Utc>, Vec<String>, f32)> {
    handle
        .drawers
        .read()
        .iter()
        .filter(|d| d.content_hash() == hash)
        .min_by_key(|d| (d.created_at, d.id))
        .map(|d| (d.id, d.created_at, d.tags.clone(), d.importance))
}

/// Merge an incoming record into the local drawer that already holds its hash.
///
/// What: earlier `created_at` wins, tags union, importance takes the maximum. Any
/// change is persisted to redb and mirrored into the in-memory table; no change is
/// [`ImportOutcome::Unchanged`] and touches nothing. The vector is untouched —
/// the body is identical by definition, so the existing embedding is still
/// correct.
/// Test: `merge_keeps_the_earlier_created_at_in_either_order`,
/// `merge_keeps_the_earlier_created_at_in_either_order`,
/// `merge_unions_tags_and_takes_the_higher_importance`, `import_is_idempotent`.
async fn merge_into_existing(
    handle: &PalaceHandle,
    rec: &SharedMemoryRecord,
    id: Uuid,
    local_created_at: DateTime<Utc>,
    local_tags: Vec<String>,
    local_importance: f32,
) -> Result<ImportOutcome> {
    let earlier = local_created_at.min(rec.created_at);
    let mut tags = local_tags.clone();
    for t in &rec.tags {
        if !tags.contains(t) {
            tags.push(t.clone());
        }
    }
    let importance = local_importance.max(rec.importance.clamp(0.0, 1.0));

    let unchanged = earlier == local_created_at
        && tags.len() == local_tags.len()
        && (importance - local_importance).abs() < f32::EPSILON;
    if unchanged {
        return Ok(ImportOutcome::Unchanged);
    }

    // Take the drawer, mutate the copy, then write: redb first so a crash cannot
    // leave the in-memory table ahead of the durable row.
    let Some(mut updated) = handle.drawers.read().iter().find(|d| d.id == id).cloned() else {
        // Raced away between `find_by_hash` and here. Nothing to merge into, and
        // inserting instead would be a decision this function was not asked to
        // make.
        return Ok(ImportOutcome::Skipped);
    };
    updated.created_at = earlier;
    updated.tags = tags;
    updated.importance = importance;

    handle
        .kg
        .upsert_drawer(&updated)
        .await
        .context("persist the merged drawer")?;
    {
        let mut drawers = handle.drawers.write();
        for d in drawers.iter_mut().filter(|d| d.id == id) {
            *d = updated.clone();
        }
    }
    Ok(ImportOutcome::Merged)
}

/// Write a record the palace has never seen as a new drawer.
///
/// 🔴 The embed runs BEFORE the durable write, and its failure aborts the whole
/// insert. That ordering is the contract, not an accident. The export carries no
/// embedding vector by owner decision (see [`super::record::SharedMemoryRecord`]),
/// so import is the only place this memory's vector can come from. If the embedder
/// is cold past its timeout or errors, the record is counted
/// [`ImportOutcome::Skipped`] and NOTHING is written — no drawer row, no in-memory
/// entry. (An embedder that cannot be resolved AT ALL fails the run before this
/// point; see [`import_palace_records`].) That ordering is what
/// `an_embedder_failure_during_import_writes_nothing_durably` pins: it drives a
/// real import through an embedder that always errors and asserts both the
/// in-memory table and `kg.load_drawers()` are still empty, so reordering the
/// embed after the write turns the test red.
/// An import that wrote the drawer and let the embed fail would leave a
/// memory that is durable and permanently unfindable, which is the failure mode
/// #4906 spent a whole lane removing from the write path. A slow import is the
/// accepted cost of excluding vectors; a silently unsearchable one is not. The
/// deferred-embed lane is deliberately NOT reached for here: it exists so an
/// interactive write need not block on a cold model, and an import has no user
/// waiting on it.
///
/// Why it does not route through `PalaceHandle::remember_with_options`: that path
/// stamps `created_at = now` and re-runs the quality filter. Both are wrong here.
/// The timestamp is the imported fact's own, and it is what the earliest-wins
/// merge rule depends on; re-filtering would reject a memory the ORIGINATING
/// machine already accepted, which would make an import's result depend on which
/// machine's filter version ran last. What this path must NOT skip is the secret
/// gate — see the note on `export_palace_records`; nothing screens content on
/// either side of this file today, and PR 2's commit gate is where that lands.
///
/// Why it re-embeds rather than carrying a vector: see
/// [`super::record::SharedMemoryRecord`].
/// What: resolves the room label to an id (minting the registry row when this
/// palace has not seen that room), embeds the body under the same bounded timeout
/// the write path uses, upserts the vector, persists the drawer, and pushes it
/// into the in-memory table.
/// Test: `import_preserves_the_record_created_at_on_insert`,
/// `export_then_import_preserves_metadata`, `imported_memory_is_recallable`,
/// `an_embedder_failure_during_import_writes_nothing_durably`.
async fn insert_new(
    handle: &PalaceHandle,
    rec: &SharedMemoryRecord,
    embedder: &Arc<dyn Embedder + Send + Sync>,
) -> Result<Uuid> {
    let room = RoomType::parse(&rec.room);
    let room_id = resolve_or_create_room_in_wing(&handle.kg, &room, DEFAULT_WING_ID).await;

    let mut drawer = Drawer::new(room_id, rec.content.clone());
    drawer.created_at = rec.created_at;
    drawer.tags = rec.tags.clone();
    drawer.importance = rec.importance.clamp(0.0, 1.0);
    drawer.drawer_type = rec.parsed_drawer_type();
    // The imported fact's TTL and slot are the sender's local state, not
    // portable claims — see `export_palace_records`, which excludes Tier C
    // drawers outright.
    drawer.expires_at = None;
    drawer.fact_key = None;
    let id = drawer.id;

    let embed_timeout = timeouts::embed_batch_timeout();
    let vecs = tokio::time::timeout(
        embed_timeout,
        embedder.embed_batch(std::slice::from_ref(&rec.content)),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "embed_batch timed out after {embed_timeout:?} while importing a shared \
             memory; raise TRUSTY_EMBED_BATCH_TIMEOUT_SECS if batches legitimately \
             take longer on this host"
        )
    })?
    .context("embed the imported content")?;
    if let Some(v) = vecs.into_iter().next() {
        handle
            .vector_store
            .upsert(id, v)
            .await
            .context("upsert the imported drawer's vector")?;
    }

    handle
        .kg
        .upsert_drawer(&drawer)
        .await
        .context("persist the imported drawer")?;
    handle.drawers.write().push(drawer);
    Ok(id)
}

/// Records grouped by hash, for a caller assembling a file from several palaces.
///
/// Why: PR 2's commit flow reads back what is already committed, merges the local
/// export into it, and writes one file. Doing that on the record level — before
/// any palace is touched — means the committed file converges by the same rule the
/// palace does, rather than by whatever order the writes happened to land.
/// What: earliest `created_at` wins, tags union, importance takes the maximum —
/// the same merge as [`merge_into_existing`], on records rather than drawers.
/// Records that fail [`SharedMemoryRecord::verify`] are dropped.
/// Test: `merge_records_converges_two_machines_exports`.
pub fn merge_records(sets: &[&[SharedMemoryRecord]]) -> Vec<SharedMemoryRecord> {
    let mut by_hash: HashMap<ContentHash, SharedMemoryRecord> = HashMap::new();
    for set in sets {
        for rec in set.iter() {
            if rec.verify().is_err() {
                continue;
            }
            match by_hash.get_mut(&rec.content_hash) {
                None => {
                    by_hash.insert(rec.content_hash, rec.clone());
                }
                Some(kept) => {
                    kept.created_at = kept.created_at.min(rec.created_at);
                    for t in &rec.tags {
                        if !kept.tags.contains(t) {
                            kept.tags.push(t.clone());
                        }
                    }
                    kept.importance = kept.importance.max(rec.importance);
                }
            }
        }
    }
    let mut out: Vec<SharedMemoryRecord> = by_hash.into_values().collect();
    out.sort_by(|a, b| {
        a.content_hash
            .cmp(&b.content_hash)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });
    out
}
