//! Tests for the internal Slack-mirror relay endpoint (#3752).
//!
//! Why: `POST /api/internal/relay-event` injects events onto the GUI bus from
//! the separate `tagent --slack` process. Its contract is narrow and
//! security-relevant: it is the load-bearing honesty control for the mirror
//! pane, so it must FAIL CLOSED without the shared secret, reject a wrong/
//! missing token, accept only with a matching token, then accept only the two
//! `Slack*` kinds with a known RBAC tier and (#3761) an honest reply identity.
//! These cases lock that contract.
//! What: oneshots the endpoint on a default (loopback-only) router — the origin
//! guard fails open on the absent `Origin` header (the real server-to-server
//! posture), so the shared-secret gate is what actually protects the route.
//! `TAGENT_RELAY_TOKEN` is a process-global env var, so every test that sets it
//! holds `RELAY_ENV_LOCK` for its whole body (the parent `tests` module allows
//! `await_holding_lock`), and only relay tests read this var.
//! Test: this module IS the test.

use std::sync::Mutex;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use tower::ServiceExt;

use crate::api::server::relay::{identity_is_honest, relay_authorized, tier_is_known};
use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;
use crate::events;

/// Serializes tests that mutate `TAGENT_RELAY_TOKEN` (process-global env).
static RELAY_ENV_LOCK: Mutex<()> = Mutex::new(());

const RELAY_TOKEN_ENV: &str = "TAGENT_RELAY_TOKEN";
const RELAY_TOKEN_HEADER: &str = "x-relay-token";

/// Set (`Some`) or clear (`None`) the server-side relay secret.
///
/// SAFETY: every caller holds `RELAY_ENV_LOCK` for its whole body and only
/// relay tests read this var, so no concurrent get/set can race.
fn set_relay_env(val: Option<&str>) {
    unsafe {
        match val {
            Some(v) => std::env::set_var(RELAY_TOKEN_ENV, v),
            None => std::env::remove_var(RELAY_TOKEN_ENV),
        }
    }
}

/// POST a raw JSON body to the relay endpoint (optionally with an
/// `x-relay-token` header) and return the response status.
async fn post_relay(json_body: &str, token_header: Option<&str>) -> StatusCode {
    let app = build_router(AppState::default());
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/api/internal/relay-event")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token_header {
        builder = builder.header(RELAY_TOKEN_HEADER, t);
    }
    let req = builder.body(Body::from(json_body.to_string())).unwrap();
    app.oneshot(req).await.unwrap().status()
}

fn inbound_body(tier: &str) -> String {
    serde_json::to_string(&events::Event::SlackMessageReceived {
        channel: "C0123ABC".into(),
        user_display: "Masa".into(),
        text: "deploy status?".into(),
        tier: tier.into(),
    })
    .unwrap()
}

/// #3761: a reply-half body carrying an arbitrary speaker label.
fn reply_body(channel: &str, identity: &str) -> String {
    serde_json::to_string(&events::Event::SlackReplySent {
        channel: channel.into(),
        text: "all green".into(),
        identity: identity.into(),
    })
    .unwrap()
}

/// Drain everything currently queued for `rx` on the process-global bus,
/// reporting how many events the channel dropped underneath us.
///
/// Why: `while let Ok(ev) = rx.try_recv()` — the idiom these tests used — stops
/// at the FIRST `Err`, and `TryRecvError::Lagged(n)` IS an `Err`. On a busy bus
/// (every other test in this binary publishes to the same global channel) that
/// silently truncates the drain. In a positive test that is a flake; in a
/// negative test asserting "nothing reached the bus" it is worse — the
/// assertion passes VACUOUSLY in exactly the case where events were dropped
/// unseen. Continuing past `Lagged` and surfacing the count lets a caller
/// refuse to conclude anything from a truncated view.
/// What: Returns `(events, skipped)`; `skipped` totals the events the broadcast
/// channel reported as dropped for this receiver. Stops on `Empty`/`Closed`.
/// Test: used by `relay_accepts_with_matching_token`,
/// `relay_accepts_honest_identity`, `relay_rejects_unknown_identity`.
fn drain_bus(
    rx: &mut tokio::sync::broadcast::Receiver<events::Event>,
) -> (Vec<events::Event>, u64) {
    use tokio::sync::broadcast::error::TryRecvError;
    let mut drained = Vec::new();
    let mut skipped = 0u64;
    loop {
        match rx.try_recv() {
            Ok(ev) => drained.push(ev),
            Err(TryRecvError::Lagged(n)) => skipped += n,
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
        }
    }
    (drained, skipped)
}

// -- shared-secret gate (fail closed) ---------------------------------------

#[tokio::test]
async fn relay_rejects_when_token_unset_server_side() {
    let _g = RELAY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_relay_env(None); // secret not configured → reject everything.
    // Even a well-formed event carrying *some* token is rejected.
    let status = post_relay(&inbound_body("all"), Some("anything")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn relay_rejects_missing_token() {
    let _g = RELAY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_relay_env(Some("s3cret"));
    let status = post_relay(&inbound_body("all"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    set_relay_env(None);
}

#[tokio::test]
async fn relay_rejects_wrong_token() {
    let _g = RELAY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_relay_env(Some("s3cret"));
    let status = post_relay(&inbound_body("all"), Some("nope")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    set_relay_env(None);
}

// -- accept path + payload validation (with a matching token) ---------------

#[tokio::test]
async fn relay_accepts_with_matching_token() {
    let _g = RELAY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_relay_env(Some("s3cret"));

    // Subscribe BEFORE the request to confirm the handler actually published.
    let mut rx = events::subscribe();
    let unique_channel = "C_RELAY_ACCEPT_TEST";
    let body = serde_json::to_string(&events::Event::SlackMessageReceived {
        channel: unique_channel.into(),
        user_display: "Masa".into(),
        text: "deploy status?".into(),
        tier: "all".into(),
    })
    .unwrap();

    assert_eq!(
        post_relay(&body, Some("s3cret")).await,
        StatusCode::ACCEPTED
    );

    // The bus is process-global, so drain and find our unique event. `drain_bus`
    // survives a `Lagged` notice instead of stopping at it.
    let (drained, skipped) = drain_bus(&mut rx);
    let found = drained.iter().any(|ev| {
        matches!(
            ev,
            events::Event::SlackMessageReceived { channel, tier, .. }
                if channel == unique_channel && tier == "all"
        )
    });
    assert!(
        found,
        "relay did not publish the injected event onto the bus \
         ({skipped} event(s) were dropped by broadcast lag)"
    );
    set_relay_env(None);
}

#[tokio::test]
async fn relay_rejects_non_slack_kind() {
    let _g = RELAY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_relay_env(Some("s3cret"));
    // A well-formed, non-Slack event kind is rejected by the whitelist so the
    // GUI can't be fed forged task/agent telemetry through this surface.
    let body = serde_json::to_string(&events::Event::Ping).unwrap();
    assert_eq!(
        post_relay(&body, Some("s3cret")).await,
        StatusCode::BAD_REQUEST
    );
    set_relay_env(None);
}

#[tokio::test]
async fn relay_rejects_unknown_tier() {
    let _g = RELAY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_relay_env(Some("s3cret"));
    // A tier outside the closed ServiceTier set must not become a badge.
    let status = post_relay(&inbound_body("root"), Some("s3cret")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    set_relay_env(None);
}

// -- #3761: reply identity must be the one honest label --------------------

#[tokio::test]
async fn relay_rejects_unknown_identity() {
    let _g = RELAY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_relay_env(Some("s3cret"));

    // Subscribe first so we can prove NOTHING was published, not merely that
    // the status was 422.
    let mut rx = events::subscribe();
    let unique_channel = "C_RELAY_FORGED_IDENTITY";

    // A valid-token holder must still not be able to stamp a reply with a
    // fabricated speaker — that is the impersonation the pane forbids.
    for forged in ["CTO Bot (as Bob)", "Masa", "", "cto bot (as itself)"] {
        assert_eq!(
            post_relay(&reply_body(unique_channel, forged), Some("s3cret")).await,
            StatusCode::UNPROCESSABLE_ENTITY,
            "identity {forged:?} must be rejected"
        );
    }

    let (drained, skipped) = drain_bus(&mut rx);
    // A truncated view cannot prove absence. If the bus lagged, FAIL rather
    // than let "nothing reached the bus" pass vacuously.
    assert_eq!(
        skipped, 0,
        "the bus dropped {skipped} event(s); this test cannot prove the forged \
         reply was absent from a truncated view"
    );
    for ev in &drained {
        if let events::Event::SlackReplySent {
            channel, identity, ..
        } = ev
        {
            assert_ne!(
                channel, unique_channel,
                "a forged-identity reply reached the bus (identity {identity:?})"
            );
        }
    }
    set_relay_env(None);
}

#[tokio::test]
async fn relay_accepts_honest_identity() {
    let _g = RELAY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_relay_env(Some("s3cret"));

    let mut rx = events::subscribe();
    let unique_channel = "C_RELAY_HONEST_IDENTITY";
    let body = reply_body(unique_channel, crate::slack::BOT_IDENTITY);

    assert_eq!(
        post_relay(&body, Some("s3cret")).await,
        StatusCode::ACCEPTED
    );

    let (drained, skipped) = drain_bus(&mut rx);
    let found = drained.iter().any(|ev| {
        matches!(
            ev,
            events::Event::SlackReplySent { channel, identity, .. }
                if channel == unique_channel && identity == crate::slack::BOT_IDENTITY
        )
    });
    assert!(
        found,
        "the honest reply was not published onto the bus \
         ({skipped} event(s) were dropped by broadcast lag)"
    );
    set_relay_env(None);
}

#[tokio::test]
async fn relay_rejects_malformed_body() {
    let _g = RELAY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_relay_env(Some("s3cret"));
    // An unknown `type` tag never deserializes to `Event`; axum's `Json`
    // extractor rejects it (422) before the handler runs. Either way it is a
    // client error, never a 2xx.
    let status = post_relay(r#"{"type":"not_a_real_kind","x":1}"#, Some("s3cret")).await;
    assert!(status.is_client_error(), "expected 4xx, got {status}");
    set_relay_env(None);
}

// -- pure helpers -----------------------------------------------------------

#[test]
fn relay_authorized_fails_closed_when_unset() {
    // No secret configured server-side → deny, regardless of what's provided.
    assert!(!relay_authorized(None, Some("anything")));
    assert!(!relay_authorized(None, None));
    assert!(!relay_authorized(Some(""), Some("")));
}

#[test]
fn relay_authorized_requires_exact_match() {
    assert!(relay_authorized(Some("abc"), Some("abc")));
    assert!(!relay_authorized(Some("abc"), Some("abcd")));
    assert!(!relay_authorized(Some("abc"), None));

    // #3761: the compare is now constant-time (`subtle::ConstantTimeEq`).
    // Correctness must be identical to the `==` it replaced, so pin the cases
    // a length-first / byte-loop implementation is most likely to get wrong.
    // Shorter, longer, and equal-length-but-different provided values:
    assert!(!relay_authorized(Some("abc"), Some("ab")));
    assert!(!relay_authorized(Some("abc"), Some("abd")));
    assert!(!relay_authorized(Some("abc"), Some("")));
    // Differs only in the LAST byte (the case `==`'s short-circuit made
    // slowest to reject) and only in the FIRST (the fastest):
    assert!(!relay_authorized(
        Some("s3cret-token"),
        Some("s3cret-tokeN")
    ));
    assert!(!relay_authorized(
        Some("s3cret-token"),
        Some("S3cret-token")
    ));
    // Long exact match still succeeds, and multi-byte UTF-8 is compared by
    // bytes without panicking on a char boundary.
    assert!(relay_authorized(
        Some("a-fairly-long-shared-secret-0123456789"),
        Some("a-fairly-long-shared-secret-0123456789")
    ));
    assert!(relay_authorized(Some("tökén-π"), Some("tökén-π")));
    assert!(!relay_authorized(Some("tökén-π"), Some("tökén-ω")));
}

#[test]
fn identity_is_honest_accepts_only_the_bot_label() {
    // #3761: exactly one honest value — the bot always speaks as itself.
    assert!(identity_is_honest(crate::slack::BOT_IDENTITY));
    assert!(!identity_is_honest("CTO Bot (as Bob)"));
    assert!(!identity_is_honest("Masa"));
    assert!(!identity_is_honest(""));
    // No case- or whitespace-insensitive near-misses.
    assert!(!identity_is_honest("cto bot (as itself)"));
    assert!(!identity_is_honest(" CTO Bot (as itself) "));
}

#[test]
fn tier_is_known_accepts_the_closed_set() {
    assert!(tier_is_known("all"));
    assert!(tier_is_known("analytics"));
    assert!(tier_is_known("read_only"));
}

#[test]
fn tier_is_known_rejects_unknown() {
    assert!(!tier_is_known("root"));
    assert!(!tier_is_known(""));
    assert!(!tier_is_known("ALL"));
}
