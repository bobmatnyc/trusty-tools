//! MCP (Model Context Protocol) server for trusty-analyzer.
//!
//! Why: full parity with the daemon's own surface so an MCP client gets the
//! same capabilities as a direct socket caller. The dispatcher is a pure
//! translator — MCP JSON-RPC in on stdio, daemon JSON-RPC out over the Unix
//! socket — and owns no state beyond that socket path.
//!
//! Tools (mirrors `service::rpc::METHODS`):
//!
//! | MCP tool              | Daemon method                     |
//! |-----------------------|-----------------------------------|
//! | `complexity_hotspots` | `analyze.complexity_hotspots`     |
//! | `find_smells`         | `analyze.smells`                  |
//! | `analyze_quality`     | `analyze.quality`                 |
//! | `list_facts`          | `analyze.facts_list`              |
//! | `upsert_fact`         | `analyze.facts_upsert`            |
//! | `delete_fact`         | `analyze.facts_delete`            |
//! | `cluster_concepts`    | `analyze.clusters`                |
//! | `ingest_scip`         | `analyze.scip_ingest`             |
//! | `analyzer_health`     | `analyze.health`                  |
//!
//! #6287 removed the `sse` submodule along with the `POST /mcp` + `GET /mcp/sse`
//! HTTP transport it served. ADR-0032 makes `trusty-console` the workspace's
//! only HTTP surface, so a remote MCP client reaches this dispatcher through
//! the console rather than through a second port this crate binds.

pub mod stdio;

// Why (#610): the `tools/list` JSON-Schema payload was extracted into its own
// module to keep this file under its frozen line-cap budget while the `review`
// feature (#630) adds new tools. `descriptors::base_tool_descriptors()` is the
// always-compiled base set.
pub mod descriptors;

// Why (#630): the `tr_review_*` LLM tools that embed the trusty-review pipeline
// live behind the optional `review` feature so the default build / crates.io
// publish are unaffected. When off, the module is not compiled and the names
// fall through to `UnknownTool`.
#[cfg(feature = "review")]
pub mod review;

// Why (#1104 Phase 0b): console_metrics tool for trusty-console dashboard polling.
// pub(crate): no cross-crate consumer exists (verified by workspace grep for
// `trusty_analyze::mcp::console_metrics`). The module is an internal
// implementation detail of the MCP dispatcher.
pub(crate) mod console_metrics;

// Why (#1195): the pure param/response helpers and the transport were
// extracted into sibling modules to keep this dispatcher under the 500-SLOC
// production cap. `helpers` holds pure functions; `rpc_client` holds the
// `impl AnalyzerMcpServer` socket plumbing.
mod helpers;
mod rpc_client;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use helpers::{
    index_id_or_default, optional_params, require_str, wrap_text_content, wrap_tool_error,
    wrap_tool_result,
};

pub mod error_codes {
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    #[serde(skip)]
    pub suppress: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
            suppress: false,
        }
    }

    pub fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
            suppress: false,
        }
    }

    pub fn suppressed() -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Value::Null,
            result: None,
            error: None,
            suppress: true,
        }
    }
}

/// MCP dispatcher backed by a framed socket client targeting the analyzer
/// daemon.
///
/// #6287: this held a `reqwest::Client` and a base URL. It holds a socket path
/// now — there is no connection pool to keep, because
/// `send_framed_request_capped` dials per call, and a Unix connect on a local
/// socket is microseconds rather than the TCP handshake the pool existed to
/// amortise.
#[derive(Clone)]
pub struct AnalyzerMcpServer {
    socket: PathBuf,
}

impl AnalyzerMcpServer {
    /// Point the dispatcher at the daemon's socket.
    ///
    /// The per-call timeout comes from `core::deadlines::mcp_client_timeout()`,
    /// applied in [`rpc_client`] rather than stored here. That value must clear
    /// two independent floors: the OpenRouter 120 s ceiling `deep_analysis` can
    /// spend, and the diagnostics handler's own budget. It used to be a flat
    /// 150 s, which cleared the first and missed the second — below the 180 s
    /// diagnostics deadline, so `run_diagnostics` handed an MCP caller a
    /// body-less transport timeout for any run between 150 s and the daemon's
    /// answer, which is the exact #6018 symptom the deadline was added to
    /// remove, reintroduced one layer out.
    ///
    /// Test: `mcp_client_timeout_outlives_the_daemon_and_openrouter`;
    /// `ladder_is_strictly_increasing_across_the_configurable_range` pins the
    /// ordering across the whole configurable range.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// The socket this dispatcher dials.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Translate one MCP JSON-RPC request into a daemon method call. Always
    /// returns a `Response`; transport / daemon failures are reported in-band.
    pub async fn dispatch(&self, req: Request) -> Response {
        let is_notification = req.id.is_none();
        let id = req.id.clone().unwrap_or(Value::Null);

        if req.jsonrpc != "2.0" {
            if is_notification {
                return Response::suppressed();
            }
            return Response::err(id, error_codes::INVALID_REQUEST, "jsonrpc must be \"2.0\"");
        }

        match req.method.as_str() {
            "initialize" => {
                return Response::ok(
                    id,
                    serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": { "tools": {}, "resources": {} },
                        "serverInfo": {
                            "name": "trusty-analyzer",
                            "version": env!("CARGO_PKG_VERSION"),
                        }
                    }),
                );
            }
            "notifications/initialized" | "initialized" => {
                return Response::suppressed();
            }
            "resources/list" => {
                return self.list_resources(id).await;
            }
            _ => {}
        }

        let (tool, arguments, via_tools_call) = match req.method.as_str() {
            "tools/call" => {
                let name = req
                    .params
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let args = req
                    .params
                    .get("arguments")
                    .cloned()
                    .unwrap_or(Value::Object(Default::default()));
                match name {
                    Some(n) => (n, args, true),
                    None => {
                        return Response::err(
                            id,
                            error_codes::INVALID_PARAMS,
                            "tools/call requires a 'name' field",
                        )
                    }
                }
            }
            "tools/list" => {
                return Response::ok(id, serde_json::json!({ "tools": tool_descriptors() }));
            }
            other => (other.to_string(), req.params.clone(), false),
        };

        let outcome = self.call_tool(&tool, &arguments).await;

        if via_tools_call {
            match outcome {
                Ok(value) => Response::ok(id, wrap_tool_result(&value)),
                Err(DispatchError::UnknownTool) => Response::err(
                    id,
                    error_codes::METHOD_NOT_FOUND,
                    format!("unknown tool: {tool}"),
                ),
                Err(DispatchError::InvalidParams(msg)) => Response::ok(id, wrap_tool_error(&msg)),
                Err(DispatchError::Transport(msg)) => Response::ok(id, wrap_tool_error(&msg)),
            }
        } else {
            match outcome {
                Ok(value) => Response::ok(id, wrap_text_content(&value)),
                Err(DispatchError::UnknownTool) => Response::err(
                    id,
                    error_codes::METHOD_NOT_FOUND,
                    format!("unknown tool: {tool}"),
                ),
                Err(DispatchError::InvalidParams(msg)) => {
                    Response::err(id, error_codes::INVALID_PARAMS, msg)
                }
                Err(DispatchError::Transport(msg)) => {
                    Response::err(id, error_codes::INTERNAL_ERROR, msg)
                }
            }
        }
    }

    /// Handle the JSON-RPC `resources/list` method.
    ///
    /// Why: MCP clients enumerate resources to discover what context a server
    /// can expose. The analyzer exposes each trusty-search index as a resource
    /// so clients can see, at a glance, what is available for analysis.
    /// What: calls `analyze.list_indexes` on the daemon, maps each index ID to
    /// an MCP resource descriptor (`trusty-analyzer://indexes/{id}`), and
    /// returns the `{ resources: [...] }` envelope. A daemon failure surfaces
    /// as an empty list rather than an error so the client still initializes
    /// cleanly.
    /// Test: `resources_list_returns_envelope` checks the shape when the daemon
    /// is unreachable (empty list).
    async fn list_resources(&self, id: Value) -> Response {
        let resources = match self.call(METHOD_LIST_INDEXES, Value::Null).await {
            Ok(value) => {
                // `analyze.list_indexes` returns `[{ "id": "..." }, ...]`.
                let ids: Vec<String> = value
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.get("id").and_then(Value::as_str).map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                ids.into_iter()
                    .map(|index_id| {
                        serde_json::json!({
                            "uri": format!("trusty-analyzer://indexes/{index_id}"),
                            "name": format!("Index: {index_id}"),
                            "description": "trusty-search index available for analysis",
                            "mimeType": "application/json",
                        })
                    })
                    .collect::<Vec<_>>()
            }
            Err(e) => {
                tracing::warn!("resources/list: {METHOD_LIST_INDEXES} failed: {e:?}");
                Vec::new()
            }
        };
        Response::ok(id, serde_json::json!({ "resources": resources }))
    }

    /// Top-level tool dispatch. Each tool delegates to a `handle_<tool>`
    /// function that owns parameter parsing and HTTP call construction.
    ///
    /// Why: A 130-line match block hid the per-tool logic. Per-handler
    /// functions cap dispatch cyclo at the number of tools and let each
    /// handler be tested without going through the JSON-RPC envelope.
    /// What: Looks up the tool name and forwards `(args, self)` to the
    /// handler.
    /// Test: `unknown_tool_returns_method_not_found` covers the fall-through
    /// arm; `handle_analyzer_health_calls_health_endpoint` exercises one
    /// handler directly.
    async fn call_tool(&self, tool: &str, args: &Value) -> Result<Value, DispatchError> {
        match tool {
            "complexity_hotspots" => self.handle_complexity_hotspots(args).await,
            "complexity_distribution" => self.handle_complexity_distribution(args).await,
            "find_smells" => self.handle_find_smells(args).await,
            "analyze_quality" => self.handle_analyze_quality(args).await,
            "run_diagnostics" => self.handle_run_diagnostics(args).await,
            "list_facts" => self.handle_list_facts(args).await,
            "upsert_fact" => self.handle_upsert_fact(args).await,
            "delete_fact" => self.handle_delete_fact(args).await,
            "extract_graph" => self.handle_extract_graph(args).await,
            "list_entities" => self.handle_list_entities(args).await,
            "cluster_concepts" => self.handle_cluster_concepts(args).await,
            // Why (#1104 rework): proxies the index list for the console
            // dashboard.
            "list_analyze_indexes" => self.call(METHOD_LIST_INDEXES, Value::Null).await,
            "analyzer_health" => self.handle_analyzer_health(args).await,
            "ingest_scip" => self.handle_ingest_scip(args).await,
            "extract_ner" => self.handle_extract_ner(args).await,
            "suggest_refactors" => self.handle_suggest_refactors(args).await,
            "review_diff" => self.handle_review_diff(args).await,
            "review_github_pr" => self.handle_review_github_pr(args).await,
            "deep_analysis" => self.handle_deep_analysis(args).await,
            // Why (#1104 Phase 0b): console_metrics — trusty-console dashboard poll.
            "console_metrics" => console_metrics::handle_console_metrics(self).await,
            // Why (#630): the `tr_review_*` LLM tools delegate into the embedded
            // trusty-review pipeline. Gated behind the `review` feature; when
            // off these names fall through to the `_ => UnknownTool` arm.
            #[cfg(feature = "review")]
            "tr_review_pr" | "tr_review_diff" | "tr_review_health" => {
                review::handle_tr_review(tool, args).await
            }
            _ => Err(DispatchError::UnknownTool),
        }
    }

    async fn handle_complexity_hotspots(&self, args: &Value) -> Result<Value, DispatchError> {
        let top_n = args.get("top_n").and_then(Value::as_u64).unwrap_or(20);
        self.call(
            "analyze.complexity_hotspots",
            serde_json::json!({ "index_id": index_id_or_default(args), "top_n": top_n }),
        )
        .await
    }

    /// #5320: the exhaustive counterpart to `complexity_hotspots` — forwards to
    /// `analyze.complexity_distribution`, which returns all five A–F bands plus
    /// the counted total.
    async fn handle_complexity_distribution(&self, args: &Value) -> Result<Value, DispatchError> {
        self.call("analyze.complexity_distribution", index_params(args))
            .await
    }

    async fn handle_find_smells(&self, args: &Value) -> Result<Value, DispatchError> {
        self.call(
            "analyze.smells",
            with_index(args, optional_params(args, &["limit", "offset", "omit_content"])),
        )
        .await
    }

    async fn handle_analyze_quality(&self, args: &Value) -> Result<Value, DispatchError> {
        self.call("analyze.quality", index_params(args)).await
    }

    /// Handle the `run_diagnostics` tool: forward to `analyze.diagnostics`,
    /// which runs the discovered external static-analysis tools (clippy, ruff,
    /// biome, ...) on demand.
    async fn handle_run_diagnostics(&self, args: &Value) -> Result<Value, DispatchError> {
        self.call(
            "analyze.diagnostics",
            with_index(
                args,
                optional_params(args, &["language", "tools", "limit", "offset"]),
            ),
        )
        .await
    }

    async fn handle_list_facts(&self, args: &Value) -> Result<Value, DispatchError> {
        let params = optional_params(args, &["subject", "predicate", "object"]);
        self.call("analyze.facts_list", Value::Object(params)).await
    }

    async fn handle_upsert_fact(&self, args: &Value) -> Result<Value, DispatchError> {
        let subject = require_str(args, "subject")?;
        let predicate = require_str(args, "predicate")?;
        let object = require_str(args, "object")?;
        let index_id = require_str(args, "index_id")?;
        let confidence = args
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(1.0);
        let provenance = args
            .get("provenance")
            .cloned()
            .unwrap_or_else(|| Value::Array(vec![]));
        let body = serde_json::json!({
            "subject": subject,
            "predicate": predicate,
            "object": object,
            "index_id": index_id,
            "confidence": confidence,
            "provenance": provenance,
        });
        self.call("analyze.facts_upsert", body).await
    }

    async fn handle_delete_fact(&self, args: &Value) -> Result<Value, DispatchError> {
        let id = args
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| DispatchError::InvalidParams("missing 'id' (u64)".into()))?;
        self.call("analyze.facts_delete", serde_json::json!({ "id": id }))
            .await
    }

    async fn handle_extract_graph(&self, args: &Value) -> Result<Value, DispatchError> {
        self.call(
            "analyze.graph",
            with_index(args, optional_params(args, &["language"])),
        )
        .await
    }

    async fn handle_list_entities(&self, args: &Value) -> Result<Value, DispatchError> {
        self.call(
            "analyze.entities",
            with_index(args, optional_params(args, &["kind", "language"])),
        )
        .await
    }

    async fn handle_cluster_concepts(&self, args: &Value) -> Result<Value, DispatchError> {
        let k = args.get("k").and_then(Value::as_u64).unwrap_or(8);
        let mut params = optional_params(args, &["method"]);
        params.insert("k".into(), Value::from(k));
        self.call("analyze.clusters", with_index(args, params)).await
    }

    async fn handle_analyzer_health(&self, _args: &Value) -> Result<Value, DispatchError> {
        self.call(METHOD_HEALTH, Value::Null).await
    }

    async fn handle_suggest_refactors(&self, args: &Value) -> Result<Value, DispatchError> {
        let top_k = args.get("top_k").and_then(Value::as_u64).unwrap_or(20);
        let mut params = optional_params(args, &["file", "min_severity"]);
        params.insert("top_k".into(), Value::from(top_k));
        self.call("analyze.refactor_suggestions", with_index(args, params))
            .await
    }

    async fn handle_extract_ner(&self, args: &Value) -> Result<Value, DispatchError> {
        let top_k = args.get("top_k").and_then(Value::as_u64).unwrap_or(50);
        self.call(
            "analyze.ner",
            serde_json::json!({ "index_id": index_id_or_default(args), "top_k": top_k }),
        )
        .await
    }

    /// Handle the `ingest_scip` tool: forward to `analyze.scip_ingest`.
    ///
    /// #6287: the base64 decode that used to happen here moved to the daemon.
    /// The tool's own argument was always base64 and the HTTP endpoint took raw
    /// bytes, so this handler decoded and the daemon re-parsed; a JSON-RPC frame
    /// carries no binary, so the base64 travels as-is and one decoder is left.
    /// The tool's schema is unchanged.
    async fn handle_ingest_scip(&self, args: &Value) -> Result<Value, DispatchError> {
        let b64 = require_str(args, "scip_base64")?;
        self.call(
            "analyze.scip_ingest",
            serde_json::json!({
                "index_id": index_id_or_default(args),
                "scip_base64": b64,
            }),
        )
        .await
    }

    /// Handle the `review_diff` tool: forward a unified diff to
    /// `analyze.review`.
    ///
    /// Why: parity with the daemon method so MCP clients (Claude Code) can ask
    /// for a PR review without shelling out. Like every other analyzer tool,
    /// review is backed by trusty-search: the daemon fetches the named index's
    /// chunk corpus to cross-reference the diff.
    /// What: requires a `diff` string param and an `index_id` string param, and
    /// sends both as `params`.
    /// Test: `review_diff_requires_diff_param` and
    /// `review_diff_requires_index_id` check the missing-param paths.
    async fn handle_review_diff(&self, args: &Value) -> Result<Value, DispatchError> {
        let diff = require_str(args, "diff")?;
        let index_id = require_str(args, "index_id")?;
        self.call(
            "analyze.review",
            serde_json::json!({ "index_id": index_id, "diff": diff }),
        )
        .await
    }

    /// Handle the `deep_analysis` MCP tool: forward to `analyze.deep_analysis`.
    ///
    /// Why: pairs with the daemon method so MCP clients can opt into the
    /// LLM-augmented analysis without going through the deterministic
    /// `review_diff` path. Keeps the two surfaces separate so `review_diff`
    /// remains cheap and deterministic.
    /// What: requires `index_id`; optional `model` overrides the daemon
    /// default.
    /// Test: `deep_analysis_requires_index_id` and
    /// `deep_analysis_calls_the_daemon_method` cover param construction.
    async fn handle_deep_analysis(&self, args: &Value) -> Result<Value, DispatchError> {
        let index_id = require_str(args, "index_id")?;
        let mut params = optional_params(args, &["model"]);
        params.insert("index_id".into(), Value::from(index_id));
        // The daemon method accepts an optional pre-computed `report`; the MCP
        // tool surface deliberately keeps the schema minimal (index_id +
        // model) — re-running the synthesis on the daemon is the simpler
        // ergonomics for AI clients.
        self.call("analyze.deep_analysis", Value::Object(params))
            .await
    }

    /// Handle the `review_github_pr` tool: forward to
    /// `analyze.review_github_pr`.
    ///
    /// Why: parity with the daemon method so MCP clients can review a GitHub PR
    /// by number. The daemon owns the GitHub token and the fetch/analyze/comment
    /// pipeline; the MCP server is a pure translator.
    /// What: requires `owner`, `repo`, `pr`, and `index_id`; `post_comment` is
    /// optional (default false). Sends a `GithubPrRequest`-shaped params object.
    /// Test: `review_github_pr_requires_owner` checks the missing-param path.
    async fn handle_review_github_pr(&self, args: &Value) -> Result<Value, DispatchError> {
        let owner = require_str(args, "owner")?;
        let repo = require_str(args, "repo")?;
        let pr = args
            .get("pr")
            .and_then(Value::as_u64)
            .ok_or_else(|| DispatchError::InvalidParams("missing or non-integer 'pr'".into()))?;
        let index_id = require_str(args, "index_id")?;
        let post_comment = args
            .get("post_comment")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let body = serde_json::json!({
            "owner": owner,
            "repo": repo,
            "pr": pr,
            "index_id": index_id,
            "post_comment": post_comment,
        });
        self.call("analyze.review_github_pr", body).await
    }
}

/// The daemon's health method, as this client names it.
///
/// Duplicated rather than imported from `crate::service::rpc`: the `service`
/// module is behind the `http-server` feature and this dispatcher is not, so an
/// import would make the MCP surface unbuildable in a `--no-default-features`
/// library build. `mcp_names_the_methods_the_router_registers` is what keeps
/// the two equal.
const METHOD_HEALTH: &str = "analyze.health";

/// The daemon's index-list method. Duplicated for the reason above.
const METHOD_LIST_INDEXES: &str = "analyze.list_indexes";

/// Put `index_id` into an already-built params object.
///
/// Why: every per-index method needs it and nine tools accept it under either
/// `index` or `index_id` (`index_id_or_default`). Inserting it last is
/// deliberate — a caller cannot smuggle a second `index_id` in through
/// `optional_params` and have it win.
fn with_index(args: &Value, mut params: serde_json::Map<String, Value>) -> Value {
    params.insert("index_id".into(), Value::from(index_id_or_default(args)));
    Value::Object(params)
}

/// A params object carrying nothing but `index_id`.
fn index_params(args: &Value) -> Value {
    serde_json::json!({ "index_id": index_id_or_default(args) })
}

#[derive(Debug)]
enum DispatchError {
    UnknownTool,
    InvalidParams(String),
    Transport(String),
}

/// Assemble the full `tools/list` descriptor array.
///
/// Why: base descriptors live in `descriptors.rs` (line-cap budget, #610);
/// `console_metrics` (#1104) is always appended; `tr_review_*` (#630) are
/// appended only under `feature = "review"`.
/// What: returns a `serde_json::Value` array.
/// Test: `tools_list_contains_full_surface` (base names).
pub fn tool_descriptors() -> Value {
    let mut tools = descriptors::base_tool_descriptors();
    if let Some(arr) = tools.as_array_mut() {
        arr.push(console_metrics::descriptor());
        // #630 / #5205: the descriptors are always compiled (see
        // `descriptors::review_tool_descriptors`); only the append is gated.
        #[cfg(feature = "review")]
        arr.extend(descriptors::review_tool_descriptors());
    }
    tools
}

#[cfg(test)]
mod helpers_tests;

#[cfg(test)]
mod tests;
