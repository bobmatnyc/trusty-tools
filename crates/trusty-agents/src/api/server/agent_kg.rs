//! `GET /api/agents/:name/kg*` — a READ-ONLY proxy onto the agent's bound
//! memory palace's Knowledge Graph (#4290).
//!
//! Why: trusty-memory already ships the entire palace-scoped KG read surface
//! (`crates/trusty-memory/src/web/kg_routes.rs`: `/kg`, `/kg/subjects`,
//! `/kg/subjects_with_counts`, `/kg/all`, `/kg/count`), but every one of those
//! routes is keyed by PALACE id. The Knowledge-Graph browser in the agents GUI
//! is keyed by AGENT — and the mapping between the two lives in this crate
//! (`[[stores]].palace`, resolved by `StoresConfig::primary()`). Without this
//! module a client would have to read `GET /api/agents/:name/stores`, dig the
//! palace id out of the first binding, discover the trusty-memory daemon's
//! address for itself, and then talk to a second origin — reimplementing
//! per-agent palace resolution in TypeScript and coupling the GUI to a daemon
//! it has no discovery path for. This is the plumbing that removes all of
//! that: agent in, upstream KG JSON out.
//!
//! **This module holds no KG query logic of its own.** `MemoryService` /
//! `kg_routes.rs` stays the single entry point for every KG read; the upstream
//! body is passed through VERBATIM under `data` — never reshaped, re-ranked,
//! truncated or re-sorted here — so the GUI and the MCP `kg_query` tool can
//! never disagree about what the graph contains.
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
//! What: four thin axum shims ([`agent_kg_subjects_route`],
//! [`agent_kg_all_route`], [`agent_kg_query_route`], [`agent_kg_count_route`])
//! over one testable core, [`kg_proxy_at`], which takes the agents-dir list
//! and the trusty-memory base URL explicitly (the injected-dependency
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
/// `suffix` is appended under `/api/v1/palaces/{palace}/`; `pairs` are the
/// query parameters to forward verbatim; `empty` is the payload shape used for
/// `data` on every degraded path (see the module doc's envelope).
pub(super) struct KgRead {
    suffix: &'static str,
    pairs: Vec<(&'static str, String)>,
    empty: Value,
}

impl KgRead {
    /// Construct one read. `pub(super)` so `super::tests::agent_kg` can drive
    /// [`kg_proxy_at`] with an arbitrary read without a test-only shim on the
    /// production type.
    pub(super) fn new(
        suffix: &'static str,
        pairs: Vec<(&'static str, String)>,
        empty: Value,
    ) -> Self {
        Self {
            suffix,
            pairs,
            empty,
        }
    }
}

/// `limit`/`offset` as forwardable query pairs, omitting whichever the caller
/// did not send.
fn page_pairs(p: &KgParams, with_offset: bool) -> Vec<(&'static str, String)> {
    let mut pairs = Vec::new();
    if let Some(limit) = p.limit {
        pairs.push(("limit", limit.to_string()));
    }
    if with_offset && let Some(offset) = p.offset {
        pairs.push(("offset", offset.to_string()));
    }
    pairs
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
    let read = KgRead::new("kg/subjects_with_counts", page_pairs(&p, false), json!([]));
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
    let read = KgRead::new("kg/all", page_pairs(&p, true), json!([]));
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
/// Test: `kg_query_route_forwards_subject`,
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
    let read = KgRead::new("kg", vec![("subject", subject)], json!([]));
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
    let read = KgRead::new("kg/count", Vec::new(), json!({ "active": 0 }));
    proxy_with_discovered_memory(&name, read).await
}

/// Shared shim body: discover the trusty-memory daemon the same way
/// `agent_stores_route` / `agent_knowledge_route` already do, then delegate.
async fn proxy_with_discovered_memory(name: &str, read: KgRead) -> Response {
    kg_proxy_at(
        &crate::agents::agents_dir_candidates(),
        name,
        trusty_common::resolve_daemon_base_url("trusty-memory").as_deref(),
        read,
    )
    .await
}

/// Core proxy logic against explicit agents dirs + a trusty-memory base URL.
///
/// Why: Same testability rationale as `agent_stores::stores_at` — the daemon
/// URL is injected so tests can point at a mock server instead of the
/// developer's live daemon, and the palace-resolution branch can be exercised
/// with no network at all.
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
    memory_base: Option<&str>,
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

    let result = match memory_base {
        None => Err("trusty-memory daemon not discoverable (is it running?)".to_string()),
        Some(base) => fetch_upstream(base, &palace, &read).await,
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

/// Perform the upstream trusty-memory GET, collapsing every failure mode to a
/// human-readable reason string.
///
/// Why: mirrors `stores::status::probe_palace`'s vocabulary verbatim ("does
/// not exist" / "returned HTTP N" / "unreachable") so a client that already
/// renders a store card's `reason` can render this one with no new cases, and
/// so the two surfaces cannot describe the same down daemon two ways.
/// What: `Ok(body)` for a 2xx with parseable JSON; `Err(reason)` for a 404,
/// any other non-2xx, a transport error, or an unreadable body. Bounded by
/// [`PROBE_TIMEOUT`] — the same ceiling every other cross-daemon read in this
/// crate uses.
/// Test: `kg_route_degrades_when_memory_unreachable`,
/// `kg_route_degrades_when_palace_missing`,
/// `kg_route_degrades_on_unreadable_upstream_body`.
async fn fetch_upstream(base: &str, palace: &str, read: &KgRead) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .map_err(|e| format!("HTTP client unavailable: {e}"))?;
    let url = upstream_url(base, palace, read)?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("trusty-memory unreachable at {base}: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(format!("memory palace `{palace}` does not exist"));
    }
    if !resp.status().is_success() {
        return Err(format!(
            "trusty-memory returned HTTP {} for palace `{palace}`",
            resp.status()
        ));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("trusty-memory returned an unreadable KG body: {e}"))
}

/// The upstream URL for one read, with every dynamic segment percent-encoded.
///
/// Why: a palace id and a KG subject are operator/user-supplied strings that
/// may contain spaces, `#`, `?` or `/`. String-formatting them into a URL
/// would let a subject silently escape its query parameter (or a palace id
/// invent extra path segments), so both go through `url`'s own encoders:
/// `path_segments_mut` for the palace, `query_pairs_mut` for the parameters.
/// Test: `upstream_url_percent_encodes_palace_and_subject`.
fn upstream_url(base: &str, palace: &str, read: &KgRead) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(base)
        .map_err(|e| format!("invalid trusty-memory base URL `{base}`: {e}"))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| format!("trusty-memory base URL `{base}` cannot carry a path"))?;
        segments.clear().extend(["api", "v1", "palaces", palace]);
        segments.extend(read.suffix.split('/'));
    }
    if !read.pairs.is_empty() {
        let mut query = url.query_pairs_mut();
        for (key, value) in &read.pairs {
            query.append_pair(key, value);
        }
    }
    Ok(url)
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

    #[test]
    fn upstream_url_percent_encodes_palace_and_subject() {
        let read = KgRead::new(
            "kg",
            vec![("subject", "Bob & Alice/2026?x".to_string())],
            json!([]),
        );
        let url = upstream_url("http://127.0.0.1:7777", "owner profile/x", &read).unwrap();
        assert_eq!(
            url.path(),
            "/api/v1/palaces/owner%20profile%2Fx/kg",
            "a palace id must never invent extra path segments"
        );
        assert_eq!(
            url.query().unwrap(),
            "subject=Bob+%26+Alice%2F2026%3Fx",
            "a subject must never escape its query parameter"
        );
    }

    #[test]
    fn upstream_url_omits_absent_page_params() {
        let read = KgRead::new("kg/all", page_pairs(&KgParams::default(), true), json!([]));
        let url = upstream_url("http://127.0.0.1:7777/", "cto", &read).unwrap();
        assert_eq!(url.path(), "/api/v1/palaces/cto/kg/all");
        assert!(
            url.query().is_none(),
            "trusty-memory's own defaults must stay the single source of truth"
        );
    }

    #[test]
    fn page_pairs_forwards_only_what_the_caller_sent() {
        let p = KgParams {
            limit: Some(25),
            offset: Some(50),
            subject: None,
        };
        assert_eq!(
            page_pairs(&p, true),
            vec![("limit", "25".to_string()), ("offset", "50".to_string())]
        );
        assert_eq!(page_pairs(&p, false), vec![("limit", "25".to_string())]);
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
