//! Fire-and-forget relay client: pushes Slack-mirror events to the `--api`
//! process so the GUI can render both sides of a live conversation (#3752).
//!
//! Why: The Slack gateway (`tagent --slack`) and the GUI backend
//! (`tagent --api`) are separate processes. To mirror a conversation in the
//! GUI the gateway POSTs each inbound / reply event to the API process's
//! `/api/internal/relay-event` endpoint, which re-publishes it on the bus that
//! feeds `/api/events`. This MUST never block or fail Slack handling: the relay
//! is best-effort telemetry, so every failure path logs and returns rather than
//! propagating — the human on Slack still gets their reply even when no GUI is
//! running.
//! What: `relay_event` resolves the target URL (`TAGENT_API_RELAY_URL`, default
//! `http://127.0.0.1:8765`), POSTs the serialized `Event` with a short timeout,
//! and swallows all errors. `tier_label` renders an RBAC `ServiceTier` as the
//! honest snake_case badge string; `BOT_IDENTITY` is the honest reply-speaker
//! label (the bot always speaks as itself — there is no impersonation mode).
//! Test: `relay_tolerates_unreachable_server`, `tier_label_matches_serde`,
//! `relay_endpoint_defaults_and_trims` in `super::tests`.

use std::time::Duration;

use tracing::warn;

use crate::events::Event;
use crate::rbac::ServiceTier;

/// Default relay target when `TAGENT_API_RELAY_URL` is unset — the loopback
/// port the Tauri shell launches `tagent --api` on (`App.svelte` bootstrap).
pub(super) const DEFAULT_RELAY_URL: &str = "http://127.0.0.1:8765";

/// Short per-request timeout. The relay is fire-and-forget; a hung GUI backend
/// must not stall the Slack reply path, so we cap the attempt aggressively.
const RELAY_TIMEOUT_MS: u64 = 500;

/// Env var holding the mandatory shared relay secret. MUST be set to the SAME
/// value on the `--api` process, which rejects (401) any post whose
/// `x-relay-token` header does not match — and rejects ALL posts when the
/// secret is unset there (fail closed). See `api::server::relay`.
pub(super) const RELAY_TOKEN_ENV: &str = "TAGENT_RELAY_TOKEN";

/// Header carrying the shared relay secret. MUST match
/// `crate::api::server::relay::RELAY_TOKEN_HEADER`.
const RELAY_TOKEN_HEADER: &str = "x-relay-token";

/// Honest reply-speaker label. The bot always replies AS ITSELF — there is no
/// "as-Bob" impersonation mode in code, so the mirror must never claim one.
pub(super) const BOT_IDENTITY: &str = "CTO Bot (as itself)";

/// Render an RBAC `ServiceTier` as its snake_case badge label.
///
/// Why: The GUI mirror shows an honest access-level badge next to the inbound
/// human message. The label must match the tier's serde wire form so the
/// front-end and any logs agree on the same three strings.
/// What: `All` → `"all"`, `Analytics` → `"analytics"`, `ReadOnly` →
/// `"read_only"` (identical to `ServiceTier`'s `snake_case` serde).
/// Test: `tier_label_matches_serde`.
pub(super) fn tier_label(tier: &ServiceTier) -> &'static str {
    match tier {
        ServiceTier::All => "all",
        ServiceTier::Analytics => "analytics",
        ServiceTier::ReadOnly => "read_only",
    }
}

/// Resolve the relay endpoint URL from env (or the loopback default).
///
/// Why: The Tauri shell launches `--api` on `127.0.0.1:8765`; a manual
/// `--api --port N` launch overrides it via `TAGENT_API_RELAY_URL`. Trailing
/// slashes are trimmed so the joined path never doubles up.
/// What: `<base>/api/internal/relay-event`, base from `TAGENT_API_RELAY_URL`
/// (non-empty) or `DEFAULT_RELAY_URL`.
/// Test: `relay_endpoint_defaults_and_trims`.
pub(super) fn relay_endpoint() -> String {
    let base = std::env::var("TAGENT_API_RELAY_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_RELAY_URL.to_string());
    format!("{}/api/internal/relay-event", base.trim_end_matches('/'))
}

/// POST one event to the resolved relay endpoint, swallowing every failure.
///
/// Why: Best-effort telemetry — see the module doc. Owns its `Event` so it is
/// `'static` and can be detached via `tokio::spawn` from the Slack handler,
/// guaranteeing the reply path never awaits the network. All env reads happen
/// HERE (not in `post_event`) so the inner POST is env-free and unit-testable
/// without racing the process environment.
/// What: Resolves the endpoint + the shared relay secret (`TAGENT_RELAY_TOKEN`,
/// sent as `x-relay-token` — REQUIRED by the `--api` side) + an optional
/// `TAGENT_API_TOKEN` bearer (when the API is `--api-token`-guarded), then
/// delegates to `post_event`.
/// Test: `relay_tolerates_unreachable_server`.
pub(super) async fn relay_event(event: Event) {
    let relay_token = std::env::var(RELAY_TOKEN_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty());
    let api_token = std::env::var("TAGENT_API_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty());
    post_event(
        &relay_endpoint(),
        &event,
        relay_token.as_deref(),
        api_token.as_deref(),
    )
    .await;
}

/// Inner POST against an explicit endpoint with explicit tokens, so tests can
/// target a closed port without mutating process env.
///
/// What: Attaches `x-relay-token: <relay_token>` (the shared secret the API
/// side mandates) and, when present, an `Authorization: Bearer <api_token>`.
/// Logs — never returns — on any error.
/// Test: `relay_tolerates_unreachable_server`.
async fn post_event(
    endpoint: &str,
    event: &Event,
    relay_token: Option<&str>,
    api_token: Option<&str>,
) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(RELAY_TIMEOUT_MS))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "slack relay: client build failed (non-fatal)");
            return;
        }
    };
    let mut req = client.post(endpoint).json(event);
    if let Some(t) = relay_token {
        req = req.header(RELAY_TOKEN_HEADER, t.trim());
    }
    if let Some(t) = api_token {
        req = req.bearer_auth(t.trim());
    }
    match req.send().await {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => warn!(status = %resp.status(), "slack relay: non-success status (non-fatal)"),
        Err(e) => warn!(error = %e, "slack relay: send failed (non-fatal)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_label_matches_serde() {
        for tier in [
            ServiceTier::All,
            ServiceTier::Analytics,
            ServiceTier::ReadOnly,
        ] {
            // The badge label must equal the tier's serde wire form (the quotes
            // are stripped) so the UI and logs share one vocabulary.
            let serde = serde_json::to_string(&tier).unwrap();
            let serde = serde.trim_matches('"');
            assert_eq!(tier_label(&tier), serde);
        }
    }

    #[test]
    fn relay_endpoint_defaults_and_trims() {
        // Default path (env unset in the normal test env).
        let ep = relay_endpoint();
        assert!(
            ep.ends_with("/api/internal/relay-event"),
            "unexpected endpoint: {ep}"
        );
        assert!(!ep.contains("//api/internal"), "double slash in {ep}");
    }

    #[tokio::test]
    async fn relay_tolerates_unreachable_server() {
        // Port 59999 has no listener → connection refused. The relay must
        // return (not panic, not hang) — the fire-and-forget contract. Passes
        // explicit tokens so it never reads process env (race-free).
        let ev = Event::SlackReplySent {
            channel: "C0".into(),
            text: "hi".into(),
            identity: BOT_IDENTITY.into(),
        };
        post_event(
            "http://127.0.0.1:59999/api/internal/relay-event",
            &ev,
            Some("secret"),
            None,
        )
        .await;
    }
}
