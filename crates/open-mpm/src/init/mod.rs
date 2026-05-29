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
//! Module layout (see #366 split):
//! - `mod.rs` — public types + `ProjectInitializer` lifecycle (scan/index/cache)
//! - `seed.rs` — the three memory seeders (docs / skills / MCP) + `seed_all`
//! - `scan.rs` — bounded directory walk + Markdown rendering helpers
//! - `tests.rs` — unit tests covering all of the above
//!
//! Test: See `tests` submodule — scan picks up source files, index write/read
//! is idempotent, marker freshness is honored, force path re-runs even when
//! fresh.

mod scan;
mod seed;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use scan::{render_index_markdown, walk_dir};

/// Session id used to tag documentation memories seeded by `seed_documentation`.
///
/// Why: Doc payloads need a stable, identifiable session_id so they can be
/// distinguished from agent-run memories during recall, export, and cleanup.
/// What: A constant string ("seed/docs") stamped into every seeded payload.
/// Test: `seed_documentation_uses_docs_seed_session` in `tests`.
pub const DOCS_SEED_SESSION_ID: &str = "seed/docs";

/// Session id used to tag skill memories seeded by `seed_skills`.
///
/// Why: Skill payloads need a stable, identifiable session_id so they can
/// be filtered/exported separately from doc and agent-run memories.
/// What: A constant string ("seed/configuration/skills") stamped into every
/// seeded payload.
/// Test: `seed_skills_indexes_skill_files` in `tests`.
pub const SKILLS_SEED_SESSION_ID: &str = "seed/configuration/skills";

/// Session id used to tag MCP-connection memories seeded by
/// `seed_mcp_connections`.
///
/// Why: MCP-connection payloads need a stable, identifiable session_id so
/// they can be filtered/exported separately from doc, skill, and agent-run
/// memories.
/// What: A constant string ("seed/configuration/mcp") stamped into every
/// seeded payload.
/// Test: `seed_mcp_connections_indexes_servers` in `tests`.
pub const MCP_SEED_SESSION_ID: &str = "seed/configuration/mcp";

/// Budget: cap lines read per source file when building the index summary.
/// 50 is enough to capture module-level docstrings and the first few items.
pub(super) const INDEX_LINES_PER_FILE: usize = 50;

/// Index-scan depth: only walk 2 levels deep (root + one subdir).
/// Keeps scan cheap on large trees; deeper layout is summarized per-dir.
pub(super) const INDEX_MAX_DEPTH: usize = 2;

/// Marker TTL: re-init if marker is older than this (24h).
const MARKER_TTL_HOURS: i64 = 24;

/// Filename patterns we pull into the index.
pub(super) const INCLUDED_EXTS: &[&str] = &["rs", "toml", "json", "md"];

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
    /// Test: `prompt_prefix_includes_both_sections` in `tests`.
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
/// `force_reinitialize` (always re-runs). Memory seeding lives in `seed.rs`.
/// Test: See the `tests` submodule.
pub struct ProjectInitializer {
    pub(super) project_dir: PathBuf,
    pub(super) open_mpm_dir: PathBuf,
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
