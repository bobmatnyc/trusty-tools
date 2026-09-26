//! Runtime KG/vector component soft-toggle logic (issue #2984 Phase 1).
//!
//! Why: `PATCH /indexes/:id/config` (`index_config.rs`) needs to flip
//! `skip_kg`/`skip_vector` on a LIVE index without a daemon restart or index
//! recreation — turning a component OFF is an instantaneous soft-disable
//! (D2: keep-on-disk, drop-from-memory); turning one ON must trigger a
//! background catch-up (D3) that must not race a concurrent reindex or
//! another catch-up. Splitting this decision/mutation/catch-up logic into its
//! own module keeps `index_config.rs` under the 500-SLOC production cap and
//! makes the pure decision function (`resolve_component_toggle`)
//! independently unit-testable, mirroring `service::reindex::validate`'s
//! pattern.
//!
//! What:
//! - `ComponentTransition` — the pure decision result (what's changing).
//! - `resolve_component_toggle` — pure function computing the transition.
//! - `apply_component_transition` — the synchronous soft-disable/enable-intent
//!   side effects (stage flip + in-memory KG drop).
//! - `spawn_component_catch_up` — the background catch-up task for a
//!   turn-on, holding the caller's already-acquired semaphore permit.
//! - `claim_semantic_rearm` / `spawn_semantic_rearm` — the #8148 embed-only
//!   re-arm, queued behind the background permit like any C2 pass.
//!
//! Test: `service::server::tests_components`.

use std::sync::Arc;

use crate::core::registry::{IndexHandle, StageState, StageStatus};

/// Pure decision result of a component-toggle request against the handle's
/// current `skip_kg`/`skip_vector` state.
///
/// Why: separating "what should change" from "how to apply it" lets the
/// decision be unit-tested without a live `IndexHandle` (mirrors
/// `service::reindex::validate::ReindexOutcome`).
/// What: the resolved target flags plus four independent on/off transition
/// booleans — KG and vector transitions are always independent (Bob's
/// locked design: every quadrant is reachable).
/// Test: `resolve_component_toggle_*` in `tests_components`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ComponentTransition {
    pub new_skip_kg: bool,
    pub new_skip_vector: bool,
    pub kg_turning_on: bool,
    pub kg_turning_off: bool,
    pub vector_turning_on: bool,
    pub vector_turning_off: bool,
}

impl ComponentTransition {
    /// True when EITHER component is transitioning OFF → ON, meaning a
    /// background catch-up (and therefore the concurrency-guard semaphore
    /// permit) is required.
    pub(super) fn needs_catch_up(&self) -> bool {
        self.kg_turning_on || self.vector_turning_on
    }
}

/// Resolve a `PATCH /indexes/:id/config` component request into a
/// [`ComponentTransition`].
///
/// Why: `kg`/`vector` on the wire are `Option<bool>` — `None` means "leave
/// this component's state unchanged", so the target state must be resolved
/// against the CURRENT `skip_kg`/`skip_vector` before the four transition
/// booleans can be computed.
/// What: `want_kg`/`want_vector` default to the current enabled state when
/// the request field is absent; each transition flag is true only when the
/// wanted state differs from the current one in that specific direction.
/// Test: `resolve_component_toggle_no_op_when_fields_absent`,
/// `resolve_component_toggle_detects_each_transition_independently`,
/// `resolve_component_toggle_vector_off_kg_on_quadrant`.
pub(super) fn resolve_component_toggle(
    kg: Option<bool>,
    vector: Option<bool>,
    existing_skip_kg: bool,
    existing_skip_vector: bool,
) -> ComponentTransition {
    let want_kg = kg.unwrap_or(!existing_skip_kg);
    let want_vector = vector.unwrap_or(!existing_skip_vector);
    ComponentTransition {
        new_skip_kg: !want_kg,
        new_skip_vector: !want_vector,
        kg_turning_on: want_kg && existing_skip_kg,
        kg_turning_off: !want_kg && !existing_skip_kg,
        vector_turning_on: want_vector && existing_skip_vector,
        vector_turning_off: !want_vector && !existing_skip_vector,
    }
}

/// Upgrade a no-op `vector: true` into a catch-up when the lane is enabled but
/// the semantic stage never got built (#8148).
///
/// Why: an index registered over an `index.redb` whose chunks carry no vectors
/// has `skip_vector: false` already, so [`resolve_component_toggle`] reports
/// `vector_turning_on: false` and `PATCH /indexes/:id/config {"vector": true}`
/// answers `catch_up_started: false`. Nothing else ever queues the C2 pass for
/// that corpus either — `boot_markers::rearm_deferred_embed_if_pending` fires
/// only on the `deferred_embed_pending` marker a reindex leaves behind — so
/// `deferred_embed_queue_depth` stays `0` and the semantic stage stays
/// `Pending` forever. This makes the existing route the embed-only trigger,
/// with no new endpoint and no full reindex.
/// What: a pure predicate. `true` only when the caller EXPLICITLY asked for
/// `vector: true`, the resolved state has the lane enabled, the transition is
/// not already a turn-on, and the semantic stage is `Pending` or `Failed`.
/// `Ready` and `InProgress` stay no-ops — the first has nothing to embed, the
/// second already has a pass running and a second one would contend for the
/// same per-index permit.
/// Test: `rearm_vector_catch_up_fires_for_a_pending_semantic_stage`,
/// `rearm_vector_catch_up_is_a_no_op_when_semantic_is_ready`.
pub(super) fn should_rearm_vector_catch_up(
    transition: &ComponentTransition,
    vector_requested: Option<bool>,
    semantic: StageStatus,
) -> bool {
    vector_requested == Some(true)
        && !transition.new_skip_vector
        && !transition.vector_turning_on
        && matches!(semantic, StageStatus::Pending | StageStatus::Failed)
}

/// Claim the semantic stage for an embed-only re-arm (#8148).
///
/// Why: the handler read the stage before taking any lock, so two concurrent
/// `vector: true` PATCHes can both see `Pending`. The claim is the atomic
/// check-and-set that lets exactly one of them queue a pass.
/// What: under the `stages` write lock, flips `semantic` from
/// `Pending`/`Failed` to `InProgress` and returns `true`; any other status is
/// left alone and returns `false`.
/// Test: `service::server::tests_8148::patch_vector_true_is_a_no_op_once_semantic_is_ready`,
/// `rearm_patches_across_indexes_wait_for_the_background_permit`.
pub(super) async fn claim_semantic_rearm(handle: &IndexHandle) -> bool {
    let mut stages = handle.stages.write().await;
    if !matches!(
        stages.semantic.status,
        StageStatus::Pending | StageStatus::Failed
    ) {
        return false;
    }
    stages.semantic = StageState {
        status: StageStatus::InProgress,
        started_at: Some(now_rfc3339()),
        ..Default::default()
    };
    true
}

/// Queue the embed-only re-arm on the deferred-embed queue (#8148).
///
/// Why: the re-arm first ran through [`spawn_component_catch_up`], which never
/// takes `background_reindex_semaphore`, so N PATCHes across N indexes ran N
/// embed passes at once. The queue takes the background permit and then the
/// per-index permit, in that order; this function takes neither, so it cannot
/// invert that order and deadlock.
/// What: sets the durable `deferred_embed_pending` marker (a restart re-arms a
/// pass it interrupts, as for a reindex's C2 pass), then enqueues via
/// `spawn_deferred_embed_pass`, keyed on the cheap `chunk_count()` upper bound.
/// Test: `service::server::tests_8148::rearm_patches_across_indexes_wait_for_the_background_permit`.
pub(super) async fn spawn_semantic_rearm(handle: Arc<IndexHandle>) {
    let chunks = handle.indexer.read().await.chunk_count();
    crate::service::boot_markers::persist_deferred_embed_pending(&handle.id.0, true);
    let progress = Arc::new(crate::service::reindex::ReindexProgress::new());
    crate::service::reindex::spawn_deferred_embed_pass(handle, progress, chunks);
}

/// Apply the immediate, synchronous half of a component transition.
///
/// Why: soft-disable is instantaneous — the caller must see the stage flip
/// and (for KG) the in-memory heap release in the SAME request that accepted
/// the toggle, not after a background task eventually gets around to it.
/// Soft-ENABLE only flips the stage to `InProgress` here; the caller is
/// responsible for spawning the actual catch-up (`spawn_component_catch_up`)
/// once the concurrency permit is confirmed.
/// What: write-locks `handle.stages` once and applies both component flips
/// (KG and vector are independent, so both may apply in the same call); a
/// KG turn-off additionally calls `clear_symbol_graph_in_memory` (D2).
/// Test: `patch_*` in `tests_components` (integration tests exercising this
/// indirectly via the PATCH endpoint).
pub(super) async fn apply_component_transition(
    handle: &IndexHandle,
    transition: &ComponentTransition,
) {
    {
        let mut stages = handle.stages.write().await;
        if transition.kg_turning_off {
            stages.graph = StageState::skipped();
        } else if transition.kg_turning_on {
            stages.graph = StageState {
                status: StageStatus::InProgress,
                started_at: Some(now_rfc3339()),
                ..Default::default()
            };
        }
        if transition.vector_turning_off {
            stages.semantic = StageState::skipped();
        } else if transition.vector_turning_on {
            stages.semantic = StageState {
                status: StageStatus::InProgress,
                started_at: Some(now_rfc3339()),
                ..Default::default()
            };
        }
    }
    // Issue #2984 Phase 1 CRITICAL finding 1: synchronize `CodeIndexer`'s
    // OWN `skip_kg` copy with the transition BEFORE any catch-up is spawned.
    // `load_or_rebuild_symbol_graph`'s documented precondition (persist.rs)
    // is that callers MUST flip `self.skip_kg` first — without this, a
    // turn-on silently no-ops (the graph stays empty forever, even though
    // the stage settles `Ready`) and a turn-off's in-memory clear wouldn't be
    // mirrored back onto the indexer's own flag either. Both directions are
    // handled here (`transition.new_skip_kg` is the resolved target for
    // either a turn-on or a turn-off) via a single write-lock acquisition.
    // #3048: the vector lane needs the identical sync. `IndexHandle` is rebuilt
    // on every PATCH but reuses this same `indexer` `Arc`, so the indexer's own
    // copy is what `CodeIndexer::index_file` reads — leave it unsynced and a
    // turn-off never reaches the watcher, which keeps embedding an index the
    // operator just disabled. Folded into the same write-lock acquisition as
    // the KG sync so a toggle of both components takes the lock once.
    if transition.kg_turning_on
        || transition.kg_turning_off
        || transition.vector_turning_on
        || transition.vector_turning_off
    {
        let mut indexer = handle.indexer.write().await;
        if transition.kg_turning_on || transition.kg_turning_off {
            indexer.set_skip_kg(transition.new_skip_kg);
        }
        if transition.vector_turning_on || transition.vector_turning_off {
            indexer.set_skip_vector(transition.new_skip_vector);
        }
    }
    if transition.kg_turning_off {
        let indexer = handle.indexer.read().await;
        indexer.clear_symbol_graph_in_memory().await;
    }
}

/// Spawn the background catch-up task for a component transition that needs
/// one, holding `permit` for the task's full duration.
///
/// Why (D3 — background backfill, not wait-for-reindex): the HTTP handler
/// must return immediately (stage already flipped to `InProgress` by
/// [`apply_component_transition`]); the actual KG reload/rebuild and/or
/// vector backfill run in the background and flip the stage to
/// `Ready`/`Failed` on completion. `permit` was acquired by the caller
/// (`patch_index_config_handler`) via `try_acquire` BEFORE any state mutated,
/// so a concurrent reindex or another catch-up on this index always sees a
/// busy semaphore and the caller returns `409` — moving it into this task
/// (rather than re-acquiring here) avoids a nested-acquire deadlock against
/// the single-permit semaphore.
/// What: KG catch-up calls `CodeIndexer::catch_up_symbol_graph` (cheap
/// `load_from_corpus`, falls back to a full rebuild — issue #2984 D3), then
/// marks the graph stage `Ready` via [`mark_kg_catch_up_complete`]. Vector
/// catch-up delegates to `service::reindex::run_embed_catch_up` (the
/// generalised #923 deferred-embed core), which marks the semantic stage
/// `Ready`/`Failed` itself. Both may run in the same task since only one
/// permit exists.
///
/// `teardown_guard` (#3049 round 3) travels the same way and for the same
/// reason: this task writes the corpus (`catch_up_symbol_graph`,
/// `run_embed_catch_up`), so a `DELETE` must not be able to `remove_dir_all`
/// underneath it. Re-acquiring here instead would leave the gap between the
/// handler returning and the task being polled unguarded.
/// Test: `service::server::tests_components::patch_vector_on_spawns_catch_up_and_reaches_ready`,
/// `patch_kg_on_spawns_catch_up_and_reaches_ready`,
/// `service::server::tests_3049::patch_index_config_waits_for_an_in_flight_teardown`.
pub(super) fn spawn_component_catch_up(
    handle: Arc<IndexHandle>,
    transition: ComponentTransition,
    permit: tokio::sync::OwnedSemaphorePermit,
    teardown_guard: tokio::sync::OwnedRwLockReadGuard<()>,
) {
    tokio::spawn(async move {
        let _permit = permit;
        let _teardown_guard = teardown_guard;
        let index_id = handle.id.0.clone();

        if transition.kg_turning_on {
            tracing::info!("components[{index_id}]: KG catch-up starting (issue #2984)");
            let indexer = handle.indexer.read().await;
            indexer.catch_up_symbol_graph().await;
            let g = indexer.symbol_graph().await;
            let (node_count, edge_count) = (g.node_count(), g.edge_count());
            drop(indexer);
            mark_kg_catch_up_complete(&handle, node_count, edge_count).await;
        }

        if transition.vector_turning_on {
            tracing::info!("components[{index_id}]: vector catch-up starting (issue #2984)");
            // A dedicated, unregistered progress handle: the catch-up's SSE
            // events are not surfaced on `GET /indexes/:id/reindex/stream`
            // (that map is keyed by index id and reserved for reindex runs);
            // stage transitions (the durable signal `index_status`/`/health`
            // read) still land on the shared `handle.stages`.
            let progress = Arc::new(crate::service::reindex::ReindexProgress::new());
            crate::service::reindex::run_embed_catch_up(Arc::clone(&handle), progress).await;
            tracing::info!("components[{index_id}]: vector catch-up complete");
        }
    });
}

/// Complete a KG catch-up: flip the graph stage to `Ready` UNLESS a
/// component disable already landed while the catch-up was running (issue
/// #2984 Phase 1 MEDIUM finding 4 — mirrors the vector completion guard in
/// `service::reindex::defer_embed::run_embed_catch_up`).
///
/// Why: `IndexHandle::skip_kg` is a plain, per-`Arc<IndexHandle>`-snapshot
/// bool — every PATCH rebuilds a brand-new `IndexHandle` and re-registers it,
/// so a task already holding an older `Arc<IndexHandle>` can never observe a
/// LATER toggle updating that field. `stages`, by contrast, is the one piece
/// of handle state preserved verbatim (`Arc::clone`, never replaced) across
/// every rebuild, so it is the only reliable "did a disable land while I was
/// running" signal available to this background task.
/// `apply_component_transition`'s synchronous half already writes
/// `stages.graph = Skipped` the instant a disable lands; checking for that
/// here, under the SAME write lock the `Ready` write uses, makes
/// disable-wins-over-a-racing-catch-up airtight.
/// What: write-locks `handle.stages`; if `graph.status` is already `Skipped`,
/// leaves it alone (logs at info, still records `node_count`/`edge_count`
/// for the log line); otherwise flips it to `Ready` with `completed_at`.
/// Test: `service::server::components::tests_pure::kg_catch_up_completion_never_clobbers_a_disable_that_landed_first`.
async fn mark_kg_catch_up_complete(
    handle: &Arc<IndexHandle>,
    node_count: usize,
    edge_count: usize,
) {
    let index_id = &handle.id.0;
    let mut stages = handle.stages.write().await;
    if stages.graph.status == StageStatus::Skipped {
        tracing::info!(
            "components[{index_id}]: KG disabled mid-catch-up — leaving graph Skipped \
             ({node_count} nodes / {edge_count} edges computed but not published as Ready, \
             issue #2984 Phase 1 finding 4)"
        );
        return;
    }
    stages.graph.status = StageStatus::Ready;
    stages.graph.completed_at = Some(now_rfc3339());
    drop(stages);
    tracing::info!(
        "components[{index_id}]: KG catch-up complete ({node_count} nodes / {edge_count} edges)"
    );
}

/// RFC-3339 timestamp helper, mirroring `service::reindex::stages::now_rfc3339`
/// (that one is `pub(super)`-scoped to `service::reindex` and unreachable
/// from here).
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests_pure {
    use super::*;

    /// `None`/`None` must be a pure no-op: every transition flag is `false`
    /// and the target state exactly mirrors the current one.
    /// Why: a hygiene-only PATCH (no `kg`/`vector` fields) must never touch
    /// component state.
    /// Test: this test.
    #[test]
    fn resolve_component_toggle_no_op_when_fields_absent() {
        let t = resolve_component_toggle(None, None, false, true);
        assert!(!t.new_skip_kg, "kg stays enabled");
        assert!(t.new_skip_vector, "vector stays disabled");
        assert!(!t.kg_turning_on && !t.kg_turning_off);
        assert!(!t.vector_turning_on && !t.vector_turning_off);
        assert!(!t.needs_catch_up());
    }

    /// Every one of the four transition directions must be detected
    /// independently of the other component's state.
    /// Why: KG and vector toggles are orthogonal by design (#2984).
    /// Test: this test.
    #[test]
    fn resolve_component_toggle_detects_each_transition_independently() {
        // KG off -> on, vector untouched (currently on).
        let t = resolve_component_toggle(Some(true), None, true, false);
        assert!(t.kg_turning_on && !t.kg_turning_off);
        assert!(!t.vector_turning_on && !t.vector_turning_off);
        assert!(t.needs_catch_up());

        // KG on -> off, vector untouched (currently off).
        let t = resolve_component_toggle(Some(false), None, false, true);
        assert!(!t.kg_turning_on && t.kg_turning_off);
        assert!(!t.vector_turning_on && !t.vector_turning_off);
        assert!(!t.needs_catch_up());

        // Vector off -> on, kg untouched (currently on).
        let t = resolve_component_toggle(None, Some(true), false, true);
        assert!(!t.kg_turning_on && !t.kg_turning_off);
        assert!(t.vector_turning_on && !t.vector_turning_off);
        assert!(t.needs_catch_up());

        // Vector on -> off, kg untouched (currently off).
        let t = resolve_component_toggle(None, Some(false), true, false);
        assert!(!t.kg_turning_on && !t.kg_turning_off);
        assert!(!t.vector_turning_on && t.vector_turning_off);
        assert!(!t.needs_catch_up());
    }

    /// #8148: an explicit `vector: true` against an already-enabled lane earns
    /// a catch-up exactly when the semantic stage never got built.
    ///
    /// Why: this predicate is the whole of the embed-only trigger's decision,
    /// and both directions matter — `Pending`/`Failed` must fire (that is the
    /// stranded corpus) while `Ready`/`InProgress` must not (nothing to do, or
    /// a pass already holds the permit).
    /// Test: this test.
    #[test]
    fn rearm_vector_catch_up_fires_for_a_pending_semantic_stage() {
        // Lane already enabled, so this is NOT a turn-on.
        let t = resolve_component_toggle(None, Some(true), false, false);
        assert!(!t.vector_turning_on, "sanity: nothing is transitioning");
        assert!(should_rearm_vector_catch_up(
            &t,
            Some(true),
            StageStatus::Pending
        ));
        assert!(should_rearm_vector_catch_up(
            &t,
            Some(true),
            StageStatus::Failed
        ));
    }

    /// #8148, the other direction: every state that must stay a no-op.
    ///
    /// Why: guards against turning an idempotent config call into a re-embed.
    /// Test: this test.
    #[test]
    fn rearm_vector_catch_up_is_a_no_op_when_semantic_is_ready() {
        let t = resolve_component_toggle(None, Some(true), false, false);
        for status in [StageStatus::Ready, StageStatus::InProgress] {
            assert!(
                !should_rearm_vector_catch_up(&t, Some(true), status),
                "{status:?} must not start a catch-up"
            );
        }
        // Absent field: a hygiene-only PATCH never embeds anything.
        assert!(!should_rearm_vector_catch_up(
            &t,
            None,
            StageStatus::Pending
        ));
        // A real off -> on transition is already handled by `needs_catch_up`.
        let turning_on = resolve_component_toggle(None, Some(true), false, true);
        assert!(!should_rearm_vector_catch_up(
            &turning_on,
            Some(true),
            StageStatus::Pending
        ));
        // A lane being turned OFF in the same call never re-arms.
        let off = resolve_component_toggle(None, Some(false), false, false);
        assert!(!should_rearm_vector_catch_up(
            &off,
            Some(false),
            StageStatus::Pending
        ));
    }

    /// The design doc's headline scenario: enabling BOTH components from a
    /// fully-off index must set both turning_on flags and require catch-up.
    /// Test: this test.
    #[test]
    fn resolve_component_toggle_both_on_from_fully_off() {
        let t = resolve_component_toggle(Some(true), Some(true), true, true);
        assert!(t.kg_turning_on && t.vector_turning_on);
        assert!(!t.new_skip_kg && !t.new_skip_vector);
        assert!(t.needs_catch_up());
    }

    /// The vector-off/KG-on quadrant (issue #2984's headline use case) must
    /// resolve to a vector-only turn-off with no KG transition at all.
    /// Test: this test.
    #[test]
    fn resolve_component_toggle_vector_off_kg_on_quadrant() {
        let t = resolve_component_toggle(None, Some(false), false, false);
        assert!(!t.kg_turning_on && !t.kg_turning_off);
        assert!(t.vector_turning_off && !t.vector_turning_on);
        assert!(!t.new_skip_kg, "kg stays enabled");
        assert!(t.new_skip_vector, "vector newly disabled");
        assert!(!t.needs_catch_up(), "a turn-off never needs catch-up");
    }

    /// Requesting the SAME state the component is already in must resolve to
    /// no transition (idempotent PATCH).
    /// Test: this test.
    #[test]
    fn resolve_component_toggle_idempotent_when_already_in_requested_state() {
        let t = resolve_component_toggle(Some(true), Some(false), false, true);
        assert!(!t.kg_turning_on && !t.kg_turning_off);
        assert!(!t.vector_turning_on && !t.vector_turning_off);
        assert!(!t.needs_catch_up());
    }

    /// Issue #2984 Phase 1 MEDIUM finding 4: if a component disable lands
    /// (synchronously writing `stages.graph = Skipped`) WHILE a KG catch-up
    /// is still in flight, the catch-up's own completion must never clobber
    /// that `Skipped` back to `Ready` — the disable must win.
    ///
    /// Why: `IndexHandle::skip_kg` is a plain per-handle-snapshot bool that a
    /// concurrently in-flight task can never observe a later toggle updating
    /// (every PATCH rebuilds and re-registers a brand-new `IndexHandle`).
    /// `stages` is the one thing that IS reliably shared across every
    /// rebuild, so `mark_kg_catch_up_complete` must arbitrate off `stages`,
    /// not off `handle.skip_kg`. This test proves it deterministically by
    /// pre-seeding `stages.graph = Skipped` (simulating "the disable already
    /// landed") before calling the completion function directly.
    /// What: builds a bare handle, sets `stages.graph` to `Skipped`, calls
    /// `mark_kg_catch_up_complete`, and asserts the stage is still `Skipped`
    /// (not clobbered to `Ready`).
    /// Test: this IS the test.
    #[tokio::test]
    async fn kg_catch_up_completion_never_clobbers_a_disable_that_landed_first() {
        use crate::core::indexer::CodeIndexer;
        use crate::core::registry::{IndexHandle, IndexId};
        use tokio::sync::RwLock;

        let handle = Arc::new(IndexHandle::bare(
            IndexId::new("kg-catchup-race"),
            Arc::new(RwLock::new(CodeIndexer::new(
                "kg-catchup-race",
                "/tmp/kg-catchup-race",
            ))),
            "/tmp/kg-catchup-race".into(),
        ));
        {
            let mut stages = handle.stages.write().await;
            stages.graph = StageState::skipped();
        }

        mark_kg_catch_up_complete(&handle, 3, 2).await;

        let stages = handle.stages.read().await;
        assert_eq!(
            stages.graph.status,
            StageStatus::Skipped,
            "a disable that landed first must win — completion must never \
             clobber Skipped back to Ready (#2984 Phase 1 MEDIUM finding 4)"
        );
    }
}
