//! `GET`/`PUT /api/agents/{name}/listeners` — the deprecated listener routes
//! (#7609 slice 5).
//!
//! Why: listeners and channels are one concept now, and `/api/agents/{name}/channels`
//! is where that concept lives. Removing the listener routes in the same
//! release would break every client mid-upgrade — the shipped UI and any script
//! an operator wrote — so they stay for ONE release, answering exactly what
//! they always answered while saying, in the response itself, that they are on
//! the way out. Slice 7 deletes this module.
//! What: both handlers forward into [`super::agent_channels`], which is the
//! surviving implementation — the channel view already composes the listener
//! view, so the `GET` is that view's own `listeners` member and is byte-identical
//! to the pre-merge body. Every answer, success or failure, carries
//! `Deprecation: true` and a `Link` naming the successor, and the first call of
//! each kind warns once per process.
//! Test: `crate::api::server::tests::global_channels::the_listeners_alias_answers_with_a_deprecation_header`.

use axum::{
    Json,
    extract::Path as AxumPath,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::Value;

/// The successor this alias points at, in `Link` header form.
const SUCCESSOR: &str = "</api/agents/{name}/channels>; rel=\"successor-version\"";

/// Say once per process that the listener routes are deprecated.
///
/// Why: same reasoning as `crate::channels::migrate::warn_global_listeners_deprecated`
/// — an operator who never read a release note should learn it from their own
/// logs, and a per-request warning would be a flood.
/// Test: `the_listeners_alias_answers_with_a_deprecation_header` exercises the
/// path; the once-ness is the `Once`'s own contract.
fn warn_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        tracing::warn!(
            route = "/api/agents/{name}/listeners",
            replacement = "/api/agents/{name}/channels",
            "the listener routes are deprecated and will be removed after this release (#7609)"
        );
    });
}

/// Attach the deprecation headers to whatever the forwarded call answered.
///
/// Why: a client that only ever sees an error from this route still has to
/// learn the route is going away, so the headers go on both arms rather than
/// only the success one.
fn deprecate(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert("deprecation", HeaderValue::from_static("true"));
    headers.insert("link", HeaderValue::from_static(SUCCESSOR));
    response
}

fn answer(result: Result<Value, (StatusCode, Json<Value>)>) -> Response {
    deprecate(match result {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err((status, body)) => (status, body).into_response(),
    })
}

/// `GET /api/agents/{name}/listeners` — deprecated; forwards to the channel view.
///
/// What: answers the channel view's own `listeners` member, which is what this
/// route returned before the merge. A channel view that will not load fails the
/// same way it fails on `/channels`.
/// Test: `the_listeners_alias_answers_with_a_deprecation_header`.
pub(super) async fn get_listeners_alias(AxumPath(name): AxumPath<String>) -> Response {
    warn_once();
    answer(
        super::agent_channels::read(&name)
            .await
            .map(|view| view["listeners"].clone()),
    )
}

/// `PUT /api/agents/{name}/listeners` — deprecated; forwards to the channel
/// module's listener write.
///
/// Why this is NOT behind the channel-write gate (#7609): the gate covers the
/// two routes that write a channel BINDING — a destination and the assistant it
/// wakes. This alias writes only the stage-two wake filter of a listener the
/// operator already declared globally, which is the surface the shipped UI
/// edits on a tokenless loopback daemon today. Gating it would break that UI in
/// the same release that deprecates the route, for a write that cannot
/// re-point a destination.
/// Test: `the_listeners_alias_answers_with_a_deprecation_header`.
pub(super) async fn put_listeners_alias(
    AxumPath(name): AxumPath<String>,
    Json(update): Json<super::agent_listeners::ListenerUpdate>,
) -> Response {
    warn_once();
    answer(super::agent_channels::write_listeners(&name, update).await)
}
