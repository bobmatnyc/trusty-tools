//! `POST /api/v1/manager/route-task` — resolver input + disambiguation judgment
//! (WI-8, #2585, epic #2109, DOC-36 phase 2).
//!
//! Why: DOC-36 §3.2 gives `tm manager` a `route-task` primitive that maps a
//! free-text task to the project it belongs to. Per DOC-35 §13 Q1's SPLIT
//! decision the deterministic name/URL/keyword MATCHING primitive stays owned by
//! DOC-22 (`crate::project::resolver::resolve_project`); #2109 owns only the
//! DISAMBIGUATION JUDGMENT layered on top — i.e. what to do when the resolver
//! reports a tie/low-confidence via [`ProjectResolution::needs_disambiguation`].
//! This endpoint therefore CONSUMES the resolver as an input signal, and escalates
//! to a single LLM call ONLY when the resolver is genuinely ambiguous; an
//! unambiguous resolution passes through with zero inference. Critically it is
//! ADVISORY: it resolves a route and returns `{ project, confidence, rationale }`
//! but NEVER launches, injects, or mutates a session — acting on the route is a
//! separate explicit call (the WI-9 proposal-and-confirm flow, `/manager/act`),
//! satisfying DOC-35 §11's "no acting without an explicit call". Curl-testable
//! with no channel/bot token (§4): a no-provider environment simply skips the
//! disambiguation LLM call and returns the deterministic top candidate.
//! What: [`RouteTaskRequest`]/[`RouteTaskResponse`]/[`ResolvedBy`], the pure
//! disambiguation helpers [`build_disambiguation_messages`] and [`pick_from_reply`],
//! and the [`manager_route_task_route`] handler.
//! Test: `pick_from_reply_*`, `build_disambiguation_messages_lists_candidates`,
//! `route_from_resolution_*` in `route_task_tests.rs`; HTTP coverage (unambiguous
//! pass-through, LLM disambiguation, no-provider degrade, no-match) in
//! `tests/manager_routing.rs`.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use serde_json::json;
use trusty_common::inference::{ChatMessage, ChatRequest};

use super::inference::{MANAGER_MAX_TOKENS, MANAGER_TEMPERATURE};
use crate::daemon::state::DaemonState;
use crate::project::resolver::{ProjectMatch, ProjectResolution, ResolverError, resolve_project};

/// Request body for `POST /api/v1/manager/route-task`.
///
/// Why: DOC-36 §3.2 fixes the shape `{ text }` — a single free-text task
/// description the resolver maps to a project.
/// What: the caller-supplied task text.
/// Test: HTTP coverage in `tests/manager_routing.rs`.
#[derive(Debug, Deserialize)]
pub struct RouteTaskRequest {
    /// The free-text task to route to a project.
    pub text: String,
}

/// How the returned route was decided.
///
/// Why: makes the DOC-35 §13 Q1 split observable — a consumer (and the WI-9
/// proposal flow) can see whether the route came straight from the deterministic
/// resolver, from an escalated LLM disambiguation judgment, or whether nothing
/// matched — without re-deriving that distinction. Also lets the hermetic suite
/// assert the escalation path fired only on a genuine tie.
/// What: `Resolver` for an unambiguous deterministic pick (no LLM call),
/// `Disambiguation` for an LLM-judged tie, `NoMatch` when the resolver found no
/// candidate.
/// Test: `route_from_resolution_unambiguous_is_resolver`,
/// `route_from_resolution_no_match`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedBy {
    /// Deterministic resolver pick, unambiguous — no inference used.
    Resolver,
    /// A tie the resolver flagged, judged by a single LLM disambiguation call.
    Disambiguation,
    /// No project matched the query at all.
    NoMatch,
}

/// Response body for `POST /api/v1/manager/route-task`.
///
/// Why: DOC-36 §3.2 + #2585 require `{ project, confidence, rationale }`; the
/// added `resolved_by` marker documents the decision path (deterministic vs.
/// judged vs. no-match) without changing the core contract. Advisory only — the
/// response never implies a session was touched.
/// What: the resolved project name (`None` on no-match), the clamped confidence
/// `[0.0, 1.0]`, a human-readable rationale, and the decision path.
/// Test: HTTP coverage in `tests/manager_routing.rs`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RouteTaskResponse {
    /// The resolved project name, or `None` when nothing matched.
    pub project: Option<String>,
    /// Confidence in the resolved route, clamped `[0.0, 1.0]` (0.0 on no-match).
    pub confidence: f32,
    /// Human-readable explanation of why this route was chosen.
    pub rationale: String,
    /// How the route was decided (deterministic / judged / no-match).
    pub resolved_by: ResolvedBy,
}

/// Build the LLM disambiguation prompt from the tied candidates.
///
/// Why: escalation is invoked ONLY when [`ProjectResolution::needs_disambiguation`]
/// is true, so the model's sole job is to pick exactly one of the already-scored
/// candidates for the given task — never to invent a project. Pinning the system
/// persona to "choose one of the listed candidates and reply with its exact name"
/// keeps [`pick_from_reply`] a simple, robust name match. Pure so it is testable
/// without a live model.
/// What: a system instruction plus a user message carrying the task text and the
/// numbered candidate list (name, confidence, and match reason for context).
/// Test: `build_disambiguation_messages_lists_candidates`.
pub fn build_disambiguation_messages(text: &str, candidates: &[ProjectMatch]) -> Vec<ChatMessage> {
    let mut listing = String::new();
    for (i, m) in candidates.iter().enumerate() {
        listing.push_str(&format!(
            "{}. {} (confidence {:.2}, matched by {})\n",
            i + 1,
            m.project.name,
            m.confidence,
            m.reason.label(),
        ));
    }
    let system = "You route a developer's free-text task to exactly ONE of a short list of \
         candidate projects that a deterministic matcher already scored as plausible but \
         could not decide between. Choose the single best-fitting project for the task. \
         Reply with ONLY that project's exact name from the list — no punctuation, no \
         explanation, no extra words. Never invent a project that is not listed.";
    let user =
        format!("Task: {text}\n\nCandidate projects:\n{listing}\nBest-fitting project name:");
    vec![ChatMessage::system(system), ChatMessage::user(user)]
}

/// Pick the candidate the model's reply names (case-insensitive).
///
/// Why: the disambiguation model is asked to reply with a bare project name, but
/// real replies may add stray whitespace/punctuation or echo the name inside a
/// sentence. Matching the reply against the KNOWN candidate names (rather than
/// trusting the raw text as a name) keeps the judgment constrained to the
/// resolver's candidate set — the model can never route to an unlisted project.
/// What: returns the first candidate whose name equals the trimmed reply
/// (case-insensitive), else the first candidate the reply CONTAINS as a substring
/// (longest name first, so a more specific name wins over a prefix), else `None`.
/// Test: `pick_from_reply_exact`, `pick_from_reply_substring`,
/// `pick_from_reply_unlisted_is_none`.
pub fn pick_from_reply<'a>(
    reply: &str,
    candidates: &'a [ProjectMatch],
) -> Option<&'a ProjectMatch> {
    let reply_lower = reply.trim().to_lowercase();
    if reply_lower.is_empty() {
        return None;
    }
    if let Some(exact) = candidates
        .iter()
        .find(|m| m.project.name.to_lowercase() == reply_lower)
    {
        return Some(exact);
    }
    // Substring fallback: prefer the longest candidate name so that when one name
    // is a prefix of another, the more specific one is chosen.
    let mut by_len: Vec<&ProjectMatch> = candidates.iter().collect();
    by_len.sort_by_key(|m| std::cmp::Reverse(m.project.name.len()));
    by_len
        .into_iter()
        .find(|m| reply_lower.contains(&m.project.name.to_lowercase()))
}

/// Turn a deterministic [`ProjectResolution`] into a response WITHOUT inference.
///
/// Why: the unambiguous path (single confident candidate, or a tie the caller
/// decides not to escalate) needs a zero-LLM answer that reuses the resolver's
/// own confidence and reason verbatim — never re-derived. Extracting it keeps the
/// handler thin and makes the deterministic mapping unit-testable.
/// What: takes the resolution's `primary` (highest-confidence) match and renders
/// it as a [`ResolvedBy::Resolver`] response; `note` is appended to the rationale
/// so a degraded-disambiguation caller can explain why it did not escalate.
/// Test: `route_from_resolution_unambiguous_is_resolver`.
fn resolver_response(resolution: &ProjectResolution, note: Option<&str>) -> RouteTaskResponse {
    match &resolution.primary {
        Some(primary) => {
            let mut rationale = format!(
                "resolved to '{}' by {} (confidence {:.2})",
                primary.project.name,
                primary.reason.label(),
                primary.confidence,
            );
            if let Some(note) = note {
                rationale.push_str("; ");
                rationale.push_str(note);
            }
            RouteTaskResponse {
                project: Some(primary.project.name.clone()),
                confidence: primary.confidence,
                rationale,
                resolved_by: ResolvedBy::Resolver,
            }
        }
        None => no_match_response("resolver returned no candidate"),
    }
}

/// Build the advisory no-match response.
///
/// Why: an unresolvable query is advisory, not an error — the caller decides what
/// to do (register a project, refine the query), so the endpoint returns 200 with
/// a null project rather than a 4xx.
/// What: a [`ResolvedBy::NoMatch`] response carrying `reason` as the rationale.
/// Test: `route_from_resolution_no_match`.
fn no_match_response(reason: &str) -> RouteTaskResponse {
    RouteTaskResponse {
        project: None,
        confidence: 0.0,
        rationale: reason.to_string(),
        resolved_by: ResolvedBy::NoMatch,
    }
}

/// `POST /api/v1/manager/route-task` handler.
///
/// Why: the curl-first (§4) task-routing surface. It runs DOC-22's deterministic
/// resolver for candidate scoring, and — because #2109 owns disambiguation —
/// escalates a genuine tie to ONE LLM judgment call, otherwise passes the
/// unambiguous pick straight through. It NEVER launches or mutates a session
/// (advisory only, DOC-35 §11).
/// What: validates `text` (400 on empty), lists registered projects, calls
/// [`resolve_project`]; an empty registry or no-match returns an advisory 200
/// no-match; an unambiguous resolution returns the resolver pick (no LLM); a tie
/// (`needs_disambiguation`) resolves via [`build_disambiguation_messages`] +
/// [`pick_from_reply`] over one grounded inference call, degrading to the
/// deterministic top candidate when no provider is configured or the call fails.
/// Never logs task text (privacy). Read-only: no session mutation.
/// Test: HTTP coverage in `tests/manager_routing.rs`.
pub async fn manager_route_task_route(
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<RouteTaskRequest>,
) -> impl IntoResponse {
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_request", "message": "text must not be empty" })),
        )
            .into_response();
    }

    let registry = state.project_registry().await;
    let projects = match registry.list().await {
        Ok(projects) => projects,
        Err(e) => {
            tracing::warn!(error = %e, "route-task: project registry read failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "registry_read_failed",
                             "message": "project registry read failed" })),
            )
                .into_response();
        }
    };

    let resolution = match resolve_project(&text, &projects) {
        Ok(resolution) => resolution,
        Err(ResolverError::EmptyRegistry) => {
            return Json(no_match_response(
                "no projects are registered; register one with `tm projects register`",
            ))
            .into_response();
        }
        Err(ResolverError::NoMatch { .. }) => {
            return Json(no_match_response("no registered project matched the task"))
                .into_response();
        }
    };

    // Unambiguous: reuse the deterministic pick verbatim — no inference.
    if !resolution.needs_disambiguation() {
        return Json(resolver_response(&resolution, None)).into_response();
    }

    // Tie/low-confidence: #2109 owns the judgment. Escalate to ONE LLM call.
    let (model, adapter) = match state.manager_state().inference().resolve() {
        Ok(pair) => pair,
        Err(_) => {
            // No provider — degrade to the deterministic top candidate rather
            // than fail; advisory output must never panic (§4 degrade bar).
            return Json(resolver_response(
                &resolution,
                Some(
                    "multiple candidates tied and no inference provider was available to \
                      disambiguate, so the highest-confidence candidate was chosen",
                ),
            ))
            .into_response();
        }
    };

    let candidates = &resolution.matches;
    let mut request = ChatRequest::new(
        model.clone(),
        build_disambiguation_messages(&text, candidates),
    );
    request.max_tokens = Some(MANAGER_MAX_TOKENS);
    request.temperature = Some(MANAGER_TEMPERATURE);

    let picked = match adapter.chat(&request).await {
        Ok(response) => response
            .first_text()
            .and_then(|reply| pick_from_reply(&reply, candidates).cloned()),
        Err(e) => {
            tracing::warn!("route-task disambiguation inference call failed: {e}");
            None
        }
    };

    match picked {
        Some(m) => Json(RouteTaskResponse {
            project: Some(m.project.name.clone()),
            confidence: m.confidence,
            rationale: format!(
                "disambiguated to '{}' by inference among {} tied candidates (resolver \
                 confidence {:.2}, matched by {})",
                m.project.name,
                candidates.len(),
                m.confidence,
                m.reason.label(),
            ),
            resolved_by: ResolvedBy::Disambiguation,
        })
        .into_response(),
        // The call failed or named an unlisted project — fall back to the
        // deterministic top candidate.
        None => Json(resolver_response(
            &resolution,
            Some("disambiguation was inconclusive, so the highest-confidence candidate was chosen"),
        ))
        .into_response(),
    }
}

#[cfg(test)]
#[path = "route_task_tests.rs"]
mod tests;
