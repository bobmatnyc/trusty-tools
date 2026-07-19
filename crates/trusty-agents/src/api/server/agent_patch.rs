//! `PATCH /api/agents/:name` — persist per-agent model/provider override
//! (#3246, epic #3052).
//!
//! Why: `GET /api/agents` (`super::projects::list_agents_route`) has been
//! read-only since #407; the create/edit UI merged in #3279 and the
//! `/api/models` catalog from #3243 both assume a write path exists to
//! actually save a user's model/provider choice per agent. Without this
//! route the picker is decorative — nothing persists past the current
//! process. Writing through [`toml_edit`] (already a workspace dependency,
//! previously unused in this crate) rather than a full
//! deserialize-mutate-reserialize round trip via `toml::Value` preserves
//! every other key, comment, and the original key ordering in the agent's
//! TOML file — important because these files are hand-authored and often
//! carry large prose blocks (system prompts) and inline comments (see
//! `.trusty-agents/agents/cto-assistant.toml`) that a naive round trip would
//! silently discard.
//! What: [`patch_agent_route`] extracts `:name` and a [`PatchAgentRequest`]
//! body, then delegates to [`patch_agent_at`] (the testable core) which:
//! validates the request isn't empty, resolves `provider_id` against
//! [`trusty_common::inference::registry`], rejects a model/provider that
//! conflicts with the agent's existing `runner` (the concrete case the issue
//! calls out: `runner = "claude-code"` requires an Anthropic-resolving
//! model), edits the `[agent]` table in place, writes the file back, and
//! returns the updated agent via [`super::projects::parse_agent_toml`] — the
//! same shape `GET /api/agents` returns, so a client can round-trip through
//! either route.
//! Test: `super::tests::agent_patch` — persists + round-trips a model
//! change, rejects an unknown agent (404), an empty body (400), an unknown
//! `provider_id` (400), and a claude-code/non-Anthropic conflict (400).

use std::path::Path;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use toml_edit::{DocumentMut, value};
use trusty_common::inference::registry::{self, ProviderId};

use super::handlers::agents_dir;
use super::projects::parse_agent_toml;
use super::state::AppState;

/// Request body for `PATCH /api/agents/:name` (#3246).
///
/// Why: The create/edit UI (#3279) and any future CLI/programmatic caller
/// need a minimal, additive body — every field is optional so a caller can
/// patch just the model, just the provider, or both in one call. At least
/// one of the two must be present; an all-`None` body is rejected as a
/// no-op (see [`patch_agent_at`]).
/// What: `model_id`, when present, is written verbatim into `[agent].model`.
/// `provider_id`, when present, must resolve via
/// [`registry::capabilities_for`]; it is written into `[agent].provider_id`
/// (a new, additive TOML key — the full-fidelity `AgentInfo` loader ignores
/// unknown keys, so this does not require a schema migration) and, when
/// `model_id` is absent, also supplies the provider's default model.
/// Test: `patch_agent_persists_model_and_round_trips`,
/// `patch_agent_provider_only_uses_default_model`.
#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct PatchAgentRequest {
    #[serde(default)]
    pub(super) model_id: Option<String>,
    #[serde(default)]
    pub(super) provider_id: Option<String>,
}

/// Build a `400 Bad Request` JSON error response.
fn bad_request(msg: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": msg.into() })),
    )
        .into_response()
}

/// `PATCH /api/agents/:name` — HTTP entry point.
///
/// Why: Thin axum glue over [`patch_agent_at`], which does the real work
/// against an injectable directory so it can be unit-tested without
/// depending on the process cwd.
/// What: Resolves the on-disk agents directory via [`agents_dir`] and
/// delegates.
/// Test: `super::tests::agent_patch` drives this through the full router.
pub(super) async fn patch_agent_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Json(req): Json<PatchAgentRequest>,
) -> Response {
    patch_agent_at(&agents_dir(), &name, req).await
}

/// Core PATCH logic against an explicit agents directory.
///
/// Why: Extracted so tests can drive it against a `tempfile::TempDir`
/// without mutating the process-global cwd (mirrors the
/// `scan_agents_dir`/`load_sessions_from` pattern already used by the
/// sibling listing routes).
/// What: See the module-level doc for the full validation + write sequence.
/// Postconditions: on `200 OK`, the file at `dir/<name>.toml` has been
/// rewritten with the requested field(s) updated and every other key/comment
/// preserved; the response body is [`parse_agent_toml`]'s view of that same
/// file, so an immediate follow-up read observes an identical result
/// (round-trip).
/// Test: `super::tests::agent_patch::*`.
pub(super) async fn patch_agent_at(dir: &Path, name: &str, req: PatchAgentRequest) -> Response {
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return bad_request("invalid agent name");
    }
    if req.model_id.is_none() && req.provider_id.is_none() {
        return bad_request("request body must set at least one of model_id/provider_id");
    }

    let path = dir.join(format!("{name}.toml"));
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "unknown agent", "name": name })),
            )
                .into_response();
        }
        Err(e) => {
            tracing::warn!(?e, agent = name, path = %path.display(), "patch_agent: read failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "failed to read agent config" })),
            )
                .into_response();
        }
    };

    let mut doc: DocumentMut = match raw.parse() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(
                ?e,
                agent = name,
                "patch_agent: existing TOML failed to parse"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "existing agent config is not valid TOML" })),
            )
                .into_response();
        }
    };

    // Resolve + validate provider_id, if given, against the registry (I1/#3243).
    let provider_cap = match req.provider_id.as_deref() {
        Some(pid) => match registry::capabilities_for(pid) {
            Some(cap) => Some(cap),
            None => return bad_request(format!("unknown provider_id '{pid}'")),
        },
        None => None,
    };

    // The model string to persist: explicit model_id wins; a provider_id
    // given alone falls back to that provider's default model so "just pick
    // a provider" is a valid, complete request.
    let model_to_write: Option<String> = req
        .model_id
        .clone()
        .or_else(|| provider_cap.map(|cap| cap.default_model.to_string()));

    // Resolve the provider implied by the write, for the runner-constraint
    // check below: explicit provider_id first, else whatever the model's
    // slug prefix implies (`openai/…`, `bedrock/…`, …); a bare slug with no
    // recognised prefix and no explicit provider_id can't be resolved here,
    // so the conflict check is skipped for it (fail-open — no known
    // conflict, not "assume compatible").
    let resolved_provider: Option<ProviderId> = provider_cap.map(|cap| cap.id).or_else(|| {
        model_to_write
            .as_deref()
            .and_then(ProviderId::from_slug_prefix)
    });

    let current_runner = doc
        .get("agent")
        .and_then(|a| a.get("runner"))
        .and_then(|v| v.as_str())
        .unwrap_or("subprocess")
        .to_string();

    // The one concrete rejection rule the issue calls out: the `claude-code`
    // runner spawns the local `claude` CLI, which only ever talks to
    // Anthropic — an OpenAI/Bedrock/Fireworks/etc. model would silently fail
    // at dispatch time, so reject it here instead.
    if current_runner == "claude-code"
        && let Some(pid) = resolved_provider
        && pid != ProviderId::Anthropic
    {
        return bad_request(format!(
            "runner 'claude-code' only supports Anthropic models; resolved provider '{}' is incompatible",
            pid.as_str()
        ));
    }

    let Some(agent_table) = doc
        .get_mut("agent")
        .and_then(|item| item.as_table_like_mut())
    else {
        tracing::warn!(agent = name, "patch_agent: TOML missing [agent] table");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "agent config is missing its [agent] table" })),
        )
            .into_response();
    };

    if let Some(model) = &model_to_write {
        agent_table.insert("model", value(model.clone()));
    }
    if let Some(pid) = &req.provider_id {
        agent_table.insert("provider_id", value(pid.clone()));
    }

    if let Err(e) = tokio::fs::write(&path, doc.to_string()).await {
        tracing::warn!(?e, agent = name, path = %path.display(), "patch_agent: write failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "failed to persist agent config" })),
        )
            .into_response();
    }

    let fallback_name = name.to_string();
    match parse_agent_toml(&doc.to_string(), &fallback_name) {
        Some(v) => (StatusCode::OK, Json(v)).into_response(),
        None => {
            // Unreachable in practice — we just wrote `doc` ourselves — but
            // handled explicitly rather than unwrapped per the no-`unwrap()`
            // library convention.
            tracing::warn!(
                agent = name,
                "patch_agent: failed to re-parse just-written TOML"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "failed to reload updated agent config" })),
            )
                .into_response()
        }
    }
}
