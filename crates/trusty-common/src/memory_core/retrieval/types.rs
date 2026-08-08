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

/// What a `PalaceHandle::forget` call actually did.
///
/// Why (#5231): `forget` used to return `Result<()>`, where `Ok` meant only
/// "nothing panicked" — `drawers.retain` never reports whether it matched, so
/// forgetting an id that was never stored looked identical to a real delete.
/// Every caller that counts or reports deletions was therefore counting
/// attempts. This makes the distinction part of the return type so a caller
/// cannot ignore it by accident.
/// What: `Deleted` when a drawer row was present and has been removed;
/// `NotFound` when no drawer with that id existed in the palace.
/// Test: `forget_reports_not_found_for_an_unknown_drawer` and
/// `forget_reports_deleted_and_the_drawer_stays_gone_after_reopen`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgetOutcome {
    /// A drawer with this id existed and was removed.
    Deleted,
    /// No drawer with this id existed; nothing was removed.
    NotFound,
}

impl ForgetOutcome {
    /// True only when a drawer was actually removed.
    pub fn is_deleted(self) -> bool {
        matches!(self, Self::Deleted)
    }

    /// Wire/CLI spelling: `"deleted"` or `"not_found"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deleted => "deleted",
            Self::NotFound => "not_found",
        }
    }
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
/// QUALITY gates (noise patterns, short-content, non-alphabetic ratio) —
/// see [`Self::allow_secret_like`] for the secret gate, which `force` does
/// NOT skip; `enforce_min_tokens` lets `memory_note` keep noise rejects
/// while accepting short content; `classify_as` pins the resulting
/// `DrawerType` (used by `memory_note` to force `UserFact`);
/// `defer_embedding` (issue #1970) tells `remember_with_options` to skip the
/// synchronous embed + vector-store upsert and instead background it, so a
/// caller whose embedder is still cold-initialising is not blocked behind a
/// 30-120s ONNX/CoreML compile just to persist a drawer.
/// Test: See `remember_force_bypasses_filter` and friends in this file;
/// `deferred_embed_backfills_vector_once_embedder_ready` covers
/// `defer_embedding`; `remember_force_still_blocks_secret`,
/// `remember_force_and_allow_secret_like_stores_secret_shaped_content` cover
/// the two-tier `force`/`allow_secret_like` split (issue #2520).
#[derive(Debug, Clone)]
pub struct RememberOptions {
    pub filter: FilterConfig,
    pub force: bool,
    pub enforce_min_tokens: bool,
    pub classify_as: Option<DrawerType>,
    pub defer_embedding: bool,
    /// Explicit, separate opt-in that bypasses the SECRET gate specifically.
    ///
    /// Why (issue #2520): `force = true` originally bypassed every
    /// content-quality gate uniformly, including secret detection — which
    /// meant an automated writer that always sets `force` (e.g.
    /// trusty-code's per-turn memory sink, which needs deterministic
    /// storage and cannot tolerate quality-gate false positives) also
    /// silently disabled credential screening on every write. Splitting
    /// `force` (quality gates) from `allow_secret_like` (the secret gate)
    /// means the common case — "store this even though it looks noisy" —
    /// stays safe by default, and only a caller that DELIBERATELY wants to
    /// persist secret-shaped content (rare; e.g. a redacted example that
    /// still matches the credential heuristic) has to opt in explicitly.
    /// What: when `false` (the default), `remember_with_options` runs
    /// [`crate::memory_core::filter::check_secret`] even when `force` is
    /// `true`, and returns `Err` on a hit. When `true`, the secret gate is
    /// skipped entirely (in addition to whatever `force` already skips).
    /// Has no effect when `force` is `false` — in that case the secret gate
    /// already runs as part of the normal `FilterConfig::apply` pipeline
    /// regardless of this flag.
    /// Test: `remember_force_still_blocks_secret`,
    /// `remember_force_and_allow_secret_like_stores_secret_shaped_content`.
    pub allow_secret_like: bool,
    /// Wing the drawer's room belongs to (ADR-0027 T9).
    ///
    /// Why: without this a non-default wing could never receive a drawer, and
    /// wing-scoped recall would be permanently empty outside the default wing
    /// — a level nobody can write to is the same defect as a level nobody
    /// reads. This is the write half of the "who" axis.
    /// What: `None` (the default) resolves the room in the palace's default
    /// wing, which is byte-identical to the pre-T9 behaviour every existing
    /// caller gets. `Some(wing_id)` resolves it in that wing instead, so
    /// `engineer`/`Planning` and `pm`/`Planning` are two distinct rooms.
    /// Test: `wing_scoped_recall_returns_only_that_wing`,
    /// `unscoped_write_still_lands_in_the_default_wing`.
    pub wing_id: Option<uuid::Uuid>,
    /// ADR-0028 D5 slot name this write claims (#4886).
    ///
    /// Why: naming a slot is what makes a write a Tier C ("current fact")
    /// write. It is not a hint — writing an occupied slot atomically retires
    /// the incumbent, which is what keeps the store self-limiting: fifty writes
    /// of `pr:4818/state` leave one live fact, not fifty stale ones. The field
    /// lives here rather than on a separate parameter so every existing caller
    /// keeps today's behaviour by construction (`None` = no slot = Tier E) and
    /// so the one enforcement point, `remember_with_options`, sees it.
    /// What: `None` (the default) is not a Tier C request at all. `Some(key)`
    /// runs the fail-closed admission gate
    /// ([`crate::memory_core::retrieval::admit_tier_c`]): the key must match
    /// `<domain>:<id>/<aspect>` and resolve a retirement condition, or the
    /// drawer is written as an ordinary Tier E drawer with no slot.
    /// Test: `tier_c_write_retires_the_prior_slot_occupant`,
    /// `malformed_fact_key_degrades_to_tier_e`.
    pub fact_key: Option<String>,
    /// Explicit retirement instant for this write (ADR-0028 D4, condition 1).
    ///
    /// Why: `Drawer::expires_at` already existed and #4885 made it enforced at
    /// read time, but no caller could set it — the write chain simply had no
    /// parameter for it, so 99.6% of the estate's point-in-time drawers carry
    /// no TTL at all. This is the missing half.
    /// What: `None` with a `fact_key` takes the 24-hour Tier C default; `None`
    /// without one leaves whatever policy `Drawer::with_type` applies (the
    /// 7-day `SessionEvent` TTL), byte-identically to today. `Some(t)` sets the
    /// drawer's TTL directly; combined with a `fact_key`, a `t` at or before
    /// now is REFUSED rather than admitted, because a fact born expired
    /// declares no live window.
    /// Test: `explicit_expiry_is_honoured_without_a_fact_key`,
    /// `already_elapsed_expiry_degrades_to_tier_e`.
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Default for RememberOptions {
    fn default() -> Self {
        Self {
            filter: FilterConfig::default(),
            force: false,
            enforce_min_tokens: true,
            classify_as: None,
            defer_embedding: false,
            allow_secret_like: false,
            wing_id: None,
            // #4886: no slot and no TTL — an ordinary Tier E drawer, which is
            // what every caller that predates ADR-0028 gets.
            fact_key: None,
            expires_at: None,
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
            allow_secret_like: false,
            wing_id: None,
            fact_key: None,
            expires_at: None,
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
