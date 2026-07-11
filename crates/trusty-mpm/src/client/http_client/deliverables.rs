//! Deliverable/Milestone methods for [`DaemonClient`] (DOC-35 §10.8, #2381).
//!
//! Why: the `tm projects deliverables|milestones` CLI subtrees are thin HTTP
//! clients over the Deliverable/Milestone CRUD API (#2378/#2380). Wiring each
//! endpoint once here keeps the CLI off hand-rolled `reqwest` calls, matching the
//! `managed.rs`/`projects.rs` sibling pattern. The one non-trivial seam is
//! `set-status`: the daemon enforces the §10.3 state machine and rejects an
//! illegal transition with a structured 409 (`{ from, to, allowed_next }`, #2380).
//! [`DaemonClient::set_deliverable_status`] parses that body into a typed
//! [`SetStatusError::Rejected`] so the CLI can show the operator the LEGAL next
//! states instead of a bare "409 Conflict".
//! What: request DTOs the client owns ([`CreateDeliverableArgs`],
//! [`CreateMilestoneArgs`]), the [`SetStatusError`] type, and the CRUD methods for
//! both resources. Response bodies deserialize into the domain types
//! (`crate::deliverable::{Deliverable, Milestone}`), which already derive
//! `Deserialize`. Status values serialize kebab-case via [`DeliverableStatus`]'s
//! own serde, matching the daemon's `PatchDeliverable.status`.
//! Test: `create_deliverable_args_serializes`, `create_milestone_args_serializes`,
//! `transition_rejection_body_deserializes`, `deliverables_list_deserializes` in
//! the `tests` submodule; live HTTP via the daemon's `deliverable_routes::tests`.

use anyhow::Context;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use super::DaemonClient;
use crate::deliverable::{
    Deliverable, DeliverableKind, DeliverableStatus, EstimationTier, Milestone,
};

/// Serializable body for `POST .../deliverables`.
///
/// Why: `tm projects deliverables add` builds this from its flags; a typed struct
/// keeps the body checkable in a unit test. Matches the daemon's
/// `CreateDeliverable` (which fixes `status = Proposed` and assigns `id`/
/// `created_at` server-side, so neither is sent).
/// What: required `name`/`kind`/`estimated_effort` plus optional description and
/// the opaque `ticket_ref`/`spec_ref`/`target_date` slots.
/// Test: `create_deliverable_args_serializes`.
#[derive(Debug, Clone, Serialize)]
pub struct CreateDeliverableArgs {
    /// Human-readable name.
    pub name: String,
    /// Free-form description (daemon defaults to empty when omitted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Category of work.
    pub kind: DeliverableKind,
    /// Coarse effort tier (S/M/L/XL).
    pub estimated_effort: EstimationTier,
    /// Opaque gh-first ticket reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket_ref: Option<String>,
    /// Repo-relative spec path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_ref: Option<String>,
}

/// Serializable body for `POST .../milestones`.
///
/// Why: `tm projects milestones add` builds this. Matches the daemon's
/// `CreateMilestone`; `status` defaults to `Proposed` server-side and member
/// deliverable ids are not settable from the CLI at creation (§10.8 `add` takes
/// name + target date), so neither is sent here.
/// What: required `name` and `target_date`, optional description.
/// Test: `create_milestone_args_serializes`.
#[derive(Debug, Clone, Serialize)]
pub struct CreateMilestoneArgs {
    /// Human-readable name.
    pub name: String,
    /// Free-form description (daemon defaults to empty when omitted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The date this Milestone targets.
    pub target_date: DateTime<Utc>,
}

/// The structured `409 Conflict` body the daemon returns for an illegal
/// `set-status` transition (#2380).
///
/// Why: the CLI must show the operator the legal next states; parsing this body
/// is how [`SetStatusError::Rejected`] carries them out of the client.
/// What: mirrors the daemon `DaemonError::InvalidTransition` JSON shape.
/// Test: `transition_rejection_body_deserializes`.
#[derive(Debug, Clone, Deserialize)]
struct TransitionRejectionBody {
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    #[serde(default)]
    allowed_next: Vec<String>,
}

/// Outcome of a rejected or failed [`DaemonClient::set_deliverable_status`].
///
/// Why: `set-status` has two failure modes the CLI treats differently — an
/// illegal transition (409, recoverable: the caller picks a legal state) versus
/// any other error (network, 404, 500). Splitting them lets the CLI render the
/// legal-next-states guidance only for the transition case.
/// What: `Rejected` carries the §10.3 legal next states; `Other` wraps everything
/// else as an [`anyhow::Error`].
/// Test: driven via `set_deliverable_status`'s 409 branch (client route tests) and
/// `transition_rejection_body_deserializes`.
#[derive(Debug)]
pub enum SetStatusError {
    /// The daemon rejected the transition; carries the legal next states (#2380).
    Rejected {
        /// The deliverable's current status (as the daemon reported it).
        from: String,
        /// The rejected target status.
        to: String,
        /// The states that WOULD have been legal from `from`.
        allowed_next: Vec<String>,
    },
    /// Any non-409 failure (network, 404, 500, deserialization).
    Other(anyhow::Error),
}

impl std::fmt::Display for SetStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetStatusError::Rejected {
                from,
                to,
                allowed_next,
            } => {
                let allowed = if allowed_next.is_empty() {
                    "none (terminal state)".to_string()
                } else {
                    allowed_next.join(", ")
                };
                write!(
                    f,
                    "illegal status transition {from} -> {to}; legal next states from {from}: [{allowed}]"
                )
            }
            SetStatusError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SetStatusError {}

/// Wire wrapper for `GET .../deliverables` (`{ deliverables: [...] }`).
#[derive(Debug, Deserialize)]
struct DeliverablesListWire {
    #[serde(default)]
    deliverables: Vec<Deliverable>,
}

/// Wire wrapper for `GET .../milestones` (`{ milestones: [...] }`).
#[derive(Debug, Deserialize)]
struct MilestonesListWire {
    #[serde(default)]
    milestones: Vec<Milestone>,
}

impl DaemonClient {
    /// List a project's Deliverables via `GET .../deliverables[?status=]`.
    ///
    /// Why: backs `tm projects deliverables list <project> [--status <s>]`.
    /// What: GETs the collection with an optional `?status=` filter, returns the
    /// `deliverables` array.
    /// Test: `deliverables_list_deserializes`; live HTTP via `deliverable_routes`.
    pub async fn list_deliverables(
        &self,
        project: &str,
        status: Option<DeliverableStatus>,
    ) -> anyhow::Result<Vec<Deliverable>> {
        let url = format!("{}/api/v1/projects/{project}/deliverables", self.base);
        let mut req = self.http.get(&url);
        if let Some(status) = status {
            req = req.query(&[("status", status.as_str())]);
        }
        let body: DeliverablesListWire = req
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("deserialize deliverable list")?;
        Ok(body.deliverables)
    }

    /// Create a Deliverable via `POST .../deliverables`.
    ///
    /// Why: backs `tm projects deliverables add`.
    /// What: POSTs `args`, returns the created [`Deliverable`] (status `Proposed`).
    /// Test: `create_deliverable_args_serializes`; live HTTP via `deliverable_routes`.
    pub async fn create_deliverable(
        &self,
        project: &str,
        args: &CreateDeliverableArgs,
    ) -> anyhow::Result<Deliverable> {
        let url = format!("{}/api/v1/projects/{project}/deliverables", self.base);
        let created: Deliverable = self
            .http
            .post(&url)
            .json(args)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("deserialize created deliverable")?;
        Ok(created)
    }

    /// Fetch one Deliverable via `GET .../deliverables/{id}`.
    ///
    /// Why: backs `tm projects deliverables show <project> <id>`.
    /// What: GETs the point-lookup, returns the [`Deliverable`].
    /// Test: live HTTP via `deliverable_routes`.
    pub async fn get_deliverable(&self, project: &str, id: &str) -> anyhow::Result<Deliverable> {
        let url = format!("{}/api/v1/projects/{project}/deliverables/{id}", self.base);
        let d: Deliverable = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("deserialize deliverable")?;
        Ok(d)
    }

    /// Transition a Deliverable's status via `PATCH .../deliverables/{id}`.
    ///
    /// Why: backs `tm projects deliverables set-status`. The daemon enforces the
    /// §10.3 state machine (#2380); an illegal transition returns a structured 409
    /// this method converts into [`SetStatusError::Rejected`] carrying the legal
    /// next states, so the CLI can guide the operator.
    /// What: PATCHes `{ "status": <status> }`; on 409 parses the transition body,
    /// otherwise returns the updated [`Deliverable`].
    /// Test: `transition_rejection_body_deserializes`; live HTTP via
    /// `deliverable_routes::patch_illegal_transition_is_409_with_allowed_next`.
    pub async fn set_deliverable_status(
        &self,
        project: &str,
        id: &str,
        status: DeliverableStatus,
    ) -> Result<Deliverable, SetStatusError> {
        let url = format!("{}/api/v1/projects/{project}/deliverables/{id}", self.base);
        let resp = self
            .http
            .patch(&url)
            .json(&serde_json::json!({ "status": status }))
            .send()
            .await
            .map_err(|e| SetStatusError::Other(e.into()))?;

        if resp.status() == StatusCode::CONFLICT {
            let body: TransitionRejectionBody = resp
                .json()
                .await
                .map_err(|e| SetStatusError::Other(e.into()))?;
            return Err(SetStatusError::Rejected {
                from: body.from,
                to: body.to,
                allowed_next: body.allowed_next,
            });
        }

        let resp = resp
            .error_for_status()
            .map_err(|e| SetStatusError::Other(e.into()))?;
        resp.json()
            .await
            .context("deserialize updated deliverable")
            .map_err(SetStatusError::Other)
    }

    /// List a project's Milestones via `GET .../milestones`.
    ///
    /// Why: backs `tm projects milestones list <project>`.
    /// What: GETs the collection, returns the `milestones` array.
    /// Test: `milestones_list_deserializes`; live HTTP via `deliverable_routes`.
    pub async fn list_milestones(&self, project: &str) -> anyhow::Result<Vec<Milestone>> {
        let url = format!("{}/api/v1/projects/{project}/milestones", self.base);
        let body: MilestonesListWire = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("deserialize milestone list")?;
        Ok(body.milestones)
    }

    /// Create a Milestone via `POST .../milestones`.
    ///
    /// Why: backs `tm projects milestones add`.
    /// What: POSTs `args`, returns the created [`Milestone`].
    /// Test: `create_milestone_args_serializes`; live HTTP via `deliverable_routes`.
    pub async fn create_milestone(
        &self,
        project: &str,
        args: &CreateMilestoneArgs,
    ) -> anyhow::Result<Milestone> {
        let url = format!("{}/api/v1/projects/{project}/milestones", self.base);
        let created: Milestone = self
            .http
            .post(&url)
            .json(args)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("deserialize created milestone")?;
        Ok(created)
    }

    /// Fetch one Milestone via `GET .../milestones/{id}`.
    ///
    /// Why: backs `tm projects milestones show <project> <id>`.
    /// What: GETs the point-lookup, returns the [`Milestone`].
    /// Test: live HTTP via `deliverable_routes`.
    pub async fn get_milestone(&self, project: &str, id: &str) -> anyhow::Result<Milestone> {
        let url = format!("{}/api/v1/projects/{project}/milestones/{id}", self.base);
        let m: Milestone = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("deserialize milestone")?;
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_deliverable_args_serializes() {
        let args = CreateDeliverableArgs {
            name: "OAuth2 flow".into(),
            description: None,
            kind: DeliverableKind::Feature,
            estimated_effort: EstimationTier::L,
            ticket_ref: Some("#2117".into()),
            spec_ref: None,
        };
        let v = serde_json::to_value(&args).unwrap();
        assert_eq!(v["name"], "OAuth2 flow");
        // kind serializes lowercase, tier uppercase — matching the daemon DTO.
        assert_eq!(v["kind"], "feature");
        assert_eq!(v["estimated_effort"], "L");
        assert_eq!(v["ticket_ref"], "#2117");
        assert!(v.get("description").is_none());
        assert!(v.get("spec_ref").is_none());
    }

    #[test]
    fn create_milestone_args_serializes() {
        let args = CreateMilestoneArgs {
            name: "v1.0 Alpha".into(),
            description: Some("first alpha".into()),
            target_date: DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let v = serde_json::to_value(&args).unwrap();
        assert_eq!(v["name"], "v1.0 Alpha");
        assert_eq!(v["description"], "first alpha");
        assert!(v["target_date"].is_string());
    }

    /// The 409 body parses into the typed rejection the CLI renders.
    #[test]
    fn transition_rejection_body_deserializes() {
        let json = serde_json::json!({
            "error": "invalid status transition proposed → complete; ...",
            "from": "proposed",
            "to": "complete",
            "allowed_next": ["in-progress"]
        });
        let body: TransitionRejectionBody = serde_json::from_value(json).unwrap();
        assert_eq!(body.from, "proposed");
        assert_eq!(body.to, "complete");
        assert_eq!(body.allowed_next, vec!["in-progress".to_string()]);
    }

    /// The `SetStatusError::Rejected` Display names the legal next states.
    #[test]
    fn set_status_rejected_display_lists_allowed() {
        let e = SetStatusError::Rejected {
            from: "proposed".into(),
            to: "complete".into(),
            allowed_next: vec!["in-progress".into()],
        };
        let msg = e.to_string();
        assert!(msg.contains("proposed"), "{msg}");
        assert!(msg.contains("complete"), "{msg}");
        assert!(msg.contains("in-progress"), "{msg}");
    }

    #[test]
    fn deliverables_list_deserializes() {
        let json = serde_json::json!({
            "deliverables": [{
                "id": "00000000-0000-0000-0000-000000000001",
                "project_name": "widget",
                "name": "OAuth2 flow",
                "kind": "feature",
                "status": "proposed",
                "estimated_effort": "L",
                "created_at": "2026-07-10T12:00:00Z"
            }]
        });
        let body: DeliverablesListWire = serde_json::from_value(json).unwrap();
        assert_eq!(body.deliverables.len(), 1);
        assert_eq!(body.deliverables[0].name, "OAuth2 flow");
        assert_eq!(body.deliverables[0].status, DeliverableStatus::Proposed);
    }

    #[test]
    fn milestones_list_deserializes() {
        let json = serde_json::json!({
            "milestones": [{
                "id": "00000000-0000-0000-0000-000000000002",
                "project_name": "widget",
                "name": "v1.0 Alpha",
                "target_date": "2026-09-01T00:00:00Z",
                "status": "proposed",
                "deliverables": [],
                "created_at": "2026-07-10T12:00:00Z"
            }]
        });
        let body: MilestonesListWire = serde_json::from_value(json).unwrap();
        assert_eq!(body.milestones.len(), 1);
        assert_eq!(body.milestones[0].name, "v1.0 Alpha");
    }
}
