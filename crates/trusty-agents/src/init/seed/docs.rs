//! `seed_documentation` — index `docs/{user,developer,design}/*.md` into memory.
//!
//! Why: Fresh installs benefit from having the project's own documentation
//! recallable via `memory_recall` from day one.
//! What: Extends `ProjectInitializer` with `seed_documentation`.
//! Test: `seeds_documentation_from_docs_dir`, `seed_documentation_uses_docs_seed_session`,
//! `seed_documentation_skips_when_docs_missing` in `init::tests`.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use chrono::Utc;

use super::file_mtime_secs;
use crate::init::{DOCS_SEED_SESSION_ID, ProjectInitializer};
use crate::memory::{Embedder, MemoryStore, Segment};

impl ProjectInitializer {
    /// Seed agent memory with the contents of `docs/user/` and `docs/developer/`.
    ///
    /// Why: Fresh installs of CTRL/workflow runs benefit from having the project's
    /// own user and developer documentation indexed in the agent memory store
    /// from day one — so `memory_recall` returns project-specific guidance
    /// without requiring explicit ingestion. We track per-file mtimes in
    /// `.trusty-agents/state/docs_seeded.json` so subsequent invocations only
    /// re-embed docs whose source file changed.
    /// What: Scans `<project>/docs/user/*.md` and `<project>/docs/developer/*.md`,
    /// embeds each file's text via the supplied `Embedder`, and inserts into
    /// `Segment::AgentMemory` with a stable id (`docs:<rel_path>`) and payload
    /// `{ "content": ..., "tag": "docs/<user|developer|design|other>", "path": ... }`. If the docs
    /// directory does not exist, returns Ok(0) and logs a debug line.
    /// Test: `seeds_documentation_from_docs_dir` in `init::tests`.
    pub async fn seed_documentation(
        &self,
        store: &dyn MemoryStore,
        embedder: &dyn Embedder,
    ) -> Result<usize> {
        let docs_root = self.project_dir.join("docs");
        if !docs_root.exists() {
            tracing::debug!(
                path = %docs_root.display(),
                "seed_documentation: docs/ not present, skipping"
            );
            return Ok(0);
        }

        // Load prior seed-state so we can skip unchanged files.
        let seeded_path = self.agent_dir.join("docs_seeded.json");
        let mut seeded: HashMap<String, u64> = match tokio::fs::read(&seeded_path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => HashMap::new(),
        };

        // Gather candidate doc paths from both subdirs.
        let mut candidates: Vec<PathBuf> = Vec::new();
        for sub in &["user", "developer", "design"] {
            let dir = docs_root.join(sub);
            if !dir.exists() {
                continue;
            }
            let mut rd = match tokio::fs::read_dir(&dir).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %dir.display(),
                        "seed_documentation: read_dir failed (skipping)"
                    );
                    continue;
                }
            };
            while let Some(entry) = rd.next_entry().await.ok().flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                candidates.push(path);
            }
        }
        candidates.sort();

        let mut seeded_count: usize = 0;
        for path in &candidates {
            let rel = path
                .strip_prefix(&self.project_dir)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            let mtime_secs = file_mtime_secs(path).await;

            // Skip if previously seeded with same-or-newer mtime.
            if let Some(prev) = seeded.get(&rel)
                && *prev >= mtime_secs
                && mtime_secs > 0
            {
                continue;
            }

            let content = match tokio::fs::read_to_string(path).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %path.display(),
                        "seed_documentation: read failed (skipping)"
                    );
                    continue;
                }
            };
            if content.trim().is_empty() {
                continue;
            }

            // Embed and insert. We move CPU-bound embedding off the async
            // executor via `spawn_blocking` is overkill here — fastembed
            // returns quickly per doc and we do them serially.
            let vec = match embedder.embed_single(&content) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %path.display(),
                        "seed_documentation: embed failed (skipping)"
                    );
                    continue;
                }
            };
            // Detect doc subtype from path so the hierarchical tag tree
            // (docs/user, docs/developer, docs/design) reflects the source dir.
            let tag = if rel.starts_with("docs/user") {
                "docs/user"
            } else if rel.starts_with("docs/developer") {
                "docs/developer"
            } else if rel.starts_with("docs/design") {
                "docs/design"
            } else {
                "docs/other"
            };
            let id = format!("docs:{rel}");
            let payload = serde_json::json!({
                "content": content,
                "tag": tag,
                "session_id": DOCS_SEED_SESSION_ID,
                "path": rel,
                "created_at": Utc::now().to_rfc3339(),
            });
            if let Err(e) = store.insert(Segment::AgentMemory, &id, &vec, payload).await {
                tracing::warn!(error = %e, id = %id, "seed_documentation: insert failed");
                continue;
            }

            seeded.insert(rel, mtime_secs);
            seeded_count += 1;
        }

        // Persist the updated seed-state so we can skip unchanged docs next run.
        if let Ok(bytes) = serde_json::to_vec_pretty(&seeded)
            && let Err(e) = tokio::fs::write(&seeded_path, &bytes).await
        {
            tracing::warn!(
                error = %e,
                path = %seeded_path.display(),
                "seed_documentation: write tracker failed (continuing)"
            );
        }

        // Update the initialized marker with seeded_docs count for inspection.
        let marker_path = self.agent_dir.join("initialized");
        if let Ok(bytes) = tokio::fs::read(&marker_path).await
            && let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(&bytes)
        {
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "seeded_docs".to_string(),
                    serde_json::Value::from(seeded.len()),
                );
            }
            let _ = tokio::fs::write(
                &marker_path,
                serde_json::to_vec_pretty(&v).unwrap_or_default(),
            )
            .await;
        }

        Ok(seeded_count)
    }
}
