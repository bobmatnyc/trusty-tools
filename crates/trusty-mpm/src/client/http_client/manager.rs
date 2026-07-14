//! `tm manager` (Layer-3 portfolio) client methods for [`DaemonClient`]
//! (DOC-36 §3.2, epic #2109, WI-6 #2583).
//!
//! Why: `tm manager status|digest|chat` (#2583) is the thin CLI half of the
//! `/api/v1/manager/*` surface — same "wire an endpoint exactly once" rule
//! DOC-35 §1.3 established for `projects.rs`. `status` is a pinned, live
//! contract (WI-2 #2579, shipped in PR #2598): [`PortfolioStatusWire`] mirrors
//! [`super::projects::ProjectStatusWire`] the same way the daemon's
//! `PortfolioStatusResponse` composes `ProjectStatusResponse` per-project, so
//! this module never re-derives the per-project shape. `digest` (WI-3 #2580)
//! and `chat` (WI-4 #2581) are being built concurrently by a sibling engineer
//! and are NOT mounted on `origin/main` as of this WI landing — calling them
//! against an older daemon must degrade cleanly rather than error confusingly,
//! so both methods special-case `404 Not Found` into `Ok(None)` ("this daemon
//! predates the endpoint") distinct from a genuine daemon-reported failure
//! (`Err`, e.g. "no inference provider configured", surfaced via
//! [`response_or_body_error`] per #2485). Because the exact response body
//! shape for `digest`/`chat` is not yet pinned by a shipped daemon contract,
//! both parse the body as a loosely-typed [`serde_json::Value`] and read a
//! small set of candidate field names — forward-tolerant of the sibling PR's
//! final shape rather than a brittle `#[derive(Deserialize)]` that could
//! reject a same-intent-different-key-name response outright.
//! What: [`PortfolioStatusWire`]/[`PortfolioTotalsWire`]/
//! [`DeliverableStatusCountsWire`]/[`MilestoneStatusCountsWire`] (the
//! `GET /manager/status` response DTOs), [`ManagerDigestOutcome`] /
//! [`ManagerChatOutcome`] (loosely-parsed digest/chat results), and three
//! [`DaemonClient`] methods: [`DaemonClient::manager_status`],
//! [`DaemonClient::manager_digest`], [`DaemonClient::manager_chat`].
//! Test: `portfolio_status_wire_parses_totals_and_projects`,
//! `portfolio_status_wire_tolerates_missing_fields` (wire-shape unit tests);
//! `manager_digest_outcome_reads_narrative_field_candidates`,
//! `manager_chat_outcome_reads_reply_field_candidates` (loose-parse unit
//! tests) in this module's `tests` submodule; live HTTP (incl. the 404
//! degrade path) in `crates/trusty-mpm/tests/manager_cli_client.rs`.

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::DaemonClient;
use super::error::response_or_body_error;
use super::projects::ProjectStatusWire;

/// Portfolio-wide Deliverable-status histogram (client mirror of the daemon's
/// `DeliverableStatusCounts`, reached via `PortfolioTotals`).
///
/// Why: `PortfolioStatusResponse.totals.deliverables` sums every project's
/// Deliverable histogram (`daemon/manager/status.rs::fold_totals`); the
/// client needs its own `Deserialize` mirror, forward-tolerant of any future
/// additive status variant.
/// What: one count per Deliverable status plus the total.
/// Test: `portfolio_status_wire_parses_totals_and_projects`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeliverableStatusCountsWire {
    #[serde(default)]
    pub proposed: usize,
    #[serde(default)]
    pub in_progress: usize,
    #[serde(default)]
    pub blocked: usize,
    #[serde(default)]
    pub complete: usize,
    #[serde(default)]
    pub delivered: usize,
    #[serde(default)]
    pub shipped: usize,
    #[serde(default)]
    pub total: usize,
}

/// Portfolio-wide Milestone-status histogram (client mirror of the daemon's
/// `MilestoneStatusCounts`).
///
/// What: one count per Milestone status, the total, and the dangling-ref
/// count carried through from the per-project rollup.
/// Test: `portfolio_status_wire_parses_totals_and_projects`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MilestoneStatusCountsWire {
    #[serde(default)]
    pub proposed: usize,
    #[serde(default)]
    pub in_progress: usize,
    #[serde(default)]
    pub complete: usize,
    #[serde(default)]
    pub shipped: usize,
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub dangling_deliverable_refs: usize,
}

/// Portfolio-wide aggregate totals (client mirror of the daemon's
/// `PortfolioTotals`, `daemon/manager/status.rs`).
///
/// What: the summed session/Deliverable/Milestone histograms plus the most
/// recent activity timestamp across the whole portfolio.
/// Test: `portfolio_status_wire_parses_totals_and_projects`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PortfolioTotalsWire {
    #[serde(default)]
    pub sessions: super::projects::SessionStateCountsWire,
    #[serde(default)]
    pub deliverables: DeliverableStatusCountsWire,
    #[serde(default)]
    pub milestones: MilestoneStatusCountsWire,
    #[serde(default)]
    pub last_activity_at: Option<DateTime<Utc>>,
}

/// Deserialized `GET /api/v1/manager/status` rollup (client mirror of the
/// daemon's `PortfolioStatusResponse`, `daemon/manager/status.rs`).
///
/// Why: no `deny_unknown_fields` — forward-tolerant of any additive field a
/// later phase adds, mirroring [`ProjectStatusWire`]'s own tolerance policy.
/// What: the registered-project count, the portfolio-wide totals, and the
/// per-project breakdown (each entry the SAME [`ProjectStatusWire`]
/// `tm projects status` already parses — no reimplementation).
/// Test: `portfolio_status_wire_parses_totals_and_projects`,
/// `portfolio_status_wire_tolerates_missing_fields`.
#[derive(Debug, Clone, Deserialize)]
pub struct PortfolioStatusWire {
    #[serde(default)]
    pub project_count: usize,
    #[serde(default)]
    pub totals: PortfolioTotalsWire,
    #[serde(default)]
    pub projects: Vec<ProjectStatusWire>,
}

/// Loosely-parsed result of `GET /api/v1/manager/digest`.
///
/// Why: see module doc — the response shape is not yet pinned by a shipped
/// daemon contract (WI-3 #2580 is concurrent), so this is read from raw JSON
/// rather than a strict DTO.
/// What: the narrative text (empty string if the body carried none of the
/// candidate field names) and whether the daemon marked it as the
/// deterministic no-LLM-provider fallback (DOC-36 §3.2/DOC-16 D1).
/// Test: `manager_digest_outcome_reads_narrative_field_candidates`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerDigestOutcome {
    /// The narrative text, or empty when the daemon returned no recognized
    /// text field.
    pub narrative: String,
    /// True when the daemon marked this as the deterministic fallback
    /// (no inference adapter configured).
    pub fallback: bool,
    /// The raw response body, kept for `--json` passthrough.
    pub raw: serde_json::Value,
}

impl ManagerDigestOutcome {
    fn from_body(raw: serde_json::Value) -> Self {
        let narrative =
            first_str_field(&raw, &["narrative", "digest", "text", "summary"]).unwrap_or_default();
        let fallback = raw
            .get("fallback")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        Self {
            narrative,
            fallback,
            raw,
        }
    }
}

/// Loosely-parsed result of `POST /api/v1/manager/chat`.
///
/// Why: see module doc — same forward-tolerant loose parse as
/// [`ManagerDigestOutcome`], for the same reason (WI-4 #2581 is concurrent).
/// What: the assistant's reply text and the conversation key the daemon
/// echoed back (falls back to the key the request sent, if the daemon
/// omitted it).
/// Test: `manager_chat_outcome_reads_reply_field_candidates`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerChatOutcome {
    /// The assistant's reply text, or empty when unrecognized.
    pub reply: String,
    /// The conversation key this turn was recorded under.
    pub conversation_key: String,
    /// The raw response body, kept for `--json` passthrough.
    pub raw: serde_json::Value,
}

impl ManagerChatOutcome {
    fn from_body(raw: serde_json::Value, requested_key: &str) -> Self {
        let reply =
            first_str_field(&raw, &["reply", "message", "text", "response"]).unwrap_or_default();
        let conversation_key = raw
            .get("conversation_key")
            .and_then(|v| v.as_str())
            .unwrap_or(requested_key)
            .to_string();
        Self {
            reply,
            conversation_key,
            raw,
        }
    }
}

/// Read the first present string field among `candidates`.
fn first_str_field(body: &serde_json::Value, candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find_map(|key| body.get(*key).and_then(|v| v.as_str()))
        .map(str::to_string)
}

impl DaemonClient {
    /// Fetch the deterministic portfolio rollup via `GET /api/v1/manager/status`.
    ///
    /// Why: backs `tm manager status` (#2583). No LLM call, no channel/bot
    /// token — DOC-36 §4's local-testability bar. This endpoint has been live
    /// since WI-2 (#2579, PR #2598), so a 404 here is a genuine "unreachable
    /// or ancient daemon" condition and surfaces as an ordinary `Err` via
    /// [`response_or_body_error`], unlike the deliberate 404-degrade on
    /// [`Self::manager_digest`]/[`Self::manager_chat`].
    /// What: GETs the rollup, returns the parsed [`PortfolioStatusWire`].
    /// Test: `portfolio_status_wire_parses_totals_and_projects`; live HTTP via
    /// `crates/trusty-mpm/tests/manager_cli_client.rs`.
    pub async fn manager_status(&self) -> anyhow::Result<PortfolioStatusWire> {
        let url = format!("{}/api/v1/manager/status", self.base);
        let resp = self.http.get(&url).send().await?;
        let status: PortfolioStatusWire = response_or_body_error(resp)
            .await?
            .json()
            .await
            .context("deserialize manager status")?;
        Ok(status)
    }

    /// Fetch the portfolio (or single-project) digest via
    /// `GET /api/v1/manager/digest?scope=<scope>`.
    ///
    /// Why: backs `tm manager digest` (#2583). WI-3 (#2580) ships this route
    /// concurrently with this CLI; against a daemon that predates it, the
    /// route answers `404` (see `version.rs`'s `advertised_endpoints`, which
    /// lists `digest` as `available: false` until WI-3 lands) — that is NOT
    /// the same condition as "the daemon has no inference provider
    /// configured" (DOC-16 D1's fallback path, which per DOC-36 §3.2 still
    /// answers `200` with `fallback: true`, or — depending on the sibling
    /// PR's final error-vs-fallback choice — a non-2xx the daemon annotates
    /// with an actionable body message). Splitting the two here means the CLI
    /// handler can print "upgrade your daemon" for the former and the
    /// daemon's own message for the latter, rather than one generic HTTP
    /// error for both.
    /// What: `Ok(None)` on `404` (older daemon); `Err` on any other non-2xx,
    /// carrying the daemon's body message via [`response_or_body_error`]
    /// (#2485); `Ok(Some(outcome))` on success with the response loosely
    /// parsed per [`ManagerDigestOutcome::from_body`].
    /// Test: `manager_digest_outcome_reads_narrative_field_candidates`; live
    /// HTTP (incl. the 404 degrade) via
    /// `crates/trusty-mpm/tests/manager_cli_client.rs`.
    pub async fn manager_digest(
        &self,
        scope: &str,
    ) -> anyhow::Result<Option<ManagerDigestOutcome>> {
        let url = format!("{}/api/v1/manager/digest", self.base);
        let resp = self
            .http
            .get(&url)
            .query(&[("scope", scope)])
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let body: serde_json::Value = response_or_body_error(resp)
            .await?
            .json()
            .await
            .context("deserialize manager digest")?;
        Ok(Some(ManagerDigestOutcome::from_body(body)))
    }

    /// Send one chat turn via `POST /api/v1/manager/chat`.
    ///
    /// Why: backs `tm manager chat` (#2583). Same concurrent-sibling-route
    /// story as [`Self::manager_digest`] — WI-4 (#2581) ships this route
    /// concurrently; a 404 here means "this daemon does not support manager
    /// chat yet". Uses the same longer chat-scoped timeout
    /// [`super::config::CHAT_REQUEST_TIMEOUT`] the existing `llm_chat`/
    /// `coordinator_chat` methods use, since this also waits on the daemon's
    /// own upstream LLM round trip (DOC-36 §3.3).
    /// What: POSTs `{ conversation_key, message }`; `Ok(None)` on `404`
    /// (older daemon); `Err` on any other non-2xx (#2485 body message);
    /// `Ok(Some(outcome))` on success with the response loosely parsed.
    /// Test: `manager_chat_outcome_reads_reply_field_candidates`; live HTTP
    /// (incl. the 404 degrade) via
    /// `crates/trusty-mpm/tests/manager_cli_client.rs`.
    pub async fn manager_chat(
        &self,
        conversation_key: &str,
        message: &str,
    ) -> anyhow::Result<Option<ManagerChatOutcome>> {
        let url = format!("{}/api/v1/manager/chat", self.base);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({
                "conversation_key": conversation_key,
                "message": message,
            }))
            .timeout(super::config::CHAT_REQUEST_TIMEOUT)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let body: serde_json::Value = response_or_body_error(resp)
            .await?
            .json()
            .await
            .context("deserialize manager chat reply")?;
        Ok(Some(ManagerChatOutcome::from_body(body, conversation_key)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portfolio_status_wire_parses_totals_and_projects() {
        let json = serde_json::json!({
            "project_count": 2,
            "totals": {
                "sessions": { "provisioning": 1, "active": 2, "stopped": 0,
                              "errored": 0, "decommissioned": 0, "total": 3 },
                "deliverables": { "proposed": 1, "in_progress": 1, "total": 2 },
                "milestones": { "complete": 1, "total": 1 },
                "last_activity_at": "2026-07-13T12:00:00Z"
            },
            "projects": [
                { "project_name": "alpha", "repo_url": "u1",
                  "sessions": { "total": 1 } },
                { "project_name": "beta", "repo_url": "u2",
                  "sessions": { "total": 2 } }
            ]
        });
        let status: PortfolioStatusWire = serde_json::from_value(json).unwrap();
        assert_eq!(status.project_count, 2);
        assert_eq!(status.totals.sessions.total, 3);
        assert_eq!(status.totals.deliverables.total, 2);
        assert_eq!(status.totals.milestones.total, 1);
        assert!(status.totals.last_activity_at.is_some());
        assert_eq!(status.projects.len(), 2);
        assert_eq!(status.projects[0].project_name, "alpha");
    }

    #[test]
    fn portfolio_status_wire_tolerates_missing_fields() {
        let json = serde_json::json!({});
        let status: PortfolioStatusWire = serde_json::from_value(json).unwrap();
        assert_eq!(status.project_count, 0);
        assert_eq!(status.totals.sessions.total, 0);
        assert!(status.projects.is_empty());
    }

    #[test]
    fn manager_digest_outcome_reads_narrative_field_candidates() {
        let a = ManagerDigestOutcome::from_body(serde_json::json!({
            "narrative": "all quiet", "fallback": true
        }));
        assert_eq!(a.narrative, "all quiet");
        assert!(a.fallback);

        // A differently-named field (e.g. the sibling PR ships `digest`
        // instead of `narrative`) still parses.
        let b = ManagerDigestOutcome::from_body(serde_json::json!({ "digest": "busy day" }));
        assert_eq!(b.narrative, "busy day");
        assert!(!b.fallback);

        let c = ManagerDigestOutcome::from_body(serde_json::json!({}));
        assert_eq!(c.narrative, "");
        assert!(!c.fallback);
    }

    #[test]
    fn manager_chat_outcome_reads_reply_field_candidates() {
        let a = ManagerChatOutcome::from_body(
            serde_json::json!({ "reply": "hi there", "conversation_key": "k1" }),
            "requested-key",
        );
        assert_eq!(a.reply, "hi there");
        assert_eq!(a.conversation_key, "k1");

        // Missing conversation_key echo falls back to the requested key.
        let b = ManagerChatOutcome::from_body(serde_json::json!({ "message": "yo" }), "req-key");
        assert_eq!(b.reply, "yo");
        assert_eq!(b.conversation_key, "req-key");
    }
}
