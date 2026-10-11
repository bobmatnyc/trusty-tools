//! The `/api/secrets/*` handlers (#9066, DOC-74 §15.6).
//!
//! Why: each route is one bridged method; the handler's only jobs are to
//! decode the body into the console's own request, attach the verdict, and
//! relay the answer without echoing anything it was not meant to.
//! What: every forwarding handler reads the body under [`MAX_BODY_BYTES`],
//! decodes it with `deny_unknown_fields` (the serde error is dropped unread,
//! A7), judges the client from the [`PeerLookup`] the listener started at
//! accept, adds the `bridge` assertion, and makes exactly one call (A4). A
//! result is relayed as the server sent it — for `set`, the server's mask
//! text; an error passes only through the closed kind table. #9067: the
//! `scopes` result must decode as project and owner scopes before it is
//! relayed; `projects` reads the tm registry and dials nothing; every
//! bridged call's project must be one the registry lists (PR B1).
//! Test: `each_route_dials_its_bridged_method_with_an_assertion`,
//! `malformed_bodies_never_echo_the_value`,
//! `upstream_errors_relay_only_the_kind`,
//! `scopes_route_never_relays_a_machine_scope`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{ConnectInfo, Request, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use trusty_secrets::api::bridge::method;
use trusty_secrets::api::methods::ScopesResponse;
use trusty_secrets::api::{BRIDGE_FIELD, BackendId, SecretKey, SecretValue, VaultName};

use super::dial::DialError;
use super::errors::{Refusal, upstream_response};
use super::projects::{read_registry, registry_lists};
use super::verdict::{PeerLookup, judge_in_time};
use super::{MAX_BODY_BYTES, SecretsBridge};

/// `POST /api/secrets/scopes` (#9067).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopesBody {
    project: String,
}

/// `POST /api/secrets/list`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListBody {
    project: String,
    vault: VaultName,
}

/// `POST /api/secrets/set`. The only body that carries a value.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetBody {
    project: String,
    vault: VaultName,
    key: SecretKey,
    value: SecretValue,
}

/// `POST /api/secrets/delete`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteBody {
    project: String,
    vault: VaultName,
    key: SecretKey,
}

/// `POST /api/secrets/copy`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CopyBody {
    project: String,
    from_backend: BackendId,
    to_backend: BackendId,
    #[serde(default)]
    keys: Vec<SecretKey>,
}

/// `POST /api/secrets/agents_may_use`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentsBody {
    project: String,
    vault: VaultName,
    key: SecretKey,
    allowed: bool,
}

/// `POST /api/secrets/session`: the per-launch CSRF token, for the page.
///
/// What: reached only past the guard's Host and Origin checks (A12).
/// Test: `session_returns_the_token_to_the_own_origin_only`.
pub async fn session(State(bridge): State<Arc<SecretsBridge>>) -> Response {
    match bridge.token() {
        Some(token) => Json(json!({ "csrf": token.to_hex() })).into_response(),
        None => Refusal::Unavailable.into_response(),
    }
}

/// `POST /api/secrets/projects`: the picker's projects from the tm registry.
///
/// Why: #9067, Architect ruling — the page picks a registered project,
/// never a typed path or the console's working directory.
/// What: reads the registry on the blocking pool; an absent or empty one
/// is `{"projects": []}`, one that cannot be read is
/// [`Refusal::RegistryUnavailable`]. Never dials the secrets socket.
/// Test: `projects_route_lists_the_registry_and_never_dials`,
/// `projects_route_answers_an_empty_list_for_an_absent_or_empty_registry`,
/// `projects_route_refuses_a_malformed_or_unknown_registry`.
pub async fn projects(State(bridge): State<Arc<SecretsBridge>>) -> Response {
    let Some(file) = bridge.registry().map(ToOwned::to_owned) else {
        return Refusal::RegistryUnavailable.into_response();
    };
    match tokio::task::spawn_blocking(move || read_registry(&file)).await {
        Ok(Ok(projects)) => Json(json!({ "projects": projects })).into_response(),
        _ => Refusal::RegistryUnavailable.into_response(),
    }
}

/// `POST /api/secrets/scopes` -> `secrets.bridge.scopes` (#9067).
///
/// What: the result is relayed only when it decodes as
/// [`ScopesResponse`], whose kinds are project and owner; anything else,
/// a machine scope included, is [`Refusal::UnknownUpstream`].
/// Test: `scopes_route_dials_secrets_bridge_scopes_with_an_assertion`,
/// `scopes_route_never_relays_a_machine_scope`.
pub async fn scopes(State(bridge): State<Arc<SecretsBridge>>, request: Request) -> Response {
    let result = match call::<ScopesBody>(&bridge, request, method::SCOPES).await {
        Ok(result) => result,
        Err(refused) => return refused,
    };
    match serde_json::from_value::<ScopesResponse>(result) {
        Ok(scopes) => Json(scopes).into_response(),
        Err(_) => Refusal::UnknownUpstream.into_response(),
    }
}

/// `POST /api/secrets/list` -> `secrets.bridge.list`.
pub async fn list(State(bridge): State<Arc<SecretsBridge>>, request: Request) -> Response {
    forward::<ListBody>(&bridge, request, method::LIST).await
}

/// `POST /api/secrets/set` -> `secrets.bridge.set`.
pub async fn set(State(bridge): State<Arc<SecretsBridge>>, request: Request) -> Response {
    forward::<SetBody>(&bridge, request, method::SET).await
}

/// `POST /api/secrets/delete` -> `secrets.bridge.delete`.
pub async fn delete(State(bridge): State<Arc<SecretsBridge>>, request: Request) -> Response {
    forward::<DeleteBody>(&bridge, request, method::DELETE).await
}

/// `POST /api/secrets/copy` -> `secrets.bridge.copy`.
pub async fn copy(State(bridge): State<Arc<SecretsBridge>>, request: Request) -> Response {
    forward::<CopyBody>(&bridge, request, method::COPY).await
}

/// `POST /api/secrets/agents_may_use` -> `secrets.bridge.set_agents_may_use`.
pub async fn agents_may_use(
    State(bridge): State<Arc<SecretsBridge>>,
    request: Request,
) -> Response {
    forward::<AgentsBody>(&bridge, request, method::SET_AGENTS_MAY_USE).await
}

/// `resolve` never runs through the console: a fixed 501.
pub async fn resolve() -> Response {
    Refusal::ResolveUnsupported.into_response()
}

/// Any other `/api/secrets/*` path: a fixed 404, never the generic proxy.
pub async fn unknown() -> Response {
    Refusal::UnknownRoute.into_response()
}

/// Decode, judge, call once, relay.
///
/// What: see the module docs.
/// Test: `each_route_dials_its_bridged_method_with_an_assertion`.
async fn forward<T>(bridge: &SecretsBridge, request: Request, name: &'static str) -> Response
where
    T: Serialize + DeserializeOwned,
{
    match call::<T>(bridge, request, name).await {
        Ok(result) => Json(result).into_response(),
        Err(refused) => refused,
    }
}

/// Decode, judge and call once: the server's result, or the response that
/// replaces it.
async fn call<T>(
    bridge: &SecretsBridge,
    request: Request,
    name: &'static str,
) -> Result<Value, Response>
where
    T: Serialize + DeserializeOwned,
{
    let Some(dial) = bridge.dial() else {
        return Err(Refusal::Unavailable.into_response());
    };
    // #9066 F2: a missing lookup is judged `unreadable` by `judge_in_time`.
    let lookup = request
        .extensions()
        .get::<ConnectInfo<PeerLookup>>()
        .map(|info| info.0.clone());
    let Ok(bytes) = axum::body::to_bytes(request.into_body(), MAX_BODY_BYTES).await else {
        return Err(Refusal::TooLarge.into_response());
    };
    // #9066 A7: the serde error can quote the value; it is dropped unread.
    let Ok(body) = serde_json::from_slice::<T>(&bytes) else {
        return Err(Refusal::BadBody.into_response());
    };
    let Ok(Value::Object(mut params)) = serde_json::to_value(&body) else {
        return Err(Refusal::BadBody.into_response());
    };
    drop(body);
    // #9067: an unlisted project, or an unreadable registry, never dials.
    let Some(Value::String(project)) = params.get("project") else {
        return Err(Refusal::BadBody.into_response());
    };
    require_registered(bridge, project.clone()).await?;
    // #9066 F1: the lookup and the ancestry walk run off the async threads.
    let assertion = judge_in_time(bridge.table(), lookup).await;
    let Ok(assertion) = serde_json::to_value(assertion) else {
        return Err(Refusal::BadBody.into_response());
    };
    params.insert(BRIDGE_FIELD.to_owned(), assertion);
    match dial.call(name, Value::Object(params)).await {
        Ok(result) => Ok(result),
        Err(DialError::Unavailable) => Err(Refusal::Unavailable.into_response()),
        Err(DialError::Transport) => Err(Refusal::Transport.into_response()),
        Err(DialError::Rpc { kind }) => Err(upstream_response(kind.as_deref())),
    }
}

/// Refuse a bridged call whose project the tm registry does not list.
///
/// Why: #9067 PR B1, Architect ruling — the secrets server acts on whatever
/// checkout the call names, so the console admits only registered ones.
/// What: reads the registry fresh on the blocking pool. A listed project is
/// `Ok`; an unlisted one is [`Refusal::BadBody`] (400). A registry with no
/// known location, or one that is missing, unreadable or malformed, and a
/// failed blocking task are all [`Refusal::RegistryUnavailable`] (503):
/// the check fails closed. The answer names no path and echoes no value.
/// Test: `an_unlisted_project_is_refused_on_every_route_without_a_dial`,
/// `an_unreadable_registry_refuses_every_route_without_a_dial`,
/// `a_listed_project_is_dialled_once_per_route`.
async fn require_registered(bridge: &SecretsBridge, project: String) -> Result<(), Response> {
    let Some(file) = bridge.registry().map(ToOwned::to_owned) else {
        return Err(Refusal::RegistryUnavailable.into_response());
    };
    match tokio::task::spawn_blocking(move || registry_lists(&file, &project)).await {
        Ok(Ok(true)) => Ok(()),
        Ok(Ok(false)) => Err(Refusal::BadBody.into_response()),
        Ok(Err(_)) | Err(_) => Err(Refusal::RegistryUnavailable.into_response()),
    }
}
