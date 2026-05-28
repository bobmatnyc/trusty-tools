//! Shared helpers for the native search tools.
//!
//! Why: The `search_code` execute() arms (daemon, hybrid, vector-only) plus
//! the grep fallback all need the same hit-envelope shaping and a common
//! substring scanner. Extracting them keeps `search_code.rs` focused on
//! dispatch.
//! What: `chunk_to_hit_json`, `compact_snippet`, `grade_from_score`, and
//! `grep_fallback_search`.
//! Test: Exercised via the `search_code` tool tests in `super::tests`
//! (notably `search_code_falls_back_to_grep_when_indexer_absent`).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::tools::file_filter::{should_skip_dir, should_skip_file};

use super::search_code::SNIPPET_MAX_CHARS;

/// Number of context lines around the match line in compact-snippet mode.
const COMPACT_CONTEXT_LINES: usize = 7;

/// Build the JSON hit envelope from a [`CodeChunk`].
///
/// Why: Three execute() arms (daemon, local hybrid, vector-only fallback)
/// all need the same shape and the same compact/full toggle. Extracting
/// it once keeps the policy in one place (#376 C1).
/// What: When `compact` is true, replaces `text` with a 7-line window
/// centred on the chunk's start line and emits a `match_reason` field.
/// When false, behaves exactly like the legacy code path (full text up
/// to `SNIPPET_MAX_CHARS`).
pub(super) fn chunk_to_hit_json(c: &crate::search::indexer::CodeChunk, compact: bool) -> Value {
    // Use the chunk's own match_reason when populated; fall back to "hybrid"
    // for chunks that were stored before #401 was deployed (empty string).
    let reason = if c.match_reason.is_empty() {
        "hybrid"
    } else {
        &c.match_reason
    };
    if compact {
        let snippet = compact_snippet(&c.text, c.start_line, c.start_line, COMPACT_CONTEXT_LINES);
        json!({
            "file": c.file.display().to_string(),
            "line": c.start_line,
            "function": c.function_name,
            "snippet": snippet,
            "score": c.score,
            "grade": grade_from_score(c.score),
            "match_reason": reason,
            "language": c.language,
            "end_line": c.end_line,
        })
    } else {
        let snippet: String = c.text.chars().take(SNIPPET_MAX_CHARS).collect::<String>();
        json!({
            "path": c.file.display().to_string(),
            "function_name": c.function_name,
            "start_line": c.start_line,
            "end_line": c.end_line,
            "language": c.language,
            "score": c.score,
            "snippet": snippet,
            "match_reason": reason,
        })
    }
}

/// Return a small window of `text` around `highlight_line` (1-indexed,
/// relative to the chunk's `start_line`).
///
/// Why: Compact snippets save context-window tokens for downstream LLM
/// reasoning; 7 lines is enough to read a function signature plus a
/// couple of body lines (#376 C1).
/// What: Slices the chunk text by line, takes `context_lines` lines
/// centred (best-effort) on the matching line, joins with `\n`. If the
/// chunk is short, returns the whole text.
fn compact_snippet(
    text: &str,
    chunk_start_line: usize,
    highlight_line: usize,
    context_lines: usize,
) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= context_lines {
        return lines.join("\n");
    }
    // Compute a 0-indexed offset of the highlight line *within* the chunk.
    let offset = highlight_line.saturating_sub(chunk_start_line);
    let half = context_lines / 2;
    let start = offset
        .saturating_sub(half)
        .min(lines.len().saturating_sub(context_lines));
    let end = (start + context_lines).min(lines.len());
    lines[start..end].join("\n")
}

/// Coarse letter grade from an RRF/cosine score.
///
/// Why: Compact output bundles a one-character signal so callers can
/// triage hits without parsing floats (#376 C1).
/// What: Bucketed thresholds tuned for RRF in [0, 2/RRF_K + ε];
/// "A" for the strongest 10% of hits, descending to "F" for noise.
fn grade_from_score(score: f32) -> &'static str {
    if score >= 0.025 {
        "A"
    } else if score >= 0.018 {
        "B"
    } else if score >= 0.012 {
        "C"
    } else if score >= 0.006 {
        "D"
    } else {
        "F"
    }
}

/// Walk `root` (depth-first), case-insensitive substring match `query` against
/// file contents, return up to `top_n` hits in the same shape as the indexed
/// path so downstream LLM reasoning gets a consistent envelope.
///
/// Why: When `SearchCodeTool` is constructed without a `CodeIndexer` (for
/// example, the in-process CTRL research path), agents would otherwise see
/// `{"error": "search index not available"}` and abort. A simple grep is
/// strictly better than a hard error — issue #213.
/// What: Honours the same `should_skip_dir` / `should_skip_file` filters as
/// `GrepFilesTool::walkdir_grep` so we don't churn through `target/`,
/// `.git/`, binaries, etc. Each hit is a JSON object with the same keys
/// (`path`, `start_line`, `end_line`, `snippet`, `score`) the vector path
/// emits, plus a `match_line` for clarity. `score` is a fixed sentinel (0.0)
/// so callers can distinguish fallback hits from semantic hits.
/// Test: `search_code_falls_back_to_grep_when_indexer_absent` writes a
/// fixture file containing the query, points CWD at the tempdir, and asserts
/// the tool surfaces it via the fallback path.
pub(super) fn grep_fallback_search(root: &Path, query: &str, top_n: usize) -> Vec<Value> {
    let needle = query.to_lowercase();
    let mut hits: Vec<Value> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];

    // Cap the snippet so a long line can't blow the context window.
    const MATCH_SNIPPET_MAX_CHARS: usize = SNIPPET_MAX_CHARS;

    while let Some(cur) = stack.pop() {
        if hits.len() >= top_n {
            break;
        }
        if cur.is_file() {
            if should_skip_file(&cur) {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&cur) else {
                continue;
            };
            for (idx, line) in body.lines().enumerate() {
                if line.to_lowercase().contains(&needle) {
                    let snippet: String = line.chars().take(MATCH_SNIPPET_MAX_CHARS).collect();
                    hits.push(json!({
                        "path": cur.display().to_string(),
                        "function_name": Value::Null,
                        "start_line": idx + 1,
                        "end_line": idx + 1,
                        "language": cur
                            .extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or(""),
                        "score": 0.0,
                        "snippet": snippet,
                    }));
                    if hits.len() >= top_n {
                        break;
                    }
                }
            }
        } else if cur.is_dir() {
            if let Some(name) = cur.file_name().and_then(|s| s.to_str())
                && should_skip_dir(name)
            {
                continue;
            }
            if let Ok(rd) = std::fs::read_dir(&cur) {
                for entry in rd.flatten() {
                    stack.push(entry.path());
                }
            }
        }
    }

    hits
}
