//! MCP (Model Context Protocol) server for trusty-analyzer.
//!
//! Why: full parity with the HTTP surface so an MCP client gets the same
//! capabilities as a curl user. The dispatcher is a pure translator — JSON-RPC
//! in, HTTP out — and owns no state beyond a `reqwest::Client` and the
//! analyzer daemon's base URL.
//!
//! Tools (mirrors `trusty-analyzer-service`):
//!
//! | MCP tool              | Daemon endpoint                              |
//! |-----------------------|----------------------------------------------|
//! | `complexity_hotspots` | `GET /indexes/:id/complexity_hotspots`       |
//! | `find_smells`         | `GET /indexes/:id/smells`                    |
//! | `analyze_quality`     | `GET /indexes/:id/quality`                   |
//! | `list_facts`          | `GET /facts`                                 |
//! | `upsert_fact`         | `POST /facts`                                |
//! | `delete_fact`         | `DELETE /facts/:id`                          |
//! | `cluster_concepts`    | `GET /indexes/:id/clusters`                  |
//! | `ingest_scip`         | `POST /indexes/:id/scip`                     |
//! | `analyzer_health`     | `GET /health`                                |

// Why (issue #249): the `sse` submodule is the axum HTTP/SSE transport and
// only compiles when the `http-server` feature is enabled. The `stdio`
// transport stays unconditional — MCP clients (Claude Code) that spawn the
// dispatcher as a subprocess only need stdio, never axum.
#[cfg(feature = "http-server")]
pub mod sse;
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

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// Per-request timeout for the MCP→daemon HTTP client (in seconds).
///
/// Why: the `deep_analysis` tool calls `POST /analyze/deep`, which in turn
/// calls OpenRouter with up to 120 s allowed (matching `OPENROUTER_REQUEST_TIMEOUT_SECS`
/// in `trusty-common/src/chat.rs`). Adding 30 s of headroom for report synthesis
/// and network round-trips gives 150 s total — safely above the 120 s
/// OpenRouter ceiling so slow or large-context models are not systematically
/// killed at the MCP transport layer before the daemon's own timeout fires.
/// What: sets the per-request timeout passed to `reqwest::ClientBuilder`.
/// Test: `mcp_client_timeout_exceeds_openrouter_ceiling` asserts this value
/// is strictly greater than 120 (the OpenRouter maximum).
const DEEP_ANALYSIS_MCP_TIMEOUT_SECS: u64 = 150;

/// MCP dispatcher backed by an HTTP client targeting the analyzer daemon.
#[derive(Clone)]
pub struct AnalyzerMcpServer {
    base_url: String,
    http: reqwest::Client,
}

impl AnalyzerMcpServer {
    /// Why: without timeouts MCP tool calls hang forever if the analyze daemon
    /// is slow or unresponsive, blocking the entire stdio dispatch loop. The
    /// per-request timeout is set to [`DEEP_ANALYSIS_MCP_TIMEOUT_SECS`] (150 s)
    /// so LLM-backed tools like `deep_analysis` — which can take up to the
    /// OpenRouter 120 s ceiling plus synthesis headroom — complete without
    /// being aborted at the transport layer.
    /// What: builds a `reqwest::Client` with a 150 s per-request timeout and a
    /// 5 s TCP connect timeout. The connect timeout is kept short because the
    /// daemon is always local; the long request timeout is required only for
    /// the LLM call path.
    /// Test: `cargo test -p trusty-analyze -- mcp` exercises dispatch paths;
    /// `mcp_client_timeout_exceeds_openrouter_ceiling` asserts the const value.
    pub fn new(base_url: impl Into<String>) -> Self {
        let http = reqwest::ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(
                DEEP_ANALYSIS_MCP_TIMEOUT_SECS,
            ))
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("reqwest ClientBuilder is infallible with valid config");
        Self {
            base_url: base_url.into(),
            http,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Translate one JSON-RPC request into a daemon HTTP call. Always returns
    /// a `Response`; transport / daemon failures are reported in-band.
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
    /// What: calls `GET /indexes` on the daemon, maps each index ID to an MCP
    /// resource descriptor (`trusty-analyzer://indexes/{id}`), and returns the
    /// `{ resources: [...] }` envelope. A daemon failure surfaces as an empty
    /// list rather than an error so the client still initializes cleanly.
    /// Test: `resources_list_returns_envelope` checks the shape when the daemon
    /// is unreachable (empty list).
    async fn list_resources(&self, id: Value) -> Response {
        let resources = match self.get("/indexes").await {
            Ok(value) => {
                // GET /indexes returns `[{ "id": "..." }, ...]`.
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
                tracing::warn!("resources/list: GET /indexes failed: {e:?}");
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
            "find_smells" => self.handle_find_smells(args).await,
            "analyze_quality" => self.handle_analyze_quality(args).await,
            "run_diagnostics" => self.handle_run_diagnostics(args).await,
            "list_facts" => self.handle_list_facts(args).await,
            "upsert_fact" => self.handle_upsert_fact(args).await,
            "delete_fact" => self.handle_delete_fact(args).await,
            "extract_graph" => self.handle_extract_graph(args).await,
            "list_entities" => self.handle_list_entities(args).await,
            "cluster_concepts" => self.handle_cluster_concepts(args).await,
            "analyzer_health" => self.handle_analyzer_health(args).await,
            "ingest_scip" => self.handle_ingest_scip(args).await,
            "extract_ner" => self.handle_extract_ner(args).await,
            "suggest_refactors" => self.handle_suggest_refactors(args).await,
            "review_diff" => self.handle_review_diff(args).await,
            "review_github_pr" => self.handle_review_github_pr(args).await,
            "deep_analysis" => self.handle_deep_analysis(args).await,
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
        let index_id = index_id_or_default(args);
        let top_n = args.get("top_n").and_then(Value::as_u64).unwrap_or(20);
        self.get(&format!(
            "/indexes/{index_id}/complexity_hotspots?top_n={top_n}"
        ))
        .await
    }

    async fn handle_find_smells(&self, args: &Value) -> Result<Value, DispatchError> {
        let index_id = index_id_or_default(args);
        let q = build_query(args, &["limit", "offset", "omit_content"]);
        self.get(&format!("/indexes/{index_id}/smells{q}")).await
    }

    async fn handle_analyze_quality(&self, args: &Value) -> Result<Value, DispatchError> {
        let index_id = index_id_or_default(args);
        self.get(&format!("/indexes/{index_id}/quality")).await
    }

    /// Handle the `run_diagnostics` tool: forward to
    /// `GET /indexes/{id}/diagnostics`, which runs the discovered external
    /// static-analysis tools (clippy, ruff, biome, ...) on demand.
    async fn handle_run_diagnostics(&self, args: &Value) -> Result<Value, DispatchError> {
        let index_id = index_id_or_default(args);
        let q = build_query(args, &["language", "tools", "limit", "offset"]);
        self.get(&format!("/indexes/{index_id}/diagnostics{q}"))
            .await
    }

    async fn handle_list_facts(&self, args: &Value) -> Result<Value, DispatchError> {
        let q = build_query(args, &["subject", "predicate", "object"]);
        self.get(&format!("/facts{q}")).await
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
        self.post("/facts", &body).await
    }

    async fn handle_delete_fact(&self, args: &Value) -> Result<Value, DispatchError> {
        let id = args
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| DispatchError::InvalidParams("missing 'id' (u64)".into()))?;
        self.delete(&format!("/facts/{id}")).await
    }

    async fn handle_extract_graph(&self, args: &Value) -> Result<Value, DispatchError> {
        let index_id = index_id_or_default(args);
        let mut path = format!("/indexes/{index_id}/graph");
        if let Some(lang) = args.get("language").and_then(Value::as_str) {
            path.push_str(&format!("?language={}", urlencode(lang)));
        }
        self.get(&path).await
    }

    async fn handle_list_entities(&self, args: &Value) -> Result<Value, DispatchError> {
        let index_id = index_id_or_default(args);
        let q = build_query(args, &["kind", "language"]);
        self.get(&format!("/indexes/{index_id}/entities{q}")).await
    }

    async fn handle_cluster_concepts(&self, args: &Value) -> Result<Value, DispatchError> {
        let index_id = index_id_or_default(args);
        let k = args.get("k").and_then(Value::as_u64).unwrap_or(8);
        let path = match args.get("method").and_then(Value::as_str) {
            Some(m) => format!("/indexes/{index_id}/clusters?k={k}&method={m}"),
            None => format!("/indexes/{index_id}/clusters?k={k}"),
        };
        self.get(&path).await
    }

    async fn handle_analyzer_health(&self, _args: &Value) -> Result<Value, DispatchError> {
        self.get("/health").await
    }

    async fn handle_suggest_refactors(&self, args: &Value) -> Result<Value, DispatchError> {
        let index_id = index_id_or_default(args);
        let top_k = args.get("top_k").and_then(Value::as_u64).unwrap_or(20);
        let mut path = format!("/indexes/{index_id}/refactor-suggestions?top_k={top_k}");
        if let Some(file) = args.get("file").and_then(Value::as_str) {
            path.push_str(&format!("&file={}", urlencode(file)));
        }
        if let Some(sev) = args.get("min_severity").and_then(Value::as_str) {
            path.push_str(&format!("&min_severity={}", urlencode(sev)));
        }
        self.get(&path).await
    }

    async fn handle_extract_ner(&self, args: &Value) -> Result<Value, DispatchError> {
        let index_id = index_id_or_default(args);
        let top_k = args.get("top_k").and_then(Value::as_u64).unwrap_or(50);
        self.get(&format!("/indexes/{index_id}/ner?top_k={top_k}"))
            .await
    }

    async fn handle_ingest_scip(&self, args: &Value) -> Result<Value, DispatchError> {
        use base64::Engine;
        let index_id = index_id_or_default(args);
        let b64 = require_str(args, "scip_base64")?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| {
                DispatchError::InvalidParams(format!("scip_base64 is not valid base64: {e}"))
            })?;
        self.post_bytes(&format!("/indexes/{index_id}/scip"), bytes)
            .await
    }

    /// Handle the `review_diff` tool: forward a unified diff to `POST /review`.
    ///
    /// Why: parity with the `POST /review` endpoint so MCP clients (Claude
    /// Code) can ask for a PR review without shelling out. Like every other
    /// analyzer tool, review is backed by trusty-search: the daemon fetches the
    /// named index's chunk corpus to cross-reference the diff.
    /// What: requires a `diff` string param and an `index_id` string param,
    /// and POSTs the diff as `text/x-patch` to `/review?index_id=...`.
    /// Test: `review_diff_requires_diff_param` and
    /// `review_diff_requires_index_id` check the missing-param paths.
    async fn handle_review_diff(&self, args: &Value) -> Result<Value, DispatchError> {
        let diff = require_str(args, "diff")?;
        let index_id = require_str(args, "index_id")?;
        let path = format!("/review?index_id={}", urlencode(index_id));
        self.post_text(&path, diff).await
    }

    /// Handle the `deep_analysis` MCP tool: forward to `POST /analyze/deep`.
    ///
    /// Why: pairs with the [`POST /analyze/deep`] HTTP endpoint so MCP clients
    /// can opt into the LLM-augmented analysis without going through the
    /// deterministic `review_diff` path. Keeps the two surfaces separate so
    /// `review_diff` remains cheap and deterministic.
    /// What: requires `index_id`; optional `model` overrides the daemon
    /// default. POSTs a JSON body shaped like [`DeepAnalyzeRequest`] and
    /// returns the [`DeepAnalysisReport`] JSON.
    /// Test: `deep_analysis_requires_index_id` and
    /// `deep_analysis_posts_to_endpoint` cover param + URL construction.
    async fn handle_deep_analysis(&self, args: &Value) -> Result<Value, DispatchError> {
        let index_id = require_str(args, "index_id")?;
        let mut body = serde_json::json!({ "index_id": index_id });
        if let Some(model) = args.get("model").and_then(Value::as_str) {
            body["model"] = Value::from(model);
        }
        // The HTTP endpoint accepts an optional pre-computed `report`; the MCP
        // tool surface deliberately keeps the schema minimal (index_id +
        // model) — re-running the synthesis on the daemon is the simpler
        // ergonomics for AI clients.
        self.post("/analyze/deep", &body).await
    }

    /// Handle the `review_github_pr` tool: forward to `POST /review/github-pr`.
    ///
    /// Why: parity with the HTTP endpoint so MCP clients can review a GitHub PR
    /// by number. The daemon owns the GitHub token and the fetch/analyze/comment
    /// pipeline; the MCP server is a pure translator.
    /// What: requires `owner`, `repo`, `pr`, and `index_id`; `post_comment` is
    /// optional (default false). POSTs a `GithubPrRequest`-shaped JSON body.
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
        self.post("/review/github-pr", &body).await
    }

    async fn post_text(&self, path: &str, body: &str) -> Result<Value, DispatchError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .post(&url)
            .header("content-type", "text/x-patch")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| DispatchError::Transport(format!("POST {url}: {e}")))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| DispatchError::Transport(format!("decode {url}: {e}")))?;
        if !status.is_success() {
            return Err(DispatchError::Transport(format!(
                "POST {url} returned {status}: {body}"
            )));
        }
        Ok(body)
    }

    async fn post_bytes(&self, path: &str, body: Vec<u8>) -> Result<Value, DispatchError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .post(&url)
            .header("content-type", "application/octet-stream")
            .body(body)
            .send()
            .await
            .map_err(|e| DispatchError::Transport(format!("POST {url}: {e}")))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| DispatchError::Transport(format!("decode {url}: {e}")))?;
        if !status.is_success() {
            return Err(DispatchError::Transport(format!(
                "POST {url} returned {status}: {body}"
            )));
        }
        Ok(body)
    }

    async fn get(&self, path: &str) -> Result<Value, DispatchError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| DispatchError::Transport(format!("GET {url}: {e}")))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| DispatchError::Transport(format!("decode {url}: {e}")))?;
        if !status.is_success() {
            return Err(DispatchError::Transport(format!(
                "GET {url} returned {status}: {body}"
            )));
        }
        Ok(body)
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, DispatchError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| DispatchError::Transport(format!("POST {url}: {e}")))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| DispatchError::Transport(format!("decode {url}: {e}")))?;
        if !status.is_success() {
            return Err(DispatchError::Transport(format!(
                "POST {url} returned {status}: {body}"
            )));
        }
        Ok(body)
    }

    async fn delete(&self, path: &str) -> Result<Value, DispatchError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .delete(&url)
            .send()
            .await
            .map_err(|e| DispatchError::Transport(format!("DELETE {url}: {e}")))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| DispatchError::Transport(format!("decode {url}: {e}")))?;
        if !status.is_success() {
            return Err(DispatchError::Transport(format!(
                "DELETE {url} returned {status}: {body}"
            )));
        }
        Ok(body)
    }
}

#[derive(Debug)]
enum DispatchError {
    UnknownTool,
    InvalidParams(String),
    Transport(String),
}

fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, DispatchError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| DispatchError::InvalidParams(format!("missing or non-string '{key}'")))
}

/// Read `index` (preferred) or `index_id` (legacy alias) from `args`,
/// falling back to `"default"`.
///
/// Why: Multiple tools accept either parameter name and need the same
/// fallback behaviour; centralising removes 9 copies of the same chain.
/// What: Tries `index`, then `index_id`, then `"default"`.
/// Test: Covered indirectly by every per-tool handler test.
fn index_id_or_default(args: &Value) -> &str {
    args.get("index")
        .or_else(|| args.get("index_id"))
        .and_then(Value::as_str)
        .unwrap_or("default")
}

/// Build a `?key=val&...` query string from whichever of `keys` is present
/// in `args`. Handles string, integer (u64), and bool values; skips keys
/// that are absent or of an unsupported type. Returns an empty string if no
/// keys were found.
///
/// Why: `find_smells` and `run_diagnostics` gained `limit` (number), `offset`
/// (number), and `omit_content` (bool) params (#917/#918); extending this
/// helper avoids duplicating query-string assembly in each handler.
/// What: tries `as_str` first, then `as_u64`, then `as_bool`; uses the first
/// match. The former `as_f64` fallback was removed because JSON integers are
/// already covered by `as_u64`, and float→u64 truncation (e.g. `3.9 → 3`) is
/// silently wrong. Non-string values are formatted without URL encoding because
/// they never contain reserved characters.
/// Test: `build_query_handles_numeric_and_bool` and
/// `build_query_integer_limit_parses_correctly` in `helpers_tests.rs`.
fn build_query(args: &Value, keys: &[&str]) -> String {
    let mut q = String::new();
    for key in keys {
        let node = args.get(*key);
        let val: Option<String> = node
            .and_then(Value::as_str)
            .map(urlencode)
            .or_else(|| node.and_then(Value::as_u64).map(|n| n.to_string()))
            .or_else(|| node.and_then(Value::as_bool).map(|b| b.to_string()));
        if let Some(v) = val {
            let sep = if q.is_empty() { '?' } else { '&' };
            q.push(sep);
            q.push_str(key);
            q.push('=');
            q.push_str(&v);
        }
    }
    q
}

/// Minimal URL encoding for the bits we pass through to `/facts?subject=...`.
/// Avoids pulling a full url crate into the MCP server.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn wrap_text_content(value: &Value) -> Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
        }]
    })
}

fn wrap_tool_result(value: &Value) -> Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
        }],
        "isError": false,
    })
}

fn wrap_tool_error(msg: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": format!("Error: {msg}") }],
        "isError": true,
    })
}

/// Assemble the full `tools/list` descriptor array.
///
/// Why: the base descriptor set lives in `descriptors.rs` to keep this file
/// within its line-cap budget (#610). When the optional `review` feature is on
/// (#630), the three `tr_review_*` descriptors are appended so `tools/list`
/// advertises exactly the tools the dispatcher can route.
/// What: starts from `descriptors::base_tool_descriptors()` and, under
/// `feature = "review"`, pushes `review::review_tool_descriptors()` onto the
/// array. Returns a `serde_json::Value` array.
/// Test: `tools_list_contains_full_surface` (base names) and, feature-gated,
/// `tools_list_includes_tr_review_tools` (the three `tr_` names).
pub fn tool_descriptors() -> Value {
    // `mut` is only needed when the `review` feature appends descriptors; the
    // attribute keeps the feature-off build free of an `unused_mut` warning.
    #[cfg_attr(not(feature = "review"), allow(unused_mut))]
    let mut tools = descriptors::base_tool_descriptors();
    #[cfg(feature = "review")]
    if let Some(arr) = tools.as_array_mut() {
        arr.extend(review::review_tool_descriptors());
    }
    tools
}

#[cfg(test)]
mod helpers_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, params: Value) -> Request {
        Request {
            jsonrpc: "2.0".into(),
            id: Some(Value::from(1u64)),
            method: method.into(),
            params,
        }
    }

    #[tokio::test]
    async fn tools_list_contains_full_surface() {
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let resp = server.dispatch(req("tools/list", Value::Null)).await;
        let result = resp.result.expect("expected result");
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .expect("array");
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        for required in [
            "complexity_hotspots",
            "find_smells",
            "analyze_quality",
            "run_diagnostics",
            "list_facts",
            "upsert_fact",
            "delete_fact",
            "analyzer_health",
            "ingest_scip",
        ] {
            assert!(
                names.contains(&required),
                "missing tool '{required}' (got {names:?})"
            );
        }
    }

    #[tokio::test]
    async fn tools_list_includes_review_diff() {
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let resp = server.dispatch(req("tools/list", Value::Null)).await;
        let result = resp.result.expect("expected result");
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .expect("array");
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert!(names.contains(&"review_diff"), "got {names:?}");
    }

    #[tokio::test]
    async fn review_diff_requires_diff_param() {
        // Missing 'diff' → InvalidParams before any HTTP call is attempted.
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let err = server
            .handle_review_diff(&serde_json::json!({ "index_id": "x" }))
            .await
            .expect_err("missing diff param should fail");
        assert!(matches!(err, DispatchError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn review_diff_requires_index_id() {
        // Missing 'index_id' → InvalidParams: review is backed by trusty-search
        // and needs an index to cross-reference against.
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let err = server
            .handle_review_diff(&serde_json::json!({ "diff": "+++ b/x.rs\n" }))
            .await
            .expect_err("missing index_id param should fail");
        assert!(matches!(err, DispatchError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn review_diff_with_args_attempts_post_to_review() {
        // Daemon unreachable — a Transport error mentioning /review proves the
        // handler built the right URL (with index_id) and method.
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let err = server
            .handle_review_diff(&serde_json::json!({
                "diff": "+++ b/x.rs\n",
                "index_id": "my-idx",
            }))
            .await
            .expect_err("daemon unreachable");
        match err {
            DispatchError::Transport(msg) => {
                assert!(msg.contains("/review"), "got {msg}");
                assert!(msg.contains("index_id=my-idx"), "got {msg}");
            }
            other => panic!("expected Transport, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tools_list_includes_review_github_pr() {
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let resp = server.dispatch(req("tools/list", Value::Null)).await;
        let result = resp.result.expect("expected result");
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .expect("array");
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert!(names.contains(&"review_github_pr"), "got {names:?}");
    }

    #[tokio::test]
    async fn review_github_pr_requires_owner() {
        // Missing 'owner' → InvalidParams before any HTTP call.
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let err = server
            .handle_review_github_pr(&serde_json::json!({
                "repo": "r", "pr": 1, "index_id": "i"
            }))
            .await
            .expect_err("missing owner should fail");
        assert!(matches!(err, DispatchError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn review_github_pr_requires_pr_number() {
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let err = server
            .handle_review_github_pr(&serde_json::json!({
                "owner": "o", "repo": "r", "index_id": "i"
            }))
            .await
            .expect_err("missing pr should fail");
        assert!(matches!(err, DispatchError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn review_github_pr_posts_to_endpoint() {
        // Daemon unreachable — a Transport error referencing /review/github-pr
        // proves the handler built the right URL after parsing all params.
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let err = server
            .handle_review_github_pr(&serde_json::json!({
                "owner": "o", "repo": "r", "pr": 7, "index_id": "i"
            }))
            .await
            .expect_err("daemon unreachable");
        match err {
            DispatchError::Transport(msg) => {
                assert!(msg.contains("/review/github-pr"), "got {msg}");
            }
            other => panic!("expected Transport, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tools_list_includes_deep_analysis() {
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let resp = server.dispatch(req("tools/list", Value::Null)).await;
        let result = resp.result.expect("expected result");
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .expect("array");
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert!(names.contains(&"deep_analysis"), "got {names:?}");
    }

    #[tokio::test]
    async fn deep_analysis_requires_index_id() {
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let err = server
            .handle_deep_analysis(&serde_json::json!({}))
            .await
            .expect_err("missing index_id should fail");
        assert!(matches!(err, DispatchError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn deep_analysis_posts_to_endpoint() {
        // Daemon unreachable — a Transport error referencing /analyze/deep
        // proves the handler built the right URL after parsing index_id.
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let err = server
            .handle_deep_analysis(&serde_json::json!({ "index_id": "i", "model": "m" }))
            .await
            .expect_err("daemon unreachable");
        match err {
            DispatchError::Transport(msg) => {
                assert!(msg.contains("/analyze/deep"), "got {msg}");
            }
            other => panic!("expected Transport, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resources_list_returns_envelope() {
        // Daemon unreachable → GET /indexes fails → empty resource list, but
        // the response is still a well-formed `{ resources: [] }` result.
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let resp = server.dispatch(req("resources/list", Value::Null)).await;
        let result = resp.result.expect("expected result");
        let resources = result
            .get("resources")
            .and_then(Value::as_array)
            .expect("resources array");
        assert!(resources.is_empty(), "expected empty list when daemon down");
    }

    #[tokio::test]
    async fn initialize_advertises_resources_capability() {
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let resp = server.dispatch(req("initialize", Value::Null)).await;
        let result = resp.result.expect("expected result");
        assert!(result["capabilities"]["resources"].is_object());
    }

    #[tokio::test]
    async fn unknown_tool_returns_method_not_found() {
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let resp = server
            .dispatch(req(
                "tools/call",
                serde_json::json!({ "name": "no_such_tool", "arguments": {} }),
            ))
            .await;
        let err = resp.error.expect("expected error");
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn handle_analyzer_health_calls_health_endpoint() {
        // Direct handler invocation, bypassing dispatch. Daemon is unreachable,
        // so we expect a Transport error referencing /health, which proves the
        // handler constructed the right URL without us going through tools/call.
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let err = server
            .handle_analyzer_health(&Value::Null)
            .await
            .expect_err("daemon unreachable, expected transport error");
        match err {
            DispatchError::Transport(msg) => {
                assert!(
                    msg.contains("/health"),
                    "expected transport error to mention /health, got: {msg}"
                );
            }
            other => panic!("expected DispatchError::Transport, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_wrong_jsonrpc_version() {
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let r = Request {
            jsonrpc: "1.0".into(),
            id: Some(Value::from(7u64)),
            method: "tools/list".into(),
            params: Value::Null,
        };
        let resp = server.dispatch(r).await;
        let err = resp.error.expect("expected error");
        assert_eq!(err.code, error_codes::INVALID_REQUEST);
    }

    // ── #630: trusty-review LLM tools (feature `review`) ─────────────────────

    /// With the `review` feature ON, `tools/list` advertises all three
    /// `tr_review_*` tools alongside the base analyzer tools.
    #[cfg(feature = "review")]
    #[tokio::test]
    async fn tools_list_includes_tr_review_tools() {
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let resp = server.dispatch(req("tools/list", Value::Null)).await;
        let result = resp.result.expect("expected result");
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .expect("array");
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        for required in ["tr_review_pr", "tr_review_diff", "tr_review_health"] {
            assert!(names.contains(&required), "missing {required} in {names:?}");
        }
        // The base surface must still be present (no descriptors lost in the
        // extract-to-descriptors.rs refactor).
        assert!(names.contains(&"complexity_hotspots"), "got {names:?}");
        assert!(names.contains(&"deep_analysis"), "got {names:?}");
    }

    /// With the `review` feature ON, the `tr_review_*` names route into the
    /// embedded pipeline rather than falling through to `UnknownTool`. We do not
    /// assert success (the credential-bound build path needs live AWS /
    /// OpenRouter config); we only assert the dispatcher did NOT return
    /// METHOD_NOT_FOUND, i.e. the name was recognised and routed.
    #[cfg(feature = "review")]
    #[tokio::test]
    async fn tr_review_health_routes_not_unknown_tool() {
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
        let resp = server
            .dispatch(req(
                "tools/call",
                serde_json::json!({ "name": "tr_review_health", "arguments": {} }),
            ))
            .await;
        // tools/call form: routed tools return a result (possibly an in-band
        // isError envelope on build failure); an *unrecognised* name returns a
        // JSON-RPC METHOD_NOT_FOUND error. Assert we are not in the latter case.
        if let Some(err) = resp.error {
            assert_ne!(
                err.code,
                error_codes::METHOD_NOT_FOUND,
                "tr_review_health must route, not be unknown: {err:?}"
            );
        }
    }

    /// With the `review` feature OFF, the `tr_review_*` names are neither
    /// advertised in `tools/list` nor routable — they return METHOD_NOT_FOUND.
    #[cfg(not(feature = "review"))]
    #[tokio::test]
    async fn tr_review_tools_absent_when_feature_off() {
        let server = AnalyzerMcpServer::new("http://127.0.0.1:1");

        // Not advertised.
        let resp = server.dispatch(req("tools/list", Value::Null)).await;
        let tools = resp
            .result
            .expect("result")
            .get("tools")
            .and_then(Value::as_array)
            .expect("array")
            .clone();
        let names: Vec<String> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        for absent in ["tr_review_pr", "tr_review_diff", "tr_review_health"] {
            assert!(
                !names.iter().any(|n| n == absent),
                "{absent} must be absent when feature off; got {names:?}"
            );
        }

        // Not routable.
        let resp = server
            .dispatch(req(
                "tools/call",
                serde_json::json!({ "name": "tr_review_health", "arguments": {} }),
            ))
            .await;
        let err = resp.error.expect("expected METHOD_NOT_FOUND error");
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
    }
}
