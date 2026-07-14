//! `tm manager` (Layer-3 portfolio) client methods for [`DaemonClient`]
//! (DOC-36 §3.2, epic #2109, WI-6 #2583).
//!
//! Why: `tm manager status|digest|chat` (#2583) is the thin CLI half of the
//! `/api/v1/manager/*` surface — same "wire an endpoint exactly once" rule
//! DOC-35 §1.3 established for `projects.rs`. All three routes are now pinned,
//! shipped contracts: `status` (WI-2 #2579, PR #2598) and `digest`/`chat`
//! (WI-3/WI-4 #2580/#2581, PR #2601) — see `daemon/manager/{status,digest,
//! chat}.rs`. [`PortfolioStatusWire`] mirrors [`super::projects::ProjectStatusWire`]
//! the same way the daemon's `PortfolioStatusResponse` composes
//! `ProjectStatusResponse` per-project. [`ManagerDigestOutcome`]/
//! [`ManagerChatOutcome`] read the daemon's REAL response fields
//! (`digest.rs`'s `DigestResponse` — `narrative`/`generated_by`/`status`;
//! `chat.rs`'s `ChatReplyBody`/`chat_error` — `reply`/`conversation_key` on
//! success, `error`/`message` on degrade) rather than a speculative candidate
//! list, now that the shape is a shipped contract, not a concurrent guess.
//!
//! Two behaviors this module gets deliberately right, both found in review
//! (PR #2600 paired review against PR #2601's shapes):
//!
//! 1. **The daemon's degrade body is not an HTTP error to discard.**
//!    `digest.rs`'s 502/503 branches (`digest_call_failed`, the no-provider
//!    503) return a FULL `DigestResponse` — narrative + status snapshot +
//!    `generated_by: "deterministic_fallback"` — by design, so a consumer can
//!    degrade gracefully without a second call. `chat.rs`'s 400/502/503
//!    branches return `{ error, message }` with an actionable `message`
//!    (never a `reply`, since there is no assistant text to synthesize).
//!    [`Self::manager_digest`]/[`Self::manager_chat`] therefore parse the
//!    response body FIRST, regardless of status code, and only fall back to
//!    treating the response as a hard error when the body carries neither
//!    shape — never blindly call `response_or_body_error` (which would
//!    discard exactly the body the daemon went out of its way to send).
//! 2. **A `404` is not always "this daemon is too old."** `digest.rs` ALSO
//!    answers `404` for a legitimate, mounted request — `scope=project:<name>`
//!    naming an unregistered project (`"project '{name}' is not registered"`).
//!    Treating every `404` as "upgrade your daemon" would misreport a typo'd
//!    project name as a version problem. [`Self::manager_endpoint_available`]
//!    feature-detects via the already-live `GET /api/v1/manager/version`
//!    (`version.rs`'s `advertised_endpoints`, flips `available: true` once
//!    WI-3/WI-4 land) and only degrades to `Ok(None)` when the endpoint is
//!    genuinely unavailable; otherwise the daemon's own 404 body surfaces as
//!    `Err`.
//!
//! What: [`PortfolioStatusWire`]/[`PortfolioTotalsWire`]/
//! [`DeliverableStatusCountsWire`]/[`MilestoneStatusCountsWire`] (the
//! `GET /manager/status` response DTOs), [`ManagerDigestOutcome`] /
//! [`ManagerChatOutcome`] (parsed digest/chat results, success OR degrade),
//! and three [`DaemonClient`] methods: [`DaemonClient::manager_status`],
//! [`DaemonClient::manager_digest`], [`DaemonClient::manager_chat`].
//! Test: `portfolio_status_wire_parses_totals_and_projects`,
//! `portfolio_status_wire_tolerates_missing_fields` (wire-shape unit tests);
//! `manager_digest_outcome_reads_real_daemon_shape`,
//! `manager_digest_outcome_marks_deterministic_fallback`,
//! `manager_chat_outcome_reads_real_daemon_shape`,
//! `manager_chat_outcome_surfaces_error_message_on_degrade` (parse unit
//! tests, bodies built from the real `DigestResponse`/`ChatReplyBody`
//! structs) in this module's `tests` submodule; live HTTP (incl. the 404
//! degrade AND the real-error-body paths) in
//! `crates/trusty-mpm/tests/manager_cli_client.rs`.

use anyhow::Context;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::Value;

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

/// `generated_by` value the daemon sends for the deterministic templated
/// fallback (`daemon/manager/digest.rs::GENERATED_BY_FALLBACK`).
const GENERATED_BY_FALLBACK: &str = "deterministic_fallback";

/// Parsed result of `GET /api/v1/manager/digest`, covering BOTH the success
/// (200, `generated_by: "llm"`) and degrade (502/503, `generated_by:
/// "deterministic_fallback"`) shapes — both are the same `DigestResponse`
/// JSON body on the wire (`daemon/manager/digest.rs`).
///
/// Why: the daemon's `DigestResponse` always carries `narrative` regardless
/// of status code (see module doc point 1), so one parse function handles
/// every case the CLI needs to render.
/// What: the narrative text and whether it is the deterministic fallback.
/// Test: `manager_digest_outcome_reads_real_daemon_shape`,
/// `manager_digest_outcome_marks_deterministic_fallback`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerDigestOutcome {
    /// The narrative text (LLM-authored or the deterministic fallback prose).
    pub narrative: String,
    /// True when `generated_by == "deterministic_fallback"` (no inference
    /// provider configured, or the LLM call failed) — DOC-16 D1.
    pub fallback: bool,
    /// The raw response body, kept for `--json` passthrough.
    pub raw: Value,
}

impl ManagerDigestOutcome {
    /// Build from a `DigestResponse`-shaped body. Returns `None` when the
    /// body carries no `narrative` field at all (not this response shape).
    fn from_body(raw: Value) -> Option<Self> {
        let narrative = raw.get("narrative").and_then(Value::as_str)?.to_string();
        let fallback = raw.get("generated_by").and_then(Value::as_str) == Some(GENERATED_BY_FALLBACK);
        Some(Self {
            narrative,
            fallback,
            raw,
        })
    }
}

/// Parsed result of `POST /api/v1/manager/chat`, covering BOTH the success
/// shape (`ChatReplyBody` — `reply`/`conversation_key`/`model`/`turn_count`)
/// and the degrade shape (`chat_error`'s `{ error, message }`, sent on
/// 400/502/503 — `daemon/manager/chat.rs`). There is no deterministic
/// fallback reply to synthesize for chat (unlike digest), so on degrade the
/// daemon's actionable `message` text is surfaced as [`Self::reply`] rather
/// than discarded — the CLI still has something useful to print.
///
/// Why: one parse function for both shapes keeps `manager_chat` from ever
/// needing to special-case status codes beyond the 404 mount-detection (see
/// module doc point 2).
/// What: the reply/message text and the conversation key (echoed by the
/// daemon on success, falls back to the requested key on degrade — the
/// error body never carries one).
/// Test: `manager_chat_outcome_reads_real_daemon_shape`,
/// `manager_chat_outcome_surfaces_error_message_on_degrade`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerChatOutcome {
    /// The assistant's reply (success) or the daemon's actionable degrade
    /// message (`chat_error`'s `message` field).
    pub reply: String,
    /// The conversation key this turn was recorded under.
    pub conversation_key: String,
    /// The raw response body, kept for `--json` passthrough.
    pub raw: Value,
}

impl ManagerChatOutcome {
    /// Build from a `ChatReplyBody`- or `chat_error`-shaped body. Returns
    /// `None` when the body carries neither a `reply` nor a `message` field
    /// (not one of chat's two response shapes).
    fn from_body(raw: Value, requested_key: &str) -> Option<Self> {
        let reply = raw
            .get("reply")
            .and_then(Value::as_str)
            .or_else(|| raw.get("message").and_then(Value::as_str))?
            .to_string();
        let conversation_key = raw
            .get("conversation_key")
            .and_then(Value::as_str)
            .unwrap_or(requested_key)
            .to_string();
        Some(Self {
            reply,
            conversation_key,
            raw,
        })
    }
}

/// Format a non-JSON (or unrecognized-shape) error body for `anyhow::bail!`.
///
/// Why: `digest.rs`'s 400 (`invalid scope`) and 404 (`project '{name}' is not
/// registered`) responses are plain text, not JSON — `(StatusCode,
/// String)::into_response()` — so there is no `error`/`message` field to
/// prefer; the raw trimmed body IS the daemon's message.
/// What: `"{status}: {trimmed body}"`, or bare `"{status}"` on an empty body.
fn daemon_error_text(status: StatusCode, body_text: &str) -> String {
    let trimmed = body_text.trim();
    if trimmed.is_empty() {
        status.to_string()
    } else {
        format!("{status}: {trimmed}")
    }
}

impl DaemonClient {
    /// Fetch the deterministic portfolio rollup via `GET /api/v1/manager/status`.
    ///
    /// Why: backs `tm manager status` (#2583). No LLM call, no channel/bot
    /// token — DOC-36 §4's local-testability bar. This endpoint has been live
    /// since WI-2 (#2579, PR #2598), so a 404 here is a genuine "unreachable
    /// or ancient daemon" condition and surfaces as an ordinary `Err` via
    /// [`response_or_body_error`], unlike the feature-detected 404 handling
    /// on [`Self::manager_digest`]/[`Self::manager_chat`].
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

    /// Whether `GET /api/v1/manager/version` reports `path` as `available`.
    ///
    /// Why: distinguishes "this daemon predates the route" from "the route
    /// exists but rejected this particular request" (module doc point 2) —
    /// called ONLY when a `manager_digest`/`manager_chat` call actually hits a
    /// `404`, so the common case (route mounted, request succeeds or degrades
    /// via a real body) never pays for the extra round trip. Conservative on
    /// any probe failure (network error, non-2xx, unparseable body, missing
    /// entry): treats the endpoint as unavailable, which routes the caller to
    /// the friendlier "upgrade your daemon" message rather than a confusing
    /// probe-failure error.
    /// What: `GET`s `/api/v1/manager/version`, reads the `endpoints` array,
    /// returns `true` iff an entry has `path == path` and `available == true`.
    /// Test: exercised indirectly by
    /// `manager_digest_and_chat_degrade_cleanly_on_404_against_older_daemon`
    /// (unavailable path) and
    /// `manager_digest_client_surfaces_unknown_project_404_against_real_daemon`
    /// (available path) in `crates/trusty-mpm/tests/manager_cli_client.rs`.
    async fn manager_endpoint_available(&self, path: &str) -> bool {
        let url = format!("{}/api/v1/manager/version", self.base);
        let Ok(resp) = self.http.get(&url).send().await else {
            return false;
        };
        if !resp.status().is_success() {
            return false;
        }
        let Ok(body) = resp.json::<Value>().await else {
            return false;
        };
        body.get("endpoints")
            .and_then(Value::as_array)
            .is_some_and(|endpoints| {
                endpoints.iter().any(|ep| {
                    ep.get("path").and_then(Value::as_str) == Some(path)
                        && ep.get("available").and_then(Value::as_bool) == Some(true)
                })
            })
    }

    /// Fetch the portfolio (or single-project) digest via
    /// `GET /api/v1/manager/digest?scope=<scope>`.
    ///
    /// Why: backs `tm manager digest` (#2583), against the shipped
    /// `daemon/manager/digest.rs` contract (WI-3, #2580, PR #2601). See the
    /// module doc's two review-found behaviors: the response body is parsed
    /// BEFORE any status-code branching (the daemon's 502/503 degrade bodies
    /// are full `DigestResponse`s, not bare errors), and a `404` is
    /// feature-detected against `GET /manager/version` rather than assumed to
    /// mean "old daemon" outright (digest.rs also 404s a genuine
    /// `scope=project:<name>` naming an unregistered project).
    /// What: parses the body as `DigestResponse`-shaped on ANY status code;
    /// `Ok(Some(outcome))` when it is; otherwise, on `404`, `Ok(None)` when
    /// [`Self::manager_endpoint_available`] reports the route unmounted, else
    /// `Err` carrying the daemon's own 404 message; on any other
    /// unrecognized-shape non-2xx, `Err` carrying the body text.
    /// Test: `manager_digest_outcome_reads_real_daemon_shape`,
    /// `manager_digest_outcome_marks_deterministic_fallback`; live HTTP in
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
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();

        // The daemon sends a full DigestResponse body (narrative + status +
        // generated_by) on 200 AND on the 502/503 degrade paths — never
        // discard it just because the status isn't 2xx.
        if let Ok(body) = serde_json::from_str::<Value>(&body_text)
            && let Some(outcome) = ManagerDigestOutcome::from_body(body)
        {
            return Ok(Some(outcome));
        }

        if status == StatusCode::NOT_FOUND {
            if !self.manager_endpoint_available("/api/v1/manager/digest").await {
                return Ok(None);
            }
            anyhow::bail!(daemon_error_text(status, &body_text));
        }

        anyhow::bail!(daemon_error_text(status, &body_text))
    }

    /// Send one chat turn via `POST /api/v1/manager/chat`.
    ///
    /// Why: backs `tm manager chat` (#2583), against the shipped
    /// `daemon/manager/chat.rs` contract (WI-4, #2581, PR #2601). Same
    /// body-first / feature-detected-404 handling as [`Self::manager_digest`]
    /// (module doc), adapted for chat's two shapes: `ChatReplyBody` on
    /// success, `chat_error`'s `{ error, message }` on 400/502/503 (whose
    /// `message` is surfaced as the outcome's `reply` — there's no assistant
    /// text to synthesize on a degrade, but the daemon's actionable message
    /// is still worth printing rather than discarding). Uses the same longer
    /// chat-scoped timeout [`super::config::CHAT_REQUEST_TIMEOUT`] the
    /// existing `llm_chat`/`coordinator_chat` methods use, since this also
    /// waits on the daemon's own upstream LLM round trip (DOC-36 §3.3).
    /// What: POSTs `{ conversation_key, message }`; parses the body as
    /// chat-shaped on ANY status code; `Ok(None)` on `404` ONLY when
    /// [`Self::manager_endpoint_available`] reports the route unmounted;
    /// `Err` otherwise (carrying the daemon's body message).
    /// Test: `manager_chat_outcome_reads_real_daemon_shape`,
    /// `manager_chat_outcome_surfaces_error_message_on_degrade`; live HTTP in
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
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();

        // chat.rs's `chat_error` sends `{ error, message }` JSON on
        // 400/502/503 — a real, actionable body, never discard it.
        if let Ok(body) = serde_json::from_str::<Value>(&body_text)
            && let Some(outcome) = ManagerChatOutcome::from_body(body, conversation_key)
        {
            return Ok(Some(outcome));
        }

        if status == StatusCode::NOT_FOUND {
            if !self.manager_endpoint_available("/api/v1/manager/chat").await {
                return Ok(None);
            }
            anyhow::bail!(daemon_error_text(status, &body_text));
        }

        anyhow::bail!(daemon_error_text(status, &body_text))
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

    /// A 200 `DigestResponse` (`generated_by: "llm"`) — the real success
    /// shape from `daemon/manager/digest.rs`.
    #[test]
    fn manager_digest_outcome_reads_real_daemon_shape() {
        let body = serde_json::json!({
            "scope": "portfolio",
            "generated_by": "llm",
            "model": "anthropic/claude-3-5-haiku",
            "narrative": "Three projects have active sessions; widget is blocked on review.",
            "status": { "project_count": 3, "totals": {}, "projects": [] },
        });
        let outcome = ManagerDigestOutcome::from_body(body).expect("DigestResponse shape");
        assert_eq!(
            outcome.narrative,
            "Three projects have active sessions; widget is blocked on review."
        );
        assert!(!outcome.fallback);
    }

    /// A 502/503 `DigestResponse` degrade body (`generated_by:
    /// "deterministic_fallback"`, plus `error`/`message`) — the real
    /// no-provider / inference-failed shape. This is the case #2600's
    /// original `fallback: bool` read could never detect (dead code, since
    /// the daemon has no such field) — the fix reads `generated_by` instead.
    #[test]
    fn manager_digest_outcome_marks_deterministic_fallback() {
        let body = serde_json::json!({
            "scope": "portfolio",
            "generated_by": "deterministic_fallback",
            "narrative": "[deterministic fallback — no inference provider configured] portfolio rollup:\n- Projects: 1",
            "status": { "project_count": 1, "totals": {}, "projects": [] },
            "error": "inference_unavailable",
            "message": "no inference provider is configured",
        });
        let outcome = ManagerDigestOutcome::from_body(body).expect("DigestResponse shape");
        assert!(outcome.fallback);
        assert!(outcome.narrative.starts_with("[deterministic fallback"));
    }

    /// A body with no `narrative` field at all (e.g. digest's plain-text
    /// 400/404 bodies) is not this shape.
    #[test]
    fn manager_digest_outcome_none_for_non_digest_shape() {
        assert!(ManagerDigestOutcome::from_body(serde_json::json!({ "error": "nope" })).is_none());
    }

    /// A 200 `ChatReplyBody` — the real success shape from
    /// `daemon/manager/chat.rs`.
    #[test]
    fn manager_chat_outcome_reads_real_daemon_shape() {
        let body = serde_json::json!({
            "conversation_key": "cli:alice",
            "reply": "Everything looks healthy right now.",
            "model": "anthropic/claude-3-5-haiku",
            "turn_count": 2,
        });
        let outcome =
            ManagerChatOutcome::from_body(body, "cli:alice").expect("ChatReplyBody shape");
        assert_eq!(outcome.reply, "Everything looks healthy right now.");
        assert_eq!(outcome.conversation_key, "cli:alice");
    }

    /// `chat_error`'s degrade shape (`{ error, message }`, no `reply`, no
    /// `conversation_key`) — the daemon's actionable `message` surfaces as
    /// the outcome's `reply`, and the conversation key falls back to the one
    /// the request sent (the error body never echoes one).
    #[test]
    fn manager_chat_outcome_surfaces_error_message_on_degrade() {
        let body = serde_json::json!({
            "error": "inference_unavailable",
            "message": "no inference provider is configured",
        });
        let outcome =
            ManagerChatOutcome::from_body(body, "cli:bob").expect("chat_error shape");
        assert_eq!(outcome.reply, "no inference provider is configured");
        assert_eq!(outcome.conversation_key, "cli:bob");
    }

    /// A body with neither `reply` nor `message` is not one of chat's shapes.
    #[test]
    fn manager_chat_outcome_none_for_non_chat_shape() {
        assert!(ManagerChatOutcome::from_body(serde_json::json!({ "ok": true }), "k").is_none());
    }
}
