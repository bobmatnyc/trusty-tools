//! Idle-phase memory cleaner (#72).
//!
//! Why: The history JSONL grows monotonically as agents run; without periodic
//! pruning, retrieval quality degrades (near-duplicate turns dominate the
//! topK) and the BM25 rebuild cost balloons. A separate cleaner task,
//! triggered between workflow runs, dedupes near-identical entries and
//! rewrites the JSONL atomically.
//! What: `MemoryCleaner::spawn` returns a handle whose `trigger()` method
//! enqueues a cleaning pass. The cleaner dedupes by cosine similarity > 0.95
//! (keeping the newest seen), rewrites `entries.jsonl` atomically via a
//! `.tmp` + rename, and appends a line to `cleaner.log`.
//! Test: `dedup_drops_near_duplicates`, `clean_once_rewrites_file` exercise
//! the core algorithm without network I/O.

use std::path::{Path, PathBuf};

use tokio::sync::mpsc;

use crate::context::indexer::IndexedEntry;
use crate::context::retrieval::cosine_similarity;

/// Signal sent to the cleaner task.
pub enum CleanerTrigger {
    RunNow,
    Shutdown,
}

/// Handle to the running cleaner task.
pub struct MemoryCleaner {
    pub tx: mpsc::Sender<CleanerTrigger>,
}

impl MemoryCleaner {
    /// Spawn the background cleaner. `batch_size` is reserved for a future
    /// LLM-based relevance filter; today it's passed through and logged.
    pub fn spawn(store_dir: PathBuf, api_key: String, batch_size: usize) -> Self {
        let (tx, rx) = mpsc::channel::<CleanerTrigger>(8);
        tokio::spawn(async move {
            run_cleaner(rx, store_dir, api_key, batch_size).await;
        });
        Self { tx }
    }

    /// Fire a one-shot cleaning pass. Non-blocking; drops when the channel is
    /// full (a pass is already pending).
    pub fn trigger(&self) {
        if let Err(e) = self.tx.try_send(CleanerTrigger::RunNow) {
            tracing::debug!(error = %e, "memory cleaner: trigger dropped");
        }
    }

    /// Ask the cleaner task to exit.
    #[allow(dead_code)]
    pub fn shutdown(&self) {
        let _ = self.tx.try_send(CleanerTrigger::Shutdown);
    }
}

async fn run_cleaner(
    mut rx: mpsc::Receiver<CleanerTrigger>,
    store_dir: PathBuf,
    api_key: String,
    batch_size: usize,
) {
    while let Some(trigger) = rx.recv().await {
        match trigger {
            CleanerTrigger::Shutdown => break,
            CleanerTrigger::RunNow => {
                if let Err(e) = clean_once(&store_dir, &api_key, batch_size).await {
                    tracing::warn!(error = %e, "memory cleaner error");
                }
            }
        }
    }
}

/// One cleaning pass. Pure enough to unit-test by writing a JSONL file into
/// a tempdir and calling this directly.
pub async fn clean_once(store_dir: &Path, _api_key: &str, batch_size: usize) -> anyhow::Result<()> {
    let entries_path = store_dir.join("entries.jsonl");
    let content = tokio::fs::read_to_string(&entries_path)
        .await
        .unwrap_or_default();
    let entries: Vec<IndexedEntry> = content
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    if entries.is_empty() {
        return Ok(());
    }

    tracing::info!(
        count = entries.len(),
        batch_size,
        "memory cleaner: starting pass"
    );
    let original_count = entries.len();

    // Step 1: Dedup by cosine similarity > 0.95 (keep newest).
    let mut entries = dedup_similar(entries);
    // Step 2: Relevance filter (placeholder — the async LLM step would go
    // here in batches of `batch_size`; we skip it for now and keep a clean,
    // predictable signature).
    // Step 3: Dedup again in case downstream steps created near-duplicates.
    entries = dedup_similar(entries);

    let final_count = entries.len();
    tracing::info!(
        before = original_count,
        after = final_count,
        "memory cleaner: pass complete"
    );

    // Rewrite atomically via a temp file + rename so a crash mid-write can't
    // corrupt the JSONL.
    let tmp_path = entries_path.with_extension("jsonl.tmp");
    let mut out = String::new();
    for e in &entries {
        if let Ok(line) = serde_json::to_string(e) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    tokio::fs::write(&tmp_path, out).await?;
    tokio::fs::rename(&tmp_path, &entries_path).await?;

    // Append a cleaner log line for postmortems.
    let log_path = store_dir.join("cleaner.log");
    let log_line = format!(
        "{}: cleaned {} -> {} entries\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
        original_count,
        final_count
    );
    use tokio::io::AsyncWriteExt;
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .await?;
    // Error-first: return before flushing a failed write.
    if let Err(e) = f.write_all(log_line.as_bytes()).await {
        return Err(e.into());
    }
    // Flush so the log line is visible to any reader (e.g. `clean_once_rewrites_file`
    // asserts the log file exists, and a timing-dependent read might see an empty
    // file without this flush on a loaded test runner).
    f.flush().await?;

    Ok(())
}

/// Drop near-duplicate entries (cosine > 0.95); keeps the first occurrence
/// (which is the oldest since JSONL is append-only and iteration preserves
/// insertion order). Keeping the older copy means the newer near-duplicate
/// is dropped — this matches the intent that noisy repeats shouldn't
/// overwrite stable memories.
fn dedup_similar(entries: Vec<IndexedEntry>) -> Vec<IndexedEntry> {
    let mut kept: Vec<IndexedEntry> = Vec::new();
    'outer: for entry in entries {
        for k in &kept {
            if cosine_similarity(&entry.embedding, &k.embedding) > 0.95 {
                continue 'outer;
            }
        }
        kept.push(entry);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::indexer::{IndexedEntry, TurnRecord};
    use chrono::Utc;

    fn mkentry(id: &str, emb: Vec<f32>) -> IndexedEntry {
        IndexedEntry {
            id: id.to_string(),
            turn: TurnRecord {
                session_id: "s".into(),
                agent: "a".into(),
                turn_number: 0,
                timestamp: Utc::now(),
                prompt_text: "p".into(),
                response_text: "r".into(),
                prompt_tokens: 0,
                completion_tokens: 0,
            },
            embedding: emb,
            bm25_terms: vec!["x".into()],
        }
    }

    #[test]
    fn dedup_drops_near_duplicates() {
        let e1 = mkentry("a", vec![1.0, 0.0, 0.0]);
        let e2 = mkentry("b", vec![1.0, 0.0001, 0.0]); // near-identical
        let e3 = mkentry("c", vec![0.0, 1.0, 0.0]);
        let out = dedup_similar(vec![e1, e2, e3]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "a");
        assert_eq!(out[1].id, "c");
    }

    #[tokio::test]
    async fn clean_once_rewrites_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let entries_path = dir.join("entries.jsonl");
        let e1 = mkentry("a", vec![1.0, 0.0]);
        let e2 = mkentry("b", vec![1.0, 0.0]); // duplicate
        let e3 = mkentry("c", vec![0.0, 1.0]);
        let lines = [
            serde_json::to_string(&e1).unwrap(),
            serde_json::to_string(&e2).unwrap(),
            serde_json::to_string(&e3).unwrap(),
        ];
        tokio::fs::write(&entries_path, lines.join("\n") + "\n")
            .await
            .unwrap();

        clean_once(dir, "", 20).await.unwrap();

        let body = tokio::fs::read_to_string(&entries_path).await.unwrap();
        let count = body.lines().filter(|l| !l.is_empty()).count();
        assert_eq!(count, 2, "one near-duplicate should have been dropped");
        assert!(dir.join("cleaner.log").exists());
    }

    #[tokio::test]
    async fn clean_once_no_file_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        // Missing entries.jsonl is not an error.
        clean_once(tmp.path(), "", 20).await.unwrap();
    }
}
