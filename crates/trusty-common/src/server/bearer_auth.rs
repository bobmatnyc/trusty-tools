//! Router-wide bearer authentication for a loopback daemon's HTTP/SSE
//! surface (#5439).
//!
//! Why: [`super::origin_guard`] stops a browser page on a foreign origin from
//! issuing a WRITE and (with `same_origin_cors`) from READING a response, but
//! it is deliberately not authentication: it passes every request that sends
//! no `Origin` at all — `curl`, a reverse proxy, any local process. That is
//! the whole of #5439: a loopback bind plus an origin guard still leaves every
//! session, transcript, and mutation route open to any program running on the
//! machine. This layer adds the missing caller check, and lives beside the
//! origin guard rather than inside one daemon because the two are the same
//! defence stack applied to the same daemon family — a second copy in a leaf
//! crate is what the common-entry-point rule forbids.
//!
//! The credential itself is [`crate::daemon_token`]; read its honesty clause
//! before describing the boundary this establishes.
//!
//! What: [`DaemonAuth`] holds the expected token, the set of paths that stay
//! public, and the short-lived SSE ticket table. [`require_bearer`] is the
//! `axum::middleware::from_fn_with_state` guard: a valid
//! `Authorization: Bearer <token>` (or a valid single-use `?ticket=`) passes
//! and marks the request [`Authenticated`]; a public path passes UNMARKED;
//! everything else is `401` with an empty body.
//!
//! Two design points a reviewer should not have to reconstruct.
//!
//! **Tickets exist because `EventSource` cannot send a header.** A browser
//! opening `GET /sessions/{id}/events` has no way to attach `Authorization`,
//! and putting the durable token in the query string would write it into every
//! access log and tracing span. [`DaemonAuth::issue_ticket`] mints a
//! single-use value that expires in [`TICKET_TTL`], obtained over the
//! header-authenticated surface; a ticket in a log is spent and stale.
//!
//! **A public path passes unmarked, not authenticated.** A handler that serves
//! both audiences (`/health`) reads `Option<Extension<Authenticated>>` and
//! decides what to disclose, so the unauthenticated shape is a deliberate
//! choice at the handler rather than an exemption the middleware guesses at.
//!
//! Test: `bearer_auth_tests::*` drive a two-route router via
//! `tower::util::ServiceExt::oneshot` — missing, malformed, wrong, and correct
//! credentials; the public-path carve-out; ticket single-use and expiry.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{StatusCode, header::AUTHORIZATION};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::daemon_token::{credentials_match, mint_token};

/// Query-string parameter carrying a single-use SSE ticket.
pub const TICKET_QUERY_PARAM: &str = "ticket";

/// How long an issued SSE ticket stays redeemable.
///
/// Why: long enough for a browser to mint one and open the stream in the same
/// user gesture, short enough that a ticket captured from a log or a shoulder
/// is worthless. Redemption is single-use, so this bounds only the window
/// before the intended client uses it.
pub const TICKET_TTL: Duration = Duration::from_secs(30);

/// Marker inserted into a request's extensions when it presented a valid
/// credential.
///
/// Why: lets a public-path handler tell an anonymous caller from an
/// authenticated one without re-reading the header or knowing the token.
#[derive(Clone, Copy, Debug)]
pub struct Authenticated;

/// The daemon's expected credential, its public-path carve-out, and its
/// live SSE ticket table.
///
/// Why: `Clone` is cheap (one `Arc`) because axum requires middleware state to
/// be `Clone`, and every clone must see the SAME ticket table — a ticket
/// issued against one clone has to be redeemable against another.
#[derive(Clone)]
pub struct DaemonAuth(Arc<Inner>);

struct Inner {
    token: String,
    public_paths: HashSet<String>,
    tickets: Mutex<HashMap<String, Instant>>,
}

impl DaemonAuth {
    /// Guard every path with `token`, except the exact paths in
    /// `public_paths`.
    ///
    /// Why: one constructor rather than a builder, so the guarded set is
    /// fixed before any ticket can be issued — a builder step that rebuilt the
    /// state would silently discard tickets minted against the earlier value.
    /// Fail-closed by construction: a route merged in later is guarded without
    /// anyone remembering to add it, and a daemon opts a path OUT here, once.
    /// What: `public_paths` is matched by exact string equality against
    /// `req.uri().path()` — never a prefix match, so `/health` cannot be
    /// widened into `/healthz-secrets` by a future route name.
    /// Test: `bearer_auth_tests::public_path_passes_without_a_credential`,
    /// `bearer_auth_tests::public_path_match_is_exact_not_prefix`.
    pub fn new<I, S>(token: impl Into<String>, public_paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self(Arc::new(Inner {
            token: token.into(),
            public_paths: public_paths.into_iter().map(Into::into).collect(),
            tickets: Mutex::new(HashMap::new()),
        }))
    }

    /// Mint a single-use ticket redeemable for [`TICKET_TTL`].
    ///
    /// Why/What: see the module docs' `EventSource` note. Expired entries are
    /// swept here rather than on a timer, so the table cannot grow without an
    /// authenticated caller driving it.
    /// Test: `bearer_auth_tests::ticket_authenticates_once_then_is_spent`.
    pub fn issue_ticket(&self) -> String {
        let ticket = mint_token();
        if let Ok(mut tickets) = self.0.tickets.lock() {
            let now = Instant::now();
            tickets.retain(|_, issued| now.duration_since(*issued) < TICKET_TTL);
            tickets.insert(ticket.clone(), now);
        }
        ticket
    }

    /// Redeem `ticket`, consuming it. `false` for unknown, spent, or expired.
    fn consume_ticket(&self, ticket: &str) -> bool {
        let Ok(mut tickets) = self.0.tickets.lock() else {
            // A poisoned table means a panic already happened while holding it;
            // refusing the ticket is the fail-closed answer.
            return false;
        };
        match tickets.remove(ticket) {
            Some(issued) => Instant::now().duration_since(issued) < TICKET_TTL,
            None => false,
        }
    }

    /// Does `header` carry `Bearer <expected token>`?
    ///
    /// What: requires the `Bearer ` scheme prefix (ASCII-case-insensitive, per
    /// RFC 7235) and compares the remainder in constant time. A malformed
    /// header — no scheme, wrong scheme, non-ASCII bytes — is simply invalid,
    /// never a distinguishable error.
    fn header_is_valid(&self, header: Option<&axum::http::HeaderValue>) -> bool {
        let Some(value) = header.and_then(|h| h.to_str().ok()) else {
            return false;
        };
        let Some((scheme, presented)) = value.split_once(' ') else {
            return false;
        };
        scheme.eq_ignore_ascii_case("Bearer") && credentials_match(&self.0.token, presented.trim())
    }

    /// The ticket value in `query`, if any — a hand-rolled scan rather than a
    /// query-string crate, since exactly one parameter is read here.
    fn ticket_in_query(query: Option<&str>) -> Option<&str> {
        query?.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == TICKET_QUERY_PARAM).then_some(value)
        })
    }
}

/// Router-wide credential guard — apply with `Router::layer`, never
/// `route_layer`.
///
/// Why: `route_layer` covers only the routes registered before it in the same
/// chain, which is how #3268 left routes unguarded on this crate's sibling
/// guard. A single `.layer()` on the fully-merged router covers every route,
/// which for an authentication layer is the difference between a hardened
/// surface and one hole.
/// What: valid `Authorization: Bearer` → mark [`Authenticated`], continue;
/// else valid `?ticket=` → mark [`Authenticated`], continue; else a path in
/// [`DaemonAuth::with_public_paths`] → continue UNMARKED; else `401` with an
/// empty body and a bare `WWW-Authenticate: Bearer`, disclosing nothing about
/// whether the path exists, why the credential failed, or what the daemon is.
/// Test: `bearer_auth_tests::*`.
pub async fn require_bearer(
    State(auth): State<DaemonAuth>,
    mut req: Request,
    next: Next,
) -> Response {
    let authenticated = auth.header_is_valid(req.headers().get(AUTHORIZATION))
        || DaemonAuth::ticket_in_query(req.uri().query())
            .is_some_and(|ticket| auth.consume_ticket(ticket));

    if authenticated {
        req.extensions_mut().insert(Authenticated);
        return next.run(req).await;
    }
    if auth.0.public_paths.contains(req.uri().path()) {
        return next.run(req).await;
    }
    // #5439: status only — no body, no reason, no route existence signal.
    (
        StatusCode::UNAUTHORIZED,
        [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
    )
        .into_response()
}

#[cfg(test)]
mod bearer_auth_tests {
    use super::*;
    use axum::{Extension, Router, body::Body, routing::get};
    use tower::util::ServiceExt;

    /// `/health`-shaped handler: says whether the caller was authenticated, so
    /// the tests can assert the marker as well as the status.
    async fn marker_handler(auth: Option<Extension<Authenticated>>) -> &'static str {
        if auth.is_some() { "authed" } else { "anon" }
    }

    fn router(auth: DaemonAuth) -> Router {
        Router::new()
            .route("/private", get(marker_handler))
            .route("/health", get(marker_handler))
            .layer(axum::middleware::from_fn_with_state(auth, require_bearer))
    }

    fn guarded(token: &str) -> Router {
        router(DaemonAuth::new(token, ["/health"]))
    }

    async fn get_with(app: Router, uri: &str, header: Option<&str>) -> (StatusCode, String) {
        let mut req = Request::builder().uri(uri);
        if let Some(value) = header {
            req = req.header(AUTHORIZATION, value);
        }
        let resp = app
            .oneshot(req.body(Body::empty()).expect("build request"))
            .await
            .expect("router response");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("read body");
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    /// The #5439 regression: no credential must not reach the handler.
    #[tokio::test]
    async fn missing_credential_is_rejected() {
        let (status, body) = get_with(guarded(&mint_token()), "/private", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.is_empty(), "401 body must disclose nothing: {body:?}");
    }

    /// A correct credential must reach the handler and be MARKED.
    #[tokio::test]
    async fn correct_credential_is_accepted_and_marked() {
        let token = mint_token();
        let (status, body) = get_with(
            guarded(&token),
            "/private",
            Some(&format!("Bearer {token}")),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "authed");
    }

    /// A wrong token, a missing scheme, the wrong scheme, and a bare token
    /// must all fail identically — no branch may leak which part was wrong.
    #[tokio::test]
    async fn malformed_and_wrong_credentials_are_rejected() {
        let token = mint_token();
        for header in [
            format!("Bearer {}", mint_token()),
            token.clone(),
            format!("Basic {token}"),
            "Bearer".to_string(),
            format!("Bearer  {token} extra"),
            String::new(),
        ] {
            let (status, _) = get_with(guarded(&token), "/private", Some(&header)).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "header {header:?} must not authenticate"
            );
        }
    }

    /// The scheme is case-insensitive per RFC 7235, so a client sending
    /// `bearer` must not be locked out.
    #[tokio::test]
    async fn bearer_scheme_is_case_insensitive() {
        let token = mint_token();
        let (status, _) = get_with(
            guarded(&token),
            "/private",
            Some(&format!("bearer {token}")),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    /// A public path serves an anonymous caller — UNMARKED, so its handler
    /// can withhold what only an authenticated caller may see (#6472).
    #[tokio::test]
    async fn public_path_passes_without_a_credential() {
        let (status, body) = get_with(guarded(&mint_token()), "/health", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "anon");
    }

    /// The same public path WITH a valid credential must be marked, which is
    /// what lets `/health` serve two payloads from one route.
    #[tokio::test]
    async fn public_path_with_a_credential_is_marked() {
        let token = mint_token();
        let (status, body) =
            get_with(guarded(&token), "/health", Some(&format!("Bearer {token}"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "authed");
    }

    /// The carve-out is exact-match: a path that merely STARTS with a public
    /// path must stay guarded.
    #[tokio::test]
    async fn public_path_match_is_exact_not_prefix() {
        let auth = DaemonAuth::new(mint_token(), ["/priv"]);
        let (status, _) = get_with(router(auth), "/private", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// A query string must not smuggle a request past the guard just by being
    /// present — only a redeemable ticket does that.
    #[tokio::test]
    async fn unknown_ticket_is_rejected() {
        let app = guarded(&mint_token());
        let (status, _) = get_with(app, "/private?ticket=nope", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// An issued ticket authenticates exactly ONE request; the replay must
    /// fail, which is what makes a ticket safe to place in a URL.
    #[tokio::test]
    async fn ticket_authenticates_once_then_is_spent() {
        let auth = DaemonAuth::new(mint_token(), ["/health"]);
        let ticket = auth.issue_ticket();
        let uri = format!("/private?{TICKET_QUERY_PARAM}={ticket}");

        let (status, body) = get_with(router(auth.clone()), &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "authed");

        let (status, _) = get_with(router(auth), &uri, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "replay must fail");
    }

    /// A ticket issued against one clone must be redeemable against another —
    /// axum clones the state per request, and a per-clone table would reject
    /// every ticket.
    #[tokio::test]
    async fn ticket_table_is_shared_across_clones() {
        let auth = DaemonAuth::new(mint_token(), Vec::<String>::new());
        let ticket = auth.clone().issue_ticket();
        assert!(auth.consume_ticket(&ticket));
    }

    /// A ticket past [`TICKET_TTL`] must be refused even though the table
    /// still holds it — expiry is enforced at redemption, not only by the
    /// sweep in `issue_ticket`.
    #[test]
    fn expired_ticket_is_rejected() {
        let auth = DaemonAuth::new(mint_token(), Vec::<String>::new());
        let stale = mint_token();
        // A machine up for less than TICKET_TTL cannot represent the earlier
        // instant; skip rather than panic on `Instant - Duration`.
        let Some(issued) = Instant::now().checked_sub(TICKET_TTL + Duration::from_secs(1)) else {
            return;
        };
        if let Ok(mut tickets) = auth.0.tickets.lock() {
            tickets.insert(stale.clone(), issued);
        }
        assert!(!auth.consume_ticket(&stale), "an expired ticket must fail");
    }

    /// Only the `ticket` parameter is read, and only when it is spelled
    /// exactly — a lookalike key must not be mistaken for it.
    #[test]
    fn ticket_in_query_reads_only_the_named_parameter() {
        assert_eq!(DaemonAuth::ticket_in_query(None), None);
        assert_eq!(DaemonAuth::ticket_in_query(Some("")), None);
        assert_eq!(DaemonAuth::ticket_in_query(Some("other=1")), None);
        assert_eq!(DaemonAuth::ticket_in_query(Some("myticket=1")), None);
        assert_eq!(DaemonAuth::ticket_in_query(Some("ticket=abc")), Some("abc"));
        assert_eq!(
            DaemonAuth::ticket_in_query(Some("a=1&ticket=abc&b=2")),
            Some("abc")
        );
    }
}
