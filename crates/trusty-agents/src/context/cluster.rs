//! Consolidated memory cluster store (#71).
//!
//! Why: After each successful memory search, the consolidation LLM produces a
//! synthesized paragraph summarizing the retrieved snippets. Persisting these
//! as "clusters" creates a positive feedback loop — next search surfaces them
//! first (via the 2x boost in the retriever) and they compress repeated
//! knowledge into fewer, richer hits.
//! What: `ClusterStore` appends JSONL records to `clusters.jsonl` under the
//! history store dir and can load them all back for the retriever.
//! Test: `cluster_save_and_load_roundtrip`.

use std::path::{Path, PathBuf};

use chrono::Utc;
use tokio::io::AsyncWriteExt;

use super::indexer::{IndexedEntry, TurnRecord, tokenize};

/// Append-only JSONL store for consolidated memory clusters.
///
/// # Intent
/// Separates cluster storage from the primary turn index so the retriever can
/// apply a distinct `cluster_boost` weight without having to filter the main
/// entries file. Append-only semantics mean we never rewrite old clusters —
/// deletion, if needed, is a separate pass.
///
/// Test: `cluster_save_and_load_roundtrip`, `cluster_load_missing_returns_empty`.
pub struct ClusterStore {
    path: PathBuf,
}

impl ClusterStore {
    /// Create a handle for `<store_dir>/clusters.jsonl`. Does not touch disk.
    pub fn new(store_dir: &Path) -> Self {
        Self {
            path: store_dir.join("clusters.jsonl"),
        }
    }

    /// Append a consolidated summary as a new cluster entry.
    ///
    /// Why: The synthesized text is what we want surfaced; storing its
    /// embedding alongside allows future retrieval without re-embedding.
    /// What: Wraps `summary + embedding` in an `IndexedEntry` with a synthetic
    /// `TurnRecord` (agent = "consolidator"). Appends one JSONL line, then
    /// flushes to ensure bytes are visible to subsequent `load_all` calls in
    /// the same process. Tokio's `File` uses `spawn_blocking` internally so
    /// `write_all` can resolve before the kernel has committed the buffer;
    /// without `flush`, a read-back in the same task (or a different one) can
    /// observe an empty file — the same race fixed in PR #532 for `append_usage`.
    /// Test: `cluster_save_and_load_roundtrip` (verified stable across ≥5 runs).
    pub async fn save(&self, summary: String, embedding: Vec<f32>) -> anyhow::Result<()> {
        let entry = IndexedEntry {
            id: uuid::Uuid::new_v4().to_string(),
            bm25_terms: tokenize(&summary),
            embedding,
            turn: TurnRecord {
                session_id: "cluster".to_string(),
                agent: "consolidator".to_string(),
                turn_number: 0,
                timestamp: Utc::now(),
                prompt_text: String::new(),
                response_text: summary,
                prompt_tokens: 0,
                completion_tokens: 0,
            },
        };
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let line = serde_json::to_string(&entry)?;
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        if let Err(e) = f.write_all(format!("{line}\n").as_bytes()).await {
            return Err(e.into());
        }
        f.flush().await?;
        Ok(())
    }

    /// Load all cluster entries from disk; silently returns `[]` when the
    /// file is missing.
    pub async fn load_all(&self) -> Vec<IndexedEntry> {
        let content = tokio::fs::read_to_string(&self.path)
            .await
            .unwrap_or_default();
        content
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cluster_save_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ClusterStore::new(tmp.path());
        store
            .save("hello world cluster".into(), vec![0.1, 0.2, 0.3])
            .await
            .unwrap();
        let loaded = store.load_all().await;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].turn.response_text, "hello world cluster");
        assert_eq!(loaded[0].embedding, vec![0.1, 0.2, 0.3]);
    }

    #[tokio::test]
    async fn cluster_load_missing_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ClusterStore::new(tmp.path());
        assert!(store.load_all().await.is_empty());
    }
}
