//! The `/api/secrets/*` handlers (#9066, DOC-74 §15.6).
//!
//! Why: each route is one bridged method; the handler's only jobs are to
//! decode the body into the console's own request, attach the verdict, and
//! relay the answer without echoing anything it was not meant to.
//! What: every forwarding handler reads the body under [`MAX_BODY_BYTES`],
//! decodes it with `deny_unknown_fields` (the serde error is dropped unread,
//! A7), judges the client from the [`AcceptedPeer`] the listener recorded at
//! accept, adds the `bridge` assertion, and makes exactly one call (A4). A
//! result is relayed as the server sent it — for `set`, the server's mask
//! text; an error passes only through the closed kind table.
//! Test: `each_route_dials_its_bridged_method_with_an_assertion`,
//! `malformed_bodies_never_echo_the_value`,
//! `upstream_errors_relay_only_the_kind`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{ConnectInfo, Request, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use trusty_secrets::api::bridge::method;
use trusty_secrets::api::{BRIDGE_FIELD, BackendId, SecretKey, SecretValue, VaultName};

use super::dial::DialError;
use super::errors::{Refusal, upstream_response};
use super::verdict::{AcceptedPeer, judge};
use super::{MAX_BODY_BYTES, SecretsBridge};

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
    let Some(dial) = bridge.dial() else {
        return Refusal::Unavailable.into_response();
    };
    let peer = request
        .extensions()
        .get::<ConnectInfo<AcceptedPeer>>()
        .map_or(AcceptedPeer::Unfound, |info| info.0);
    let Ok(bytes) = axum::body::to_bytes(request.into_body(), MAX_BODY_BYTES).await else {
        return Refusal::TooLarge.into_response();
    };
    // #9066 A7: the serde error can quote the value; it is dropped unread.
    let Ok(body) = serde_json::from_slice::<T>(&bytes) else {
        return Refusal::BadBody.into_response();
    };
    let Ok(Value::Object(mut params)) = serde_json::to_value(&body) else {
        return Refusal::BadBody.into_response();
    };
    drop(body);
    let assertion = judge(bridge.table(), peer);
    let Ok(assertion) = serde_json::to_value(assertion) else {
        return Refusal::BadBody.into_response();
    };
    params.insert(BRIDGE_FIELD.to_owned(), assertion);
    match dial.call(name, Value::Object(params)).await {
        Ok(result) => Json(result).into_response(),
        Err(DialError::Unavailable) => Refusal::Unavailable.into_response(),
        Err(DialError::Transport) => Refusal::Transport.into_response(),
        Err(DialError::Rpc { kind }) => upstream_response(kind.as_deref()),
    }
}
