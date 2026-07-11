//! The [`Milestone`] record and its value types (DOC-35 §10.5).
//!
//! Why: a Milestone groups Deliverables toward a dated target (e.g. `"v1.0
//! Alpha"`). Like the Deliverable, it is L3-substrate bookkeeping and must
//! persist across daemon restarts and be exchanged over HTTP.
//! What: defines [`MilestoneId`] (a UUID newtype), [`MilestoneStatus`], and the
//! [`Milestone`] struct. Per §10.3 a Milestone's status is a *rollup* of its
//! contained Deliverables — it is NOT a user-driven state machine, so (unlike
//! [`DeliverableStatus`](crate::deliverable::DeliverableStatus)) this crate
//! enforces no transition table on it; computing the rollup is #2382 / the
//! read-only status endpoint's job (§4.1). Here it is a plain stored field.
//! Test: `milestone_serde_round_trip`, `milestone_id_round_trip`,
//! `milestone_status_wire_format`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::record::DeliverableId;

/// A stable, globally-unique identifier for a [`Milestone`].
///
/// Why: a newtype prevents mixing Milestone ids with Deliverable/session ids at
/// the type level, matching the id-newtype convention used elsewhere.
/// What: wraps `uuid::Uuid`; transparent serde; `Display`/`FromStr` for routing.
/// Test: `milestone_id_round_trip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MilestoneId(pub Uuid);

impl MilestoneId {
    /// Generate a new random Milestone id.
    ///
    /// Why: every Milestone created via the API needs a fresh unique id.
    /// What: wraps `Uuid::new_v4()`.
    /// Test: `milestone_serde_round_trip`.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for MilestoneId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for MilestoneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for MilestoneId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

/// The status of a [`Milestone`] — a rollup of its Deliverables (§10.5).
///
/// Why: §10.3/§10.5 define Milestone status as mirroring the rollup of its
/// contained Deliverables, not an independently user-driven machine; the closed
/// enum still gives a stable wire vocabulary for the CRUD API and the future
/// histogram endpoint.
/// What: the four milestone states from §10.5, serialized kebab-case. No
/// `Blocked` variant — blocking is a per-Deliverable concept.
/// Test: `milestone_status_wire_format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MilestoneStatus {
    /// No contained Deliverable has started.
    #[default]
    Proposed,
    /// At least one contained Deliverable is in progress.
    InProgress,
    /// All contained Deliverables are complete.
    Complete,
    /// The Milestone shipped.
    Shipped,
}

/// A dated grouping of Deliverables (DOC-35 §10.5).
///
/// Why: the persisted record the Milestone CRUD API creates, reads, and mutates.
/// Keyed by `id` in the store; references its project by `project_name` and its
/// members by their [`DeliverableId`]s.
/// What: identity, project linkage, a dated `target_date`, the rollup `status`,
/// the member Deliverable ids, and a creation timestamp.
/// Test: `milestone_serde_round_trip`, and the store/route tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Milestone {
    /// Store key: a stable UUID assigned at creation.
    pub id: MilestoneId,

    /// The owning project's registry-B name (`repo_url`-keyed `Project.name`).
    pub project_name: String,

    /// Human-readable name, e.g. `"v1.0 Alpha"`.
    pub name: String,

    /// Free-form description.
    #[serde(default)]
    pub description: String,

    /// The date the Milestone targets.
    pub target_date: DateTime<Utc>,

    /// Rollup status (§10.5); a plain stored field here (rollup computation is
    /// #2382 / the read-only status endpoint).
    #[serde(default)]
    pub status: MilestoneStatus,

    /// The Deliverables that make up this Milestone.
    #[serde(default)]
    pub deliverables: Vec<DeliverableId>,

    /// When the Milestone record was created.
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Milestone {
        Milestone {
            id: MilestoneId::new(),
            project_name: "trusty-tools".into(),
            name: "v1.0 Alpha".into(),
            description: "first alpha".into(),
            target_date: Utc::now(),
            status: MilestoneStatus::Proposed,
            deliverables: vec![DeliverableId::new(), DeliverableId::new()],
            created_at: Utc::now(),
        }
    }

    #[test]
    fn milestone_serde_round_trip() {
        let m = sample();
        let json = serde_json::to_string(&m).unwrap();
        let back: Milestone = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn milestone_id_round_trip() {
        let id = MilestoneId::new();
        let parsed: MilestoneId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn milestone_status_wire_format() {
        assert_eq!(
            serde_json::to_string(&MilestoneStatus::InProgress).unwrap(),
            "\"in-progress\""
        );
        assert_eq!(
            serde_json::to_string(&MilestoneStatus::Shipped).unwrap(),
            "\"shipped\""
        );
        assert_eq!(MilestoneStatus::default(), MilestoneStatus::Proposed);
    }
}
