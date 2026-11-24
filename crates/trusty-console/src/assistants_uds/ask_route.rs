//! `POST /api/console/assistants/{id}/ask` and the guards on every assistants
//! route (#11748).
//!
//! Why: the console has no login, so its guards are the only protection on a
//! route that makes a model run. Three of them apply here, on top of the
//! router-wide ones: the routes refuse a request no gated listener admitted,
//! the POST needs a same-origin `Origin`, and each ask is logged with its peer.
//! What: [`router`] mounts the three assistants routes behind
//! [`require_gated_listener`], and the POST behind [`require_same_origin`] and
//! a body cap. [`ask_handler`] validates, dials the instance's running daemon
//! through [`ask_socket`], maps the outcome to a status, and logs one line.
//! Test: `tests/assistants_ask.rs`, `gate_tests.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path as AxumPath, Request, State};
use axum::http::{StatusCode, header::ORIGIN};
use axum::middleware::{Next, from_fn, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};

use super::ask::{AskFailure, AskLimits, ask_socket};
use super::routes::{
    blocking, error, instances_or_500, list_handler, turns_handler, unknown_instance,
};
use super::{AGENTS_APP_NAME, InstanceId, ReplyErrorKind, socket_path_in};
use crate::routes::origin_guard::{SelfOrigins, origin_is_loopback, origin_matches_self};
use crate::server::AppState;
use crate::tailnet_peer::AdmittedBy;

/// Largest question accepted, in bytes.
///
/// Why: a typed question is a few hundred bytes; 16 KiB leaves room for a
/// pasted paragraph while staying far under the daemon's 1 MiB line cap.
pub const MAX_QUESTION_BYTES: usize = 16 * 1024;

/// Asks one console runs at once; one more is `429` (#11748 review).
///
/// Why: each ask holds a connection, a daemon slot and up to [`super::ask::ASK_TIMEOUT`]
/// of a request; the daemon answers one at a time anyway, so a deeper queue
/// only piles up timeouts. Four leaves room for a few tabs and devices.
pub const MAX_CONCURRENT_ASKS: usize = 4;

/// Largest request body read; past it the ask is `413`.
/// Four times the question cap leaves room for JSON escaping.
pub const MAX_ASK_BODY_BYTES: usize = 4 * MAX_QUESTION_BYTES;

/// The three assistants routes, each refused on an ungated listener.
///
/// Why: #11748 item 5 — the assistants routes may be served only where the
/// loopback guard or the tailnet peer gate ran. The router-level guards are
/// per listener, so a router served any other way (a plain `axum::serve` on a
/// public address) would otherwise answer.
/// What: the two PR1 GETs and the POST, with [`require_gated_listener`] as a
/// route layer over all three; the POST also carries [`require_same_origin`]
/// and a [`MAX_ASK_BODY_BYTES`] body limit.
/// Test: `ungated_listener_cannot_reach_assistants` (`tests/assistants_ask.rs`).
pub(crate) fn router(self_origins: SelfOrigins) -> Router<AppState> {
    Router::new()
        .route("/api/console/assistants", get(list_handler))
        .route("/api/console/assistants/{id}/turns", get(turns_handler))
        .route(
            "/api/console/assistants/{id}/ask",
            post(ask_handler)
                .layer(DefaultBodyLimit::max(MAX_ASK_BODY_BYTES))
                .route_layer(from_fn_with_state(self_origins, require_same_origin)),
        )
        .route_layer(from_fn(require_gated_listener))
}

/// Refuse with `403` a request no gated listener admitted.
///
/// What: passes only when the request carries an [`AdmittedBy`]. Only the
/// loopback guard and the tailnet peer gate insert one, and no code outside
/// this crate can construct one; `guard_loopback_listener` refuses a
/// non-loopback socket (#11748 review).
/// Test: `ungated_listener_cannot_reach_assistants` (`tests/assistants_ask.rs`).
async fn require_gated_listener(req: Request, next: Next) -> Response {
    if req.extensions().get::<AdmittedBy>().is_none() {
        warn!(
            path = req.uri().path(),
            "assistants: refused a request no gated listener admitted"
        );
        return error(
            StatusCode::FORBIDDEN,
            "the assistants routes are served only on the loopback or tailnet-gated listener",
        );
    }
    next.run(req).await
}

/// Refuse with `403` a POST whose `Origin` is missing or not the console's own.
///
/// Why: #11748 item 6. The router-wide write guard admits a POST with no
/// `Origin` (for `curl` and `tm`); a model run from the browser page must
/// name its origin.
/// What: the `Origin` must be present and be loopback or one of the console's
/// bound self-origins (`routes/origin_guard.rs`). The per-listener guards
/// have already pinned it to this listener's own authority.
/// Test: `ask_with_a_foreign_origin_is_403`, `ask_with_no_origin_is_403`
/// (`tests/assistants_ask.rs`).
async fn require_same_origin(
    State(self_origins): State<SelfOrigins>,
    req: Request,
    next: Next,
) -> Response {
    let verdict = match req.headers().get(ORIGIN).map(|v| v.to_str()) {
        None => Err("missing"),
        Some(Err(_)) => Err("not UTF-8"),
        Some(Ok(o)) if origin_is_loopback(o) || origin_matches_self(o, &self_origins) => Ok(()),
        Some(Ok(_)) => Err("not this console"),
    };
    if let Err(reason) = verdict {
        warn!(
            reason,
            "assistants: refused an ask whose Origin is {reason}"
        );
        return error(
            StatusCode::FORBIDDEN,
            "an ask needs the console's own Origin header",
        );
    }
    next.run(req).await
}

/// The ask body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AskBody {
    question: String,
}

/// What the per-ask log line records; filled in as the ask proceeds.
struct AskLog {
    instance: Option<InstanceId>,
    question_bytes: Option<usize>,
    outcome: &'static str,
}

/// `POST /api/console/assistants/{id}/ask` — one question to a running instance.
///
/// What: `{"question":"…"}` in; `200 {"instance","answer"}` out. Refusals, in
/// order: `400` invalid id (before any filesystem or socket access); `413`
/// body over [`MAX_ASK_BODY_BYTES`] or question over [`MAX_QUESTION_BYTES`];
/// `400` malformed body or blank question; `404` an id `ta list` does not
/// name, with `available`; `503` with a `ta ask-daemon` hint when no daemon serves
/// the socket (the console never starts one); `504` on a connect or ask
/// timeout; `502` with a `kind` for a daemon error or a bad reply; `500` when
/// the root or data directory cannot be resolved. No body names a path. One
/// `info!` line per ask: instance, peer, question length, outcome, duration —
/// never the question or the answer.
///
/// #11748 review: the handler fails closed by itself — no [`AdmittedBy`] is
/// `403` before any parse or dial — and a full ask pool
/// ([`MAX_CONCURRENT_ASKS`]) is `429` `{"error":"assistants busy","kind":"busy"}`
/// before any dial. The body is read only once the request is admitted, and
/// before the permit is taken (#11754).
/// Test: `tests/assistants_ask.rs`, `gate_tests::the_handler_alone_refuses_an_unmarked_request`,
/// `gate_tests::the_handler_alone_never_polls_an_unmarked_body`.
pub(crate) async fn ask_handler(
    State(state): State<AppState>,
    AxumPath(raw_id): AxumPath<String>,
    admitted: Option<Extension<AdmittedBy>>,
    // #11754: the raw body, read only after the admission check below.
    body: Body,
) -> Response {
    let started = Instant::now();
    let mut log = AskLog {
        instance: None,
        question_bytes: None,
        outcome: "unknown",
    };
    let response = if admitted.is_some() {
        ask(&state, &raw_id, body, &mut log).await
    } else {
        // #11748 review: fail closed even if the route layer is ever lost.
        log.outcome = "ungated";
        error(
            StatusCode::FORBIDDEN,
            "the assistants routes are served only on the loopback or tailnet-gated listener",
        )
    };
    let (listener, peer_addr, login, node) = peer_fields(admitted.as_ref().map(|e| &e.0));
    // #11748 item 7: lengths and labels only — never the question or answer.
    info!(
        instance = log.instance.as_ref().map_or("<invalid>", InstanceId::as_str),
        listener,
        peer_addr = %peer_addr,
        peer_login = %login,
        peer_node = %node,
        question_bytes = log.question_bytes,
        outcome = log.outcome,
        status = response.status().as_u16(),
        duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "assistants: ask"
    );
    response
}

/// The ask itself; records its outcome in `log`.
async fn ask(state: &AppState, raw_id: &str, body: Body, log: &mut AskLog) -> Response {
    // #11748: validate before any filesystem or socket access.
    let id = match InstanceId::parse(raw_id) {
        Ok(id) => id,
        Err(reason) => {
            log.outcome = "invalid_id";
            return error(StatusCode::BAD_REQUEST, reason);
        }
    };
    log.instance = Some(id.clone());
    // #11754: read the body after admission and before the permit; a read
    // error is kept and refused by `parse_question`, never parsed as a question.
    let body = read_body(body).await;
    // #11748 review: bound concurrent asks; held until this fn returns,
    // timeout path included, and taken before any dial.
    let Ok(_permit) = Arc::clone(&state.assistants_ask_permits).try_acquire_owned() else {
        log.outcome = "busy";
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": "assistants busy", "kind": "busy" })),
        )
            .into_response();
    };
    let question = match parse_question(body) {
        Ok(q) => q,
        Err((status, outcome, message)) => {
            log.outcome = outcome;
            return error(status, message);
        }
    };
    log.question_bytes = Some(question.len());
    let root = match state.assistants_root() {
        Ok(root) => root,
        Err(e) => {
            log.outcome = "root_unresolved";
            return error(StatusCode::INTERNAL_SERVER_ERROR, e);
        }
    };
    let work_id = id.clone();
    let listed = blocking(move || {
        let available = instances_or_500(&root)?;
        if available.contains(&work_id) {
            Ok(())
        } else {
            Err(unknown_instance(&work_id, &available))
        }
    })
    .await;
    if let Err(resp) = listed {
        log.outcome = if resp.status() == StatusCode::NOT_FOUND {
            "unknown_id"
        } else {
            "list_failed"
        };
        return resp;
    }
    let data_dir = match state.assistants_data_dir() {
        Ok(dir) => dir,
        Err(e) => {
            warn!(error = %e, "assistants: cannot resolve the trusty-agents data directory");
            log.outcome = "data_dir_unresolved";
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot resolve the trusty-agents data directory",
            );
        }
    };
    let socket = socket_path_in(&data_dir, &id);
    match ask_socket(&socket, &question, state.assistants_ask_limits).await {
        Ok(answer) => {
            log.outcome = "answer";
            Json(json!({ "instance": id, "answer": answer })).into_response()
        }
        Err(failure) => {
            let (status, outcome, body) = failure_response(&id, &failure);
            log.outcome = outcome;
            (status, Json(body)).into_response()
        }
    }
}

/// A refusal before any dial: status, log outcome, message.
type Refusal = (StatusCode, &'static str, String);

/// Read `body`, at most [`MAX_ASK_BODY_BYTES`]: `413` past the cap, `400` on
/// any other read error.
///
/// Why: #11754 — the handler reads the body itself, after its admission check,
/// so the cap holds even without the route's `DefaultBodyLimit` layer.
/// Test: `gate_tests::an_admitted_body_over_the_cap_is_413`,
/// `gate_tests::an_admitted_body_read_error_is_400`.
async fn read_body(body: Body) -> Result<Bytes, Refusal> {
    axum::body::to_bytes(body, MAX_ASK_BODY_BYTES)
        .await
        .map_err(|e| {
            let inner = e.into_inner();
            let mut source: Option<&(dyn std::error::Error + 'static)> = Some(inner.as_ref());
            let mut over_cap = false;
            while let Some(err) = source {
                if err.is::<http_body_util::LengthLimitError>() {
                    over_cap = true;
                    break;
                }
                source = err.source();
            }
            let (status, outcome) = if over_cap {
                (StatusCode::PAYLOAD_TOO_LARGE, "body_too_large")
            } else {
                (StatusCode::BAD_REQUEST, "body_unreadable")
            };
            let message =
                format!("the request body is over {MAX_ASK_BODY_BYTES} bytes or unreadable");
            (status, outcome, message)
        })
}

/// The question in `body`, or why it is refused.
/// Test: `ask_with_an_oversize_question_is_413`, `ask_with_a_blank_question_is_400`
/// (`tests/assistants_ask.rs`).
fn parse_question(body: Result<Bytes, Refusal>) -> Result<String, Refusal> {
    let bytes = body?;
    let parsed: AskBody = serde_json::from_slice(&bytes).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            "bad_body",
            format!("the body must be {{\"question\": \"…\"}}: {e}"),
        )
    })?;
    if parsed.question.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "blank_question",
            "the question may not be blank".into(),
        ));
    }
    if parsed.question.len() > MAX_QUESTION_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "question_too_large",
            format!("a question is at most {MAX_QUESTION_BYTES} bytes"),
        ));
    }
    Ok(parsed.question)
}

/// Map an [`AskFailure`] to a status, a log outcome and a body naming no path.
///
/// What: no daemon → `503` with the [`super::daemon_hint`]; a timeout → `504`; every
/// other failure → `502` with its `kind`. Never `200` (#11748 item 8).
/// Test: the `ask_*` cases in `tests/assistants_ask.rs`.
fn failure_response(
    id: &InstanceId,
    failure: &AskFailure,
) -> (StatusCode, &'static str, serde_json::Value) {
    let instance = id.as_str();
    let (status, kind, message) = match failure {
        AskFailure::NotRunning => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "not_running",
                json!({
                    "error": "assistant not running",
                    "instance": instance,
                    "hint": super::daemon_hint(id),
                }),
            );
        }
        AskFailure::ConnectTimeout(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            "connect_timeout",
            "the assistant did not accept the connection in time",
        ),
        AskFailure::Timeout(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            "timeout",
            "the assistant did not answer in time",
        ),
        AskFailure::Unreachable(_) => (
            StatusCode::BAD_GATEWAY,
            "unreachable",
            "the assistant socket could not be dialled",
        ),
        AskFailure::Io { .. } => (
            StatusCode::BAD_GATEWAY,
            "io",
            "the connection to the assistant failed",
        ),
        AskFailure::Disconnected => (
            StatusCode::BAD_GATEWAY,
            "disconnected",
            "the assistant closed the connection without answering",
        ),
        AskFailure::ReplyTooLarge(_) => (
            StatusCode::BAD_GATEWAY,
            "reply_too_large",
            "the assistant's reply is too large",
        ),
        AskFailure::BadReply(_) => (
            StatusCode::BAD_GATEWAY,
            "bad_reply",
            "the assistant's reply is malformed",
        ),
        AskFailure::ProtocolMismatch { .. } => (
            StatusCode::BAD_GATEWAY,
            "protocol_mismatch",
            "the assistant speaks another protocol version",
        ),
        AskFailure::Daemon { kind, .. } => {
            let kind = match kind {
                ReplyErrorKind::Ask => "ask",
                ReplyErrorKind::BadRequest => "bad_request",
                ReplyErrorKind::ProtocolMismatch => "protocol_mismatch",
            };
            (
                StatusCode::BAD_GATEWAY,
                kind,
                "the assistant could not answer",
            )
        }
    };
    // The detail may name a path or echo daemon text: logs only, by kind.
    warn!(instance, kind, error = %failure, "assistants: ask failed");
    (
        status,
        kind,
        json!({ "error": message, "kind": kind, "instance": instance }),
    )
}

/// The log's peer fields: listener, address, login, node.
pub(super) fn peer_fields(admitted: Option<&AdmittedBy>) -> (&'static str, String, String, String) {
    const UNKNOWN: &str = "unknown";
    match admitted {
        Some(AdmittedBy::Loopback) => (
            "loopback",
            "loopback".into(),
            "loopback".into(),
            "loopback".into(),
        ),
        Some(AdmittedBy::Tailnet { peer, identity }) => {
            let login = identity
                .as_ref()
                .map_or(UNKNOWN.into(), |i| i.login.clone());
            let node = identity
                .as_ref()
                .and_then(|i| i.node_name.clone())
                .unwrap_or_else(|| UNKNOWN.into());
            ("tailnet", peer.to_string(), login, node)
        }
        None => ("none", UNKNOWN.into(), UNKNOWN.into(), UNKNOWN.into()),
    }
}

impl AppState {
    /// The trusty-agents data directory holding the ask sockets.
    ///
    /// What: the override, else `resolve_data_dir_no_create(AGENTS_APP_NAME)`
    /// — `ta`'s resolution (`TRUSTY_DATA_DIR_OVERRIDE`, else the platform data
    /// directory) without creating it, since only the daemon's bind may (#11667).
    ///
    /// # Errors
    ///
    /// When neither an override nor a home directory resolves.
    pub(crate) fn assistants_data_dir(&self) -> anyhow::Result<PathBuf> {
        match &self.assistants_data_dir {
            Some(dir) => Ok(dir.as_ref().clone()),
            None => trusty_common::resolve_data_dir_no_create(AGENTS_APP_NAME),
        }
    }

    /// Dial ask sockets under `dir` instead of the resolved data directory.
    ///
    /// Why: `TRUSTY_DATA_DIR_OVERRIDE` is process-global, and the console's
    /// test binaries run many routers at once.
    /// Test: `tests/assistants_ask.rs` sets it on every case.
    #[must_use]
    pub fn with_assistants_data_dir(mut self, dir: PathBuf) -> Self {
        self.assistants_data_dir = Some(Arc::new(dir));
        self
    }

    /// Allow `permits` concurrent asks instead of [`MAX_CONCURRENT_ASKS`].
    /// Test: `concurrent_asks_past_the_cap_are_429` (`tests/assistants_ask.rs`).
    #[must_use]
    pub fn with_assistants_max_concurrent_asks(mut self, permits: usize) -> Self {
        self.assistants_ask_permits = Arc::new(tokio::sync::Semaphore::new(permits));
        self
    }

    /// Bound asks by `limits` instead of [`AskLimits::default`].
    /// Test: `ask_to_a_slow_daemon_is_504` (`tests/assistants_ask.rs`).
    #[must_use]
    pub fn with_assistants_ask_limits(mut self, limits: AskLimits) -> Self {
        self.assistants_ask_limits = limits;
        self
    }
}
