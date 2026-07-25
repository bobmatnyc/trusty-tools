//! `PATCH /api/agents/:name` — persist per-agent config overrides
//! (model/provider #3246; personality/tools #3819, epic #3052).
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
//! [`trusty_common::inference::registry`] (falling back to whatever
//! `provider_id` is already on disk when the request doesn't supply one),
//! and rejects a model/provider that conflicts with the agent's existing
//! `runner`. For `runner = "claude-code"` this is a closed-world (fail-shut,
//! not fail-open) check: the local `claude` CLI only ever talks to
//! Anthropic, so the resolved provider must be [`ProviderId::Anthropic`] —
//! anything else, OR a provider that can't be resolved at all (a bare
//! `model_id` with no recognised `provider/` prefix and no `provider_id`
//! anywhere), is rejected with `400`. A bare non-Anthropic model_id with no
//! prefix would otherwise slip past a naive prefix-only check (see #3287
//! review) and persist a model the claude-code runner can never actually
//! dispatch. On success the handler edits the `[agent]` table in place,
//! writes the file back, and returns the updated agent via
//! [`super::projects::parse_agent_toml`] — the same shape `GET /api/agents`
//! returns, so a client can round-trip through either route.
//!
//! **#3819 additions (agent-pane gear panel):** [`resolve_agent_paths`] fixes
//! a pre-existing gap this endpoint had for every bundled demo agent
//! (`assistant`/`izzie`/`cto-assistant`/`ctrl`): they are all directory
//! PACKAGES (`<name>/agent.toml` + `<name>/persona.md`, higher-ranked than a
//! same-named flat `<name>.toml` shadow per `scan_agents_dir_tiered`'s
//! resolution order), but the pre-#3819 code unconditionally wrote
//! `dir/<name>.toml` — the LOWER-ranked flat shadow that is never actually
//! dispatched. Model/provider edits therefore silently landed on a file
//! nothing reads. `resolve_agent_paths` now prefers the package path when one
//! exists, so every field this endpoint writes (existing and new) lands on
//! the file that actually dispatches. Two new optional request fields:
//! `personality` (writes the sibling `persona.md` — package agents only,
//! `400` otherwise) and `tools_allow` (writes `[tools].allow`, the persona
//! glob tool-allowlist). Neither has a server-side backup-on-change step —
//! that convention is being established elsewhere for `~/.trusty-agents/`
//! writes generally; see the PR body for the follow-up. [`get_agent_persona_route`]
//! is the read counterpart the gear panel's Personality tab loads from.
//! Test: `super::tests::agent_patch` — persists + round-trips a model
//! change, rejects an unknown agent (404), an empty body (400), an unknown
//! `provider_id` (400), a claude-code/non-Anthropic conflict via an explicit
//! prefix (400), the same conflict via a *bare* unprefixed model_id (400,
//! the fail-shut case), the accepted Anthropic case, malformed on-disk TOML
//! (500), package-vs-flat path resolution, `tools_allow` round-trip, and
//! `personality` accept (package)/reject (flat-only) cases.

use std::path::PathBuf;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use toml_edit::{Array, DocumentMut, Item, Table, value};
use trusty_common::inference::registry::{self, ProviderId};

use super::projects::parse_agent_toml;
use super::state::AppState;

/// Resolve `name`'s on-disk config path within `dir`, preferring a directory
/// PACKAGE (`dir/<name>/agent.toml`) over a flat shadow (`dir/<name>.toml`) —
/// matching `scan_agents_dir_tiered`'s package-over-flat dispatch-resolution
/// order (see module doc comment for why this matters). Returns
/// `(toml_path, package_dir)`; `package_dir` is `Some` only for the package
/// case, and is what [`patch_agent_at`] uses to locate a sibling `persona.md`.
/// Returns `None` when neither a package nor a flat file exists for `name`.
/// Test: `patch_agent_prefers_package_over_flat_shadow`,
/// `patch_agent_prefers_first_tier_over_second`,
/// `get_agent_searches_second_tier_when_first_misses`,
/// `get_agent_route_unknown_agent_404` (the `None` case).
///
/// `dirs` is searched in order (project-local tier before `$HOME`, matching
/// [`crate::agents::agents_dir_candidates`]'s documented precedence) —
/// critical for the packaged desktop app, where the Tauri sidecar's cwd is
/// `/` (sealed volume, #3741): a SINGLE cwd-relative directory (the
/// pre-#3819 behavior) would never resolve ANY bundled agent there, since
/// every bundled agent lives in the `$HOME/.trusty-agents/agents` tier, not
/// a nonexistent `/.trusty-agents/agents`.
pub(super) fn resolve_agent_paths(
    dirs: &[PathBuf],
    name: &str,
) -> Option<(PathBuf, Option<PathBuf>)> {
    for dir in dirs {
        let package_dir = dir.join(name);
        let package_toml = package_dir.join("agent.toml");
        if package_toml.is_file() {
            return Some((package_toml, Some(package_dir)));
        }
        let flat = dir.join(format!("{name}.toml"));
        if flat.is_file() {
            return Some((flat, None));
        }
    }
    None
}

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
    /// #3819: overwrites the sibling `persona.md` for a directory-package
    /// agent. `None` for a flat (non-package) agent is rejected with `400` —
    /// see `resolve_agent_paths`'s doc comment for why only packages have a
    /// persona file to write.
    #[serde(default)]
    pub(super) personality: Option<String>,
    /// #3819: overwrites `[tools].allow` (the glob-style persona
    /// tool-allowlist, `crate::agents::config::ToolsConfig::allow`) with this
    /// exact list. An empty (non-null) array clears the allowlist entirely
    /// (falls through to "all tools", matching the field's own `None`
    /// semantics) rather than being ignored — the caller can tell the
    /// difference from omitting the field.
    #[serde(default)]
    pub(super) tools_allow: Option<Vec<String>>,
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
/// What: Resolves the on-disk agent directory CANDIDATES via
/// [`crate::agents::agents_dir_candidates`] (project-local tier, then
/// `$HOME/.trusty-agents/agents` — see [`resolve_agent_paths`]'s doc comment
/// for why a single cwd-relative directory is wrong for the packaged
/// desktop app) and delegates.
/// Test: `super::tests::agent_patch` drives this through the full router.
pub(super) async fn patch_agent_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Json(req): Json<PatchAgentRequest>,
) -> Response {
    patch_agent_at(&crate::agents::agents_dir_candidates(), &name, req).await
}

/// Core PATCH logic against explicit candidate agent directories.
///
/// Why: Extracted so tests can drive it against `tempfile::TempDir`s
/// without mutating the process-global cwd (mirrors the
/// `scan_agents_dir`/`load_sessions_from` pattern already used by the
/// sibling listing routes).
/// What: See the module-level doc for the full validation + write sequence.
/// Postconditions: on `200 OK`, the resolved file (see
/// [`resolve_agent_paths`]) has been rewritten with the requested field(s)
/// updated and every other key/comment preserved; the response body is
/// [`parse_agent_toml`]'s view of that same file, so an immediate follow-up
/// read observes an identical result (round-trip).
/// Test: `super::tests::agent_patch::*`.
pub(super) async fn patch_agent_at(
    dirs: &[PathBuf],
    name: &str,
    req: PatchAgentRequest,
) -> Response {
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return bad_request("invalid agent name");
    }
    if req.model_id.is_none()
        && req.provider_id.is_none()
        && req.personality.is_none()
        && req.tools_allow.is_none()
    {
        return bad_request(
            "request body must set at least one of model_id/provider_id/personality/tools_allow",
        );
    }

    let Some((path, package_dir)) = resolve_agent_paths(dirs, name) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "unknown agent", "name": name })),
        )
            .into_response();
    };
    // #3819: `personality` has nowhere to write without a package dir —
    // checked up front, before any file is touched, so a rejected request
    // never leaves a partial write behind.
    if req.personality.is_some() && package_dir.is_none() {
        return bad_request(format!(
            "agent '{name}' has no persona.md to edit (flat-file agents don't support a \
             personality field) — only directory-package agents (agent.toml + persona.md) do"
        ));
    }

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

    // #3819: the model/provider resolution + claude-code runner-constraint
    // check below only makes sense when the caller is actually touching
    // model/provider — skip it entirely for a request that only sets
    // `tools_allow`/`personality`, otherwise a claude-code agent would fail
    // every tools-only/personality-only patch with a spurious "requires an
    // unambiguous Anthropic-resolvable model" error (nothing about model
    // resolution changed; `model_to_write` would just be `None` and trip the
    // fail-shut branch below).
    if req.model_id.is_some() || req.provider_id.is_some() {
        // Resolve + validate an explicitly-requested provider_id against the
        // registry (I1/#3243) — an unknown value here is always a hard error,
        // it's the caller's own input.
        let requested_provider_cap = match req.provider_id.as_deref() {
            Some(pid) => match registry::capabilities_for(pid) {
                Some(cap) => Some(cap),
                None => return bad_request(format!("unknown provider_id '{pid}'")),
            },
            None => None,
        };

        // Fall back to whatever `provider_id` is already persisted on disk when
        // the request doesn't supply one — e.g. a follow-up patch that only
        // changes `model_id` shouldn't have to repeat a `provider_id` a prior
        // patch already established. A disk value that no longer resolves
        // (should not happen since writes always validate first, but is not a
        // caller input) is silently ignored rather than erroring the request.
        let existing_provider_id_str = doc
            .get("agent")
            .and_then(|a| a.get("provider_id"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let provider_cap = requested_provider_cap.or_else(|| {
            existing_provider_id_str
                .as_deref()
                .and_then(registry::capabilities_for)
        });

        // The model string to persist: explicit model_id wins; a provider_id
        // given alone falls back to that provider's default model so "just pick
        // a provider" is a valid, complete request.
        let model_to_write: Option<String> = req
            .model_id
            .clone()
            .or_else(|| provider_cap.map(|cap| cap.default_model.to_string()));

        // Resolve the provider implied by the write, for the runner-constraint
        // check below: explicit-or-existing provider_id first, else whatever the
        // model's slug prefix implies (`openai/…`, `bedrock/…`, …). A bare slug
        // with no recognised prefix and no provider_id anywhere resolves to
        // `None` here — see the runner check below for why that is NOT treated
        // as "no conflict" for claude-code agents.
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

        // The `claude-code` runner spawns the local `claude` CLI, which only
        // ever talks to Anthropic. This check is deliberately fail-shut, not
        // fail-open: an unresolved provider (a bare model_id with no recognised
        // prefix and no provider_id anywhere) is just as unacceptable as an
        // explicitly non-Anthropic one, because a bare "gpt-4o-mini"-style
        // model_id would otherwise silently persist and break at dispatch time.
        // A claude-code agent can only ever end up with an
        // Anthropic-*resolvable* model — ambiguous is rejected exactly like
        // wrong.
        if current_runner == "claude-code" {
            match resolved_provider {
                Some(ProviderId::Anthropic) => {}
                Some(pid) => {
                    return bad_request(format!(
                        "runner 'claude-code' only supports Anthropic models; resolved provider '{}' is incompatible",
                        pid.as_str()
                    ));
                }
                None => {
                    return bad_request(
                        "runner 'claude-code' requires an unambiguous Anthropic-resolvable model: \
                         pass an explicit provider_id (\"anthropic\") or a prefixed model_id \
                         (e.g. \"anthropic/claude-...\") — a bare, unprefixed model_id cannot be \
                         validated and is rejected rather than silently accepted",
                    );
                }
            }
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
    }

    // #3819: `tools_allow` overwrites `[tools].allow` — the glob-style
    // persona tool-allowlist (`ToolsConfig::allow`). Creates the `[tools]`
    // table if absent so a package with no prior `[tools]` section can still
    // gain one. An empty array is written as an empty TOML array (not
    // omitted) so the caller's explicit "clear the allowlist" is
    // distinguishable on the next `GET` from "never set".
    if let Some(tools_allow) = &req.tools_allow {
        let tools_table = doc
            .entry("tools")
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_like_mut();
        match tools_table {
            Some(tools_table) => {
                let mut arr = Array::new();
                for tool in tools_allow {
                    arr.push(tool.as_str());
                }
                tools_table.insert("allow", value(arr));
            }
            None => {
                tracing::warn!(
                    agent = name,
                    "patch_agent: [tools] exists but is not a table"
                );
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(
                        serde_json::json!({ "error": "agent config's [tools] key is not a table" }),
                    ),
                )
                    .into_response();
            }
        }
    }

    // #3819: `personality` overwrites the sibling `persona.md`. Validated
    // up front (package_dir.is_some() required) before any write happened,
    // so this is the only write touching a file other than `path` itself.
    if let Some(personality) = &req.personality {
        // Guarded above: personality.is_some() ⟹ package_dir.is_some().
        let persona_path = package_dir
            .as_ref()
            .expect("personality requires package_dir, checked above")
            .join("persona.md");
        if let Err(e) = tokio::fs::write(&persona_path, personality).await {
            tracing::warn!(
                ?e, agent = name, path = %persona_path.display(),
                "patch_agent: persona.md write failed"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "failed to persist persona.md" })),
            )
                .into_response();
        }
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

/// `GET /api/agents/:name` — HTTP entry point (#3819).
///
/// Why: `GET /api/agents` (`list_agents_route`) lists the picker's
/// SELECTABLE set, which a role filter (#3812, in flight alongside this PR)
/// narrows to `role == "assistant"` — deliberately excluding `ctrl`
/// (`role = "controller"`). The gear panel needs the Concierge/`ctrl`
/// agent's full config detail regardless of picker-list filtering (Bob:
/// "Concierge editable-by-name via API"), so it needs a single-agent read
/// path independent of the list route's filter. This is the read-only
/// sibling of [`patch_agent_route`] — same [`resolve_agent_paths`] +
/// [`parse_agent_toml`] machinery, no mutation.
/// What: `404` for an unknown name; `200` with the same JSON shape
/// `GET /api/agents`'s entries and `PATCH`'s response share.
/// Test: `super::tests::agent_patch::get_agent_route_*`.
pub(super) async fn get_agent_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    get_agent_at(&crate::agents::agents_dir_candidates(), &name).await
}

pub(super) async fn get_agent_at(dirs: &[PathBuf], name: &str) -> Response {
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return bad_request("invalid agent name");
    }
    let Some((path, _package_dir)) = resolve_agent_paths(dirs, name) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "unknown agent", "name": name })),
        )
            .into_response();
    };
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(?e, agent = name, path = %path.display(), "get_agent_at: read failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "failed to read agent config" })),
            )
                .into_response();
        }
    };
    match parse_agent_toml(&raw, name) {
        Some(v) => (StatusCode::OK, Json(v)).into_response(),
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "existing agent config is not valid TOML" })),
        )
            .into_response(),
    }
}

/// `GET /api/agents/:name/persona` — HTTP entry point (#3819).
///
/// Why: The gear-panel Personality tab needs the agent's actual `persona.md`
/// prose to populate its editor, not just the `[agent]` table `PATCH`
/// already round-trips. Split from `patch_agent_route` because it's a read,
/// not a write, and has its own response shape.
/// What: Resolves `:name` via [`resolve_agent_paths`] and delegates to
/// [`persona_at`].
/// Test: `super::tests::agent_patch::get_agent_persona_reads_package_persona`.
pub(super) async fn get_agent_persona_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    persona_at(&crate::agents::agents_dir_candidates(), &name).await
}

/// Core persona-read logic against an explicit agents directory.
///
/// Why: Same testability rationale as [`patch_agent_at`].
/// What: `404` for an unknown agent name (no package, no flat file at all).
/// For a resolved but non-package (flat-file) agent, returns `200` with
/// `content: null, editable: false` — a flat agent has no persona.md, but
/// that's a legitimate state, not an error, so the gear panel can render a
/// clear "not editable" notice rather than an error toast. For a package
/// agent, reads `persona.md` (empty string if the file is present but
/// somehow unreadable-as-missing is treated as `content: null` — see the
/// `NotFound` arm) and returns `editable: true`.
/// Test: `get_agent_persona_reads_package_persona`,
/// `get_agent_persona_not_editable_for_flat_agent`, `get_agent_persona_unknown_agent_404`.
pub(super) async fn persona_at(dirs: &[PathBuf], name: &str) -> Response {
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return bad_request("invalid agent name");
    }
    let Some((_path, package_dir)) = resolve_agent_paths(dirs, name) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "unknown agent", "name": name })),
        )
            .into_response();
    };
    let Some(package_dir) = package_dir else {
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "content": null, "editable": false })),
        )
            .into_response();
    };
    let persona_path = package_dir.join("persona.md");
    match tokio::fs::read_to_string(&persona_path).await {
        Ok(content) => (
            StatusCode::OK,
            Json(serde_json::json!({ "content": content, "editable": true })),
        )
            .into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
            StatusCode::OK,
            Json(serde_json::json!({ "content": null, "editable": true })),
        )
            .into_response(),
        Err(e) => {
            tracing::warn!(
                ?e, agent = name, path = %persona_path.display(),
                "persona_at: read failed"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "failed to read persona.md" })),
            )
                .into_response()
        }
    }
}
