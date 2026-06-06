//! Native search tools (#133, #137) — typed replacements for shell_exec-based search.
//!
//! Why: Agents currently invoke code search / skill listing through the
//! `shell_exec` tool. That couples the agent to bash and makes it hard to
//! enforce structure in the result. Native tools return JSON directly, so
//! downstream LLM reasoning has a predictable shape.
//! What: Three tools — `search_code` (`search_code`), `search_memory`
//! (`search_memory`), `search_skills` (`search_skills`), plus shared
//! hit-shaping/grep helpers (`helpers`). When the underlying backend is not
//! wired, each tool returns a structured `{"error": "...", "hits": []}` (or
//! grep fallback) payload rather than panicking.
//! Test: Each tool's `name()`, `schema()`, happy-path `execute()` and
//! graceful-degradation path are covered in the `tests` module below.

mod helpers;
mod search_code;
mod search_memory;
mod search_skills;

pub use search_code::SearchCodeTool;
pub use search_memory::SearchMemoryTool;
pub use search_skills::SearchSkillsTool;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::helpers::grep_fallback_search;
    use super::{SearchCodeTool, SearchMemoryTool, SearchSkillsTool};
    use crate::memory::store::{MemoryResult, MemoryStore, Segment};
    use crate::memory::{AgentSession, Embedder, MemoryGraph};
    use crate::search::indexer::CodeIndexer;
    use crate::tools::traits::{SkillResolver, ToolExecutor};

    // ------- Shared mock infrastructure (insertion-order search) -------

    struct MockStore {
        inner: Mutex<HashMap<String, (Vec<f32>, Value)>>,
        order: Mutex<Vec<String>>,
    }

    impl MockStore {
        fn new() -> Self {
            Self {
                inner: Mutex::new(HashMap::new()),
                order: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl MemoryStore for MockStore {
        async fn insert(
            &self,
            _segment: Segment,
            id: &str,
            vector: &[f32],
            payload: Value,
        ) -> anyhow::Result<()> {
            self.inner
                .lock()
                .unwrap()
                .insert(id.to_string(), (vector.to_vec(), payload));
            let mut order = self.order.lock().unwrap();
            if !order.contains(&id.to_string()) {
                order.push(id.to_string());
            }
            Ok(())
        }

        async fn search(
            &self,
            _segment: Segment,
            _query_vec: &[f32],
            top_k: usize,
        ) -> anyhow::Result<Vec<MemoryResult>> {
            let order = self.order.lock().unwrap().clone();
            let inner = self.inner.lock().unwrap();
            let mut out = Vec::new();
            for (score_idx, id) in order.iter().take(top_k).enumerate() {
                if let Some((_, payload)) = inner.get(id) {
                    out.push(MemoryResult {
                        id: id.clone(),
                        score: 1.0 - (score_idx as f32) * 0.1,
                        payload: payload.clone(),
                        segment: "mem".to_string(),
                    });
                }
            }
            Ok(out)
        }

        async fn get(&self, _segment: Segment, id: &str) -> anyhow::Result<Option<Value>> {
            Ok(self.inner.lock().unwrap().get(id).map(|(_, p)| p.clone()))
        }

        async fn delete(&self, _segment: Segment, id: &str) -> anyhow::Result<()> {
            self.inner.lock().unwrap().remove(id);
            self.order.lock().unwrap().retain(|x| x != id);
            Ok(())
        }
    }

    struct MockEmbedder {
        dim: usize,
    }

    impl Embedder for MockEmbedder {
        fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| vec![t.len() as f32 / 100.0; self.dim])
                .collect())
        }

        fn embed_single(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![text.len() as f32 / 100.0; self.dim])
        }

        fn dimension(&self) -> usize {
            self.dim
        }
    }

    // Filesystem-backed mock skill resolver.
    struct FakeSkillResolver {
        skills: HashMap<String, String>,
    }

    impl SkillResolver for FakeSkillResolver {
        fn resolve(&self, name: &str) -> Option<String> {
            self.skills.get(name).cloned()
        }
        fn list(&self) -> Vec<String> {
            let mut v: Vec<String> = self.skills.keys().cloned().collect();
            v.sort();
            v
        }
    }

    // ------- search_code tests -------

    #[tokio::test]
    async fn search_code_reports_name_and_schema() {
        let t = SearchCodeTool::new();
        assert_eq!(t.name(), "search_code");
        let s = t.schema();
        assert_eq!(s["function"]["name"], "search_code");
        assert_eq!(s["function"]["parameters"]["required"][0], "query");
    }

    #[tokio::test]
    async fn search_code_errors_on_missing_query() {
        let t = SearchCodeTool::new();
        let out = t.execute(json!({})).await;
        assert!(out.is_error());
    }

    #[tokio::test]
    async fn search_code_degrades_gracefully() {
        // No backend injected → tool must return Success without panicking.
        // After #213 the tool falls back to a grep over CWD, so the response
        // shape is `{query, hits, fallback}` (no `error` field). `hits` may
        // be empty or non-empty depending on what's in the working dir; we
        // only assert the envelope is well-formed.
        let t = SearchCodeTool::new();
        let out = t
            .execute(json!({"query": "__definitely_no_such_token_xyz__"}))
            .await;
        assert!(!out.is_error());
        let v: Value = serde_json::from_str(out.content()).unwrap();
        assert_eq!(v["query"], "__definitely_no_such_token_xyz__");
        assert!(v["hits"].is_array());
        assert_eq!(v["fallback"], "grep");
    }

    #[tokio::test]
    async fn search_code_falls_back_to_grep_when_indexer_absent() {
        // We can't safely chdir in tests (process-global state) and the
        // CWD-walking branch could miss our file inside a large repo before
        // hitting `top_n`. Drive the helper directly with a tempdir + fixture
        // to get deterministic results.
        let dir = tempdir().unwrap();
        let fixture = dir.path().join("notes.md");
        std::fs::write(
            &fixture,
            "line one\nintent classifier explained here\nline three\n",
        )
        .unwrap();

        let hits = grep_fallback_search(dir.path(), "intent classifier", 5);
        assert!(!hits.is_empty(), "expected at least one hit");
        let first = &hits[0];
        assert!(first["path"].as_str().unwrap().ends_with("notes.md"));
        assert_eq!(first["start_line"].as_u64().unwrap(), 2);
        assert!(
            first["snippet"]
                .as_str()
                .unwrap()
                .contains("intent classifier")
        );

        // Round-trip via the tool to confirm the fallback envelope wiring
        // (and that the legacy "search index not available" error is gone).
        let t = SearchCodeTool::new();
        let out = t
            .execute(json!({"query": "__no_match_token_zzz_213__", "top_n": 5}))
            .await;
        assert!(!out.is_error());
        let v: Value = serde_json::from_str(out.content()).unwrap();
        assert_eq!(v["fallback"], "grep");
        assert!(v["hits"].is_array());
        assert!(v["note"].is_string());
        assert!(v.get("error").is_none());
    }

    #[tokio::test]
    async fn search_code_returns_hits_from_indexer() {
        // Wire a real CodeIndexer over mock store + embedder, index one
        // Rust file, and assert the tool surfaces it via search.
        let dir = tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, "fn hello() { println!(\"hi\"); }\n").unwrap();

        let store: Arc<dyn MemoryStore> = Arc::new(MockStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
        let indexer = Arc::new(CodeIndexer::new(store, embedder));
        indexer.index_file(&file, None).await.unwrap();

        let tool = SearchCodeTool::with_indexer(indexer);
        let out = tool.execute(json!({"query": "hello", "top_n": 5})).await;
        assert!(!out.is_error());
        let v: Value = serde_json::from_str(out.content()).unwrap();
        let hits = v["hits"].as_array().unwrap();
        assert!(!hits.is_empty(), "expected at least one hit; got {v:?}");
        // Path + snippet should be present.
        assert!(hits[0]["path"].as_str().unwrap().ends_with("lib.rs"));
        assert!(hits[0]["snippet"].as_str().unwrap().contains("hello"));
    }

    // ------- search_memory tests -------

    #[tokio::test]
    async fn search_memory_degrades_gracefully() {
        let t = SearchMemoryTool::new();
        let out = t.execute(json!({"query": "decisions"})).await;
        assert!(!out.is_error());
        let v: Value = serde_json::from_str(out.content()).unwrap();
        assert!(v["hits"].is_array());
        assert_eq!(v["hits"].as_array().unwrap().len(), 0);
        assert!(v["error"].is_string());
    }

    #[tokio::test]
    async fn search_memory_errors_on_missing_query() {
        let t = SearchMemoryTool::new();
        assert!(t.execute(json!({})).await.is_error());
    }

    #[tokio::test]
    async fn search_memory_executes_with_graph() {
        let store: Arc<dyn MemoryStore> = Arc::new(MockStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
        let graph = Arc::new(MemoryGraph::new(store, embedder));
        let session = AgentSession {
            id: "s1".to_string(),
            agent_name: "engineer".to_string(),
            workflow_run_id: "r1".to_string(),
            phase: "build".to_string(),
            prompt: "write me a function".to_string(),
            response: "here is hello_world".to_string(),
            timestamp: chrono::Utc::now(),
            parent_id: None,
            segment: None,
        };
        graph.record(session).await.unwrap();

        let tool = SearchMemoryTool::with_graph(graph);
        let out = tool.execute(json!({"query": "hello"})).await;
        assert!(!out.is_error());
        let v: Value = serde_json::from_str(out.content()).unwrap();
        let hits = v["hits"].as_array().unwrap();
        assert!(!hits.is_empty(), "expected at least one hit; got {v:?}");
    }

    // ------- search_skills tests -------

    #[tokio::test]
    async fn search_skills_errors_on_missing_query() {
        let t = SearchSkillsTool::new();
        assert!(t.execute(json!({})).await.is_error());
    }

    #[tokio::test]
    async fn search_skills_executes_with_resolver() {
        let mut skills = HashMap::new();
        skills.insert(
            "rust-testing".to_string(),
            "# Rust Testing\nHow to write tests in Rust.".to_string(),
        );
        skills.insert(
            "python-packaging".to_string(),
            "# Python Packaging\nBuild wheels.".to_string(),
        );
        let resolver: Arc<dyn SkillResolver> = Arc::new(FakeSkillResolver { skills });
        let t = SearchSkillsTool::with_resolver(resolver);
        let out = t.execute(json!({"query": "rust"})).await;
        assert!(!out.is_error());
        let v: Value = serde_json::from_str(out.content()).unwrap();
        let hits = v["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["name"], "rust-testing");
        assert!(hits[0]["first_line"].as_str().unwrap().contains("Rust"));
    }

    #[tokio::test]
    async fn search_skills_scans_config_skills_dir() {
        // No resolver; point skills_dir at a tempdir and verify fs fallback.
        let dir = tempdir().unwrap();
        let a = dir.path().join("alpha.md");
        std::fs::write(&a, "# Alpha\nAlpha skill body mentions widgets.").unwrap();
        let b = dir.path().join("beta.md");
        std::fs::write(&b, "# Beta\nUnrelated content.").unwrap();

        let t = SearchSkillsTool::new().with_skills_dir(dir.path().to_path_buf());
        let out = t.execute(json!({"query": "widgets"})).await;
        assert!(!out.is_error());
        let v: Value = serde_json::from_str(out.content()).unwrap();
        let hits = v["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["name"], "alpha");
    }

    #[tokio::test]
    async fn search_skills_missing_dir_degrades_gracefully() {
        // No resolver and a skills dir that does not exist → error payload,
        // but the tool call itself still succeeds.
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let t = SearchSkillsTool::new().with_skills_dir(missing);
        let out = t.execute(json!({"query": "anything"})).await;
        assert!(!out.is_error());
        let v: Value = serde_json::from_str(out.content()).unwrap();
        assert!(v["error"].is_string());
        assert_eq!(v["hits"].as_array().unwrap().len(), 0);
    }
}
