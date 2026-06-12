//! Phase-discriminant encoding shared between the SSE event loop and the
//! wall-clock ticker.
//!
//! Why: the ticker must read the current phase without locking `ReindexUi`.
//! Encoding the `ReindexPhase` as a small integer in an `AtomicU64` lets the
//! single-writer event loop publish the phase and the ticker read it back to
//! produce a footer label that always matches the header.
//! What: `phase_to_u64` maps a phase to its discriminant; `u64_to_label` maps
//! the discriminant back to its display label.
//! Test: round-trip covered indirectly by `tests::eta_logic_loading_model_and_zero_denom`.

use crate::commands::reindex_ui::ReindexPhase;

/// Encode a [`ReindexPhase`] as a stable discriminant for the ticker atomic.
///
/// Why: lets the ticker observe the active phase without locking `ReindexUi`.
/// What: maps each phase to a fixed `u64`; `Embedding`/`ParseEmbed` collapse to
/// `4` and any other variant defaults to `4` (Embedding).
/// Test: paired with `u64_to_label` in the ticker; exercised by ETA-logic test.
///
/// Encoding: we (ab)use AtomicU64 to carry a discriminant.  The mapping is:
///   0 = Connecting, 1 = Walking, 2 = Chunking, 3 = InitializingEmbedder,
///   4 = Embedding, 5 = KnowledgeGraph  (other variants map to 4 as default)
pub(super) fn phase_to_u64(p: ReindexPhase) -> u64 {
    use ReindexPhase as P;
    match p {
        P::Connecting => 0,
        P::Walking => 1,
        P::Chunking => 2,
        P::InitializingEmbedder => 3,
        P::Embedding | P::ParseEmbed => 4,
        P::KnowledgeGraph => 5,
        _ => 4,
    }
}

/// Decode a phase discriminant back to its display label.
///
/// Why: the ticker renders a footer label matching the header by reading the
/// shared discriminant and resolving it back to `ReindexPhase::label()`.
/// What: maps each discriminant to the corresponding phase label; unknown
/// values fall back to the `Embedding` label.
/// Test: paired with `phase_to_u64`; exercised via the ticker.
pub(super) fn u64_to_label(v: u64) -> &'static str {
    use ReindexPhase as P;
    match v {
        0 => P::Connecting.label(),
        1 => P::Walking.label(),
        2 => P::Chunking.label(),
        3 => P::InitializingEmbedder.label(),
        5 => P::KnowledgeGraph.label(),
        _ => P::Embedding.label(),
    }
}
