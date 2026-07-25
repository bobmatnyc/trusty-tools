//! Live resolution of OKG store bindings against the running daemons
//! (#3816/#3864, DOC-54 SPEC-AGENTS-04 §5.1).
//!
//! Why: A binding in `agent.toml` is a claim, not a fact. The GUI's OKG
//! Stores pane previously rendered a hardcoded "not connected" placeholder
//! precisely because nothing checked whether the named index existed; this
//! module turns the claim into an observed status by asking trusty-search
//! (`GET /indexes/{id}/status`) and trusty-memory (`GET
//! /api/v1/palaces/{id}/drawers?limit=1`) directly. Every failure mode —
//! daemon undiscoverable, connection refused, 404, malformed body, timeout —
//! collapses to `connected: false` plus a human-readable `reason`, mirroring
//! `crate::system_status::daemons`' established "down is a normal, reportable
//! state" contract. A bound store that cannot be resolved must never stop an
//! agent booting.
//! What: [`StoreStatus`] is the per-store report (serialized straight to the
//! sidecar API and the GUI card). [`resolve_store_statuses`] resolves a whole
//! [`StoresConfig`]; base URLs are injected so tests can point at a mock
//! server rather than the developer's live daemons.
//! Test: `super::status::tests` — a mock trusty-search/-memory router covers
//! connected, 404, and unreachable; config-validation short-circuiting is
//! covered without any network at all.

use std::time::Duration;

use serde::Serialize;

use super::config::{AgentStoreBinding, StoresConfig};

/// Per-probe timeout.
///
/// Why: Identical rationale (and value) to
/// [`crate::system_status::daemons::PROBE_TIMEOUT`] — a daemon that accepts
/// the TCP connection but never answers must degrade to "not connected"
/// within a couple of seconds rather than stalling agent boot or an API
/// request behind it.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// One bound store's resolved, live status.
///
/// Why: The GUI card and the sidecar API need exactly one shape covering
/// both "CONNECTED, here is the index and how big it is" and "NOT CONNECTED,
/// here is why" — a client should never have to distinguish an absent field
/// from a failed probe. `reason` is `Some` only when `connected` is false.
/// What: `name`/`tree`/`index`/`palace` echo the resolved binding (defaults
/// already applied); `connected` reflects the SEARCH INDEX only, since that
/// is the corpus `vector_search` routes to. Palace health is reported
/// separately in `palace_connected`/`palace_reason` so a missing palace
/// downgrades one line of the card rather than the whole store.
/// Test: `resolves_connected_store_with_stats`,
/// `reports_missing_index_as_not_connected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoreStatus {
    pub name: String,
    pub tree: String,
    pub index: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub palace: Option<String>,
    pub connected: bool,
    /// Why the store is not connected. `None` when `connected` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Chunk count reported by the index, when the probe succeeded and the
    /// daemon supplied one. Cheap — it comes from the same status call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_count: Option<u64>,
    /// The indexed root directory, when reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_path: Option<String>,
    /// The index's own readiness string (`"ready"`, `"indexing"`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_status: Option<String>,
    /// `None` when the binding declares no palace; otherwise whether the
    /// palace was observed to exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub palace_connected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub palace_reason: Option<String>,
}

impl StoreStatus {
    /// A status carrying only the binding's identity and a disconnect reason.
    ///
    /// Why: Every not-connected path (invalid config, undiscoverable daemon,
    /// 404, timeout) produces the same shape; building it in one place keeps
    /// the `Option` fields from drifting out of sync.
    fn disconnected(binding: &AgentStoreBinding, agent_name: &str, reason: String) -> Self {
        Self {
            name: binding.name.clone(),
            tree: binding.resolved_tree(agent_name),
            index: binding.resolved_index().to_string(),
            palace: binding.palace.clone(),
            connected: false,
            reason: Some(reason),
            chunk_count: None,
            root_path: None,
            index_status: None,
            palace_connected: None,
            palace_reason: None,
        }
    }
}

/// Resolve every binding in `stores` against the live daemons.
///
/// Why: One entry point for both the sidecar API (`GET
/// /api/agents/:name/stores`) and any boot-time reporting, so the GUI and the
/// logs can never disagree about whether a store is connected.
/// What: `search_base`/`memory_base` are full base URLs (e.g.
/// `http://127.0.0.1:7878`); `None` means the daemon was not discoverable and
/// every store resolves to not-connected with that as the reason — injected
/// rather than resolved internally so tests can drive a mock server. Returns
/// one [`StoreStatus`] per binding, in declaration order. Never errors.
/// Test: `resolves_connected_store_with_stats`,
/// `reports_missing_index_as_not_connected`,
/// `reports_undiscoverable_search_daemon`,
/// `invalid_binding_short_circuits_without_network`.
pub async fn resolve_store_statuses(
    agent_name: &str,
    stores: &StoresConfig,
    search_base: Option<&str>,
    memory_base: Option<&str>,
) -> Vec<StoreStatus> {
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            // Building a client cannot normally fail; if it does, report it
            // rather than panicking an agent's boot path.
            return stores
                .bindings
                .iter()
                .map(|b| {
                    StoreStatus::disconnected(
                        b,
                        agent_name,
                        format!("HTTP client unavailable: {e}"),
                    )
                })
                .collect();
        }
    };

    let mut out = Vec::with_capacity(stores.bindings.len());
    for binding in &stores.bindings {
        out.push(resolve_one(&client, agent_name, binding, search_base, memory_base).await);
    }
    out
}

/// Resolve a single binding. See [`resolve_store_statuses`].
async fn resolve_one(
    client: &reqwest::Client,
    agent_name: &str,
    binding: &AgentStoreBinding,
    search_base: Option<&str>,
    memory_base: Option<&str>,
) -> StoreStatus {
    // Config problems short-circuit before any network call — an unusable
    // binding has nothing meaningful to probe.
    if let Some(issue) = binding.validate() {
        return StoreStatus::disconnected(binding, agent_name, issue);
    }

    let index = binding.resolved_index().to_string();
    let Some(search_base) = search_base else {
        return StoreStatus::disconnected(
            binding,
            agent_name,
            "trusty-search daemon not discoverable (no address file; is it running?)".to_string(),
        );
    };

    let url = format!(
        "{}/indexes/{}/status",
        search_base.trim_end_matches('/'),
        index
    );
    let mut status = match client.get(&url).send().await {
        Err(e) => {
            return StoreStatus::disconnected(
                binding,
                agent_name,
                format!("trusty-search unreachable at {search_base}: {e}"),
            );
        }
        Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
            return StoreStatus::disconnected(
                binding,
                agent_name,
                format!("search index `{index}` is not registered on the trusty-search daemon"),
            );
        }
        Ok(resp) if !resp.status().is_success() => {
            let code = resp.status();
            return StoreStatus::disconnected(
                binding,
                agent_name,
                format!("trusty-search returned HTTP {code} for index `{index}`"),
            );
        }
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Err(e) => {
                return StoreStatus::disconnected(
                    binding,
                    agent_name,
                    format!("trusty-search returned an unreadable status body: {e}"),
                );
            }
            Ok(body) => StoreStatus {
                name: binding.name.clone(),
                tree: binding.resolved_tree(agent_name),
                index: index.clone(),
                palace: binding.palace.clone(),
                connected: true,
                reason: None,
                chunk_count: body.get("chunk_count").and_then(serde_json::Value::as_u64),
                root_path: body
                    .get("root_path")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                index_status: body
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                palace_connected: None,
                palace_reason: None,
            },
        },
    };

    if let Some(palace) = &binding.palace {
        let (ok, reason) = probe_palace(client, memory_base, palace).await;
        status.palace_connected = Some(ok);
        status.palace_reason = reason;
    }

    status
}

/// Probe whether `palace` exists on the trusty-memory daemon.
///
/// Why: `GET /api/v1/palaces/{id}/drawers?limit=1` is the cheapest existence
/// check that does NOT create anything — the `POST /api/v1/palaces` route the
/// rest of this crate uses (`memory::trusty_client`) is an ensure/create and
/// would silently conjure a palace that a status probe has no business
/// creating. This mirrors `api::server::workstreams::fetch_drawers`' existing
/// read path.
/// What: `(true, None)` when the daemon answers 2xx; `(false, Some(reason))`
/// for every other outcome.
/// Test: `reports_missing_palace_without_downgrading_index`.
async fn probe_palace(
    client: &reqwest::Client,
    memory_base: Option<&str>,
    palace: &str,
) -> (bool, Option<String>) {
    let Some(base) = memory_base else {
        return (
            false,
            Some("trusty-memory daemon not discoverable (is it running?)".to_string()),
        );
    };
    let url = format!(
        "{}/api/v1/palaces/{}/drawers?limit=1",
        base.trim_end_matches('/'),
        palace
    );
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => (true, None),
        Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => (
            false,
            Some(format!("memory palace `{palace}` does not exist")),
        ),
        Ok(resp) => (
            false,
            Some(format!(
                "trusty-memory returned HTTP {} for palace `{palace}`",
                resp.status()
            )),
        ),
        Err(e) => (false, Some(format!("trusty-memory unreachable: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::Path, http::StatusCode, routing::get};
    use serde_json::json;
    use tokio::net::TcpListener;

    /// Spin up a mock daemon exposing both the trusty-search status route and
    /// the trusty-memory drawers route, and return its base URL.
    ///
    /// Why: Testing against the developer's real daemons would make the suite
    /// depend on machine state; a mock keeps the probe logic (status codes,
    /// body parsing, reason strings) under test deterministically.
    async fn mock_daemon() -> String {
        let app = Router::new()
            .route(
                "/indexes/{id}/status",
                get(|Path(id): Path<String>| async move {
                    if id == "bob-kb" {
                        (
                            StatusCode::OK,
                            Json(json!({
                                "index_id": "bob-kb",
                                "chunk_count": 552,
                                "root_path": "/Users/masa/trusty-agents/bob-kb",
                                "status": "ready",
                            })),
                        )
                    } else {
                        (
                            StatusCode::NOT_FOUND,
                            Json(json!({"error": "no such index"})),
                        )
                    }
                }),
            )
            .route(
                "/api/v1/palaces/{id}/drawers",
                get(|Path(id): Path<String>| async move {
                    if id == "owner-profile" {
                        (StatusCode::OK, Json(json!({"drawers": []})))
                    } else {
                        (StatusCode::NOT_FOUND, Json(json!({"error": "no palace"})))
                    }
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn stores_toml(raw: &str) -> StoresConfig {
        #[derive(serde::Deserialize)]
        struct W {
            #[serde(default)]
            stores: StoresConfig,
        }
        toml::from_str::<W>(raw).unwrap().stores
    }

    #[tokio::test]
    async fn resolves_connected_store_with_stats() {
        let base = mock_daemon().await;
        let stores = stores_toml(
            r#"
[[stores]]
name = "bob-kb"
tree = "okg://izzie"
index = "bob-kb"
palace = "owner-profile"
"#,
        );
        let out = resolve_store_statuses("izzie", &stores, Some(&base), Some(&base)).await;
        assert_eq!(out.len(), 1);
        let s = &out[0];
        assert!(s.connected, "expected connected, got reason {:?}", s.reason);
        assert_eq!(s.reason, None);
        assert_eq!(s.chunk_count, Some(552));
        assert_eq!(s.index_status.as_deref(), Some("ready"));
        assert_eq!(s.tree, "okg://izzie");
        assert_eq!(s.palace_connected, Some(true));
        assert_eq!(s.palace_reason, None);
    }

    #[tokio::test]
    async fn reports_missing_index_as_not_connected() {
        let base = mock_daemon().await;
        let stores = stores_toml("[[stores]]\nname = \"nope\"\n");
        let out = resolve_store_statuses("izzie", &stores, Some(&base), Some(&base)).await;
        assert!(!out[0].connected);
        let reason = out[0].reason.as_deref().unwrap();
        assert!(reason.contains("not registered"), "reason was: {reason}");
        // The binding identity must still round-trip so the GUI can render
        // WHAT is disconnected, not just that something is.
        assert_eq!(out[0].index, "nope");
        assert_eq!(out[0].tree, "okg://izzie");
    }

    #[tokio::test]
    async fn reports_missing_palace_without_downgrading_index() {
        let base = mock_daemon().await;
        let stores = stores_toml(
            "[[stores]]\nname = \"bob-kb\"\nindex = \"bob-kb\"\npalace = \"ghost-palace\"\n",
        );
        let out = resolve_store_statuses("izzie", &stores, Some(&base), Some(&base)).await;
        assert!(
            out[0].connected,
            "index health must not depend on the palace"
        );
        assert_eq!(out[0].palace_connected, Some(false));
        assert!(
            out[0]
                .palace_reason
                .as_deref()
                .unwrap()
                .contains("does not exist")
        );
    }

    #[tokio::test]
    async fn reports_undiscoverable_search_daemon() {
        let stores = stores_toml("[[stores]]\nname = \"bob-kb\"\n");
        let out = resolve_store_statuses("izzie", &stores, None, None).await;
        assert!(!out[0].connected);
        assert!(
            out[0]
                .reason
                .as_deref()
                .unwrap()
                .contains("not discoverable")
        );
    }

    #[tokio::test]
    async fn invalid_binding_short_circuits_without_network() {
        // A bad tree scheme must be reported as the reason WITHOUT any probe
        // — note the deliberately unroutable base URL: if this test made a
        // network call it would report a connection error instead.
        let stores = stores_toml("[[stores]]\nname = \"kb\"\ntree = \"https://example.com\"\n");
        let out = resolve_store_statuses("izzie", &stores, Some("http://127.0.0.1:1"), None).await;
        assert!(!out[0].connected);
        assert!(out[0].reason.as_deref().unwrap().contains("okg://"));
    }

    #[tokio::test]
    async fn no_bindings_resolves_to_empty() {
        let out = resolve_store_statuses("izzie", &StoresConfig::default(), None, None).await;
        assert!(out.is_empty());
    }
}
