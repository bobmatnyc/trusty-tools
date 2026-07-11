//! Shared, async manager over the Deliverable and Milestone stores (§10.7).
//!
//! Why: the CRUD routes need a single, `Arc`-shareable handle that owns both the
//! deliverables and milestones stores behind async locks — the same pattern
//! [`crate::project::ProjectRegistry`] uses over `ProjectStore`. Centralizing
//! load + locking here keeps the daemon's `OnceCell` accessor and the route
//! handlers thin, and gives the future read-only status endpoint (#2382) one
//! place to read both stores for its rollup histograms.
//! What: [`DeliverableManager`] wraps a [`DeliverableStore`] and a
//! [`MilestoneStore`], each in an `Arc<RwLock<…>>`, and exposes create / get /
//! project-scoped list / replace operations for both. It deliberately does NOT
//! enforce the status state machine — that gate (#2380) lives in the API layer
//! (see `daemon::api::deliverable_routes`), calling
//! [`crate::deliverable::status::validate_transition`] — so the manager stays a
//! pure persistence facade.
//! Test: `manager_create_get_deliverable`, `manager_list_by_project`,
//! `manager_replace_deliverable`, `manager_milestone_round_trip`.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::RwLock;

use super::milestone::Milestone;
use super::record::Deliverable;
use super::store::{DeliverableStore, MilestoneStore, StoreError};

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
}
