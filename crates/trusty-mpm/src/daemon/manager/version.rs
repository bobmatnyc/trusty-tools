//! `GET /api/v1/manager/version` — the manager surface's self-describing
//! capabilities stub (WI-1, #2578).
//!
//! Why: DOC-36 §4's local-testability bar requires the `/api/v1/manager/*`
//! surface to be reachable and meaningful via `curl` "from day one", before any
//! channel/bot token exists and before the later-phase endpoints (digest, chat,
//! route-task, escalations) are implemented. A version/capabilities endpoint is
//! the smallest thing that makes the scaffold observably alive: it names the
//! manager API version, the delivery phase, the full DOC-36 §3.2 verb set with a
//! per-endpoint `available` flag (so the roadmap is honest — implemented vs.
//! planned), and the portfolio palace status (WI-5, #2582) so provisioning is
//! curl-observable. This single endpoint therefore ties all three phase-1a work
//! items into one hermetic, token-free smoke surface.
//! What: [`ManagerVersionResponse`] + the [`manager_version_route`] handler,
//! which reads the daemon-owned [`crate::daemon::manager::ManagerState`] for
//! live palace availability and returns a static-plus-live capabilities snapshot.
//! Test: `manager_version_route_reports_capabilities` in `tests/manager_routes.rs`.

use std::sync::Arc;

use axum::{Json, extract::State, response::IntoResponse};
use serde::Serialize;

use crate::daemon::state::DaemonState;

/// The manager HTTP API version this daemon serves.
///
/// Why: pins a stable contract version for the `/api/v1/manager/*` surface,
/// independent of the crate version, so a channel/CLI client can feature-detect
/// against a value that only changes when the manager wire contract does.
/// What: the manager-surface semantic version string.
/// Test: `manager_version_route_reports_capabilities`.
const MANAGER_API_VERSION: &str = "0.1.0";

/// The DOC-36 delivery phase this build implements.
///
/// Why: DOC-36 §6 phases the manager surface (1 read-only chat → 2 routing → 3
/// proactive → 4 channels); surfacing the phase lets an operator confirm which
/// slice is live. Phase 2 (epic #2109) adds `route-task` + the `act`
/// proposal-and-confirm flow on top of the phase-1 read-only surface.
/// What: the integer phase number.
/// Test: `manager_version_route_reports_capabilities`.
const MANAGER_PHASE: u8 = 2;

/// One entry in the manager surface's advertised verb set.
///
/// Why: makes the DOC-36 §3.2 roadmap self-describing over HTTP — a consumer
/// reads which endpoints exist NOW (`available = true`) versus which are planned
/// for a later phase (`available = false`), instead of discovering a 404 by
/// trial. Later WIs flip their own flag to `true` as they land.
/// What: the HTTP `method`, the `path`, and whether it is live in this build.
/// Test: `manager_version_route_reports_capabilities`.
#[derive(Debug, Clone, Serialize)]
pub struct ManagerEndpoint {
    /// HTTP method (`GET`/`POST`).
    pub method: &'static str,
    /// Route path under the manager namespace.
    pub path: &'static str,
    /// Whether the endpoint is implemented and live in this build.
    pub available: bool,
}

/// Live status of the portfolio manager palace (WI-5, #2582).
///
/// Why: DOC-36 §4's degrade bar means the palace may be absent (feature not
/// compiled) or unavailable (open failed) while the manager surface still works;
/// surfacing that here makes provisioning success/failure curl-observable without
/// a separate diagnostic endpoint.
/// What: the stable palace id, an `available` flag, and an optional `reason`
/// string when unavailable.
/// Test: `manager_version_route_reports_capabilities`.
#[derive(Debug, Clone, Serialize)]
pub struct ManagerPalaceStatus {
    /// The stable portfolio palace id.
    pub id: String,
    /// Whether the palace is provisioned and usable.
    pub available: bool,
    /// Why the palace is unavailable, when it is (`None` when available).
    pub reason: Option<String>,
}

/// Response body for `GET /api/v1/manager/version`.
///
/// Why: the single self-describing snapshot of the manager surface — versions,
/// phase, verb-set roadmap, and palace status — that the DOC-36 §4 local-test
/// bar exercises first.
/// What: the manager API version, the crate version, the delivery phase, the
/// advertised endpoint set, and the portfolio palace status.
/// Test: `manager_version_route_reports_capabilities`.
#[derive(Debug, Clone, Serialize)]
pub struct ManagerVersionResponse {
    /// The manager HTTP API contract version ([`MANAGER_API_VERSION`]).
    pub manager_api_version: &'static str,
    /// The `trusty-mpm` crate version serving this surface.
    pub crate_version: &'static str,
    /// The DOC-36 delivery phase implemented ([`MANAGER_PHASE`]).
    pub phase: u8,
    /// The advertised verb set (implemented + planned).
    pub endpoints: Vec<ManagerEndpoint>,
    /// Live portfolio palace status.
    pub palace: ManagerPalaceStatus,
}

/// The advertised DOC-36 §3.2 verb set for the manager surface.
///
/// Why: centralises the roadmap so `version` stays the single source of truth for
/// "what does `/api/v1/manager/*` offer". Phase-1 ships `version` + `status` +
/// `digest` + `chat`; `route-task` and `escalations` are advertised as planned so
/// the surface documents its own trajectory without pretending to serve endpoints
/// later WIs own.
/// What: the five §3.2 endpoints plus this `version` stub, each tagged with its
/// current availability in this build.
/// Test: `manager_version_route_reports_capabilities`.
fn advertised_endpoints() -> Vec<ManagerEndpoint> {
    vec![
        ManagerEndpoint {
            method: "GET",
            path: "/api/v1/manager/version",
            available: true,
        },
        ManagerEndpoint {
            method: "GET",
            path: "/api/v1/manager/status",
            available: true,
        },
        ManagerEndpoint {
            method: "GET",
            path: "/api/v1/manager/digest",
            available: true,
        },
        ManagerEndpoint {
            method: "POST",
            path: "/api/v1/manager/chat",
            available: true,
        },
        ManagerEndpoint {
            method: "POST",
            path: "/api/v1/manager/route-task",
            available: true,
        },
        ManagerEndpoint {
            method: "POST",
            path: "/api/v1/manager/act",
            available: true,
        },
        ManagerEndpoint {
            method: "GET",
            path: "/api/v1/manager/escalations",
            available: false,
        },
    ]
}

/// `GET /api/v1/manager/version` — capabilities + palace status snapshot.
///
/// Why: the curl-first smoke endpoint (DOC-36 §4) that proves the manager
/// scaffold is mounted and reports the live portfolio palace state (§2582)
/// without any LLM call, channel, or bot token.
/// What: reads the daemon-owned [`crate::daemon::manager::ManagerState`] palace
/// handle and returns a [`ManagerVersionResponse`] combining static version/phase
/// /verb-set data with the live palace availability. Read-only; never mutates.
/// Test: `manager_version_route_reports_capabilities` in `tests/manager_routes.rs`.
pub async fn manager_version_route(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let manager = state.manager_state();
    let palace = manager.palace();
    Json(ManagerVersionResponse {
        manager_api_version: MANAGER_API_VERSION,
        crate_version: env!("CARGO_PKG_VERSION"),
        phase: MANAGER_PHASE,
        endpoints: advertised_endpoints(),
        palace: ManagerPalaceStatus {
            id: palace.id().to_string(),
            available: palace.is_available(),
            reason: palace.unavailable_reason().map(str::to_string),
        },
    })
}
