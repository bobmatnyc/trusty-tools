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
    /// `skip_vector` flag (issue #2984 Phase 1): forces semantic stage to
    /// `Skipped`, independently of `skip_kg`.
    pub skip_vector: bool,
    /// Issue #1158: `Some(kind)` when the redb corpus file existed but could
    /// not be opened; `None` when the open succeeded.
    ///
    /// Why: before this signal, a failed corpus open caused `chunk_count = 0`
    /// (no corpus store → `unwrap_or(0)`), which the classifier treated as
    /// `InProgress` — indistinguishable from a freshly-created, never-indexed
    /// handle. Operators saw `chunks=0` in the warm-boot log and the index
    /// looked healthy-ish when it was silently broken. This lets the
    /// classifier emit `StageStatus::Failed` with an actionable reason
    /// instead of the misleading `InProgress` state.
    ///
    /// #4333: widened from a bare `bool` to the classified failure kind. The
    /// bool could only produce ONE reason string, so a transient open timeout
    /// under warm-boot contention was reported as "incompatible or corrupted
    /// format; run `--force` to rebuild" — the destructive remedy for a
    /// corpus that was never even read. Carrying the kind lets each state
    /// report its own remedy.
    pub corpus_open_failure: Option<crate::core::corpus::CorpusOpenFailure>,
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
///   5. `skip_vector == true` → semantic is `Skipped` (issue #2984 Phase 1,
///      independent of `skip_kg`/graph).
///   6. `hnsw_snapshot_ready` → semantic is `Ready`.
///   7. `graph_node_count > 0` → graph is `Ready`.
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
    if let Some(kind) = inputs.corpus_open_failure {
        // #4333: the reason is now derived from the CLASSIFIED failure kind
        // instead of one hardcoded string. The old string named the only
        // permanent cause ("incompatible or corrupted format") and prescribed
        // the only destructive remedy (`--force`) for every case, including a
        // 30 s open timeout under warm-boot contention where the corpus was
        // never read and was verifiably intact (201,206 chunks, 2026-07-29).
        let reason = kind.stage_reason();
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
        // Issue #2984 Phase 1: `skip_vector` forces semantic to `Skipped`,
        // mirroring `skip_kg`'s graph gate below — independently, so the
        // vector-off/KG-on quadrant classifies correctly at warm-boot.
        let semantic = if inputs.skip_vector {
            StageState::skipped()
        } else if inputs.hnsw_snapshot_ready {
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

/// Issue #4680: `true` when an index's stage surface claims lexical work is
/// still owed but no walk has ever been driven for it in this daemon's
/// lifetime — i.e. the index is STUCK, not working.
///
/// Why: [`derive_warm_boot_stages`] rule 3 stamps `lexical = InProgress` for
/// every restored index whose durable corpus is empty, and `IndexStages::
/// lifecycle_status` renders that as `"walking"`. Nothing in the restore path
/// actually schedules a walk, so the label is a claim the daemon never backs
/// up: a production deployment ran 7.6 days with 221 of 222 indexes reporting
/// `lexical: in_progress` / `last_walk_files_seen: 0` / `last_walk_error:
/// null` — the exact byte-for-byte signature of `WalkDiagnostics::default()`
/// on a handle no walk ever touched. Boot reconcile then classified those
/// indexes as `up_to_date` (git path: the HEAD SHA is re-derived from live git
/// at restore, so `stored == current` always — issue #4391) or
/// `skipped_no_data` (mtime path: `last_indexed_unix` has no production
/// writer), so they were never re-driven either. This predicate is the shared
/// definition of that state, consumed by boot reconcile (to retry the walk)
/// and by `GET /health` (to stop reporting such a daemon as healthy).
///
/// What: pure two-signal test.
///   - `lexical == InProgress` — the index CLAIMS a walk is underway. `Ready`
///     means the corpus has chunks; `Failed` means a real failure is already
///     surfaced with an actionable reason (retrying it would loop); `Skipped`
///     is an explicit config choice. `Pending` is deliberately excluded: it is
///     what `create_index` stamps for a brand-new empty index (issue #4110,
///     "at registration nothing has started yet"), a legitimate state whose
///     owner is the API caller about to POST a reindex — and it is also
///     `IndexStages::default()`, so treating it as stuck would flag every
///     bare handle. Only a restore-derived `InProgress` claims work that is
///     not happening, which is the whole defect.
///   - `walk_started == false` — `WalkDiagnostics::last_walk_started_at` is
///     `None`, so no reindex has walked this root since the daemon started.
///     This is what distinguishes "never walked" from "walked and legitimately
///     found zero indexable files" (the latter sets both `last_walk_started_at`
///     and `last_walk_error`), and it bounds the reconcile retry to at most one
///     walk per index per daemon lifetime.
///
/// Test: `stuck_unwalked_matches_only_a_never_walked_in_progress_lane` and
/// `stuck_unwalked_clears_once_a_walk_has_started` in `warm_boot_tests.rs`.
pub fn index_is_stuck_unwalked(lexical: StageStatus, walk_started: bool) -> bool {
    !walk_started && matches!(lexical, StageStatus::InProgress)
}
