//! Project self-initialization: auto-index + memory seeding.
//!
//! Why: Fresh workflow runs benefit from project-specific context (file layout,
//! architecture notes, prior decisions) without requiring a human to hand-write
//! a system prompt. On first invocation per project, we scan the working tree
//! to produce a compact Markdown index. The result gets injected as a prefix
//! into every agent phase so downstream LLM calls start with "here is the
//! project you're working on" already in context.
//!
//! Note: This module previously also slurped Markdown artifacts from
//! `.kuzu-memory/` directories produced by the now-archived KùzuDB Python
//! shim. That path was removed when memory_recall was wired to the embedded
//! redb + usearch store; relevant memories are now retrieved on demand via
//! `MemoryRecallTool` rather than batch-injected into every prompt prefix.
//!
//! What: `ProjectInitializer` takes a project root + the `.open-mpm/state/`
//! runtime-state directory. `initialize_if_needed` checks the `initialized`
//! marker file — if present and fresh (<24h), it rehydrates `InitContext` from
//! disk; else it runs `scan_project`, writes `.open-mpm/state/project-index.md`
//! and `.open-mpm/state/initialized`, and returns the fresh `InitContext`.
//!
//! Test: See unit tests below — scan picks up source files, index write/read
//! is idempotent, marker freshness is honored, force path re-runs even when
//! fresh.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::memory::{Embedder, MemoryStore, Segment};

/// Session id used to tag documentation memories seeded by `seed_documentation`.
///
/// Why: Doc payloads need a stable, identifiable session_id so they can be
/// distinguished from agent-run memories during recall, export, and cleanup.
/// What: A constant string ("seed/docs") stamped into every seeded payload.
/// Test: `seed_documentation_uses_docs_seed_session` in tests below.
pub const DOCS_SEED_SESSION_ID: &str = "seed/docs";

/// Session id used to tag skill memories seeded by `seed_skills`.
///
/// Why: Skill payloads need a stable, identifiable session_id so they can
/// be filtered/exported separately from doc and agent-run memories.
/// What: A constant string ("seed/configuration/skills") stamped into every
/// seeded payload.
/// Test: `seed_skills_indexes_skill_files` in tests below.
pub const SKILLS_SEED_SESSION_ID: &str = "seed/configuration/skills";

/// Session id used to tag MCP-connection memories seeded by
/// `seed_mcp_connections`.
///
/// Why: MCP-connection payloads need a stable, identifiable session_id so
/// they can be filtered/exported separately from doc, skill, and agent-run
/// memories.
/// What: A constant string ("seed/configuration/mcp") stamped into every
/// seeded payload.
/// Test: `seed_mcp_connections_indexes_servers` in tests below.
pub const MCP_SEED_SESSION_ID: &str = "seed/configuration/mcp";

/// Per-source skill scan budget — bounds how many `.md` files we read from
/// a single search path during seeding.
///
/// Why: Mirrors `crate::skills::registry::MAX_SKILLS_PER_SOURCE` so a giant
/// external skills tree (e.g. claude-mpm's 700+-file `~/.claude/skills/`)
/// can't make seed_skills hang on cold-cache disks.
const SKILL_SEED_MAX_PER_SOURCE: usize = 50;

/// Budget: cap lines read per source file when building the index summary.
/// 50 is enough to capture module-level docstrings and the first few items.
const INDEX_LINES_PER_FILE: usize = 50;

/// Index-scan depth: only walk 2 levels deep (root + one subdir).
/// Keeps scan cheap on large trees; deeper layout is summarized per-dir.
const INDEX_MAX_DEPTH: usize = 2;

/// Marker TTL: re-init if marker is older than this (24h).
const MARKER_TTL_HOURS: i64 = 24;

/// Filename patterns we pull into the index.
const INCLUDED_EXTS: &[&str] = &["rs", "toml", "json", "md"];

/// Injected context returned from `initialize_if_needed`.
///
/// Why: The workflow engine wants a single struct to prepend to every phase
/// prompt; splitting into `project_summary` + `relevant_memories` lets the
/// engine format them distinctly (different headers).
/// What: Holds the Markdown project index, a list of memory snippet strings,
/// and when init ran.
/// Test: `initialize_if_needed_creates_index_and_marker`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitContext {
    pub project_summary: String,
    pub relevant_memories: Vec<String>,
    pub initialized_at: DateTime<Utc>,
}

impl InitContext {
    /// Render as a single prompt-ready prefix string.
    ///
    /// Why: Workflow engine wants to splice one block at the top of each
    /// phase's rendered template. Centralizing format here keeps headings
    /// stable across agents.
    /// What: Returns `## Project Context (auto-indexed)\n...\n## Relevant
    /// Prior Knowledge\n...\n\n---\n\n` or empty string if both fields empty.
    /// Test: `prompt_prefix_includes_both_sections` below.
    pub fn to_prompt_prefix(&self) -> String {
        if self.project_summary.is_empty() && self.relevant_memories.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        if !self.project_summary.is_empty() {
            out.push_str("## Project Context (auto-indexed)\n\n");
            out.push_str(self.project_summary.trim());
            out.push_str("\n\n");
        }
        if !self.relevant_memories.is_empty() {
            out.push_str("## Relevant Prior Knowledge\n\n");
            for m in &self.relevant_memories {
                out.push_str("- ");
                out.push_str(m.trim());
                out.push('\n');
            }
            out.push('\n');
        }
        out.push_str("---\n\n");
        out
    }
}

/// Persistent summary stored in `.open-mpm/initialized`.
///
/// Why: Separate from `InitContext` so the on-disk schema is small/stable —
/// we only need to persist what's cheap to recompute on reload.
/// What: Timestamp + a short project fingerprint (root name + file count).
/// Test: Parsed in `initialize_if_needed_skips_when_fresh`.
#[derive(Debug, Serialize, Deserialize)]
struct InitializedMarker {
    initialized_at: DateTime<Utc>,
    project_name: String,
    file_count: usize,
}

/// Intermediate value passed from `scan_project` to `write_index`.
///
/// Why: Separating scan from write keeps `write_index` side-effect-free for
/// unit tests, and lets callers inspect scan results before committing.
/// What: Canonical project name + per-file summaries.
/// Test: `scan_project_finds_source_files`.
#[derive(Debug, Clone)]
pub struct ProjectIndex {
    pub project_name: String,
    pub entries: Vec<IndexEntry>,
}

/// One indexed file with a short summary.
///
/// Why: Named struct (vs tuple) makes the Markdown emit more readable.
/// What: Relative path + single-line summary extracted from the file head.
/// Test: Exercised via `scan_project_finds_source_files`.
#[derive(Debug, Clone)]
pub struct IndexEntry {
    pub rel_path: String,
    pub summary: String,
}

/// Project self-initializer.
///
/// Why: Encapsulates the "first run of workflow → scan project + seed
/// memory" step so `main.rs` only has to call one method. Keeps the state
/// dir (`.open-mpm/`) and the project root separate so tests can point them
/// both at tempdirs.
/// What: Holds both paths; provides `initialize_if_needed` (idempotent) and
/// `force_reinitialize` (always re-runs).
/// Test: See all unit tests below.
pub struct ProjectInitializer {
    project_dir: PathBuf,
    open_mpm_dir: PathBuf,
}

impl ProjectInitializer {
    /// Construct from a project root and the companion state dir.
    pub fn new(project_dir: impl Into<PathBuf>, open_mpm_dir: impl Into<PathBuf>) -> Self {
        Self {
            project_dir: project_dir.into(),
            open_mpm_dir: open_mpm_dir.into(),
        }
    }

    /// Initialize the project if we haven't recently.
    ///
    /// Why: Calling on every workflow run should be cheap — the marker lets
    /// us skip the walk when we ran within the last 24h. A stale or missing
    /// marker triggers a full re-scan.
    /// What: Reads `.open-mpm/initialized`; if fresh (< `MARKER_TTL_HOURS`),
    /// loads the cached index + memories. Otherwise runs the full scan,
    /// writes both the index and a fresh marker, and returns the new context.
    /// Test: `initialize_if_needed_creates_index_and_marker` and
    /// `initialize_if_needed_skips_when_fresh`.
    pub async fn initialize_if_needed(&self) -> Result<InitContext> {
        tokio::fs::create_dir_all(&self.open_mpm_dir)
            .await
            .with_context(|| {
                format!(
                    "failed to create open-mpm dir: {}",
                    self.open_mpm_dir.display()
                )
            })?;

        if let Some(fresh) = self.read_cached_context().await? {
            tracing::debug!("project init: reusing fresh cached index");
            return Ok(fresh);
        }

        self.do_initialize().await
    }

    /// Always re-run initialization, ignoring any existing marker.
    ///
    /// Why: `--reinit` CLI flag path; also useful if callers detect the
    /// project changed substantially.
    /// What: Skips the marker check and always runs `do_initialize`.
    /// Test: `force_reinitialize_rewrites_index`.
    pub async fn force_reinitialize(&self) -> Result<InitContext> {
        tokio::fs::create_dir_all(&self.open_mpm_dir).await?;
        self.do_initialize().await
    }

    /// Seed agent memory with the contents of `docs/user/` and `docs/developer/`.
    ///
    /// Why: Fresh installs of CTRL/workflow runs benefit from having the project's
    /// own user and developer documentation indexed in the agent memory store
    /// from day one — so `memory_recall` returns project-specific guidance
    /// without requiring explicit ingestion. We track per-file mtimes in
    /// `.open-mpm/state/docs_seeded.json` so subsequent invocations only
    /// re-embed docs whose source file changed.
    /// What: Scans `<project>/docs/user/*.md` and `<project>/docs/developer/*.md`,
    /// embeds each file's text via the supplied `Embedder`, and inserts into
    /// `Segment::AgentMemory` with a stable id (`docs:<rel_path>`) and payload
    /// `{ "content": ..., "tag": "docs/<user|developer|design|other>", "path": ... }`. If the docs
    /// directory does not exist, returns Ok(0) and logs a debug line.
    /// Test: `seeds_documentation_from_docs_dir` below.
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
        let seeded_path = self.open_mpm_dir.join("docs_seeded.json");
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
            let mtime_secs = match tokio::fs::metadata(path).await {
                Ok(m) => m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                Err(_) => 0,
            };

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
        let marker_path = self.open_mpm_dir.join("initialized");
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

    /// Seed agent memory with skill markdown files.
    ///
    /// Why: Skills are project- and user-level prompt fragments (FastAPI
    /// patterns, pytest fixtures, git workflows…). Indexing them into the
    /// memory store lets agents semantically recall the right skill via
    /// `memory_recall` without needing the harness to inject every skill into
    /// every prompt. We track per-file mtimes in
    /// `.open-mpm/state/skills_seeded.json` so subsequent invocations only
    /// re-embed skills whose source file changed.
    /// What: Scans `.open-mpm/skills/**/*.md` in the project dir (and
    /// `~/.open-mpm/skills/` and `~/.claude/skills/` when present, subject to
    /// the same per-source bound the registry enforces), parses YAML
    /// frontmatter to extract `name`/`tags`/`summary`/`description`, embeds
    /// the full markdown content, and inserts into `Segment::AgentMemory`
    /// with a stable id (`skill:<skill_name>`) and payload
    /// `{ "content": ..., "tag": "configuration/skill", "session_id": "seed/configuration/skills",
    ///    "skill_name": ..., "skill_tags": [...], "path": ... }`. Skill files
    /// without a `name` frontmatter field fall back to the relative path
    /// stem so we still index them rather than silently dropping them.
    /// Test: `seed_skills_indexes_skill_files` below.
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
        let project_skills = self.project_dir.join(".open-mpm").join("skills");
        if project_skills.exists() {
            search_paths.push(project_skills);
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            for sub in &[".open-mpm/skills", ".claude/skills"] {
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
        let seeded_path = self.open_mpm_dir.join("skills_seeded.json");
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
            let mtime_secs = match tokio::fs::metadata(path).await {
                Ok(m) => m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                Err(_) => 0,
            };

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

    /// Seed agent memory with MCP server connection definitions.
    ///
    /// Why: Agents using `memory_recall` should be able to ask "what MCP
    /// servers are available?" or "is there a server that can search vector
    /// databases?" and get the right hit. Indexing the MCP config gives them
    /// that visibility without bespoke tooling.
    /// What: Reads `.mcp.json` from the project root (and `~/.claude/.mcp.json`
    /// if present), parses the `mcpServers` map, and for each server builds a
    /// human-readable description embedding command/args/env. Stored in
    /// `Segment::AgentMemory` with a stable id (`mcp:<server_name>`) and
    /// payload `{ "content": ..., "tag": "configuration/mcp",
    /// "session_id": "seed/configuration/mcp", "server_name": ..., "command": ...,
    /// "path": ... }`. Tracked in `.open-mpm/state/mcp_seeded.json`.
    /// Test: `seed_mcp_connections_indexes_servers` below.
    pub async fn seed_mcp_connections(
        &self,
        store: &dyn MemoryStore,
        embedder: &dyn Embedder,
    ) -> Result<usize> {
        let mut sources: Vec<PathBuf> = Vec::new();
        let project_mcp = self.project_dir.join(".mcp.json");
        if project_mcp.exists() {
            sources.push(project_mcp);
        }
        if let Some(home) = std::env::var_os("HOME") {
            let user_mcp = PathBuf::from(home).join(".claude").join(".mcp.json");
            if user_mcp.exists() {
                sources.push(user_mcp);
            }
        }
        if sources.is_empty() {
            tracing::debug!("seed_mcp_connections: no .mcp.json present, skipping");
            return Ok(0);
        }

        let seeded_path = self.open_mpm_dir.join("mcp_seeded.json");
        let mut seeded: HashMap<String, u64> = match tokio::fs::read(&seeded_path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => HashMap::new(),
        };

        let mut seeded_count: usize = 0;
        for source in &sources {
            let mtime_secs = match tokio::fs::metadata(source).await {
                Ok(m) => m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                Err(_) => 0,
            };

            let raw = match tokio::fs::read_to_string(source).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %source.display(),
                        "seed_mcp_connections: read failed (skipping)"
                    );
                    continue;
                }
            };
            let parsed: serde_json::Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %source.display(),
                        "seed_mcp_connections: parse failed (skipping)"
                    );
                    continue;
                }
            };

            let servers = match parsed.get("mcpServers").and_then(|v| v.as_object()) {
                Some(s) => s,
                None => {
                    tracing::debug!(
                        path = %source.display(),
                        "seed_mcp_connections: no mcpServers key, skipping"
                    );
                    continue;
                }
            };

            let rel = source
                .strip_prefix(&self.project_dir)
                .unwrap_or(source)
                .to_string_lossy()
                .to_string();

            for (server_name, def) in servers {
                let track_key = format!("{rel}#{server_name}");
                if let Some(prev) = seeded.get(&track_key)
                    && *prev >= mtime_secs
                    && mtime_secs > 0
                {
                    continue;
                }

                let command = def
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let args: Vec<String> = def
                    .get("args")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let description = def
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let env_keys: Vec<String> = def
                    .get("env")
                    .and_then(|v| v.as_object())
                    .map(|o| o.keys().cloned().collect())
                    .unwrap_or_default();

                let content =
                    render_mcp_description(server_name, &command, &args, &description, &env_keys);

                let vec = match embedder.embed_single(&content) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            server = %server_name,
                            "seed_mcp_connections: embed failed (skipping)"
                        );
                        continue;
                    }
                };
                let id = format!("mcp:{server_name}");
                let payload = serde_json::json!({
                    "content": content,
                    "tag": "configuration/mcp",
                    "session_id": MCP_SEED_SESSION_ID,
                    "server_name": server_name,
                    "command": command,
                    "args": args,
                    "env_keys": env_keys,
                    "path": rel.clone(),
                    "created_at": Utc::now().to_rfc3339(),
                });
                if let Err(e) = store.insert(Segment::AgentMemory, &id, &vec, payload).await {
                    tracing::warn!(error = %e, id = %id, "seed_mcp_connections: insert failed");
                    continue;
                }

                seeded.insert(track_key, mtime_secs);
                seeded_count += 1;
            }
        }

        if let Ok(bytes) = serde_json::to_vec_pretty(&seeded)
            && let Err(e) = tokio::fs::write(&seeded_path, &bytes).await
        {
            tracing::warn!(
                error = %e,
                path = %seeded_path.display(),
                "seed_mcp_connections: write tracker failed (continuing)"
            );
        }

        Ok(seeded_count)
    }

    /// Run all memory seeders (docs + skills + MCP) in sequence and report.
    ///
    /// Why: Callers (CTRL startup, workflow runner) want one entrypoint that
    /// guarantees every structured-context source is indexed. Centralizing
    /// the trio also gives us one consistent log line per startup.
    /// What: Calls `seed_documentation`, `seed_skills`, and
    /// `seed_mcp_connections` in order. Each is best-effort: a failure in
    /// one stage logs a WARN and the remaining stages still run. Returns a
    /// `(docs, skills, mcp)` tuple of seeded counts. Emits a combined
    /// `[open-mpm] Memory seeded: N docs, N skills, N MCP connections` line
    /// to stderr on success.
    /// Test: `seed_all_runs_all_three_seeders` below.
    pub async fn seed_all(
        &self,
        store: &dyn MemoryStore,
        embedder: &dyn Embedder,
    ) -> (usize, usize, usize) {
        let docs = match self.seed_documentation(store, embedder).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "seed_all: docs failed (continuing)");
                0
            }
        };
        let skills = match self.seed_skills(store, embedder).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "seed_all: skills failed (continuing)");
                0
            }
        };
        let mcp = match self.seed_mcp_connections(store, embedder).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "seed_all: mcp failed (continuing)");
                0
            }
        };
        eprintln!(
            "\r[open-mpm] Memory seeded: {docs} docs, {skills} skills, {mcp} MCP connections"
        );
        (docs, skills, mcp)
    }

    async fn do_initialize(&self) -> Result<InitContext> {
        let index = self.scan_project().await?;
        self.write_index(&index).await?;

        // `relevant_memories` is preserved as a field for backwards
        // compatibility with `to_prompt_prefix`, but it's no longer
        // populated at init time. Agents pull relevant memories on demand
        // via the `memory_recall` tool against the embedded store.
        let memories: Vec<String> = Vec::new();

        let now = Utc::now();
        let marker = InitializedMarker {
            initialized_at: now,
            project_name: index.project_name.clone(),
            file_count: index.entries.len(),
        };
        let marker_path = self.open_mpm_dir.join("initialized");
        let bytes = serde_json::to_vec_pretty(&marker)?;
        tokio::fs::write(&marker_path, &bytes)
            .await
            .with_context(|| format!("failed to write {}", marker_path.display()))?;

        let project_summary = render_index_markdown(&index, now);
        Ok(InitContext {
            project_summary,
            relevant_memories: memories,
            initialized_at: now,
        })
    }

    /// Try to rehydrate an `InitContext` from the cached index + marker.
    ///
    /// Returns `Ok(Some(...))` if the marker exists and is fresh; `Ok(None)`
    /// otherwise. Errors only on I/O problems that shouldn't be swallowed.
    async fn read_cached_context(&self) -> Result<Option<InitContext>> {
        let marker_path = self.open_mpm_dir.join("initialized");
        let index_path = self.open_mpm_dir.join("project-index.md");
        if !marker_path.exists() || !index_path.exists() {
            return Ok(None);
        }
        let bytes = match tokio::fs::read(&marker_path).await {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        let marker: InitializedMarker = match serde_json::from_slice(&bytes) {
            Ok(m) => m,
            Err(_) => return Ok(None),
        };
        let age = Utc::now() - marker.initialized_at;
        if age > Duration::hours(MARKER_TTL_HOURS) {
            return Ok(None);
        }
        let project_summary = tokio::fs::read_to_string(&index_path)
            .await
            .unwrap_or_default();
        // Memories are retrieved on demand via `memory_recall`, not
        // injected at init time anymore.
        Ok(Some(InitContext {
            project_summary,
            relevant_memories: Vec::new(),
            initialized_at: marker.initialized_at,
        }))
    }

    /// Scan the project tree up to 2 levels deep, collect file summaries.
    ///
    /// Why: Kept public-ish (`pub(crate)` via `pub`) so tests can drive it
    /// directly without running the full init.
    /// What: Walks the project dir (excluding common noise like `.git`,
    /// `target/`, `node_modules/`, `.venv/`), reads the first
    /// `INDEX_LINES_PER_FILE` lines of each included file, derives a summary.
    /// Test: `scan_project_finds_source_files`.
    pub async fn scan_project(&self) -> Result<ProjectIndex> {
        let project_name = self
            .project_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("project")
            .to_string();

        let mut entries: Vec<IndexEntry> = Vec::new();
        walk_dir(&self.project_dir, &self.project_dir, 0, &mut entries).await?;
        // Deterministic order so the written index is stable across runs.
        entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        Ok(ProjectIndex {
            project_name,
            entries,
        })
    }

    /// Emit the index Markdown to `.open-mpm/project-index.md`.
    pub async fn write_index(&self, index: &ProjectIndex) -> Result<()> {
        let path = self.open_mpm_dir.join("project-index.md");
        let md = render_index_markdown(index, Utc::now());
        tokio::fs::write(&path, md.as_bytes())
            .await
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }
}

/// Render a `ProjectIndex` as Markdown.
///
/// Why: Kept as a free function (not method) so both the live write path and
/// tests can call it without instantiating a `ProjectInitializer`.
/// What: Produces sections for Source Structure / Config / Docs.
/// Test: `render_markdown_groups_by_kind`.
fn render_index_markdown(index: &ProjectIndex, at: DateTime<Utc>) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Project: {}\n", index.project_name));
    out.push_str(&format!("Indexed: {}\n\n", at.format("%Y-%m-%d")));

    let mut source: Vec<&IndexEntry> = Vec::new();
    let mut config: Vec<&IndexEntry> = Vec::new();
    let mut docs: Vec<&IndexEntry> = Vec::new();
    for e in &index.entries {
        let p = e.rel_path.as_str();
        if p.ends_with(".md") {
            docs.push(e);
        } else if p.ends_with(".toml") || p.ends_with(".json") {
            config.push(e);
        } else {
            source.push(e);
        }
    }

    if !source.is_empty() {
        out.push_str("## Source Structure\n\n");
        for e in source {
            out.push_str(&format!("- {} — {}\n", e.rel_path, e.summary));
        }
        out.push('\n');
    }
    if !config.is_empty() {
        out.push_str("## Config\n\n");
        for e in config {
            out.push_str(&format!("- {} — {}\n", e.rel_path, e.summary));
        }
        out.push('\n');
    }
    if !docs.is_empty() {
        out.push_str("## Docs\n\n");
        for e in docs {
            out.push_str(&format!("- {} — {}\n", e.rel_path, e.summary));
        }
        out.push('\n');
    }
    out
}

/// Async, bounded-depth directory walk.
///
/// Uses boxed recursion because `async fn` recursion requires a heap
/// indirection. Walks `dir` relative to `root`, up to `INDEX_MAX_DEPTH`.
fn walk_dir<'a>(
    root: &'a Path,
    dir: &'a Path,
    depth: usize,
    out: &'a mut Vec<IndexEntry>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        if depth > INDEX_MAX_DEPTH {
            return Ok(());
        }
        let mut rd = match tokio::fs::read_dir(dir).await {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };
        while let Some(entry) = rd.next_entry().await.ok().flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if should_skip_dirname(&name_str) {
                continue;
            }
            if path.is_dir() {
                walk_dir(root, &path, depth + 1, out).await?;
                continue;
            }
            if !path.is_file() {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !INCLUDED_EXTS.contains(&ext.as_str()) {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            let summary = summarize_file(&path)
                .await
                .unwrap_or_else(|| "(no summary)".to_string());
            out.push(IndexEntry {
                rel_path: rel,
                summary,
            });
        }
        Ok(())
    })
}

/// Return true if this directory name should be skipped during the walk.
fn should_skip_dirname(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | "target"
            | "node_modules"
            | ".venv"
            | "venv"
            | "__pycache__"
            | ".mypy_cache"
            | ".pytest_cache"
            | ".ruff_cache"
            | ".mcp-vector-search"
            | ".open-mpm"
            | "out"
            | "dist"
            | "build"
    )
}

/// Extract a single-line summary from a file's head.
///
/// Why: Cheap substitute for AST parsing — we just pull the first non-blank
/// comment/docstring line to give a human reading the index some orientation.
/// What: Reads up to `INDEX_LINES_PER_FILE`, finds the first line that looks
/// like a doc comment (`//!`, `///`, `//`, `#!`, `#`, `"""...`) or the first
/// non-blank non-code-fence line.
/// Test: Implicit via `scan_project_finds_source_files`.
async fn summarize_file(path: &Path) -> Option<String> {
    let text = tokio::fs::read_to_string(path).await.ok()?;
    for line in text.lines().take(INDEX_LINES_PER_FILE) {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(rest) = t.strip_prefix("//!").or_else(|| t.strip_prefix("///")) {
            return Some(first_sentence(rest.trim()));
        }
        if let Some(rest) = t.strip_prefix("# ") {
            return Some(first_sentence(rest));
        }
        if t.starts_with("#!") {
            continue;
        }
        if t.starts_with("//") {
            let rest = t.trim_start_matches('/').trim();
            if !rest.is_empty() {
                return Some(first_sentence(rest));
            }
        }
        if t.starts_with("\"\"\"") {
            let inner = t.trim_matches('"').trim();
            if !inner.is_empty() {
                return Some(first_sentence(inner));
            }
        }
    }
    None
}

/// Take the first sentence (up to the first `.`, `!`, `?`, or 120 chars).
fn first_sentence(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.char_indices() {
        if i >= 120 {
            break;
        }
        out.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            break;
        }
    }
    out
}

/// Recursively collect `.md` file paths under `root`, bounded by `max`.
///
/// Why: `seed_skills` needs every nested skill file (skills are organized in
/// `languages/`, `frameworks/`, `workflow/` subdirs). Doing the walk inline
/// in `seed_skills` would obscure the seeding loop; pulling it into a helper
/// keeps both readable.
/// What: Async walker that pushes onto `out` until `*count >= max`.
/// Test: Exercised via `seed_skills_indexes_skill_files` (which writes nested
/// skill files and asserts they're indexed).
fn collect_md_files<'a>(
    root: &'a Path,
    out: &'a mut Vec<PathBuf>,
    count: &'a mut usize,
    max: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        if *count >= max {
            return;
        }
        let mut rd = match tokio::fs::read_dir(root).await {
            Ok(r) => r,
            Err(_) => return,
        };
        while let Some(entry) = rd.next_entry().await.ok().flatten() {
            if *count >= max {
                return;
            }
            let path = entry.path();
            if path.is_dir() {
                collect_md_files(&path, out, count, max).await;
                continue;
            }
            if !path.is_file() {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            out.push(path);
            *count += 1;
        }
    })
}

/// Parse `name`, `tags`, and a summary/description out of a skill's
/// YAML-style frontmatter block.
///
/// Why: Skill memories should carry semantic metadata (name + tags) so the
/// `memory_recall` tool can return them with a meaningful payload. We
/// hand-roll the parser to avoid pulling a YAML dep just for three fields,
/// matching the approach in `skills::registry::parse_skill_meta`.
/// What: Returns `Some((name, tags, summary))` when a `---` ... `---` fence
/// is present and at minimum contains a `name:` line. `summary` falls back
/// to `description` when the former isn't set, and is empty when neither is.
/// Test: `parse_skill_frontmatter_extracts_fields`.
fn parse_skill_frontmatter(content: &str) -> Option<(String, Vec<String>, String)> {
    let rest = content.strip_prefix("---")?;
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))?;
    let end_rel = rest.find("\n---")?;
    let fm = &rest[..end_rel];

    let mut name: Option<String> = None;
    let mut tags: Vec<String> = Vec::new();
    let mut summary = String::new();
    for line in fm.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("name:") {
            let v = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() {
                name = Some(v);
            }
        } else if let Some(rest) = t.strip_prefix("tags:") {
            let v = rest.trim();
            if let Some(inner) = v.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                tags = inner
                    .split(',')
                    .map(|x| x.trim().trim_matches('"').trim_matches('\'').to_string())
                    .filter(|x| !x.is_empty())
                    .collect();
            }
        } else if let Some(rest) = t.strip_prefix("summary:") {
            let v = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() && summary.is_empty() {
                summary = v;
            }
        } else if let Some(rest) = t.strip_prefix("description:") {
            let v = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() && summary.is_empty() {
                summary = v;
            }
        }
    }

    Some((name?, tags, summary))
}

/// Build the human-readable description embedded for an MCP server.
///
/// Why: Embedding raw JSON would give the embedder little semantic signal;
/// rendering as natural-language sentences ("MCP Server: foo. Command: bar")
/// makes recall like "MCP server for vector search" land on the right hit.
/// What: Returns a multi-line string with name/command/args/description/env.
/// Test: `render_mcp_description_includes_all_fields`.
fn render_mcp_description(
    name: &str,
    command: &str,
    args: &[String],
    description: &str,
    env_keys: &[String],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("MCP Server: {name}\n"));
    if !command.is_empty() {
        let argline = if args.is_empty() {
            command.to_string()
        } else {
            format!("{command} {}", args.join(" "))
        };
        out.push_str(&format!("Command: {argline}\n"));
    }
    if !description.is_empty() {
        out.push_str(&format!("Description: {description}\n"));
    }
    if !env_keys.is_empty() {
        out.push_str(&format!("Environment: {}\n", env_keys.join(", ")));
    }
    out
}

#[cfg(test)]
mod tests {
    // Why: These tests hold `HOME_LOCK` (a `std::sync::Mutex`) across async
    // I/O to serialize global $HOME mutation between tests. The lock is
    // intentionally held for the full test body; see `crate::test_env` for
    // the rationale. The tokio multi-threaded test runtime keeps this from
    // deadlocking.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use tempfile::tempdir;

    async fn fixture_project() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let omd = root.join(".open-mpm");
        tokio::fs::create_dir_all(&root.join("src")).await.unwrap();
        tokio::fs::write(
            root.join("src/lib.rs"),
            "//! Example library crate root.\nfn main() {}\n",
        )
        .await
        .unwrap();
        tokio::fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\n")
            .await
            .unwrap();
        tokio::fs::write(root.join("README.md"), "# Hello\nProject docs.\n")
            .await
            .unwrap();
        (tmp, root, omd)
    }

    #[tokio::test]
    async fn scan_project_finds_source_files() {
        let (_g, root, omd) = fixture_project().await;
        let init = ProjectInitializer::new(&root, &omd);
        let idx = init.scan_project().await.unwrap();
        assert!(idx.entries.iter().any(|e| e.rel_path.ends_with("lib.rs")));
        assert!(idx.entries.iter().any(|e| e.rel_path == "Cargo.toml"));
        assert!(idx.entries.iter().any(|e| e.rel_path == "README.md"));
    }

    #[tokio::test]
    async fn render_markdown_groups_by_kind() {
        let idx = ProjectIndex {
            project_name: "demo".into(),
            entries: vec![
                IndexEntry {
                    rel_path: "src/lib.rs".into(),
                    summary: "Entry".into(),
                },
                IndexEntry {
                    rel_path: "Cargo.toml".into(),
                    summary: "Manifest".into(),
                },
                IndexEntry {
                    rel_path: "README.md".into(),
                    summary: "Docs".into(),
                },
            ],
        };
        let md = render_index_markdown(&idx, Utc::now());
        assert!(md.contains("# Project: demo"));
        assert!(md.contains("## Source Structure"));
        assert!(md.contains("## Config"));
        assert!(md.contains("## Docs"));
        assert!(md.contains("src/lib.rs"));
    }

    #[tokio::test]
    async fn initialize_if_needed_creates_index_and_marker() {
        let (_g, root, omd) = fixture_project().await;
        let init = ProjectInitializer::new(&root, &omd);
        let ctx = init.initialize_if_needed().await.unwrap();
        assert!(ctx.project_summary.contains("# Project:"));
        assert!(omd.join("project-index.md").exists());
        assert!(omd.join("initialized").exists());
    }

    #[tokio::test]
    async fn initialize_if_needed_skips_when_fresh() {
        let (_g, root, omd) = fixture_project().await;
        let init = ProjectInitializer::new(&root, &omd);
        let first = init.initialize_if_needed().await.unwrap();
        // Mutate the written index and confirm the second call doesn't
        // regenerate it (proves cache hit).
        tokio::fs::write(omd.join("project-index.md"), b"CACHED_CONTENT")
            .await
            .unwrap();
        let second = init.initialize_if_needed().await.unwrap();
        assert_eq!(second.project_summary, "CACHED_CONTENT");
        assert_eq!(second.initialized_at, first.initialized_at);
    }

    #[tokio::test]
    async fn force_reinitialize_rewrites_index() {
        let (_g, root, omd) = fixture_project().await;
        let init = ProjectInitializer::new(&root, &omd);
        let _ = init.initialize_if_needed().await.unwrap();
        tokio::fs::write(omd.join("project-index.md"), b"STALE")
            .await
            .unwrap();
        let fresh = init.force_reinitialize().await.unwrap();
        assert!(fresh.project_summary.contains("# Project:"));
        assert!(!fresh.project_summary.contains("STALE"));
    }

    #[tokio::test]
    async fn prompt_prefix_includes_both_sections() {
        let ctx = InitContext {
            project_summary: "# Project: x".into(),
            relevant_memories: vec!["m1".into(), "m2".into()],
            initialized_at: Utc::now(),
        };
        let p = ctx.to_prompt_prefix();
        assert!(p.contains("## Project Context"));
        assert!(p.contains("## Relevant Prior Knowledge"));
        assert!(p.contains("- m1"));
        assert!(p.contains("- m2"));
        assert!(p.ends_with("---\n\n"));
    }

    #[tokio::test]
    async fn seeds_documentation_from_docs_dir() {
        use crate::memory::redb_usearch::RedbUsearchStore;
        use crate::memory::store::Segment;

        // Stub embedder: deterministic vector independent of network.
        struct StubEmbedder {
            dim: usize,
        }
        impl crate::memory::Embedder for StubEmbedder {
            fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
                Ok(texts
                    .iter()
                    .map(|t| {
                        let mut v = vec![0.0f32; self.dim];
                        // tiny content-dependent fingerprint so distinct docs
                        // don't collapse to identical vectors.
                        for (i, b) in t.bytes().take(self.dim).enumerate() {
                            v[i] = (b as f32) / 255.0;
                        }
                        v
                    })
                    .collect())
            }
            fn embed_single(&self, text: &str) -> Result<Vec<f32>> {
                Ok(self.embed(&[text])?.remove(0))
            }
            fn dimension(&self) -> usize {
                self.dim
            }
        }

        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let omd = root.join(".open-mpm").join("state");
        tokio::fs::create_dir_all(root.join("docs/user"))
            .await
            .unwrap();
        tokio::fs::write(
            root.join("docs/user/quickstart.md"),
            "# Quickstart\n\nRun `cargo run` to launch CTRL.\n",
        )
        .await
        .unwrap();
        tokio::fs::create_dir_all(&omd).await.unwrap();

        let dim = 16;
        let store = RedbUsearchStore::open(&omd.join("sessions/default"), dim).unwrap();
        let embedder = StubEmbedder { dim };

        let init = ProjectInitializer::new(&root, &omd);
        let n = init.seed_documentation(&store, &embedder).await.unwrap();
        assert_eq!(n, 1, "expected one doc seeded");

        // Verify the memory store now has the content under the expected id.
        let payload = store
            .get(Segment::AgentMemory, "docs:docs/user/quickstart.md")
            .await
            .unwrap()
            .expect("doc payload should be present");
        let content = payload.get("content").and_then(|v| v.as_str()).unwrap();
        assert!(content.contains("Quickstart"));
        assert!(content.contains("cargo run"));
        assert_eq!(
            payload.get("tag").and_then(|v| v.as_str()),
            Some("docs/user")
        );
        // session_id should be the docs-seed constant so doc memories are
        // distinguishable from agent-run memories during recall/export.
        assert_eq!(
            payload.get("session_id").and_then(|v| v.as_str()),
            Some(DOCS_SEED_SESSION_ID),
        );
        assert!(
            payload.get("created_at").and_then(|v| v.as_str()).is_some(),
            "created_at timestamp should be stamped"
        );

        // Re-seeding without changes should be a no-op.
        let n2 = init.seed_documentation(&store, &embedder).await.unwrap();
        assert_eq!(n2, 0, "unchanged docs should not re-seed");

        // Confirm tracker file written.
        assert!(omd.join("docs_seeded.json").exists());
    }

    #[tokio::test]
    async fn seed_documentation_uses_docs_seed_session() {
        // Focused test: every payload written by seed_documentation must
        // carry session_id == DOCS_SEED_SESSION_ID so doc memories can be
        // identified separately from agent run memories.
        use crate::memory::redb_usearch::RedbUsearchStore;
        use crate::memory::store::Segment;

        struct StubEmbedder {
            dim: usize,
        }
        impl crate::memory::Embedder for StubEmbedder {
            fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
                Ok(texts.iter().map(|_| vec![0.1f32; self.dim]).collect())
            }
            fn embed_single(&self, _: &str) -> Result<Vec<f32>> {
                Ok(vec![0.1f32; self.dim])
            }
            fn dimension(&self) -> usize {
                self.dim
            }
        }

        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let omd = root.join(".open-mpm").join("state");
        tokio::fs::create_dir_all(root.join("docs/developer"))
            .await
            .unwrap();
        tokio::fs::write(
            root.join("docs/developer/architecture.md"),
            "# Arch\n\nAgent harness in Rust.\n",
        )
        .await
        .unwrap();
        tokio::fs::create_dir_all(&omd).await.unwrap();

        let dim = 16;
        let store = RedbUsearchStore::open(&omd.join("sessions/default"), dim).unwrap();
        let embedder = StubEmbedder { dim };

        let init = ProjectInitializer::new(&root, &omd);
        let n = init.seed_documentation(&store, &embedder).await.unwrap();
        assert_eq!(n, 1);

        let payload = store
            .get(Segment::AgentMemory, "docs:docs/developer/architecture.md")
            .await
            .unwrap()
            .expect("payload present");
        assert_eq!(
            payload.get("session_id").and_then(|v| v.as_str()),
            Some(DOCS_SEED_SESSION_ID),
            "doc memories must be tagged with the docs-seed session"
        );
    }

    #[tokio::test]
    async fn seed_documentation_skips_when_docs_missing() {
        use crate::memory::redb_usearch::RedbUsearchStore;

        struct StubEmbedder;
        impl crate::memory::Embedder for StubEmbedder {
            fn embed(&self, _: &[&str]) -> Result<Vec<Vec<f32>>> {
                Ok(vec![])
            }
            fn embed_single(&self, _: &str) -> Result<Vec<f32>> {
                Ok(vec![0.0; 4])
            }
            fn dimension(&self) -> usize {
                4
            }
        }

        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let omd = root.join(".open-mpm").join("state");
        tokio::fs::create_dir_all(&omd).await.unwrap();

        let store = RedbUsearchStore::open(&omd.join("sessions/default"), 4).unwrap();
        let init = ProjectInitializer::new(&root, &omd);
        let n = init
            .seed_documentation(&store, &StubEmbedder)
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn prompt_prefix_empty_when_both_blank() {
        let ctx = InitContext {
            project_summary: String::new(),
            relevant_memories: vec![],
            initialized_at: Utc::now(),
        };
        assert!(ctx.to_prompt_prefix().is_empty());
    }

    /// Process-wide mutex serializing tests that mutate `$HOME`.
    ///
    /// Why: cargo test runs unit tests on a multi-threaded executor by
    /// default; `std::env::set_var` is a process-wide mutation, so two
    /// concurrent tests sandboxing HOME stomp on each other and one will
    /// see the other's tempdir (or the developer's real `~/.claude/skills/`)
    /// before it's restored. We re-export the crate-wide `HOME_LOCK` so
    /// this module's tests serialize with HOME-mutating tests in OTHER
    /// modules (e.g. `mistake_log`) too — a per-module static was the
    /// original implementation but it allowed cross-module races.
    use crate::test_env::HOME_LOCK;

    /// Stub embedder shared by the skill / MCP / seed_all tests so we don't
    /// need to load the real ONNX model in CI.
    struct StubEmbedderShared {
        dim: usize,
    }
    impl crate::memory::Embedder for StubEmbedderShared {
        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    let mut v = vec![0.0f32; self.dim];
                    for (i, b) in t.bytes().take(self.dim).enumerate() {
                        v[i] = (b as f32) / 255.0;
                    }
                    v
                })
                .collect())
        }
        fn embed_single(&self, text: &str) -> Result<Vec<f32>> {
            Ok(self.embed(&[text])?.remove(0))
        }
        fn dimension(&self) -> usize {
            self.dim
        }
    }

    #[tokio::test]
    async fn seed_skills_indexes_skill_files() {
        use crate::memory::redb_usearch::RedbUsearchStore;
        use crate::memory::store::Segment;

        // Sandbox HOME so seed_skills doesn't pick up the developer's real
        // ~/.claude/skills/ during the test. The HOME_LOCK serializes
        // concurrent HOME-sandboxing tests so they don't stomp on each other.
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let home_sandbox = tempdir().unwrap();
        // SAFETY: HOME_LOCK is held; restoration runs before guard drop.
        unsafe {
            std::env::set_var("HOME", home_sandbox.path());
        }

        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let omd = root.join(".open-mpm").join("state");
        let skills_dir = root.join(".open-mpm").join("skills");
        tokio::fs::create_dir_all(&skills_dir).await.unwrap();
        tokio::fs::write(
            skills_dir.join("python-compat.md"),
            "---\nname: python-compat\ntags: [python, fastapi, bcrypt]\nsummary: Python compatibility fixes\n---\n\n# Python Compat\n\nbody\n",
        )
        .await
        .unwrap();
        // Nested skill should also be picked up.
        let nested = skills_dir.join("frameworks");
        tokio::fs::create_dir_all(&nested).await.unwrap();
        tokio::fs::write(
            nested.join("fastapi.md"),
            "---\nname: fastapi\ntags: [python, fastapi]\ndescription: FastAPI patterns\n---\n\n# FastAPI\n\nbody\n",
        )
        .await
        .unwrap();
        tokio::fs::create_dir_all(&omd).await.unwrap();

        let dim = 16;
        let store = RedbUsearchStore::open(&omd.join("sessions/default"), dim).unwrap();
        let embedder = StubEmbedderShared { dim };

        let init = ProjectInitializer::new(&root, &omd);
        let n = init.seed_skills(&store, &embedder).await.unwrap();
        assert_eq!(n, 2, "expected two skills seeded, got {n}");

        let payload = store
            .get(Segment::AgentMemory, "skill:python-compat")
            .await
            .unwrap()
            .expect("skill payload should be present");
        assert_eq!(
            payload.get("tag").and_then(|v| v.as_str()),
            Some("configuration/skill")
        );
        assert_eq!(
            payload.get("session_id").and_then(|v| v.as_str()),
            Some(SKILLS_SEED_SESSION_ID)
        );
        assert_eq!(
            payload.get("skill_name").and_then(|v| v.as_str()),
            Some("python-compat")
        );
        let tags = payload
            .get("skill_tags")
            .and_then(|v| v.as_array())
            .expect("skill_tags array");
        assert!(tags.iter().any(|t| t.as_str() == Some("python")));
        assert!(tags.iter().any(|t| t.as_str() == Some("fastapi")));
        assert!(payload.get("path").and_then(|v| v.as_str()).is_some());
        assert!(payload.get("created_at").and_then(|v| v.as_str()).is_some());

        // Re-seeding without changes should be a no-op.
        let n2 = init.seed_skills(&store, &embedder).await.unwrap();
        assert_eq!(n2, 0, "unchanged skills should not re-seed");

        assert!(omd.join("skills_seeded.json").exists());

        // Restore HOME.
        unsafe {
            match prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[tokio::test]
    async fn seed_skills_falls_back_to_filename_without_frontmatter() {
        use crate::memory::redb_usearch::RedbUsearchStore;
        use crate::memory::store::Segment;

        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let home_sandbox = tempdir().unwrap();
        // SAFETY: HOME_LOCK is held for the duration of this test.
        unsafe {
            std::env::set_var("HOME", home_sandbox.path());
        }

        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let omd = root.join(".open-mpm").join("state");
        let skills_dir = root.join(".open-mpm").join("skills");
        tokio::fs::create_dir_all(&skills_dir).await.unwrap();
        // No frontmatter — name should fall back to file stem.
        tokio::fs::write(
            skills_dir.join("orphan-skill.md"),
            "# Orphan Skill\n\nNo frontmatter at all.\n",
        )
        .await
        .unwrap();
        tokio::fs::create_dir_all(&omd).await.unwrap();

        let dim = 16;
        let store = RedbUsearchStore::open(&omd.join("sessions/default"), dim).unwrap();
        let embedder = StubEmbedderShared { dim };

        let init = ProjectInitializer::new(&root, &omd);
        let n = init.seed_skills(&store, &embedder).await.unwrap();
        assert_eq!(n, 1);

        let payload = store
            .get(Segment::AgentMemory, "skill:orphan-skill")
            .await
            .unwrap()
            .expect("orphan skill should be indexed");
        assert_eq!(
            payload.get("skill_name").and_then(|v| v.as_str()),
            Some("orphan-skill")
        );

        unsafe {
            match prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[tokio::test]
    async fn seed_skills_skips_when_dirs_missing() {
        use crate::memory::redb_usearch::RedbUsearchStore;

        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let home_sandbox = tempdir().unwrap();
        // SAFETY: HOME_LOCK is held for the duration of this test.
        unsafe {
            std::env::set_var("HOME", home_sandbox.path());
        }

        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let omd = root.join(".open-mpm").join("state");
        tokio::fs::create_dir_all(&omd).await.unwrap();

        let store = RedbUsearchStore::open(&omd.join("sessions/default"), 16).unwrap();
        let embedder = StubEmbedderShared { dim: 16 };
        let init = ProjectInitializer::new(&root, &omd);
        let n = init.seed_skills(&store, &embedder).await.unwrap();
        assert_eq!(n, 0);

        unsafe {
            match prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[tokio::test]
    async fn seed_mcp_connections_indexes_servers() {
        use crate::memory::redb_usearch::RedbUsearchStore;
        use crate::memory::store::Segment;

        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let home_sandbox = tempdir().unwrap();
        // SAFETY: HOME_LOCK is held for the duration of this test.
        unsafe {
            std::env::set_var("HOME", home_sandbox.path());
        }

        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let omd = root.join(".open-mpm").join("state");
        tokio::fs::create_dir_all(&omd).await.unwrap();

        // Fixture .mcp.json with two servers.
        let mcp_json = serde_json::json!({
            "mcpServers": {
                "kuzu-memory": {
                    "type": "stdio",
                    "command": "kuzu-memory",
                    "args": ["mcp"],
                    "env": {"KUZU_MEMORY_DB": "/tmp/db"}
                },
                "vector-search": {
                    "type": "stdio",
                    "command": "uv",
                    "args": ["run", "mcp-vector-search", "mcp"],
                    "description": "Semantic code search"
                }
            }
        });
        tokio::fs::write(
            root.join(".mcp.json"),
            serde_json::to_string_pretty(&mcp_json).unwrap(),
        )
        .await
        .unwrap();

        let dim = 16;
        let store = RedbUsearchStore::open(&omd.join("sessions/default"), dim).unwrap();
        let embedder = StubEmbedderShared { dim };

        let init = ProjectInitializer::new(&root, &omd);
        let n = init.seed_mcp_connections(&store, &embedder).await.unwrap();
        assert_eq!(n, 2, "expected two MCP servers seeded, got {n}");

        let payload = store
            .get(Segment::AgentMemory, "mcp:kuzu-memory")
            .await
            .unwrap()
            .expect("kuzu-memory MCP entry");
        assert_eq!(
            payload.get("tag").and_then(|v| v.as_str()),
            Some("configuration/mcp")
        );
        assert_eq!(
            payload.get("session_id").and_then(|v| v.as_str()),
            Some(MCP_SEED_SESSION_ID)
        );
        assert_eq!(
            payload.get("server_name").and_then(|v| v.as_str()),
            Some("kuzu-memory")
        );
        assert_eq!(
            payload.get("command").and_then(|v| v.as_str()),
            Some("kuzu-memory")
        );
        let content = payload.get("content").and_then(|v| v.as_str()).unwrap();
        assert!(content.contains("MCP Server: kuzu-memory"));
        assert!(content.contains("Command: kuzu-memory mcp"));
        assert!(content.contains("Environment: KUZU_MEMORY_DB"));

        let payload2 = store
            .get(Segment::AgentMemory, "mcp:vector-search")
            .await
            .unwrap()
            .expect("vector-search MCP entry");
        let content2 = payload2.get("content").and_then(|v| v.as_str()).unwrap();
        assert!(content2.contains("Description: Semantic code search"));

        // Re-seed should be a no-op.
        let n2 = init.seed_mcp_connections(&store, &embedder).await.unwrap();
        assert_eq!(n2, 0);

        assert!(omd.join("mcp_seeded.json").exists());

        unsafe {
            match prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[tokio::test]
    async fn seed_all_runs_all_three_seeders() {
        use crate::memory::redb_usearch::RedbUsearchStore;

        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let home_sandbox = tempdir().unwrap();
        // SAFETY: HOME_LOCK is held for the duration of this test.
        unsafe {
            std::env::set_var("HOME", home_sandbox.path());
        }

        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let omd = root.join(".open-mpm").join("state");
        tokio::fs::create_dir_all(&omd).await.unwrap();
        // docs
        tokio::fs::create_dir_all(root.join("docs/user"))
            .await
            .unwrap();
        tokio::fs::write(root.join("docs/user/quickstart.md"), "# Quickstart\n")
            .await
            .unwrap();
        // skills
        tokio::fs::create_dir_all(root.join(".open-mpm/skills"))
            .await
            .unwrap();
        tokio::fs::write(
            root.join(".open-mpm/skills/x.md"),
            "---\nname: x\ntags: [a]\n---\n\nbody\n",
        )
        .await
        .unwrap();
        // mcp
        tokio::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"s":{"command":"c","args":[]}}}"#,
        )
        .await
        .unwrap();

        let dim = 16;
        let store = RedbUsearchStore::open(&omd.join("sessions/default"), dim).unwrap();
        let embedder = StubEmbedderShared { dim };
        let init = ProjectInitializer::new(&root, &omd);
        let (docs, skills, mcp) = init.seed_all(&store, &embedder).await;
        assert_eq!(docs, 1);
        assert_eq!(skills, 1);
        assert_eq!(mcp, 1);

        unsafe {
            match prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn parse_skill_frontmatter_extracts_fields() {
        let content = "---\nname: foo\ntags: [a, b, c]\nsummary: A summary\n---\n\nbody\n";
        let (name, tags, summary) = parse_skill_frontmatter(content).unwrap();
        assert_eq!(name, "foo");
        assert_eq!(tags, vec!["a", "b", "c"]);
        assert_eq!(summary, "A summary");

        // Missing frontmatter returns None.
        assert!(parse_skill_frontmatter("# Just markdown\n").is_none());

        // description falls back into the summary slot.
        let content2 = "---\nname: bar\ntags: [x]\ndescription: Desc here\n---\n\n";
        let (_, _, s) = parse_skill_frontmatter(content2).unwrap();
        assert_eq!(s, "Desc here");
    }

    #[test]
    fn render_mcp_description_includes_all_fields() {
        let s = render_mcp_description(
            "myserver",
            "node",
            &["server.js".to_string(), "--port=3000".to_string()],
            "Helpful tool",
            &["API_KEY".to_string(), "DB_URL".to_string()],
        );
        assert!(s.contains("MCP Server: myserver"));
        assert!(s.contains("Command: node server.js --port=3000"));
        assert!(s.contains("Description: Helpful tool"));
        assert!(s.contains("Environment: API_KEY, DB_URL"));
    }
}
