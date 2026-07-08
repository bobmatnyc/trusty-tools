//! Pure stage-classifier for warm-booted indexes (issue #135 + #993).
//!
//! Why: `WarmBootInputs` and `derive_warm_boot_stages` were originally in
//! `commands/start.rs`, making them inaccessible from the library's service
//! layer. Moved here (issue #993 refactor) so the lazy-load restore path in
//! `service/` can call the same classifier without depending on the binary's
//! `commands` module.
//! What: `WarmBootInputs` carries the four on-disk signals; `derive_warm_boot_stages`
//! maps them to an `IndexStages` value. Both are pure — no I/O, no side effects.
//! Test: `warm_boot_*` tests in `commands/start.rs` drive this via the
//! re-export there; `lazy_restore::tests` calls it directly.

use crate::core::registry::{IndexStages, StageState, StageStatus};

/// On-disk signals needed to classify each stage of a restored index.
///
/// Why (issue #135): the warm-boot path was instantiating every restored
/// handle with `stages = Pending` and never consulting the on-disk artifacts.
/// Searches against existing indexes silently dropped the semantic + graph
/// lanes because `search_capabilities` is derived from `stages`. Extracting
/// the classification into a pure function lets the call site keep its single
/// disk-inspection sweep and lets the unit tests pin every transition.
/// What: a small data carrier — no `Default` impl, no methods. Construction
/// is intentionally explicit so a future caller cannot forget to wire one of
/// the four signals.
/// Test: every `warm_boot_*` test in `commands/start.rs` constructs one of
/// these inputs directly and asserts the resulting [`IndexStages`].
#[derive(Debug, Clone, Copy)]
pub struct WarmBootInputs {
    /// Number of chunks the durable corpus reports (`CorpusStore::chunk_count`).
    pub chunk_count: usize,
    /// `true` when the HNSW snapshot was restored with the correct dimension.
    pub hnsw_snapshot_ready: bool,
    /// Node count from the rehydrated symbol graph.
    pub graph_node_count: usize,
    /// `lexical_only` flag from the persisted registry entry.
    pub lexical_only: bool,
    /// `skip_kg` flag (issue #313): forces graph stage to `Skipped`.
    pub skip_kg: bool,
    /// Issue #1158: `true` when the redb corpus file existed but could not be
    /// opened (e.g. incompatible redb page-format, corrupted file).
    ///
    /// Why: before this flag, a failed corpus open caused `chunk_count = 0`
    /// (no corpus store → `unwrap_or(0)`), which the classifier treated as
    /// `InProgress` — indistinguishable from a freshly-created, never-indexed
    /// handle. Operators saw `chunks=0` in the warm-boot log and the index
    /// looked healthy-ish when it was silently broken. This flag lets the
    /// classifier emit `StageStatus::Failed` with an actionable reason
    /// instead of the misleading `InProgress` state.
    pub corpus_open_failed: bool,
}

/// Pure classifier: given on-disk signals, derive the [`IndexStages`] for a
/// restored index handle.
///
/// Why (issue #135): see [`WarmBootInputs`].
/// What: applies rules in order:
///   1. `corpus_open_failed == true` → lexical, semantic, AND graph are all
///      `Failed` (issues #1158, #1870, #2203 — see the inline rationale
///      below).
///   2. `chunk_count > 0` → lexical is `Ready`.
///   3. `chunk_count == 0` → lexical is `InProgress`.
///   4. `lexical_only == true` → semantic + graph are `Skipped`.
///   5. `hnsw_snapshot_ready` → semantic is `Ready`.
///   6. `graph_node_count > 0` → graph is `Ready`.
///
/// Test: `warm_boot_*` tests in `commands/start.rs`.
pub fn derive_warm_boot_stages(inputs: WarmBootInputs) -> IndexStages {
    // Issues #1870 / #2203 (joint root cause): a failed durable-corpus open
    // must fail EVERY stage, not just lexical. Before this guard, `semantic`
    // and `graph` were classified purely from `hnsw_snapshot_ready` /
    // `graph_node_count` — signals that are entirely independent of the
    // corpus. A restored HNSW mmap snapshot (or symbol graph) can load
    // successfully even when the redb corpus failed to open (e.g.
    // `DatabaseAlreadyOpen`, #1870), so `semantic` kept reporting `Ready`
    // while the query hot path's `fetch_chunks_for_ids` could never resolve
    // any chunk id against the (unwired) corpus — every HNSW hit was
    // silently dropped at materialisation (`search/materialize.rs`'s "fused
    // id not in corpus" trace), producing HTTP 200 + `results: []` for
    // essentially every query while `/health` / `GET /indexes/:id/status`
    // kept reporting `semantic.status: "ready"` (#2203). Both lanes depend
    // on the same durable corpus for chunk text, so both must fail together
    // with it — this is the `search_capabilities`/corpus-resolvability
    // consistency guard.
    if inputs.corpus_open_failed {
        let reason = "redb corpus could not be opened (incompatible or corrupted format); \
             run `trusty-search index <path> --force` to rebuild from source \
             (issues #1158, #1870, #2203)";
        return IndexStages {
            lexical: StageState::failed(reason),
            semantic: StageState::failed(reason),
            graph: StageState::failed(reason),
        };
    }

    // Issue #1158: corpus open failure must surface as Failed, not InProgress.
    // Before this guard, `chunk_count == 0` (corpus store absent) was treated
    // identically to a freshly-created, never-indexed handle — silently
    // appearing healthy while the durable store was unreadable.
    let lexical = if inputs.chunk_count > 0 {
        StageState {
            status: StageStatus::Ready,
            ..Default::default()
        }
    } else {
        StageState {
            status: StageStatus::InProgress,
            ..Default::default()
        }
    };

    let (semantic, graph) = if inputs.lexical_only {
        (StageState::skipped(), StageState::skipped())
    } else {
        let semantic = if inputs.hnsw_snapshot_ready {
            StageState {
                status: StageStatus::Ready,
                ..Default::default()
            }
        } else {
            StageState::pending()
        };
        let graph = if inputs.skip_kg {
            StageState::skipped()
        } else if inputs.graph_node_count > 0 {
            StageState {
                status: StageStatus::Ready,
                ..Default::default()
            }
        } else {
            StageState::pending()
        };
        (semantic, graph)
    };

    IndexStages {
        lexical,
        semantic,
        graph,
    }
}
