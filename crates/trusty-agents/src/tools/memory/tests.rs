use super::*;
use crate::memory::Embedder;
use crate::memory::redb_usearch::RedbUsearchStore;
use crate::memory::store::{MemoryStore, Segment};
use crate::tools::native_memory::MemoryBackend;
use crate::tools::traits::ToolExecutor;
use serde_json::json;
use std::sync::Arc;
use tempfile::tempdir;

/// Tiny deterministic embedder that maps every text to a fixed-length
/// vector by hashing characters. Avoids loading the real ONNX model in
/// unit tests.
struct StubEmbedder {
    dim: usize,
}

impl Embedder for StubEmbedder {
    fn dimension(&self) -> usize {
        self.dim
    }

    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed_single(t)).collect()
    }

    fn embed_single(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let mut v = vec![0.0f32; self.dim];
        for (i, b) in text.bytes().enumerate() {
            v[i % self.dim] += (b as f32) / 255.0;
        }
        // Normalize so cosine similarity behaves.
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        for x in &mut v {
            *x /= norm;
        }
        Ok(v)
    }
}

#[tokio::test]
async fn memory_recall_returns_graceful_error_without_backend() {
    let tool = MemoryRecallTool::new();
    let out = tool.execute(json!({"query": "anything"})).await;
    // Degrades gracefully: returns Success (not error), with an error
    // key embedded in the JSON payload. This lets the LLM decide to
    // skip memory and continue the task.
    assert!(!out.is_error());
    let body = out.content();
    assert!(body.contains("error"));
    assert!(body.contains("not available") || body.contains("proceed"));
}

#[tokio::test]
async fn memory_recall_requires_query() {
    let tool = MemoryRecallTool::new();
    let out = tool.execute(json!({})).await;
    assert!(out.is_error());
    assert!(out.content().contains("query"));
}

#[tokio::test]
async fn memory_recall_searches_embedded_store() {
    // Wire a real RedbUsearchStore in a tempdir + a stub embedder; insert
    // a memory and confirm `memory_recall` returns it.
    let dir = tempdir().unwrap();
    let dim = 16;
    let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
    let embedder = Arc::new(StubEmbedder { dim });
    let backend = MemoryBackend::new(store.clone(), embedder.clone());

    let content = "PM uses delegate_to_agent to spawn sub-agents over NDJSON.";
    let vec = embedder.embed_single(content).unwrap();
    store
        .insert(
            Segment::AgentMemory,
            "fact-1",
            &vec,
            json!({ "content": content }),
        )
        .await
        .unwrap();

    let tool = MemoryRecallTool::with_backend(backend);
    let out = tool
        .execute(json!({"query": "delegate_to_agent NDJSON", "limit": 5}))
        .await;
    assert!(!out.is_error());
    let body = out.content();
    assert!(
        body.contains("fact-1") && body.contains("delegate_to_agent"),
        "expected hit content in payload, got: {body}"
    );
}

#[tokio::test]
async fn memory_recall_defaults_to_session_scope() {
    // Two memories with different session_ids; without scope, the tool
    // must default to "session" and return only the current session's hit.
    let dir = tempdir().unwrap();
    let dim = 16;
    let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
    let embedder = Arc::new(StubEmbedder { dim });

    let content_a = "Auth flow uses bearer tokens.";
    let content_b = "Auth flow uses bearer tokens.";
    let vec_a = embedder.embed_single(content_a).unwrap();
    let vec_b = embedder.embed_single(content_b).unwrap();
    store
        .insert(
            Segment::AgentMemory,
            "fact-a",
            &vec_a,
            json!({ "content": content_a, "session_id": "session-aaa" }),
        )
        .await
        .unwrap();
    store
        .insert(
            Segment::AgentMemory,
            "fact-b",
            &vec_b,
            json!({ "content": content_b, "session_id": "session-bbb" }),
        )
        .await
        .unwrap();

    let backend =
        MemoryBackend::new(store.clone(), embedder.clone()).with_session_id("session-aaa");
    let tool = MemoryRecallTool::with_backend(backend);

    // Omit scope: must default to current session.
    let out = tool
        .execute(json!({"query": "auth bearer tokens", "limit": 5}))
        .await;
    let body = out.content();
    assert!(
        body.contains("fact-a") && !body.contains("fact-b"),
        "default scope should be 'session' (current); got: {body}"
    );
}

#[tokio::test]
async fn memory_recall_all_scope_returns_cross_session() {
    // scope=all returns memories from every session in the store.
    let dir = tempdir().unwrap();
    let dim = 16;
    let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
    let embedder = Arc::new(StubEmbedder { dim });

    let content = "Auth flow uses bearer tokens.";
    let v = embedder.embed_single(content).unwrap();
    store
        .insert(
            Segment::AgentMemory,
            "fact-a",
            &v,
            json!({ "content": content, "session_id": "session-aaa" }),
        )
        .await
        .unwrap();
    store
        .insert(
            Segment::AgentMemory,
            "fact-b",
            &v,
            json!({ "content": content, "session_id": "session-bbb" }),
        )
        .await
        .unwrap();

    let backend =
        MemoryBackend::new(store.clone(), embedder.clone()).with_session_id("session-aaa");
    let tool = MemoryRecallTool::with_backend(backend);

    let out = tool
        .execute(json!({"query": "auth bearer tokens", "limit": 5, "scope": "all"}))
        .await;
    let body = out.content();
    assert!(
        body.contains("fact-a") && body.contains("fact-b"),
        "scope=all should return both sessions; got: {body}"
    );
}

#[tokio::test]
async fn memory_recall_imported_scope_filters_by_imported_flag() {
    // scope=imported returns only memories whose payload has imported=true.
    let dir = tempdir().unwrap();
    let dim = 16;
    let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
    let embedder = Arc::new(StubEmbedder { dim });

    let content = "Cross-machine fact.";
    let v = embedder.embed_single(content).unwrap();
    store
        .insert(
            Segment::AgentMemory,
            "local-fact",
            &v,
            json!({ "content": content, "session_id": "local-1" }),
        )
        .await
        .unwrap();
    store
        .insert(
            Segment::AgentMemory,
            "remote-fact",
            &v,
            json!({
                "content": content,
                "session_id": "remote-x",
                "imported": true,
                "machine_id": "remote-host"
            }),
        )
        .await
        .unwrap();

    let backend = MemoryBackend::new(store.clone(), embedder.clone()).with_session_id("local-1");
    let tool = MemoryRecallTool::with_backend(backend);

    let out = tool
        .execute(json!({"query": "cross machine", "limit": 5, "scope": "imported"}))
        .await;
    let body = out.content();
    assert!(
        body.contains("remote-fact") && !body.contains("local-fact"),
        "scope=imported should return only imported=true memories; got: {body}"
    );
}

#[tokio::test]
async fn memory_recall_filters_by_tag() {
    // Insert three memories with different tags; queries with tag filter
    // should return only the matching one.
    let dir = tempdir().unwrap();
    let dim = 16;
    let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
    let embedder = Arc::new(StubEmbedder { dim });

    let content = "Useful information about deployment.";
    let v = embedder.embed_single(content).unwrap();
    store
        .insert(
            Segment::AgentMemory,
            "doc-1",
            &v,
            json!({ "content": content, "tag": "docs/user", "session_id": "s" }),
        )
        .await
        .unwrap();
    store
        .insert(
            Segment::AgentMemory,
            "skill-1",
            &v,
            json!({ "content": content, "tag": "configuration/skill", "session_id": "s" }),
        )
        .await
        .unwrap();
    store
        .insert(
            Segment::AgentMemory,
            "mcp-1",
            &v,
            json!({ "content": content, "tag": "configuration/mcp", "session_id": "s" }),
        )
        .await
        .unwrap();

    let backend = MemoryBackend::new(store.clone(), embedder.clone()).with_session_id("s");
    let tool = MemoryRecallTool::with_backend(backend);

    // tag=configuration/skill (with scope=all so the session filter doesn't interfere).
    let out = tool
        .execute(json!({"query": "deployment info", "scope": "all", "tag": "configuration/skill"}))
        .await;
    let body = out.content();
    assert!(
        body.contains("skill-1"),
        "expected skill-1 in payload: {body}"
    );
    assert!(
        !body.contains("doc-1"),
        "doc-1 should be filtered out: {body}"
    );
    assert!(
        !body.contains("mcp-1"),
        "mcp-1 should be filtered out: {body}"
    );

    // tag=configuration/mcp
    let out2 = tool
        .execute(json!({"query": "deployment info", "scope": "all", "tag": "configuration/mcp"}))
        .await;
    let body2 = out2.content();
    assert!(body2.contains("mcp-1"), "expected mcp-1: {body2}");
    assert!(!body2.contains("skill-1"));
    assert!(!body2.contains("doc-1"));

    // Prefix match: tag=configuration returns both skills AND MCP, but not docs.
    let out_prefix = tool
        .execute(json!({"query": "deployment info", "scope": "all", "tag": "configuration"}))
        .await;
    let body_prefix = out_prefix.content();
    assert!(
        body_prefix.contains("skill-1"),
        "prefix tag=configuration should match skill-1: {body_prefix}"
    );
    assert!(
        body_prefix.contains("mcp-1"),
        "prefix tag=configuration should match mcp-1: {body_prefix}"
    );
    assert!(
        !body_prefix.contains("doc-1"),
        "prefix tag=configuration should NOT match doc-1: {body_prefix}"
    );

    // Prefix match: tag=docs returns only docs.
    let out_docs = tool
        .execute(json!({"query": "deployment info", "scope": "all", "tag": "docs"}))
        .await;
    let body_docs = out_docs.content();
    assert!(body_docs.contains("doc-1"));
    assert!(!body_docs.contains("skill-1"));
    assert!(!body_docs.contains("mcp-1"));

    // No tag = all matches returned.
    let out3 = tool
        .execute(json!({"query": "deployment info", "scope": "all"}))
        .await;
    let body3 = out3.content();
    assert!(body3.contains("doc-1"));
    assert!(body3.contains("skill-1"));
    assert!(body3.contains("mcp-1"));
}

/// #193: An agent caller MUST be capped at `RecallCeiling::Agent` —
/// even when it requests `scope: "all"`, only memories tagged with its
/// own `agent/<id>` should come back (untagged legacy rows are still
/// allowed for back-compat, but a foreign-agent tag must be filtered).
///
/// We use std::env::set_var inside a serial test guarded with a Mutex
/// because the env var bridge is global. Running in a single tokio
/// runtime per test prevents interleaving with other env-reading tests.
#[tokio::test]
async fn agent_ceiling_filters_foreign_agent_tags() {
    let dir = tempdir().unwrap();
    let dim = 16;
    let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
    let embedder = Arc::new(StubEmbedder { dim });

    let content = "Useful agent memory.";
    let v = embedder.embed_single(content).unwrap();
    // Memory written by `code-agent` (foreign).
    store
        .insert(
            Segment::AgentMemory,
            "foreign",
            &v,
            json!({
                "content": content,
                "session_id": "sess-1",
                "tags": ["session/sess-1", "agent/code-agent"]
            }),
        )
        .await
        .unwrap();
    // Memory written by `research-agent` (self).
    store
        .insert(
            Segment::AgentMemory,
            "self",
            &v,
            json!({
                "content": content,
                "session_id": "sess-1",
                "tags": ["session/sess-1", "agent/research-agent"]
            }),
        )
        .await
        .unwrap();
    // Legacy untagged memory — allowed under back-compat.
    store
        .insert(
            Segment::AgentMemory,
            "legacy",
            &v,
            json!({
                "content": content,
                "session_id": "sess-1"
            }),
        )
        .await
        .unwrap();

    // Pin identity explicitly via the builder so this test doesn't
    // pollute process-wide env vars (other parallel tests would observe
    // them and erroneously apply the agent ceiling).
    let identity = crate::identity::CallerIdentity::Agent {
        session_id: "sess-1".into(),
        project_id: "proj".into(),
        agent_id: "research-agent".into(),
    };
    let backend = MemoryBackend::new(store.clone(), embedder.clone());
    let tool = MemoryRecallTool::with_backend(backend).with_identity(Some(identity));
    // Even when the agent asks for scope=all, the ceiling must downgrade
    // to session and the agent-tag filter must drop "foreign".
    let out = tool
        .execute(json!({"query": "useful", "limit": 50, "scope": "all"}))
        .await;
    let body = out.content();

    assert!(
        body.contains("\"self\""),
        "self-agent memory should be returned: {body}"
    );
    assert!(
        body.contains("\"legacy\""),
        "legacy untagged memory should be returned (back-compat): {body}"
    );
    assert!(
        !body.contains("\"foreign\""),
        "foreign-agent memory must be filtered: {body}"
    );
}

/// #277: With no `segment` arg, `memory_recall` searches `AgentMemory`
/// (the legacy default). Memories inserted into `Context` must NOT come
/// back from a default-segment query.
#[tokio::test]
async fn memory_recall_defaults_to_agent_memory_segment() {
    let dir = tempdir().unwrap();
    let dim = 16;
    let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
    let embedder = Arc::new(StubEmbedder { dim });

    let content = "shared content text";
    let v = embedder.embed_single(content).unwrap();
    store
        .insert(
            Segment::AgentMemory,
            "in-agent",
            &v,
            json!({ "content": content, "session_id": "s" }),
        )
        .await
        .unwrap();
    store
        .insert(
            Segment::Context,
            "in-context",
            &v,
            json!({ "content": content, "session_id": "s" }),
        )
        .await
        .unwrap();

    let backend = MemoryBackend::new(store.clone(), embedder.clone()).with_session_id("s");
    let tool = MemoryRecallTool::with_backend(backend);
    let out = tool
        .execute(json!({"query": "shared content", "scope": "all"}))
        .await;
    let body = out.content();
    assert!(
        body.contains("in-agent") && !body.contains("in-context"),
        "default segment must be AgentMemory; got: {body}"
    );
}

/// #277: With `segment: "context"`, `memory_recall` searches
/// `Segment::Context` and returns rows stored there.
#[tokio::test]
async fn memory_recall_routes_to_context_segment() {
    let dir = tempdir().unwrap();
    let dim = 16;
    let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
    let embedder = Arc::new(StubEmbedder { dim });

    let content = "architecture fact";
    let v = embedder.embed_single(content).unwrap();
    store
        .insert(
            Segment::Context,
            "ctx-1",
            &v,
            json!({ "content": content, "session_id": "s" }),
        )
        .await
        .unwrap();

    let backend = MemoryBackend::new(store.clone(), embedder.clone()).with_session_id("s");
    let tool = MemoryRecallTool::with_backend(backend);
    let out = tool
        .execute(json!({"query": "architecture", "scope": "all", "segment": "context"}))
        .await;
    let body = out.content();
    assert!(
        body.contains("ctx-1"),
        "segment=context should return Context rows: {body}"
    );
}

/// #277: With `segment: "brief"`, recall hits `Segment::Brief`.
#[tokio::test]
async fn memory_recall_routes_to_brief_segment() {
    let dir = tempdir().unwrap();
    let dim = 16;
    let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
    let embedder = Arc::new(StubEmbedder { dim });

    let content = "active goal";
    let v = embedder.embed_single(content).unwrap();
    store
        .insert(
            Segment::Brief,
            "brief-1",
            &v,
            json!({ "content": content, "session_id": "s" }),
        )
        .await
        .unwrap();

    let backend = MemoryBackend::new(store.clone(), embedder.clone()).with_session_id("s");
    let tool = MemoryRecallTool::with_backend(backend);
    let out = tool
        .execute(json!({"query": "active goal", "scope": "all", "segment": "brief"}))
        .await;
    let body = out.content();
    assert!(
        body.contains("brief-1"),
        "segment=brief should return Brief rows: {body}"
    );
}

/// #277: An unknown segment string falls back to `AgentMemory` rather
/// than failing the call (graceful degradation).
#[tokio::test]
async fn memory_recall_unknown_segment_falls_back_to_agent_memory() {
    let dir = tempdir().unwrap();
    let dim = 16;
    let store = Arc::new(RedbUsearchStore::open(dir.path(), dim).unwrap());
    let embedder = Arc::new(StubEmbedder { dim });

    let content = "fallback content";
    let v = embedder.embed_single(content).unwrap();
    store
        .insert(
            Segment::AgentMemory,
            "agent-1",
            &v,
            json!({ "content": content, "session_id": "s" }),
        )
        .await
        .unwrap();

    let backend = MemoryBackend::new(store.clone(), embedder.clone()).with_session_id("s");
    let tool = MemoryRecallTool::with_backend(backend);
    let out = tool
        .execute(json!({"query": "fallback", "scope": "all", "segment": "totally_unknown"}))
        .await;
    let body = out.content();
    assert!(!out.is_error());
    assert!(
        body.contains("agent-1"),
        "unknown segment should fall back to AgentMemory: {body}"
    );
}

#[tokio::test]
async fn vector_search_returns_graceful_error_without_index() {
    let tmp = tempdir().unwrap();
    let missing = tmp.path().join("no-index");
    let tool = VectorSearchTool::new().with_code_dir(missing);
    let out = tool.execute(json!({"query": "foo"})).await;
    // Falls back to grep mode rather than erroring out.
    assert!(!out.is_error());
    let body = out.content();
    assert!(body.contains("grep_fallback"));
}

#[tokio::test]
async fn vector_search_requires_query() {
    let tool = VectorSearchTool::new();
    let out = tool.execute(json!({})).await;
    assert!(out.is_error());
    assert!(out.content().contains("query"));
}

#[test]
fn memory_recall_schema_names_tool() {
    let t = MemoryRecallTool::new();
    assert_eq!(t.name(), "memory_recall");
    let s = t.schema();
    assert_eq!(s["function"]["name"], "memory_recall");
}

#[test]
fn vector_search_schema_names_tool() {
    let t = VectorSearchTool::new();
    assert_eq!(t.name(), "vector_search");
    let s = t.schema();
    assert_eq!(s["function"]["name"], "vector_search");
}

/// #3864: the parameter whose ABSENCE was the bug — the persona documented
/// `vector_search(index_id=…)` while the schema advertised no such field, so
/// index routing silently no-op'd.
#[test]
fn vector_search_schema_advertises_index_id() {
    let s = VectorSearchTool::new().schema();
    let props = &s["function"]["parameters"]["properties"];
    assert!(
        props.get("index_id").is_some(),
        "index_id must be advertised: {props}"
    );
    assert_eq!(props["index_id"]["type"], "string");
    // Still optional — an agent with no bound store must keep working.
    let required = s["function"]["parameters"]["required"].as_array().unwrap();
    assert!(!required.iter().any(|v| v == "index_id"));
}

/// The bound-store id is named in the schema so the model can see which
/// corpus an unqualified call actually searches.
#[test]
fn vector_search_schema_names_bound_default_index() {
    let s = VectorSearchTool::new()
        .with_default_index(Some("cto-assistant".to_string()))
        .schema();
    let desc = s["function"]["parameters"]["properties"]["index_id"]["description"]
        .as_str()
        .unwrap();
    assert!(desc.contains("cto-assistant"), "description was: {desc}");
}

#[test]
fn vector_search_index_defaults_to_bound_store() {
    let t = VectorSearchTool::new().with_default_index(Some("bob-kb".to_string()));
    assert_eq!(
        t.effective_index_id(&json!({"query": "q"})).as_deref(),
        Some("bob-kb")
    );
}

#[test]
fn vector_search_prefers_explicit_index_over_default() {
    let t = VectorSearchTool::new().with_default_index(Some("bob-kb".to_string()));
    assert_eq!(
        t.effective_index_id(&json!({"query": "q", "index_id": "cto-assistant"}))
            .as_deref(),
        Some("cto-assistant"),
        "an explicit index_id must always win over the bound default"
    );
}

#[test]
fn vector_search_index_none_without_binding() {
    let t = VectorSearchTool::new();
    assert_eq!(t.effective_index_id(&json!({"query": "q"})), None);
}

/// A blank/whitespace id (from a malformed binding or a model emitting `""`)
/// must be treated as absent — never used to build a `/indexes//search` URL.
#[test]
fn vector_search_blank_index_ids_are_treated_as_absent() {
    let t = VectorSearchTool::new().with_default_index(Some("   ".to_string()));
    assert_eq!(t.effective_index_id(&json!({"query": "q"})), None);
    let t = VectorSearchTool::new().with_default_index(Some("bob-kb".to_string()));
    assert_eq!(
        t.effective_index_id(&json!({"query": "q", "index_id": "  "}))
            .as_deref(),
        Some("bob-kb"),
        "a blank explicit id falls back to the bound default, not to an empty id"
    );
}

/// End-to-end #3864: a bound store routes the query to that index on the
/// trusty-search daemon and returns the normalized hit envelope.
#[tokio::test]
async fn vector_search_routes_to_daemon_index() {
    use axum::{Json, Router, extract::Path, http::StatusCode, routing::post};
    use tokio::net::TcpListener;

    let app = Router::new().route(
        "/indexes/{id}/search",
        post(|Path(id): Path<String>| async move {
            if id == "bob-kb" {
                (
                    StatusCode::OK,
                    Json(json!({"results": [
                        {"path": "notes/travel.md", "score": 0.87, "content": "flight to NYC"}
                    ]})),
                )
            } else {
                (StatusCode::NOT_FOUND, Json(json!({"error": "no index"})))
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let tmp = tempdir().unwrap();
    let tool = VectorSearchTool::new()
        .with_code_dir(tmp.path().join("no-index"))
        .with_default_index(Some("bob-kb".to_string()))
        .with_search_base_url(Some(format!("http://{addr}")));

    let out = tool.execute(json!({"query": "travel"})).await;
    assert!(!out.is_error());
    let body = out.content();
    assert!(body.contains("notes/travel.md"), "body was: {body}");
    assert!(
        !body.contains("grep_fallback"),
        "daemon hit must not fall through: {body}"
    );
}

/// A missing index on the daemon degrades to the local/grep path rather than
/// erroring the agent turn.
#[tokio::test]
async fn vector_search_falls_back_when_daemon_index_missing() {
    use axum::{Router, http::StatusCode, routing::post};
    use tokio::net::TcpListener;

    let app = Router::new().route(
        "/indexes/{id}/search",
        post(|| async { (StatusCode::NOT_FOUND, "no such index") }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let tmp = tempdir().unwrap();
    let tool = VectorSearchTool::new()
        .with_code_dir(tmp.path().join("no-index"))
        .with_default_index(Some("ghost".to_string()))
        .with_search_base_url(Some(format!("http://{addr}")));

    let out = tool.execute(json!({"query": "anything"})).await;
    assert!(!out.is_error(), "must degrade, not error");
    assert!(out.content().contains("grep_fallback"));
}
