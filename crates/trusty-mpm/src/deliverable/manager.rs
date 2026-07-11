//! Shared, async manager over the Deliverable and Milestone stores (§10.7).
//!
//! Why: the CRUD routes need a single, `Arc`-shareable handle that owns both the
//! deliverables and milestones stores behind async locks — the same pattern
//! [`crate::project::ProjectRegistry`] uses over `ProjectStore`. Centralizing
//! load + locking here keeps the daemon's `OnceCell` accessor and the route
//! handlers thin, and gives the future read-only status endpoint (#2382) one
//! place to read both stores for its rollup histograms.
//! What: [`DeliverableManager`] wraps a [`DeliverableStore`] and a
//! [`MilestoneStore`], each in an `Arc<RwLock<…>>`. `upsert_*`/`get_*`/`*_by_project`
//! are cheap, independently-locked primitives for create/read paths (no race
//! hazard — creates use a fresh id, reads are eventually-consistent by design).
//! [`update_deliverable_with`](Self::update_deliverable_with) and
//! [`update_milestone_with`](Self::update_milestone_with) are the atomic
//! read-validate-mutate-persist seam PATCH handlers MUST use (#2395 review HIGH):
//! they hold ONE write-lock guard across the whole sequence so no other task can
//! observe or clobber the intermediate state. The §10.3 transition CHECK itself
//! still lives in the caller's closure (calling
//! [`crate::deliverable::status::validate_transition`]) — the manager does not
//! hard-code deliverable business rules — but the closure now runs against a
//! status that was fetched and will be persisted under the SAME lock, closing
//! the TOCTOU window the two-lock `get_*` + `upsert_*` pattern left open.
//! Test: `manager_create_get_deliverable`, `manager_list_by_project`,
//! `manager_replace_deliverable`, `manager_milestone_round_trip`,
//! `read_then_upsert_pattern_reproduces_lost_update_pre_fix`,
//! `update_deliverable_with_serializes_concurrent_transitions`,
//! `update_deliverable_with_rejects_illegal_transition_without_mutating`,
//! `update_milestone_with_round_trips`.

use std::path::Path;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::RwLock;

use super::milestone::Milestone;
use super::record::Deliverable;
use super::status::TransitionError;
use super::store::{DeliverableStore, MilestoneStore, StoreError};

/// Error from an atomic [`DeliverableManager::update_deliverable_with`] call.
///
/// Why: the atomic update seam must distinguish a store-level failure (I/O,
/// not-found/wrong-project) from a REJECTED §10.3 transition, so the daemon
/// error layer (`daemon::api::deliverable_routes`) can map each to its own HTTP
/// status (404 vs 409) without losing which failure actually occurred.
/// What: wraps either a [`StoreError`] (lookup/persist failure) or a
/// [`TransitionError`] (the caller's mutation closure rejected an illegal
/// transition against the freshly-reloaded status).
/// Test: `update_deliverable_with_rejects_illegal_transition_without_mutating`,
/// and the route-level 409 test in `deliverable_routes::tests`.
#[derive(Debug, Error)]
pub enum UpdateError {
    /// The record could not be located, scoped, or persisted.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The mutation closure rejected an illegal status transition.
    #[error(transparent)]
    Transition(#[from] TransitionError),
}

/// The JSON filename for the central deliverables store (§10.7).
const DELIVERABLES_FILE: &str = "deliverables.json";
/// The JSON filename for the central milestones store (§10.7).
const MILESTONES_FILE: &str = "milestones.json";

/// Async manager over the central Deliverable and Milestone stores.
///
/// Why: one shared handle, cheaply cloneable (`Arc` inside), for injection into
/// every CRUD handler — mirroring `ProjectRegistry` and `SessionManager`.
/// What: holds both stores behind `Arc<RwLock<…>>`; all ops take a write lock
/// because the underlying store reloads-before-read for cross-process freshness.
/// Test: see this module's tests.
#[derive(Debug, Clone)]
pub struct DeliverableManager {
    deliverables: Arc<RwLock<DeliverableStore>>,
    milestones: Arc<RwLock<MilestoneStore>>,
}

impl DeliverableManager {
    /// Load (or create) both central stores under `data_dir` (§10.7).
    ///
    /// Why: at startup the daemon restores both ledgers; absent files start
    /// fresh so the daemon boots cleanly.
    /// What: opens `<data_dir>/deliverables.json` and `<data_dir>/milestones.json`
    /// and wraps each in an `Arc<RwLock>`.
    /// Test: `manager_create_get_deliverable`.
    pub async fn load(data_dir: &Path) -> Result<Self, StoreError> {
        let deliverables = DeliverableStore::load(data_dir.join(DELIVERABLES_FILE)).await?;
        let milestones = MilestoneStore::load(data_dir.join(MILESTONES_FILE)).await?;
        Ok(Self {
            deliverables: Arc::new(RwLock::new(deliverables)),
            milestones: Arc::new(RwLock::new(milestones)),
        })
    }

    // ----- Deliverables -----

    /// Persist a new (or replacement) Deliverable, keyed by its id.
    ///
    /// Why: the create and non-status update paths both upsert a fully-formed
    /// record; keeping one method avoids a spurious create/update split.
    /// What: acquires a write lock and upserts.
    /// Test: `manager_create_get_deliverable`, `manager_replace_deliverable`.
    pub async fn upsert_deliverable(&self, d: Deliverable) -> Result<(), StoreError> {
        self.deliverables.write().await.upsert(d).await
    }

    /// Fetch a Deliverable by its stringified id.
    ///
    /// Why: the GET and PATCH handlers both need a point lookup.
    /// What: reloads-then-reads via the store; `NotFound` when absent.
    /// Test: `manager_create_get_deliverable`.
    pub async fn get_deliverable(&self, id: &str) -> Result<Deliverable, StoreError> {
        self.deliverables.write().await.get(id).await
    }

    /// List a project's Deliverables.
    ///
    /// Why: the list endpoint is project-scoped; filtering lives in the store.
    /// What: returns every Deliverable whose `project_name` matches.
    /// Test: `manager_list_by_project`.
    pub async fn deliverables_by_project(
        &self,
        project_name: &str,
    ) -> Result<Vec<Deliverable>, StoreError> {
        self.deliverables
            .write()
            .await
            .by_project(project_name)
            .await
    }

    /// Return every Deliverable across all projects (for the #2382 rollup).
    ///
    /// Why: the read-only status endpoint aggregates status histograms; exposing
    /// `all` keeps that consumer from reaching into the private store.
    /// What: reloads-then-clones all values.
    /// Test: `manager_list_by_project` (asserts total count).
    pub async fn all_deliverables(&self) -> Result<Vec<Deliverable>, StoreError> {
        self.deliverables.write().await.all().await
    }

    /// Atomically read-validate-mutate-persist a Deliverable under ONE lock.
    ///
    /// Why (fixes the #2395 review HIGH): the original PATCH path called
    /// [`get_deliverable`](Self::get_deliverable) (one write-lock acquisition),
    /// released the lock, THEN separately validated the requested transition in
    /// the route handler and called
    /// [`upsert_deliverable`](Self::upsert_deliverable) (a SECOND, later
    /// acquisition). Two concurrent PATCHes could interleave in the gap between
    /// those two critical sections: both would validate against the SAME stale
    /// status (both legal from it), then whichever `upsert` ran last would win —
    /// silently discarding the other's ALREADY-validated, ALREADY-"successful"
    /// transition with no error ever surfacing to that caller. Worse, the
    /// persisted end state can be one neither validated call ever actually
    /// checked (e.g. A validates in-progress→complete, B validates the same
    /// stale in-progress→blocked; if B's write lands first and A's lands last,
    /// the final `complete` was validated, but only against a status that, by
    /// the time it landed, was no longer live — B's committed `blocked` state was
    /// never itself a validated predecessor of `complete`).
    /// What: acquires `self.deliverables.write().await` ONCE and holds the guard
    /// for the ENTIRE read→validate→mutate→persist sequence. Fetches the CURRENT
    /// on-disk record via the store's own reload-on-read `get` (so the fetch
    /// reflects any external write), checks project scope, then calls `f` with
    /// that freshly-reloaded record — so any transition validation the caller's
    /// closure performs runs against a status that cannot go stale before the
    /// resulting `upsert` lands, because no other task can acquire the same lock
    /// in between. Returns [`UpdateError::Transition`] without mutating anything
    /// if `f` rejects the transition; otherwise persists `f`'s result before
    /// releasing the lock.
    /// Test: `update_deliverable_with_serializes_concurrent_transitions` (the
    /// concurrency regression test — proves exactly one of two racing
    /// transitions from the same starting status can ever succeed, and the
    /// final persisted state always matches the winner, never a clobbered or
    /// unreachable value), `update_deliverable_with_rejects_illegal_transition_without_mutating`.
    pub async fn update_deliverable_with<F>(
        &self,
        id: &str,
        project: &str,
        f: F,
    ) -> Result<Deliverable, UpdateError>
    where
        F: FnOnce(Deliverable) -> Result<Deliverable, TransitionError>,
    {
        let mut guard = self.deliverables.write().await;
        let current = guard.get(id).await?;
        if current.project_name != project {
            return Err(StoreError::NotFound(id.to_string()).into());
        }
        let updated = f(current)?;
        guard.upsert(updated.clone()).await?;
        Ok(updated)
    }

    // ----- Milestones -----

    /// Persist a new (or replacement) Milestone, keyed by its id.
    pub async fn upsert_milestone(&self, m: Milestone) -> Result<(), StoreError> {
        self.milestones.write().await.upsert(m).await
    }

    /// Fetch a Milestone by its stringified id.
    pub async fn get_milestone(&self, id: &str) -> Result<Milestone, StoreError> {
        self.milestones.write().await.get(id).await
    }

    /// List a project's Milestones.
    pub async fn milestones_by_project(
        &self,
        project_name: &str,
    ) -> Result<Vec<Milestone>, StoreError> {
        self.milestones.write().await.by_project(project_name).await
    }

    /// Return every Milestone across all projects (for the #2382 rollup).
    pub async fn all_milestones(&self) -> Result<Vec<Milestone>, StoreError> {
        self.milestones.write().await.all().await
    }

    /// Atomically read-mutate-persist a Milestone under ONE lock.
    ///
    /// Why: the same lost-update hazard `update_deliverable_with` closes applies
    /// to Milestone PATCH too — a read-then-upsert pattern can silently drop a
    /// concurrent field edit (e.g. two PATCHes racing on `deliverables` list
    /// membership vs `name`) even though Milestone has no status state machine
    /// to invalidate. Holding one lock across the whole sequence closes that
    /// window here as well, per the same precedent.
    /// What: acquires `self.milestones.write().await` ONCE, fetches the current
    /// record (reload-on-read), checks project scope, applies the mutation
    /// (infallible — Milestone status is a plain rollup field with no rejectable
    /// transition, §10.3/§10.5), and persists before releasing the lock.
    /// Test: `update_milestone_with_round_trips`.
    pub async fn update_milestone_with<F>(
        &self,
        id: &str,
        project: &str,
        f: F,
    ) -> Result<Milestone, StoreError>
    where
        F: FnOnce(Milestone) -> Milestone,
    {
        let mut guard = self.milestones.write().await;
        let current = guard.get(id).await?;
        if current.project_name != project {
            return Err(StoreError::NotFound(id.to_string()));
        }
        let updated = f(current);
        guard.upsert(updated.clone()).await?;
        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deliverable::milestone::{Milestone, MilestoneId, MilestoneStatus};
    use crate::deliverable::record::{Deliverable, DeliverableId, DeliverableKind, EstimationTier};
    use crate::deliverable::status::DeliverableStatus;
    use chrono::Utc;
    use tempfile::TempDir;

    fn deliverable(project: &str, name: &str) -> Deliverable {
        Deliverable {
            id: DeliverableId::new(),
            project_name: project.into(),
            name: name.into(),
            description: String::new(),
            kind: DeliverableKind::Feature,
            ticket_ref: None,
            spec_ref: None,
            status: DeliverableStatus::Proposed,
            estimated_effort: EstimationTier::M,
            created_at: Utc::now(),
            target_date: None,
        }
    }

    #[tokio::test]
    async fn manager_create_get_deliverable() {
        let dir = TempDir::new().unwrap();
        let mgr = DeliverableManager::load(dir.path()).await.unwrap();
        let d = deliverable("p", "one");
        let key = d.id.to_string();
        mgr.upsert_deliverable(d).await.unwrap();
        assert_eq!(mgr.get_deliverable(&key).await.unwrap().name, "one");
        // Re-loading the manager sees the persisted record (central store).
        let mgr2 = DeliverableManager::load(dir.path()).await.unwrap();
        assert_eq!(mgr2.get_deliverable(&key).await.unwrap().name, "one");
    }

    #[tokio::test]
    async fn manager_list_by_project() {
        let dir = TempDir::new().unwrap();
        let mgr = DeliverableManager::load(dir.path()).await.unwrap();
        mgr.upsert_deliverable(deliverable("p1", "a"))
            .await
            .unwrap();
        mgr.upsert_deliverable(deliverable("p1", "b"))
            .await
            .unwrap();
        mgr.upsert_deliverable(deliverable("p2", "c"))
            .await
            .unwrap();
        assert_eq!(mgr.deliverables_by_project("p1").await.unwrap().len(), 2);
        assert_eq!(mgr.deliverables_by_project("p2").await.unwrap().len(), 1);
        assert_eq!(mgr.all_deliverables().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn manager_replace_deliverable() {
        let dir = TempDir::new().unwrap();
        let mgr = DeliverableManager::load(dir.path()).await.unwrap();
        let mut d = deliverable("p", "orig");
        let key = d.id.to_string();
        mgr.upsert_deliverable(d.clone()).await.unwrap();
        d.status = DeliverableStatus::InProgress;
        d.name = "renamed".into();
        mgr.upsert_deliverable(d).await.unwrap();
        let got = mgr.get_deliverable(&key).await.unwrap();
        assert_eq!(got.name, "renamed");
        assert_eq!(got.status, DeliverableStatus::InProgress);
        assert_eq!(mgr.deliverables_by_project("p").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn manager_milestone_round_trip() {
        let dir = TempDir::new().unwrap();
        let mgr = DeliverableManager::load(dir.path()).await.unwrap();
        let m = Milestone {
            id: MilestoneId::new(),
            project_name: "p".into(),
            name: "v1".into(),
            description: String::new(),
            target_date: Utc::now(),
            status: MilestoneStatus::Proposed,
            deliverables: vec![],
            created_at: Utc::now(),
        };
        let key = m.id.to_string();
        mgr.upsert_milestone(m).await.unwrap();
        assert_eq!(mgr.get_milestone(&key).await.unwrap().name, "v1");
        assert_eq!(mgr.milestones_by_project("p").await.unwrap().len(), 1);
        assert_eq!(mgr.all_milestones().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn update_deliverable_with_rejects_illegal_transition_without_mutating() {
        let dir = TempDir::new().unwrap();
        let mgr = DeliverableManager::load(dir.path()).await.unwrap();
        let d = deliverable("p", "x");
        let id = d.id.to_string();
        mgr.upsert_deliverable(d).await.unwrap();

        let err = mgr
            .update_deliverable_with(&id, "p", |mut current| {
                crate::deliverable::status::validate_transition(
                    current.status,
                    DeliverableStatus::Complete,
                )?;
                current.status = DeliverableStatus::Complete;
                Ok(current)
            })
            .await
            .expect_err("proposed -> complete must be rejected");
        assert!(matches!(err, UpdateError::Transition(_)));

        // The rejected closure must not have mutated the persisted record.
        let unchanged = mgr.get_deliverable(&id).await.unwrap();
        assert_eq!(unchanged.status, DeliverableStatus::Proposed);
    }

    /// Reproduces, using the still-available low-level primitives
    /// (`get_deliverable` + `upsert_deliverable`), the EXACT pre-fix PATCH hazard
    /// the #2395 review flagged as a HIGH: a `get` under one lock followed LATER
    /// by a separate `upsert` under a second lock leaves a window in which
    /// another task's own fetch-validate-upsert sequence can run to completion
    /// entirely — both tasks validate against the SAME stale status (both are
    /// independently legal transitions FROM that shared stale read), and
    /// whichever `upsert` lands last silently discards the other's committed,
    /// already-"validated-successful" transition. This is the test that "reliably
    /// fails pre-fix": it exercises exactly the two-call shape the OLD
    /// `patch_deliverable` handler used (fetch under one lock, then mutate+upsert
    /// under a later, separate lock) — the same shape this PR's
    /// `update_deliverable_with` replaces. A `tokio::sync::Notify` gate forces the
    /// deterministic interleave (B's whole fetch-validate-upsert completes while
    /// A is parked between its OWN fetch and its OWN upsert), so the outcome never
    /// depends on scheduler luck.
    /// Test: this IS the test (the #2395 HIGH regression guard, negative case).
    #[tokio::test]
    async fn read_then_upsert_pattern_reproduces_lost_update_pre_fix() {
        let dir = TempDir::new().unwrap();
        let mgr = Arc::new(DeliverableManager::load(dir.path()).await.unwrap());
        let mut d = deliverable("p", "race");
        d.status = DeliverableStatus::InProgress;
        let id = d.id.to_string();
        mgr.upsert_deliverable(d).await.unwrap();

        // Gate: task A fetches, then PARKS here — simulating the exact window the
        // two-lock pattern leaves open between its `get` and its `upsert` — until
        // B has fetched, validated, AND upserted its own (different) transition.
        let gate = Arc::new(tokio::sync::Notify::new());

        let mgr_a = Arc::clone(&mgr);
        let gate_a = Arc::clone(&gate);
        let id_a = id.clone();
        let a = tokio::spawn(async move {
            // Lock #1 (fetch), released as soon as `get_deliverable` returns —
            // exactly the OLD `fetch_scoped` call.
            let current = mgr_a.get_deliverable(&id_a).await.unwrap();
            // Park until B's own fetch-validate-upsert has fully landed.
            gate_a.notified().await;
            let target = DeliverableStatus::Complete;
            crate::deliverable::status::validate_transition(current.status, target)
                .expect("A's own validation, against the stale read, succeeds");
            let mut updated = current;
            updated.status = target;
            // Lock #2 (upsert) — a SEPARATE, later acquisition than lock #1.
            mgr_a.upsert_deliverable(updated).await.unwrap();
        });

        let mgr_b = Arc::clone(&mgr);
        let gate_b = Arc::clone(&gate);
        let id_b = id.clone();
        let b = tokio::spawn(async move {
            let current = mgr_b.get_deliverable(&id_b).await.unwrap();
            let target = DeliverableStatus::Blocked;
            crate::deliverable::status::validate_transition(current.status, target)
                .expect("B's own validation, against the SAME stale read, ALSO succeeds");
            let mut updated = current;
            updated.status = target;
            mgr_b.upsert_deliverable(updated).await.unwrap();
            // B's committed write has landed — release A to clobber it.
            gate_b.notify_one();
        });

        b.await.unwrap();
        a.await.unwrap();

        // The bug: BOTH validations independently succeeded (both are legal FROM
        // the shared stale in-progress read), yet only A's write survives — B's
        // committed, validated transition to `blocked` is silently discarded with
        // NO error ever surfaced to B's caller (B's own call returned Ok).
        let final_state = mgr.get_deliverable(&id).await.unwrap();
        assert_eq!(
            final_state.status,
            DeliverableStatus::Complete,
            "demonstrates the pre-fix hazard: B's validated, committed transition \
             to `blocked` is lost — silently clobbered by A's later upsert against \
             the same stale read, even though B's own call reported success"
        );
    }

    /// Proves the fix: racing two `update_deliverable_with` calls attempting
    /// DIFFERENT transitions from the SAME starting status can never both
    /// succeed, and the final persisted status always matches exactly the
    /// winner's target — no lost update, no state unreachable by a validated
    /// chain. This assertion is deliberately ORDER-INDEPENDENT (it does not care
    /// which task's write lands first): from `in-progress`, the legal successors
    /// are exactly `{blocked, complete}` (§10.3); whichever of the two tasks
    /// acquires the manager's single write-lock second observes the FIRST task's
    /// already-persisted target as the current status, and `blocked -> complete`
    /// / `complete -> blocked` are both illegal (`blocked`'s only successor is
    /// `in-progress`; `complete`'s successors are `delivered`/`shipped`) — so the
    /// second task's closure is structurally guaranteed to be rejected, REGARDLESS
    /// of scheduling order. Pre-fix (the two-lock pattern exercised by
    /// `read_then_upsert_pattern_reproduces_lost_update_pre_fix` above), this same
    /// race would have both closures return Ok.
    /// Test: this IS the test (the #2395 HIGH regression guard, positive case).
    #[tokio::test]
    async fn update_deliverable_with_serializes_concurrent_transitions() {
        let dir = TempDir::new().unwrap();
        let mgr = Arc::new(DeliverableManager::load(dir.path()).await.unwrap());
        let mut d = deliverable("p", "race");
        d.status = DeliverableStatus::InProgress;
        let id = d.id.to_string();
        mgr.upsert_deliverable(d).await.unwrap();

        fn transition_to(
            target: DeliverableStatus,
        ) -> impl FnOnce(Deliverable) -> Result<Deliverable, crate::deliverable::status::TransitionError>
        {
            move |mut current| {
                crate::deliverable::status::validate_transition(current.status, target)?;
                current.status = target;
                Ok(current)
            }
        }

        let mgr_a = Arc::clone(&mgr);
        let id_a = id.clone();
        let a = tokio::spawn(async move {
            mgr_a
                .update_deliverable_with(&id_a, "p", transition_to(DeliverableStatus::Complete))
                .await
        });

        let mgr_b = Arc::clone(&mgr);
        let id_b = id.clone();
        let b = tokio::spawn(async move {
            mgr_b
                .update_deliverable_with(&id_b, "p", transition_to(DeliverableStatus::Blocked))
                .await
        });

        let ra = a.await.unwrap();
        let rb = b.await.unwrap();

        // Capture the outcomes as plain bools BEFORE moving either `Result` —
        // re-testing `.is_ok()` on a value already moved out of one `if` arm is
        // a static (not path-sensitive) borrow-check error, so decide once here.
        let ra_ok = ra.is_ok();
        let rb_ok = rb.is_ok();

        // Exactly one of the two racing transitions may succeed — never both,
        // never neither (no lost update either direction).
        assert_eq!(
            [ra_ok, rb_ok].iter().filter(|ok| **ok).count(),
            1,
            "exactly one racing transition must win: a_ok={ra_ok} b_ok={rb_ok}"
        );

        // The loser must be rejected via the SAME structured transition error the
        // route layer surfaces as a 409 — not a store error, not a silent no-op.
        // The final persisted status is reachable via exactly the winner's
        // validated transition — never a third, unreachable value.
        let (loser_err, winner_target) = if ra_ok {
            (rb.unwrap_err(), DeliverableStatus::Complete)
        } else {
            (ra.unwrap_err(), DeliverableStatus::Blocked)
        };
        assert!(
            matches!(loser_err, UpdateError::Transition(_)),
            "the losing transition must be rejected, not silently dropped: {loser_err:?}"
        );

        let final_state = mgr.get_deliverable(&id).await.unwrap();
        assert_eq!(final_state.status, winner_target);
    }

    #[tokio::test]
    async fn update_milestone_with_round_trips() {
        let dir = TempDir::new().unwrap();
        let mgr = DeliverableManager::load(dir.path()).await.unwrap();
        let m = Milestone {
            id: MilestoneId::new(),
            project_name: "p".into(),
            name: "v1".into(),
            description: String::new(),
            target_date: Utc::now(),
            status: MilestoneStatus::Proposed,
            deliverables: vec![],
            created_at: Utc::now(),
        };
        let id = m.id.to_string();
        mgr.upsert_milestone(m).await.unwrap();

        let updated = mgr
            .update_milestone_with(&id, "p", |mut current| {
                current.name = "v1.0".into();
                current.status = MilestoneStatus::Shipped;
                current
            })
            .await
            .unwrap();
        assert_eq!(updated.name, "v1.0");
        assert_eq!(updated.status, MilestoneStatus::Shipped);

        // Wrong project must be rejected without mutating the record.
        let err = mgr
            .update_milestone_with(&id, "wrong-project", |m| m)
            .await
            .expect_err("wrong project scope must be rejected");
        assert!(matches!(err, StoreError::NotFound(_)));
        assert_eq!(mgr.get_milestone(&id).await.unwrap().name, "v1.0");
    }
}
