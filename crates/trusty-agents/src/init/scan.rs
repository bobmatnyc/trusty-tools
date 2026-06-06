//! Project-tree scanning and Markdown rendering helpers for `init`.
//!
//! Why: The project-index walk (bounded-depth directory traversal + per-file
//! summary extraction) and the Markdown emitter are mechanically distinct from
//! the memory-seeding logic. Splitting them out keeps `mod.rs` focused on the
//! `ProjectInitializer` lifecycle.
//! What: Free functions for rendering a `ProjectIndex` to Markdown, walking the
//! tree, deciding which directories to skip, and summarizing a file's head.
//! Test: Exercised via `init::tests` (scan + render unit tests).

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::{INCLUDED_EXTS, INDEX_LINES_PER_FILE, INDEX_MAX_DEPTH, IndexEntry, ProjectIndex};

/// Render a `ProjectIndex` as Markdown.
///
/// Why: Kept as a free function (not method) so both the live write path and
/// tests can call it without instantiating a `ProjectInitializer`.
/// What: Produces sections for Source Structure / Config / Docs.
/// Test: `render_markdown_groups_by_kind`.
pub(super) fn render_index_markdown(index: &ProjectIndex, at: DateTime<Utc>) -> String {
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
pub(super) fn walk_dir<'a>(
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
            | ".trusty-agents"
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
