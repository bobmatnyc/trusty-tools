//! Reindex lifecycle phases and bar-slot mapping.
//!
//! Why: the phase enum and its bar-slot mapping are referenced by three separate
//! files (`bars.rs`, `timings.rs`, and the `reindex_engine` submodules). Placing
//! them in their own module avoids circular dependencies and makes the phase
//! semantics independently reviewable.
//!
//! What: `ReindexPhase` encodes the lifecycle of a foreground reindex as a
//! strongly-typed value; `phase_to_bar_slot` maps each phase to the indicatif
//! bar slot it drives (0–3, or `None` for phases with no dedicated bar).
//!
//! Test: `tests::phase_labels_are_stable` and `tests::phase_to_bar_slot_coverage`
//! pin every string and slot mapping to prevent silent regressions.

// ─── Phase enum ──────────────────────────────────────────────────────────────

/// Distinct phases of a reindex, each corresponding to one of the 4 sequential
/// progress bars shown in the CLI.
///
/// Why: encodes the lifecycle of a reindex as a strongly-typed value so the
/// event loop in `reindex_engine.rs` can call `set_phase(…)` without magic
/// strings.  Issue #401: four named bars replace the single relabelled bar of
/// the previous design (issue #317).
///
/// `ParseEmbed` is kept as a legacy alias for `Embedding` so existing tests
/// that used the old variant compile without modification.
///
/// What: each variant maps to a human-readable label via `label()`.
/// Test: `tests::phase_labels_are_stable` pins every label string.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReindexPhase {
    /// Waiting for the daemon's first SSE event.
    Connecting,
    /// Stage 1 — walking the source tree (→ Crawl bar).
    Walking,
    /// Stage 2 — parse sub-step that runs before the first batch event.
    Chunking,
    /// Embedder sidecar is spawning / ONNX model is loading (→ Chunk bar shows
    /// "Loading model…" instead of a frozen 0/N count during the ~30-45s stall).
    ///
    /// Why: the `trusty-embedderd` sidecar is spawned on the first embed request
    /// (lazy-spawn, issue #315).  This cold-start includes subprocess fork +
    /// ONNX model load + CoreML/CUDA provider init, which can take 30–60 s with
    /// no user-visible progress.  The `embedder_init` SSE event (emitted by the
    /// daemon before the first embed call) transitions the header to this phase
    /// so the operator sees "Loading model…" instead of a frozen Chunk bar at
    /// 0/N for nearly a minute.
    InitializingEmbedder,
    /// Stage 3 — parse + embed per batch (→ Embed bar).
    Embedding,
    /// Legacy alias for `Embedding`; retained for backward compatibility.
    ParseEmbed,
    /// Stage 4 — knowledge-graph rebuild (→ KG bar).
    KnowledgeGraph,
    /// Building the BM25 lexical index (fused into batches; no separate bar).
    Bm25,
    /// Upserting embedding vectors (fused into batches; no separate bar).
    Upsert,
    /// Terminal: the reindex finished.
    Done,
}

impl ReindexPhase {
    /// Human-readable phase label rendered on the header line.
    ///
    /// Why: keeps all user-facing strings in one place so a rename is a single
    /// reviewed change rather than a grep hunt.
    /// What: returns a `&'static str` label for each phase variant.
    /// Test: `tests::phase_labels_are_stable` pins every string.
    pub(crate) fn label(self) -> &'static str {
        match self {
            ReindexPhase::Connecting => "Connecting to daemon\u{2026}",
            ReindexPhase::Walking => "Walking files\u{2026}",
            ReindexPhase::Chunking => "Chunking\u{2026}",
            ReindexPhase::InitializingEmbedder => "Loading model\u{2026}",
            ReindexPhase::Embedding => "Embedding chunks\u{2026}",
            ReindexPhase::ParseEmbed => "Embedding chunks\u{2026}",
            ReindexPhase::Bm25 => "Building BM25 index\u{2026}",
            ReindexPhase::KnowledgeGraph => "Building knowledge graph\u{2026}",
            ReindexPhase::Upsert => "Upserting vectors\u{2026}",
            ReindexPhase::Done => "Done",
        }
    }
}

// ─── Bar-slot mapping ─────────────────────────────────────────────────────────

/// Which of the 4 stage bars a given phase maps to.
///
/// Why: the 4-bar layout has Crawl/Chunk/Embed/KG slots (bars 0–3).  Not every
/// `ReindexPhase` drives a bar (e.g. `Bm25` and `Upsert` are fused into
/// batches and have no dedicated bar), so this mapping lives here rather than
/// on the enum itself.
/// What: returns `Some(0..=3)` for the four concrete stages, `None` for
/// everything else (the caller leaves the bar layout unchanged).
/// Test: `tests::phase_to_bar_slot_coverage` asserts every variant.
pub(crate) fn phase_to_bar_slot(phase: ReindexPhase) -> Option<usize> {
    match phase {
        ReindexPhase::Walking => Some(0),
        // InitializingEmbedder shares the Chunk bar slot: the Chunk bar is
        // already active at 0/N when the embedder spawn begins, so keeping the
        // focus on slot 1 avoids a visual jump.  The header spinner transitions
        // to "Loading model…" so the operator knows exactly why the bar is stuck.
        ReindexPhase::Chunking | ReindexPhase::InitializingEmbedder => Some(1),
        ReindexPhase::Embedding | ReindexPhase::ParseEmbed => Some(2),
        ReindexPhase::KnowledgeGraph => Some(3),
        _ => None,
    }
}
