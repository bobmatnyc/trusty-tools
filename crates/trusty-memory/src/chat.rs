//! Chat HTTP surface for trusty-memory: OpenRouter/Ollama SSE chat, tool
//! dispatch, chat-session CRUD, and inter-project messaging endpoints.
//!
//! Why: Extracted from `web.rs` to keep the HTTP router thin and isolate the
//! tool-calling loop (which is by far the largest single concern in this
//! crate's HTTP surface) behind its own module. The router still owns wiring,
//! but the chat-specific request/response handlers, the tool dispatcher, and
//! the inter-project messaging handlers all live here.
//! What: Re-exports `chat_handler`, provider/session handlers, the
//! `execute_*` dispatcher set, and the `/api/v1/messages*` handlers. Items
//! kept `pub(crate)` so `web::router()` and the cross-palace recall handler
//! can reference them without enlarging the public crate surface.
//! Test: Behaviour is covered by `web::tests::all_tools_returns_expected_set`
//! and `web::tests::execute_tool_dispatches_known_tools`, which still call
//! into this module via the `pub(crate)` re-exports.

use crate::web::{
    creator_info_from_http, load_user_config, open_handle, palace_info_from, ApiError,
    DreamStatusPayload,
};
use crate::{ActivitySource, AppState, DaemonEvent};
use axum::{
    body::Body,
    extract::{Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use trusty_common::memory_core::dream::PersistedDreamStats;
use trusty_common::memory_core::palace::{PalaceId, RoomType};
use trusty_common::memory_core::retrieval::{
    recall_across_palaces_with_default_embedder, recall_with_default_embedder,
};
use trusty_common::memory_core::store::kg::Triple;
use trusty_common::memory_core::PalaceRegistry;
use trusty_common::{ChatEvent, ChatMessage, ToolDef};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Chat (OpenRouter, SSE-streaming)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct ChatBody {
    #[serde(default)]
    palace_id: Option<String>,
    message: String,
    #[serde(default)]
    history: Vec<ChatMessage>,
    /// Optional existing chat-session id; when provided we load+append+save.
    #[serde(default)]
    session_id: Option<String>,
}

/// Hard cap on the number of `tool -> assistant` round trips per chat turn.
///
/// Why: Without a bound, a malicious or confused model could request tools
/// indefinitely; 10 is generous enough for any realistic plan-and-act loop
/// while still terminating quickly when the model gets stuck.
const MAX_TOOL_ROUNDS: usize = 10;

/// Build the complete set of tool definitions the chat assistant can call.
///
/// Why: Centralizing the tool surface keeps the wire schema, the dispatcher in
/// `execute_tool`, and the system prompt in lock-step — adding a new tool means
/// editing this one function plus a match arm.
/// What: Returns the 11 read/write tools spanning palace introspection,
/// memory recall/create, KG read/write, and daemon status.
/// Test: `all_tools_returns_expected_set` asserts names and required-arg shape.
pub(crate) fn all_tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "list_palaces".into(),
            description: "List all memory palaces on this machine with their metadata (id, name, description, counts).".into(),
            parameters: json!({ "type": "object", "properties": {}, "required": [] }),
        },
        ToolDef {
            name: "get_palace".into(),
            description: "Get details for a specific palace by id.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "palace_id": { "type": "string", "description": "Palace id (kebab-case)" } },
                "required": ["palace_id"],
            }),
        },
        ToolDef {
            name: "recall_memories".into(),
            description: "Semantic search for memories in a palace. Returns the top-k most relevant drawers ranked by similarity to the query.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "palace_id": { "type": "string" },
                    "query": { "type": "string", "description": "Free-text query" },
                    "top_k": { "type": "integer", "minimum": 1, "maximum": 50, "default": 5 }
                },
                "required": ["palace_id", "query"],
            }),
        },
        ToolDef {
            name: "list_drawers".into(),
            description: "List all drawers (memories) in a palace, most recent first.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "palace_id": { "type": "string" } },
                "required": ["palace_id"],
            }),
        },
        ToolDef {
            name: "kg_query".into(),
            description: "Query the temporal knowledge graph for all currently-active triples whose subject matches.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "palace_id": { "type": "string" },
                    "subject": { "type": "string" }
                },
                "required": ["palace_id", "subject"],
            }),
        },
        ToolDef {
            name: "get_config".into(),
            description: "Get the trusty-memory daemon's configuration (provider, model, data root). API keys are masked.".into(),
            parameters: json!({ "type": "object", "properties": {}, "required": [] }),
        },
        ToolDef {
            name: "get_status".into(),
            description: "Get daemon health: version, palace count, totals for drawers/vectors/triples.".into(),
            parameters: json!({ "type": "object", "properties": {}, "required": [] }),
        },
        ToolDef {
            name: "get_dream_status".into(),
            description: "Get aggregated dreamer activity across all palaces (merged/pruned/compacted counts, last run timestamp).".into(),
            parameters: json!({ "type": "object", "properties": {}, "required": [] }),
        },
        ToolDef {
            name: "get_palace_dream_status".into(),
            description: "Get dreamer activity stats for a specific palace.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "palace_id": { "type": "string" } },
                "required": ["palace_id"],
            }),
        },
        ToolDef {
            name: "create_memory".into(),
            description: "Store a new memory (drawer) in a palace. The content is embedded and inserted into the vector index plus the drawer table.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "palace_id": { "type": "string" },
                    "content": { "type": "string", "description": "Verbatim memory text" },
                    "room": { "type": "string", "description": "Room name (Frontend/Backend/Testing/Planning/Documentation/Research/Configuration/Meetings/General or a custom name); defaults to General." },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "importance": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.5 }
                },
                "required": ["palace_id", "content"],
            }),
        },
        ToolDef {
            name: "kg_assert".into(),
            description: "Assert a knowledge-graph triple. Any prior active triple with the same (subject, predicate) is closed out (valid_to set to now) before the new one is inserted.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "palace_id": { "type": "string" },
                    "subject": { "type": "string" },
                    "predicate": { "type": "string" },
                    "object": { "type": "string" },
                    "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 1.0 }
                },
                "required": ["palace_id", "subject", "predicate", "object"],
            }),
        },
        ToolDef {
            name: "memory_recall_all".into(),
            description: "Semantic search across ALL palaces simultaneously. Returns the top-k most relevant drawers ranked by similarity, regardless of which palace they belong to. Each result includes a `palace_id` field identifying its source.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "q": { "type": "string", "description": "Free-text query" },
                    "top_k": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 },
                    "deep": { "type": "boolean", "default": false }
                },
                "required": ["q"],
            }),
        },
    ]
}

/// Execute a tool call against the live `AppState`.
///
/// Why: We want the model's tool invocations to call the same Rust paths the
/// HTTP handlers use — no extra HTTP round-trip, no JSON re-parsing, and the
/// results always reflect this daemon's view of the world.
/// What: Parses `arguments` as JSON, dispatches by tool name, returns a JSON
/// value that becomes the `role: "tool"` message content. Errors are caught
/// and returned as `{"error": "..."}` JSON so the model can react.
/// Test: `execute_tool_dispatches_known_tools` covers the dispatch path and
/// the unknown-tool error case.
pub(crate) async fn execute_tool(name: &str, args: &str, state: &AppState) -> Value {
    let parsed: Value = serde_json::from_str(args).unwrap_or(json!({}));
    match name {
        "list_palaces" => execute_list_palaces(state).await,
        "get_palace" => match parsed.get("palace_id").and_then(|v| v.as_str()) {
            Some(id) => execute_get_palace(state, id).await,
            None => json!({ "error": "missing required argument: palace_id" }),
        },
        "recall_memories" => {
            let pid = parsed.get("palace_id").and_then(|v| v.as_str());
            let q = parsed.get("query").and_then(|v| v.as_str());
            let top_k = parsed.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            match (pid, q) {
                (Some(p), Some(q)) => execute_recall(state, p, q, top_k).await,
                _ => json!({ "error": "missing required argument(s): palace_id, query" }),
            }
        }
        "list_drawers" => match parsed.get("palace_id").and_then(|v| v.as_str()) {
            Some(id) => execute_list_drawers(state, id).await,
            None => json!({ "error": "missing required argument: palace_id" }),
        },
        "kg_query" => {
            let pid = parsed.get("palace_id").and_then(|v| v.as_str());
            let subj = parsed.get("subject").and_then(|v| v.as_str());
            match (pid, subj) {
                (Some(p), Some(s)) => execute_kg_query(state, p, s).await,
                _ => json!({ "error": "missing required argument(s): palace_id, subject" }),
            }
        }
        "get_config" => execute_get_config(state),
        "get_status" => execute_get_status(state).await,
        "get_dream_status" => execute_get_dream_status(state).await,
        "get_palace_dream_status" => match parsed.get("palace_id").and_then(|v| v.as_str()) {
            Some(id) => execute_get_palace_dream_status(state, id).await,
            None => json!({ "error": "missing required argument: palace_id" }),
        },
        "create_memory" => {
            let pid = parsed.get("palace_id").and_then(|v| v.as_str());
            let content = parsed.get("content").and_then(|v| v.as_str());
            let room = parsed.get("room").and_then(|v| v.as_str());
            let tags: Vec<String> = parsed
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|t| t.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let importance = parsed
                .get("importance")
                .and_then(|v| v.as_f64())
                .map(|f| f as f32)
                .unwrap_or(0.5);
            match (pid, content) {
                (Some(p), Some(c)) => {
                    execute_create_memory(state, p, c, room, tags, importance).await
                }
                _ => json!({ "error": "missing required argument(s): palace_id, content" }),
            }
        }
        "kg_assert" => {
            let pid = parsed.get("palace_id").and_then(|v| v.as_str());
            let subj = parsed.get("subject").and_then(|v| v.as_str());
            let pred = parsed.get("predicate").and_then(|v| v.as_str());
            let obj = parsed.get("object").and_then(|v| v.as_str());
            let conf = parsed
                .get("confidence")
                .and_then(|v| v.as_f64())
                .map(|f| f as f32)
                .unwrap_or(1.0);
            match (pid, subj, pred, obj) {
                (Some(p), Some(s), Some(pr), Some(o)) => {
                    execute_kg_assert(state, p, s, pr, o, conf).await
                }
                _ => json!({
                    "error": "missing required argument(s): palace_id, subject, predicate, object"
                }),
            }
        }
        "memory_recall_all" => {
            let q = parsed.get("q").and_then(|v| v.as_str());
            let top_k = parsed.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let deep = parsed
                .get("deep")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            match q {
                Some(q) => execute_recall_all(state, q, top_k, deep).await,
                None => json!({ "error": "missing required argument: q" }),
            }
        }
        _ => json!({ "error": format!("unknown tool: {name}") }),
    }
}

async fn execute_list_palaces(state: &AppState) -> Value {
    let palaces = match PalaceRegistry::list_palaces(&state.data_root) {
        Ok(v) => v,
        Err(e) => return json!({ "error": format!("list palaces: {e:#}") }),
    };
    let out: Vec<Value> = palaces
        .into_iter()
        .map(|p| {
            let handle = state.registry.open_palace(&state.data_root, &p.id).ok();
            let info = palace_info_from(&p, handle.as_ref());
            serde_json::to_value(info).unwrap_or(json!({}))
        })
        .collect();
    json!(out)
}

async fn execute_get_palace(state: &AppState, id: &str) -> Value {
    let palaces = match PalaceRegistry::list_palaces(&state.data_root) {
        Ok(v) => v,
        Err(e) => return json!({ "error": format!("list palaces: {e:#}") }),
    };
    match palaces.into_iter().find(|p| p.id.0 == id) {
        Some(p) => {
            let handle = state.registry.open_palace(&state.data_root, &p.id).ok();
            serde_json::to_value(palace_info_from(&p, handle.as_ref())).unwrap_or(json!({}))
        }
        None => json!({ "error": format!("palace not found: {id}") }),
    }
}

async fn execute_recall(state: &AppState, palace_id: &str, query: &str, top_k: usize) -> Value {
    let handle = match state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(palace_id))
    {
        Ok(h) => h,
        Err(e) => return json!({ "error": format!("open palace {palace_id}: {e:#}") }),
    };
    match recall_with_default_embedder(&handle, query, top_k).await {
        Ok(hits) => json!(hits
            .into_iter()
            .map(|r| json!({
                "drawer_id": r.drawer.id.to_string(),
                "content": r.drawer.content,
                "importance": r.drawer.importance,
                "tags": r.drawer.tags,
                "score": r.score,
                "layer": r.layer,
            }))
            .collect::<Vec<_>>()),
        Err(e) => json!({ "error": format!("recall: {e:#}") }),
    }
}

/// Execute a cross-palace recall and return JSON results tagged with palace id.
///
/// Why: Both the MCP `memory_recall_all` tool and the `GET /api/v1/recall`
/// HTTP route share the same wiring — list palaces, open handles, fan out via
/// `recall_across_palaces_with_default_embedder`, and serialize.
/// What: Lists every palace on disk, opens each (skipping any that fail with
/// a `tracing::warn!`), and delegates to the core fan-out. On success returns
/// a JSON array; on listing failure returns `{ "error": "..." }`.
/// Test: Indirectly via `recall_across_palaces_merges_results` (core merge
/// logic) and the HTTP/MCP integration paths.
pub(crate) async fn execute_recall_all(
    state: &AppState,
    query: &str,
    top_k: usize,
    deep: bool,
) -> Value {
    let palaces = match PalaceRegistry::list_palaces(&state.data_root) {
        Ok(v) => v,
        Err(e) => return json!({ "error": format!("list palaces: {e:#}") }),
    };
    let mut handles = Vec::with_capacity(palaces.len());
    for p in &palaces {
        match state.registry.open_palace(&state.data_root, &p.id) {
            Ok(h) => handles.push(h),
            Err(e) => {
                tracing::warn!(palace = %p.id, "execute_recall_all: open failed: {e:#}");
            }
        }
    }
    if handles.is_empty() {
        return json!([]);
    }
    match recall_across_palaces_with_default_embedder(&handles, query, top_k, deep).await {
        Ok(results) => json!(results
            .into_iter()
            .map(|r| json!({
                "palace_id": r.palace_id,
                "drawer_id": r.result.drawer.id.to_string(),
                "content": r.result.drawer.content,
                "importance": r.result.drawer.importance,
                "tags": r.result.drawer.tags,
                "score": r.result.score,
                "layer": r.result.layer,
            }))
            .collect::<Vec<_>>()),
        Err(e) => json!({ "error": format!("recall_across_palaces: {e:#}") }),
    }
}

async fn execute_list_drawers(state: &AppState, palace_id: &str) -> Value {
    let handle = match state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(palace_id))
    {
        Ok(h) => h,
        Err(e) => return json!({ "error": format!("open palace {palace_id}: {e:#}") }),
    };
    let drawers = handle.list_drawers(None, None, 200);
    serde_json::to_value(drawers).unwrap_or(json!([]))
}

async fn execute_kg_query(state: &AppState, palace_id: &str, subject: &str) -> Value {
    let handle = match state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(palace_id))
    {
        Ok(h) => h,
        Err(e) => return json!({ "error": format!("open palace {palace_id}: {e:#}") }),
    };
    match handle.kg.query_active(subject).await {
        Ok(triples) => serde_json::to_value(triples).unwrap_or(json!([])),
        Err(e) => json!({ "error": format!("kg query: {e:#}") }),
    }
}

fn execute_get_config(state: &AppState) -> Value {
    let cfg = load_user_config().unwrap_or_default();
    json!({
        "openrouter_configured": !cfg.openrouter_api_key.is_empty(),
        "openrouter_model": cfg.openrouter_model,
        "local_model": {
            "enabled": cfg.local_model.enabled,
            "base_url": cfg.local_model.base_url,
            "model": cfg.local_model.model,
        },
        "data_root": state.data_root.display().to_string(),
    })
}

async fn execute_get_status(state: &AppState) -> Value {
    let palaces = PalaceRegistry::list_palaces(&state.data_root).unwrap_or_default();
    let (mut total_drawers, mut total_vectors, mut total_kg_triples) = (0usize, 0usize, 0usize);
    for p in &palaces {
        if let Ok(handle) = state.registry.open_palace(&state.data_root, &p.id) {
            total_drawers = total_drawers.saturating_add(handle.drawers.read().len());
            total_vectors = total_vectors.saturating_add(handle.vector_store.index_size());
            total_kg_triples = total_kg_triples.saturating_add(handle.kg.count_active_triples());
        }
    }
    json!({
        "version": state.version,
        "palace_count": palaces.len(),
        "default_palace": state.default_palace,
        "data_root": state.data_root.display().to_string(),
        "total_drawers": total_drawers,
        "total_vectors": total_vectors,
        "total_kg_triples": total_kg_triples,
    })
}

async fn execute_get_dream_status(state: &AppState) -> Value {
    let palaces = PalaceRegistry::list_palaces(&state.data_root).unwrap_or_default();
    let mut out = DreamStatusPayload::default();
    let mut latest: Option<chrono::DateTime<chrono::Utc>> = None;
    for p in palaces {
        let data_dir = state.data_root.join(p.id.as_str());
        let snap = match PersistedDreamStats::load(&data_dir) {
            Ok(Some(s)) => s,
            _ => continue,
        };
        out.merged = out.merged.saturating_add(snap.stats.merged);
        out.pruned = out.pruned.saturating_add(snap.stats.pruned);
        out.compacted = out.compacted.saturating_add(snap.stats.compacted);
        out.closets_updated = out
            .closets_updated
            .saturating_add(snap.stats.closets_updated);
        out.duration_ms = out.duration_ms.saturating_add(snap.stats.duration_ms);
        latest = match latest {
            Some(t) if t >= snap.last_run_at => Some(t),
            _ => Some(snap.last_run_at),
        };
    }
    out.last_run_at = latest;
    serde_json::to_value(out).unwrap_or(json!({}))
}

async fn execute_get_palace_dream_status(state: &AppState, palace_id: &str) -> Value {
    let data_dir = state.data_root.join(palace_id);
    if !data_dir.exists() {
        return json!({ "error": format!("palace not found: {palace_id}") });
    }
    match PersistedDreamStats::load(&data_dir) {
        Ok(Some(s)) => serde_json::to_value(DreamStatusPayload::from(s)).unwrap_or(json!({})),
        Ok(None) => serde_json::to_value(DreamStatusPayload::default()).unwrap_or(json!({})),
        Err(e) => json!({ "error": format!("read dream stats: {e:#}") }),
    }
}

async fn execute_create_memory(
    state: &AppState,
    palace_id: &str,
    content: &str,
    room: Option<&str>,
    tags: Vec<String>,
    importance: f32,
) -> Value {
    let handle = match state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(palace_id))
    {
        Ok(h) => h,
        Err(e) => return json!({ "error": format!("open palace {palace_id}: {e:#}") }),
    };
    let room = room.map(RoomType::parse).unwrap_or(RoomType::General);
    match handle
        .remember(content.to_string(), room, tags, importance)
        .await
    {
        Ok(id) => json!({ "drawer_id": id.to_string(), "status": "stored" }),
        Err(e) => json!({ "error": format!("remember: {e:#}") }),
    }
}

async fn execute_kg_assert(
    state: &AppState,
    palace_id: &str,
    subject: &str,
    predicate: &str,
    object: &str,
    confidence: f32,
) -> Value {
    let handle = match state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(palace_id))
    {
        Ok(h) => h,
        Err(e) => return json!({ "error": format!("open palace {palace_id}: {e:#}") }),
    };
    let triple = Triple {
        subject: subject.to_string(),
        predicate: predicate.to_string(),
        object: object.to_string(),
        valid_from: chrono::Utc::now(),
        valid_to: None,
        confidence,
        provenance: Some("chat:assistant".to_string()),
    };
    match handle.kg.assert(triple).await {
        Ok(()) => json!({ "status": "asserted" }),
        Err(e) => json!({ "error": format!("kg assert: {e:#}") }),
    }
}

pub(crate) async fn chat_handler(
    State(state): State<AppState>,
    Json(body): Json<ChatBody>,
) -> Response {
    // Select the active provider (Ollama auto-detect, else OpenRouter).
    let Some(provider) = state.chat_provider().await else {
        return (
            StatusCode::PRECONDITION_FAILED,
            "No chat provider configured (no local Ollama detected and no OpenRouter key set)",
        )
            .into_response();
    };

    // Resolve palace id (explicit > default).
    let palace_id = body
        .palace_id
        .clone()
        .or_else(|| state.default_palace.clone())
        .unwrap_or_default();

    // Resolve / create chat session when a palace is bound.
    let (session_id, mut history): (Option<String>, Vec<ChatMessage>) = if !palace_id.is_empty() {
        let store = match state.session_store(&palace_id) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(palace = %palace_id, "session_store open failed: {e:#}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("session store: {e:#}"),
                )
                    .into_response();
            }
        };
        match body.session_id.clone() {
            Some(sid) => match store.get_session(&sid) {
                Ok(Some(s)) => (
                    Some(sid),
                    s.history
                        .into_iter()
                        .map(|m| ChatMessage {
                            role: m.role,
                            content: m.content,
                            tool_call_id: None,
                            tool_calls: None,
                        })
                        .collect(),
                ),
                _ => (Some(sid), body.history.clone()),
            },
            None => {
                let new_id = store.create_session(None).unwrap_or_else(|e| {
                    tracing::warn!("create_session failed: {e:#}");
                    String::new()
                });
                (
                    if new_id.is_empty() {
                        None
                    } else {
                        Some(new_id)
                    },
                    body.history.clone(),
                )
            }
        }
    } else {
        (None, body.history.clone())
    };

    // Full palace roster for the identity block — names + ids, not just count,
    // so the model can pick the right one when the user names a palace.
    let all_palaces = PalaceRegistry::list_palaces(&state.data_root).unwrap_or_default();
    let palace_count = all_palaces.len();
    let palace_roster: String = all_palaces
        .iter()
        .map(|p| format!("- {} (id: {})", p.name, p.id.0))
        .collect::<Vec<_>>()
        .join("\n");

    // Config + global dream snapshot — give the model an honest view of what's
    // available so it doesn't invent tools or providers that aren't there.
    let cfg = load_user_config().unwrap_or_default();
    let active_provider_name = state
        .chat_provider()
        .await
        .map(|p| p.name().to_string())
        .unwrap_or_else(|| "none".to_string());
    let dream_snapshot = execute_get_dream_status(&state).await;

    // Look up the selected palace's metadata (name/description) and open its
    // handle for live counts + recall context.
    let selected_palace_meta = if palace_id.is_empty() {
        None
    } else {
        all_palaces.iter().find(|p| p.id.0 == palace_id).cloned()
    };

    let mut palace_block = String::new();
    let mut context = String::new();
    let mut palace_display_name = palace_id.clone();

    if !palace_id.is_empty() {
        if let Ok(handle) = state
            .registry
            .open_palace(&state.data_root, &PalaceId::new(&palace_id))
        {
            // Live counts from the opened handle.
            let drawer_count = handle.drawers.read().len();
            let vector_count = handle.vector_store.index_size();
            let kg_triple_count = handle.kg.count_active_triples();

            // Prefer the on-disk palace.json name/description; fall back to id.
            let (name, description) = match &selected_palace_meta {
                Some(p) => (p.name.clone(), p.description.clone()),
                None => (palace_id.clone(), None),
            };
            palace_display_name = name.clone();

            palace_block.push_str(&format!(
                "Currently selected palace:\n\
                 - id: {id}\n\
                 - name: {name}\n",
                id = palace_id,
                name = name,
            ));
            if let Some(desc) = description.as_deref().filter(|s| !s.is_empty()) {
                palace_block.push_str(&format!("- description: {desc}\n"));
            }
            palace_block.push_str(&format!(
                "- drawers: {drawer_count}\n\
                 - vectors: {vector_count}\n\
                 - kg_triples: {kg_triple_count}\n",
            ));
            let identity_trimmed = handle.identity.trim();
            if !identity_trimmed.is_empty() {
                palace_block.push_str(&format!("- identity:\n{identity_trimmed}\n",));
            }

            if let Ok(hits) = recall_with_default_embedder(&handle, &body.message, 5).await {
                for r in hits.iter().take(5) {
                    context.push_str(&format!("- (L{}) {}\n", r.layer, r.drawer.content));
                }
            }
        }
    }

    // Build the grounded system prompt with identity, palace, RAG, config,
    // dream-snapshot, and behavior blocks so the LLM never confuses
    // trusty-memory palaces with real-world architectural palaces.
    let mut system = String::new();
    system.push_str(&format!(
        "You are the assistant for trusty-memory, a machine-wide AI memory \
         service running locally on this user's machine. trusty-memory stores \
         knowledge in named \"palaces\" — isolated memory namespaces, each with \
         its own vector index (usearch HNSW) and temporal knowledge graph \
         (SQLite). Memories are organized as Palace -> Wing -> Room -> Closet \
         -> Drawer, where a Drawer is an atomic memory unit.\n\
         There are currently {palace_count} palace(s) on this machine.\n",
    ));
    if !palace_roster.is_empty() {
        system.push_str(&format!("Palaces:\n{palace_roster}\n"));
    }
    system.push('\n');

    // Config block — what providers/models are wired up right now.
    system.push_str(&format!(
        "System configuration:\n\
         - active chat provider: {active_provider_name}\n\
         - openrouter model: {or_model}\n\
         - local model: {local_model} ({local_url}, enabled={local_enabled})\n\
         - data root: {data_root}\n\n",
        or_model = cfg.openrouter_model,
        local_model = cfg.local_model.model,
        local_url = cfg.local_model.base_url,
        local_enabled = cfg.local_model.enabled,
        data_root = state.data_root.display(),
    ));

    // Dream snapshot — give the model a sense of how stale memory state is.
    system.push_str(&format!(
        "Global dream status (background memory maintenance):\n{}\n\n",
        dream_snapshot,
    ));

    if !palace_block.is_empty() {
        system.push_str(&palace_block);
        system.push('\n');
    }

    if !context.is_empty() {
        system.push_str(&format!(
            "Relevant memories from the '{palace_display_name}' palace \
             (L0 = identity, L1 = essentials, L2 = topic-filtered, L3 = deep):\n\
             {context}\n",
        ));
    }

    system.push_str(
        "You have a set of tools to introspect and modify this trusty-memory \
         daemon. Prefer calling a tool over guessing — e.g. call \
         `list_palaces` rather than relying on the roster above if you need \
         live counts, and call `recall_memories` to search for facts you \
         don't have in context. When the user asks about \"palaces\", they \
         mean trusty-memory palaces (memory namespaces on this machine), not \
         architectural palaces like Versailles. If a tool returns an error, \
         report it honestly and don't fabricate results.",
    );

    // Append the new user message to the in-memory history we'll persist.
    history.push(ChatMessage {
        role: "user".to_string(),
        content: body.message.clone(),
        tool_call_id: None,
        tool_calls: None,
    });

    let mut messages: Vec<ChatMessage> = Vec::with_capacity(history.len() + 1);
    messages.push(ChatMessage {
        role: "system".to_string(),
        content: system,
        tool_call_id: None,
        tool_calls: None,
    });
    messages.extend(history.iter().cloned());

    let tools = all_tools();
    let (sse_tx, sse_rx) =
        tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(64);

    // Capture session-persistence inputs.
    let session_store = if !palace_id.is_empty() && session_id.is_some() {
        state.session_store(&palace_id).ok()
    } else {
        None
    };
    let persist_session_id = session_id.clone();

    // Drive the tool-execution loop in a background task so the response can
    // start streaming immediately.
    let loop_state = state.clone();
    tokio::spawn(async move {
        // Emit a leading session_id frame so the SPA can correlate this stream
        // with a persisted session row.
        if let Some(sid) = persist_session_id.as_deref() {
            let frame = format!("data: {}\n\n", json!({ "session_id": sid }));
            if sse_tx
                .send(Ok(axum::body::Bytes::from(frame)))
                .await
                .is_err()
            {
                return;
            }
        }

        let mut final_assistant_text = String::new();
        let mut stream_err: Option<String> = None;

        for round in 0..MAX_TOOL_ROUNDS {
            let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<ChatEvent>(256);
            let messages_clone = messages.clone();
            let tools_clone = tools.clone();
            let provider_clone = provider.clone();
            let stream_handle = tokio::spawn(async move {
                provider_clone
                    .chat_stream(messages_clone, tools_clone, event_tx)
                    .await
            });

            let mut tool_calls_this_round: Vec<trusty_common::ToolCall> = Vec::new();
            let mut round_assistant_text = String::new();

            while let Some(event) = event_rx.recv().await {
                match event {
                    ChatEvent::Delta(text) => {
                        round_assistant_text.push_str(&text);
                        let frame = format!("data: {}\n\n", json!({ "delta": text }));
                        if sse_tx
                            .send(Ok(axum::body::Bytes::from(frame)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    ChatEvent::ToolCall(tc) => {
                        let frame = format!(
                            "data: {}\n\n",
                            json!({ "tool_call": {
                                "id": tc.id,
                                "name": tc.name,
                                "arguments": tc.arguments,
                            }})
                        );
                        let _ = sse_tx.send(Ok(axum::body::Bytes::from(frame))).await;
                        tool_calls_this_round.push(tc);
                    }
                    ChatEvent::Done => break,
                    ChatEvent::Error(e) => {
                        stream_err = Some(e);
                        break;
                    }
                }
            }

            // Drain the spawned stream task; surface any error.
            match stream_handle.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => stream_err = Some(e.to_string()),
                Err(e) => stream_err = Some(format!("join: {e}")),
            }

            if stream_err.is_some() {
                break;
            }

            final_assistant_text.push_str(&round_assistant_text);

            if tool_calls_this_round.is_empty() {
                // Model produced a plain answer — we're done.
                break;
            }

            // Build the assistant message that requested these tool calls.
            let assistant_tool_calls_json: Vec<Value> = tool_calls_this_round
                .iter()
                .map(|tc| {
                    json!({
                        "id": tc.id,
                        "type": "function",
                        "function": { "name": tc.name, "arguments": tc.arguments },
                    })
                })
                .collect();
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: round_assistant_text,
                tool_call_id: None,
                tool_calls: Some(assistant_tool_calls_json),
            });

            // Execute each tool and append its result as a `role: "tool"`
            // message. The next loop iteration feeds these back to the model.
            for tc in &tool_calls_this_round {
                let result = execute_tool(&tc.name, &tc.arguments, &loop_state).await;
                let result_str = result.to_string();
                let frame = format!(
                    "data: {}\n\n",
                    json!({ "tool_result": {
                        "id": tc.id,
                        "name": tc.name,
                        "content": &result_str,
                    }})
                );
                let _ = sse_tx.send(Ok(axum::body::Bytes::from(frame))).await;
                messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: result_str,
                    tool_call_id: Some(tc.id.clone()),
                    tool_calls: None,
                });
            }

            // Safety net: log when we walk off the round limit.
            if round + 1 == MAX_TOOL_ROUNDS {
                tracing::warn!(
                    "chat: hit MAX_TOOL_ROUNDS={} — terminating tool loop",
                    MAX_TOOL_ROUNDS
                );
            }
        }

        // Persist the completed conversation regardless of streaming error
        // (partial assistant reply still better than nothing).
        if let (Some(store), Some(sid)) = (session_store, persist_session_id.as_deref()) {
            if !final_assistant_text.is_empty() {
                history.push(ChatMessage {
                    role: "assistant".into(),
                    content: final_assistant_text,
                    tool_call_id: None,
                    tool_calls: None,
                });
            }
            let core_history: Vec<trusty_common::memory_core::store::chat_sessions::ChatMessage> =
                history
                    .iter()
                    .map(
                        |m| trusty_common::memory_core::store::chat_sessions::ChatMessage {
                            role: m.role.clone(),
                            content: m.content.clone(),
                        },
                    )
                    .collect();
            if let Err(e) = store.upsert_session(sid, &core_history) {
                tracing::warn!("upsert_session failed: {e:#}");
            }
        }

        match stream_err {
            None => {
                let _ = sse_tx
                    .send(Ok(axum::body::Bytes::from("data: [DONE]\n\n")))
                    .await;
            }
            Some(e) => {
                let out = format!("data: {}\n\n", json!({ "error": e }));
                let _ = sse_tx.send(Ok(axum::body::Bytes::from(out))).await;
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(sse_rx);

    Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .body(Body::from_stream(stream))
        .expect("static SSE response builds")
}

// ---------------------------------------------------------------------------
// Providers + sessions
// ---------------------------------------------------------------------------

/// GET /api/v1/chat/providers — report provider availability + active choice.
///
/// Why: The UI's chat panel surfaces whether the user has a local model
/// running or is hitting OpenRouter. Probing both upstreams here keeps that
/// logic on the server so the SPA stays dumb.
/// What: Calls `auto_detect_local_provider` (1s timeout) for Ollama and checks
/// for a non-empty OpenRouter key. Returns shape `{providers:[...], active}`.
/// Test: `providers_endpoint_returns_payload`.
pub(crate) async fn list_providers(State(state): State<AppState>) -> Json<Value> {
    let cfg = load_user_config().unwrap_or_default();
    let ollama_available = if cfg.local_model.enabled {
        trusty_common::auto_detect_local_provider(&cfg.local_model.base_url)
            .await
            .is_some()
    } else {
        false
    };
    let openrouter_available = !cfg.openrouter_api_key.is_empty();
    let active = state.chat_provider().await.map(|p| p.name().to_string());
    Json(json!({
        "providers": [
            {
                "name": "ollama",
                "model": cfg.local_model.model,
                "available": ollama_available,
            },
            {
                "name": "openrouter",
                "model": cfg.openrouter_model,
                "available": openrouter_available,
            }
        ],
        "active": active,
    }))
}

#[derive(Deserialize, Default)]
pub(crate) struct CreateSessionBody {
    #[serde(default)]
    title: Option<String>,
}

pub(crate) async fn create_chat_session(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    body: Option<Json<CreateSessionBody>>,
) -> Result<Json<Value>, ApiError> {
    let store = state
        .session_store(&id)
        .map_err(|e| ApiError::internal(format!("session store: {e:#}")))?;
    let title = body.and_then(|b| b.0.title);
    let sid = store
        .create_session(title)
        .map_err(|e| ApiError::internal(format!("create session: {e:#}")))?;
    Ok(Json(json!({ "id": sid })))
}

pub(crate) async fn list_chat_sessions(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    let store = state
        .session_store(&id)
        .map_err(|e| ApiError::internal(format!("session store: {e:#}")))?;
    let metas = store
        .list_sessions()
        .map_err(|e| ApiError::internal(format!("list sessions: {e:#}")))?;
    Ok(Json(serde_json::to_value(metas).unwrap_or(json!([]))))
}

pub(crate) async fn get_chat_session(
    State(state): State<AppState>,
    AxumPath((id, session_id)): AxumPath<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let store = state
        .session_store(&id)
        .map_err(|e| ApiError::internal(format!("session store: {e:#}")))?;
    let s = store
        .get_session(&session_id)
        .map_err(|e| ApiError::internal(format!("get session: {e:#}")))?
        .ok_or_else(|| ApiError::not_found(format!("session not found: {session_id}")))?;
    Ok(Json(serde_json::to_value(s).unwrap_or(json!({}))))
}

pub(crate) async fn delete_chat_session(
    State(state): State<AppState>,
    AxumPath((id, session_id)): AxumPath<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let store = state
        .session_store(&id)
        .map_err(|e| ApiError::internal(format!("session store: {e:#}")))?;
    store
        .delete_session(&session_id)
        .map_err(|e| ApiError::internal(format!("delete session: {e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Inter-project messaging (issue #99)
// ---------------------------------------------------------------------------

/// Query parameters for `GET /api/v1/messages`.
///
/// Why: the receiver's SessionStart hook calls `unread_only=true` to fetch
/// pending mail; the UI's audit view calls `unread_only=false` to render
/// the full history.
/// What: `palace` is the recipient slug; `unread_only` defaults to `false`.
/// Test: `messages_endpoint_round_trip`.
#[derive(Deserialize)]
pub(crate) struct ListMessagesQuery {
    palace: String,
    #[serde(default)]
    unread_only: Option<bool>,
}

/// `GET /api/v1/messages?palace=<id>&unread_only=<bool>` — list messages in
/// a palace, optionally filtering to unread.
///
/// Why: serves the same data the MCP `inbox-check` CLI consumes, plus the UI
/// audit log. Returns a JSON array of `{id, from_palace, to_palace, purpose,
/// sent_at, read, content, formatted}` objects; `formatted` is the
/// pre-rendered Markdown block the SessionStart hook emits to stdout.
/// What: opens the palace, calls
/// [`crate::messaging::list_messages`], and renders each message envelope
/// plus its formatted block to JSON.
/// Test: `messages_endpoint_round_trip`.
pub(crate) async fn list_messages_handler(
    State(state): State<AppState>,
    Query(q): Query<ListMessagesQuery>,
) -> Result<Json<Value>, ApiError> {
    let handle = open_handle(&state, &q.palace)?;
    let unread_only = q.unread_only.unwrap_or(false);
    let messages = crate::messaging::list_messages(&handle, unread_only);
    let payload: Vec<Value> = messages
        .into_iter()
        .map(|m| {
            let formatted = m.to_injection_block();
            json!({
                "id":          m.id.to_string(),
                "from_palace": m.from_palace,
                "to_palace":   m.to_palace,
                "purpose":     m.purpose,
                "sent_at":     m.sent_at.to_rfc3339(),
                "read":        m.read,
                "content":     m.content,
                "formatted":   formatted,
            })
        })
        .collect();
    Ok(Json(json!(payload)))
}

/// Request body for `POST /api/v1/messages`.
///
/// Why: the send path takes the same four fields whether invoked from MCP,
/// CLI, or HTTP; sharing the JSON shape keeps callers interchangeable.
/// What: `to_palace`, `purpose`, `content` are required; `from_palace`
/// defaults to the server's `--palace` default if set, otherwise to
/// `<unknown>` (sender SHOULD set it explicitly; the CLI does cwd
/// derivation client-side so the daemon stays project-agnostic).
/// Test: `messages_endpoint_round_trip`.
#[derive(Deserialize)]
pub(crate) struct SendMessageBody {
    to_palace: String,
    purpose: String,
    content: String,
    #[serde(default)]
    from_palace: Option<String>,
}

/// `POST /api/v1/messages` — deliver an inter-project message.
///
/// Why: lets non-MCP callers (the `trusty-memory send-message` CLI, future
/// remote callers) put messages on a recipient palace's queue. Mirrors the
/// MCP `memory_send_message` tool exactly so they stay in lockstep.
/// What: writes a tagged drawer into the recipient palace via
/// [`crate::messaging::send_message_to_palace`]. Returns
/// `{drawer_id, from_palace, to_palace, purpose, status: "sent"}` on success.
/// Test: `messages_endpoint_round_trip`.
pub(crate) async fn send_message_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SendMessageBody>,
) -> Result<Json<Value>, ApiError> {
    let from_palace = body
        .from_palace
        .or_else(|| state.default_palace.clone())
        .unwrap_or_else(|| "<unknown>".to_string());
    let drawer_id = crate::messaging::send_message_to_palace(
        &state.registry,
        &state.data_root,
        &from_palace,
        &body.to_palace,
        &body.purpose,
        body.content,
        creator_info_from_http(&headers),
    )
    .await
    .map_err(|e| ApiError::internal(format!("send_message: {e:#}")))?;
    // Emit a drawer-added SSE event so the dashboard activity feed shows
    // the new message immediately.
    let drawer_count = open_handle(&state, &body.to_palace)
        .map(|h| h.drawers.read().len())
        .unwrap_or(0);
    state.emit(DaemonEvent::DrawerAdded {
        palace_id: body.to_palace.clone(),
        palace_name: body.to_palace.clone(),
        drawer_count,
        timestamp: chrono::Utc::now(),
        content_preview: format!("[msg from {from_palace}] {}", body.purpose),
        // Issue #96 — record the originating subsystem so the activity feed
        // can badge this row as an HTTP-initiated message.
        source: ActivitySource::Http,
    });
    Ok(Json(json!({
        "drawer_id": drawer_id.to_string(),
        "from_palace": from_palace,
        "to_palace": body.to_palace,
        "purpose": body.purpose,
        "status": "sent",
    })))
}

/// Request body for `POST /api/v1/messages/mark_read`.
///
/// Why: the SessionStart hook needs an explicit, idempotent ack so two
/// concurrent sessions starting on the same palace don't double-deliver.
/// What: identifies a single message by `(palace, drawer_id)`.
/// Test: `messages_endpoint_round_trip`.
#[derive(Deserialize)]
pub(crate) struct MarkReadBody {
    palace: String,
    drawer_id: String,
}

/// `POST /api/v1/messages/mark_read` — atomically flip a message's read flag.
///
/// Why: separating ack from list lets the receiver atomically retire
/// exactly the messages it printed, even when other writers are landing
/// new messages in the same palace.
/// What: parses the drawer id, calls
/// [`crate::messaging::mark_message_read`], and returns `{flipped: bool}`
/// where `flipped == true` iff this call was the one that flipped the flag
/// (returning `false` is fine — it means the drawer was already read or
/// has been concurrently removed; either way no further work is needed).
/// Test: `messages_endpoint_round_trip`.
pub(crate) async fn mark_message_read_handler(
    State(state): State<AppState>,
    Json(body): Json<MarkReadBody>,
) -> Result<Json<Value>, ApiError> {
    let uuid = Uuid::parse_str(&body.drawer_id)
        .map_err(|_| ApiError::bad_request("drawer_id must be a UUID"))?;
    let handle = open_handle(&state, &body.palace)?;
    let flipped = crate::messaging::mark_message_read(&handle, uuid)
        .await
        .map_err(|e| ApiError::internal(format!("mark_read: {e:#}")))?;
    Ok(Json(json!({"flipped": flipped})))
}
