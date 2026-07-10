//! Palace-memory seam for the SM goal store (DOC-14 §9.4).
//!
//! Why: the goal store treats the SM palace as its source of truth, but it must
//! (a) be UNIT-TESTABLE without the heavy ONNX-backed Memory Palace, and (b)
//! depend on an ABSTRACTION rather than the concrete `SmMemory` (which lives
//! behind the `sm-memory` feature) so the store compiles and is testable on the
//! default build. This module is that seam: a tiny async trait the store writes
//! through, implemented by `SmMemory` under `--features sm-memory` and by an
//! in-memory mock in tests. Per §9.4 the palace is durable truth and `goals.json`
//! is a cache rebuilt from it, so the trait surfaces exactly two operations — a
//! tagged write and a tag-scoped enumeration — and nothing else.
//! What: defines [`GoalMemory`] (two async methods returning a `String` error so
//! the trait stays feature-independent) and, behind `sm-memory`, implements it for
//! [`super::super::memory::SmMemory`] by delegating to its `remember_tagged` /
//! `list_tagged` scoped-write/enumerate methods.
//! Test: the mock impl in `goals/store_tests.rs` exercises the store; the real
//! `SmMemory` impl is covered by the `#[ignore]`-free seam delegation plus the
//! SM-4 scope tests it reuses.

use async_trait::async_trait;

/// The stable palace tag every SM goal entry carries (DOC-14 §9.4).
///
/// Why: rebuilding the cache enumerates goal entries by an exact tag (not fuzzy
/// recall), so the write and the enumerate must agree on one constant. Isolating
/// it here makes the contract auditable and impossible to typo apart.
/// What: the `"sm-goal"` tag attached to every persisted goal drawer and used as
/// the [`GoalMemory::list_goals`] filter.
/// Test: `goals/store_tests.rs` round-trips writes and reads through this tag via
/// the mock; the production path reuses the same constant.
pub const GOAL_TAG: &str = "sm-goal";

/// Abstraction over the SM palace for goal persistence (the source of truth).
///
/// Why: decouples [`super::store::SmGoalStore`] from the concrete, feature-gated
/// `SmMemory` (Dependency Inversion) so the store is testable with a mock and
/// builds without the `sm-memory` feature. The two methods are the complete set
/// the store needs: persist a serialised goal (tagged for later enumeration) and
/// enumerate every persisted goal deterministically on startup.
/// What: an async trait with `remember_goal` (write one tagged JSON entry) and
/// `list_goals` (return every entry's raw JSON for `tag`). Errors are `String`s so
/// the trait carries no dependency on the feature-gated `SmMemoryError`.
/// Test: the mock in `goals/store_tests.rs`; the `SmMemory` impl below.
#[async_trait]
pub trait GoalMemory: Send + Sync {
    /// Persist a serialised goal as a tagged palace entry (source of truth).
    ///
    /// Why: every goal mutation re-writes the goal's current JSON to the palace so
    /// the palace always holds the latest state — the cache is derived from it.
    /// What: writes `json` to the SM palace tagged with `tag` (always
    /// [`GOAL_TAG`]); returns `Ok(())` on success or a message on failure.
    /// Test: mock asserts the entry is stored; `SmMemory` impl delegates to
    /// `remember_tagged`.
    async fn remember_goal(&self, json: String, tag: &str) -> Result<(), String>;

    /// Enumerate the raw JSON of every persisted goal entry carrying `tag`.
    ///
    /// Why: startup rebuilds the hot cache from the palace — it must see EVERY
    /// goal, deterministically, not an embedding-ranked subset.
    /// What: returns each matching entry's stored JSON string; `Ok(vec![])` when
    /// none exist.
    /// Test: mock returns the stored set; `SmMemory` impl delegates to
    /// `list_tagged`.
    async fn list_goals(&self, tag: &str) -> Result<Vec<String>, String>;
}

/// A no-op [`GoalMemory`]: persists nothing, enumerates nothing (#1477).
///
/// Why: without the `sm-memory` feature there is no SM palace, but the goal store
/// (SM-6) is feature-INDEPENDENT and works over any [`GoalMemory`]. Backing it with
/// this no-op seam lets `sm.goals.*` and the delegation loop DEGRADE GRACEFULLY to
/// in-memory, non-durable operation on the default build — the headline SM-8
/// capability functions, it simply does not persist across a restart — instead of
/// returning "unavailable". This mirrors the recall no-op pattern in `agent::chat`
/// (memory-dependent work becomes an absent no-op, never a hard failure). Durable
/// goal persistence remains behind `--features sm-memory`.
/// What: `remember_goal` succeeds without storing; `list_goals` always returns an
/// empty set (so a fresh-start store rebuilds to empty). Always compiled.
/// Test: `store_tests.rs` exercises the store over a mock seam; the wiring that
/// selects this seam on the default build is covered by `daemon::sm_stdio::tests`
/// (the no-feature `sm.goals.*` / `sm.delegate` branches).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopGoalMemory;

#[async_trait]
impl GoalMemory for NoopGoalMemory {
    async fn remember_goal(&self, _json: String, _tag: &str) -> Result<(), String> {
        Ok(())
    }

    async fn list_goals(&self, _tag: &str) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
}

/// `SmMemory` is the production [`GoalMemory`]: the dedicated SM palace.
///
/// Why: in the daemon the goal store's source of truth is the real SM palace
/// (§9.4). Implementing the seam directly on `SmMemory` keeps writes scoped to the
/// SM palace by construction (SM-4 guarantees) while the store stays
/// palace-agnostic.
/// What: delegates `remember_goal` → `SmMemory::remember_tagged` and `list_goals`
/// → `SmMemory::list_tagged`, mapping the structured `SmMemoryError` to a message.
/// Gated behind `sm-memory` because `SmMemory` (and its heavy memory-core dep) is.
/// Test: exercised whenever the store is built over a real `SmMemory` (the
/// `sm-memory` feature build); the SM-4 scope tests cover the underlying writes.
#[cfg(feature = "sm-memory")]
#[async_trait]
impl GoalMemory for super::super::memory::SmMemory {
    async fn remember_goal(&self, json: String, tag: &str) -> Result<(), String> {
        self.remember_tagged(json, vec![tag.to_string()])
            .await
            .map(|_id| ())
            .map_err(|e| e.to_string())
    }

    async fn list_goals(&self, tag: &str) -> Result<Vec<String>, String> {
        self.list_tagged(tag).map_err(|e| e.to_string())
    }
}
