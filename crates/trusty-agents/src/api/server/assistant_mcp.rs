//! `GET`/`PUT /api/assistants/:id/mcp` — the two MCP tiers for one assistant
//! (#7454, ADR-0060, product spec §5.4).
//!
//! Why: a per-assistant MCP override is a USER setting, so it needs a write
//! route rather than only a hand-edited `[mcp]` table. And it needs a read
//! route that reports all THREE views, not just the stored one: an override is
//! a delta, so a pane showing only `disabled = ["github"]` cannot tell the user
//! what their assistant actually connects to. `global`, `overrides` and
//! `resolved` together are what makes the difference legible.
//!
//! What: [`get`] answers those three plus per-server `statuses` and any
//! `issues` with either tier's file. [`put`] replaces the `[mcp]` table.
//! Validation is the write side's job, deliberately — the resolver DROPS a
//! stale entry so a chat turn never breaks, while this route REFUSES a bad one
//! at the moment the user writes it, which is the only point where they can be
//! told why.
//!
//! Test: `super::tests::assistant_mcp`.

use std::path::{Path, PathBuf};

use axum::{
    Json,
    extract::Path as AxumPath,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use trusty_mcp::config::McpServerConfig;

use crate::assistants::mcp::{McpOverrides, read_overrides, write_overrides};
use crate::assistants::{AssistantHome, AssistantInstanceId, discover_instances};
use crate::mcp::shared::{GlobalTier, ResolvedMcp, load_global, resolve_in};

/// The `PUT` body: the whole `[mcp]` table, replaced wholesale.
///
/// Why replace rather than patch: the disable list is a SET the user edits in
/// one control, so a partial update has no meaning — "remove the last entry"
/// and "send no entries" would be indistinguishable.
/// Test: `put_replaces_the_whole_table`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpBody {
    #[serde(default)]
    servers: Vec<McpServerConfig>,
    #[serde(default)]
    disabled: Vec<String>,
}

/// `GET /api/assistants/:id/mcp` — HTTP entry point.
/// Test: `super::tests::assistant_mcp::mcp_route_is_wired_into_the_router`.
pub(super) async fn get(AxumPath(name): AxumPath<String>) -> Response {
    match live_roots() {
        Ok((dirs, root)) => read_at(&dirs, &root, &name, load_tier()),
        Err(response) => response,
    }
}

/// `PUT /api/assistants/:id/mcp` — HTTP entry point.
/// Test: `super::tests::assistant_mcp::put_refuses_a_duplicate_name`.
pub(super) async fn put(AxumPath(name): AxumPath<String>, Json(body): Json<McpBody>) -> Response {
    match live_roots() {
        Ok((dirs, root)) => write_at(&dirs, &root, &name, body, load_tier()),
        Err(response) => response,
    }
}

/// The global tier, or an empty one when no path resolves.
///
/// Why: no home directory is a degraded environment, not a 500 — the route
/// still answers with this assistant's own overrides and an empty global list.
fn load_tier() -> GlobalTier {
    load_global().unwrap_or_default()
}

/// The live agent directories and assistants root, or the response to send
/// when the root cannot be resolved (no `$HOME`).
fn live_roots() -> Result<(Vec<PathBuf>, PathBuf), Response> {
    let root = crate::assistants::assistants_root()
        .map_err(|e| fail(StatusCode::SERVICE_UNAVAILABLE, e))?;
    Ok((crate::agents::agents_dir_candidates(), root))
}

/// One error body, in this server's `{"error": …}` shape.
fn fail(status: StatusCode, message: impl std::fmt::Display) -> Response {
    (status, Json(json!({ "error": message.to_string() }))).into_response()
}

/// Validate `name` as an existing assistant instance.
///
/// Why: the id becomes a directory name under the assistants root, and the
/// setting belongs to user-facing Assistants only. Same membership rule as
/// `super::assistant_memory`.
/// Test: `get_rejects_an_unknown_assistant`.
fn instance(dirs: &[PathBuf], name: &str) -> Result<AssistantInstanceId, Response> {
    let id = AssistantInstanceId::new(name).map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    if id.as_str() != name || !discover_instances(dirs).contains(&id) {
        return Err(fail(StatusCode::NOT_FOUND, "Assistant not found"));
    }
    Ok(id)
}

/// The three views plus the statuses and the issues.
///
/// Why: see the module doc — an override is a delta, so the pane needs the
/// list it applies to as well as the result.
/// Test: `get_reports_global_overrides_and_resolved`.
pub(super) fn read_at(dirs: &[PathBuf], root: &Path, name: &str, global: GlobalTier) -> Response {
    let id = match instance(dirs, name) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let home = AssistantHome::under(root, id.clone());
    let read = read_overrides(&home);
    let error = read.error.map(|detail| (read.path, detail));
    let resolved = resolve_in(Some(id.as_str()), global, read.overrides, error);
    Json(body_for(&resolved)).into_response()
}

/// Replace the `[mcp]` table after validating every entry.
///
/// Why/What: see the module doc. Three refusals, each naming what to change: a
/// blank name (unaddressable — overrides match by name), a name repeated
/// within `servers` (the shared file's own rule, and the resolver would keep
/// only one), and a name in `servers` that is ALSO in `disabled` when the user
/// probably meant one or the other. The last is the only one the resolver
/// could answer on its own; refusing it here means the user finds out now
/// rather than wondering later which instruction won.
/// Test: `put_replaces_the_whole_table`, `put_refuses_a_blank_name`,
/// `put_refuses_a_duplicate_name`, `put_refuses_a_name_in_both_lists`.
pub(super) fn write_at(
    dirs: &[PathBuf],
    root: &Path,
    name: &str,
    body: McpBody,
    global: GlobalTier,
) -> Response {
    let id = match instance(dirs, name) {
        Ok(id) => id,
        Err(response) => return response,
    };

    let mut seen: Vec<&str> = Vec::with_capacity(body.servers.len());
    for server in &body.servers {
        let trimmed = server.name.trim();
        if trimmed.is_empty() {
            return fail(
                StatusCode::BAD_REQUEST,
                "every MCP server needs a name; overrides match the global list by name",
            );
        }
        if seen.contains(&trimmed) {
            return fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("MCP server `{trimmed}` is listed twice; each name may appear once"),
            );
        }
        seen.push(trimmed);
    }

    let mut disabled: Vec<String> = Vec::with_capacity(body.disabled.len());
    for raw in &body.disabled {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return fail(
                StatusCode::BAD_REQUEST,
                "a disabled entry names a server; it cannot be blank",
            );
        }
        if seen.contains(&trimmed) {
            return fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "MCP server `{trimmed}` is both configured and disabled here; \
                     remove it from one of the two lists"
                ),
            );
        }
        if !disabled.iter().any(|d| d == trimmed) {
            disabled.push(trimmed.to_string());
        }
    }

    let overrides = McpOverrides {
        servers: body.servers,
        disabled,
    };
    let home = AssistantHome::under(root, id.clone());
    if let Err(e) = home
        .ensure()
        .and_then(|_| write_overrides(&home, &overrides))
    {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    let resolved = resolve_in(Some(id.as_str()), global, overrides, None);
    Json(body_for(&resolved)).into_response()
}

/// The response body shared by both routes.
fn body_for(resolved: &ResolvedMcp) -> Value {
    let issues: Vec<Value> = resolved
        .issues
        .iter()
        .map(|issue| {
            json!({
                "tier": issue.tier,
                "path": issue.path.display().to_string(),
                "detail": issue.detail,
                "remedy": issue.remedy,
            })
        })
        .collect();
    json!({
        "assistant": resolved.assistant,
        "global": resolved.global,
        "overrides": {
            "servers": resolved.overrides.servers,
            "disabled": resolved.overrides.disabled,
        },
        "resolved": resolved.servers,
        "statuses": resolved.statuses,
        "issues": issues,
    })
}
