//! `GET /api/agents/:name/stores` — the agent's OKG store bindings, resolved
//! live against the running daemons (#3816/#3864, epic #3052).
//!
//! Why: The gear panel's "OKG Stores" tab previously rendered a hardcoded
//! client-side placeholder ("agent.toml has no OKG store binding field yet")
//! because there was neither a config field nor a route to read one. This is
//! the read side of that field: it parses the agent's `[[stores]]` bindings
//! off disk and asks trusty-search / trusty-memory whether each one actually
//! exists, so the card can say CONNECTED with a chunk count or NOT CONNECTED
//! with a reason instead of asserting the same thing for every agent.
//! What: [`agent_stores_route`] is the axum shim; [`stores_at`] is the
//! testable core taking the agents-dir list and both daemon base URLs
//! explicitly (same injected-dependency convention as `agent_patch::*_at` and
//! `workstreams::list_workstreams_at`). Response shape:
//! `{"stores": [StoreStatus, …]}` — an empty array for an agent that binds
//! nothing, which is a valid state, not an error. A daemon being down is
//! likewise reported per-store, never as a route failure: this endpoint
//! returns `200` whenever the agent resolves at all.
//! Test: `super::tests::agent_stores`.

use std::path::PathBuf;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};

use super::agent_patch::resolve_agent_paths;
use super::state::AppState;
use crate::stores::{StoresConfig, resolve_store_statuses};

/// `GET /api/agents/:name/stores` — HTTP entry point.
///
/// Why/What: see the module doc. Daemon base URLs are discovered here (via
/// the shared `http_addr` convention every trusty-* daemon writes) and
/// threaded into [`stores_at`] so tests can substitute a mock.
/// Test: `super::tests::agent_stores::stores_route_reports_connected_binding_with_stats`.
pub(super) async fn agent_stores_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    stores_at(
        &crate::agents::agents_dir_candidates(),
        &name,
        trusty_common::resolve_daemon_base_url("trusty-search").as_deref(),
        trusty_common::resolve_daemon_base_url("trusty-memory").as_deref(),
    )
    .await
}

/// Core store-resolution logic against explicit agents dirs + daemon URLs.
///
/// Why: Same testability rationale as `agent_patch::patch_agent_at`.
/// What: `400` for an invalid name, `404` for an unknown agent, `500` only
/// when the resolved config file cannot be read. Malformed TOML is NOT a
/// `500` here — it degrades to "no bindings" plus a `config_error` field, so
/// a hand-edited file breaking the stores table still lets the panel render
/// the rest of the agent. Every per-store failure is inside the `StoreStatus`
/// entries, not the HTTP status.
/// Test: `stores_route_reports_connected_binding_with_stats`, `stores_route_unknown_agent_404`,
/// `stores_route_empty_for_unbound_agent`,
/// `stores_route_degrades_on_malformed_toml`.
pub(super) async fn stores_at(
    dirs: &[PathBuf],
    name: &str,
    search_base: Option<&str>,
    memory_base: Option<&str>,
) -> Response {
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid agent name" })),
        )
            .into_response();
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
            tracing::warn!(?e, agent = name, path = %path.display(), "stores_at: read failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "failed to read agent config" })),
            )
                .into_response();
        }
    };

    let (stores, config_error) = parse_stores(&raw);
    let statuses = resolve_store_statuses(name, &stores, search_base, memory_base).await;
    let mut body = serde_json::json!({
        "stores": statuses,
        "issues": stores.validate(),
    });
    if let Some(err) = config_error {
        body["config_error"] = serde_json::Value::String(err);
    }
    (StatusCode::OK, Json(body)).into_response()
}

/// Parse just the `[[stores]]` table out of a raw `agent.toml`.
///
/// Why: Reading the whole file through `AgentConfig` would require `[llm]`
/// and `[system_prompt]` to be present and valid — true for a flat agent but
/// NOT for a directory package, whose `agent.toml` deliberately omits
/// `system_prompt.content` (it lives in `persona.md`). Deserializing only the
/// field this endpoint needs keeps the route working for both file layouts,
/// matching `parse_agent_toml`'s partial-read precedent.
/// What: `(bindings, None)` on success; `(empty, Some(message))` when the
/// file isn't valid TOML or its `stores` value has an unusable shape.
/// Test: `parse_stores_reads_array_form`, `parse_stores_reports_bad_toml`.
fn parse_stores(raw: &str) -> (StoresConfig, Option<String>) {
    #[derive(serde::Deserialize)]
    struct Partial {
        #[serde(default)]
        stores: StoresConfig,
    }
    match toml::from_str::<Partial>(raw) {
        Ok(p) => (p.stores, None),
        Err(e) => (StoresConfig::default(), Some(e.to_string())),
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn parse_stores_reads_array_form() {
        let raw = r#"
[agent]
name = "izzie"

[[stores]]
name = "bob-kb"
index = "bob-kb"
palace = "owner-profile"
"#;
        let (stores, err) = parse_stores(raw);
        assert!(err.is_none());
        assert_eq!(stores.default_search_index(), Some("bob-kb"));
    }

    #[test]
    fn parse_stores_ignores_other_sections() {
        // A package `agent.toml` has no `[system_prompt]` — parsing must not
        // require one (see this fn's doc comment).
        let raw = "[agent]\nname = \"x\"\n[tools]\nallow = [\"a\"]\n";
        let (stores, err) = parse_stores(raw);
        assert!(err.is_none());
        assert!(stores.bindings.is_empty());
    }

    #[test]
    fn parse_stores_reports_bad_toml() {
        let (stores, err) = parse_stores("this is not = = toml");
        assert!(stores.bindings.is_empty());
        assert!(err.is_some());
    }
}
