//! Session Manager dedicated memory palace + recall/remember wiring (DOC-14 §8).
//!
//! Why: the SM keeps durable, cross-session knowledge — goals, outcomes,
//! decisions — in its OWN memory palace (default `"session-manager"`), strictly
//! separate from per-project palaces. Per spec ref D8.3a the SM must call the
//! memory engine as a DIRECT LIBRARY call (not over MCP) to avoid a network hop
//! and an extra daemon dependency; this module is that direct binding. SM-4
//! lands the palace lifecycle + the four scoped operations (recall, recall_deep,
//! remember, note); SM-7/SM-8 (the endpoint + agent loop) consume them — this
//! ticket deliberately does NOT wire them into any endpoint yet. The §7.5
//! step-3 recall-injection point lives in the future context engine.
//! What: [`SmMemory`] owns a [`PalaceRegistry`] rooted at a data dir and a single
//! [`PalaceId`] (from [`SmMemoryConfig::palace`]). Construction IDEMPOTENTLY
//! ensures that one palace exists (open-from-disk, else create — creating twice
//! yields exactly one palace). Every read/write is scoped to that single palace
//! id, so the SM can never touch another namespace. A backend failure surfaces
//! as a structured [`SmMemoryError`] (no panics, graceful degradation).
//! Test: `#[path = "memory_tests.rs"] mod tests` — palace idempotency, scoped
//! remember→recall round-trip, scope isolation, restart survival, and the
//! "never writes to a non-SM palace" guard.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use trusty_common::memory_core::palace::{Palace, PalaceId, RoomType};
use trusty_common::memory_core::registry::PalaceRegistry;
use trusty_common::memory_core::retrieval::{
    PalaceHandle, RecallResult, RememberOptions, recall_deep_with_default_embedder,
    recall_with_default_embedder,
};

use super::config::SmMemoryConfig;

/// Default importance assigned to SM-remembered drawers.
///
/// Why: SM memories (goals/outcomes/decisions) are curated, high-signal facts —
/// they should rank above incidental auto-capture but need not be pinned at the
/// ceiling. `0.7` matches the importance the agents adapter and the memory-core
/// restart test use for canonical curated content.
/// What: a `0.0..=1.0` weight passed to the memory-core write path; the engine
/// re-clamps defensively.
/// Test: exercised indirectly by every `remember`/`note` test in `memory_tests`.
const SM_DEFAULT_IMPORTANCE: f32 = 0.7;

/// Structured errors for the SM memory subsystem (library code → `thiserror`).
///
/// Why: SM-4 is library code in `trusty-mpm-core`; per the workspace convention
/// library errors are typed `thiserror` enums, never `unwrap()`/`panic!`. A
/// memory backend that is unavailable (corrupt store, missing model, locked
/// palace) must degrade into a clear, matchable error the caller can log and
/// proceed past — not crash the daemon.
/// What: wraps the two failure surfaces — palace lifecycle (`anyhow` from the
/// registry) and recall/remember/note operations — preserving the source chain.
/// Test: `construct_on_unwritable_root_is_error` asserts the `Palace` variant;
/// the happy-path tests assert `Ok`.
#[derive(Debug, thiserror::Error)]
pub enum SmMemoryError {
    /// Ensuring the dedicated palace exists (open-or-create) failed.
    #[error("session-manager palace '{palace}' unavailable: {source}")]
    Palace {
        /// The SM palace id the operation targeted.
        palace: String,
        /// The underlying registry/store error.
        source: anyhow::Error,
    },

    /// A recall / remember / note operation against the SM palace failed.
    #[error("session-manager memory operation '{op}' failed: {source}")]
    Operation {
        /// Short operation tag (`"recall"`, `"recall_deep"`, `"remember"`,
        /// `"note"`) for actionable logs.
        op: &'static str,
        /// The underlying engine error.
        source: anyhow::Error,
    },
}

/// Result alias for SM memory operations.
///
/// Why: keeps the public signatures terse and consistent with the rest of the
/// `sm` module.
/// What: `Result<T, SmMemoryError>`.
/// Test: used throughout `memory_tests`.
pub type SmMemoryResult<T> = std::result::Result<T, SmMemoryError>;

/// Dedicated Session-Manager memory palace, scoped to a single palace id.
///
/// Why: the SM needs durable cross-session memory it fully owns. Binding ONE
/// [`PalaceId`] for the lifetime of the struct makes "SM only ever reads/writes
/// its own palace" a structural invariant rather than a caller discipline —
/// there is no API surface here that accepts a different palace id, so scope
/// leakage is impossible by construction (the headline §8 guarantee).
/// What: holds the open-handle [`PalaceRegistry`], the on-disk `data_root`, the
/// bound `palace_id`, and the configured `recall_top_k`. Built via
/// [`SmMemory::open`], which idempotently ensures the palace exists.
/// Test: `palace_create_is_idempotent`, `remember_then_recall_round_trips`,
/// `recall_is_scoped_to_sm_palace`, `data_survives_fresh_construction`,
/// `writes_target_only_the_sm_palace` in `memory_tests`.
#[derive(Clone)]
pub struct SmMemory {
    /// Open-handle registry (LRU-bounded). Cheap to clone — state is behind `Arc`.
    registry: PalaceRegistry,
    /// Directory under which the SM palace's `<id>/` subtree lives on disk.
    data_root: PathBuf,
    /// The single palace this SM instance is bound to. Never changes.
    palace_id: PalaceId,
    /// Number of recall hits to return (from [`SmMemoryConfig::recall_top_k`]).
    recall_top_k: usize,
}

impl SmMemory {
    /// Open the SM memory subsystem, idempotently ensuring its palace exists.
    ///
    /// Why: startup must guarantee the dedicated palace is present without
    /// erroring when it already exists (the SM may restart against a populated
    /// store). Calling this twice — in one process or across restarts — must
    /// leave EXACTLY ONE palace on disk, never duplicate or wipe it.
    /// What: derives `palace_id` from `cfg.palace`, then: if
    /// `<data_root>/<id>/palace.json` already exists, opens it from disk;
    /// otherwise creates and persists a fresh palace. Both paths register the
    /// handle in the registry. Any failure is wrapped in
    /// [`SmMemoryError::Palace`].
    /// Test: `palace_create_is_idempotent` (two `open` calls → one palace),
    /// `data_survives_fresh_construction` (restart survival).
    pub fn open(data_root: impl Into<PathBuf>, cfg: &SmMemoryConfig) -> SmMemoryResult<Self> {
        let data_root = data_root.into();
        let palace_id = PalaceId::new(cfg.palace.clone());
        let recall_top_k = cfg.recall_top_k as usize;

        let registry =
            PalaceRegistry::open(&data_root).map_err(|source| SmMemoryError::Palace {
                palace: palace_id.to_string(),
                source,
            })?;

        let me = Self {
            registry,
            data_root,
            palace_id,
            recall_top_k,
        };
        // Eagerly materialise the palace so the first recall/remember cannot race
        // creation and so idempotency is observable immediately after `open`.
        me.ensure_palace()?;
        Ok(me)
    }

    /// The palace id this instance is bound to (read-only accessor).
    ///
    /// Why: tests and callers (and the future §7.5 injection point) need to
    /// confirm which namespace the SM is scoped to without reaching into private
    /// state; exposing the bound id makes the "SM-only" contract auditable.
    /// What: returns the bound [`PalaceId`].
    /// Test: `writes_target_only_the_sm_palace` asserts this equals the
    /// configured name.
    pub fn palace_id(&self) -> &PalaceId {
        &self.palace_id
    }

    /// Open-or-create the bound palace, returning a live handle.
    ///
    /// Why: every operation needs a handle; centralising the idempotent
    /// open-or-create here means a single, well-tested code path enforces
    /// "exactly one palace" for both construction and every later call. The
    /// open-or-create must also be RACE-SAFE: two concurrent first-starts
    /// against the same data root (e.g. two SM processes, or two threads) must
    /// converge on ONE palace without either erroring. The previous
    /// `exists()`-then-branch form had a TOCTOU window where both callers could
    /// observe "missing" and both take the create branch; this form closes it.
    /// What: returns the cached handle if registered (fast path). Otherwise
    /// tries `open_palace` first — which succeeds whenever `palace.json` is
    /// already on disk, including the case where a concurrent creator just
    /// finished writing it. If the open fails (the normal first-run case: no
    /// metadata yet), it falls back to `create_palace`, which is itself
    /// idempotent (`create_dir_all` + an atomic `palace.json` rename), so two
    /// racing creators both succeed and the registry de-duplicates by id. Both
    /// branches are scoped entirely to `self.palace_id` — no other id is
    /// reachable. Any final failure is wrapped in [`SmMemoryError::Palace`].
    /// Test: `palace_create_is_idempotent`, `writes_target_only_the_sm_palace`,
    /// `ensure_palace_falls_back_to_open_when_palace_already_exists`.
    fn ensure_palace(&self) -> SmMemoryResult<Arc<PalaceHandle>> {
        if let Some(handle) = self.registry.get(&self.palace_id) {
            return Ok(handle);
        }

        // Open-first, create-on-failure: no `exists()` pre-check, so there is no
        // TOCTOU window. `open_palace` resolves the common "already on disk"
        // case (including a concurrent creator that has finished); only when it
        // fails do we attempt the idempotent create.
        if let Ok(handle) = self.registry.open_palace(&self.data_root, &self.palace_id) {
            return Ok(handle);
        }

        let palace = Palace {
            id: self.palace_id.clone(),
            name: "Session Manager".to_string(),
            description: Some(
                "Dedicated Session-Manager memory palace (DOC-14 §8): goals, \
                 outcomes, and decisions across sessions."
                    .to_string(),
            ),
            created_at: Utc::now(),
            data_dir: self.data_root.join(self.palace_id.as_str()),
        };
        self.registry
            .create_palace(&self.data_root, palace)
            .map_err(|source| SmMemoryError::Palace {
                palace: self.palace_id.to_string(),
                source,
            })
    }

    /// Recall the top-k SM memories matching `query` (standard L0+L1+L2 path).
    ///
    /// Why: feeds the SM's working context with its most relevant durable
    /// knowledge (§8 / the future §7.5 step-3 injection). Scoped to the SM
    /// palace only, so recall can never surface another namespace's facts.
    /// What: ensures the palace, then runs memory-core's
    /// `recall_with_default_embedder` against the bound handle with the
    /// configured `recall_top_k`. Returns the ranked [`RecallResult`]s.
    /// Test: `remember_then_recall_round_trips`, `recall_is_scoped_to_sm_palace`.
    pub async fn recall(&self, query: &str) -> SmMemoryResult<Vec<RecallResult>> {
        let handle = self.ensure_palace()?;
        recall_with_default_embedder(&handle, query, self.recall_top_k)
            .await
            .map_err(|source| SmMemoryError::Operation {
                op: "recall",
                source,
            })
    }

    /// Deep recall of the top-k SM memories (L0+L1+L3 path).
    ///
    /// Why: when the SM explicitly wants a heavier, full-corpus search (not the
    /// metadata-filtered L2), this exposes the engine's deep path — still scoped
    /// strictly to the SM palace.
    /// What: ensures the palace, then delegates to
    /// `recall_deep_with_default_embedder` with the configured `recall_top_k`.
    /// Test: `recall_deep_round_trips`.
    pub async fn recall_deep(&self, query: &str) -> SmMemoryResult<Vec<RecallResult>> {
        let handle = self.ensure_palace()?;
        recall_deep_with_default_embedder(&handle, query, self.recall_top_k)
            .await
            .map_err(|source| SmMemoryError::Operation {
                op: "recall_deep",
                source,
            })
    }

    /// Remember a piece of prose (goal / outcome / decision) into the SM palace.
    ///
    /// Why: the SM persists durable cross-session knowledge through this single
    /// write path. Routing every write through the bound palace id guarantees
    /// the SM never writes elsewhere.
    /// What: ensures the palace, then calls `PalaceHandle::remember` in the
    /// `General` room at [`SM_DEFAULT_IMPORTANCE`] with no extra tags. Returns
    /// the new drawer's UUID as a string.
    /// Test: `remember_then_recall_round_trips`, `writes_target_only_the_sm_palace`.
    pub async fn remember(&self, text: impl Into<String>) -> SmMemoryResult<String> {
        let handle = self.ensure_palace()?;
        handle
            .remember(
                text.into(),
                RoomType::General,
                Vec::new(),
                SM_DEFAULT_IMPORTANCE,
            )
            .await
            .map(|id| id.to_string())
            .map_err(|source| SmMemoryError::Operation {
                op: "remember",
                source,
            })
    }

    /// Note a short, curated SM fact, bypassing only the min-token gate.
    ///
    /// Why: SM decisions are sometimes terse ("Chose worktree-per-ticket") and
    /// would trip the engine's token-length filter under the normal `remember`
    /// path; `note` uses the curated-fact preset so short high-signal facts land
    /// while noise patterns are still rejected. Scoped to the SM palace only.
    /// What: ensures the palace, then calls `remember_with_options` with
    /// [`RememberOptions::note`] (pins `UserFact`, skips the token check). Returns
    /// the new drawer's UUID as a string.
    /// Test: `note_stores_short_fact`, `writes_target_only_the_sm_palace`.
    pub async fn note(&self, text: impl Into<String>) -> SmMemoryResult<String> {
        let handle = self.ensure_palace()?;
        handle
            .remember_with_options(
                text.into(),
                RoomType::General,
                Vec::new(),
                SM_DEFAULT_IMPORTANCE,
                RememberOptions::note(),
            )
            .await
            .map(|id| id.to_string())
            .map_err(|source| SmMemoryError::Operation { op: "note", source })
    }

    /// Remember a tagged, short-fact-tolerant entry into the SM palace.
    ///
    /// Why: SM-6 persists each goal as a STRUCTURED palace entry it must later
    /// enumerate deterministically (not via fuzzy recall) to rebuild the hot
    /// cache on startup. Enumeration keys on a stable tag, so the write path must
    /// accept explicit tags; and a serialised goal can be short (a one-line
    /// description), so — like [`SmMemory::note`] — it must bypass the
    /// min-token gate. Routing through the bound palace id keeps the SM-only
    /// scope invariant intact.
    /// What: ensures the palace, then calls `remember_with_options` in the
    /// `General` room at [`SM_DEFAULT_IMPORTANCE`] with the supplied `tags` and
    /// the curated-fact preset ([`RememberOptions::note`], which pins `UserFact`
    /// and skips the token check). Returns the new drawer's UUID as a string.
    /// Test: `goals` module rebuild/dual-persistence tests drive this via the
    /// `GoalMemory` seam; `writes_target_only_the_sm_palace` covers scope.
    pub async fn remember_tagged(
        &self,
        text: impl Into<String>,
        tags: Vec<String>,
    ) -> SmMemoryResult<String> {
        let handle = self.ensure_palace()?;
        handle
            .remember_with_options(
                text.into(),
                RoomType::General,
                tags,
                SM_DEFAULT_IMPORTANCE,
                RememberOptions::note(),
            )
            .await
            .map(|id| id.to_string())
            .map_err(|source| SmMemoryError::Operation {
                op: "remember_tagged",
                source,
            })
    }

    /// Enumerate the content of every SM-palace entry carrying `tag`.
    ///
    /// Why: rebuilding the SM goal cache on startup (§9.4) must be DETERMINISTIC
    /// and COMPLETE — every persisted goal, not the embedding-ranked top-k a
    /// `recall` would return. `list_drawers` is the exact-match, non-fuzzy
    /// enumeration that gives that guarantee, scoped strictly to the bound SM
    /// palace.
    /// What: ensures the palace, then lists drawers filtered to `tag` (no room
    /// filter, a generous cap), returning each matching drawer's raw `content`
    /// string. The goal store deserialises these back into `Goal`s. No fuzzy
    /// scoring is involved.
    /// Test: `goals` module rebuild tests (`rebuild_*`) assert the full set
    /// round-trips through this enumeration.
    pub fn list_tagged(&self, tag: &str) -> SmMemoryResult<Vec<String>> {
        let handle = self.ensure_palace()?;
        // A cap far above any realistic live-goal count; `list_drawers` returns
        // up to this many, sorted by importance (irrelevant here — we take all).
        const LIST_CAP: usize = 100_000;
        Ok(handle
            .list_drawers(None, Some(tag.to_string()), LIST_CAP)
            .into_iter()
            .map(|d| d.content)
            .collect())
    }

    /// Number of palaces currently persisted under the SM data root.
    ///
    /// Why: the idempotency contract ("create twice → one palace") is most
    /// directly asserted by counting palaces on disk; exposing this keeps the
    /// test honest without reaching into registry internals.
    /// What: lists persisted palaces via [`PalaceRegistry::list_palaces`] and
    /// returns the count. Wrapped errors use [`SmMemoryError::Palace`].
    /// Test: `palace_create_is_idempotent`, `writes_target_only_the_sm_palace`.
    pub fn persisted_palace_count(&self) -> SmMemoryResult<usize> {
        Self::count_persisted(&self.data_root, &self.palace_id)
    }

    /// Count persisted palaces under `data_root` (internal helper).
    ///
    /// Why: factors the registry-listing + error-mapping out of
    /// [`SmMemory::persisted_palace_count`] so the public accessor stays a thin
    /// one-liner. Private by design — the only caller is `persisted_palace_count`
    /// (which the idempotency tests drive); it takes the path/id as arguments
    /// purely to avoid borrowing `self` twice, not to expose a standalone API.
    /// What: delegates to [`PalaceRegistry::list_palaces`]; maps failure to
    /// [`SmMemoryError::Palace`] tagged with `palace_id`.
    /// Test: `palace_create_is_idempotent` (via `persisted_palace_count`).
    fn count_persisted(data_root: &Path, palace_id: &PalaceId) -> SmMemoryResult<usize> {
        PalaceRegistry::list_palaces(data_root)
            .map(|palaces| palaces.len())
            .map_err(|source| SmMemoryError::Palace {
                palace: palace_id.to_string(),
                source,
            })
    }
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
