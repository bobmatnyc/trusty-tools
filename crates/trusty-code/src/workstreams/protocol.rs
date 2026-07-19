//! `workstream.activate` / `workstream.deactivate` JSON-RPC method handlers
//! (DOC-48 §5.1/§6, issue #3294).
//!
//! # Spec References
//!
//! - [`SPEC-WS-05~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-05~draft)
//! - [`SPEC-WS-06~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-06~draft)
//!
//! Why: mirrors `crate::session::protocol`'s role for `session.*` — the RPC
//! surface the daemon exposes over `POST /rpc` (and, via
//! `crate::serve::rest::workstreams`, the REST twin), thin handlers that
//! parse `params` and delegate to [`super::activation`]'s business logic, so
//! the activation-lock rules live in exactly one place regardless of which
//! transport a caller uses.
//!
//! **Scope note (issue #3294):** this file registers ONLY `activate` and
//! `deactivate` — `workstream.create`/`get`/`list`/`close` are issue #3295's
//! surface (a sibling ticket landing concurrently; see that ticket for the
//! rest of `workstream.*`). Both tickets' `register` functions are additive
//! (`Router::register` on a distinct method name each), so whichever merges
//! first, the other rebases cleanly onto the same `router.register(...)`
//! call in `crate::serve::build_router` rather than colliding.
//!
//! What: [`register`] wires both methods onto a [`Router`], sharing one
//! [`SharedWorkstreamStore`](super::activation::SharedWorkstreamStore).
//! [`activate`] maps [`ActivationError::NotFound`] to `-32002 not_found`,
//! [`ActivationError::ActiveConflict`] to the new `-32008 active_conflict`
//! (`RpcError::active_conflict`, carrying `active_id`), and
//! [`ActivationError::Store`] to `-32603 internal_error`. [`deactivate`]
//! never fails on a valid UUID — see [`super::activation::deactivate`]'s
//! idempotent-no-op contract.
//! Test: `protocol_tests`.

use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::jsonrpc::{ConnectionContext, Router, RpcError};

use super::activation::{self, ActivationError, SharedWorkstreamStore};
use super::model::WorkstreamId;

/// Register `workstream.activate`/`workstream.deactivate` onto `router`, both
/// sharing `store`.
///
/// Why: the one place that lists this ticket's slice of the `workstream.*`
/// surface — mirrors `crate::session::protocol::register`'s role.
/// What: clones `store` once per method (cheap — `Arc`) into a small adapter
/// closure that forwards to the corresponding free function below.
/// Test: `protocol_tests::register_wires_activate_and_deactivate`.
pub fn register(router: &mut Router, store: SharedWorkstreamStore) {
    let s = store.clone();
    router.register(
        "workstream.activate",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { activate(&s, params, ctx).await }
        },
    );

    let s = store;
    router.register(
        "workstream.deactivate",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { deactivate(&s, params, ctx).await }
        },
    );
}

/// `params` shape for `workstream.activate` (DOC-48 §5.1).
#[derive(Deserialize)]
struct ActivateParams {
    id: String,
    #[serde(default)]
    force: bool,
}

/// `params` shape shared by `workstream.deactivate` (a bare workstream id).
#[derive(Deserialize)]
struct WorkstreamIdParams {
    id: String,
}

/// Parse a wire `id` string into a [`WorkstreamId`], mapping a malformed
/// UUID onto `-32602 Invalid params` rather than a panic.
fn parse_id(raw: &str, method: &str) -> Result<WorkstreamId, RpcError> {
    Uuid::parse_str(raw)
        .map(WorkstreamId::from)
        .map_err(|e| RpcError::invalid_params(format!("{method}: invalid workstream id: {e}")))
}

/// `workstream.activate(id, force?) -> {active_id, prior_id?}` (DOC-48
/// §5.1/§6.1).
///
/// Why: the RPC entry point for the activation-lock exclusivity model.
/// What: parses `params`, delegates to [`activation::activate`], and maps
/// its typed error onto the JSON-RPC error taxonomy — see the module docs.
/// Test: `protocol_tests::activate_succeeds_with_no_prior_active`,
/// `protocol_tests::activate_without_force_maps_to_active_conflict`,
/// `protocol_tests::activate_with_force_switches`,
/// `protocol_tests::activate_unknown_id_maps_to_not_found`,
/// `protocol_tests::activate_malformed_id_maps_to_invalid_params`.
async fn activate(
    store: &SharedWorkstreamStore,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: ActivateParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("workstream.activate: {e}")))?;
    let id = parse_id(&p.id, "workstream.activate")?;

    match activation::activate(store, id, p.force).await {
        Ok(outcome) => Ok(json!({
            "active_id": outcome.active_id,
            "prior_id": outcome.prior_id,
        })),
        Err(ActivationError::NotFound(id)) => {
            Err(RpcError::not_found(format!("workstream not found: {id}")))
        }
        Err(ActivationError::ActiveConflict(active_id)) => {
            Err(RpcError::active_conflict(active_id))
        }
        Err(ActivationError::Store(e)) => Err(RpcError::internal(e.to_string())),
    }
}

/// `workstream.deactivate(id) -> {}` (DOC-48 §5.1/§4.3).
///
/// Why: the RPC entry point for clearing the active pointer.
/// What: idempotent — see [`activation::deactivate`]'s docs; the only
/// failure mode is a lower-level store error, mapped to `-32603
/// internal_error`.
/// Test: `protocol_tests::deactivate_active_clears_pointer`,
/// `protocol_tests::deactivate_idle_is_idempotent_noop`.
async fn deactivate(
    store: &SharedWorkstreamStore,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: WorkstreamIdParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("workstream.deactivate: {e}")))?;
    let id = parse_id(&p.id, "workstream.deactivate")?;

    activation::deactivate(store, id)
        .await
        .map_err(|e| match e {
            ActivationError::Store(e) => RpcError::internal(e.to_string()),
            // `deactivate` never constructs `NotFound`/`ActiveConflict` itself
            // (see its docs) — reachable only if a future change to
            // `activation::deactivate` starts returning them without updating
            // this mapping, so fail loudly rather than mis-mapping silently.
            other => RpcError::internal(format!("unexpected activation error: {other}")),
        })?;
    Ok(json!({}))
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod protocol_tests;
