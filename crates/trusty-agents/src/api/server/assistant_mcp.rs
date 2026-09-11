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
//! Every server this route RENDERS is redacted first ([`redacted`]): a stdio
//! server's `transport.env` and a remote one's `transport.headers` routinely
//! carry API keys and bearer tokens, and `McpTransport`'s `Serialize` is
//! deliberately unredacted so the config file round-trips. Serialising a
//! resolved server straight into the response would hand every one of those
//! secrets back over the API. Key NAMES survive, because which variables a
//! server needs is the diagnostic the pane renders; the values are replaced
//! with [`REDACTED`].
//!
//! The write side is the other half of that: `servers` is OPTIONAL, so a
//! client changing only the disable list never echoes a server back at all,
//! and a value that arrives still carrying [`REDACTED`] is refused rather than
//! written — persisting the marker would destroy the real credential.
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

/// What one hidden `env` or `headers` value renders as.
///
/// Why: a fixed marker rather than an empty string or a dropped key, so the
/// pane can show that a variable IS set without showing what it is, and so the
/// write side can recognise a round-tripped value and refuse it.
/// Test: `get_redacts_env_and_header_values`, `put_refuses_a_redacted_value`.
pub(super) const REDACTED: &str = "<redacted>";

/// The `PUT` body: the `[mcp]` table.
///
/// Why `disabled` replaces wholesale: it is a SET the user edits in one
/// control, so a partial update has no meaning — "remove the last entry" and
/// "send no entries" would be indistinguishable.
///
/// Why `servers` is optional: the GET that a client edits from carries
/// REDACTED transport secrets, so a body that always had to restate `servers`
/// would make the disable toggle round-trip a marker back into the file and
/// destroy the credential. Absent means "leave this assistant's own servers as
/// they are"; present still replaces them wholesale, for the same reason
/// `disabled` does.
/// Test: `put_replaces_the_whole_table`,
/// `put_without_servers_keeps_the_stored_ones`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpBody {
    #[serde(default)]
    servers: Option<Vec<McpServerConfig>>,
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
/// Why/What: see the module doc. Four refusals, each naming what to change: a
/// blank name (unaddressable — overrides match by name), a name repeated
/// within `servers` (the shared file's own rule, and the resolver would keep
/// only one), a name in `servers` that is ALSO in `disabled` when the user
/// probably meant one or the other, and a transport value still carrying
/// [`REDACTED`]. The third is the only one the resolver could answer on its
/// own; refusing it here means the user finds out now rather than wondering
/// later which instruction won. The fourth is the write half of the read
/// side's redaction: persisting the marker would replace a working credential
/// with the word that stood in for it.
///
/// A validated name is stored TRIMMED, not as it arrived. Validation already
/// trims, so an untrimmed store would accept `" github"` as a valid disable of
/// `github` and then match nothing — a dead entry rather than the override the
/// user asked for.
/// Test: `put_replaces_the_whole_table`, `put_refuses_a_blank_name`,
/// `put_refuses_a_duplicate_name`, `put_refuses_a_name_in_both_lists`,
/// `put_refuses_a_redacted_value`, `put_trims_a_stored_name`,
/// `put_without_servers_keeps_the_stored_ones`.
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
    let home = AssistantHome::under(root, id.clone());

    // Absent `servers` keeps what is stored, so a client editing only the
    // disable list never restates a transport it was shown redacted.
    let read = read_overrides(&home);
    let submitted = match body.servers {
        Some(servers) => servers,
        None => {
            if let Some(detail) = read.error {
                return fail(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!(
                        "this assistant's `[mcp]` table could not be read, so its servers \
                         cannot be kept across this write: {detail}. Send the full `servers` \
                         list, or fix {}.",
                        read.path.display()
                    ),
                );
            }
            read.overrides.servers
        }
    };

    let mut servers: Vec<McpServerConfig> = Vec::with_capacity(submitted.len());
    for mut server in submitted {
        let trimmed = server.name.trim().to_string();
        if trimmed.is_empty() {
            return fail(
                StatusCode::BAD_REQUEST,
                "every MCP server needs a name; overrides match the global list by name",
            );
        }
        if servers.iter().any(|s| s.name == trimmed) {
            return fail(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("MCP server `{trimmed}` is listed twice; each name may appear once"),
            );
        }
        if let Some(key) = redacted_secret_key(&server) {
            return fail(
                StatusCode::BAD_REQUEST,
                format!(
                    "MCP server `{trimmed}` sends `{key}` as `{REDACTED}`, which is what this \
                     route renders in place of a secret. Send the real value, or omit \
                     `servers` to leave this assistant's servers unchanged."
                ),
            );
        }
        server.name = trimmed;
        servers.push(server);
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
        if servers.iter().any(|s| s.name == trimmed) {
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

    let overrides = McpOverrides { servers, disabled };
    if let Err(e) = home
        .ensure()
        .and_then(|_| write_overrides(&home, &overrides))
    {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    let resolved = resolve_in(Some(id.as_str()), global, overrides, None);
    Json(body_for(&resolved)).into_response()
}

/// The first `env` or `headers` key whose value is the redaction marker.
///
/// Why: a client that edits what [`redacted`] rendered sends the marker back,
/// and storing it would overwrite a working credential with the word that
/// stood in for it. Naming the KEY is what lets the refusal say which value to
/// re-enter.
/// Test: `put_refuses_a_redacted_value`.
fn redacted_secret_key(server: &McpServerConfig) -> Option<String> {
    let map = match &server.transport {
        trusty_mcp::config::McpTransport::Stdio { env, .. } => env,
        trusty_mcp::config::McpTransport::Http { headers, .. } => headers,
        trusty_mcp::config::McpTransport::Sse { headers, .. } => headers,
        // `McpTransport` is `#[non_exhaustive]`: a transport this build does
        // not know carries no map it can check, so there is nothing to refuse.
        _ => return None,
    };
    map.iter()
        .find(|(_, value)| value.as_str() == REDACTED)
        .map(|(key, _)| key.clone())
}

/// The response body shared by both routes.
///
/// Every server list goes through [`redacted`] — see the module doc.
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
        "global": redacted_all(&resolved.global),
        "overrides": {
            "servers": redacted_all(&resolved.overrides.servers),
            "disabled": resolved.overrides.disabled,
        },
        "resolved": redacted_all(&resolved.servers),
        "statuses": resolved.statuses,
        "issues": issues,
    })
}

/// [`redacted`] over a whole list.
fn redacted_all(servers: &[McpServerConfig]) -> Vec<Value> {
    servers.iter().map(redacted).collect()
}

/// One server as JSON, with every `env` and `headers` VALUE hidden.
///
/// Why: `McpTransport`'s `Serialize` is deliberately unredacted so the config
/// file round-trips (its `Debug` is the redacting one, and a route does not go
/// through `Debug`). Serialising a resolved server straight into the response
/// therefore returns every inline API key and bearer token over the API. This
/// is the route's own redaction, applied to what it RENDERS rather than to
/// what it stores.
/// What: serialises, then replaces each value under `transport.env` and
/// `transport.headers` with [`REDACTED`]. Keys survive — which variables a
/// server needs is the diagnostic; the values are not. `command`, `args` and
/// `url` are untouched: they identify the server and none is a credential,
/// which is the same line `McpTransport`'s `Debug` draws.
///
/// `extensions` is untouched too. Its documented shapes carry credential
/// REFERENCES, never secrets — an `auth` block names an environment variable
/// (`crate::mcp::extensions::AuthSpec`) — and the pane reads `tools`,
/// `description` and `source` out of it.
/// Test: `get_redacts_env_and_header_values`.
fn redacted(server: &McpServerConfig) -> Value {
    let mut value = match serde_json::to_value(server) {
        Ok(value) => value,
        // Unreachable for this shape; a bare error object is still safer than
        // a fallback that could serialise the server unredacted.
        Err(e) => return json!({ "name": server.name, "error": e.to_string() }),
    };
    if let Some(transport) = value.get_mut("transport").and_then(Value::as_object_mut) {
        for key in ["env", "headers"] {
            let Some(map) = transport.get_mut(key).and_then(Value::as_object_mut) else {
                continue;
            };
            for secret in map.values_mut() {
                *secret = Value::String(REDACTED.to_string());
            }
        }
    }
    value
}
