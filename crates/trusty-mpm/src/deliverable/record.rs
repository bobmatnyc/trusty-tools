//! The [`Deliverable`] record and its value types (DOC-35 §10.2).
//!
//! Why: a Deliverable is the L3-substrate unit of work — "does this exist, what
//! tier is it, what state is it in" (§10.1). It must be serializable to survive
//! daemon restarts and be exchanged over HTTP without ambiguity, and its shape
//! must be ready to be referenced by a session (the `SessionRecord.deliverable_id`
//! link is Wave-2 #2379, NOT added here — but the `id` this file defines is what
//! that field will point at).
//! What: defines [`DeliverableId`] (a UUID newtype), [`DeliverableKind`],
//! [`EstimationTier`] (S/M/L/XL — coarse tiers, no hours, DOC-30 Decision #2),
//! and the [`Deliverable`] struct. `spec_ref` is a plain repo-relative path
//! string (§13 Q6: no `SpecRef` abstraction); `ticket_ref` is a loosely-typed
//! opaque slot (§13 Q6: gh-first, validation deferred).
//! Test: `deliverable_serde_round_trip`, `deliverable_id_round_trip`,
//! `estimation_tier_wire_format`, `kind_wire_format`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::status::DeliverableStatus;

/// A stable, globally-unique identifier for a [`Deliverable`].
///
/// Why: a newtype over [`Uuid`] prevents accidental confusion with other
/// UUID-typed identifiers ([`MilestoneId`](crate::deliverable::MilestoneId),
/// `ManagedSessionId`) at the type level, mirroring the existing id-newtype
/// convention in `session_manager::record`.
/// What: wraps `uuid::Uuid`; serializes transparently as the bare UUID string;
/// implements `Display`/`FromStr` for path-param parsing in the CRUD routes.
/// Test: `deliverable_id_round_trip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeliverableId(pub Uuid);

impl DeliverableId {
    /// Generate a new random Deliverable id.
    ///
    /// Why: every Deliverable created via the API needs a fresh, unique id.
    /// What: wraps `Uuid::new_v4()`.
    /// Test: `deliverable_serde_round_trip`.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for DeliverableId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for DeliverableId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for DeliverableId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

/// The category of work a [`Deliverable`] represents (§10.2).
///
/// Why: a small closed vocabulary lets operators filter and group Deliverables
/// without parsing free-text descriptions.
/// What: the six kinds from §10.2, serialized lowercase (`feature`, `bugfix`, …).
/// Test: `kind_wire_format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeliverableKind {
    /// A new capability.
    Feature,
    /// A defect repair.
    Bugfix,
    /// A behavior-preserving restructuring.
    Refactor,
    /// Maintenance / housekeeping.
    Chore,
    /// Test-only work.
    Test,
    /// Documentation-only work.
    Docs,
}

/// Coarse-grained effort estimate (DOC-30 Decision #2, unchanged).
///
/// Why: tier-based estimation deliberately avoids the false precision of
/// hour/range estimates. S/M/L/XL is enough signal for planning without inviting
/// spurious accuracy — and it stays deterministic (no inference, §11).
/// What: the four tiers, serialized as their uppercase letters (`S`, `M`, `L`,
/// `XL`) to match the CLI `--estimate` values in §10.8.
/// Test: `estimation_tier_wire_format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum EstimationTier {
    /// Small.
    S,
    /// Medium.
    M,
    /// Large.
    L,
    /// Extra-large.
    Xl,
}

/// A discrete unit of work within a project (DOC-35 §10.2).
///
/// Why: this is the persisted record the Deliverable CRUD API (#2378) creates,
/// reads, and mutates. It is keyed by `id` in the store but references its owning
/// project by `project_name` (registry-B `Project.name`, `repo_url`-keyed — this
/// epic introduces no second project identity, §10.2). It is intentionally flat
/// (no recursive sub-tasks, DOC-30 Decision #3).
/// What: captures identity, project linkage, descriptive metadata, the lifecycle
/// `status` ([`DeliverableStatus`], §10.3), a coarse `estimated_effort` tier, and
/// creation/target timestamps. `ticket_ref` is an opaque gh-first slot;
/// `spec_ref` is a plain `docs/specs/*.md` path (§13 Q6).
/// Test: `deliverable_serde_round_trip`, and the store/route tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deliverable {
    /// Store key: a stable UUID assigned at creation.
    pub id: DeliverableId,

    /// The owning project's registry-B name (`repo_url`-keyed `Project.name`).
    ///
    /// Why: Deliverables are always scoped to exactly one project; the CRUD
    /// routes nest under `/api/v1/projects/{name}/deliverables`, and this field
    /// records that ownership so the central store can be filtered by project.
    pub project_name: String,

    /// Human-readable name, e.g. `"OAuth2 authentication flow"`.
    pub name: String,

    /// Free-form description of the work.
    #[serde(default)]
    pub description: String,

    /// The category of work.
    pub kind: DeliverableKind,

    /// Opaque, loosely-typed ticket reference (e.g. `"#2117"`), §13 Q6.
    ///
    /// Why: gh-first today; JIRA (#2082) may write through this slot later, so it
    /// is intentionally unvalidated — a placeholder, not a typed reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_ref: Option<String>,

    /// Repo-relative path to the spec this Deliverable implements (§10.4, §13 Q6).
    ///
    /// Why: manual linking only (§10.4) — the operator sets it explicitly; no
    /// auto-scan (that would be inference, out of scope §11). A plain path string,
    /// not an abstraction (§13 Q6), e.g. `"docs/specs/tm-project-control-plane.md"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_ref: Option<String>,

    /// Lifecycle state, mutated only via the §10.3 state machine (#2380).
    pub status: DeliverableStatus,

    /// Coarse effort estimate.
    pub estimated_effort: EstimationTier,

    /// When the Deliverable record was created.
    pub created_at: DateTime<Utc>,

    /// Optional target completion date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_date: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Deliverable {
        Deliverable {
            id: DeliverableId::new(),
            project_name: "trusty-tools".into(),
            name: "OAuth2 flow".into(),
            description: "add oauth".into(),
            kind: DeliverableKind::Feature,
            ticket_ref: Some("#2117".into()),
            spec_ref: Some("docs/specs/tm-project-control-plane.md".into()),
            status: DeliverableStatus::Proposed,
            estimated_effort: EstimationTier::L,
            created_at: Utc::now(),
            target_date: None,
        }
    }

    #[test]
    fn deliverable_serde_round_trip() {
        let d = sample();
        let json = serde_json::to_string(&d).unwrap();
        let back: Deliverable = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn deliverable_id_round_trip() {
        let id = DeliverableId::new();
        let s = id.to_string();
        let parsed: DeliverableId = s.parse().unwrap();
        assert_eq!(id, parsed);
        // Transparent serde: the JSON is the bare quoted UUID.
        assert_eq!(serde_json::to_string(&id).unwrap(), format!("\"{s}\""));
    }

    #[test]
    fn estimation_tier_wire_format() {
        assert_eq!(serde_json::to_string(&EstimationTier::S).unwrap(), "\"S\"");
        assert_eq!(
            serde_json::to_string(&EstimationTier::Xl).unwrap(),
            "\"XL\""
        );
        let back: EstimationTier = serde_json::from_str("\"M\"").unwrap();
        assert_eq!(back, EstimationTier::M);
    }

    #[test]
    fn kind_wire_format() {
        assert_eq!(
            serde_json::to_string(&DeliverableKind::Feature).unwrap(),
            "\"feature\""
        );
        assert_eq!(
            serde_json::to_string(&DeliverableKind::Bugfix).unwrap(),
            "\"bugfix\""
        );
        let back: DeliverableKind = serde_json::from_str("\"docs\"").unwrap();
        assert_eq!(back, DeliverableKind::Docs);
    }

    #[test]
    fn optional_fields_omitted_when_absent() {
        let d = Deliverable {
            ticket_ref: None,
            spec_ref: None,
            target_date: None,
            ..sample()
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(!json.contains("ticket_ref"));
        assert!(!json.contains("spec_ref"));
        assert!(!json.contains("target_date"));
    }
}
