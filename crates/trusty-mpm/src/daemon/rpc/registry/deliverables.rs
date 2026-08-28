//! Deliverable and Milestone CRUD as RPC methods (#6288 slice 5).
//!
//! Why registration only, with no bodies: `api::deliverable_routes` had SLOC
//! headroom, so its `*_op` functions stayed beside the handlers that share them
//! — which is the preferred shape. A body moves out of its route file only when
//! the file cannot hold it.
//!
//! What: eight methods over the four Deliverable and four Milestone verbs. The
//! `{name}`/`{id}` path segments become named parameter fields, since a JSON-RPC
//! call has no path to carry them in.
//!
//! Test: the `parity_deliverable_*` and `parity_milestone_*` cases in
//! `super::tests`.

use std::sync::Arc;

use serde::Deserialize;
use trusty_common::uds::server::RpcRouter;

use crate::daemon::api::deliverable_routes as dl;
use crate::daemon::state::DaemonState;
use crate::deliverable::{Deliverable, DeliverableStatus, Milestone};

/// The project scope every one of these routes nests under.
#[derive(Debug, Deserialize)]
pub struct ProjectParams {
    /// The project name from the URL path.
    pub project: String,
}

/// A project scope plus the optional `?status=` filter.
#[derive(Debug, Deserialize)]
pub struct ListDeliverablesParams {
    /// The project name from the URL path.
    pub project: String,
    /// Optional status filter, matching the HTTP query parameter.
    #[serde(default)]
    pub status: Option<DeliverableStatus>,
}

/// A project scope plus a record id.
#[derive(Debug, Deserialize)]
pub struct RecordParams {
    /// The project name from the URL path.
    pub project: String,
    /// The Deliverable or Milestone id from the URL path.
    pub id: String,
}

/// A project scope plus a create body.
#[derive(Debug, Deserialize)]
pub struct CreateDeliverableParams {
    /// The project name from the URL path.
    pub project: String,
    /// The create body, verbatim.
    #[serde(flatten)]
    pub body: dl::CreateDeliverable,
}

/// A project scope plus a Milestone create body.
#[derive(Debug, Deserialize)]
pub struct CreateMilestoneParams {
    /// The project name from the URL path.
    pub project: String,
    /// The create body, verbatim.
    #[serde(flatten)]
    pub body: dl::CreateMilestone,
}

/// A project scope, a record id, and a Deliverable patch body.
#[derive(Debug, Deserialize)]
pub struct PatchDeliverableParams {
    /// The project name from the URL path.
    pub project: String,
    /// The Deliverable id from the URL path.
    pub id: String,
    /// The patch body, verbatim.
    #[serde(flatten)]
    pub body: dl::PatchDeliverable,
}

/// A project scope, a record id, and a Milestone patch body.
#[derive(Debug, Deserialize)]
pub struct PatchMilestoneParams {
    /// The project name from the URL path.
    pub project: String,
    /// The Milestone id from the URL path.
    pub id: String,
    /// The patch body, verbatim.
    #[serde(flatten)]
    pub body: dl::PatchMilestone,
}

/// Mount the eight Deliverable/Milestone methods.
///
/// Test: `rpc_router_registers_every_documented_method`.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let held = Arc::clone(state);
    let r = router.typed::<CreateDeliverableParams, Deliverable, _, _>(
        "mpm.deliverables.create",
        move |p| {
            let s = Arc::clone(&held);
            async move {
                dl::create_deliverable_op(&s, p.project, p.body)
                    .await
                    .map_err(Into::into)
            }
        },
    );

    let held = Arc::clone(state);
    let r = r.typed::<ListDeliverablesParams, dl::DeliverablesResponse, _, _>(
        "mpm.deliverables.list",
        move |p| {
            let s = Arc::clone(&held);
            async move {
                dl::list_deliverables_op(
                    &s,
                    &p.project,
                    dl::DeliverableListQuery { status: p.status },
                )
                .await
                .map_err(Into::into)
            }
        },
    );

    let held = Arc::clone(state);
    let r = r.typed::<RecordParams, Deliverable, _, _>("mpm.deliverables.get", move |p| {
        let s = Arc::clone(&held);
        async move {
            dl::fetch_scoped(&s, &p.project, &p.id)
                .await
                .map_err(Into::into)
        }
    });

    let held = Arc::clone(state);
    let r =
        r.typed::<PatchDeliverableParams, Deliverable, _, _>("mpm.deliverables.patch", move |p| {
            let s = Arc::clone(&held);
            async move {
                dl::patch_deliverable_op(&s, &p.project, &p.id, p.body)
                    .await
                    .map_err(Into::into)
            }
        });

    let held = Arc::clone(state);
    let r = r.typed::<CreateMilestoneParams, Milestone, _, _>("mpm.milestones.create", move |p| {
        let s = Arc::clone(&held);
        async move {
            dl::create_milestone_op(&s, p.project, p.body)
                .await
                .map_err(Into::into)
        }
    });

    let held = Arc::clone(state);
    let r =
        r.typed::<ProjectParams, dl::MilestonesResponse, _, _>("mpm.milestones.list", move |p| {
            let s = Arc::clone(&held);
            async move {
                dl::list_milestones_op(&s, &p.project)
                    .await
                    .map_err(Into::into)
            }
        });

    let held = Arc::clone(state);
    let r = r.typed::<RecordParams, Milestone, _, _>("mpm.milestones.get", move |p| {
        let s = Arc::clone(&held);
        async move {
            dl::fetch_scoped_milestone(&s, &p.project, &p.id)
                .await
                .map_err(Into::into)
        }
    });

    let held = Arc::clone(state);
    r.typed::<PatchMilestoneParams, Milestone, _, _>("mpm.milestones.patch", move |p| {
        let s = Arc::clone(&held);
        async move {
            dl::patch_milestone_op(&s, &p.project, &p.id, p.body)
                .await
                .map_err(Into::into)
        }
    })
}
