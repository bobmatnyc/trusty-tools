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
}

/// Pure classifier: given on-disk signals, derive the [`IndexStages`] for a
/// restored index handle.
///
/// Why (issue #135): see [`WarmBootInputs`].
/// What: applies rules in order:
///   1. `lexical_only == true` → semantic + graph are `Skipped`.
///   2. `chunk_count > 0` → lexical is `Ready`.
///   3. `chunk_count == 0` → lexical is `InProgress`.
///   4. `hnsw_snapshot_ready` → semantic is `Ready`.
///   5. `graph_node_count > 0` → graph is `Ready`.
///
/// Test: `warm_boot_*` tests in `commands/start.rs`.
pub fn derive_warm_boot_stages(inputs: WarmBootInputs) -> IndexStages {
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
