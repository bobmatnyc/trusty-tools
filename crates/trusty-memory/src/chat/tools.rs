//! Chat tool definitions + the `execute_*` dispatcher set.
//!
//! Why: the chat assistant's tool surface (`all_tools`) and the in-process
//! dispatcher that runs each tool (`execute_tool` + the per-tool `execute_*`
//! functions) form one cohesive concern split out of the former monolithic
//! `chat.rs` (issue #607).
//! What: `ChatBody`, `MAX_TOOL_ROUNDS`, `all_tools`, `execute_tool`, and every
//! `execute_*` helper, moved verbatim. Visibility unchanged.
//! Test: `all_tools_returns_expected_set`, `execute_tool_dispatches_known_tools`
//! in `web::tests`.

use crate::web::{load_user_config, palace_info_from, DreamStatusPayload};
use crate::AppState;
use serde::Deserialize;
use serde_json::{json, Value};
use trusty_common::memory_core::dream::PersistedDreamStats;
use trusty_common::memory_core::palace::{PalaceId, RoomType};
use trusty_common::memory_core::retrieval::{
    recall_across_palaces_with_default_embedder, recall_with_default_embedder,
};
use trusty_common::memory_core::store::kg::Triple;
use trusty_common::memory_core::PalaceRegistry;
use trusty_common::{ChatMessage, ToolDef};

// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct ChatBody {
    #[serde(default)]
    pub(crate) palace_id: Option<String>,
    pub(crate) message: String,
    #[serde(default)]
    pub(crate) history: Vec<ChatMessage>,
    /// Optional existing chat-session id; when provided we load+append+save.
    #[serde(default)]
    pub(crate) session_id: Option<String>,
}

/// Hard cap on the number of `tool -> assistant` round trips per chat turn.
///
/// Why: Without a bound, a malicious or confused model could request tools
/// indefinitely; 10 is generous enough for any realistic plan-and-act loop
/// while still terminating quickly when the model gets stuck.
pub(crate) const MAX_TOOL_ROUNDS: usize = 10;

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

pub(crate) async fn execute_get_dream_status(state: &AppState) -> Value {
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
