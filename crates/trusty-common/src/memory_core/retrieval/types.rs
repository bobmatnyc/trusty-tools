//! Core retrieval types: layer result structs, RememberOptions, stubs.
//!
//! Why: Extracted from retrieval/mod.rs to keep each file under the 500-SLOC
//! cap (#607). Pure data types with no heavy logic.
//! What: `L0Identity`, `L1Essential`, `RecallResult`, `L1_CAP`,
//! `RememberOptions`, `CrossPalaceResult`, `RetrievalLayers`.
//! Test: Types are exercised by retrieval::tests and callers throughout the
//! workspace.

use crate::memory_core::filter::FilterConfig;
use crate::memory_core::palace::{Drawer, DrawerType, PalaceId};
use anyhow::Result;
use std::path::Path;

/// L0 — palace identity. Tiny (~100 tokens), always loaded, read from
/// `<data_dir>/identity.txt` on palace open.
pub struct L0Identity {
    pub content: String,
}

/// L1 — essential drawers (top-15 by importance, ~800 tokens), pre-computed
/// at write time and cached on the `PalaceHandle`.
pub struct L1Essential {
    pub drawers: Vec<Drawer>,
}

/// A single ranked memory result produced by any retrieval layer.
///
/// Why: All four layers need to produce a comparable, layer-tagged result so
/// callers can stitch them together and present consistent context to the LLM.
/// What: Bundles the matched drawer with an effective score (importance times
/// vector similarity for L2/L3, importance for L1, fixed 1.0 for L0) and the
/// originating layer index.
/// Test: See `l0_l1_always_present` and `l2_returns_relevant_drawer`.
#[derive(Debug, Clone)]
pub struct RecallResult {
    pub drawer: Drawer,
    pub score: f32,
    pub layer: u8,
}

/// Maximum number of drawers held in the L1 cache.
pub(super) const L1_CAP: usize = 15;

/// Options for `PalaceHandle::remember_with_options` (issue #61).
///
/// Why: The signal/noise gate, the curated-fact escape hatch (`memory_note`),
/// and the unconditional `force` override all share the same write pipeline.
/// Bundling them lets future knobs (e.g. per-call decay overrides) attach
/// without breaking the call surface again.
/// What: `filter` defaults to `FilterConfig::default()`; `force` skips the
/// gate entirely; `enforce_min_tokens` lets `memory_note` keep noise rejects
/// while accepting short content; `classify_as` pins the resulting
/// `DrawerType` (used by `memory_note` to force `UserFact`);
/// `defer_embedding` (issue #1970) tells `remember_with_options` to skip the
/// synchronous embed + vector-store upsert and instead background it, so a
/// caller whose embedder is still cold-initialising is not blocked behind a
/// 30-120s ONNX/CoreML compile just to persist a drawer.
/// Test: See `remember_force_bypasses_filter` and friends in this file;
/// `deferred_embed_backfills_vector_once_embedder_ready` covers
/// `defer_embedding`.
#[derive(Debug, Clone)]
pub struct RememberOptions {
    pub filter: FilterConfig,
    pub force: bool,
    pub enforce_min_tokens: bool,
    pub classify_as: Option<DrawerType>,
    pub defer_embedding: bool,
}

impl Default for RememberOptions {
    fn default() -> Self {
        Self {
            filter: FilterConfig::default(),
            force: false,
            enforce_min_tokens: true,
            classify_as: None,
            defer_embedding: false,
        }
    }
}

impl RememberOptions {
    /// Preset for the `memory_note` curated-fact path.
    ///
    /// Why: `memory_note` stores short, high-signal facts ("User prefers
    /// snake_case") that would otherwise trip the token threshold. The
    /// noise-pattern rejects still apply so the tool can't be used to
    /// silently store auto-capture garbage.
    /// What: Disables `enforce_min_tokens` and pins `classify_as =
    /// UserFact`. Leaves `filter` at the default so noise patterns still
    /// reject.
    /// Test: `note_options_skip_token_check_but_keep_noise_filter`.
    pub fn note() -> Self {
        Self {
            filter: FilterConfig::default(),
            force: false,
            enforce_min_tokens: false,
            classify_as: Some(DrawerType::UserFact),
            defer_embedding: false,
        }
    }

    /// Preset that bypasses every filter (the `force = true` MCP arg).
    pub fn forced() -> Self {
        Self {
            force: true,
            ..Self::default()
        }
    }
}

/// A cross-palace recall result, tagging each ranked drawer with its source
/// palace id so callers can attribute hits back to a namespace.
///
/// Why: When agents fan out a query across every palace on the machine, the
/// raw `RecallResult` loses the namespace signal — without the palace id the
/// caller cannot decide which palace a fact lives in. Wrapping rather than
/// extending `RecallResult` keeps single-palace call sites untouched.
/// What: Bundles the originating `palace_id` (kebab-case string) with the
/// underlying `RecallResult`.
/// Test: `recall_across_palaces_merges_results` asserts both palace ids appear
/// in the merged output.
#[derive(Debug, Clone)]
pub struct CrossPalaceResult {
    pub palace_id: String,
    pub result: RecallResult,
}

// -- Legacy stubs (kept for backwards compatibility with existing callers) --

pub struct RetrievalLayers;

impl RetrievalLayers {
    /// Load L0 identity for a palace.
    ///
    /// Why: Provides a stable persona / project description that grounds every
    /// reply, without taking up real context budget.
    /// What: Reads `identity.txt` from the palace data dir; returns empty
    /// content if the file does not yet exist.
    /// Test: For a freshly created palace dir, returns `L0Identity { content: "" }`.
    pub async fn load_l0(_palace_data_dir: &Path) -> Result<L0Identity> {
        Ok(L0Identity {
            content: String::new(),
        })
    }

    /// Load L1 essential drawers.
    ///
    /// Why: Top-importance drawers are queried on virtually every request, so
    /// we want them already in memory and pre-ranked.
    /// What: Returns the top-15 drawers across the palace, sorted by importance.
    /// Test: For an empty palace, returns `L1Essential { drawers: [] }`.
    pub async fn load_l1(_palace_id: &PalaceId) -> Result<L1Essential> {
        Ok(L1Essential {
            drawers: Vec::new(),
        })
    }
}
