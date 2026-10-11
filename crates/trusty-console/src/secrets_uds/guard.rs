//! The secrets routes' request guard (#9066, DOC-74 §15.6 hardening, A6).
//!
//! Why: a secrets route changes credentials, so only the console's own page,
//! loaded from the console's own loopback address, may call it. The console
//! elsewhere answers any origin with permissive CORS; these routes do not.
//! What: [`check`], one pure decision per request, in this order: a tailnet
//! request; an unknown own authority; `Host` and `Origin` (an absent
//! `Origin` is refused on every method, Architect Q2); `OPTIONS`; any method
//! but `POST` (405, Architect Q3); the CSRF header (all but `session`); a
//! declared body over the cap. [`guard`] is the middleware. A refusal is one
//! fixed `warn!` naming the route and reason, and writes no audit (Q4).
//! Test: `origin_absent_or_foreign_is_refused_on_every_method`,
//! `host_not_this_listener_is_refused`, `csrf_must_match_this_launch`,
//! `tailnet_requests_are_refused_before_any_lookup`,
//! `unknown_own_origin_refuses_every_route`.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::errors::Refusal;
use super::{CSRF_HEADER, MAX_BODY_BYTES, SecretsBridge, TailnetRequest};
use crate::tailnet_peer::check_target;

/// The one path that needs no CSRF token: it is where the page gets it.
pub const SESSION_PATH: &str = "/api/secrets/session";

/// Decide whether `request` may reach a secrets handler.
///
/// What: see the module docs; `Ok(())` admits it.
/// Test: the guard tests in `tests.rs`.
pub fn check(bridge: &SecretsBridge, request: &Request) -> Result<(), Refusal> {
    if request.extensions().get::<TailnetRequest>().is_some() {
        return Err(Refusal::Tailnet);
    }
    let Some(own) = bridge.authorities().filter(|own| !own.is_empty()) else {
        return Err(Refusal::OriginUnknown);
    };
    let headers = request.headers();
    // #9066 Architect Q2: an absent Origin is refused, whatever the method.
    if !headers.contains_key(header::ORIGIN) {
        return Err(Refusal::Origin);
    }
    if !own
        .iter()
        .any(|allowed| check_target(headers, request.uri(), allowed).is_ok())
    {
        return Err(Refusal::Origin);
    }
    if request.method() == Method::OPTIONS {
        return Err(Refusal::Preflight);
    }
    if request.method() != Method::POST {
        return Err(Refusal::MethodNotAllowed);
    }
    if request.uri().path() != SESSION_PATH {
        let sent = headers.get(CSRF_HEADER).and_then(|v| v.to_str().ok());
        let ok = match (bridge.token(), sent) {
            (Some(token), Some(sent)) => token.matches(sent),
            _ => false,
        };
        if !ok {
            return Err(Refusal::Csrf);
        }
    }
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|len| len > MAX_BODY_BYTES) {
        return Err(Refusal::TooLarge);
    }
    Ok(())
}

/// Middleware form of [`check`].
pub async fn guard(
    State(bridge): State<Arc<SecretsBridge>>,
    request: Request,
    next: Next,
) -> Response {
    match check(&bridge, &request) {
        Ok(()) => next.run(request).await,
        Err(refusal) => {
            // #9066 Q4: route and reason only — never a body, key or vault.
            tracing::warn!(
                route = request.uri().path(),
                reason = ?refusal,
                "secrets bridge refused a request"
            );
            refusal.into_response()
        }
    }
}
