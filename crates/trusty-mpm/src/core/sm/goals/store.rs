//! The SM goal store: lifecycle + dual (palace + cache) persistence (§9).
//!
//! Why: SM-6 owns the goal lifecycle (§9.2) — create, link a session, update a
//! session's verification state/evidence, append notes, and close through the
//! BLOCKING verification gate (§3.5) — over a DUAL persistence design (§9.4): the
//! SM palace is the source of truth (every mutation writes the goal's JSON there
//! through the [`GoalMemory`] seam), and `goals.json` is a hot cache mirrored on
//! every mutation and REBUILT from the palace on startup. This file is the store;
//! it deliberately does NOT wire into any endpoint or the agent loop (SM-7/SM-8).
//! Both the memory seam and the data root are injectable, so the whole store is
//! unit-testable with a mock palace + a tempdir (no ONNX).
//! What: [`SmGoalStore`] holds an in-memory goal map, a [`GoalMemory`] (palace),
//! and a [`GoalCache`] (file). [`SmGoalStore::load`] rebuilds the map from the
//! palace (falling back to the cache when the palace is unavailable). `create` /
//! `link` / `update` / `close` mutate, recompute derived progress, then dual-write
//! (palace first as truth, then cache). A monotonic clock is injectable for
//! deterministic timestamps in tests.
//! Test: `goals/store_tests.rs` — id stability across reload, progress on link,
//! the verification gate, palace→cache rebuild equality, and palace-unavailable
//! fallback.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::cache::GoalCache;
use super::error::{SmGoalError, SmGoalResult};
use super::memory::{GOAL_TAG, GoalMemory};
use super::model::{Goal, GoalStatus, SessionLink, SessionTaskState};

/// A mutation applied to a linked session via [`SmGoalStore::update`] (§9.2).
///
/// Why: as sessions are observed/verified (§3.4 phases 4-5) the SM updates the
/// matching [`SessionLink`]'s state and captures evidence, and may append a
/// decision/blocker note. Bundling those into one optional-field struct keeps the
/// update API a single call (and a single dual-write) rather than several.
/// What: a `session_id` selecting the link, plus optional `state` / `evidence` /
/// `note`. `None` fields are left unchanged; a `note` is appended to the goal.
/// Test: `update_sets_state_and_evidence`, `update_appends_note`.
#[derive(Debug, Clone, Default)]
pub struct SessionUpdate {
    /// The linked session to mutate (must already be linked to the goal).
    pub session_id: String,
    /// New verification state, if changing.
    pub state: Option<SessionTaskState>,
    /// Evidence to record (PR URL, captured test output, …), if any.
    pub evidence: Option<String>,
    /// A decision/blocker note to append to the goal, if any.
    pub note: Option<String>,
}

/// In-memory goal map backed by the SM palace (truth) + `goals.json` (cache).
///
/// Why: the single owner of the goal lifecycle and its dual persistence. Keeping
/// the goals in a `BTreeMap` keyed by id gives stable iteration order (so the
/// cache file is deterministic) and O(log n) lookup for link/update/close.
/// What: holds the goal map, the [`GoalMemory`] palace seam (`Arc<dyn …>` for
/// injection), the [`GoalCache`], and an injectable clock. Built via
/// [`SmGoalStore::load`] (rebuild-from-palace) for production or
/// [`SmGoalStore::with_clock`] for deterministic tests.
/// Test: `goals/store_tests.rs`.
pub struct SmGoalStore {
    /// Live goals keyed by stable id (sorted iteration → deterministic cache).
    goals: BTreeMap<String, Goal>,
    /// Source-of-truth palace seam (mocked in tests, `SmMemory` in production).
    memory: Arc<dyn GoalMemory>,
    /// Hot-cache file store (`goals.json`).
    cache: GoalCache,
    /// Injectable clock for `created`/`updated`; defaults to `Utc::now`.
    clock: fn() -> DateTime<Utc>,
}

impl SmGoalStore {
    /// Build an empty store over the given palace seam and data root.
    ///
    /// Why: the lowest-level constructor — no rebuild, real clock. Used internally
    /// by [`SmGoalStore::load`] and directly when an empty start is wanted.
    /// What: constructs the store with an empty goal map, a [`GoalCache`] rooted at
    /// `data_root`, and `Utc::now` as the clock.
    /// Test: indirectly via every other constructor.
    pub fn new(memory: Arc<dyn GoalMemory>, data_root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            goals: BTreeMap::new(),
            memory,
            cache: GoalCache::new(data_root),
            clock: Utc::now,
        }
    }

    /// Build an empty store with an injected clock (deterministic tests).
    ///
    /// Why: tests need reproducible `created`/`updated` timestamps; production uses
    /// the wall clock. Injecting the clock keeps the model pure.
    /// What: like [`SmGoalStore::new`] but stores the supplied `clock` fn.
    /// Test: used throughout `goals/store_tests.rs`.
    pub fn with_clock(
        memory: Arc<dyn GoalMemory>,
        data_root: impl Into<std::path::PathBuf>,
        clock: fn() -> DateTime<Utc>,
    ) -> Self {
        Self {
            goals: BTreeMap::new(),
            memory,
            cache: GoalCache::new(data_root),
            clock,
        }
    }

    /// Load the store, rebuilding the goal map from the palace (§9.4).
    ///
    /// Why: the palace is the source of truth, so on startup the hot map (and the
    /// `goals.json` cache) is DERIVED by enumerating every persisted goal entry. If
    /// the palace is unavailable (degraded backend), the store falls back to the
    /// last-written cache so the SM still surfaces the goals it knew about — never
    /// panicking. After a successful palace rebuild the cache is re-written so it
    /// matches the palace-derived state.
    /// What: enumerates `GOAL_TAG` entries via [`GoalMemory::list_goals`],
    /// deserialises each into a [`Goal`], and indexes them by id. On enumeration
    /// success the cache is overwritten to match; on enumeration failure the store
    /// loads from the cache instead. Per-entry JSON that fails to parse is skipped
    /// (one corrupt drawer must not sink the whole rebuild).
    /// Test: `rebuild_from_palace_matches_cache`,
    /// `palace_unavailable_falls_back_to_cache`.
    pub async fn load(
        memory: Arc<dyn GoalMemory>,
        data_root: impl Into<std::path::PathBuf>,
    ) -> SmGoalResult<Self> {
        let mut store = Self::new(memory, data_root);
        match store.memory.list_goals(GOAL_TAG).await {
            Ok(entries) => {
                for json in entries {
                    // Skip an individual unparseable entry rather than failing the
                    // whole rebuild — one corrupt drawer must not hide every goal.
                    if let Ok(goal) = serde_json::from_str::<Goal>(&json) {
                        store.goals.insert(goal.id.clone(), goal);
                    }
                }
                // Re-derive the cache so it matches the palace-derived truth.
                store.persist_cache()?;
            }
            Err(_palace_err) => {
                // Palace unavailable → fall back to the last-written cache. This is
                // graceful degradation: the SM keeps the goals it last cached.
                for goal in store.cache.load()? {
                    store.goals.insert(goal.id.clone(), goal);
                }
            }
        }
        Ok(store)
    }

    /// Create a goal from operator intent and dual-persist it (§9.2 create).
    ///
    /// Why: intake (§3.4 phase 1) turns operator intent into a tracked goal with a
    /// STABLE id that survives restarts. The id is generated once here and is the
    /// join key the palace and cache key on.
    /// What: mints `g-<short-uuid>`, builds a `Pending` [`Goal`] with the injected
    /// clock, inserts it, then dual-writes (palace truth first, then cache).
    /// Returns the created goal.
    /// Test: `create_assigns_stable_id`, `create_dual_persists`.
    pub async fn create(
        &mut self,
        description: impl Into<String>,
        acceptance: Vec<String>,
    ) -> SmGoalResult<Goal> {
        let id = new_goal_id();
        let goal = Goal::new(id.clone(), description, acceptance, (self.clock)());
        self.goals.insert(id.clone(), goal);
        self.persist(&id).await?;
        Ok(self.goals.get(&id).cloned().expect("just inserted"))
    }

    /// Link a launched session to a goal and recompute progress (§9.2 decompose).
    ///
    /// Why: each session-sized task launched for a goal is recorded so progress and
    /// the close gate can derive from the per-session verification state. Linking
    /// must recompute the derived `progress` and dual-persist.
    /// What: appends `link` to the goal's `sessions`, moves a `Pending` goal to
    /// `InProgress`, recomputes `progress`, bumps `updated`, then dual-writes.
    /// Returns the updated goal. Unknown id → [`SmGoalError::NotFound`].
    /// Test: `link_updates_progress`, `link_unknown_goal_is_not_found`.
    pub async fn link(&mut self, goal_id: &str, link: SessionLink) -> SmGoalResult<Goal> {
        {
            let goal = self
                .goals
                .get_mut(goal_id)
                .ok_or_else(|| SmGoalError::NotFound(goal_id.to_string()))?;
            goal.sessions.push(link);
            if goal.status == GoalStatus::Pending {
                goal.status = GoalStatus::InProgress;
            }
            goal.recompute_progress();
            goal.updated = (self.clock)();
        }
        self.persist(goal_id).await?;
        Ok(self.goals.get(goal_id).cloned().expect("present"))
    }

    /// Apply a session update (state / evidence / note) and recompute (§9.2 update).
    ///
    /// Why: as the SM observes and verifies sessions (§3.4 phases 4-5) it records
    /// the new verification state and the captured evidence, and may append a
    /// decision/blocker note; progress is re-derived from the updated states.
    /// What: finds the [`SessionLink`] by `session_id`, applies the supplied
    /// non-`None` fields, appends any `note` to the goal, recomputes `progress`,
    /// bumps `updated`, then dual-writes. Returns the updated goal. Unknown goal or
    /// session → [`SmGoalError::NotFound`].
    /// Test: `update_sets_state_and_evidence`, `update_appends_note`,
    /// `update_unknown_session_is_not_found`.
    pub async fn update(&mut self, goal_id: &str, upd: SessionUpdate) -> SmGoalResult<Goal> {
        {
            let goal = self
                .goals
                .get_mut(goal_id)
                .ok_or_else(|| SmGoalError::NotFound(goal_id.to_string()))?;
            let link = goal
                .sessions
                .iter_mut()
                .find(|s| s.session_id == upd.session_id)
                .ok_or_else(|| SmGoalError::NotFound(upd.session_id.clone()))?;
            if let Some(state) = upd.state {
                link.state = state;
            }
            if let Some(evidence) = upd.evidence {
                link.evidence = Some(evidence);
            }
            if let Some(note) = upd.note {
                goal.notes.push(note);
            }
            goal.recompute_progress();
            goal.updated = (self.clock)();
        }
        self.persist(goal_id).await?;
        Ok(self.goals.get(goal_id).cloned().expect("present"))
    }

    /// Append a free-form note to a goal and dual-persist (§9.2 decisions/blockers).
    ///
    /// Why: the SM records decisions and blockers against a goal independently of
    /// any session update; a dedicated path keeps that intent explicit.
    /// What: pushes `note`, bumps `updated`, dual-writes. Unknown id →
    /// [`SmGoalError::NotFound`].
    /// Test: `note_appends_and_persists`.
    pub async fn note(&mut self, goal_id: &str, note: impl Into<String>) -> SmGoalResult<Goal> {
        {
            let goal = self
                .goals
                .get_mut(goal_id)
                .ok_or_else(|| SmGoalError::NotFound(goal_id.to_string()))?;
            goal.notes.push(note.into());
            goal.updated = (self.clock)();
        }
        self.persist(goal_id).await?;
        Ok(self.goals.get(goal_id).cloned().expect("present"))
    }

    /// Set a goal's status, enforcing the verification gate on `Done` (§3.5).
    ///
    /// Why: §3.5 forbids claiming a goal `Done` without observed evidence — every
    /// linked task must be `Verified`. The gate lives HERE so no caller can bypass
    /// it; a `Done` request that fails the gate is REJECTED with a structured
    /// error (not a panic), and the goal is left unchanged. Other statuses
    /// (`Blocked`, `Abandoned`, …) carry no gate.
    /// What: if `status == Done` and not [`Goal::all_verified`], returns
    /// [`SmGoalError::VerificationGate`] with the verified/total counts and does
    /// NOT mutate or persist. Otherwise sets the status, bumps `updated`, and
    /// dual-writes. Returns the updated goal. Unknown id →
    /// [`SmGoalError::NotFound`].
    /// Test: `close_without_all_verified_is_rejected`,
    /// `close_with_all_verified_succeeds`, `set_blocked_has_no_gate`.
    pub async fn set_status(&mut self, goal_id: &str, status: GoalStatus) -> SmGoalResult<Goal> {
        {
            let goal = self
                .goals
                .get_mut(goal_id)
                .ok_or_else(|| SmGoalError::NotFound(goal_id.to_string()))?;
            if status == GoalStatus::Done && !goal.all_verified() {
                let total = goal.sessions.len();
                let verified = goal
                    .sessions
                    .iter()
                    .filter(|s| s.state.is_verified())
                    .count();
                return Err(SmGoalError::VerificationGate {
                    goal_id: goal_id.to_string(),
                    verified,
                    total,
                });
            }
            goal.status = status;
            goal.updated = (self.clock)();
        }
        self.persist(goal_id).await?;
        Ok(self.goals.get(goal_id).cloned().expect("present"))
    }

    /// Close a goal as `Done`, applying the verification gate (§9.2 close).
    ///
    /// Why: the canonical close path — a thin, intent-revealing alias over
    /// [`SmGoalStore::set_status`]`(Done)` so call sites read as "close this goal"
    /// while still going through the single gated transition.
    /// What: delegates to `set_status(goal_id, GoalStatus::Done)`.
    /// Test: `close_without_all_verified_is_rejected`,
    /// `close_with_all_verified_succeeds`.
    pub async fn close(&mut self, goal_id: &str) -> SmGoalResult<Goal> {
        self.set_status(goal_id, GoalStatus::Done).await
    }

    /// Fetch a goal by id (read-only).
    ///
    /// Why: callers (and the future TUI/endpoint) read individual goals; tests
    /// assert state without reaching into the private map.
    /// What: returns a reference to the goal, or `None` if absent.
    /// Test: used throughout `goals/store_tests.rs`.
    pub fn get(&self, goal_id: &str) -> Option<&Goal> {
        self.goals.get(goal_id)
    }

    /// All goals in stable (id-sorted) order (read-only).
    ///
    /// Why: the TUI lists goals; the cache writes them in this order. Sorted
    /// iteration makes both deterministic.
    /// What: returns the goals as a `Vec` in `BTreeMap` key order.
    /// Test: `rebuild_from_palace_matches_cache`.
    pub fn all(&self) -> Vec<Goal> {
        self.goals.values().cloned().collect()
    }

    /// Dual-write one goal: palace (truth) first, then the cache (§9.4).
    ///
    /// Why: the palace is the source of truth, so it is written FIRST — if the
    /// palace write fails the operation fails before the cache diverges from truth.
    /// The cache is then re-derived from the full in-memory map (which already
    /// reflects this goal's mutation).
    /// What: serialises the goal to JSON, writes it tagged to the palace via
    /// [`GoalMemory::remember_goal`] (mapping the `String` error to
    /// [`SmGoalError::Palace`]), then calls [`SmGoalStore::persist_cache`].
    /// Test: `create_dual_persists`, `palace_write_failure_propagates`.
    async fn persist(&self, goal_id: &str) -> SmGoalResult<()> {
        let goal = self
            .goals
            .get(goal_id)
            .ok_or_else(|| SmGoalError::NotFound(goal_id.to_string()))?;
        let json = serde_json::to_string(goal).map_err(|source| SmGoalError::Serde { source })?;
        self.memory
            .remember_goal(json, GOAL_TAG)
            .await
            .map_err(|message| SmGoalError::Palace { message })?;
        self.persist_cache()
    }

    /// Re-write the `goals.json` cache from the full in-memory map (§9.4).
    ///
    /// Why: the cache is a faithful mirror of the live goal set; re-writing the
    /// whole list (rather than patching) keeps it trivially consistent and
    /// deterministic.
    /// What: snapshots all goals (sorted) and atomically saves them via
    /// [`GoalCache::save`].
    /// Test: `create_dual_persists`, `rebuild_from_palace_matches_cache`.
    fn persist_cache(&self) -> SmGoalResult<()> {
        let goals = self.all();
        self.cache.save(&goals)
    }
}

/// Mint a fresh stable goal id of the form `g-<short-uuid>` (§9.1).
///
/// Why: each goal needs an id that is unique and STABLE for its lifetime (the
/// palace/cache join key). A v4 UUID gives collision-resistance; the `g-` prefix
/// and the 8-char short form match the spec's `"g-<short-uuid>"` example and keep
/// the id readable in logs/TUI.
/// What: returns `"g-"` followed by the first 8 hex chars of a fresh v4 UUID's
/// simple (dash-free) form.
/// Test: `create_assigns_stable_id` (stability across reload), `goal_id_is_prefixed`.
fn new_goal_id() -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    format!("g-{}", &uuid[..8])
}
