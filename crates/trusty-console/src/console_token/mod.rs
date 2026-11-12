//! The console token: a credential on every TCP route (#11074, DOC-90 §4).
//!
//! Why: the loopback listener trusted any caller whose `Host` matched, the
//! MagicDNS name included, so on a cloud sled a sidecar in a shared network
//! namespace reached every route (audit gap 2). Owner ruling D1b makes the
//! console the single inbound surface, and it needs a token.
//! What: [`TokenGate::from_env`] loads `TRUSTY_CONSOLE_TOKEN_FILE` through
//! [`trusty_common::console_token`] and fails before any bind on a bad file.
//! [`guard`] mounts `GET`/`POST /login` and layers [`require_token`] over the
//! whole router, fallback included. A request passes with
//! `Authorization: Bearer <token>` (constant-time) or a live session cookie;
//! the gate then removes the `Authorization` header and the session cookie,
//! so request logging, handlers and the reverse proxy never see either. A
//! browser navigation without a session is sent to `/login`; anything else
//! gets a fixed `401` with `WWW-Authenticate: Bearer`. With the variable
//! unset, [`guard`] returns the router untouched.
//!
//! The listener's own checks run first: [`crate::tailnet_peer::guard_loopback`]
//! and the tailnet peer gate wrap the router this returns, so the token adds
//! to them and replaces neither. The console's Unix socket is exempt
//! (DOC-90 IN-4): it is a separate listener, never served this router, and its
//! peer-uid check is its gate.
//! Test: `console_token/tests.rs`.
//!
//! # Spec References
//! - [`SPEC-SLEDNET-04~draft`](docs/specs/DOC-90-cloud-sled-network-boundary.md#SPEC-SLEDNET-04~draft)

mod login;
mod session;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use trusty_common::console_token::ConsoleToken;

use session::SessionStore;

/// The browser login path; served by the gate itself, reachable unauthenticated.
pub const LOGIN_PATH: &str = "/login";

/// The session cookie's name. Its value is a server-minted id, never the token.
pub const SESSION_COOKIE: &str = "trusty_console_session";

/// How long a browser session lasts (DOC-90 TOK-5: 12 h).
pub const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);

/// The fixed body every refused request gets; it never says which check failed.
const UNAUTHORIZED_BODY: &str = r#"{"error":"unauthorized"}"#;

/// The loaded token plus the in-memory browser sessions minted from it.
///
/// Sessions live only in memory, so a restart (the rotation step, TOK-9) ends
/// every one of them.
pub struct TokenGate {
    token: ConsoleToken,
    sessions: SessionStore,
}

impl std::fmt::Debug for TokenGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenGate(<redacted>)")
    }
}

impl TokenGate {
    /// Load the gate from `TRUSTY_CONSOLE_TOKEN_FILE`, or `None` when unset.
    ///
    /// Why: DOC-90 TOK-3 — `run_serve` calls this before it binds, so a bad
    /// file stops the console instead of serving it open.
    /// What: [`trusty_common::console_token::load_from_env`]; its error names
    /// the path and the problem, never the content.
    /// Test: `run_serve_refuses_a_bad_token_file_before_binding_11074`.
    pub fn from_env() -> anyhow::Result<Option<Arc<Self>>> {
        Ok(trusty_common::console_token::load_from_env()?.map(|t| Arc::new(Self::new(t))))
    }

    /// A gate for `token` with the production session lifetime.
    pub fn new(token: ConsoleToken) -> Self {
        Self::with_session_ttl(token, SESSION_TTL)
    }

    /// A gate whose sessions last `ttl`; tests use a short one.
    pub fn with_session_ttl(token: ConsoleToken, ttl: Duration) -> Self {
        Self {
            token,
            sessions: SessionStore::new(ttl),
        }
    }

    /// Whether `headers` carry the token as a bearer or a live session cookie.
    ///
    /// Test: `right_bearer_is_served_on_every_probe_11074`,
    /// `login_cookie_opens_the_console_and_holds_no_token_11074`,
    /// `expired_session_is_refused_11074`.
    fn authorizes(&self, headers: &HeaderMap) -> bool {
        let bearer = headers
            .get_all(header::AUTHORIZATION)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(bearer_credential)
            .any(|presented| self.token.matches(presented));
        bearer || session_ids(headers).any(|id| self.sessions.is_live(id))
    }
}

/// Wrap `router` with the token gate, or return it untouched when `gate` is
/// `None`.
///
/// Why: one wrapper, so `run_serve` and the tests serve the same thing, and
/// the unset path stays byte-for-byte what it was (TOK-1).
/// What: merges `GET`/`POST` [`LOGIN_PATH`], then layers [`require_token`]
/// over every route and the fallback.
/// Test: `unset_token_leaves_the_router_unchanged_11074`,
/// `unauthenticated_request_is_401_with_token_file_set_11074`.
pub fn guard(router: Router, gate: Option<Arc<TokenGate>>) -> Router {
    let Some(gate) = gate else {
        return router;
    };
    Router::new()
        .route(LOGIN_PATH, get(login::login_form).post(login::login_submit))
        .with_state(Arc::clone(&gate))
        .merge(router)
        .layer(axum::middleware::from_fn_with_state(gate, require_token))
}

/// Admit a request carrying the token, strip the credential, or refuse it.
///
/// Why: TOK-4/TOK-6/TOK-7 in one place, ahead of routing.
/// What: decides on the original headers, then removes `Authorization` and
/// the session cookie from the request whatever the verdict. [`LOGIN_PATH`]
/// passes unauthenticated; its own handler checks the token.
/// Test: `missing_and_wrong_token_get_the_same_401_11074`,
/// `browser_navigation_redirects_to_login_11074`,
/// `credentials_never_reach_a_handler_11074`.
async fn require_token(
    State(gate): State<Arc<TokenGate>>,
    mut req: Request,
    next: Next,
) -> Response {
    let authorized = gate.authorizes(req.headers());
    // #11074: nothing past this point sees the token or the session id.
    strip_credentials(req.headers_mut());
    if authorized || req.uri().path() == LOGIN_PATH {
        return next.run(req).await;
    }
    tracing::warn!(
        method = %req.method(),
        path = req.uri().path(),
        "console token gate refused a request without a valid token"
    );
    if is_browser_navigation(&req) {
        return login::redirect_to(LOGIN_PATH, None);
    }
    unauthorized()
}

/// The fixed `401` with `WWW-Authenticate: Bearer` (TOK-6).
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [
            (header::WWW_AUTHENTICATE, "Bearer"),
            (header::CONTENT_TYPE, "application/json"),
        ],
        UNAUTHORIZED_BODY,
    )
        .into_response()
}

/// A top-level page load: `GET`/`HEAD` that accepts HTML.
fn is_browser_navigation(req: &Request) -> bool {
    (req.method() == Method::GET || req.method() == Method::HEAD)
        && req
            .headers()
            .get_all(header::ACCEPT)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|v| v.contains("text/html"))
}

/// The credential of a `Bearer` `Authorization` value; the scheme is
/// case-insensitive (RFC 9110 §11.1).
fn bearer_credential(value: &str) -> Option<&str> {
    let (scheme, credential) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| credential.trim())
}

/// Every value of [`SESSION_COOKIE`] across the `Cookie` headers.
fn session_ids(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .filter(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value)
}

/// Remove `Authorization` and the session cookie, keeping any other cookie.
///
/// Why: TOK-7 — the reverse proxy forwards request headers upstream and
/// request logging may record them, so the credential leaves the request at
/// the gate. With the token on, `Authorization` on the console's TCP surface
/// is the console's own, so it is dropped whole.
/// Test: `credentials_never_reach_a_handler_11074`.
fn strip_credentials(headers: &mut HeaderMap) {
    headers.remove(header::AUTHORIZATION);
    let kept: Vec<String> = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .filter(|pair| pair.split_once('=').map(|(n, _)| n) != Some(SESSION_COOKIE))
        .map(str::to_owned)
        .collect();
    headers.remove(header::COOKIE);
    if kept.is_empty() {
        return;
    }
    if let Ok(value) = HeaderValue::from_str(&kept.join("; ")) {
        headers.insert(header::COOKIE, value);
    }
}

#[cfg(all(test, unix))]
mod tests;
