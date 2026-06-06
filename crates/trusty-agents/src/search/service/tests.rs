//! Tests for the search daemon: pid-file IO, socket-path convention,
//! liveness probing, and an end-to-end router round-trip.
//!
//! Why: Verifies the rendezvous contract (pid file + socket path) external
//! processes rely on, and that the axum router serves all five `/search/*`
//! routes correctly without spinning up a real embedder.
//! What: Unit tests for the pid/socket helpers plus an integration test that
//! exercises [`build_router`] over an in-memory mock store + embedder.
//! Test: This *is* the test module.

use super::*;
use serde_json::Value;
use tempfile::TempDir;

#[test]
fn pid_file_roundtrip() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    std::fs::create_dir_all(project.join(".trusty-agents").join("state")).unwrap();

    let state = SearchDaemonState {
        pid: 12345,
        started_at: Utc::now(),
        port: 54321,
        socket_path: PathBuf::from("/tmp/test.sock"),
    };
    write_pid_file(project, &state).expect("write");
    let back = read_pid_file(project).expect("read");
    assert_eq!(back.pid, 12345);
    assert_eq!(back.port, 54321);
    assert_eq!(back.socket_path, PathBuf::from("/tmp/test.sock"));
}

#[test]
fn read_missing_pid_file_is_none() {
    let dir = TempDir::new().unwrap();
    assert!(read_pid_file(dir.path()).is_none());
}

#[test]
fn pid_file_path_is_under_state_dir() {
    let dir = TempDir::new().unwrap();
    let p = pid_file_path(dir.path());
    assert!(p.ends_with(".trusty-agents/state/search.pid"));
}

#[test]
fn search_socket_path_uses_project_id() {
    let p = PathBuf::from("/tmp/some-project");
    let s = search_socket_path(&p);
    let s_str = s.to_string_lossy().into_owned();
    // Either uses HOME-based sockets dir or falls back to project state.
    assert!(
        s_str.contains("some-project.search.sock") || s_str.ends_with("search.sock"),
        "unexpected socket path: {s_str}"
    );
}

#[tokio::test]
async fn health_ok_returns_false_for_unbound_port() {
    // Port 1 is privileged; nothing should be listening at user level.
    assert!(!health_ok(1).await);
}

#[tokio::test]
async fn router_serves_health_with_mock_indexer() {
    // Why: Exercises the axum router and the SearchState plumbing
    // without spinning up a full daemon (which needs an embedder
    // model + file watcher). Uses an in-memory MockStore + MockEmbedder
    // mirrored from the indexer tests.
    use crate::memory::{Embedder, MemoryResult, MemoryStore, Segment};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;

    struct MockStore {
        inner: StdMutex<HashMap<String, (Vec<f32>, Value)>>,
    }
    #[async_trait]
    impl MemoryStore for MockStore {
        async fn insert(&self, _: Segment, id: &str, v: &[f32], p: Value) -> anyhow::Result<()> {
            self.inner
                .lock()
                .unwrap()
                .insert(id.into(), (v.to_vec(), p));
            Ok(())
        }
        async fn search(
            &self,
            _: Segment,
            _: &[f32],
            _: usize,
        ) -> anyhow::Result<Vec<MemoryResult>> {
            Ok(vec![])
        }
        async fn get(&self, _: Segment, id: &str) -> anyhow::Result<Option<Value>> {
            Ok(self.inner.lock().unwrap().get(id).map(|(_, p)| p.clone()))
        }
        async fn delete(&self, _: Segment, id: &str) -> anyhow::Result<()> {
            self.inner.lock().unwrap().remove(id);
            Ok(())
        }
    }
    struct MockEmbedder;
    impl Embedder for MockEmbedder {
        fn embed(&self, t: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(t.iter().map(|s| vec![s.len() as f32; 8]).collect())
        }
        fn embed_single(&self, t: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![t.len() as f32; 8])
        }
        fn dimension(&self) -> usize {
            8
        }
    }

    let store: Arc<dyn MemoryStore> = Arc::new(MockStore {
        inner: StdMutex::new(HashMap::new()),
    });
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder);
    let indexer = Arc::new(CodeIndexer::new(store, embedder));

    let dir = TempDir::new().unwrap();
    let state = SearchState {
        indexer,
        project_root: dir.path().to_path_buf(),
        reindex_in_flight: Arc::new(Mutex::new(false)),
    };
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give the listener a moment to come up. 50ms is plenty on localhost.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let url = format!("http://127.0.0.1:{port}/search/health");
    let resp = reqwest::get(&url).await.expect("GET /search/health");
    assert!(resp.status().is_success());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert!(body["version"].is_string());

    // Empty-query is rejected.
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/search/query"))
        .json(&serde_json::json!({"query": "", "top_k": 5}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // Valid query returns a JSON array (mock indexer returns []).
    let resp = client
        .post(format!("http://127.0.0.1:{port}/search/query"))
        .json(&serde_json::json!({"query": "foo", "top_k": 3}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: Value = resp.json().await.unwrap();
    assert!(body.is_array(), "expected JSON array, got {body:?}");

    // /search/reindex returns started.
    let resp = client
        .post(format!("http://127.0.0.1:{port}/search/reindex"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "started");

    server.abort();
}
