//! `GET /api/agents/:name/knowledge` — the unified "what does this agent
//! know" surface: store bindings + knowledge-classified tools + MCP
//! knowledge connections, in one response (#3935, DOC-57 §4).
//!
//! Why: DOC-57 §4.1 widens the old OKG-Stores-only pane into three
//! sub-surfaces that answer ONE question, so a client must not have to
//! correlate a store card, a tool glob and a harness endpoint table across
//! three files/routes. K-a (store bindings) is `agent_stores::stores_at`'s
//! existing, unchanged resolution; K-b (knowledge tools) filters the agent's
//! resolved skill set (#3933's `SkillCatalog`) to `kind == Knowledge`; K-c
//! (MCP knowledge connections) reads `~/.trusty-agents/config.toml`'s
//! `[tool_registry.endpoints]` — read-only, via `GlobalConfig::load()`,
//! which never creates the file (unlike `load_or_create()`).
//!
//! **Honesty rules this route holds to** (DOC-57 §4, restated):
//! - K-a: identical posture to `/stores` — a bound-but-unreachable index
//!   renders NOT CONNECTED with a reason, never a route failure (K-1).
//! - K-b: a tool is "knowledge" **only** when the skill wrapping it declares
//!   `kind = Knowledge` (`skills::manifest::builtin::knowledge`) — no
//!   hardcoded tool-name list (§4.3). `vector_search` renders under its bound
//!   store when the agent has one (§4.3's "not a free-floating chip" rule).
//! - K-c: **no live probe is performed.** Spawning/discovering an MCP
//!   subprocess on every knowledge fetch would be slow and side-effecting;
//!   this route reports the DECLARED config truthfully instead. `connected`
//!   is therefore always `false` here — never fabricated as `true` — with a
//!   `reason` distinguishing "disabled in config.toml" from "not probed by
//!   this route" (§4.4's "never as connected, never hidden" rule, applied
//!   conservatively to the enabled case too). Reads BOTH
//!   `[[tool_registry.endpoints]]` and `[[mcp.services]]` (§4.1's own list of
//!   sources) — see [`knowledge_mcp_connections`] for an honest gap this
//!   route surfaces: `trusty-memory`/`trusty-search` have already moved off
//!   the former onto the latter in `default-config.toml`, so DOC-57 §4.4's
//!   illustrative "both disabled under tool_registry.endpoints" table is
//!   stale against the live default; this route reports what is actually
//!   configured, not the issue's example.
//!
//! What: [`agent_knowledge_route`] is the axum shim; [`knowledge_at`] is the
//! testable core. Like `agent_skills::skills_at`, K-b is read from the
//! agent's OWN `agent.toml` without resolving `extends` (matching that
//! route's documented, pre-existing limitation — not a new gap introduced
//! here).
//! Test: `super::tests::agent_knowledge`.

use std::path::PathBuf;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use super::agent_patch::resolve_agent_paths;
use super::state::AppState;
use crate::agents::{SkillsConfig, ToolsConfig};
use crate::ctrl::pm_task::match_any_glob;
use crate::mcp::config::McpService;
use crate::skills::manifest::{SkillCatalog, SkillKind, effective_tool_patterns};
use crate::stores::{StoresConfig, resolve_store_statuses};
use crate::tools::registry::config::{DriverKind, EndpointConfig};

/// `GET /api/agents/:name/knowledge` — HTTP entry point.
///
/// Why/What: see the module doc. Daemon base URLs are discovered the same
/// way as `agent_stores_route`; the skill-source root mirrors the dispatch
/// call sites (project CWD), matching `agent_skills_route`.
/// Test: `super::tests::agent_knowledge::knowledge_route_reports_bound_store_and_tools`.
pub(super) async fn agent_knowledge_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    knowledge_at(
        &crate::agents::agents_dir_candidates(),
        &name,
        &project_root,
        trusty_common::resolve_daemon_base_url("trusty-search").as_deref(),
        trusty_common::resolve_daemon_base_url("trusty-memory").as_deref(),
    )
    .await
}

/// Core knowledge-resolution logic against explicit dirs/roots/daemon URLs.
///
/// Why: Same testability rationale as `agent_stores::stores_at` /
/// `agent_skills::skills_at`.
/// What: `400` for an invalid name, `404` for an unknown agent, `500` only
/// when the resolved config file cannot be read. Malformed TOML degrades to
/// empty sub-surfaces plus `config_error`, never a `500` (K-1).
/// Test: `knowledge_route_reports_bound_store_and_tools`,
/// `knowledge_route_unknown_agent_404`,
/// `knowledge_route_degrades_on_malformed_toml`,
/// `knowledge_route_reports_disabled_endpoint_with_reason`.
pub(super) async fn knowledge_at(
    dirs: &[PathBuf],
    name: &str,
    project_root: &std::path::Path,
    search_base: Option<&str>,
    memory_base: Option<&str>,
) -> Response {
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid agent name" })),
        )
            .into_response();
    }
    let Some((path, _package_dir)) = resolve_agent_paths(dirs, name) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown agent", "name": name })),
        )
            .into_response();
    };
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(?e, agent = name, path = %path.display(), "knowledge_at: read failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to read agent config" })),
            )
                .into_response();
        }
    };

    let (stores, tools, skills, config_error) = parse_knowledge_sections(&raw);

    // K-a: unchanged resolution logic from `/stores` (#3878/#3864).
    let statuses = resolve_store_statuses(name, &stores, search_base, memory_base).await;
    let issues = stores.validate();

    // K-b: the agent's resolved skill set, filtered to `kind == Knowledge`.
    // Deliberately does NOT resolve `extends` — same documented limitation
    // as `agent_skills::skills_at` (see that route's module doc).
    let catalog =
        SkillCatalog::builtin().with_authored(crate::skills::manifest::authored::load_from_paths(
            &crate::skills::sources::SkillSourceRegistry::load(project_root).resolved_paths(),
        ));
    let (patterns, _unresolved) =
        effective_tool_patterns(tools.allow.as_ref(), skills.allow.as_ref(), &catalog);
    let knowledge_tools = knowledge_tool_cards(&catalog, patterns.as_deref(), &stores);

    // K-c: declared MCP knowledge connections — no live probe (see module doc).
    let global_config = crate::mcp::config::GlobalConfig::load().await;
    let mcp = knowledge_mcp_connections(&global_config);

    let mut body = json!({
        "stores": statuses,
        "tools": knowledge_tools,
        "mcp": mcp,
        "issues": issues,
    });
    if let Some(err) = config_error {
        body["config_error"] = Value::String(err);
    }
    (StatusCode::OK, Json(body)).into_response()
}

/// Every catalog skill classified `kind == Knowledge`, rendered against this
/// agent's grants (DOC-57 §4.3).
///
/// Why: The pane must show "what does this agent know", not merely "what
/// COULD it know" — every catalog knowledge skill is returned (mirroring
/// `/skills`' "every catalog skill, not only the granted subset" precedent)
/// so the pane can render both an available capability and a reason a
/// capability is missing.
/// What: One entry per knowledge-kind, tool-wrapping skill in the catalog —
/// `{tool, skill, bound_store, available, reason}`. `bound_store` is `Some`
/// only for `vector_search` (the seam DOC-57 §4.3 names explicitly) AND only
/// when the agent has a bound store — every other knowledge tool renders
/// `bound_store: null`, never a fabricated linkage. A tool-less knowledge
/// skill (none exist in the current catalog, but the shape is defensive) is
/// skipped: this array reports resolvable TOOLS, and a tool-less skill has
/// none to name.
/// Test: `knowledge_tool_cards_reports_vector_search_bound_to_the_store`,
/// `knowledge_tool_cards_reports_ungranted_tool_with_a_reason`,
/// `knowledge_tool_cards_excludes_action_kind_skills`.
fn knowledge_tool_cards(
    catalog: &SkillCatalog,
    patterns: Option<&[String]>,
    stores: &StoresConfig,
) -> Vec<Value> {
    catalog
        .manifests()
        .iter()
        .filter(|m| m.kind == SkillKind::Knowledge)
        .filter_map(|m| {
            let tool = m.tool()?;
            let available = patterns.is_some_and(|p| match_any_glob(tool, p));
            let bound_store = if tool == "vector_search" {
                stores.primary().map(|b| b.name.clone())
            } else {
                None
            };
            Some(json!({
                "tool": tool,
                "skill": m.id,
                "bound_store": bound_store,
                "available": available,
                "reason": if available {
                    None
                } else {
                    Some(
                        "not granted via [tools].allow or [skills].allow for this agent"
                            .to_string(),
                    )
                },
            }))
        })
        .collect()
}

/// Configured MCP surfaces that plausibly back a knowledge tool (DOC-57
/// §4.4), from BOTH harness registries §4.1 names: `[[tool_registry.endpoints]]`
/// (OpenRPC/stdio-mcp, scope-tagged) and `[[mcp.services]]` (native
/// MCP-stdio, no scope tagging).
///
/// Why: There is no live discovery data linking a service to the specific
/// tools it advertises (probing would mean spawning its subprocess on every
/// knowledge fetch — see the module doc). `tool_registry.endpoints` entries
/// carry declared `scopes`, so those are classified by scope NAMESPACE (the
/// segment before the first `.`) being `memory`, `search` or `google` — the
/// three families DOC-57 §4.4 names. `mcp.services` entries carry no scopes
/// at all (`McpService` has no such field), so those are classified by NAME
/// against the harness's own two first-party knowledge daemons
/// (`trusty-memory`, `trusty-search`) — a small, stable, first-party pair,
/// not the large drift-prone tool-name list DOC-57 §4.3 rejects for K-b.
/// **Honest gap discovered while building this route:** `default-config.toml`
/// has already moved `trusty-memory`/`trusty-search` OFF
/// `[[tool_registry.endpoints]]` (where DOC-57 §4.4's table still places
/// them, `enabled = false`) and onto `[[mcp.services]]` (`enabled = true`,
/// `discover = true`) — this route reads the config LIVE, so it reports
/// today's actual shape rather than the issue's now-stale illustration.
/// What: One entry per qualifying service, `{name, kind, enabled, connected,
/// scopes, reason}`. `kind` is `"openrpc"` for `driver = "direct"`, `"mcp"`
/// for `driver = "stdio-mcp"` or an `[[mcp.services]]` entry. `connected` is
/// always `false` (no probe — see module doc); `scopes` is `[]` for an
/// `mcp.services` entry (nothing to report).
/// Test: `knowledge_mcp_connections_flags_memory_and_search_by_scope`,
/// `knowledge_mcp_connections_ignores_an_unrelated_endpoint`,
/// `knowledge_mcp_connections_empty_when_no_registry_configured`,
/// `knowledge_mcp_connections_flags_first_party_mcp_services_by_name`.
fn knowledge_mcp_connections(global: &crate::mcp::config::GlobalConfig) -> Vec<Value> {
    const KNOWLEDGE_NAMESPACES: &[&str] = &["memory", "search", "google"];
    const KNOWLEDGE_SERVICE_NAMES: &[&str] = &["trusty-memory", "trusty-search"];

    let mut out: Vec<Value> = Vec::new();
    if let Some(registry) = &global.tool_registry {
        out.extend(
            registry
                .endpoints
                .iter()
                .filter(|ep| {
                    ep.scopes.iter().any(|s| {
                        let namespace = s.split('.').next().unwrap_or(s);
                        KNOWLEDGE_NAMESPACES.contains(&namespace)
                    })
                })
                .map(endpoint_card),
        );
    }
    out.extend(
        global
            .mcp
            .services
            .iter()
            .filter(|svc| KNOWLEDGE_SERVICE_NAMES.contains(&svc.name.as_str()))
            .map(mcp_service_card),
    );
    out
}

fn endpoint_card(ep: &EndpointConfig) -> Value {
    let kind = match ep.driver {
        DriverKind::Direct => "openrpc",
        DriverKind::StdioMcp => "mcp",
    };
    let reason = if ep.enabled {
        "endpoint is enabled in config.toml; this route does not probe live connectivity"
            .to_string()
    } else {
        "endpoint disabled in config.toml".to_string()
    };
    json!({
        "name": ep.name,
        "kind": kind,
        "enabled": ep.enabled,
        "connected": false,
        "scopes": ep.scopes,
        "reason": reason,
    })
}

fn mcp_service_card(svc: &McpService) -> Value {
    let reason = if svc.enabled {
        "service is enabled in config.toml; this route does not probe live connectivity".to_string()
    } else {
        "service disabled in config.toml".to_string()
    };
    json!({
        "name": svc.name,
        "kind": "mcp",
        "enabled": svc.enabled,
        "connected": false,
        "scopes": Vec::<String>::new(),
        "reason": reason,
    })
}

/// Parse `[stores]`, `[tools]` and `[skills]` out of a raw `agent.toml`.
///
/// Why: Same partial-read rationale as `agent_stores::parse_stores` /
/// `agent_skills::parse_capability_sections` — a directory-package
/// `agent.toml` omits `[system_prompt]`, so reading through the full
/// `AgentConfig` would reject it.
/// What: `(stores, tools, skills, None)` on success; defaults plus
/// `Some(message)` when the file is not valid TOML.
/// Test: `parse_knowledge_sections_reads_all_three`,
/// `parse_knowledge_sections_reports_bad_toml`.
fn parse_knowledge_sections(
    raw: &str,
) -> (StoresConfig, ToolsConfig, SkillsConfig, Option<String>) {
    #[derive(serde::Deserialize)]
    struct Partial {
        #[serde(default)]
        stores: StoresConfig,
        #[serde(default)]
        tools: ToolsConfig,
        #[serde(default)]
        skills: SkillsConfig,
    }
    match toml::from_str::<Partial>(raw) {
        Ok(p) => (p.stores, p.tools, p.skills, None),
        Err(e) => (
            StoresConfig::default(),
            ToolsConfig::default(),
            SkillsConfig::default(),
            Some(e.to_string()),
        ),
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn parse_knowledge_sections_reads_all_three() {
        let raw = r#"
[agent]
name = "cto-assistant"

[[stores]]
name = "cto-assistant"

[tools]
allow = ["vector_search"]

[skills]
allow = ["memory-recall"]
"#;
        let (stores, tools, skills, err) = parse_knowledge_sections(raw);
        assert!(err.is_none());
        assert_eq!(stores.default_search_index(), Some("cto-assistant"));
        assert_eq!(tools.allow.unwrap(), vec!["vector_search".to_string()]);
        assert_eq!(skills.allow.unwrap(), vec!["memory-recall".to_string()]);
    }

    #[test]
    fn parse_knowledge_sections_reports_bad_toml() {
        let (stores, _tools, _skills, err) = parse_knowledge_sections("not = = toml");
        assert!(stores.bindings.is_empty());
        assert!(err.is_some());
    }

    #[test]
    fn knowledge_tool_cards_reports_vector_search_bound_to_the_store() {
        let catalog = SkillCatalog::builtin();
        let stores: StoresConfig = toml::from_str::<StoresWrapper>(
            "[[stores]]\nname = \"cto-assistant\"\nindex = \"cto-assistant\"\n",
        )
        .unwrap()
        .stores;
        let patterns = vec!["vector_search".to_string()];
        let cards = knowledge_tool_cards(&catalog, Some(&patterns), &stores);
        let vs = cards
            .iter()
            .find(|c| c["tool"] == "vector_search")
            .expect("vector_search must be a knowledge tool");
        assert_eq!(vs["bound_store"], "cto-assistant");
        assert_eq!(vs["available"], true);
        assert_eq!(vs["reason"], Value::Null);
    }

    #[test]
    fn knowledge_tool_cards_reports_ungranted_tool_with_a_reason() {
        let catalog = SkillCatalog::builtin();
        let cards = knowledge_tool_cards(&catalog, Some(&[]), &StoresConfig::default());
        let vs = cards.iter().find(|c| c["tool"] == "vector_search").unwrap();
        assert_eq!(vs["available"], false);
        assert_eq!(vs["bound_store"], Value::Null);
        assert!(vs["reason"].as_str().unwrap().contains("not granted"));
    }

    #[test]
    fn knowledge_tool_cards_excludes_action_kind_skills() {
        let catalog = SkillCatalog::builtin();
        let patterns = vec!["*".to_string()];
        let cards = knowledge_tool_cards(&catalog, Some(&patterns), &StoresConfig::default());
        assert!(
            cards.iter().all(|c| c["tool"] != "search_gmail_messages"),
            "an action-kind skill must not appear in the knowledge pane"
        );
        assert!(
            cards.iter().any(|c| c["tool"] == "memory_recall"),
            "a knowledge-kind skill must appear"
        );
    }

    #[test]
    fn knowledge_mcp_connections_flags_memory_and_search_by_scope() {
        let global: crate::mcp::config::GlobalConfig = toml::from_str(
            r#"
[tool_registry]
[[tool_registry.endpoints]]
name = "trusty-memory"
driver = "direct"
command = "trusty-memory"
enabled = false
scopes = ["memory.read", "memory.write"]

[[tool_registry.endpoints]]
name = "trusty-search"
driver = "direct"
command = "trusty-search"
enabled = false
scopes = ["search.read"]
"#,
        )
        .unwrap();
        let cards = knowledge_mcp_connections(&global);
        assert_eq!(cards.len(), 2);
        let memory = cards.iter().find(|c| c["name"] == "trusty-memory").unwrap();
        assert_eq!(memory["enabled"], false);
        assert_eq!(memory["connected"], false);
        assert!(
            memory["reason"]
                .as_str()
                .unwrap()
                .contains("disabled in config.toml")
        );
    }

    #[test]
    fn knowledge_mcp_connections_ignores_an_unrelated_endpoint() {
        let global: crate::mcp::config::GlobalConfig = toml::from_str(
            r#"
[tool_registry]
[[tool_registry.endpoints]]
name = "some-ticketing-mcp"
driver = "direct"
command = "ticketing-rpc"
scopes = ["ticketing.read"]
"#,
        )
        .unwrap();
        assert!(knowledge_mcp_connections(&global).is_empty());
    }

    #[test]
    fn knowledge_mcp_connections_empty_when_no_registry_configured() {
        let global = crate::mcp::config::GlobalConfig::default();
        assert!(knowledge_mcp_connections(&global).is_empty());
    }

    /// Today's actual shape (see the module doc's "honest gap discovered"
    /// note): `trusty-memory`/`trusty-search` live under `[[mcp.services]]`,
    /// not `[[tool_registry.endpoints]]`. `McpService` carries no `scopes`
    /// field, so these are classified by name.
    #[test]
    fn knowledge_mcp_connections_flags_first_party_mcp_services_by_name() {
        let global: crate::mcp::config::GlobalConfig = toml::from_str(
            r#"
[[mcp.services]]
name = "trusty-memory"
description = "memory"
command = "trusty-memory"
args = ["serve", "--stdio"]
transport = "stdio"
enabled = true
discover = true

[[mcp.services]]
name = "trusty-mpm"
description = "unrelated"
command = "tm"
transport = "stdio"
enabled = true
"#,
        )
        .unwrap();
        let cards = knowledge_mcp_connections(&global);
        assert_eq!(cards.len(), 1, "{cards:?}");
        assert_eq!(cards[0]["name"], "trusty-memory");
        assert_eq!(cards[0]["kind"], "mcp");
        assert_eq!(cards[0]["enabled"], true);
        assert_eq!(cards[0]["connected"], false);
    }

    #[derive(serde::Deserialize)]
    struct StoresWrapper {
        stores: StoresConfig,
    }
}
