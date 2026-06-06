//! `seed_skills` — index `.trusty-agents/skills/**/*.md` (+ user dirs) into memory.
//!
//! Why: Skills are prompt fragments that agents should be able to recall
//! semantically via `memory_recall` rather than having every skill injected
//! into every prompt.
//! What: Extends `ProjectInitializer` with `seed_skills`.
//! Test: `seed_skills_indexes_skill_files`,
//! `seed_skills_falls_back_to_filename_without_frontmatter`,
//! `seed_skills_skips_when_dirs_missing` in `init::tests`.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use chrono::Utc;

use super::{
    SKILL_SEED_MAX_PER_SOURCE, collect_md_files, file_mtime_secs, parse_skill_frontmatter,
};
use crate::init::{ProjectInitializer, SKILLS_SEED_SESSION_ID};
use crate::memory::{Embedder, MemoryStore, Segment};

impl ProjectInitializer {
    /// Seed agent memory with skill markdown files.
    ///
    /// Why: Skills are project- and user-level prompt fragments (FastAPI
    /// patterns, pytest fixtures, git workflows…). Indexing them into the
    /// memory store lets agents semantically recall the right skill via
    /// `memory_recall` without needing the harness to inject every skill into
    /// every prompt. We track per-file mtimes in
    /// `.trusty-agents/state/skills_seeded.json` so subsequent invocations only
    /// re-embed skills whose source file changed.
    /// What: Scans `.trusty-agents/skills/**/*.md` in the project dir (and
    /// `~/.trusty-agents/skills/` and `~/.claude/skills/` when present, subject to
    /// the same per-source bound the registry enforces), parses YAML
    /// frontmatter to extract `name`/`tags`/`summary`/`description`, embeds
    /// the full markdown content, and inserts into `Segment::AgentMemory`
    /// with a stable id (`skill:<skill_name>`) and payload
    /// `{ "content": ..., "tag": "configuration/skill", "session_id": "seed/configuration/skills",
    ///    "skill_name": ..., "skill_tags": [...], "path": ... }`. Skill files
    /// without a `name` frontmatter field fall back to the relative path
    /// stem so we still index them rather than silently dropping them.
    /// Test: `seed_skills_indexes_skill_files` in `init::tests`.
    pub async fn seed_skills(
        &self,
        store: &dyn MemoryStore,
        embedder: &dyn Embedder,
    ) -> Result<usize> {
        // Build the search-path list. Project-local always first; user-level
        // dirs are only added when present. We deliberately don't pull in the
        // bundled `<config_dir>/skills` path here — `seed_skills` is for the
        // *project's* runtime memory, not the harness's bundled defaults.
        let mut search_paths: Vec<PathBuf> = Vec::new();
        let project_skills = self.project_dir.join(".trusty-agents").join("skills");
        if project_skills.exists() {
            search_paths.push(project_skills);
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            for sub in &[".trusty-agents/skills", ".claude/skills"] {
                let p = home.join(sub);
                if p.exists() {
                    search_paths.push(p);
                }
            }
        }
        if search_paths.is_empty() {
            tracing::debug!("seed_skills: no skill dirs present, skipping");
            return Ok(0);
        }

        // Load prior seed-state.
        let seeded_path = self.agent_dir.join("skills_seeded.json");
        let mut seeded: HashMap<String, u64> = match tokio::fs::read(&seeded_path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => HashMap::new(),
        };

        // Gather every .md file across the search paths, bounded per source.
        let mut candidates: Vec<PathBuf> = Vec::new();
        for root in &search_paths {
            let mut count = 0usize;
            collect_md_files(root, &mut candidates, &mut count, SKILL_SEED_MAX_PER_SOURCE).await;
        }
        candidates.sort();
        candidates.dedup();

        let mut seeded_count: usize = 0;
        for path in &candidates {
            let rel_for_id = path
                .strip_prefix(&self.project_dir)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            let mtime_secs = file_mtime_secs(path).await;

            if let Some(prev) = seeded.get(&rel_for_id)
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
                        "seed_skills: read failed (skipping)"
                    );
                    continue;
                }
            };
            if content.trim().is_empty() {
                continue;
            }

            let (skill_name, skill_tags, summary) = parse_skill_frontmatter(&content)
                .unwrap_or_else(|| {
                    // Fall back to the file stem so we still index frontmatter-less
                    // skill files rather than silently dropping them.
                    let stem = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    (stem, Vec::new(), String::new())
                });

            let vec = match embedder.embed_single(&content) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %path.display(),
                        "seed_skills: embed failed (skipping)"
                    );
                    continue;
                }
            };
            let id = format!("skill:{skill_name}");
            let mut payload = serde_json::json!({
                "content": content,
                "tag": "configuration/skill",
                "session_id": SKILLS_SEED_SESSION_ID,
                "skill_name": skill_name,
                "skill_tags": skill_tags,
                "path": rel_for_id.clone(),
                "created_at": Utc::now().to_rfc3339(),
            });
            if !summary.is_empty()
                && let Some(obj) = payload.as_object_mut()
            {
                obj.insert("summary".to_string(), serde_json::Value::String(summary));
            }
            if let Err(e) = store.insert(Segment::AgentMemory, &id, &vec, payload).await {
                tracing::warn!(error = %e, id = %id, "seed_skills: insert failed");
                continue;
            }

            seeded.insert(rel_for_id, mtime_secs);
            seeded_count += 1;
        }

        if let Ok(bytes) = serde_json::to_vec_pretty(&seeded)
            && let Err(e) = tokio::fs::write(&seeded_path, &bytes).await
        {
            tracing::warn!(
                error = %e,
                path = %seeded_path.display(),
                "seed_skills: write tracker failed (continuing)"
            );
        }

        Ok(seeded_count)
    }
}
