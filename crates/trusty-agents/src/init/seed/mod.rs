//! Memory-seeding methods for `ProjectInitializer` (docs / skills / MCP).
//!
//! Why: The three seeders plus `seed_all` are mechanically independent from the
//! project-index lifecycle in the parent `mod.rs`; isolating them keeps each
//! file focused and under the 500-line cap.
//! What: This `mod.rs` holds the shared helpers (mtime probe, recursive `.md`
//! collector, frontmatter parser, MCP description renderer) and `seed_all`; the
//! three individual seeders live in sibling files (`docs.rs`, `skills.rs`,
//! `mcp.rs`) as further `impl ProjectInitializer` blocks.
//! Test: See `init::tests` (seed_* unit tests with stubbed embedder + store).

mod docs;
mod mcp;
mod skills;

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::ProjectInitializer;
use crate::memory::{Embedder, MemoryStore};

/// Per-source skill scan budget — bounds how many `.md` files we read from
/// a single search path during seeding.
///
/// Why: Mirrors `crate::skills::registry::MAX_SKILLS_PER_SOURCE` so a giant
/// external skills tree (e.g. claude-mpm's 700+-file `~/.claude/skills/`)
/// can't make seed_skills hang on cold-cache disks.
const SKILL_SEED_MAX_PER_SOURCE: usize = 50;

impl ProjectInitializer {
    /// Run all memory seeders (docs + skills + MCP) in sequence and report.
    ///
    /// Why: Callers (CTRL startup, workflow runner) want one entrypoint that
    /// guarantees every structured-context source is indexed. Centralizing
    /// the trio also gives us one consistent log line per startup.
    /// What: Calls `seed_documentation`, `seed_skills`, and
    /// `seed_mcp_connections` in order. Each is best-effort: a failure in
    /// one stage logs a WARN and the remaining stages still run. Returns a
    /// `(docs, skills, mcp)` tuple of seeded counts. Emits a combined
    /// `[trusty-agents] Memory seeded: N docs, N skills, N MCP connections` line
    /// to stderr on success.
    /// Test: `seed_all_runs_all_three_seeders` in `init::tests`.
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
            "\r[trusty-agents] Memory seeded: {docs} docs, {skills} skills, {mcp} MCP connections"
        );
        (docs, skills, mcp)
    }
}

/// Read a file's mtime as whole seconds since the UNIX epoch, or 0 on error.
///
/// Why: All three seeders share the same "skip if unchanged" logic keyed on
/// mtime; centralizing avoids three copies of the same metadata dance.
/// What: Returns `metadata.modified()` converted to epoch seconds, or 0 if any
/// step fails (missing file, pre-epoch time, unsupported platform).
/// Test: Exercised indirectly by the re-seed no-op assertions in `init::tests`.
pub(super) async fn file_mtime_secs(path: &Path) -> u64 {
    match tokio::fs::metadata(path).await {
        Ok(m) => m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0),
        Err(_) => 0,
    }
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
pub(super) fn collect_md_files<'a>(
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
pub(in crate::init) fn parse_skill_frontmatter(
    content: &str,
) -> Option<(String, Vec<String>, String)> {
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
pub(in crate::init) fn render_mcp_description(
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
