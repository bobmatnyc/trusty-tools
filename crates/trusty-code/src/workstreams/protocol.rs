//! `workstream.*` JSON-RPC method handlers (issues #3294, #3295, epic #3292).
//!
//! # Spec References
//!
//! - [`SPEC-WS-05~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-05~draft)
//! - [`SPEC-WS-06~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-06~draft)
//! - [`SPEC-WS-07~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-07~draft)
//!
//! Why: mirrors `crate::session::protocol`'s role for `session.*` — the RPC
//! surface the daemon exposes over `POST /rpc` (and, via
//! `crate::serve::rest::workstreams`, the REST twin). #3293 (merged) built
//! the domain model and the flat-JSON [`super::store::WorkstreamStore`], but
//! left it unreachable from any transport. This file is where BOTH
//! concurrent follow-up tickets against that foundation land: #3294's
//! activation-lock exclusivity model (`activate`/`deactivate`, built on
//! [`super::activation`]) and #3295's CRUD/inspection surface
//! (`create`/`get`/`list`/`close`, built directly on the store) — six thin
//! handlers, one shared [`SharedWorkstreamStore`], one `register` call.
//!
//! **Scope note:** `activate`/`deactivate`'s business logic (conflict
//! detection, force-switch) lives entirely in [`super::activation`] — this
//! file only parses `params` and maps [`ActivationError`] onto the JSON-RPC
//! taxonomy. `create`/`get`/`list`/`close` have no activation-lock rules to
//! apply, so they call [`super::store::WorkstreamStore`] directly. The CLI
//! (#3296) and the SSE aggregation route (#3297) remain out of scope.
//!
//! What: [`register`] wires `workstream.create`, `workstream.get`,
//! `workstream.list`, `workstream.close`, `workstream.activate`,
//! `workstream.deactivate`, `workstream.rename` (Phase C, issue #3300) onto
//! a [`crate::jsonrpc::Router`], all sharing one [`SharedWorkstreamStore`].
//! Every verb that takes a wire `id` parses it via
//! [`parse_id`] (a manual `Uuid::parse_str` + `-32602 Invalid params` on
//! failure) rather than deserializing straight into a [`WorkstreamId`] —
//! chosen as the ONE id-parsing style for this file since #3294 landed it
//! first. [`WorkstreamView`] is the wire-shape adapter `get`/`list` share:
//! the domain [`Workstream`] has no `state` FIELD (§2.2 — it is computed,
//! never stored), so every read path builds this view by pairing a record
//! with the store's current `active_workstream_id` pointer via
//! [`Workstream::state`], exactly once, right before serializing.
//! Test: `protocol_tests`.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::jsonrpc::{ConnectionContext, Router, RpcError};

use super::activation::{self, ActivationError, SharedWorkstreamStore, publish_state_inferred};
use super::model::{Workstream, WorkstreamId, WorkstreamState};
use super::store::StoreError;

/// Register every `workstream.*` method onto `router`, all sharing `store`.
///
/// Why: the one place that lists the full `workstream.*` surface — mirrors
/// `crate::session::protocol::register`'s role for `session.*`.
/// What: clones `store` once per method (cheap — `Arc`) into a small adapter
/// closure that forwards to the corresponding free function below.
/// Test: `protocol_tests::register_wires_activate_and_deactivate`,
/// `protocol_tests::register_wires_create_get_list_close`.
pub fn register(router: &mut Router, store: SharedWorkstreamStore) {
    let s = store.clone();
    router.register(
        "workstream.create",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { create(&s, params, ctx).await }
        },
    );

    let s = store.clone();
    router.register(
        "workstream.get",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { get(&s, params, ctx).await }
        },
    );

    let s = store.clone();
    router.register(
        "workstream.list",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { list(&s, params, ctx).await }
        },
    );

    let s = store.clone();
    router.register(
        "workstream.close",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { close(&s, params, ctx).await }
        },
    );

    let s = store.clone();
    router.register(
        "workstream.activate",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { activate(&s, params, ctx).await }
        },
    );

    let s = store.clone();
    router.register(
        "workstream.deactivate",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { deactivate(&s, params, ctx).await }
        },
    );

    let s = store;
    router.register(
        "workstream.rename",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { rename(&s, params, ctx).await }
        },
    );
}

/// The wire shape for a workstream record — the domain [`Workstream`] plus
/// its computed [`WorkstreamState`] (§2.2: never a stored field), assembled
/// fresh for every response.
#[derive(Debug, serde::Serialize)]
struct WorkstreamView {
    id: WorkstreamId,
    name: String,
    state: WorkstreamState,
    session_ids: Vec<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    metadata: serde_json::Map<String, Value>,
}

impl WorkstreamView {
    /// Pair a stored record with the pointer that governs its state.
    fn new(ws: Workstream, active_workstream_id: Option<WorkstreamId>) -> Self {
        let state = ws.state(active_workstream_id);
        Self {
            id: ws.id,
            name: ws.name,
            state,
            session_ids: ws.session_ids,
            created_at: ws.created_at,
            updated_at: ws.updated_at,
            metadata: ws.metadata,
        }
    }
}

/// Resolve the EFFECTIVE workstream target for a freshly-minted session and
/// persist the binding (DOC-48 §4.1/§4.2, issue #3298).
///
/// Why: `session::protocol::create` and `task::protocol::task_run` (minting
/// a NEW session) both need this exact resolution — an explicit
/// `workstream_id` param (§4.1) takes precedence; otherwise the daemon's
/// currently active workstream is the ambient default target (§4.2); if
/// neither applies, the session stays projectless (valid, never an error).
/// Centralising it here (rather than duplicating in both `protocol` modules)
/// is the same "one place, both callers" precedent
/// `crate::binding::ProjectBinding::resolve` already set for project
/// binding.
/// What: `explicit` is the caller's raw wire `workstream_id` string, if any.
/// `Ok(None)` — no explicit param and no active workstream. `Ok(Some(id))` —
/// after `WorkstreamStore::bind_session` has persisted the append.
/// `Err(-32602 invalid_params)` — `explicit` is not a valid UUID.
/// `Err(-32002 not_found)` — `explicit` names no workstream.
/// `Err(-32003 invalid_argument)` — the target (explicit OR ambient) is
/// closed (§4.1: "new sessions may not bind to it"). `StoreError::AlreadyBound`
/// maps to `-32603 internal` — unreachable in practice, since `session_id`
/// here is always a just-minted, globally-unique id that cannot already
/// appear in any workstream's `session_ids`.
/// Test: `protocol_tests::resolve_and_bind_session_explicit_wins_over_active`,
/// `protocol_tests::resolve_and_bind_session_falls_back_to_active`,
/// `protocol_tests::resolve_and_bind_session_none_when_nothing_active`,
/// `protocol_tests::resolve_and_bind_session_rejects_closed_explicit_target`,
/// `protocol_tests::resolve_and_bind_session_rejects_unknown_explicit_id`.
pub async fn resolve_and_bind_session(
    store: &SharedWorkstreamStore,
    explicit: Option<&str>,
    session_id: &str,
    method: &str,
) -> Result<Option<WorkstreamId>, RpcError> {
    let mut store = store.lock().await;
    let target = match explicit {
        Some(raw) => Some(parse_id(raw, method)?),
        None => store.active_workstream_id().await.map_err(map_store_err)?,
    };
    let Some(id) = target else {
        return Ok(None);
    };
    store
        .bind_session(id, session_id)
        .await
        .map_err(|e| match e {
            super::store::BindError::NotFound(id) => {
                RpcError::not_found(format!("workstream not found: {id}"))
            }
            super::store::BindError::Closed(id) => RpcError::invalid_argument(format!(
                "workstream {id} is closed; new sessions may not bind to it"
            )),
            super::store::BindError::AlreadyBound(sid) => RpcError::internal(format!(
                "unexpected: freshly-minted session `{sid}` already bound to a workstream"
            )),
            super::store::BindError::Store(e) => map_store_err(e),
        })?;
    Ok(Some(id))
}

/// Enforce DOC-48 §4.1's binding immutability on a REUSED session (AC-1.3,
/// issue #3298): "if a session with an existing workstream binding receives
/// a task with a different workstream, the task is rejected."
///
/// Why: `task::protocol::task_run`'s `session_id` ("sessionful") path is the
/// one call site that can present a session already carrying a persisted
/// `workstream_id` alongside a fresh, possibly-conflicting `workstream_id`
/// param — mirrors the existing `project` mismatch guard right next to it
/// (`task::protocol::task_run`'s own docs) at the exact same layer.
/// What: `explicit: None` never rejects — ambient default-targeting (§4.2)
/// applies only to a session's FIRST binding at creation, never re-applied
/// on reuse. `explicit: Some(raw)` — parses `raw`; `Ok(())` if it restates
/// `existing` exactly (including `existing: None` restated by... it cannot
/// restate `None`, so any `Some` explicit id against an unbound session is
/// ALSO a mismatch, per the same authoritative-binding rule
/// `task::protocol::task_run`'s `project` check already applies); otherwise
/// `Err(-32003 invalid_argument)` naming both ids.
/// Test: `protocol_tests::check_workstream_immutable_allows_none`,
/// `protocol_tests::check_workstream_immutable_allows_restating_same_id`,
/// `protocol_tests::check_workstream_immutable_rejects_mismatch`,
/// `protocol_tests::check_workstream_immutable_rejects_binding_an_unbound_session`.
pub fn check_workstream_immutable(
    existing: Option<WorkstreamId>,
    explicit: Option<&str>,
    method: &str,
) -> Result<(), RpcError> {
    let Some(raw) = explicit else {
        return Ok(());
    };
    let requested = parse_id(raw, method)?;
    if existing == Some(requested) {
        return Ok(());
    }
    Err(RpcError::invalid_argument(format!(
        "{method}: workstream `{requested}` does not match this session's existing binding \
         `{}` — a session's workstream binding is immutable once set (DOC-48 §4.1)",
        existing
            .map(|id| id.to_string())
            .unwrap_or_else(|| "<unbound>".to_string()),
    )))
}

/// Deserialise `params` into `T`, mapping a failure onto `-32602 Invalid
/// params` with the method name for context (mirrors
/// `crate::session::protocol::parse`).
fn parse<T: DeserializeOwned>(params: Value, method: &str) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|e| RpcError::invalid_params(format!("{method}: {e}")))
}

/// Parse a wire `id` string into a [`WorkstreamId`], mapping a malformed
/// UUID onto `-32602 Invalid params` rather than a panic (issue #3294's
/// id-parsing style, applied uniformly to every verb in this file — see
/// module docs).
fn parse_id(raw: &str, method: &str) -> Result<WorkstreamId, RpcError> {
    Uuid::parse_str(raw)
        .map(WorkstreamId::from)
        .map_err(|e| RpcError::invalid_params(format!("{method}: invalid workstream id: {e}")))
}

/// Map a [`StoreError`] onto the JSON-RPC error taxonomy (vision spec §13.2)
/// for the `create`/`get`/`list`/`close` verbs (the `activate`/`deactivate`
/// verbs instead map [`ActivationError`], which wraps `StoreError` alongside
/// its own `NotFound`/`ActiveConflict` variants).
///
/// Why: the one place `NotFound` becomes the spec's general `-32002
/// not_found` (workstreams have no session-specific error code) and every
/// other variant (I/O, serialization) becomes `-32603 internal` — a
/// caller-triggerable "id doesn't exist" must never be indistinguishable
/// from a daemon-side storage fault.
/// Test: `protocol_tests::get_unknown_id_maps_to_not_found`.
fn map_store_err(err: StoreError) -> RpcError {
    match err {
        StoreError::NotFound(id) => RpcError::not_found(format!("workstream not found: {id}")),
        StoreError::Io(_) | StoreError::Serialize(_) => RpcError::internal(err.to_string()),
    }
}

/// `params` shape for `workstream.create`.
#[derive(Deserialize)]
struct CreateParams {
    #[serde(default)]
    name: Option<String>,
}

/// `params` shape shared by every verb that only needs a workstream id
/// (`get`, `close`, `deactivate`).
#[derive(Deserialize)]
struct WorkstreamIdParams {
    id: String,
}

/// `params` shape for `workstream.list`.
#[derive(Deserialize, Default)]
struct ListParams {
    #[serde(default)]
    include_closed: bool,
}

/// `params` shape for `workstream.activate` (DOC-48 §5.1).
#[derive(Deserialize)]
struct ActivateParams {
    id: String,
    #[serde(default)]
    force: bool,
}

/// `params` shape for `workstream.rename` (DOC-48 §5.1, Phase C).
#[derive(Deserialize)]
struct RenameParams {
    id: String,
    name: String,
}

/// `workstream.create{name?} -> {id: UUID}` (DOC-48 §5.1, AC-2.1).
///
/// Why: mints a brand-new daemon-owned workstream. No `project_id`
/// parameter — the daemon's own `ProjectBinding` is implicit (§2.3): the
/// store this handler shares is already scoped to it via the file the daemon
/// booted with.
/// What: `name` defaults to an empty string when omitted (§5.1: "Name is
/// initially empty"). Returns `{"id": <new id>}`.
/// Test: `protocol_tests::create_returns_new_id`,
/// `protocol_tests::create_without_name_defaults_to_empty`.
async fn create(
    store: &SharedWorkstreamStore,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: CreateParams = parse(params, "workstream.create")?;
    let mut store = store.lock().await;
    let id = store
        .create(p.name.unwrap_or_default())
        .await
        .map_err(map_store_err)?;
    Ok(json!({ "id": id }))
}

/// `workstream.get{id} -> Workstream` (DOC-48 §5.1, AC-2.1).
///
/// Why: point lookup for one workstream's current (inferred) state.
/// What: `-32002 not_found` if `id` is unknown; otherwise the full
/// [`WorkstreamView`] — id, name, computed `state`, `session_ids`,
/// timestamps, `metadata`.
/// Test: `protocol_tests::get_returns_workstream_with_state`,
/// `protocol_tests::get_unknown_id_maps_to_not_found`.
async fn get(
    store: &SharedWorkstreamStore,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: WorkstreamIdParams = parse(params, "workstream.get")?;
    let id = parse_id(&p.id, "workstream.get")?;
    let mut store = store.lock().await;
    let ws = store.get(id).await.map_err(map_store_err)?;
    let active_id = store.active_workstream_id().await.map_err(map_store_err)?;
    Ok(json!(WorkstreamView::new(ws, active_id)))
}

/// `workstream.list{include_closed?} -> {active_workstream_id, workstreams: [Workstream]}`
/// (DOC-48 §5.1, AC-2.1, AC-2.3).
///
/// Why: enumerates every workstream in the daemon's project, so a client can
/// build a switcher without a `get` per id.
/// What: default `include_closed = false` filters out records whose
/// [`Workstream::is_closed`] flag is set (§4.4: "optionally listable ...
/// for audit/historical query"); `true` includes them. Every returned record
/// is paired with the SAME `active_workstream_id` snapshot (read once, not
/// per record) so the response is internally consistent even if a concurrent
/// writer changes the pointer mid-list. `active_workstream_id` is always
/// present at the top level (`null` if none active, AC-2.3).
/// Test: `protocol_tests::list_returns_active_workstream_id_and_records`,
/// `protocol_tests::list_default_excludes_closed`,
/// `protocol_tests::list_include_closed_true_includes_closed`.
async fn list(
    store: &SharedWorkstreamStore,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: ListParams = if params.is_null() {
        ListParams::default()
    } else {
        parse(params, "workstream.list")?
    };
    let mut store = store.lock().await;
    let all = store.list().await.map_err(map_store_err)?;
    let active_id = store.active_workstream_id().await.map_err(map_store_err)?;
    let views: Vec<WorkstreamView> = all
        .into_iter()
        .filter(|ws| p.include_closed || !ws.is_closed())
        .map(|ws| WorkstreamView::new(ws, active_id))
        .collect();
    Ok(json!({
        "active_workstream_id": active_id,
        "workstreams": views,
    }))
}

/// `workstream.close{id} -> {}` (DOC-48 §4.4/§5.1, AC-2.1).
///
/// Why: irreversibly closes a workstream — rejects future session bindings
/// (enforced at the session-binding call site, #3298 scope, not here) and,
/// per §4.4, auto-deactivates it if it is currently the active one.
/// What: `-32002 not_found` if `id` is unknown; otherwise `{}` (matching the
/// spec's §5.1 signature exactly — not the updated record, since a client
/// that needs the post-close view already has `workstream.get`). Idempotent:
/// closing an already-closed workstream succeeds again (see
/// [`super::model::Workstream::mark_closed`]'s docs). (Issue #3297)
/// Unconditionally publishes `Event::WorkstreamStateInferred{state: "closed"}`
/// via [`publish_state_inferred`] — even a repeat close re-asserts the same
/// state to any freshly-subscribed `GET /workstreams/{id}/events` observer,
/// and re-publishing an unchanged state is harmless.
/// Test: `protocol_tests::close_succeeds_and_clears_active_pointer`,
/// `protocol_tests::close_unknown_id_maps_to_not_found`,
/// `protocol_tests::close_publishes_state_inferred`.
async fn close(
    store: &SharedWorkstreamStore,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: WorkstreamIdParams = parse(params, "workstream.close")?;
    let id = parse_id(&p.id, "workstream.close")?;
    let mut store = store.lock().await;
    store.close(id).await.map_err(map_store_err)?;
    publish_state_inferred(id, "closed", "closed");
    Ok(json!({}))
}

/// `workstream.activate(id, force?) -> {active_id, prior_id?}` (DOC-48
/// §5.1/§6.1).
///
/// Why: the RPC entry point for the activation-lock exclusivity model.
/// What: parses `params`, delegates to [`activation::activate`], and maps
/// its typed error onto the JSON-RPC error taxonomy — [`ActivationError::NotFound`]
/// to `-32002 not_found`, [`ActivationError::ActiveConflict`] to `-32008
/// active_conflict` (`RpcError::active_conflict`, carrying `active_id`), and
/// [`ActivationError::Store`] to `-32603 internal_error`.
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
    let p: ActivateParams = parse(params, "workstream.activate")?;
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
    let p: WorkstreamIdParams = parse(params, "workstream.deactivate")?;
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

/// `workstream.rename{id, name} -> Workstream` (DOC-48 §5.1, Phase C, issue
/// #3300).
///
/// Why: the RPC entry point for the GUI switcher's rename action — the one
/// verb DOC-48 §5.1 marked "future, Phase C" when Phase 1A shipped; this
/// issue is that phase.
/// What: `-32002 not_found` if `id` is unknown; otherwise the updated
/// [`WorkstreamView`] (matching `get`'s response shape, since a client that
/// just renamed a record typically wants the fresh view without a second
/// round-trip). An empty `name` is accepted — matching `workstream.create`'s
/// own "name defaults to empty" tolerance rather than inventing a new
/// validation rule this spec never states.
/// Test: `protocol_tests::rename_succeeds_and_returns_updated_view`,
/// `protocol_tests::rename_unknown_id_maps_to_not_found`.
async fn rename(
    store: &SharedWorkstreamStore,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: RenameParams = parse(params, "workstream.rename")?;
    let id = parse_id(&p.id, "workstream.rename")?;
    let mut store = store.lock().await;
    let ws = store.rename(id, p.name).await.map_err(map_store_err)?;
    let active_id = store.active_workstream_id().await.map_err(map_store_err)?;
    Ok(json!(WorkstreamView::new(ws, active_id)))
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod protocol_tests;
