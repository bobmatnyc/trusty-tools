//! Relevance-first project-index filtering.
//!
//! Why: trusty-agents injects the project-index Markdown summary into every phase
//! prompt (see `InitContext::to_prompt_prefix` and `WorkflowEngine`). For
//! large projects the wholesale index burns tokens — an agent fixing a UI bug
//! doesn't need the full memory module description. The Kilo.ai
//! "relevant-files-first context" research recommends scanning the task
//! string for symbols/filenames and including only matching index entries.
//! What: `filter_index_entries` extracts keywords from the task, scores each
//! `- `-prefixed entry by substring overlap, and returns the top-N entries.
//! When no keywords match anything, falls back to the first-N entries so the
//! existing wholesale-injection behavior is preserved as a safety net.
//! Test: See unit tests at the bottom of this file — relevance ranking,
//! fallback-when-no-match, top_n cap, case-insensitive matching, empty input.
//!
//! Design notes:
//! - No embeddings or external services: simple lowercase substring match keeps
//!   the filter deterministic, dependency-free, and fast on cold start.
//! - We only score lines that start with `- ` (the project-index format
//!   emitted by `render_index_markdown` in `src/init/mod.rs`). Other lines
//!   (headers, blank lines, non-bulleted prose) are preserved verbatim at the
//!   top of the output so the section's structure stays intact.

/// Common English stop words filtered from task keywords. Kept tiny on
/// purpose — the goal is to drop obvious noise tokens, not to rebuild a full
/// NLP pipeline. Anything else falls through to substring matching.
const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "not", "are", "was", "has",
    "have", "been", "will", "can", "its", "all", "via", "per",
];

/// Filter project-index entries by relevance to `task`.
///
/// Why: Reduces tokens spent on irrelevant project-index lines when the task
/// scope is narrow (e.g. "fix credential routing" should not pull in the full
/// REPL/UI tree).
/// What: Splits `index_content` into bullet entries (`- ` prefix), scores
/// each by counting how many task-derived keywords appear as case-insensitive
/// substrings, sorts by score descending and returns the top `top_n` entries
/// joined by newlines. If every score is zero (no match), falls back to the
/// first `top_n` entries so we never strip context entirely.
/// Test: `filter_returns_relevant_entries`, `filter_falls_back_when_no_match`,
/// `filter_respects_top_n`, `filter_case_insensitive`, `filter_empty_index`.
pub fn filter_index_entries(index_content: &str, task: &str, top_n: usize) -> String {
    if index_content.trim().is_empty() || top_n == 0 {
        return String::new();
    }

    // Partition lines: bullet entries are scored, everything else (headers,
    // blank lines, prose) is preserved verbatim as a preamble so the output
    // still reads as a coherent Markdown section.
    let mut preamble: Vec<&str> = Vec::new();
    let mut entries: Vec<&str> = Vec::new();
    let mut seen_first_bullet = false;
    for line in index_content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("- ") {
            seen_first_bullet = true;
            entries.push(line);
        } else if !seen_first_bullet {
            preamble.push(line);
        }
        // Lines after the first bullet that aren't themselves bullets are
        // dropped — typical project-index format has bullets contiguously,
        // and any trailing prose would skew ordering after sort.
    }

    if entries.is_empty() {
        // No bullet entries — return content as-is; nothing to filter.
        return index_content.to_string();
    }

    let keywords = extract_keywords(task);

    // Score each entry by keyword-substring overlap (case-insensitive). Stable
    // index is preserved as a tiebreaker by zipping with original position.
    let mut scored: Vec<(usize, usize, &str)> = entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let lower = entry.to_lowercase();
            let score: usize = keywords
                .iter()
                .filter(|kw| lower.contains(kw.as_str()))
                .count();
            (score, idx, *entry)
        })
        .collect();

    let any_match = scored.iter().any(|(score, _, _)| *score > 0);

    let selected: Vec<&str> = if any_match {
        // Sort by score desc, then by original index asc to keep a stable order.
        // Drop zero-score entries — when keywords matched anything, irrelevant
        // entries should not pad up to top_n (that defeats the relevance goal).
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        scored
            .into_iter()
            .filter(|(score, _, _)| *score > 0)
            .take(top_n)
            .map(|(_, _, entry)| entry)
            .collect()
    } else {
        // Fallback: preserve existing wholesale-injection semantics by taking
        // the first `top_n` entries in document order.
        entries.iter().take(top_n).copied().collect()
    };

    let mut out = String::new();
    for line in preamble {
        out.push_str(line);
        out.push('\n');
    }
    for (i, entry) in selected.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(entry);
    }
    out
}

/// Extract searchable keywords from a task description.
///
/// Why: The task string is free-form English; we need a deterministic way to
/// turn it into a list of substrings to match against index entries.
/// What: Splits on non-alphanumeric characters, lowercases, drops tokens
/// shorter than 3 chars and drops common stop words. Returns deduped keywords
/// in first-seen order.
/// Test: Indirectly via `filter_returns_relevant_entries` and
/// `filter_case_insensitive`.
fn extract_keywords(task: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for raw in task.split(|c: char| !c.is_alphanumeric()) {
        if raw.len() < 3 {
            continue;
        }
        let lower = raw.to_lowercase();
        if STOP_WORDS.contains(&lower.as_str()) {
            continue;
        }
        if !seen.iter().any(|s| s == &lower) {
            seen.push(lower);
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_index() -> String {
        // Mirrors the `- path — summary` shape emitted by render_index_markdown.
        [
            "## Project Index",
            "",
            "- src/llm/credentials.rs — credential resolution and routing",
            "- src/ctrl/mod.rs — ctrl REPL loop and routing",
            "- src/repl/tui.rs — TUI rendering for ratatui REPL",
            "- src/agents/prompt_builder.rs — layered system prompt builder",
            "- src/init/mod.rs — project self-init and index generation",
            "- src/workflow/engine.rs — phase orchestration engine",
            "- src/memory/store.rs — in-process memory store",
            "- src/ipc/codec.rs — NDJSON IPC codec",
            "- src/tools/dispatch.rs — tool-call dispatch",
            "- src/agents/registry.rs — agent loading from TOML",
        ]
        .join("\n")
    }

    #[test]
    fn filter_returns_relevant_entries() {
        let idx = sample_index();
        let out = filter_index_entries(&idx, "fix credential routing", 3);
        // Both the credentials.rs and ctrl/mod.rs lines mention "credential" /
        // "routing" so they must rank above unrelated lines like repl/tui.rs.
        assert!(
            out.contains("credentials.rs"),
            "expected credentials.rs in output, got:\n{out}"
        );
        assert!(
            out.contains("ctrl/mod.rs"),
            "expected ctrl/mod.rs in output, got:\n{out}"
        );
        assert!(
            !out.contains("repl/tui.rs"),
            "did not expect repl/tui.rs (no keyword match), got:\n{out}"
        );
    }

    #[test]
    fn filter_falls_back_when_no_match() {
        let idx = sample_index();
        let out = filter_index_entries(&idx, "xyzzy gibberish frobnicate", 3);
        // Nothing matches; fall back to the first 3 entries in document order.
        assert!(out.contains("credentials.rs"));
        assert!(out.contains("ctrl/mod.rs"));
        assert!(out.contains("repl/tui.rs"));
        // The 4th entry should NOT be present.
        assert!(
            !out.contains("prompt_builder.rs"),
            "fallback should cap at top_n=3, got:\n{out}"
        );
    }

    #[test]
    fn filter_respects_top_n() {
        let idx = sample_index();
        let out = filter_index_entries(&idx, "src", 5);
        // "src" matches every bullet, so all entries score equally. Top-5 cap.
        let bullet_count = out
            .lines()
            .filter(|l| l.trim_start().starts_with("- "))
            .count();
        assert_eq!(bullet_count, 5, "expected exactly 5 bullets, got:\n{out}");
    }

    #[test]
    fn filter_case_insensitive() {
        let idx = sample_index();
        let out = filter_index_entries(&idx, "Credentials", 1);
        assert!(
            out.contains("credentials.rs"),
            "case-insensitive match failed, got:\n{out}"
        );
    }

    #[test]
    fn filter_empty_index() {
        let out = filter_index_entries("", "any task", 5);
        assert!(out.is_empty(), "empty index must yield empty output");

        let out2 = filter_index_entries("   \n  \n", "any task", 5);
        assert!(
            out2.is_empty(),
            "whitespace-only index must yield empty output"
        );
    }

    #[test]
    fn filter_top_n_zero_returns_empty() {
        let idx = sample_index();
        let out = filter_index_entries(&idx, "credential", 0);
        assert!(out.is_empty());
    }

    #[test]
    fn extract_keywords_drops_stop_words_and_short() {
        let kws = extract_keywords("Fix the bug in credential routing for all users");
        assert!(kws.contains(&"fix".to_string()));
        assert!(kws.contains(&"bug".to_string()));
        assert!(kws.contains(&"credential".to_string()));
        assert!(kws.contains(&"routing".to_string()));
        assert!(kws.contains(&"users".to_string()));
        // Stop words filtered.
        assert!(!kws.contains(&"the".to_string()));
        assert!(!kws.contains(&"for".to_string()));
        assert!(!kws.contains(&"all".to_string()));
        // 'in' is len 2 — too short.
        assert!(!kws.contains(&"in".to_string()));
    }

    #[test]
    fn filter_preserves_preamble_headers() {
        let idx = sample_index();
        let out = filter_index_entries(&idx, "credential", 2);
        assert!(
            out.contains("## Project Index"),
            "preamble header must be preserved, got:\n{out}"
        );
    }

    #[test]
    fn filter_no_bullets_returns_input_unchanged() {
        let content = "## Header\n\nSome prose here.\n";
        let out = filter_index_entries(content, "task", 5);
        assert_eq!(out, content);
    }
}
