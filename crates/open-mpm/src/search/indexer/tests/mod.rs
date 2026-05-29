//! Unit + integration tests for the AST code indexer.
//!
//! Why: Exercises AST extraction, markdown/fallback chunking, agentconfig
//! promotion, hybrid RRF ranking, KG expansion, and the warm-start /
//! cool-down lifecycle without requiring a real embedder or HNSW.
//! What: Shared mock `MemoryStore` + `Embedder` implementations plus three
//! topic submodules — [`chunker`] (pure extraction), [`lifecycle`]
//! (index/warm/cool-down + agentconfig), and [`ranking`] (vector/hybrid/KG
//! search + the bench).
//! Test: This *is* the test module.

mod chunker;
mod lifecycle;
mod ranking;

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;

use crate::memory::{Embedder, MemoryResult, MemoryStore, Segment};

/// Minimal in-memory store that records inserts and returns them in
/// insertion order on `search` (regardless of vector).
///
/// Why: Exercising AST extraction and payload serialization end-to-end
/// doesn't need a real HNSW; insertion order is deterministic and
/// lets tests assert on concrete results.
pub(super) struct MockStore {
    pub(super) inner: Mutex<HashMap<String, (Vec<f32>, Value)>>,
    pub(super) order: Mutex<Vec<String>>,
}
impl MockStore {
    pub(super) fn new() -> Self {
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
        self.order.lock().unwrap().push(id.to_string());
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
                    segment: "code".to_string(),
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

/// Deterministic fake embedder: maps each text to a fixed-length
/// vector where every element is `text.len() as f32 / 100.0`. Equal
/// inputs yield equal vectors, which is all the tests need.
pub(super) struct MockEmbedder {
    pub(super) dim: usize,
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
