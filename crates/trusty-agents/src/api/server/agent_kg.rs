//! `GET /api/agents/:name/kg*` — a READ-ONLY proxy onto the agent's bound
//! memory palace's Knowledge Graph (#4290).
//!
//! Why: trusty-memory ships the entire palace-scoped KG read surface
//! (`memory.kg_subjects_with_counts`, `memory.kg_all`, `memory.kg_count`, and
//! the `kg_query` tool), but every one of them is keyed by PALACE id. The
//! Knowledge-Graph browser in the agents GUI is keyed by AGENT — and the
//! mapping between the two lives in this crate (`[[stores]].palace`, resolved
//! by `StoresConfig::primary()`). Without this module a client would have to
//! read `GET /api/agents/:name/stores`, dig the palace id out of the first
//! binding, find the trusty-memory socket for itself, and then speak framed
//! JSON-RPC over a Unix socket from a browser — which it cannot do at all.
//! This is the plumbing that removes that: agent in, KG JSON out.
//!
//! **This module holds no KG query logic of its own.** `MemoryService` stays
//! the single entry point for every KG read; the upstream body is passed
//! through under `data` — never re-ranked, truncated or re-sorted here — so the
//! GUI and the MCP `kg_query` tool can never disagree about what the graph
//! contains.
//!
//! One method's answer is projected rather than passed whole, and only one:
//! `/kg?subject=` maps onto the `kg_query` TOOL, which answers
//! `{subject, triples, kg_triple_count, …}` where the retired route answered a
//! bare `Vec<Triple>`. `data` carries `triples`, because this route's contract
//! is that every array-shaped read has the same TYPE in `data` and a client
//! branches only on `connected` (see the envelope below). The projection is
//! named here rather than buried: `KgRead::project`.
//!
//! **Read-only by owner decision.** trusty-memory's `POST /kg` (assert) and
//! `DELETE /kg/triples/{id}` (retract) are deliberately NOT proxied;
//! editability is a separate follow-up ticket. Adding a write here would also
//! put a mutating cross-daemon call behind this crate's same-origin write
//! guard for the first time, which is a decision this slice does not make.
//!
//! **Never-fail posture** (identical contract to
//! [`crate::stores::resolve_store_statuses`], which this module deliberately
//! mirrors rather than inventing a second error vocabulary): an agent that
//! binds no palace, a trusty-memory daemon that is down, a palace that does
//! not exist, and an unreadable upstream body ALL resolve to `200 OK` with
//! `connected: false` plus a machine-readable `reason` and an empty-but-
//! well-typed `data`. A browser pane must render an empty state, not an error
//! toast, for the ordinary condition "this agent has no palace yet". Only
//! genuinely client-side faults keep a non-200: `400` for an invalid agent
//! name or a missing `subject`, `404` for an unknown agent, `500` only when
//! the agent's own config file cannot be read off disk.
//!
//! **The upstream is a Unix socket, not an HTTP origin (#6286).** This module
//! resolved `resolve_daemon_base_url("trusty-memory")`, which reads an
//! `http_addr` file ADR-0032 stopped writing — so it was permanently `None` and
//! every request took the degraded arm, rendering the GUI's KG pane empty with
//! "daemon not discoverable" whether or not the daemon was up. The four reads
//! map onto folded methods (`memory.kg_subjects_with_counts`, `memory.kg_all`,
//! `memory.kg_count`) and the `kg_query` tool.
//!
//! What: four thin axum shims ([`agent_kg_subjects_route`],
//! [`agent_kg_all_route`], [`agent_kg_query_route`], [`agent_kg_count_route`])
//! over one testable core, [`kg_proxy_at`], which takes the agents-dir list
//! and the trusty-memory socket explicitly (the injected-dependency
//! convention of `agent_stores::stores_at`). Response envelope, identical for
//! all four routes:
//!
//! ```json
//! { "palace": "cto" | null, "connected": true, "data": <upstream JSON> }
//! { "palace": null, "connected": false, "reason": "…", "data": [] }
//! ```
//!
//! plus `config_error` when the agent's `agent.toml` failed to parse. `data`
//! is the upstream array for `/kg`, `/kg/subjects` and `/kg/all`, and the
//! upstream `{"active": N}` object for `/kg/count`; its empty form (`[]` /
//! `{"active": 0}`) is preserved on every degraded path so a client never has
//! to branch on the payload's TYPE, only on `connected`.
//! Test: `super::tests::agent_kg`.

use std::path::PathBuf;

use axum::{
    Json,
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::agent_patch::resolve_agent_paths;
use super::state::AppState;
use crate::stores::StoresConfig;
use crate::stores::status::PROBE_TIMEOUT;

/// Query parameters accepted across all four KG proxy routes.
///
/// Every field is optional and forwarded ONLY when present, so trusty-memory's
/// own defaults and clamps (`limit` default 50, max 200 —
/// `kg_routes::MAX_KG_LIST_LIMIT`) stay the single source of truth. Restating
/// them here would give the GUI two different ceilings to reason about.
#[derive(Debug, Default, Deserialize)]
pub(super) struct KgParams {
    limit: Option<usize>,
    offset: Option<usize>,
    subject: Option<String>,
}

/// One upstream KG read, resolved from the route that was hit.
///
/// `method` is the name dialled on trusty-memory's socket; `params` are that
/// method's own arguments minus the palace, which is filled in once the agent's
/// binding resolves; `empty` is the payload shape used for `data` on every
/// degraded path (see the module doc's envelope).
pub(super) struct KgRead {
    method: &'static str,
    params: serde_json::Map<String, Value>,
    /// The key this method names the palace under.
    ///
    /// The folded methods take `palace_id`, flattened out of the former path
    /// segment. `kg_query` is a TOOL and has always taken `palace`. Naming the
    /// key per read is what lets both go through one call site.
    palace_key: &'static str,
    empty: Value,
    /// The field to lift out of the answer, when the method's shape is wider
    /// than this route's `data` contract.
    ///
    /// `Some("triples")` for `kg_query` only — see the module doc.
    project: Option<&'static str>,
}

impl KgRead {
    /// Construct one read. `pub(super)` so `super::tests::agent_kg` can drive
    /// [`kg_proxy_at`] with an arbitrary read without a test-only shim on the
    /// production type.
    pub(super) fn new(
        method: &'static str,
        palace_key: &'static str,
        params: Vec<(&'static str, Value)>,
        empty: Value,
    ) -> Self {
        Self {
            method,
            params: params
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            palace_key,
            empty,
            project: None,
        }
    }

    /// Lift `field` out of the answer instead of passing the object whole.
    pub(super) fn projecting(mut self, field: &'static str) -> Self {
        self.project = Some(field);
        self
    }

    /// The params to send, with the resolved palace filled in.
    fn params_for(&self, palace: &str) -> Value {
        let mut params = self.params.clone();
        params.insert(
            self.palace_key.to_string(),
            Value::String(palace.to_string()),
        );
        Value::Object(params)
    }
}

/// `limit`/`offset` as forwardable params, omitting whichever the caller did
/// not send so trusty-memory's own defaults and clamps stay the single source
/// of truth.
fn page_params(p: &KgParams, with_offset: bool) -> Vec<(&'static str, Value)> {
    let mut params = Vec::new();
    if let Some(limit) = p.limit {
        params.push(("limit", Value::from(limit)));
    }
    if with_offset && let Some(offset) = p.offset {
        params.push(("offset", Value::from(offset)));
    }
    params
}

/// `GET /api/agents/:name/kg/subjects?limit=N` — HTTP entry point.
///
/// Why: The browser's left-hand subject list needs a count badge per subject,
/// so this proxies `kg_list_subjects_with_counts` (not the bare
/// `kg_list_subjects`) — one upstream pass instead of one query per subject.
/// What: `data` is the upstream `[{subject, count}, …]` array verbatim.
/// Test: `kg_subjects_route_passes_upstream_through`.
// #4290: per-agent entry onto trusty-memory's palace-scoped subject list.
pub(super) async fn agent_kg_subjects_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Query(p): Query<KgParams>,
) -> Response {
    let read = KgRead::new(
        "memory.kg_subjects_with_counts",
        "palace_id",
        page_params(&p, false),
        json!([]),
    );
    proxy_with_discovered_memory(&name, read).await
}

/// `GET /api/agents/:name/kg/all?limit=N&offset=N` — HTTP entry point.
///
/// Why: The browser's "All" mode pages across every active triple regardless
/// of subject; `kg_query` cannot serve it because it REQUIRES a subject.
/// What: `data` is the upstream `[Triple, …]` array verbatim.
/// Test: `kg_all_route_forwards_limit_and_offset`.
// #4290: per-agent entry onto trusty-memory's paginated all-triples list.
pub(super) async fn agent_kg_all_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Query(p): Query<KgParams>,
) -> Response {
    let read = KgRead::new(
        "memory.kg_all",
        "palace_id",
        page_params(&p, true),
        json!([]),
    );
    proxy_with_discovered_memory(&name, read).await
}

/// `GET /api/agents/:name/kg?subject=<s>` — HTTP entry point.
///
/// Why: The browser's detail pane fetches one subject's edges after a click,
/// which is far cheaper than filtering a full `/kg/all` page client-side.
/// What: `data` is the upstream `[Triple, …]` array verbatim. `subject` is
/// REQUIRED (as upstream requires it) and its absence is a `400` with the same
/// `{"error": …}` shape the sibling routes use for an invalid name — never a
/// silent full-graph fetch.
/// Test: `kg_query_route_projects_the_triples_array`,
/// `kg_query_route_requires_subject`.
// #4290: per-agent entry onto trusty-memory's subject-scoped triple query.
pub(super) async fn agent_kg_query_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Query(p): Query<KgParams>,
) -> Response {
    let Some(subject) = p.subject.filter(|s| !s.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "the `subject` query parameter is required" })),
        )
            .into_response();
    };
    // `kg_query` is a dispatcher tool, not a folded method — the branch that
    // moved this daemon onto a socket deliberately did not fold what the
    // dispatcher already routes. It answers `{subject, triples, …}` where the
    // retired route answered a bare array, so `data` carries `triples`.
    let read = KgRead::new(
        "kg_query",
        "palace",
        vec![("subject", Value::String(subject))],
        json!([]),
    )
    .projecting("triples");
    proxy_with_discovered_memory(&name, read).await
}

/// `GET /api/agents/:name/kg/count` — HTTP entry point.
///
/// Why: The browser header shows an "N triples" badge; counting server-side
/// avoids materialising the whole graph to measure it.
/// What: `data` is the upstream `{"active": N}` object verbatim, and `{"active":
/// 0}` on every degraded path — the ONE route whose empty shape is an object
/// rather than an array.
/// Test: `kg_count_route_passes_upstream_object_through`,
/// `kg_count_route_empty_state_keeps_object_shape`.
// #4290: per-agent entry onto trusty-memory's active-triple count.
pub(super) async fn agent_kg_count_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    let read = KgRead::new(
        "memory.kg_count",
        "palace_id",
        Vec::new(),
        json!({ "active": 0 }),
    );
    proxy_with_discovered_memory(&name, read).await
}

/// Shared shim body: resolve the trusty-memory socket the same way
/// `agent_stores_route` / `agent_knowledge_route` already do, then delegate.
///
/// An unresolvable data directory yields `None`, which the core reports as the
/// daemon being undiscoverable — the same degraded arm a daemon that is not
/// running takes, because from a browser pane's point of view they are the same
/// empty state with a reason.
async fn proxy_with_discovered_memory(name: &str, read: KgRead) -> Response {
    kg_proxy_at(
        &crate::agents::agents_dir_candidates(),
        name,
        trusty_common::memory_rpc::resolve_memory_socket()
            .ok()
            .as_deref(),
        read,
    )
    .await
}

/// Core proxy logic against explicit agents dirs + a trusty-memory socket.
///
/// Why: Same testability rationale as `agent_stores::stores_at` — the socket is
/// injected so tests can point at a mock daemon instead of the developer's live
/// one, and the palace-resolution branch can be exercised with no daemon at all.
/// What: resolves the agent's palace from `StoresConfig::primary()`'s `palace`
/// field, fetches the upstream KG read, and wraps the result in the module
/// doc's envelope. `400` invalid name, `404` unknown agent, `500` only when
/// the agent's own config cannot be read; EVERY palace/daemon/upstream failure
/// is a `200` carrying `connected: false` + `reason`. Malformed `agent.toml`
/// degrades to "no palace bound" plus `config_error`, matching
/// `stores_at`'s precedent, so a hand-edit typo does not blank the pane with a
/// `500`.
/// Test: `kg_subjects_route_passes_upstream_through`,
/// `kg_route_empty_state_when_no_palace_bound`,
/// `kg_route_degrades_when_memory_unreachable`,
/// `kg_route_degrades_when_palace_missing`,
/// `kg_route_unknown_agent_404`, `kg_route_rejects_traversal_name`,
/// `kg_route_degrades_on_malformed_toml`.
pub(super) async fn kg_proxy_at(
    dirs: &[PathBuf],
    name: &str,
    memory_socket: Option<&std::path::Path>,
    read: KgRead,
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
            tracing::warn!(?e, agent = name, path = %path.display(), "kg_proxy_at: read failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to read agent config" })),
            )
                .into_response();
        }
    };

    let (stores, config_error) = parse_stores(&raw);
    let Some(palace) = stores.primary().and_then(|b| b.palace.clone()) else {
        return envelope(
            None,
            Err("this agent binds no memory palace (`[[stores]].palace` is unset)".to_string()),
            &read.empty,
            config_error,
        );
    };

    let result = match memory_socket {
        None => Err("trusty-memory daemon not discoverable (is it running?)".to_string()),
        Some(socket) => fetch_upstream(socket, &palace, &read).await,
    };
    envelope(Some(palace), result, &read.empty, config_error)
}

/// Build the module doc's response envelope from a resolved palace + outcome.
fn envelope(
    palace: Option<String>,
    result: Result<Value, String>,
    empty: &Value,
    config_error: Option<String>,
) -> Response {
    let mut body = match result {
        Ok(data) => json!({ "palace": palace, "connected": true, "data": data }),
        Err(reason) => json!({
            "palace": palace,
            "connected": false,
            "reason": reason,
            "data": empty.clone(),
        }),
    };
    if let Some(err) = config_error {
        body["config_error"] = Value::String(err);
    }
    (StatusCode::OK, Json(body)).into_response()
}

/// Perform the upstream trusty-memory call, collapsing every failure mode to a
/// human-readable reason string.
///
/// Why: mirrors `stores::status::probe_palace`'s vocabulary ("does not exist" /
/// "unreachable") so a client that already renders a store card's `reason` can
/// render this one with no new cases, and so the two surfaces cannot describe
/// the same down daemon two ways.
/// What: `Ok(data)` when the daemon answers; `Err(reason)` for a not-found
/// refusal, any other refusal, or a transport failure. A read declaring
/// [`KgRead::project`] lifts that field out; a projected field the answer does
/// not carry is a reason rather than a silent empty, because the two mean
/// different things to a pane rendering "no triples". Bounded by
/// [`PROBE_TIMEOUT`] — the same ceiling every other cross-daemon read in this
/// crate uses.
/// Test: `kg_route_degrades_when_memory_unreachable`,
/// `kg_route_degrades_when_palace_missing`,
/// `kg_query_route_projects_the_triples_array`.
async fn fetch_upstream(
    socket: &std::path::Path,
    palace: &str,
    read: &KgRead,
) -> Result<Value, String> {
    let answer = trusty_common::memory_rpc::call_memory_tool_at_with_timeout(
        socket,
        read.method,
        read.params_for(palace),
        PROBE_TIMEOUT,
    )
    .await
    .map_err(|e| describe_failure(&e, palace))?;

    let Some(field) = read.project else {
        return Ok(answer);
    };
    answer.get(field).cloned().ok_or_else(|| {
        format!(
            "trusty-memory answered {} without a `{field}` field",
            read.method
        )
    })
}

/// Turn a call failure into the reason a pane renders.
///
/// A not-found refusal is the palace being absent, which is an ordinary state
/// for an agent whose palace was never created; anything else is the daemon
/// being unreachable or in trouble, and carries its own message so an operator
/// reads the reason they were given.
fn describe_failure(e: &anyhow::Error, palace: &str) -> String {
    match e.downcast_ref::<trusty_common::memory_rpc::MemoryRpcError>() {
        Some(rpc) if rpc.is_not_found() => format!("memory palace `{palace}` does not exist"),
        Some(rpc) => format!("trusty-memory refused the read for palace `{palace}`: {rpc}"),
        None => format!("trusty-memory unreachable: {e:#}"),
    }
}

/// Parse just the `[[stores]]` table out of a raw `agent.toml`.
///
/// Identical partial-read rationale to `agent_stores::parse_stores`: a
/// directory-package `agent.toml` omits `[system_prompt]`, so reading through
/// the full `AgentConfig` would reject it.
fn parse_stores(raw: &str) -> (StoresConfig, Option<String>) {
    #[derive(Deserialize)]
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

    /// Why: the palace is filled in per read rather than baked into `params`,
    /// and the two method families name it differently — `palace_id` for the
    /// folded reads, `palace` for the `kg_query` tool. A read that sent the
    /// wrong key would be refused as invalid params, which reads to a pane as
    /// the daemon being unreachable.
    /// Test: itself.
    #[test]
    fn params_for_uses_each_methods_own_palace_key() {
        let folded = KgRead::new("memory.kg_all", "palace_id", Vec::new(), json!([]));
        assert_eq!(folded.params_for("cto")["palace_id"], "cto");
        assert!(folded.params_for("cto").get("palace").is_none());

        let tool = KgRead::new("kg_query", "palace", Vec::new(), json!([]));
        assert_eq!(tool.params_for("cto")["palace"], "cto");
        assert!(tool.params_for("cto").get("palace_id").is_none());
    }

    /// Why: trusty-memory's own defaults and clamps are the single source of
    /// truth, so a parameter the caller did not send must not be invented here.
    /// Test: itself.
    #[test]
    fn page_params_forwards_only_what_the_caller_sent() {
        let p = KgParams {
            limit: Some(25),
            offset: Some(50),
            subject: None,
        };
        assert_eq!(
            page_params(&p, true),
            vec![("limit", Value::from(25)), ("offset", Value::from(50))]
        );
        assert_eq!(page_params(&p, false), vec![("limit", Value::from(25))]);

        let read = KgRead::new(
            "memory.kg_all",
            "palace_id",
            page_params(&KgParams::default(), true),
            json!([]),
        );
        let params = read.params_for("cto");
        assert!(params.get("limit").is_none());
        assert!(params.get("offset").is_none());
    }

    /// Why: a palace id and a KG subject are operator- and user-supplied
    /// strings. Over HTTP they had to be percent-encoded or a subject could
    /// escape its query parameter; as JSON params there is no such escape, and
    /// this pins that the value arrives unmangled rather than re-encoded.
    /// Test: itself.
    #[test]
    fn params_carry_awkward_values_unchanged() {
        let read = KgRead::new(
            "kg_query",
            "palace",
            vec![("subject", Value::String("Bob & Alice/2026?x".to_string()))],
            json!([]),
        );
        let params = read.params_for("owner profile/x");
        assert_eq!(params["palace"], "owner profile/x");
        assert_eq!(params["subject"], "Bob & Alice/2026?x");
    }

    #[test]
    fn parse_stores_reads_the_primary_palace() {
        let raw = "[agent]\nname = \"izzie\"\n\n[[stores]]\nname = \"bob-kb\"\npalace = \"owner-profile\"\n";
        let (stores, err) = parse_stores(raw);
        assert!(err.is_none());
        assert_eq!(
            stores.primary().and_then(|b| b.palace.as_deref()),
            Some("owner-profile")
        );
    }

    #[test]
    fn parse_stores_reports_bad_toml_without_a_palace() {
        let (stores, err) = parse_stores("not = = toml");
        assert!(stores.primary().is_none());
        assert!(err.is_some());
    }
}
