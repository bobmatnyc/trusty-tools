//! Unit tests for `MemoryGraph` using mock store + embedder.
//!
//! Why: The graph's record/search/traversal logic is testable without disk or
//! a real embedder; mocks keep CI fast and deterministic.
//! What: Mock `Embedder`/`MemoryStore`, fixtures, and tests for record+search
//! round-trip, temporal ordering, delegation traversal, edge-row exclusion,
//! and cross-session merge.
//! Test: This module is itself the test coverage.

use super::*;
use async_trait::async_trait;
use chrono::TimeZone;
use std::collections::HashMap;
use tokio::sync::Mutex as TokioMutex;

// --- Mock Embedder -------------------------------------------------

/// Deterministic embedder that returns fixed 384-dim vectors.
///
/// Why: Avoid downloading the real fastembed model in unit tests. The
/// graph code cares that embedding produces a vector of the right size;
/// similarity quality is tested elsewhere.
struct MockEmbedder {
    dim: usize,
}

impl Embedder for MockEmbedder {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.1f32; self.dim]).collect())
    }
    fn embed_single(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.1f32; self.dim])
    }
    fn dimension(&self) -> usize {
        self.dim
    }
}

// --- Mock Store ----------------------------------------------------

/// In-memory `MemoryStore` impl keyed by (segment prefix, id).
///
/// Why: Unit tests need a store they can construct synchronously without
/// touching disk. Search returns all matching-segment entries in insertion
/// order truncated to `top_k` — good enough because the graph tests don't
/// assert nearest-neighbor ordering, only that specific ids appear.
#[derive(Default)]
struct MockStore {
    inner: TokioMutex<MockInner>,
}

#[derive(Default)]
struct MockInner {
    // Insertion order tracker so "search" is deterministic.
    order: Vec<(String, String)>, // (segment_prefix, id)
    data: HashMap<(String, String), (Vec<f32>, serde_json::Value)>,
}

#[async_trait]
impl MemoryStore for MockStore {
    async fn insert(
        &self,
        segment: Segment,
        id: &str,
        vector: &[f32],
        payload: serde_json::Value,
    ) -> Result<()> {
        let mut g = self.inner.lock().await;
        let key = (segment.prefix().to_string(), id.to_string());
        if !g.data.contains_key(&key) {
            g.order.push(key.clone());
        }
        g.data.insert(key, (vector.to_vec(), payload));
        Ok(())
    }
    async fn search(
        &self,
        segment: Segment,
        _query_vec: &[f32],
        top_k: usize,
    ) -> Result<Vec<MemoryResult>> {
        let g = self.inner.lock().await;
        let prefix = segment.prefix().to_string();
        let mut out = Vec::new();
        for (seg, id) in &g.order {
            if seg != &prefix {
                continue;
            }
            let (_v, payload) = g.data.get(&(seg.clone(), id.clone())).unwrap();
            out.push(MemoryResult {
                id: id.clone(),
                score: 1.0,
                payload: payload.clone(),
                segment: seg.clone(),
            });
            if out.len() >= top_k {
                break;
            }
        }
        Ok(out)
    }
    async fn get(&self, segment: Segment, id: &str) -> Result<Option<serde_json::Value>> {
        let g = self.inner.lock().await;
        Ok(g.data
            .get(&(segment.prefix().to_string(), id.to_string()))
            .map(|(_v, p)| p.clone()))
    }
    async fn delete(&self, segment: Segment, id: &str) -> Result<()> {
        let mut g = self.inner.lock().await;
        let key = (segment.prefix().to_string(), id.to_string());
        g.data.remove(&key);
        g.order.retain(|k| k != &key);
        Ok(())
    }
}

fn make_graph() -> MemoryGraph {
    let store: Arc<dyn MemoryStore> = Arc::new(MockStore::default());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 384 });
    MemoryGraph::new(store, embedder)
}

fn session(
    id: &str,
    run_id: &str,
    parent: Option<&str>,
    ts_secs: i64,
    prompt: &str,
    response: &str,
) -> AgentSession {
    AgentSession {
        id: id.to_string(),
        agent_name: "test-agent".to_string(),
        workflow_run_id: run_id.to_string(),
        phase: "test".to_string(),
        prompt: prompt.to_string(),
        response: response.to_string(),
        timestamp: Utc.timestamp_opt(ts_secs, 0).unwrap(),
        parent_id: parent.map(str::to_string),
        segment: None,
    }
}

#[tokio::test]
async fn record_and_search_round_trip() {
    let graph = make_graph();
    let s = session(
        "sess-1",
        "run-1",
        None,
        1000,
        "write a markdown table",
        "| a | b |\n|---|---|",
    );
    graph.record(s.clone()).await.unwrap();

    let hits = graph.search("write a markdown table", 10).await.unwrap();
    let ids: Vec<_> = hits.iter().map(|h| h.id.as_str()).collect();
    assert!(
        ids.contains(&"sess-1"),
        "expected sess-1 in search results, got: {ids:?}"
    );
}

#[tokio::test]
async fn get_run_returns_sessions_in_temporal_order() {
    let graph = make_graph();
    // Insert out of order to verify sorting.
    graph
        .record(session("b", "run-x", None, 2000, "q2", "r2"))
        .await
        .unwrap();
    graph
        .record(session("a", "run-x", None, 1000, "q1", "r1"))
        .await
        .unwrap();
    graph
        .record(session("c", "run-x", None, 3000, "q3", "r3"))
        .await
        .unwrap();

    let sessions = graph.get_run("run-x").await.unwrap();
    let ids: Vec<_> = sessions.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "b", "c"], "expected chronological order");
}

#[tokio::test]
async fn get_children_returns_delegated_sessions() {
    let graph = make_graph();
    graph
        .record(session("parent", "run-p", None, 1000, "pq", "pr"))
        .await
        .unwrap();
    graph
        .record(session(
            "child1",
            "run-p",
            Some("parent"),
            1100,
            "c1q",
            "c1r",
        ))
        .await
        .unwrap();
    graph
        .record(session(
            "child2",
            "run-p",
            Some("parent"),
            1200,
            "c2q",
            "c2r",
        ))
        .await
        .unwrap();

    let kids = graph.get_children("parent").await.unwrap();
    let ids: Vec<_> = kids.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&"child1"));
    assert!(ids.contains(&"child2"));
}

#[tokio::test]
async fn search_excludes_edge_entries() {
    let graph = make_graph();
    // Recording two sessions in the same run creates a `followed_by` edge
    // plus `run:` marker + `run-sessions:` list rows — all should be
    // filtered out of search results.
    graph
        .record(session(
            "s1",
            "run-e",
            None,
            1000,
            "first prompt",
            "first response",
        ))
        .await
        .unwrap();
    graph
        .record(session(
            "s2",
            "run-e",
            Some("s1"),
            2000,
            "second prompt",
            "second response",
        ))
        .await
        .unwrap();

    let hits = graph.search("first prompt", 50).await.unwrap();
    for h in &hits {
        assert!(
            !MemoryGraph::is_auxiliary_key(&h.id),
            "search returned auxiliary row: {}",
            h.id
        );
    }
    let ids: Vec<_> = hits.iter().map(|h| h.id.as_str()).collect();
    assert!(ids.contains(&"s1"));
    assert!(ids.contains(&"s2"));
}

#[tokio::test]
async fn get_session_returns_none_when_missing() {
    let graph = make_graph();
    let got = graph.get_session("nope").await.unwrap();
    assert!(got.is_none());
}

#[tokio::test]
async fn search_all_sessions_merges_across_run_ids() {
    // Uses real SessionStores backed by tempdir to exercise the actual
    // directory walk. The MockEmbedder avoids downloading fastembed.
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let sessions_dir = dir.path().join("sessions");

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 384 });

    // Session A: record one session.
    {
        let store = SessionStore::new_arc(&sessions_dir, "run-a", 384).unwrap();
        let graph = MemoryGraph::new(store.clone(), embedder.clone());
        graph
            .record(session("sa1", "run-a", None, 1000, "query a", "response a"))
            .await
            .unwrap();
    }
    // Session B: record one session.
    {
        let store = SessionStore::new_arc(&sessions_dir, "run-b", 384).unwrap();
        let graph = MemoryGraph::new(store.clone(), embedder.clone());
        graph
            .record(session("sb1", "run-b", None, 2000, "query b", "response b"))
            .await
            .unwrap();
    }

    let hits = MemoryGraph::search_all_sessions(&sessions_dir, embedder.as_ref(), "anything", 10)
        .await
        .unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
    assert!(ids.contains(&"sa1"), "expected sa1 in: {ids:?}");
    assert!(ids.contains(&"sb1"), "expected sb1 in: {ids:?}");
    for h in &hits {
        assert!(
            !MemoryGraph::is_auxiliary_key(&h.id),
            "auxiliary id leaked: {}",
            h.id
        );
    }
}
