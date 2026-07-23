//! Internal loopback relay endpoint for cross-process event injection (#3752).
//!
//! Why: The Slack Socket-Mode gateway (`tagent --slack`) and the GUI backend
//! (`tagent --api`) are **separate processes** with independent event buses.
//! For the live Slack-conversation mirror the gateway must push its inbound /
//! reply events onto the API process's bus so the GUI's `/api/events` SSE
//! stream (and thus the desktop app) sees them. Rather than merge the two
//! processes, the gateway POSTs a serialized `Event` here and this handler
//! re-publishes it locally — a minimal relay.
//! What: `POST /api/internal/relay-event` accepts one JSON `Event` ONLY when the
//! caller presents the shared relay secret (`x-relay-token` header matching the
//! `TAGENT_RELAY_TOKEN` env on this process); it then rejects any kind that is
//! not a `Slack*` mirror event (whitelist) and any `SlackMessageReceived` whose
//! `tier` is not a known `ServiceTier`, and publishes what survives. The route
//! also inherits the router-wide same-origin write guard + optional bearer auth
//! from `routes::build_router_with_origins`, but the mandatory shared secret is
//! the load-bearing control: the origin guard fails OPEN for absent-`Origin`
//! (server-to-server) callers, so without this token ANY local process could
//! forge a reply with a fabricated identity badge — the exact honesty the pane
//! promises. The gate is FAIL CLOSED: if `TAGENT_RELAY_TOKEN` is unset on this
//! process, every relay POST is rejected.
//! Test: `relay_accepts_with_matching_token`, `relay_rejects_missing_token`,
//! `relay_rejects_wrong_token`, `relay_rejects_when_token_unset_server_side`,
//! `relay_rejects_non_slack_kind`, `relay_rejects_unknown_tier`,
//! `relay_rejects_malformed_body`, `relay_authorized_fails_closed_when_unset`,
//! `tier_is_known_accepts_the_closed_set` in `super::tests::relay`.

use axum::{
    Json,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};

use crate::events::{self, Event};

/// Header carrying the shared relay secret. MUST match the header the Slack
/// gateway sends (`crate::slack::relay::RELAY_TOKEN_HEADER`).
pub(super) const RELAY_TOKEN_HEADER: &str = "x-relay-token";

/// Env var holding the mandatory shared secret on the `--api` process.
pub(super) const RELAY_TOKEN_ENV: &str = "TAGENT_RELAY_TOKEN";

/// Decide whether a relay request is authorized. FAIL CLOSED.
///
/// Why: The relay injects displayed-identity events the pane presents as
/// truth. The origin guard admits absent-`Origin` local callers, so a shared
/// secret is the only thing stopping a local process from forging one. Absence
/// of a configured secret must therefore DENY, never allow.
/// What: Returns `true` only when `expected` is present and non-empty AND
/// `provided` equals it exactly. Any `None`/empty `expected` (secret not
/// configured server-side) → `false`.
/// Test: `relay_authorized_fails_closed_when_unset`,
/// `relay_authorized_requires_exact_match`.
pub(super) fn relay_authorized(expected: Option<&str>, provided: Option<&str>) -> bool {
    match expected {
        Some(exp) if !exp.is_empty() => provided == Some(exp),
        _ => false,
    }
}

/// Whether `tier` names a known `ServiceTier` (the closed RBAC set).
///
/// Why: The inbound badge renders an RBAC tier; an attacker (or a bug) must not
/// be able to inject an arbitrary badge string, so the server validates against
/// the enum's own serde form rather than trusting the wire value.
/// What: Parses `tier` as a `ServiceTier` via serde (`all` / `analytics` /
/// `read_only`); `true` iff it deserializes.
/// Test: `tier_is_known_accepts_the_closed_set`, `tier_is_known_rejects_unknown`.
pub(super) fn tier_is_known(tier: &str) -> bool {
    serde_json::from_value::<crate::rbac::ServiceTier>(serde_json::Value::String(tier.to_string()))
        .is_ok()
}

/// `POST /api/internal/relay-event` — inject one `Slack*` event onto the local
/// bus, gated by the mandatory shared secret. (#3752)
///
/// Why: See the module doc — the whitelist bounds WHICH events can be injected;
/// the shared secret bounds WHO can inject them (fail closed); the tier check
/// bounds what a badge can claim.
/// What: `401` when the secret is unset server-side or the `x-relay-token`
/// header is missing/wrong; `400` for a well-formed non-`Slack*` event; `422`
/// for an unknown tier (or a body that does not deserialize to `Event`, via the
/// `Json` extractor); `202 Accepted` + publish otherwise.
/// Test: `relay_accepts_with_matching_token`, `relay_rejects_missing_token`,
/// `relay_rejects_wrong_token`, `relay_rejects_when_token_unset_server_side`,
/// `relay_rejects_non_slack_kind`, `relay_rejects_unknown_tier`.
pub(super) async fn relay_event_handler(headers: HeaderMap, Json(event): Json<Event>) -> Response {
    // Mandatory shared-secret gate (fail closed).
    let expected = std::env::var(RELAY_TOKEN_ENV).ok();
    let expected = expected.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let provided = headers
        .get(RELAY_TOKEN_HEADER)
        .and_then(|h| h.to_str().ok())
        .map(str::trim);
    if !relay_authorized(expected, provided) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "relay unauthorized: set TAGENT_RELAY_TOKEN on the --api process and send a matching x-relay-token header"
            })),
        )
            .into_response();
    }

    match &event {
        Event::SlackMessageReceived { tier, .. } => {
            if !tier_is_known(tier) {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({ "error": "unknown RBAC tier" })),
                )
                    .into_response();
            }
            events::publish(event);
            StatusCode::ACCEPTED.into_response()
        }
        Event::SlackReplySent { .. } => {
            events::publish(event);
            StatusCode::ACCEPTED.into_response()
        }
        other => {
            tracing::warn!(
                kind = ?std::mem::discriminant(other),
                "relay-event rejected: only slack_* event kinds are accepted"
            );
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "relay endpoint accepts only slack_message_received / slack_reply_sent"
                })),
            )
                .into_response()
        }
    }
}
