//! KnowledgeGraph storage delegation and utility impl methods.
//!
//! Why: Extracted from store/kg.rs to keep each file under the 500-SLOC cap
//! (#607). Contains all methods that delegate to `KgStoreRedb` or `KgWriter`
//! for storage operations (query, list, drawer ops, cascade delete, sync ops).
//! What: `KnowledgeGraph` impl for `query_active`, `list_subjects`,
//! `list_subjects_with_counts`, `list_active`, `count_active_triples`,
//! `node_count`, `edge_count`, `community_count`, `checkpoint`,
//! `upsert_drawer`, `delete_drawer`, `delete_drawer_sync`, `load_drawer_ids`,
//! `load_drawers`, `knowledge_gaps`, `is_read_only`,
//! `cascade_delete_by_drawer`, `retract_triple`, `assert_sync`,
//! `upsert_drawer_sync`, `store`, `dump_all_triples`.
//! Test: See kg/tests.rs for comprehensive storage round-trip tests.

use super::graph::KnowledgeGraph;
use super::types::Triple;
use crate::memory_core::palace::Drawer;
use crate::memory_core::store::kg_redb::KgStoreRedb;
use anyhow::{Context, Result};
use petgraph::visit::EdgeRef;
use uuid::Uuid;

impl KnowledgeGraph {
    /// Return all currently active triples (`valid_to is None`) for `subject`.
    ///
    /// Why: Most queries want "what is true *now*".
    /// What: Delegates to `KgStoreRedb::query_active` on the blocking pool.
    /// Test: `assert_then_query_active_returns_fact`.
    pub async fn query_active(&self, subject: &str) -> Result<Vec<Triple>> {
        let store = self.store.clone();
        let subject = subject.to_string();
        let triples = tokio::task::spawn_blocking(move || store.query_active(&subject))
            .await
            .context("query_active spawn_blocking join error")??;
        Ok(triples)
    }

    /// List up to `limit` distinct subjects with at least one active triple.
    ///
    /// Why: KG Explorer UI browses subjects without knowing one upfront.
    /// What: Delegates to `KgStoreRedb::list_subjects` synchronously.
    /// Test: `list_subjects_returns_distinct_active_subjects`.
    pub fn list_subjects(&self, limit: usize) -> Result<Vec<String>> {
        self.store.list_subjects(limit)
    }

    /// List up to `limit` `(subject, active_count)` rows.
    ///
    /// Why: KG Explorer UI shows a triple-count badge next to each subject.
    /// What: Delegates to `KgStoreRedb::list_subjects_with_counts`.
    /// Test: `list_subjects_with_counts_returns_grouped_counts`.
    pub fn list_subjects_with_counts(&self, limit: usize) -> Result<Vec<(String, u64)>> {
        self.store.list_subjects_with_counts(limit)
    }

    /// List up to `limit` active triples ordered by `valid_from` desc.
    ///
    /// Why: KG Explorer "All" mode pages through every active triple.
    /// What: Delegates to `KgStoreRedb::list_active` on the blocking pool.
    /// Test: `list_active_returns_ordered_window`.
    pub async fn list_active(&self, limit: usize, offset: usize) -> Result<Vec<Triple>> {
        let store = self.store.clone();
        let triples = tokio::task::spawn_blocking(move || store.list_active(limit, offset))
            .await
            .context("list_active spawn_blocking join error")??;
        Ok(triples)
    }

    /// Count currently active triples.
    ///
    /// Why: Dashboard tally of live facts. Fallible since #5384 — swallowing
    /// the store's read error here would hand every caller a `0` that reads
    /// as "the graph is empty".
    /// What: Delegates to `KgStoreRedb::count_active_triples` and clamps the
    /// u64 to `usize`, which is what the existing callers consume.
    /// Test: `count_active_triples_returns_live_only`,
    /// `count_active_triples_surfaces_read_failure`.
    pub fn count_active_triples(&self) -> Result<usize> {
        let n = self.store.count_active_triples()?;
        Ok(usize::try_from(n).unwrap_or(usize::MAX))
    }

    /// Number of distinct entities (nodes) in the in-memory adjacency.
    ///
    /// Why: Per-palace dashboards want a node tally alongside the active
    /// triple count to gauge graph breadth (many subjects ↔ many facts about
    /// one). The adjacency is the authoritative node set for graph
    /// operations because triples are deduplicated by `(subject, object)`
    /// edges and entities can appear as either endpoint.
    /// What: Acquires the adjacency read lock and returns
    /// `StableGraph::node_count()`. Returns `0` if the lock is poisoned —
    /// node counts are diagnostic, not critical, so we degrade gracefully
    /// rather than propagating the error.
    /// Test: `kg_graph_tests::node_and_edge_count_match_adjacency`.
    pub fn node_count(&self) -> usize {
        match self.adj.read() {
            Ok(adj) => adj.graph.node_count(),
            Err(_) => 0,
        }
    }

    /// Number of directed edges in the in-memory adjacency.
    ///
    /// Why: Companion to `node_count` for dashboards that surface graph
    /// density at a glance. Counted from the adjacency (not the redb
    /// triple table) because parallel edges between the same pair of nodes
    /// collapse into one petgraph edge; the adjacency view is what every
    /// graph algorithm (BFS, A*, Louvain) sees.
    /// What: Acquires the adjacency read lock and returns
    /// `StableGraph::edge_count()`. Returns `0` on a poisoned lock.
    /// Test: `kg_graph_tests::node_and_edge_count_match_adjacency`.
    pub fn edge_count(&self) -> usize {
        match self.adj.read() {
            Ok(adj) => adj.graph.edge_count(),
            Err(_) => 0,
        }
    }

    /// Number of Louvain communities detected in the active graph.
    ///
    /// Why: The MEMORY tab in the operator TUI shows a community tally per
    /// palace so operators can see clustering at a glance. Centralising the
    /// call here avoids the TUI importing the `community` module directly.
    /// What: Delegates to `community::partition(self)` and returns the
    /// number of non-empty partition groups. Returns `0` for an empty
    /// graph or when the adjacency snapshot fails (the partition function
    /// itself returns an empty vec in those cases).
    /// Test: `kg_graph_tests::community_count_returns_partition_size`.
    pub fn community_count(&self) -> usize {
        crate::memory_core::community::partition(self)
            .iter()
            .filter(|c| !c.is_empty())
            .count()
    }

    /// The underlying redb store, for the #6652 compaction path.
    ///
    /// Why: copy-then-swap needs the store's file path and its shared
    /// `KgDbState`, and both are below the `KnowledgeGraph` facade. Exposing a
    /// borrow rather than a clone keeps the caller from accidentally holding a
    /// second handle across the swap.
    /// What: `&self.store`.
    /// Test: `compaction_swaps_the_live_handle_in_place`.
    pub fn redb_store(&self) -> &crate::memory_core::store::kg_redb::KgStoreRedb {
        &self.store
    }

    /// Compatibility shim for the old WAL checkpoint API.
    ///
    /// Why: The Dreamer cycle called this to bound SQLite's WAL. redb manages
    /// its own write log internally, so there is nothing to do; we return
    /// `(0, 0)` to preserve the tuple shape callers expect.
    /// What: Delegates to `KgStoreRedb::checkpoint` (a no-op) and returns the
    /// (wal_pages, checkpointed_pages) tuple as `(0, 0)`.
    /// Test: `wal_checkpoint_returns_pages`.
    pub fn checkpoint(&self) -> Result<(i64, i64)> {
        self.store.checkpoint()?;
        Ok((0, 0))
    }

    /// Persist a drawer's metadata. See [`KgStoreRedb::upsert_drawer`].
    ///
    /// Why: HNSW only stores vectors; without the metadata persisted
    /// alongside, drawers cannot be reconstructed after restart. Routing
    /// through the coalescing writer means a `remember` burst (which calls
    /// `upsert_drawer` per drawer) shares a single redb commit with any
    /// concurrent `kg_assert` ops in the same window.
    /// What: Forwards to `KgWriter::upsert_drawer`, which queues the op,
    /// awaits the batched commit, and reports errors.
    /// Test: `upsert_drawer_then_load_drawers_round_trips`.
    pub async fn upsert_drawer(&self, drawer: &Drawer) -> Result<()> {
        self.writer.upsert_drawer(drawer.clone()).await
    }

    /// Persist several drawers inside ONE redb write transaction (#4886).
    ///
    /// Why: ADR-0028 D5's retire-on-write must be indivisible. The single-op
    /// `upsert_drawer` path routes through the coalescing writer actor, which
    /// batches opportunistically — it may or may not put two queued upserts in
    /// the same commit, and "may" is not a guarantee an invariant can rest on.
    /// A crash landing between the incumbent's retirement and the replacement's
    /// arrival would leave a slot whose occupant was retired with nothing
    /// taking its place, which is strictly worse than the stale fact the tier
    /// exists to retire. Going straight to `KgStoreRedb::apply_batch` makes the
    /// pair atomic by construction rather than by scheduling luck.
    /// What: maps each drawer to a `BatchWriteOp::UpsertDrawer` — preserving
    /// the caller's order, which the `DRAWERS_BY_FACT_KEY` maintenance in
    /// `batch_upsert_drawer` depends on — and applies them in one transaction
    /// on the blocking pool. Rolls the whole batch back on any per-op error.
    /// Read-only handles are rejected by `apply_batch`'s own writability check.
    /// Test: `tier_c_write_retires_the_prior_slot_occupant`,
    /// `concurrent_tier_c_writes_to_one_slot_leave_exactly_one_claimant`.
    pub async fn upsert_drawers_atomic(&self, drawers: Vec<Drawer>) -> Result<()> {
        use crate::memory_core::store::kg_redb::BatchWriteOp;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let ops: Vec<BatchWriteOp> = drawers
                .into_iter()
                .map(BatchWriteOp::UpsertDrawer)
                .collect();
            store.apply_batch(&ops).map(|_| ())
        })
        .await
        .context("upsert_drawers_atomic spawn_blocking join error")?
    }

    /// Which drawer, if any, currently occupies the `fact_key` slot (#4884).
    ///
    /// Why: the ADR-0028 D5 write path asks "who holds this slot?" exactly once
    /// per Tier C write; answering it from the secondary index is a point
    /// lookup rather than a decode of every drawer row in the palace.
    /// What: forwards to `KgStoreRedb::drawer_id_for_fact_key`. Synchronous —
    /// it is a single redb read transaction, and the Tier C write path already
    /// holds the per-palace write mutex when it calls this, so there is nothing
    /// to yield to.
    /// Test: `tier_c_write_retires_the_prior_slot_occupant`,
    /// `fact_key_index_tracks_upsert_and_delete`.
    pub fn drawer_id_for_fact_key(&self, fact_key: &str) -> Result<Option<Uuid>> {
        self.store.drawer_id_for_fact_key(fact_key)
    }

    /// Fetch one drawer by id straight from redb (#6438).
    ///
    /// Why: the Tier C retirement path needs the incumbent named by the
    /// `fact_key` index, and the in-memory drawer table can be missing a row
    /// redb still holds. Reading the durable row is what stops that gap from
    /// admitting a second live claimant for one slot.
    /// What: forwards to `KgStoreRedb::load_drawer`. Synchronous for the same
    /// reason as `drawer_id_for_fact_key` — one redb read transaction, taken
    /// while the caller already holds the per-palace commit-order guard.
    /// `Ok(None)` is a dangling index entry; `Err` is a row present but
    /// unreadable, which the caller must not treat as absence.
    /// Test: `an_on_disk_incumbent_absent_from_the_mirror_is_still_retired`.
    pub fn load_drawer(&self, id: Uuid) -> Result<Option<Drawer>> {
        self.store.load_drawer(id)
    }

    /// Remove a drawer's metadata by ID.
    ///
    /// Why: Forgetting must clear both the vector index and the persistent
    /// metadata row. Same coalescing rationale as `upsert_drawer`.
    /// What: Forwards to `KgWriter::delete_drawer`.
    /// Test: `delete_drawer_removes_row`.
    pub async fn delete_drawer(&self, id: Uuid) -> Result<()> {
        self.writer.delete_drawer(id).await
    }

    /// Synchronous drawer delete used by palace open-time pruning.
    ///
    /// Why: Issue #61's TTL sweep runs inside `PalaceHandle::open`, which is
    /// synchronous and predates any tokio runtime context. The async writer
    /// path requires an executor we don't have here; going straight to the
    /// underlying redb store keeps the sweep contention-free at startup.
    /// Outside of open we always prefer `delete_drawer` so writes coalesce.
    /// What: Forwards directly to `KgStoreRedb::delete_drawer`.
    /// Test: Covered indirectly by `purge_expired_drops_only_past_ttl`.
    pub fn delete_drawer_sync(&self, id: Uuid) -> Result<()> {
        self.store.delete_drawer(id)
    }

    /// Load the set of drawer IDs currently stored.
    ///
    /// Why: Compaction only needs "is this UUID a live drawer?".
    /// What: Delegates to `KgStoreRedb::load_drawer_ids`.
    /// Test: `load_drawer_ids_matches_load_drawers`.
    pub fn load_drawer_ids(&self) -> Result<std::collections::HashSet<Uuid>> {
        self.store.load_drawer_ids()
    }

    /// Load all drawer metadata.
    ///
    /// Why: Cold-start retrieval needs the full drawer table to map every
    /// HNSW vector hit back to metadata.
    /// What: Delegates to `KgStoreRedb::load_drawers`.
    /// Test: `upsert_drawer_then_load_drawers_round_trips`.
    pub fn load_drawers(&self) -> Result<Vec<Drawer>> {
        self.store.load_drawers()
    }

    /// Load all drawer metadata, reporting how many rows were unreadable.
    ///
    /// Why: #6201 — `open_with_intent` must flag a handle as degraded when a
    /// partially-corrupt drawer table loads fewer rows than it holds, not only
    /// when the whole table fails to open.
    /// What: Delegates to `KgStoreRedb::load_drawers_with_skipped`, returning
    /// `(rows, skipped)`.
    /// Test: `drawer_load_degraded_true_on_partial_row_corruption` in
    /// `retrieval::tests`.
    pub fn load_drawers_with_skipped(&self) -> Result<(Vec<Drawer>, usize)> {
        self.store.load_drawers_with_skipped()
    }

    /// Identify community-shaped knowledge gaps in the active graph.
    ///
    /// Why: Convenience accessor so callers don't need to import the
    /// `community` module just to get gap suggestions.
    /// What: Delegates to `community::find_communities(self)`.
    /// Test: `community_tests::knowledge_gaps_on_sparse_graph`.
    pub fn knowledge_gaps(&self) -> Vec<crate::memory_core::community::KnowledgeGap> {
        crate::memory_core::community::find_communities(self)
    }

    /// Whether this KG was opened against a read-only snapshot of a redb
    /// file locked by another process.
    ///
    /// Why: Issue #59 — `PalaceHandle::is_read_only` aggregates this with
    /// the vector store's flag so the MCP layer can produce a clear
    /// "route writes through the HTTP daemon" error before any write is
    /// attempted.
    /// What: Delegates to `KgStoreRedb::is_read_only`.
    /// Test: `palace_handle_read_only_when_kg_snapshotted` (in
    /// `retrieval.rs`).
    pub fn is_read_only(&self) -> bool {
        self.store.is_read_only()
    }

    /// Delete all active triples whose subject is `drawer:<drawer_id>`.
    ///
    /// Why: Issue #278 (cascade-delete) — when a drawer is forgotten via
    /// `PalaceHandle::forget`, every auto-extracted triple anchored to that
    /// drawer (identified by the `drawer:<uuid>` subject prefix) would otherwise
    /// remain as orphaned edges, polluting the KG with facts that reference a
    /// non-existent source. This method closes them all in one shot.
    /// What: Delegates to `KgStoreRedb::delete_by_subject` using the canonical
    /// `drawer:<uuid>` subject format (`drawer:<hyphenated-uuid>`), then drops
    /// the corresponding edges from the in-memory adjacency so subsequent graph
    /// queries see a consistent view without a restart. The commit lands before
    /// the adjacency update, so both steps route through the #5424 contract:
    /// [`KnowledgeGraph::ensure_adjacency_live`] refuses once the index is
    /// known stale, and [`KnowledgeGraph::sync_adjacency`] reports the rows the
    /// cascade already made durable rather than a bare lock error. That matters
    /// most here because the caller, `PalaceHandle::forget`, only logs the
    /// failure and carries on.
    /// Test: `cascade_delete_removes_triples_for_drawer`,
    /// `cascade_delete_after_poisoned_adjacency_is_distinguishable_from_never_deleted`.
    pub async fn cascade_delete_by_drawer(&self, drawer_id: Uuid) -> Result<usize> {
        self.ensure_adjacency_live("cascade_delete_by_drawer")?;
        // Canonical subject format used by `kg_extract.rs::drawer_subject`.
        let subject = format!("drawer:{drawer_id}");
        let store = self.store.clone();
        let subject_clone = subject.clone();
        let closed = tokio::task::spawn_blocking(move || store.delete_by_subject(&subject_clone))
            .await
            .context("cascade_delete_by_drawer spawn_blocking join error")??;

        // Sync the in-memory adjacency — remove every edge from the drawer's
        // node so the graph view reflects the deletion without a restart.
        if closed > 0 {
            self.sync_adjacency("cascade_delete_by_drawer", closed, |adj| {
                if let Some(&s_idx) = adj.node_index.get(&subject) {
                    let to_remove: Vec<_> = adj.graph.edges(s_idx).map(|e| e.id()).collect();
                    for eid in to_remove {
                        adj.graph.remove_edge(eid);
                    }
                }
            })?;
        }
        Ok(closed)
    }

    /// Close the active triple `subject --predicate--> object`, leaving every
    /// other object at that pair live. Returns 1 when a row was closed, 0 when
    /// there was none.
    ///
    /// Why: [`KnowledgeGraph::retract`] closes the WHOLE `(subject, predicate)`
    /// pair, so removing one wrong object took the correct siblings with it
    /// (#5396). Re-asserting is no escape either — `is-a`, `works-at`, `uses`
    /// and `depends-on` are absent from `FUNCTIONAL_PREDICATES`, so a new object
    /// joins the bad one rather than displacing it.
    /// What: delegates to [`KgStoreRedb::retract_triple`] on the blocking pool,
    /// then drops the single matching edge from the in-memory adjacency so
    /// `neighbors` and the graph views agree with storage without a restart.
    /// It writes directly rather than through the coalescing `KgWriter`,
    /// matching `cascade_delete_by_drawer` — `BatchWriteOp` is a public
    /// non-`#[non_exhaustive]` enum, so a new variant would be a breaking change
    /// for a path that is a maintenance sweep, not a hot write. redb serialises
    /// write transactions, so a concurrent batch is ordered, not raced.
    ///
    /// The commit is durable before the adjacency update runs, so a failure of
    /// that second step means the row IS closed and only the derived index
    /// disagrees (#5424). It returns [`AdjacencyDesync::CommittedButStale`](crate::memory_core::store::kg::AdjacencyDesync::CommittedButStale)
    /// naming the closed row instead of a bare lock error, and every later
    /// mutation on the handle returns [`AdjacencyDesync::HandleStale`](crate::memory_core::store::kg::AdjacencyDesync::HandleStale) — a
    /// retry can no longer read `Ok(0)` and conclude the fact was never there.
    /// Test: `retract_triple_drops_one_edge_and_keeps_the_siblings`,
    /// `retract_triple_after_poisoned_adjacency_is_distinguishable_from_never_retracted`,
    /// `poisoned_adjacency_error_downcasts_with_the_committed_row_count`.
    pub async fn retract_triple(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
    ) -> Result<usize> {
        // #5396: remove one object without collateral loss of its siblings.
        self.ensure_adjacency_live("retract_triple")?;
        let store = self.store.clone();
        let (s, p, o) = (
            subject.to_string(),
            predicate.to_string(),
            object.to_string(),
        );
        let (sc, pc, oc) = (s.clone(), p.clone(), o.clone());
        let closed = tokio::task::spawn_blocking(move || store.retract_triple(&sc, &pc, &oc))
            .await
            .context("retract_triple spawn_blocking join error")??;

        if closed > 0 {
            self.sync_adjacency("retract_triple", closed, |adj| {
                adj.remove_edge_to(&s, &p, &o);
            })?;
        }
        Ok(closed)
    }

    /// Synchronous triple assert; see `KgWriter::assert_sync`.
    ///
    /// Why: CLI commands (e.g. `migrate kuzu-data`) run outside a tokio
    /// runtime and need a direct write path without spawning an executor.
    /// What: Delegates to `KgWriter::assert_sync` on the bypass path.
    /// Test: Used by `kuzu_migrate::tests` and the fixture-based integration
    /// test in `tests/kuzu_migrate_tests.rs`.
    pub fn assert_sync(&self, triple: &Triple) -> Result<()> {
        self.writer.assert_sync(triple)
    }

    /// Synchronous drawer upsert; see `KgWriter::upsert_drawer_sync`.
    ///
    /// Why: Same motivation as `assert_sync` — CLI migrate commands need a
    /// synchronous write path.
    /// What: Delegates to `KgWriter::upsert_drawer_sync`.
    /// Test: Used by `kuzu_migrate::tests`.
    pub fn upsert_drawer_sync(&self, drawer: &Drawer) -> Result<()> {
        self.writer.upsert_drawer_sync(drawer)
    }

    /// Expose the underlying store for read-only inspection (e.g. schema
    /// discovery in migrate commands).
    ///
    /// Why: CLI commands that need to call store methods not exposed on
    /// `KnowledgeGraph` directly (e.g. `query_active` in a sync context)
    /// need access to the raw store reference. The store reference is
    /// `Arc<KgStoreRedb>` so cloning it is cheap.
    /// What: Returns a clone of the `Arc<KgStoreRedb>` via the writer's
    /// `store()` accessor.
    /// Test: Used by `kuzu_migrate` for idempotency checks.
    pub fn store(&self) -> std::sync::Arc<KgStoreRedb> {
        self.writer.store()
    }

    /// Dump every triple including closed history rows.
    ///
    /// Why: Issue #45's SQLite → redb migration walks the entire SQLite table.
    /// This complementary helper exposes the redb side for downstream
    /// consistency checks.
    /// What: Delegates to `KgStoreRedb::dump_all_triples`.
    /// Test: Covered indirectly by `kg_redb::tests::assert_supersedes_prior`.
    pub fn dump_all_triples(&self) -> Result<Vec<Triple>> {
        self.store.dump_all_triples()
    }
}
