//! Index/remove + warm-start/cool-down lifecycle tests, plus the
//! agentconfig promotion behaviour.
//!
//! Why: Covers the write path end-to-end (index a file, search it back) and
//! the #372 warm/evict contract that keeps the HNSW resident under load but
//! frees RAM when idle.
//! What: A full index+search round-trip, root-vs-subdir agentconfig
//! promotion, the agentconfig score boost, and the warm/cool-down monitor.
//! Test: This *is* the test module.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tempfile::NamedTempFile;

use super::{MockEmbedder, MockStore};
use crate::memory::{Embedder, MemoryResult, MemoryStore, Segment};
use crate::search::indexer::{CodeChunk, CodeIndexer};

// ---------- end-to-end test ----------

#[tokio::test]
async fn search_returns_code_chunk_with_metadata() {
    // Write a tiny Rust file, index it, search, and assert that the
    // top hit has the right file path + function name.
    let mut tmp = NamedTempFile::new().expect("tempfile");
    let src = "fn greet() {\n    println!(\"hello world\");\n}\n";
    tmp.write_all(src.as_bytes()).expect("write");
    // Rename to `.rs` so `detect_language` picks Rust. NamedTempFile
    // doesn't expose a rename, so we build a sibling path.
    let new_path = tmp.path().with_extension("rs");
    std::fs::copy(tmp.path(), &new_path).expect("copy to .rs");
    let _guard = scopeguard_for(&new_path);

    let store: Arc<dyn MemoryStore> = Arc::new(MockStore::new());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
    let indexer = CodeIndexer::new(Arc::clone(&store), Arc::clone(&embedder));

    let inserted = indexer
        .index_file(&new_path, None)
        .await
        .expect("index_file");
    assert_eq!(inserted, 1, "expected one chunk inserted");

    let hits = indexer.search("greet", 5).await.expect("search");
    assert_eq!(hits.len(), 1, "expected one hit");
    let hit = &hits[0];
    assert_eq!(hit.function_name.as_deref(), Some("greet"));
    assert_eq!(hit.language, "rust");
    assert_eq!(hit.start_line, 1);
    assert_eq!(hit.end_line, 3);
    // File path should be absolute (canonicalized).
    assert!(
        hit.file.is_absolute(),
        "expected absolute path, got {:?}",
        hit.file
    );
    assert!(hit.score > 0.0, "expected score > 0, got {}", hit.score);

    // Filter by language: rust passes, go excludes.
    let rust_only = indexer
        .search_filtered("greet", 5, Some("rust"))
        .await
        .expect("search_filtered rust");
    assert_eq!(rust_only.len(), 1);
    let go_only = indexer
        .search_filtered("greet", 5, Some("go"))
        .await
        .expect("search_filtered go");
    assert!(go_only.is_empty(), "expected no go hits");
}

// ---------- agentconfig promotion tests ----------

#[tokio::test]
async fn root_agents_md_gets_agentconfig_language() {
    // Why: Files named AGENTS.md at the *project root* should be
    // indexed as "agentconfig" so they can be boosted in search.
    let dir = tempfile::Builder::new()
        .prefix("agentcfg-")
        .tempdir()
        .expect("tempdir");
    let agents_md = dir.path().join("AGENTS.md");
    std::fs::write(
        &agents_md,
        "# Agents\n\n## Overview\n\nAgent instructions here.\n",
    )
    .expect("write AGENTS.md");

    let store = Arc::new(MockStore::new());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
    let indexer = CodeIndexer::new(store.clone(), Arc::clone(&embedder));

    let n = indexer
        .index_file(&agents_md, Some(dir.path()))
        .await
        .expect("index_file");
    assert!(n >= 1, "expected at least one chunk, got {n}");

    // Pull any stored CodeChunk payload and verify its language.
    let inner = store.inner.lock().unwrap();
    let saw_agentconfig = inner
        .iter()
        .filter(|(id, _)| !id.starts_with("manifest:"))
        .any(|(_, (_, payload))| {
            payload.get("language").and_then(|v| v.as_str()) == Some("agentconfig")
        });
    assert!(
        saw_agentconfig,
        "expected an agentconfig-language chunk to be stored"
    );
}

#[tokio::test]
async fn subdir_agents_md_stays_markdown() {
    // Why: Only *root-level* AGENTS.md is promoted. Nested copies in
    // subdirectories should remain plain markdown so they don't leak
    // into the boosted agentconfig bucket.
    let dir = tempfile::Builder::new()
        .prefix("agentcfg-sub-")
        .tempdir()
        .expect("tempdir");
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).expect("create subdir");
    let nested = sub.join("AGENTS.md");
    std::fs::write(&nested, "## Section\n\nNested content.\n").expect("write nested");

    let store = Arc::new(MockStore::new());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
    let indexer = CodeIndexer::new(store.clone(), Arc::clone(&embedder));

    let n = indexer
        .index_file(&nested, Some(dir.path()))
        .await
        .expect("index_file");
    assert!(n >= 1, "expected at least one chunk, got {n}");

    let inner = store.inner.lock().unwrap();
    let languages: Vec<String> = inner
        .iter()
        .filter(|(id, _)| !id.starts_with("manifest:"))
        .filter_map(|(_, (_, payload))| {
            payload
                .get("language")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    assert!(
        languages.iter().all(|l| l == "markdown"),
        "expected only markdown chunks for nested file, got {languages:?}"
    );
    assert!(
        !languages.is_empty(),
        "expected at least one markdown chunk"
    );
}

#[tokio::test]
async fn claude_md_at_root_gets_agentconfig() {
    // Why: CLAUDE.md at the project root is the other canonical
    // agent-config filename and must be promoted alongside AGENTS.md.
    let dir = tempfile::Builder::new()
        .prefix("agentcfg-claude-")
        .tempdir()
        .expect("tempdir");
    let claude_md = dir.path().join("CLAUDE.md");
    std::fs::write(&claude_md, "# Project\n\n## Goals\n\nClaude guidance.\n")
        .expect("write CLAUDE.md");

    let store = Arc::new(MockStore::new());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
    let indexer = CodeIndexer::new(store.clone(), Arc::clone(&embedder));

    indexer
        .index_file(&claude_md, Some(dir.path()))
        .await
        .expect("index_file");

    let inner = store.inner.lock().unwrap();
    let saw_agentconfig = inner
        .iter()
        .filter(|(id, _)| !id.starts_with("manifest:"))
        .any(|(_, (_, payload))| {
            payload.get("language").and_then(|v| v.as_str()) == Some("agentconfig")
        });
    assert!(
        saw_agentconfig,
        "expected CLAUDE.md at root to be indexed as agentconfig"
    );
}

#[tokio::test]
async fn agentconfig_score_boosted_in_search_results() {
    // Why: After deserialization, chunks with language == "agentconfig"
    // must have their score multiplied by 1.1 (capped at 1.0) and the
    // result set re-sorted so they rank above equal-raw-score markdown
    // siblings.
    //
    // Uses a custom mock store that returns a fixed pair of hits with
    // *equal* raw scores — one agentconfig, one markdown — regardless
    // of query vector. This isolates the boost+sort logic.
    struct BoostMockStore;
    #[async_trait]
    impl MemoryStore for BoostMockStore {
        async fn insert(&self, _: Segment, _: &str, _: &[f32], _: Value) -> anyhow::Result<()> {
            Ok(())
        }
        async fn search(
            &self,
            _: Segment,
            _: &[f32],
            _top_k: usize,
        ) -> anyhow::Result<Vec<MemoryResult>> {
            let md_chunk = CodeChunk {
                file: PathBuf::from("/tmp/readme.md"),
                function_name: Some("Readme".to_string()),
                start_line: 1,
                end_line: 3,
                language: "markdown".to_string(),
                score: 0.0,
                text: "# readme".to_string(),
                match_reason: String::new(),
            };
            let agent_chunk = CodeChunk {
                file: PathBuf::from("/tmp/AGENTS.md"),
                function_name: Some("Agents".to_string()),
                start_line: 1,
                end_line: 3,
                language: "agentconfig".to_string(),
                score: 0.0,
                text: "# agents".to_string(),
                match_reason: String::new(),
            };
            // Raw scores are equal; markdown comes first in the raw
            // order to prove the sort promotes agentconfig above it.
            Ok(vec![
                MemoryResult {
                    id: "md:1".to_string(),
                    score: 0.5,
                    segment: "code".to_string(),
                    payload: serde_json::to_value(&md_chunk).unwrap(),
                },
                MemoryResult {
                    id: "agents:1".to_string(),
                    score: 0.5,
                    segment: "code".to_string(),
                    payload: serde_json::to_value(&agent_chunk).unwrap(),
                },
            ])
        }
        async fn get(&self, _: Segment, _: &str) -> anyhow::Result<Option<Value>> {
            Ok(None)
        }
        async fn delete(&self, _: Segment, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    let store: Arc<dyn MemoryStore> = Arc::new(BoostMockStore);
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
    let indexer = CodeIndexer::new(store, embedder);

    let hits = indexer.search("agents", 5).await.expect("search");
    assert_eq!(hits.len(), 2, "expected two hits");
    assert_eq!(
        hits[0].language, "agentconfig",
        "agentconfig chunk should rank first after boost"
    );
    assert!(
        hits[0].score > hits[1].score,
        "boosted score {} should exceed unboosted {}",
        hits[0].score,
        hits[1].score
    );
    // 0.5 * 1.1 = 0.55, comfortably under the 1.0 cap.
    assert!(
        (hits[0].score - 0.55).abs() < 1e-5,
        "expected boosted score ~0.55, got {}",
        hits[0].score
    );
}

// ---------- warm-start / cool-down tests (#372) ----------

/// Mock store that tracks warm/evict state so tests can assert the
/// cool-down monitor and warm-up gate are actually invoked.
///
/// Why: The default trait impls of `warm_segment` / `evict_segment` are
/// no-ops; without a tracking mock we can't observe the calls the
/// `CodeIndexer` makes on behalf of #372. This mock flips a flag and
/// counts calls so each test asserts the exact behavior it cares about.
/// What: Wraps `MockStore` with `warm`, `warm_calls`, `evict_calls`
/// counters guarded by a `Mutex`. Forwards `insert`/`search`/`get`/
/// `delete` to an inner `MockStore` so existing chunk-storage paths
/// keep working.
struct WarmTrackingStore {
    inner: MockStore,
    warm: Mutex<bool>,
    warm_calls: Mutex<usize>,
    evict_calls: Mutex<usize>,
}
impl WarmTrackingStore {
    fn new() -> Self {
        Self {
            inner: MockStore::new(),
            warm: Mutex::new(true),
            warm_calls: Mutex::new(0),
            evict_calls: Mutex::new(0),
        }
    }
}
#[async_trait]
impl MemoryStore for WarmTrackingStore {
    async fn insert(
        &self,
        segment: Segment,
        id: &str,
        vector: &[f32],
        payload: Value,
    ) -> anyhow::Result<()> {
        self.inner.insert(segment, id, vector, payload).await
    }
    async fn search(
        &self,
        segment: Segment,
        query_vec: &[f32],
        top_k: usize,
    ) -> anyhow::Result<Vec<MemoryResult>> {
        self.inner.search(segment, query_vec, top_k).await
    }
    async fn get(&self, segment: Segment, id: &str) -> anyhow::Result<Option<Value>> {
        self.inner.get(segment, id).await
    }
    async fn delete(&self, segment: Segment, id: &str) -> anyhow::Result<()> {
        self.inner.delete(segment, id).await
    }
    async fn evict_segment(&self, _segment: Segment) -> anyhow::Result<()> {
        *self.warm.lock().unwrap() = false;
        *self.evict_calls.lock().unwrap() += 1;
        Ok(())
    }
    async fn warm_segment(&self, _segment: Segment) -> anyhow::Result<()> {
        *self.warm.lock().unwrap() = true;
        *self.warm_calls.lock().unwrap() += 1;
        Ok(())
    }
    async fn is_segment_warm(&self, _segment: Segment) -> anyhow::Result<bool> {
        Ok(*self.warm.lock().unwrap())
    }
}

#[tokio::test]
async fn warm_up_marks_segment_warm() {
    // Why: `warm_up()` is the eager pre-load entry point called from
    // `main()` so the first user query never pays a cold-start penalty.
    // It must call `store.warm_segment(CodeIndex)` exactly once and
    // refresh `last_access` so the cool-down clock starts from "load
    // completed", not from construction.
    let store = Arc::new(WarmTrackingStore::new());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
    let indexer = CodeIndexer::new(Arc::clone(&store) as Arc<dyn MemoryStore>, embedder);

    // Reset count: the segment starts "warm" by default but warm_up
    // should still call through (idempotent).
    *store.warm_calls.lock().unwrap() = 0;
    indexer.warm_up().await.expect("warm_up");
    assert_eq!(
        *store.warm_calls.lock().unwrap(),
        1,
        "warm_up should call store.warm_segment exactly once"
    );
    assert!(store.is_segment_warm(Segment::CodeIndex).await.unwrap());
}

#[tokio::test]
async fn cool_down_evicts_after_inactivity() {
    // Why: After `cool_after` of search inactivity, the background
    // monitor must call `evict_segment` to free RAM. We use a 50 ms
    // cool_after and a 10 ms tick so the test finishes in well under
    // a second.
    let store = Arc::new(WarmTrackingStore::new());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
    let indexer = Arc::new(
        CodeIndexer::new(Arc::clone(&store) as Arc<dyn MemoryStore>, embedder)
            .with_cool_after(Duration::from_millis(50)),
    );

    let handle = indexer.spawn_cool_down_monitor_with_tick(Duration::from_millis(10));

    // Wait long enough for at least one tick after the cool_after window.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let evict_calls = *store.evict_calls.lock().unwrap();
    assert!(
        evict_calls >= 1,
        "expected at least one evict_segment call, got {evict_calls}"
    );
    assert!(
        !store.is_segment_warm(Segment::CodeIndex).await.unwrap(),
        "segment should be evicted (not warm) after cool-down"
    );

    handle.abort();
}

#[tokio::test]
async fn search_warms_index_after_eviction() {
    // Why: After cool-down has evicted the in-memory HNSW, the next
    // `search()` call must transparently re-warm the index before
    // serving the query. Callers shouldn't see a difference.
    let store = Arc::new(WarmTrackingStore::new());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder { dim: 8 });
    let indexer = CodeIndexer::new(Arc::clone(&store) as Arc<dyn MemoryStore>, embedder);

    // Simulate eviction.
    store.evict_segment(Segment::CodeIndex).await.unwrap();
    assert!(!store.is_segment_warm(Segment::CodeIndex).await.unwrap());

    // Reset counters so we observe only the search-triggered warm.
    *store.warm_calls.lock().unwrap() = 0;
    let _ = indexer.search("anything", 3).await.expect("search");

    let warm_calls = *store.warm_calls.lock().unwrap();
    assert!(
        warm_calls >= 1,
        "search should call warm_segment at least once after eviction, got {warm_calls}"
    );
    assert!(
        store.is_segment_warm(Segment::CodeIndex).await.unwrap(),
        "segment should be warm again after search"
    );
}

/// Delete `path` on drop — a tiny hand-rolled scopeguard so we don't
/// pull in the `scopeguard` crate for one test.
fn scopeguard_for(path: &Path) -> impl Drop {
    struct G(PathBuf);
    impl Drop for G {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    G(path.to_path_buf())
}
