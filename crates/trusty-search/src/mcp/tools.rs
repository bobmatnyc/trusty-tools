//! MCP tool dispatcher: JSON-RPC 2.0 over a daemon HTTP back-end.
//!
//! Why: Claude Code speaks MCP/JSON-RPC; the trusty-search daemon speaks
//! REST. This module is a pure translator. It owns no state beyond a
//! `reqwest::Client` and a base URL, so the same dispatcher can be driven
//! from `stdio` (one process per session) or `sse` (long-lived axum task).
//!
//! What: [`McpServer::dispatch`] takes a [`Request`] and returns a
//! [`Response`]. Tool calls map 1:1 to daemon endpoints:
//!
//! | MCP tool        | Daemon endpoint                           |
//! |-----------------|-------------------------------------------|
//! | `search`        | `POST /indexes/:id/search`                |
//! | `index_file`    | `POST /indexes/:id/index-file`            |
//! | `remove_file`   | `POST /indexes/:id/remove-file`           |
//! | `list_indexes`  | `GET  /indexes`                           |
//! | `create_index`  | `POST /indexes`                           |
//! | `search_health` | `GET  /health`                            |
//! | `delete_index`  | `DELETE /indexes/:id`                     |
//! | `reindex`       | `POST /indexes/:id/reindex`               |
//! | `index_status`  | `GET  /indexes/:id/status`                |
//! | `list_chunks`   | `GET  /indexes/:id/chunks?offset=&limit=` |
//! | `chat`          | `POST /chat`                              |
//!
//! Test: `cargo test -p trusty-search-mcp` covers JSON-RPC parsing, error
//! shapes (-32600 invalid request, -32601 method not found, -32602 invalid
//! params), and dispatch routing without hitting a real daemon.

use serde_json::Value;

// JSON-RPC 2.0 primitives moved to the shared `trusty-mcp-core` crate so
// trusty-memory and trusty-search agree on the wire shape. Re-exported here
// to keep `pub use` consumers (and `crate::mcp::tools::error_codes` etc.) working.
pub use trusty_common::mcp::{error_codes, initialize_response, JsonRpcError, Request, Response};

/// Application-level JSON-RPC error code surfaced when a per-lane MCP tool
/// (`search_semantic`, `search_kg`) is invoked but its prerequisite stage
/// is not yet `Ready` on the target index (issue #138).
///
/// Why: Falls inside the JSON-RPC 2.0 "server reserved" range (`-32099` ..
/// `-32000`) so it never collides with transport-level codes (parse error,
/// invalid request, method not found, invalid params, internal error). The
/// LLM and any orchestrator can branch on this code to retry against
/// `search_lexical` instead of asking the user.
/// What: a free integer constant; emitted on bare-method invocations. The
/// `tools/call` form surfaces the same condition via `_meta.error_code =
/// "STAGE_NOT_READY"` per MCP's in-band error convention.
/// Test: covered by `search_semantic_tool_returns_stage_not_ready_when_*`
/// and `search_kg_tool_returns_stage_not_ready_when_*` in the unit tests
/// at the bottom of this file.
pub const STAGE_NOT_READY_CODE: i32 = -32010;

/// Tool dispatcher backed by an HTTP client targeting the daemon.
#[derive(Clone)]
pub struct McpServer {
    base_url: String,
    http: reqwest::Client,
}

impl McpServer {
    /// Construct a dispatcher pointing at the daemon's base URL
    /// (e.g. `http://127.0.0.1:7878`).
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::new(),
        }
    }

    /// Inject a pre-built reqwest client (useful for tests / pooling).
    pub fn with_client(base_url: impl Into<String>, http: reqwest::Client) -> Self {
        Self {
            base_url: base_url.into(),
            http,
        }
    }

    /// Daemon base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Translate a JSON-RPC request into a daemon HTTP call and wrap the
    /// response. Always returns a `Response`; transport / daemon failures are
    /// reported as `INTERNAL_ERROR` rather than panicking.
    pub async fn dispatch(&self, req: Request) -> Response {
        let is_notification = req.id.is_none();
        let id = req.id.clone();

        if req.jsonrpc.as_deref() != Some("2.0") {
            if is_notification {
                return Response::suppressed();
            }
            return Response::err(id, error_codes::INVALID_REQUEST, "jsonrpc must be \"2.0\"");
        }

        // MCP lifecycle methods. `initialize` exchanges capabilities;
        // `notifications/initialized` confirms the client finished setup
        // and is silenced (per JSON-RPC 2.0 notification semantics — no
        // `id`, no reply).
        match req.method.as_str() {
            "initialize" => {
                return Response::ok(
                    id,
                    initialize_response("trusty-search", env!("CARGO_PKG_VERSION"), None),
                );
            }
            "notifications/initialized" | "initialized" => {
                return Response::suppressed();
            }
            _ => {}
        }

        // MCP "tools/call" wraps tool name + arguments. We also accept the
        // bare method name for ergonomics (`search` directly).
        let params = req.params.clone().unwrap_or(Value::Null);
        let (tool, arguments, via_tools_call) = match req.method.as_str() {
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let args = params
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
            // OpenRPC 1.3.2 discovery — see `mcp::openrpc`. Returns the
            // full service description so orchestrators (open-mpm, etc.)
            // can introspect every tool and its required
            // `search.read`/`search.write` scope without bespoke adapters.
            "rpc.discover" => {
                return Response::ok(
                    id,
                    crate::mcp::openrpc::build_discover_response(env!("CARGO_PKG_VERSION")),
                );
            }
            other => (other.to_string(), params, false),
        };

        let outcome = self.call_tool(&tool, &arguments).await;

        if via_tools_call {
            // Per MCP spec, tool execution failures are reported in-band as
            // `{content: [...], isError: true}` rather than JSON-RPC errors —
            // the protocol-level error space is reserved for malformed
            // requests / unknown tools.
            match outcome {
                Ok(value) => Response::ok(id, wrap_tool_result(&value, false)),
                Err(DispatchError::UnknownTool) => Response::err(
                    id,
                    error_codes::METHOD_NOT_FOUND,
                    format!("unknown tool: {tool}"),
                ),
                Err(DispatchError::InvalidParams(msg)) => Response::ok(id, wrap_tool_error(&msg)),
                Err(DispatchError::Transport(msg)) => Response::ok(id, wrap_tool_error(&msg)),
                Err(DispatchError::StageNotReady {
                    message,
                    current_stages,
                    suggested_tools,
                }) => Response::ok(
                    id,
                    wrap_stage_not_ready_error(&message, &current_stages, &suggested_tools),
                ),
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
                Err(DispatchError::StageNotReady {
                    message,
                    current_stages,
                    suggested_tools,
                }) => {
                    // Bare-method form has no `_meta` slot, so we surface
                    // the structured payload as the error `data` field and
                    // keep the message human-readable. JSON-RPC `code` -32010
                    // is in the server-defined range reserved for app-level
                    // semantics (issue #138).
                    let data = serde_json::json!({
                        "error_code": "STAGE_NOT_READY",
                        "current_stages": current_stages,
                        "suggested_tools": suggested_tools,
                    });
                    let mut resp = Response::err(id, STAGE_NOT_READY_CODE, message);
                    if let Some(ref mut e) = resp.error {
                        e.data = Some(data);
                    }
                    resp
                }
            }
        }
    }

    async fn call_tool(&self, tool: &str, args: &Value) -> Result<Value, DispatchError> {
        match tool {
            // Issue #138 — per-lane search tools that push intent
            // classification to the LLM. Each tool pins the daemon to a
            // fixed lane combination and returns a structured
            // STAGE_NOT_READY error when its prerequisite stage isn't
            // ready, instead of silently degrading.
            "search_lexical" => self.run_lane_search(args, SearchLane::Lexical).await,
            "search_semantic" => self.run_lane_search(args, SearchLane::Semantic).await,
            "search_kg" => self.run_lane_search(args, SearchLane::Graph).await,
            "search_all" => {
                // Polymorphic for back-compat (issue #138):
                //   * with `index_id`  → per-index full hybrid (alias for
                //     `search`; matches the #138 ticket spec).
                //   * without `index_id` → cross-project fan-out (legacy
                //     issue #10 behaviour preserved).
                if args.get("index_id").and_then(Value::as_str).is_some() {
                    return self.run_lane_search(args, SearchLane::All).await;
                }
                let query = require_str(args, "query")?;
                let top_k = args.get("top_k").and_then(Value::as_u64).unwrap_or(10);
                let full_content = args
                    .get("full_content")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let body = serde_json::json!({
                    "query": query,
                    "top_k": top_k,
                    "full_content": full_content,
                });
                self.post("/search", &body).await
            }
            "search" => {
                let index_id = require_str(args, "index_id")?;
                // Accept the spec form `{query: string, top_k?: int}` and
                // also a pre-built `{query: object}` body for callers that
                // need to pass advanced search parameters directly.
                let body = match args.get("query") {
                    Some(v @ Value::Object(_)) => v.clone(),
                    Some(Value::String(text)) => {
                        let mut b = serde_json::json!({ "text": text });
                        if let Some(k) = args.get("top_k").and_then(Value::as_u64) {
                            b["top_k"] = Value::from(k);
                        }
                        // Issue #122 — branch-aware search: forward optional
                        // branch context fields through to the daemon body so
                        // MCP callers can boost on-branch matches without
                        // building the `{query: object}` shape themselves.
                        if let Some(bf) = args.get("branch_files") {
                            b["branch_files"] = bf.clone();
                        }
                        if let Some(bb) = args.get("branch_boost") {
                            b["branch_boost"] = bb.clone();
                        }
                        if let Some(br) = args.get("branch").and_then(Value::as_str) {
                            b["branch"] = Value::String(br.to_string());
                        }
                        // Issue #77 (final design): forward optional `mode`
                        // ("code" | "text" | "data"; default "code") so the
                        // daemon applies the matching penalty matrix.
                        if let Some(m) = args.get("mode").and_then(Value::as_str) {
                            b["mode"] = Value::String(m.to_string());
                        }
                        // Issue #74: forward optional `exclude_archived`
                        // (default false) so callers can hard-filter archived
                        // / deprecated / legacy chunks instead of only
                        // downranking them.
                        if let Some(ea) = args.get("exclude_archived").and_then(Value::as_bool) {
                            b["exclude_archived"] = Value::Bool(ea);
                        }
                        b
                    }
                    _ => {
                        return Err(DispatchError::InvalidParams(
                            "missing or invalid 'query' (expected string or object)".into(),
                        ))
                    }
                };
                let resp = self
                    .post(&format!("/indexes/{index_id}/search"), &body)
                    .await?;
                // Mirror the daemon's per-query INFO log (issue #125) so the
                // MCP transport surfaces the same query/intent/latency line.
                let query_text = body.get("text").and_then(Value::as_str).unwrap_or_default();
                let log_intent = resp
                    .get("intent")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown");
                let log_latency = resp.get("latency_ms").and_then(Value::as_u64).unwrap_or(0);
                let log_results = resp
                    .get("results")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0);
                tracing::info!(
                    index_id = %index_id,
                    intent = %log_intent,
                    latency_ms = log_latency,
                    results = log_results,
                    query = %&query_text[..query_text.len().min(80)],
                    "search"
                );
                Ok(resp)
            }
            "index_file" => {
                let index_id = require_str(args, "index_id")?;
                let path = require_str(args, "path")?;
                let content = require_str(args, "content")?;
                self.post(
                    &format!("/indexes/{index_id}/index-file"),
                    &serde_json::json!({ "path": path, "content": content }),
                )
                .await
            }
            "remove_file" => {
                let index_id = require_str(args, "index_id")?;
                let path = require_str(args, "path")?;
                self.post(
                    &format!("/indexes/{index_id}/remove-file"),
                    &serde_json::json!({ "path": path }),
                )
                .await
            }
            // Issue #312: request details=true so the response includes
            // per-index size_bytes in addition to the id list.
            "list_indexes" => self.get("/indexes?details=true").await,
            "create_index" => {
                let id = require_str(args, "id")?;
                let root_path = require_str(args, "root_path")?;
                self.post(
                    "/indexes",
                    &serde_json::json!({ "id": id, "root_path": root_path }),
                )
                .await
            }
            "search_similar" => {
                // Code-to-code similarity (issue #31). Index defaults to "default"
                // so simple call sites don't need to specify it.
                let index_id = args
                    .get("index")
                    .and_then(Value::as_str)
                    .unwrap_or("default");
                let file = require_str(args, "file")?;
                let mut body = serde_json::json!({ "file": file });
                if let Some(func) = args.get("function").and_then(Value::as_str) {
                    body["function"] = Value::String(func.to_string());
                }
                if let Some(k) = args.get("top_k").and_then(Value::as_u64) {
                    body["top_k"] = Value::from(k);
                }
                self.post(&format!("/indexes/{index_id}/search_similar"), &body)
                    .await
            }
            "search_health" => self.get("/health").await,
            "delete_index" => {
                let index_id = require_str(args, "index_id")?;
                self.delete(&format!("/indexes/{index_id}")).await
            }
            "reindex" => {
                let index_id = require_str(args, "index_id")?;
                // Accept optional root_path override (mirrors the HTTP body).
                let mut body = serde_json::json!({});
                if let Some(rp) = args.get("root_path").and_then(Value::as_str) {
                    body["root_path"] = Value::String(rp.to_string());
                }
                self.post(&format!("/indexes/{index_id}/reindex"), &body)
                    .await
            }
            "index_status" => {
                let index_id = require_str(args, "index_id")?;
                self.get(&format!("/indexes/{index_id}/status")).await
            }
            "chat" => {
                let index_id = require_str(args, "index_id")?;
                // Accept either `message` (legacy / UI) or `question` (issue #15 spec).
                let message = args
                    .get("message")
                    .and_then(Value::as_str)
                    .or_else(|| args.get("question").and_then(Value::as_str))
                    .ok_or_else(|| {
                        DispatchError::InvalidParams(
                            "missing required string field: message (or question)".into(),
                        )
                    })?;
                let mut body = serde_json::json!({
                    "index_id": index_id,
                    "message": message,
                });
                if let Some(history) = args.get("history") {
                    body["history"] = history.clone();
                }
                if let Some(model) = args.get("model").and_then(Value::as_str) {
                    body["model"] = Value::String(model.to_string());
                }
                if let Some(top_k) = args.get("top_k").and_then(Value::as_u64) {
                    body["top_k"] = Value::from(top_k);
                }
                if let Some(key) = args.get("api_key").and_then(Value::as_str) {
                    body["api_key"] = Value::String(key.to_string());
                }
                self.post("/chat", &body).await
            }
            "get_call_chain" => {
                // Issue #76 — annotated call tree for an entry-point function.
                // The daemon endpoint returns `text/plain`; we wrap the body in
                // the JSON envelope MCP clients consume.
                let index_id = require_str(args, "index_id")?;
                let entry_point = require_str(args, "entry_point")?;
                let mut query: Vec<(&str, String)> = vec![("entry_point", entry_point.to_string())];
                if let Some(dir) = args.get("direction").and_then(Value::as_str) {
                    query.push(("direction", dir.to_string()));
                }
                if let Some(d) = args.get("max_depth").and_then(Value::as_u64) {
                    query.push(("max_depth", d.to_string()));
                }
                if let Some(inc) = args.get("include_source").and_then(Value::as_bool) {
                    query.push(("include_source", inc.to_string()));
                }
                self.get_text(&format!("/indexes/{index_id}/call_chain"), &query)
                    .await
                    .map(|text| serde_json::json!({ "text": text }))
            }
            "list_chunks" => {
                // Issue #54 — paginated enumeration of an index's corpus.
                // Mirrors `GET /indexes/:id/chunks?offset=&limit=`.
                let index_id = require_str(args, "index_id")?;
                let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100);
                self.get(&format!(
                    "/indexes/{index_id}/chunks?offset={offset}&limit={limit}"
                ))
                .await
            }
            "grep" => {
                // grep-parity regex/literal search over an index's files.
                // Mirrors `POST /grep` (global) and `POST /indexes/:id/grep`.
                // `index_id` is optional — when omitted, the daemon fans out
                // across every registered index.
                let pattern = require_str(args, "pattern")?;
                let mut body = serde_json::json!({ "pattern": pattern });
                if let Some(v) = args.get("case_insensitive").and_then(Value::as_bool) {
                    body["case_insensitive"] = Value::Bool(v);
                }
                if let Some(v) = args.get("context").and_then(Value::as_u64) {
                    body["context"] = Value::from(v);
                }
                if let Some(v) = args.get("context_before").and_then(Value::as_u64) {
                    body["context_before"] = Value::from(v);
                }
                if let Some(v) = args.get("context_after").and_then(Value::as_u64) {
                    body["context_after"] = Value::from(v);
                }
                if let Some(v) = args.get("glob").and_then(Value::as_str) {
                    body["glob"] = Value::String(v.to_string());
                }
                if let Some(v) = args.get("multiline").and_then(Value::as_bool) {
                    body["multiline"] = Value::Bool(v);
                }
                if let Some(v) = args.get("fixed_strings").and_then(Value::as_bool) {
                    body["fixed_strings"] = Value::Bool(v);
                }
                if let Some(v) = args.get("files_with_matches").and_then(Value::as_bool) {
                    body["files_with_matches"] = Value::Bool(v);
                }
                if let Some(v) = args.get("invert_match").and_then(Value::as_bool) {
                    body["invert_match"] = Value::Bool(v);
                }
                if let Some(v) = args.get("word_regexp").and_then(Value::as_bool) {
                    body["word_regexp"] = Value::Bool(v);
                }
                // Issue #447: accept `max_count` as a ripgrep-parity alias for
                // `max_results`. `max_results` wins when both are supplied.
                if let Some(v) = args
                    .get("max_results")
                    .or_else(|| args.get("max_count"))
                    .and_then(Value::as_u64)
                {
                    body["max_results"] = Value::from(v);
                }
                match args.get("index_id").and_then(Value::as_str) {
                    Some(id) => self.post(&format!("/indexes/{id}/grep"), &body).await,
                    None => self.post("/grep", &body).await,
                }
            }
            _ => Err(DispatchError::UnknownTool),
        }
    }

    /// GET an endpoint that returns `text/plain` and return the body as a
    /// `String`. Used by `get_call_chain` (issue #76), whose response is
    /// prose intended for direct LLM consumption.
    async fn get_text(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<String, DispatchError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .get(&url)
            .query(query)
            .send()
            .await
            .map_err(|e| DispatchError::Transport(format!("GET {url}: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| DispatchError::Transport(format!("decode {url}: {e}")))?;
        if !status.is_success() {
            // 400 from the daemon means invalid params; surface that to the
            // caller as an INVALID_PARAMS error rather than INTERNAL_ERROR.
            if status == reqwest::StatusCode::BAD_REQUEST {
                return Err(DispatchError::InvalidParams(body));
            }
            return Err(DispatchError::Transport(format!(
                "GET {url} returned {status}: {body}"
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

    /// Common dispatcher for the four per-lane search tools introduced in
    /// issue #138 (`search_lexical`, `search_semantic`, `search_kg`,
    /// `search_all`).
    ///
    /// Why: every per-lane tool follows the same recipe — parse `index_id`
    /// plus `query`, optionally pre-check `search_capabilities` against
    /// the lane's prerequisite stage, build a `SearchQuery` body with the
    /// correct `stage` / `expand_graph` pinning, and POST to the
    /// per-index search endpoint. Factoring this out keeps `call_tool`
    /// readable and guarantees all four tools share identical schema,
    /// error shape, and logging.
    ///
    /// What: validates args; for lanes that require a stage beyond Stage
    /// 1 (`Semantic` needs `vector`, `Graph` needs `kg`), fetches `GET
    /// /indexes/:id/status` and inspects `search_capabilities`. If the
    /// prerequisite lane is missing, returns
    /// `DispatchError::StageNotReady` carrying a human-readable message,
    /// the full stages snapshot, and a `suggested_tools` retry hint.
    /// Otherwise constructs the search body (including lane-specific
    /// `stage` and `expand_graph` settings), forwards optional caller
    /// fields (`top_k`, `mode`, branch-aware boost, archive filter),
    /// POSTs the request, and mirrors the daemon's per-query INFO log.
    ///
    /// Test: unit tests at the bottom of this file exercise each tool's
    /// happy path, stage-not-ready path, and routing shape.
    async fn run_lane_search(
        &self,
        args: &Value,
        lane: SearchLane,
    ) -> Result<Value, DispatchError> {
        let index_id = require_str(args, "index_id")?;
        let query_text = require_str(args, "query")?;

        // Pre-flight stage check for lanes that need Stage 2 or Stage 3.
        // The lexical and `search_all` tools always work — they degrade
        // gracefully through the daemon's adaptive routing.
        if let Some(required) = lane.required_capability() {
            let status = self.get(&format!("/indexes/{index_id}/status")).await?;
            let caps: Vec<String> = status
                .get("search_capabilities")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            if !caps.iter().any(|c| c == required) {
                let stages = status
                    .get("stages")
                    .cloned()
                    .unwrap_or(Value::Object(Default::default()));
                let stages_summary = summarise_stages(&stages);
                let suggested = lane.suggested_fallback_tools(&caps);
                let suggested_list = suggested.join(", ");
                let stage_label = lane.stage_label();
                let message = format!(
                    "{tool} requires Stage {stage_num} ({stage_name}), which is not yet \
                     ready on index '{index_id}'. Current stages: {stages_summary}. \
                     Suggested: use {suggested_list}, or wait for the reindex to complete.",
                    tool = lane.tool_name(),
                    stage_num = lane.stage_number(),
                    stage_name = stage_label,
                );
                return Err(DispatchError::StageNotReady {
                    message,
                    current_stages: stages,
                    suggested_tools: suggested,
                });
            }
        }

        // Build the SearchQuery body honouring the lane pin plus optional
        // caller-supplied fields. The bare-string `query` form is the
        // primary shape; advanced callers may still pass `query` as an
        // object to override defaults (kept for symmetry with `search`).
        let mut body = match args.get("query") {
            Some(v @ Value::Object(_)) => v.clone(),
            _ => serde_json::json!({ "text": query_text }),
        };
        // Lane-specific `stage` and `expand_graph` pinning. Always
        // overwrites whatever the caller may have set so the LLM's tool
        // selection is honoured (a `search_lexical` call must NOT silently
        // run the full hybrid pipeline because the caller forgot to clear
        // `stage`).
        if let Some(stage_str) = lane.stage_serde_value() {
            body["stage"] = Value::String(stage_str.into());
        } else {
            body.as_object_mut().map(|m| m.remove("stage"));
        }
        body["expand_graph"] = Value::Bool(lane.expand_graph_default());

        // Pass-through optional caller fields. `top_k` is the only one
        // that's lane-relevant for cost control; the rest are shared with
        // the `search` tool for parity.
        if let Some(k) = args.get("top_k").and_then(Value::as_u64) {
            body["top_k"] = Value::from(k);
        }
        if let Some(bf) = args.get("branch_files") {
            body["branch_files"] = bf.clone();
        }
        if let Some(bb) = args.get("branch_boost") {
            body["branch_boost"] = bb.clone();
        }
        if let Some(br) = args.get("branch").and_then(Value::as_str) {
            body["branch"] = Value::String(br.to_string());
        }
        if let Some(m) = args.get("mode").and_then(Value::as_str) {
            body["mode"] = Value::String(m.to_string());
        }
        if let Some(ea) = args.get("exclude_archived").and_then(Value::as_bool) {
            body["exclude_archived"] = Value::Bool(ea);
        }
        // Issue #147: pass refine_query through to the search body for the
        // KG lane. The field is ignored by lexical / semantic lanes since
        // `expand_with_kg` only reads it when the graph stage is active.
        if let Some(rq) = args.get("refine_query").and_then(Value::as_str) {
            body["refine_query"] = Value::String(rq.to_string());
        }

        let resp = self
            .post(&format!("/indexes/{index_id}/search"), &body)
            .await?;
        // Mirror the daemon's per-query INFO log so the MCP transport
        // surfaces the same query/intent/latency line.
        let log_intent = resp
            .get("intent")
            .and_then(Value::as_str)
            .unwrap_or("Unknown");
        let log_latency = resp.get("latency_ms").and_then(Value::as_u64).unwrap_or(0);
        let log_results = resp
            .get("results")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        let tool_name = lane.tool_name();
        tracing::info!(
            tool = %tool_name,
            index_id = %index_id,
            intent = %log_intent,
            latency_ms = log_latency,
            results = log_results,
            query = %&query_text[..query_text.len().min(80)],
            "search"
        );
        Ok(resp)
    }
}

/// Per-lane router (issue #138).
///
/// Why: maps the four MCP tool names to their canonical lane combination
/// without scattering the policy across the dispatcher arms.
/// What: a unit-like enum that knows its `SearchStage` serde value,
/// `expand_graph` default, prerequisite `search_capabilities` entry, and
/// human-readable stage label / number for STAGE_NOT_READY messages.
/// Test: per-lane routing covered by `search_*_tool_routes_to_*_stage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchLane {
    /// BM25 + grep-fallback only. Always available; no prerequisite.
    Lexical,
    /// BM25 + HNSW via RRF. Requires Stage 2 (`vector`).
    Semantic,
    /// BM25 + HNSW + KG expansion. Requires Stage 3 (`kg`).
    Graph,
    /// Full hybrid pipeline; adaptive routing chooses what to run.
    All,
}

impl SearchLane {
    fn tool_name(self) -> &'static str {
        match self {
            SearchLane::Lexical => "search_lexical",
            SearchLane::Semantic => "search_semantic",
            SearchLane::Graph => "search_kg",
            SearchLane::All => "search_all",
        }
    }

    /// Serialised value for `SearchQuery::stage`, or `None` for adaptive
    /// (no pin).
    fn stage_serde_value(self) -> Option<&'static str> {
        match self {
            SearchLane::Lexical => Some("lexical"),
            SearchLane::Semantic => Some("semantic"),
            SearchLane::Graph => Some("graph"),
            SearchLane::All => None,
        }
    }

    fn expand_graph_default(self) -> bool {
        matches!(self, SearchLane::Graph | SearchLane::All)
    }

    /// Required entry in `search_capabilities` (returned by
    /// `GET /indexes/:id/status`). `None` for lanes that always work.
    fn required_capability(self) -> Option<&'static str> {
        match self {
            SearchLane::Lexical | SearchLane::All => None,
            SearchLane::Semantic => Some("vector"),
            SearchLane::Graph => Some("kg"),
        }
    }

    fn stage_number(self) -> u8 {
        match self {
            SearchLane::Lexical => 1,
            SearchLane::Semantic => 2,
            SearchLane::Graph => 3,
            SearchLane::All => 0,
        }
    }

    fn stage_label(self) -> &'static str {
        match self {
            SearchLane::Lexical => "lexical",
            SearchLane::Semantic => "embeddings",
            SearchLane::Graph => "symbol graph",
            SearchLane::All => "all",
        }
    }

    /// Compute the LLM's retry hint when this lane is unavailable. Returns
    /// a small ordered list of alternative tools that are likely to
    /// succeed given the current `caps` snapshot.
    fn suggested_fallback_tools(self, caps: &[String]) -> Vec<&'static str> {
        let has_vector = caps.iter().any(|c| c == "vector");
        match self {
            // Semantic missing: lexical always works; full hybrid degrades
            // to whatever's ready.
            SearchLane::Semantic => vec!["search_lexical", "search_all"],
            // Graph missing: semantic if vector ready, else lexical.
            SearchLane::Graph => {
                if has_vector {
                    vec!["search_semantic", "search_lexical", "search_all"]
                } else {
                    vec!["search_lexical", "search_all"]
                }
            }
            // Lexical / All never fall through this path (no prereq).
            SearchLane::Lexical | SearchLane::All => vec!["search_lexical"],
        }
    }
}

/// Render an `IndexStages` JSON value as a compact debug-style string for
/// the human-readable STAGE_NOT_READY message. Lifted into a free
/// function so the unit tests can call it directly.
///
/// Why: the `current_stages` JSON snapshot already lives in the structured
/// `_meta` / `error.data` field; the textual message just needs a short
/// summary like `lexical=Ready, semantic=InProgress, graph=Pending` so a
/// human reader (or LLM) can see at a glance which stages are blocking.
/// What: walks the three known stage keys (`lexical`, `semantic`, `graph`)
/// and pulls each one's `.status` field, falling back to `"unknown"` when
/// the field is missing or non-string.
/// Test: `summarise_stages_renders_in_order`.
fn summarise_stages(stages: &Value) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(3);
    for key in ["lexical", "semantic", "graph"] {
        let status = stages
            .get(key)
            .and_then(|s| s.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        // Title-case the status for human readability (snake_case → CamelCase).
        let pretty = status
            .split('_')
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<String>();
        parts.push(format!("{key}={pretty}"));
    }
    parts.join(", ")
}

#[derive(Debug)]
enum DispatchError {
    UnknownTool,
    InvalidParams(String),
    Transport(String),
    /// Issue #138 — a per-lane tool was called but its prerequisite stage
    /// is not `Ready` on this index. Carries the resolved stage map and a
    /// retry hint so the LLM can pick a different tool without a second
    /// round-trip.
    StageNotReady {
        message: String,
        current_stages: Value,
        suggested_tools: Vec<&'static str>,
    },
}

fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, DispatchError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| DispatchError::InvalidParams(format!("missing or non-string '{key}'")))
}

/// Wrap a structured JSON result in MCP's `content[]` envelope so downstream
/// LLM clients can render it directly.
fn wrap_text_content(value: &Value) -> Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
        }]
    })
}

/// Wrap a successful `tools/call` payload with the spec-required
/// `isError: false` flag so MCP clients can branch without parsing text.
fn wrap_tool_result(value: &Value, is_error: bool) -> Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
        }],
        "isError": is_error,
    })
}

/// Wrap a tool execution failure as `{content, isError: true}` per MCP spec.
fn wrap_tool_error(msg: &str) -> Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!("Error: {msg}"),
        }],
        "isError": true,
    })
}

/// Wrap a STAGE_NOT_READY error in MCP's structured tool-error envelope
/// (issue #138).
///
/// Why: MCP `tools/call` failures use `isError: true` rather than the
/// JSON-RPC error envelope. The LLM gets the human-readable text in
/// `content[]` AND a machine-readable `_meta` block with the exact retry
/// hint (`suggested_tools`) and the current stages snapshot so it can
/// pick the right fallback tool without a second probe.
/// What: returns a JSON object matching the spec in issue #138 — `isError:
/// true`, single text content node, and `_meta` carrying `error_code`,
/// `current_stages`, and `suggested_tools`.
/// Test: `search_semantic_tool_returns_stage_not_ready_when_stage_2_missing`.
fn wrap_stage_not_ready_error(
    message: &str,
    current_stages: &Value,
    suggested_tools: &[&'static str],
) -> Value {
    serde_json::json!({
        "isError": true,
        "content": [{
            "type": "text",
            "text": message,
        }],
        "_meta": {
            "error_code": "STAGE_NOT_READY",
            "current_stages": current_stages,
            "suggested_tools": suggested_tools,
        }
    })
}

/// Static metadata for `tools/list`. Keep in sync with [`McpServer::call_tool`].
pub fn tool_descriptors() -> Value {
    serde_json::json!([
        // Issue #138 — per-lane MCP tools. Tool descriptions are
        // first-class LLM prompts: each one opens with "when to use",
        // gives concrete fit/don't-fit examples, states the cost, and
        // explains the failure mode (STAGE_NOT_READY). The legacy
        // `search` tool is preserved below as a back-compat alias.
        {
            "name": "search_lexical",
            "description": "Find code by exact symbol name, regex, or literal string. Equivalent to a fast ripgrep on the indexed codebase. Use this FIRST for any query where the user mentions a specific identifier (function name, struct name, file name) or a literal phrase. Best for: `apply_archive_downrank`, `pub fn main`, `\"TODO: refactor\"`, filename globs like `*.toml`. Don't use for: conceptual queries like \"how does authentication work\" — use `search_semantic` instead. Always available on any indexed project. Cheapest tool in this family.",
            "inputSchema": {
                "type": "object",
                "required": ["index_id", "query"],
                "properties": {
                    "index_id":         { "type": "string", "description": "Target index id (from `list_indexes`)" },
                    "query":            { "type": "string", "description": "Exact symbol, regex, or literal phrase" },
                    "top_k":            { "type": "integer", "default": 10 },
                    "mode":             { "type": "string", "enum": ["code", "text", "data"], "default": "code" },
                    "exclude_archived": { "type": "boolean", "default": false },
                    "branch_files":     { "type": "array", "items": { "type": "string" } },
                    "branch_boost":     { "type": "number" },
                    "branch":           { "type": "string" }
                },
                "examples": [
                    { "index_id": "trusty-tools", "query": "apply_archive_downrank" },
                    { "index_id": "trusty-tools", "query": "pub fn main" },
                    { "index_id": "trusty-tools", "query": "TODO: refactor" }
                ]
            }
        },
        {
            "name": "search_semantic",
            "description": "Find code by meaning, not by literal text. Uses embedding-based similarity to retrieve chunks that semantically match the query, even when the query words don't appear in the code. Best for: \"code that handles JWT verification\", \"the place that does community detection\", \"how does the embedder batch requests\". Don't use for: exact symbol lookups (use `search_lexical`) or finding callers of a known function (use `search_kg`). Requires Stage 2 (embeddings) to be ready on the index — returns a STAGE_NOT_READY error with a `suggested_tools` retry hint if not. Medium cost.",
            "inputSchema": {
                "type": "object",
                "required": ["index_id", "query"],
                "properties": {
                    "index_id":         { "type": "string" },
                    "query":            { "type": "string", "description": "Conceptual query — meaning, not literal text" },
                    "top_k":            { "type": "integer", "default": 10 },
                    "mode":             { "type": "string", "enum": ["code", "text", "data"], "default": "code" },
                    "exclude_archived": { "type": "boolean", "default": false }
                },
                "examples": [
                    { "index_id": "trusty-tools", "query": "code that handles JWT verification" },
                    { "index_id": "trusty-tools", "query": "the place that does community detection" }
                ]
            }
        },
        {
            "name": "search_kg",
            "description": "Explore code structure from a known seed — either a chunk_id (from a previous search result) or a symbol name. Returns chunks connected to the seed via `calls`, `called_by`, `contains`, `inherits` edges. Best for: \"what calls `validate_token`\", \"what does `Authenticator` use internally\", impact analysis before a refactor. Don't use for: free-text discovery (use `search_semantic`) or initial entry-point finding (use `search_lexical` first). Requires Stage 3 (symbol graph) to be ready. Returns empty if the seed is not in the index. Cheap once you have a seed. Optional `refine_query`: provide a longer natural-language description to rerank and filter the expanded neighbourhood by semantic relevance — useful when the seed chunk is correct but you want only the most relevant callers/callees (issue #147).",
            "inputSchema": {
                "type": "object",
                "required": ["index_id", "query"],
                "properties": {
                    "index_id":      { "type": "string" },
                    "query":         { "type": "string", "description": "Seed: a symbol name or chunk_id from a previous result" },
                    "top_k":         { "type": "integer", "default": 10 },
                    "mode":          { "type": "string", "enum": ["code", "text", "data"], "default": "code" },
                    "refine_query":  { "type": "string", "description": "Optional: rerank and filter expanded KG neighbours by cosine similarity to this natural-language description. Neighbours below the 0.4 cosine threshold are dropped. Omit to use default KG expansion without filtering." }
                },
                "examples": [
                    { "index_id": "trusty-tools", "query": "validate_token" },
                    { "index_id": "trusty-tools", "query": "Authenticator", "refine_query": "callers that handle token refresh in the auth middleware" }
                ]
            }
        },
        {
            "name": "search_all",
            "description": "When in doubt, use this. Runs the full hybrid pipeline (lexical + semantic + KG expansion) and merges results via RRF. More expensive than the targeted tools but catches edge cases. Use when: your query has both literal symbols AND conceptual phrasing (\"find the `AuthValidator` that handles refresh tokens\"), or when you've tried the targeted tools and they didn't surface what you need. Always available; gracefully degrades to whatever lanes are ready. When called without `index_id`, falls back to legacy cross-project fan-out behaviour (issue #10) — provide `index_id` for the per-index hybrid path.",
            "inputSchema": {
                "type": "object",
                "required": ["query"],
                "properties": {
                    "index_id":         { "type": "string", "description": "Target index (omit for cross-project fan-out)" },
                    "query":            { "type": "string" },
                    "top_k":            { "type": "integer", "default": 10 },
                    "mode":             { "type": "string", "enum": ["code", "text", "data"], "default": "code" },
                    "exclude_archived": { "type": "boolean", "default": false },
                    "full_content":     { "type": "boolean", "default": false, "description": "Legacy fan-out only: include full chunk content in each hit" },
                    "branch_files":     { "type": "array", "items": { "type": "string" } },
                    "branch_boost":     { "type": "number" },
                    "branch":           { "type": "string" }
                },
                "examples": [
                    { "index_id": "trusty-tools", "query": "AuthValidator that handles refresh tokens" },
                    { "query": "global cross-project fan-out without index_id" }
                ]
            }
        },
        {
            "name": "search",
            "description": "Unified hybrid search (BM25+vector+KG+RRF) with mode-aware ranking (issue #77). The `mode` parameter (\"code\" | \"text\" | \"data\", default \"code\") picks the file-type penalty matrix: code prefers source (prose 0.1x, data 0.2x); text prefers prose docs (source 0.5x, data 0.3x); data prefers structured data (source 0.3x, prose 0.3x). Set `exclude_archived: true` to drop archived/deprecated/legacy chunks entirely instead of downranking them (issue #74). Supports branch-aware scoring via branch_files/branch_boost/branch (issue #122). Replaces the legacy `search_code` tool name; callers that omit `mode` get identical pre-#77 behaviour.",
            "inputSchema": {
                "type": "object",
                "required": ["index_id", "query"],
                "properties": {
                    "index_id": { "type": "string" },
                    "query": { "type": "string" },
                    "top_k": { "type": "integer", "default": 10 },
                    "mode": {
                        "type": "string",
                        "enum": ["code", "text", "data"],
                        "default": "code",
                        "description": "Ranking mode: prefer source code, prose docs, or structured data."
                    },
                    "exclude_archived": {
                        "type": "boolean",
                        "default": false,
                        "description": "Drop archived/deprecated/legacy chunks (paths like _archive/, archive/, _deprecated/, old/, .archive/; #[deprecated]; .archived/DEPRECATED markers) instead of downranking them."
                    },
                    "branch_files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Files modified on current git branch (relative to index root). Boosted in results."
                    },
                    "branch_boost": {
                        "type": "number",
                        "description": "Score multiplier for branch files (default 1.5, range 1.0-3.0)."
                    },
                    "branch": {
                        "type": "string",
                        "description": "Branch name; daemon will compute branch_files via git if branch_files is absent."
                    }
                }
            }
        },
        {
            "name": "index_file",
            "description": "Add or update one file in an index",
            "inputSchema": {
                "type": "object",
                "required": ["index_id", "path", "content"],
                "properties": {
                    "index_id": { "type": "string" },
                    "path":     { "type": "string" },
                    "content":  { "type": "string" }
                }
            }
        },
        {
            "name": "remove_file",
            "description": "Remove a file's chunks from an index",
            "inputSchema": {
                "type": "object",
                "required": ["index_id", "path"],
                "properties": {
                    "index_id": { "type": "string" },
                    "path":     { "type": "string" }
                }
            }
        },
        {
            "name": "list_indexes",
            "description": "List all registered indexes on this daemon",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "create_index",
            "description": "Register a new (empty) index",
            "inputSchema": {
                "type": "object",
                "required": ["id", "root_path"],
                "properties": {
                    "id":        { "type": "string" },
                    "root_path": { "type": "string" }
                }
            }
        },
        {
            "name": "search_similar",
            "description": "Find chunks semantically similar to a given file/function via HNSW (issue #31)",
            "inputSchema": {
                "type": "object",
                "required": ["file"],
                "properties": {
                    "file":     { "type": "string" },
                    "function": { "type": "string" },
                    "top_k":    { "type": "number" },
                    "index":    { "type": "string" }
                }
            }
        },
        {
            "name": "search_health",
            "description": "Probe daemon liveness and version",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "delete_index",
            "description": "Delete a registered index and all its data",
            "inputSchema": {
                "type": "object",
                "required": ["index_id"],
                "properties": {
                    "index_id": { "type": "string" }
                }
            }
        },
        {
            "name": "reindex",
            "description": "Trigger a full reindex of a collection (async, returns immediately)",
            "inputSchema": {
                "type": "object",
                "required": ["index_id"],
                "properties": {
                    "index_id":  { "type": "string" },
                    "root_path": { "type": "string" }
                }
            }
        },
        {
            "name": "index_status",
            "description": "Get stats for an index (chunk count, root path)",
            "inputSchema": {
                "type": "object",
                "required": ["index_id"],
                "properties": {
                    "index_id": { "type": "string" }
                }
            }
        },
        {
            "name": "list_chunks",
            "description": "Paginated enumeration of every chunk in an index (issue #54). Stable order by (file, start_line).",
            "inputSchema": {
                "type": "object",
                "required": ["index_id"],
                "properties": {
                    "index_id": { "type": "string" },
                    "offset":   { "type": "integer", "default": 0 },
                    "limit":    { "type": "integer", "default": 100 }
                }
            }
        },
        {
            "name": "get_call_chain",
            "description": "Annotated call tree for a function entry point (issue #76). \
                            Returns plain-text prose with the entry function's signature, \
                            Why/What doc lines, its depth-1 callees with full source, and \
                            its depth-1 callers as signatures only. LLMs read this prose \
                            tree more reliably than JSON. Entry point accepts an exact \
                            symbol name, a case-insensitive fuzzy substring, or a \
                            `file:line` reference; the most-connected match wins ties.",
            "inputSchema": {
                "type": "object",
                "required": ["index_id", "entry_point"],
                "properties": {
                    "index_id":       { "type": "string" },
                    "entry_point":    { "type": "string", "description": "Function name, fuzzy substring, or file:line" },
                    "direction":      { "type": "string", "enum": ["both", "outgoing", "callers"], "default": "both" },
                    "max_depth":      { "type": "integer", "minimum": 1, "maximum": 4, "default": 2 },
                    "include_source": { "type": "boolean", "default": true, "description": "Embed full source at depth <= 1" }
                }
            }
        },
        {
            "name": "grep",
            "description": "Search indexed files using regex/literal patterns with ripgrep-compatible options. \
                            Greps the on-disk bytes of files the index already knows about, so no \
                            re-embedding occurs and line numbers are exact. Supports regex or fixed-string \
                            matching, case folding (-i), context windows (-A/-B/-C), include globs, \
                            multiline mode, files-with-matches (-l), invert (-v), and word-regexp (-w). \
                            When `index_id` is omitted the daemon fans out across every registered index.",
            "inputSchema": {
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern":            { "type": "string", "description": "Regex (default) or literal when fixed_strings=true" },
                    "index_id":           { "type": "string", "description": "Optional index id; omit to fan out across all indexes" },
                    "case_insensitive":   { "type": "boolean", "default": false, "description": "-i / --ignore-case" },
                    "context":            { "type": "integer", "description": "-C: equal before/after context, overrides context_before/context_after" },
                    "context_before":     { "type": "integer", "description": "-B: lines of context before each match" },
                    "context_after":      { "type": "integer", "description": "-A: lines of context after each match" },
                    "glob":                { "type": "string", "description": "--include glob (e.g. '**/*.rs')" },
                    "multiline":          { "type": "boolean", "default": false, "description": "Let `.` span newlines" },
                    "fixed_strings":      { "type": "boolean", "default": false, "description": "-F: treat pattern as literal" },
                    "files_with_matches": { "type": "boolean", "default": false, "description": "-l: return one path per matching file" },
                    "invert_match":       { "type": "boolean", "default": false, "description": "-v: return lines that do NOT match" },
                    "word_regexp":        { "type": "boolean", "default": false, "description": "-w: require word boundaries" },
                    "max_results":        { "type": "integer", "default": 100, "description": "Hard cap on returned matches (alias: max_count)" },
                    "max_count":          { "type": "integer", "description": "Alias for max_results (ripgrep --max-count parity)" }
                }
            }
        },
        {
            "name": "chat",
            "description": "Ask a natural-language question about the indexed codebase. \
                            Automatically searches for the top_k most relevant chunks and \
                            sends them as context to an OpenRouter LLM (default model: \
                            anthropic/claude-haiku-4.5). Returns {answer, sources, model}. \
                            Requires OPENROUTER_API_KEY env var on the daemon, or an \
                            `api_key` field in the request.",
            "inputSchema": {
                "type": "object",
                "required": ["index_id"],
                "properties": {
                    "index_id": { "type": "string" },
                    "message":  { "type": "string", "description": "User question (alias: question)" },
                    "question": { "type": "string", "description": "User question (alias: message)" },
                    "history":  { "type": "array", "items": { "type": "object" } },
                    "model":    { "type": "string", "description": "OpenRouter model id (default: anthropic/claude-haiku-4.5)" },
                    "top_k":    { "type": "integer", "description": "Number of context chunks (default: 5)", "default": 5 },
                    "api_key":  { "type": "string", "description": "Fallback OpenRouter API key when OPENROUTER_API_KEY env is unset" }
                }
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, params: Value) -> Request {
        Request {
            jsonrpc: Some("2.0".into()),
            id: Some(Value::from(1u64)),
            method: method.into(),
            params: Some(params),
        }
    }

    #[tokio::test]
    async fn rejects_wrong_jsonrpc_version() {
        let server = McpServer::new("http://127.0.0.1:1");
        let r = Request {
            jsonrpc: Some("1.0".into()),
            id: Some(Value::from(7u64)),
            method: "search_health".into(),
            params: None,
        };
        let resp = server.dispatch(r).await;
        let err = resp.error.expect("expected error");
        assert_eq!(err.code, error_codes::INVALID_REQUEST);
        assert_eq!(resp.id, Some(Value::from(7u64)));
    }

    #[tokio::test]
    async fn unknown_tool_returns_method_not_found() {
        let server = McpServer::new("http://127.0.0.1:1");
        let resp = server.dispatch(req("not_a_tool", Value::Null)).await;
        let err = resp.error.expect("expected error");
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn missing_params_returns_invalid_params() {
        let server = McpServer::new("http://127.0.0.1:1");
        let resp = server
            .dispatch(req("index_file", serde_json::json!({})))
            .await;
        let err = resp.error.expect("expected error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn tools_list_returns_all_tools() {
        let server = McpServer::new("http://127.0.0.1:1");
        let resp = server.dispatch(req("tools/list", Value::Null)).await;
        let result = resp.result.expect("expected result");
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .expect("array");
        // Issue #36 requires the 6 core MCP tools to be present; we ship
        // additional tools beyond that minimum.
        assert!(
            tools.len() >= 6,
            "expected at least 6 tools, got {}",
            tools.len()
        );
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        for required in [
            "search",
            "index_file",
            "remove_file",
            "list_indexes",
            "create_index",
            "search_health",
        ] {
            assert!(
                names.contains(&required),
                "missing required tool: {required}"
            );
        }
    }

    /// Issue #36 — verify the `initialize` handshake returns the spec-shaped
    /// payload Claude Code expects on startup.
    #[tokio::test]
    async fn test_initialize_response() {
        let server = McpServer::new("http://127.0.0.1:1");
        let r = Request {
            jsonrpc: Some("2.0".into()),
            id: Some(Value::from(1u64)),
            method: "initialize".into(),
            params: Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.0.0" }
            })),
        };
        let resp = server.dispatch(r).await;
        assert!(resp.error.is_none(), "initialize must not error");
        let result = resp.result.expect("expected result");
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert!(result["capabilities"].get("tools").is_some());
        assert_eq!(result["serverInfo"]["name"], "trusty-search");
        assert!(result["serverInfo"]["version"].is_string());
    }

    /// Issue #36 — `tools/list` must surface every spec-required tool so
    /// MCP clients can render the full manifest.
    #[tokio::test]
    async fn test_tools_list_response() {
        let server = McpServer::new("http://127.0.0.1:1");
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
            "search",
            "index_file",
            "remove_file",
            "list_indexes",
            "create_index",
            "search_health",
        ] {
            assert!(
                names.contains(&required),
                "tools/list missing '{required}' (got {names:?})"
            );
        }
        // Each tool must carry an inputSchema so clients can validate args.
        for t in tools {
            assert!(t.get("name").is_some());
            assert!(t.get("inputSchema").is_some());
        }
    }

    /// Issue #36 — JSON-RPC method-not-found surfaces as -32601.
    #[tokio::test]
    async fn test_unknown_method_returns_error() {
        let server = McpServer::new("http://127.0.0.1:1");
        let resp = server
            .dispatch(req("definitely_not_a_method", Value::Null))
            .await;
        let err = resp.error.expect("expected error");
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
    }

    /// `notifications/initialized` is a JSON-RPC notification — the server
    /// must NOT emit a response, signalled by `Response::suppress = true`.
    #[tokio::test]
    async fn notification_initialized_is_suppressed() {
        let server = McpServer::new("http://127.0.0.1:1");
        let r = Request {
            jsonrpc: Some("2.0".into()),
            id: None, // notifications carry no id
            method: "notifications/initialized".into(),
            params: None,
        };
        let resp = server.dispatch(r).await;
        assert!(resp.suppress, "notifications must be suppressed");
    }

    /// Parity gate: every HTTP endpoint reachable via REST must also be callable
    /// as an MCP tool. This guards the "MCP and HTTP are functionally equivalent"
    /// invariant — if a new HTTP route lands without a matching tool, this fails.
    #[tokio::test]
    async fn test_tools_list_complete() {
        let server = McpServer::new("http://127.0.0.1:1");
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
            "search",
            "index_file",
            "remove_file",
            "list_indexes",
            "create_index",
            "search_health",
            "delete_index",
            "reindex",
            "index_status",
            "list_chunks",
            "chat",
            "search_all",
        ] {
            assert!(
                names.contains(&required),
                "tools/list missing '{required}' (got {names:?})"
            );
        }
    }

    /// Issue #10 — `search_all` requires the `query` arg and rejects missing it
    /// before any HTTP round-trip.
    #[tokio::test]
    async fn search_all_missing_query_returns_invalid_params() {
        let server = McpServer::new("http://127.0.0.1:1");
        let resp = server
            .dispatch(req("search_all", serde_json::json!({})))
            .await;
        let err = resp.error.expect("expected error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn tools_call_without_name_returns_invalid_params() {
        let server = McpServer::new("http://127.0.0.1:1");
        let resp = server
            .dispatch(req("tools/call", serde_json::json!({})))
            .await;
        let err = resp.error.expect("expected error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    /// `grep` is listed and missing-pattern fast-fails before any HTTP hop.
    #[tokio::test]
    async fn grep_missing_pattern_returns_invalid_params() {
        let server = McpServer::new("http://127.0.0.1:1");
        let resp = server.dispatch(req("grep", serde_json::json!({}))).await;
        let err = resp.error.expect("expected error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    /// Issue #447 — `max_count` is forwarded as `max_results` to the daemon.
    ///
    /// Why: when the MCP client passes `max_count` (ripgrep's `--max-count`
    /// flag name) the dispatcher must translate it to `max_results` before
    /// POSTing to the daemon. Without the alias the parameter was silently
    /// dropped and the daemon applied its default cap of 100 regardless.
    /// What: asserts that a `grep` call with `max_count=5` (and no
    /// `max_results`) forwards `max_results: 5` in the daemon request body.
    /// Test: spins up a tiny mock daemon that echoes back the request body,
    /// then asserts the forwarded body contains `max_results == 5`.
    #[tokio::test]
    async fn grep_max_count_alias_forwarded_as_max_results() {
        use axum::routing::post;
        use axum::{Json, Router};
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let captured_clone = Arc::clone(&captured);

        async fn grep_handler(
            axum::extract::State(captured): axum::extract::State<Arc<Mutex<Option<Value>>>>,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            *captured.lock().await = Some(body);
            Json(serde_json::json!({ "matches": [], "total": 0, "truncated": false }))
        }

        let app = Router::new()
            .route("/indexes/idx/grep", post(grep_handler))
            .with_state(captured_clone);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let server = McpServer::new(format!("http://{addr}"));
        let resp = server
            .dispatch(req(
                "grep",
                serde_json::json!({
                    "pattern": "fn foo",
                    "index_id": "idx",
                    "max_count": 5_u64,
                }),
            ))
            .await;
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
        let body = captured.lock().await.clone().expect("no request captured");
        assert_eq!(
            body.get("max_results").and_then(Value::as_u64),
            Some(5),
            "max_count must be forwarded as max_results; got body: {body:?}"
        );
    }

    /// `grep` appears in `tools/list` with a `pattern`-required schema.
    #[tokio::test]
    async fn grep_listed_in_tools_with_required_pattern() {
        let server = McpServer::new("http://127.0.0.1:1");
        let resp = server.dispatch(req("tools/list", Value::Null)).await;
        let result = resp.result.expect("expected result");
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .expect("array");
        let grep = tools
            .iter()
            .find(|t| t.get("name").and_then(Value::as_str) == Some("grep"))
            .expect("grep tool missing from tools/list");
        let required = grep["inputSchema"]["required"]
            .as_array()
            .expect("required array");
        assert!(
            required.iter().any(|v| v.as_str() == Some("pattern")),
            "grep schema must require 'pattern'"
        );
    }

    // ----------------------------------------------------------------
    // Issue #138 — per-lane MCP tools
    // ----------------------------------------------------------------

    /// Spin up a one-shot axum mock daemon on a loopback port. The handler
    /// closure controls what `GET /indexes/:id/status` and
    /// `POST /indexes/:id/search` return. Each call captures the inbound
    /// request body into the shared `captured` slot so tests can assert
    /// against the SearchQuery shape the MCP tool dispatched.
    ///
    /// Returns `(base_url, captured_search_bodies, captured_search_paths)`.
    async fn spawn_mock_daemon(
        status_response: Value,
        search_response: Value,
    ) -> (
        String,
        std::sync::Arc<tokio::sync::Mutex<Vec<Value>>>,
        std::sync::Arc<tokio::sync::Mutex<Vec<String>>>,
    ) {
        use axum::extract::{Path, State};
        use axum::routing::{get, post};
        use axum::{Json, Router};
        use std::sync::Arc;
        use tokio::sync::Mutex;

        #[derive(Clone)]
        struct MockState {
            status_response: Value,
            search_response: Value,
            captured_bodies: Arc<Mutex<Vec<Value>>>,
            captured_paths: Arc<Mutex<Vec<String>>>,
        }

        let captured_bodies: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_paths: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let state = MockState {
            status_response,
            search_response,
            captured_bodies: Arc::clone(&captured_bodies),
            captured_paths: Arc::clone(&captured_paths),
        };

        async fn status_handler(Path(id): Path<String>, State(s): State<MockState>) -> Json<Value> {
            // Inject the index_id so the handler returns a payload that
            // looks like a real daemon response.
            let mut v = s.status_response.clone();
            if v.is_object() {
                v["index_id"] = Value::String(id);
            }
            Json(v)
        }

        async fn search_handler_mock(
            Path(id): Path<String>,
            State(s): State<MockState>,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            s.captured_paths
                .lock()
                .await
                .push(format!("/indexes/{id}/search"));
            s.captured_bodies.lock().await.push(body);
            Json(s.search_response.clone())
        }

        let app = Router::new()
            .route("/indexes/{id}/status", get(status_handler))
            .route("/indexes/{id}/search", post(search_handler_mock))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let base_url = format!("http://{addr}");
        (base_url, captured_bodies, captured_paths)
    }

    /// `summarise_stages` renders the three known keys in lexical →
    /// semantic → graph order and Title-cases snake_case statuses.
    #[test]
    fn summarise_stages_renders_in_order() {
        let stages = serde_json::json!({
            "lexical":  { "status": "ready" },
            "semantic": { "status": "in_progress" },
            "graph":    { "status": "pending" },
        });
        let s = summarise_stages(&stages);
        assert_eq!(s, "lexical=Ready, semantic=InProgress, graph=Pending");
    }

    /// `tools/list` returns five search tools after #138 (legacy `search`
    /// plus the four per-lane tools). Bumps the original
    /// `test_tools_list_complete` assertion.
    #[tokio::test]
    async fn tools_list_returns_five_search_tools() {
        let server = McpServer::new("http://127.0.0.1:1");
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
            "search",
            "search_lexical",
            "search_semantic",
            "search_kg",
            "search_all",
        ] {
            assert!(
                names.contains(&required),
                "tools/list missing '{required}' (got {names:?})"
            );
        }
        // Spec: exactly five "search*" tools (the four new + legacy).
        let search_tools: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| *n == "search" || n.starts_with("search_"))
            .collect();
        // `search_similar` and `search_health` also start with "search_"
        // but are distinct surfaces; assert only on the lane-related ones.
        let lane_tools: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| {
                matches!(
                    *n,
                    "search" | "search_lexical" | "search_semantic" | "search_kg" | "search_all"
                )
            })
            .collect();
        assert_eq!(
            lane_tools.len(),
            5,
            "expected exactly 5 lane-related search tools, got {lane_tools:?} (all: {search_tools:?})"
        );
    }

    /// Each per-lane tool description embeds the authoring-guide hook
    /// (when-to-use phrasing) so the LLM can pick reliably.
    #[tokio::test]
    async fn per_lane_tool_descriptions_carry_when_to_use_hooks() {
        let server = McpServer::new("http://127.0.0.1:1");
        let resp = server.dispatch(req("tools/list", Value::Null)).await;
        let result = resp.result.expect("expected result");
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .expect("array");
        for (name, hook) in [
            ("search_lexical", "exact symbol name"),
            ("search_semantic", "by meaning"),
            ("search_kg", "from a known seed"),
            ("search_all", "When in doubt"),
        ] {
            let tool = tools
                .iter()
                .find(|t| t.get("name").and_then(Value::as_str) == Some(name))
                .unwrap_or_else(|| panic!("tool {name} missing"));
            let desc = tool["description"].as_str().expect("description");
            assert!(
                desc.contains(hook),
                "tool {name} description must mention '{hook}': {desc}"
            );
        }
    }

    /// `search_lexical` pins `stage=lexical` and `expand_graph=false` on
    /// the dispatched SearchQuery. Always-available — no status pre-check
    /// because Stage 1 is the baseline for every index.
    #[tokio::test]
    async fn search_lexical_tool_routes_to_lexical_stage_only() {
        let status = serde_json::json!({
            "stages": {
                "lexical":  { "status": "ready" },
                "semantic": { "status": "pending" },
                "graph":    { "status": "pending" },
            },
            "search_capabilities": ["bm25", "literal", "exact_match"],
        });
        let search = serde_json::json!({
            "results": [],
            "intent": "Definition",
            "latency_ms": 1,
        });
        let (base, bodies, paths) = spawn_mock_daemon(status, search).await;
        let server = McpServer::new(base);
        let resp = server
            .dispatch(req(
                "search_lexical",
                serde_json::json!({
                    "index_id": "demo",
                    "query": "apply_archive_downrank",
                    "top_k": 5,
                }),
            ))
            .await;
        assert!(resp.error.is_none(), "lexical tool must not error");

        let bodies = bodies.lock().await;
        assert_eq!(bodies.len(), 1, "exactly one search dispatched");
        let dispatched = &bodies[0];
        assert_eq!(dispatched["stage"], "lexical");
        assert_eq!(dispatched["expand_graph"], false);
        assert_eq!(dispatched["text"], "apply_archive_downrank");
        assert_eq!(dispatched["top_k"], 5);
        let paths = paths.lock().await;
        assert_eq!(paths[0], "/indexes/demo/search");
    }

    /// `search_semantic` pins `stage=semantic` and `expand_graph=false`.
    /// Requires Stage 2 (`vector`) capability; happy path verifies the
    /// pre-flight status check sees the ready vector lane.
    #[tokio::test]
    async fn search_semantic_tool_routes_to_semantic_stage_when_stage_2_ready() {
        let status = serde_json::json!({
            "stages": {
                "lexical":  { "status": "ready" },
                "semantic": { "status": "ready" },
                "graph":    { "status": "pending" },
            },
            "search_capabilities": ["bm25", "literal", "exact_match", "vector"],
        });
        let search = serde_json::json!({
            "results": [],
            "intent": "Conceptual",
            "latency_ms": 7,
        });
        let (base, bodies, _paths) = spawn_mock_daemon(status, search).await;
        let server = McpServer::new(base);
        let resp = server
            .dispatch(req(
                "search_semantic",
                serde_json::json!({
                    "index_id": "demo",
                    "query": "code that handles JWT verification",
                }),
            ))
            .await;
        assert!(resp.error.is_none());

        let bodies = bodies.lock().await;
        let dispatched = &bodies[0];
        assert_eq!(dispatched["stage"], "semantic");
        assert_eq!(dispatched["expand_graph"], false);
    }

    /// `search_semantic` returns a STAGE_NOT_READY structured error when
    /// the index lacks the `vector` capability. The error includes the
    /// full stages snapshot and a `suggested_tools` retry hint.
    #[tokio::test]
    async fn search_semantic_tool_returns_stage_not_ready_when_stage_2_missing() {
        let status = serde_json::json!({
            "stages": {
                "lexical":  { "status": "ready" },
                "semantic": { "status": "in_progress" },
                "graph":    { "status": "pending" },
            },
            "search_capabilities": ["bm25", "literal", "exact_match"],
        });
        let search = serde_json::json!({ "results": [] });
        let (base, bodies, _) = spawn_mock_daemon(status, search).await;
        let server = McpServer::new(base);

        // Bare-method form returns a JSON-RPC error with code STAGE_NOT_READY_CODE.
        let resp = server
            .dispatch(req(
                "search_semantic",
                serde_json::json!({
                    "index_id": "demo",
                    "query": "anything",
                }),
            ))
            .await;
        let err = resp.error.expect("expected JSON-RPC error");
        assert_eq!(err.code, STAGE_NOT_READY_CODE);
        assert!(err.message.contains("Stage 2"), "{}", err.message);
        assert!(err.message.contains("embeddings"), "{}", err.message);
        let data = err.data.expect("data field");
        assert_eq!(data["error_code"], "STAGE_NOT_READY");
        let suggested = data["suggested_tools"]
            .as_array()
            .expect("suggested_tools array");
        assert!(suggested
            .iter()
            .any(|v| v.as_str() == Some("search_lexical")));
        assert_eq!(data["current_stages"]["semantic"]["status"], "in_progress");

        // No daemon search call must have happened — the pre-check short-circuited.
        assert!(bodies.lock().await.is_empty());

        // `tools/call` form returns the same condition as
        // `{ isError: true, _meta: { error_code: ... } }`.
        let resp = server
            .dispatch(req(
                "tools/call",
                serde_json::json!({
                    "name": "search_semantic",
                    "arguments": { "index_id": "demo", "query": "x" }
                }),
            ))
            .await;
        let result = resp.result.expect("tools/call returns result envelope");
        assert_eq!(result["isError"], true);
        assert_eq!(result["_meta"]["error_code"], "STAGE_NOT_READY");
        let suggested = result["_meta"]["suggested_tools"]
            .as_array()
            .expect("suggested array");
        assert!(suggested
            .iter()
            .any(|v| v.as_str() == Some("search_lexical")));
    }

    /// `search_kg` pins `stage=graph`, `expand_graph=true`, and pre-checks
    /// the `kg` capability.
    #[tokio::test]
    async fn search_kg_tool_routes_to_graph_stage_when_stage_3_ready() {
        let status = serde_json::json!({
            "stages": {
                "lexical":  { "status": "ready" },
                "semantic": { "status": "ready" },
                "graph":    { "status": "ready" },
            },
            "search_capabilities": ["bm25", "literal", "exact_match", "vector", "kg"],
        });
        let search = serde_json::json!({
            "results": [],
            "intent": "Usage",
            "latency_ms": 12,
        });
        let (base, bodies, _) = spawn_mock_daemon(status, search).await;
        let server = McpServer::new(base);
        let resp = server
            .dispatch(req(
                "search_kg",
                serde_json::json!({
                    "index_id": "demo",
                    "query": "validate_token",
                }),
            ))
            .await;
        assert!(resp.error.is_none());

        let bodies = bodies.lock().await;
        let dispatched = &bodies[0];
        assert_eq!(dispatched["stage"], "graph");
        assert_eq!(dispatched["expand_graph"], true);
    }

    /// `search_kg` returns STAGE_NOT_READY when the index lacks the `kg`
    /// capability, with appropriate fallback hints.
    #[tokio::test]
    async fn search_kg_tool_returns_stage_not_ready_when_stage_3_missing() {
        let status = serde_json::json!({
            "stages": {
                "lexical":  { "status": "ready" },
                "semantic": { "status": "ready" },
                "graph":    { "status": "in_progress" },
            },
            "search_capabilities": ["bm25", "literal", "exact_match", "vector"],
        });
        let search = serde_json::json!({ "results": [] });
        let (base, bodies, _) = spawn_mock_daemon(status, search).await;
        let server = McpServer::new(base);
        let resp = server
            .dispatch(req(
                "search_kg",
                serde_json::json!({
                    "index_id": "demo",
                    "query": "Authenticator",
                }),
            ))
            .await;
        let err = resp.error.expect("expected JSON-RPC error");
        assert_eq!(err.code, STAGE_NOT_READY_CODE);
        assert!(err.message.contains("Stage 3"), "{}", err.message);
        assert!(err.message.contains("symbol graph"), "{}", err.message);
        let data = err.data.expect("data");
        // Semantic IS ready, so the fallback should suggest search_semantic
        // ahead of search_lexical.
        let suggested = data["suggested_tools"].as_array().expect("suggested_tools");
        assert_eq!(
            suggested[0].as_str(),
            Some("search_semantic"),
            "stage 3 missing with stage 2 ready should suggest search_semantic first"
        );
        // No search was dispatched.
        assert!(bodies.lock().await.is_empty());
    }

    /// `search_all` with `index_id` runs the per-index full hybrid: no
    /// stage pin, `expand_graph: true`. Mirrors the ticket's #138 spec.
    #[tokio::test]
    async fn search_all_with_index_id_routes_to_full_hybrid() {
        let status = serde_json::json!({
            "stages": {
                "lexical":  { "status": "ready" },
                "semantic": { "status": "ready" },
                "graph":    { "status": "ready" },
            },
            "search_capabilities": ["bm25", "literal", "exact_match", "vector", "kg"],
        });
        let search = serde_json::json!({
            "results": [],
            "intent": "Conceptual",
            "latency_ms": 8,
        });
        let (base, bodies, paths) = spawn_mock_daemon(status, search).await;
        let server = McpServer::new(base);
        let resp = server
            .dispatch(req(
                "search_all",
                serde_json::json!({
                    "index_id": "demo",
                    "query": "AuthValidator that handles refresh tokens",
                }),
            ))
            .await;
        assert!(resp.error.is_none());

        let bodies = bodies.lock().await;
        let dispatched = &bodies[0];
        // No stage pin (full hybrid adaptive).
        assert!(
            dispatched.get("stage").is_none() || dispatched["stage"].is_null(),
            "search_all must not pin a stage: got {dispatched:?}"
        );
        assert_eq!(dispatched["expand_graph"], true);
        let paths = paths.lock().await;
        assert_eq!(paths[0], "/indexes/demo/search");
    }

    /// `search_all` and the legacy `search` tool produce identical
    /// dispatched SearchQuery shapes — `search` stays as a back-compat
    /// alias per the ticket's spec.
    #[tokio::test]
    async fn search_all_and_legacy_search_dispatch_equivalent_bodies() {
        let status = serde_json::json!({
            "stages": {
                "lexical":  { "status": "ready" },
                "semantic": { "status": "ready" },
                "graph":    { "status": "ready" },
            },
            "search_capabilities": ["bm25", "vector", "kg"],
        });
        let search = serde_json::json!({ "results": [] });
        let (base, bodies, _) = spawn_mock_daemon(status, search).await;
        let server = McpServer::new(base);
        let args = serde_json::json!({
            "index_id": "demo",
            "query": "find the AuthValidator",
            "top_k": 7,
        });
        let _ = server.dispatch(req("search_all", args.clone())).await;
        let _ = server.dispatch(req("search", args.clone())).await;

        let bodies = bodies.lock().await;
        assert_eq!(bodies.len(), 2, "both tools must dispatch a search");
        // Compare text / top_k / expand_graph. `search_all` explicitly
        // sets `expand_graph=true`; the legacy `search` tool does NOT set
        // expand_graph in its body (the daemon defaults to true). Both
        // shapes resolve to identical SearchQuery semantics at the daemon.
        assert_eq!(bodies[0]["text"], bodies[1]["text"]);
        assert_eq!(bodies[0]["top_k"], bodies[1]["top_k"]);
        // Daemon-side: SearchQuery::default sets expand_graph=true, so
        // omitting the field is semantically equivalent to setting true.
        let expand_a = bodies[0]
            .get("expand_graph")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let expand_b = bodies[1]
            .get("expand_graph")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        assert!(expand_a && expand_b, "both must expand the graph");
        // Neither pins a stage.
        assert!(bodies[0].get("stage").is_none_or(|v| v.is_null()));
        assert!(bodies[1].get("stage").is_none_or(|v| v.is_null()));
    }

    /// Missing-arg fast-fail: every per-lane tool rejects an empty arg
    /// object before any HTTP round-trip.
    #[tokio::test]
    async fn per_lane_tools_require_index_id_and_query() {
        let server = McpServer::new("http://127.0.0.1:1");
        for tool in ["search_lexical", "search_semantic", "search_kg"] {
            let resp = server.dispatch(req(tool, serde_json::json!({}))).await;
            let err = resp.error.expect("expected error");
            assert_eq!(
                err.code,
                error_codes::INVALID_PARAMS,
                "{tool} must reject empty args"
            );
        }
    }

    /// `search_all` without `index_id` keeps the legacy fan-out behaviour
    /// (issue #10) — the tool's input schema requires `query` only, and
    /// the daemon's `POST /search` endpoint is responsible for the fan-out
    /// logic. The pre-#138 missing-query test already pins the validation
    /// error; this test just guards that adding `index_id` does NOT
    /// activate fan-out mode.
    #[tokio::test]
    async fn search_all_without_index_id_calls_global_fanout_endpoint() {
        // Mock daemon that returns a fan-out response from POST /search.
        use axum::routing::post;
        use axum::{Json, Router};
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);

        async fn fanout_handler(
            State(captured): State<Arc<Mutex<Vec<String>>>>,
            Json(_body): Json<Value>,
        ) -> Json<Value> {
            captured.lock().await.push("/search".into());
            Json(serde_json::json!({ "results": [] }))
        }
        use axum::extract::State;

        let app = Router::new()
            .route("/search", post(fanout_handler))
            .with_state(captured_clone);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let server = McpServer::new(format!("http://{addr}"));
        let resp = server
            .dispatch(req(
                "search_all",
                serde_json::json!({ "query": "anything" }),
            ))
            .await;
        assert!(resp.error.is_none());
        assert_eq!(captured.lock().await.as_slice(), &["/search".to_string()]);
    }
}
