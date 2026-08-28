//! The registry, deliverable, manager, bus, pairing and delegation RPC methods
//! (#6288 slice 5).
//!
//! Why one module for six families: they are the daemon's RECORD-KEEPING
//! surface — projects and their deliverables, the L3 rollup over both, the peer
//! roster, the bot binding, and the delegation ledger. They share one property
//! that matters here: every one of them is a plain request/response route over
//! daemon-owned state, so all of them move onto the socket by the same
//! mechanism and none of them needs the streaming seam. Splitting them across
//! six top-level modules would put six `register` lines in `socket.rs` for one
//! slice.
//!
//! What: [`register`] mounts all of them, and [`METHODS`] pins the names. The
//! per-family registrations live in the submodules, and the transport-neutral
//! bodies live beside their HTTP handlers — except where the handler's file sits
//! on a frozen SLOC budget, which is why the legacy `/projects*` and `/pair/*`
//! bodies live in [`projects`] and [`pairing`] instead of in `api.rs`.
//!
//! Nothing in this module or its submodules names an HTTP type. The two error
//! enums that cross both transports — [`DaemonError`] and [`BusError`] — each
//! own their own `From<…> for RpcError`, derived from the same status table the
//! HTTP transport reads, so a code and a status cannot drift apart.
//!
//! ## Method → route
//!
//! | Method | Route |
//! |---|---|
//! | `mpm.projects.list` | `GET /projects` |
//! | `mpm.projects.register` | `POST /projects` |
//! | `mpm.projects.current` | `GET /projects/current?path=` |
//! | `mpm.projects.discover` | `GET /projects/discover` |
//! | `mpm.projects.registry.list` | `GET /api/v1/projects` |
//! | `mpm.projects.registry.register` | `POST /api/v1/projects` |
//! | `mpm.projects.registry.get` | `GET /api/v1/projects/{name}` |
//! | `mpm.projects.registry.patch` | `PATCH /api/v1/projects/{name}` |
//! | `mpm.projects.status` | `GET /api/v1/projects/{name}/status` |
//! | `mpm.deliverables.create` | `POST /api/v1/projects/{name}/deliverables` |
//! | `mpm.deliverables.list` | `GET /api/v1/projects/{name}/deliverables` |
//! | `mpm.deliverables.get` | `GET /api/v1/projects/{name}/deliverables/{id}` |
//! | `mpm.deliverables.patch` | `PATCH /api/v1/projects/{name}/deliverables/{id}` |
//! | `mpm.milestones.create` | `POST /api/v1/projects/{name}/milestones` |
//! | `mpm.milestones.list` | `GET /api/v1/projects/{name}/milestones` |
//! | `mpm.milestones.get` | `GET /api/v1/projects/{name}/milestones/{id}` |
//! | `mpm.milestones.patch` | `PATCH /api/v1/projects/{name}/milestones/{id}` |
//! | `mpm.manager.version` | `GET /api/v1/manager/version` |
//! | `mpm.manager.status` | `GET /api/v1/manager/status` |
//! | `mpm.manager.digest` | `GET /api/v1/manager/digest?scope=` |
//! | `mpm.manager.chat` | `POST /api/v1/manager/chat` |
//! | `mpm.manager.route_task` | `POST /api/v1/manager/route-task` |
//! | `mpm.manager.act` | `POST /api/v1/manager/act` |
//! | `mpm.bus.register` | `POST /api/v1/bus/instances` |
//! | `mpm.bus.deregister` | `DELETE /api/v1/bus/instances/{instance_id}` |
//! | `mpm.bus.list` | `GET /api/v1/bus/instances` |
//! | `mpm.bus.publish` | `POST /api/v1/bus/publish` |
//! | `mpm.pair.request` | `POST /pair/request` |
//! | `mpm.pair.confirm` | `POST /pair/confirm` |
//! | `mpm.pair.status` | `GET /pair/status` |
//! | `mpm.pair.reset` | `POST /pair/reset` |
//! | `mpm.delegation.shared_tree_dispatch` | `POST /api/v1/sessions/{id}/delegations/shared-tree-dispatch` |
//! | `mpm.delegation.granted_worktree` | `POST /api/v1/sessions/{id}/delegations/granted-worktree` |
//!
//! **`mpm.manager.digest` reports failure in its RESULT, not an error frame.**
//! Where HTTP answers `503` (no inference provider) or `502` (the provider call
//! failed), the socket answers a normal result carrying the same complete
//! `DigestResponse` — deterministic narrative plus the full rollup — with
//! `error: Some("inference_unavailable")` or `Some("inference_failed")`. A
//! caller that reads only the narrative will silently treat a degraded digest as
//! an LLM-authored one, so **check `error` before trusting `narrative`**;
//! `generated_by` says the same thing a second way. The three genuinely empty
//! refusals — malformed scope, unknown project, store read failed — ARE error
//! frames. See [`manager`]'s module doc for why the degrade legs cannot be.
//!
//! **Pending slice 6:** `GET /api/v1/bus/subscribe/{instance_id}` is SSE and has
//! no RPC method here. It needs the `RpcStreamMethod` seam slice 6 owns; its
//! HTTP handler is untouched by this slice.
//!
//! ## Path and query parameters become named fields
//!
//! A JSON-RPC call has no path and no query string, so `{name}`, `{id}`,
//! `{instance_id}` and `?status=` / `?scope=` / `?path=` all arrive as named
//! parameter fields. Where a route takes a path segment AND a body, the params
//! struct `#[serde(flatten)]`s the body around the segment field, so the wire is
//! the HTTP body plus one key rather than a nested object.
//!
//! Test: `registry/tests.rs` — one parity case per route.
//!
//! [`DaemonError`]: crate::daemon::error::DaemonError
//! [`BusError`]: crate::daemon::bus::BusError

use std::sync::Arc;

use serde::Deserialize;
use trusty_common::uds::server::RpcRouter;

use crate::daemon::state::DaemonState;

pub mod bus;
pub mod delegation;
pub mod deliverables;
pub mod manager;
pub mod pairing;
pub mod projects;

#[cfg(test)]
mod tests;

/// Every method name this module registers, sorted.
///
/// Why pinned here: the slice-7 client swap and trusty-console will dial these
/// names by literal, with no compile-time link to the registrations. A rename
/// then becomes a failing assertion rather than a consumer that silently
/// reports `method_not_found`.
/// Test: `rpc_router_registers_every_documented_method`.
pub const METHODS: &[&str] = &[
    "mpm.bus.deregister",
    "mpm.bus.list",
    "mpm.bus.publish",
    "mpm.bus.register",
    "mpm.delegation.granted_worktree",
    "mpm.delegation.shared_tree_dispatch",
    "mpm.deliverables.create",
    "mpm.deliverables.get",
    "mpm.deliverables.list",
    "mpm.deliverables.patch",
    "mpm.manager.act",
    "mpm.manager.chat",
    "mpm.manager.digest",
    "mpm.manager.route_task",
    "mpm.manager.status",
    "mpm.manager.version",
    "mpm.milestones.create",
    "mpm.milestones.get",
    "mpm.milestones.list",
    "mpm.milestones.patch",
    "mpm.pair.confirm",
    "mpm.pair.request",
    "mpm.pair.reset",
    "mpm.pair.status",
    "mpm.projects.current",
    "mpm.projects.discover",
    "mpm.projects.list",
    "mpm.projects.register",
    "mpm.projects.registry.get",
    "mpm.projects.registry.list",
    "mpm.projects.registry.patch",
    "mpm.projects.registry.register",
    "mpm.projects.status",
];

/// A method that takes no arguments, tolerating `null` and a stray object.
///
/// Why a local copy rather than a re-export of [`crate::daemon::rpc::core`]'s:
/// that one is `pub`, so this could import it — but importing a sibling FAMILY's
/// type would make slice 2's file a dependency of slice 5's for a six-line
/// struct, and the two slices are meant to be independently reviewable. The
/// consolidation belongs in `trusty-common` alongside `RpcRouter`, where every
/// daemon's no-argument methods can share one.
/// Test: `rpc_projects_list_answers_with_no_params`.
pub struct NoParams;

impl<'de> Deserialize<'de> for NoParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(NoParams)
    }
}

/// Mount every method in this module's table onto `router`.
///
/// Test: `rpc_router_registers_every_documented_method`.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let r = projects::register(router, state);
    let r = deliverables::register(r, state);
    let r = manager::register(r, state);
    let r = bus::register(r, state);
    let r = pairing::register(r, state);
    delegation::register(r, state)
}
