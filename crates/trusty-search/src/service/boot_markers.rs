//! Durable boot-integrity markers on `indexes.toml` (#4390, #4391).
//!
//! Why: the daemon could not tell that an index was incompletely built, so it
//! served partial results as if complete. Two markers were missing, in opposite
//! directions. #4391 is the DETECTION side — `indexed_head_sha` lived only in
//! memory and both restore paths re-derived it from live git, so
//! `reconcile_git_path` compared current HEAD against current HEAD and could
//! never see a commit that landed while the daemon was down. #4390 is the
//! COMPLETENESS side — a deferred-embed (C2) pass commits its vectors once at
//! the very end, so any stop mid-pass strands every chunk it was embedding with
//! nothing on disk recording that. Both are the same failure shape: state
//! advances, the failure leaves no trace, and the loss is invisible after
//! restart.
//!
//! What: one place that reads and writes the two `PersistedIndex` fields, plus
//! the restore-time decisions that consume them. Every write is best-effort and
//! logged rather than propagated — a marker is a recovery aid, and failing to
//! record one must never abort the reindex or restore that was otherwise
//! succeeding. The failure directions are deliberately asymmetric: see
//! [`persist_deferred_embed_pending`].
//!
//! Test: see the `tests` submodule below for the marker read/write contract
//! and re-arm decision, and `commands::start_restore`'s `markers_tests`
//! submodule for the end-to-end restore-path proof (#4390 / #4391).
//!
//! [`persist_deferred_embed_pending`]: crate::service::boot_markers::persist_deferred_embed_pending

use std::sync::Arc;

use crate::core::registry::IndexHandle;
use crate::service::persistence::{
    indexes_toml_path, patch_index_registry_entry_at, PersistedIndex,
};

/// Resolve the HEAD SHA a restored handle should carry (#4391).
///
/// Why: restore used to call `git::head_sha(root)` unconditionally, which made
/// the stored and current SHA equal by construction and left
/// `reconcile_git_path` unable to fire for any pre-restore drift. Preferring the
/// persisted stamp is the whole fix.
/// What: returns the persisted stamp when the entry has one. When it does not —
/// a legacy `indexes.toml` written before the field existed — falls back to live
/// git and BACKFILLS that value so the next boot has a genuine stored SHA. The
/// first boot after upgrade therefore behaves exactly as it did before: it
/// cannot detect down-time drift, because nothing was ever stored to compare
/// against. Backfilling from live git rather than triggering a reindex is what
/// keeps a 200-index fleet from re-walking every tree on one upgrade.
/// Test: `resolve_prefers_the_persisted_head_sha_over_live_git` and
/// `resolve_backfills_a_missing_head_sha_from_live_git`.
pub fn resolve_indexed_head_sha(entry: &PersistedIndex) -> Option<String> {
    if let Some(stored) = entry.indexed_head_sha.clone() {
        return Some(stored);
    }
    // #4391: legacy entry — adopt live HEAD once so later boots can compare.
    let live = crate::core::git::head_sha(&entry.root_path);
    if let Some(sha) = live.as_deref() {
        persist_indexed_head_sha(&entry.id, Some(sha));
    }
    live
}

/// Write the indexed HEAD SHA through to `indexes.toml` (#4391).
///
/// Why: the in-memory stamp is lost on every handle rebuild — restart, cold-park
/// reload, selective warm boot. Only the persisted copy survives those, and
/// surviving them is the point.
/// What: best-effort patch of the entry; a failure is logged at `warn` and
/// nothing is propagated. The cost of a lost write is one boot's worth of
/// missed drift detection, which is exactly the pre-fix behaviour — never a
/// corrupted index.
/// Test: `stamp_handle_persists_the_sha_it_writes` exercises this exact
/// write-through contract via `reconcile::stamp_handle`, its other caller.
pub fn persist_indexed_head_sha(index_id: &str, sha: Option<&str>) {
    let owned = sha.map(str::to_owned);
    patch_marker(index_id, "indexed_head_sha", move |entry| {
        entry.indexed_head_sha = owned;
    });
}

/// Record whether a deferred-embed (C2) pass is outstanding (#4390).
///
/// Why: C2 accumulates every embedding in memory and commits once at the end, so
/// a stop anywhere inside it — a crash, a routine upgrade, a clean
/// `trusty-search stop` — persists zero vectors and leaves the chunks it was
/// embedding with no vector at all. Nothing on disk recorded that the pass had
/// been owed, so warm boot read the pre-existing HNSW snapshot and reported
/// `semantic: ready`.
/// What: `true` is written before the pass is enqueued, `false` when it reaches
/// a terminal state. **The two failure directions are not symmetric and that is
/// the design.** A lost `true` costs only the detection this marker adds — the
/// index is no worse off than before the fix. A lost `false` costs one extra
/// catch-up pass on the next boot, and that pass is gap-filling by construction
/// (`embed_deferred_chunks` embeds only what the vector store reports missing).
/// So no write failure can produce a silently under-embedded index; the worst
/// case is redundant work.
/// Test: `interrupted_embed_pass_leaves_the_pending_marker_set`.
pub fn persist_deferred_embed_pending(index_id: &str, pending: bool) {
    patch_marker(index_id, "deferred_embed_pending", move |entry| {
        entry.deferred_embed_pending = pending;
    });
}

/// Apply one field patch to an `indexes.toml` entry, logging any failure.
///
/// Why: both markers want identical best-effort semantics, and neither caller is
/// in a position to do anything useful with an error — `finish_reindex` has
/// already committed the corpus, and restore has already registered the handle.
/// What: resolves the registry path and patches the entry under the registry
/// write lock. `patch_index_registry_entry_at` is a no-op when the entry is
/// absent, which is the correct reading of "the index was deregistered while
/// this pass ran".
fn patch_marker(index_id: &str, field: &'static str, patch: impl FnOnce(&mut PersistedIndex)) {
    let outcome =
        indexes_toml_path().and_then(|path| patch_index_registry_entry_at(&path, index_id, patch));
    if let Err(e) = outcome {
        tracing::warn!(
            "boot_markers[{index_id}]: could not persist {field} — {e:#}. The marker is a \
             recovery aid, so this is not fatal; see #4390 / #4391 for what the loss costs."
        );
    }
}

/// Whether a restored index should have its deferred-embed catch-up re-armed
/// (#4390).
///
/// Why: re-arming is only meaningful when the vector lane is supposed to exist.
/// An index built `skip_vector` or `lexical_only` must never have the embedder
/// run for it, and an index restored on a daemon with no embedder wired cannot
/// embed anything — queueing a pass in either case would burn the single
/// background permit on work that provably cannot progress.
/// What: pure decision, so the restore paths and the tests agree on one rule.
/// Test: `rearm_decision_respects_skip_vector_and_embedder_presence`.
pub fn should_rearm_deferred_embed(
    pending: bool,
    skip_vector: bool,
    lexical_only: bool,
    has_embedder: bool,
) -> bool {
    pending && !skip_vector && !lexical_only && has_embedder
}

/// Re-arm the deferred-embed catch-up pass for a freshly restored index (#4390).
///
/// Why: this is the automatic trigger the issue says does not exist. Without it
/// a stranded C2 pass waits for an unrelated reindex — hours on an active repo,
/// indefinitely on a dormant one — while every surface reports the index
/// healthy.
/// What: consults [`should_rearm_deferred_embed`], then enqueues through the
/// same size-ordered queue `finish_reindex` uses, so boot re-arms serialise
/// behind the one background permit and drain smallest-first rather than
/// stampeding. `total_chunks` is the corpus count restore already read from redb
/// metadata — an upper bound on the pending set, and deliberately NOT
/// `pending_embed_count()`, whose exact answer costs a full chunk-map
/// rehydration (27–40 s on a 315K-chunk corpus, #3683) that selective warm boot
/// exists to avoid.
///
/// A `skip_vector` / `lexical_only` index that still carries the marker has it
/// cleared here: the component was disabled after the pass was queued, so the
/// pass is owed to nobody and the marker would otherwise re-fire on every boot
/// forever. An index with no embedder KEEPS its marker — the embedder may be
/// wired on a later boot, and the work is still genuinely owed.
/// Test: `restore_rearms_an_interrupted_deferred_embed_pass`,
/// `restore_drops_the_marker_for_a_skip_vector_index`.
pub async fn rearm_deferred_embed_if_pending(
    handle: &Arc<IndexHandle>,
    pending: bool,
    total_chunks: usize,
) {
    if !pending {
        return;
    }
    let index_id = handle.id.0.clone();
    if handle.skip_vector || handle.lexical_only {
        // #4390: the vector lane was disabled after the pass was queued — the
        // work is owed to nobody, so drop the marker instead of re-firing it.
        persist_deferred_embed_pending(&index_id, false);
        return;
    }
    let has_embedder = handle.indexer.read().await.has_embedder();
    if !should_rearm_deferred_embed(pending, false, false, has_embedder) {
        tracing::warn!(
            "boot_markers[{index_id}]: a deferred-embed pass was interrupted, but this daemon \
             has no embedder wired — leaving the marker set so a later boot re-arms it (#4390)"
        );
        return;
    }
    tracing::warn!(
        "boot_markers[{index_id}]: a deferred-embed pass was interrupted before it committed \
         — re-arming the catch-up over {total_chunks} corpus chunks. Until it finishes the \
         semantic lane is short the chunks that pass was embedding (#4390)"
    );
    crate::service::reindex::spawn_deferred_embed_pass(
        Arc::clone(handle),
        Arc::new(crate::service::reindex::ReindexProgress::new()),
        total_chunks,
    );
}

#[cfg(test)]
#[path = "boot_markers_tests.rs"]
mod tests;
