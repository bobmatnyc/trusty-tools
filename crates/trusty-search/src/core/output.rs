//! Result consolidation and token-budgeted formatting for LLM consumers.
//!
//! Why: Raw hybrid search returns many overlapping chunks (the same function
//! appearing as multiple AST chunks, plus its KG-expanded neighbors). When
//! these flow back to an LLM via MCP, they waste tokens on duplicates and
//! blow past context budgets. This module deduplicates overlapping ranges,
//! caps the cumulative token estimate, and emits a compact markdown view.
//!
//! What:
//! - [`consolidate_results`]: merge overlapping same-file chunks (union of
//!   line ranges, max score) and greedily truncate at `max_tokens` (chars/4).
//!   Returns `(kept, truncated)`.
//! - [`format_results_markdown`]: render `Vec<CodeChunk>` as the structured
//!   markdown block specified in issue #13, using `compact_snippet` unless
//!   `full_content` is true.
//!
//! Test: see the `tests` module below — covers dedup union, score
//! preservation, token-budget truncation, compact vs full formatting, and
//! the "no overlap" pass-through path.
//!
//! Issue: #13.

use crate::core::indexer::CodeChunk;

/// Approx chars-per-token ratio used to estimate token budget without a
/// real tokenizer. Conservative for code (~3.5–4 chars/token typical).
const CHARS_PER_TOKEN: usize = 4;

/// Default token budget for consolidated MCP responses. ~4k leaves headroom
/// in a 200k-context model after other tools / system prompt overhead.
pub const DEFAULT_MAX_TOKENS: usize = 4000;

/// Estimate token count of a chunk given the rendering mode. Uses the
/// snippet length when `full_content == false`, else the full content.
fn estimate_tokens(chunk: &CodeChunk, full_content: bool) -> usize {
    let body_len = if full_content {
        chunk.content.len()
    } else {
        chunk
            .compact_snippet
            .as_deref()
            .map(str::len)
            .unwrap_or_else(|| chunk.content.len().min(560)) // ~7 lines * 80 chars
    };
    // +overhead for the markdown header (file:line, score, reason).
    (body_len + chunk.file.len() + 80) / CHARS_PER_TOKEN
}

/// Merge two overlapping chunks from the same file into one. Keeps the
/// higher score, the union line range, the longer content, and preserves
/// the winning chunk's metadata (match_reason from the higher-scored one).
fn merge_chunks(a: CodeChunk, b: CodeChunk) -> CodeChunk {
    // Pick the chunk with the higher score as the "primary" — its
    // match_reason, function_name, etc. survive.
    let (primary, secondary) = if a.score >= b.score { (a, b) } else { (b, a) };
    let start_line = primary.start_line.min(secondary.start_line);
    let end_line = primary.end_line.max(secondary.end_line);
    // Use the longer content body (likely the wider line range).
    let content = if secondary.content.len() > primary.content.len() {
        secondary.content.clone()
    } else {
        primary.content.clone()
    };
    let compact_snippet = primary
        .compact_snippet
        .clone()
        .or_else(|| secondary.compact_snippet.clone());
    CodeChunk {
        id: format!("{}:{}:{}", primary.file, start_line, end_line),
        file: primary.file,
        path: primary.path,
        language: primary.language,
        start_line,
        end_line,
        content,
        function_name: primary.function_name,
        score: primary.score, // already the max
        compact_snippet,
        match_reason: primary.match_reason,
        chunk_type: primary.chunk_type,
        calls: primary.calls,
        inherits_from: primary.inherits_from,
        chunk_depth: primary.chunk_depth,
        index_id: primary.index_id,
    }
}

/// Two ranges `[a_start, a_end]` and `[b_start, b_end]` overlap iff
/// `a_start <= b_end && b_start <= a_end` (inclusive endpoints).
fn ranges_overlap(a_start: usize, a_end: usize, b_start: usize, b_end: usize) -> bool {
    a_start <= b_end && b_start <= a_end
}

/// Deduplicate overlapping chunks within the same file, then greedily
/// truncate by estimated token count.
///
/// Why: LLM callers see many near-duplicate hits (AST chunks for the same
/// function, plus KG expansions). Merging same-file overlaps and capping
/// total tokens keeps responses readable and under context budget.
///
/// What: O(n²) dedup pass (fine for `top_k <= ~50`), then a stable
/// greedy fill ordered by descending score until `max_tokens` is reached.
/// Returns `(kept_chunks, was_truncated)` where `was_truncated == true`
/// means at least one ranked result was dropped to meet the budget.
///
/// Test: `dedup_merges_overlapping_same_file`, `dedup_keeps_disjoint`,
/// `truncation_respects_token_budget`, `truncation_flag_when_capped`.
pub fn consolidate_results(
    chunks: Vec<CodeChunk>,
    max_tokens: usize,
    full_content: bool,
) -> (Vec<CodeChunk>, bool) {
    // 1) Deduplicate: walk results in their incoming order; for each new
    //    chunk, either merge into an existing accumulator (same file +
    //    overlapping line range) or push it as new.
    let mut merged: Vec<CodeChunk> = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let mut absorbed = false;
        for existing in merged.iter_mut() {
            if existing.file == chunk.file
                && ranges_overlap(
                    existing.start_line,
                    existing.end_line,
                    chunk.start_line,
                    chunk.end_line,
                )
            {
                let taken = std::mem::replace(existing, placeholder_chunk());
                *existing = merge_chunks(taken, chunk.clone());
                absorbed = true;
                break;
            }
        }
        if !absorbed {
            merged.push(chunk);
        }
    }

    // 2) Re-sort by score descending — merging can change the effective
    //    rank, and the caller wants the strongest hits first.
    merged.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // 3) Greedy fill: include results until the running token estimate
    //    would exceed `max_tokens`. Always include at least one result so
    //    we never return an empty set for a tiny budget.
    let total = merged.len();
    let mut kept: Vec<CodeChunk> = Vec::new();
    let mut used_tokens: usize = 0;
    for chunk in merged {
        let cost = estimate_tokens(&chunk, full_content);
        if !kept.is_empty() && used_tokens + cost > max_tokens {
            continue;
        }
        used_tokens += cost;
        kept.push(chunk);
    }
    let truncated = kept.len() < total;
    (kept, truncated)
}

/// Cheap placeholder used during the in-place merge swap above. Never
/// observed by callers — only lives for the duration of one `replace`.
fn placeholder_chunk() -> CodeChunk {
    CodeChunk {
        id: String::new(),
        file: String::new(),
        path: None,
        language: None,
        start_line: 0,
        end_line: 0,
        content: String::new(),
        function_name: None,
        score: 0.0,
        compact_snippet: None,
        match_reason: String::new(),
        chunk_type: Default::default(),
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        index_id: None,
    }
}

/// Render consolidated results as the issue-#13 markdown block.
///
/// Why: MCP clients are LLMs — a structured markdown response with
/// fenced code blocks is both human- and model-readable, and avoids
/// JSON token overhead.
///
/// What: emits `## Search Results for "{query}" (N results)`, then for
/// each chunk a `### N. \`file:start-end\` [score, reason]` heading
/// followed by a fenced code block (using `language` when known).
///
/// Test: `format_markdown_uses_compact_by_default`,
/// `format_markdown_full_content_when_requested`.
pub fn format_results_markdown(
    query: &str,
    chunks: &[CodeChunk],
    full_content: bool,
    truncated: bool,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "## Search Results for \"{}\" ({} result{})\n",
        query,
        chunks.len(),
        if chunks.len() == 1 { "" } else { "s" }
    ));
    if truncated {
        out.push_str("\n_Note: results truncated to fit token budget._\n");
    }
    for (i, c) in chunks.iter().enumerate() {
        let lang = c.language.as_deref().unwrap_or("");
        let body = if full_content {
            c.content.as_str()
        } else {
            c.compact_snippet.as_deref().unwrap_or(c.content.as_str())
        };
        out.push_str(&format!(
            "\n### {}. `{}:{}-{}` [score: {:.2}, {}]\n",
            i + 1,
            c.file,
            c.start_line,
            c.end_line,
            c.score,
            c.match_reason
        ));
        out.push_str(&format!("```{lang}\n"));
        out.push_str(body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("```\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunk(file: &str, start: usize, end: usize, score: f32) -> CodeChunk {
        let content = (start..=end)
            .map(|i| format!("line {i}\n"))
            .collect::<String>();
        let compact = content.lines().take(7).collect::<Vec<_>>().join("\n");
        CodeChunk {
            id: format!("{file}:{start}:{end}"),
            file: file.into(),
            path: None,
            language: Some("rust".into()),
            start_line: start,
            end_line: end,
            content,
            function_name: None,
            score,
            compact_snippet: Some(compact),
            match_reason: "hybrid".into(),
            chunk_type: Default::default(),
            calls: vec![],
            inherits_from: vec![],
            chunk_depth: 0,
            index_id: None,
        }
    }

    #[test]
    fn dedup_merges_overlapping_same_file() {
        let chunks = vec![
            make_chunk("a.rs", 10, 20, 0.8),
            make_chunk("a.rs", 15, 25, 0.9), // overlaps -> merge
        ];
        let (kept, truncated) = consolidate_results(chunks, 4000, false);
        assert_eq!(kept.len(), 1);
        assert!(!truncated);
        assert_eq!(kept[0].start_line, 10);
        assert_eq!(kept[0].end_line, 25);
        assert!((kept[0].score - 0.9).abs() < 1e-6); // max score preserved
    }

    #[test]
    fn dedup_keeps_disjoint() {
        let chunks = vec![
            make_chunk("a.rs", 10, 20, 0.8),
            make_chunk("a.rs", 30, 40, 0.7), // disjoint -> separate
            make_chunk("b.rs", 10, 20, 0.6), // different file -> separate
        ];
        let (kept, _) = consolidate_results(chunks, 4000, false);
        assert_eq!(kept.len(), 3);
    }

    #[test]
    fn truncation_respects_token_budget() {
        // 5 large chunks, tiny budget -> only the first survives.
        let chunks = (0..5)
            .map(|i| make_chunk(&format!("f{i}.rs"), 1, 100, 1.0 - i as f32 * 0.1))
            .collect::<Vec<_>>();
        let (kept, truncated) = consolidate_results(chunks, 50, false);
        assert_eq!(kept.len(), 1, "tiny budget should keep only the top hit");
        assert!(truncated);
    }

    #[test]
    fn truncation_flag_false_when_all_fit() {
        let chunks = vec![make_chunk("a.rs", 1, 5, 0.9)];
        let (kept, truncated) = consolidate_results(chunks, 4000, false);
        assert_eq!(kept.len(), 1);
        assert!(!truncated);
    }

    #[test]
    fn results_sorted_by_score_descending() {
        let chunks = vec![
            make_chunk("a.rs", 1, 5, 0.3),
            make_chunk("b.rs", 1, 5, 0.9),
            make_chunk("c.rs", 1, 5, 0.6),
        ];
        let (kept, _) = consolidate_results(chunks, 4000, false);
        assert_eq!(kept[0].file, "b.rs");
        assert_eq!(kept[1].file, "c.rs");
        assert_eq!(kept[2].file, "a.rs");
    }

    #[test]
    fn format_markdown_uses_compact_by_default() {
        let chunks = vec![make_chunk("src/lib.rs", 10, 30, 0.91)];
        let md = format_results_markdown("auth", &chunks, false, false);
        assert!(md.contains("## Search Results for \"auth\" (1 result)"));
        assert!(md.contains("`src/lib.rs:10-30`"));
        assert!(md.contains("[score: 0.91, hybrid]"));
        // compact snippet is the first 7 lines only — full content has 21
        // lines, so the rendered body must NOT contain "line 30".
        assert!(!md.contains("line 30"), "should use compact snippet by default");
    }

    #[test]
    fn format_markdown_full_content_when_requested() {
        let chunks = vec![make_chunk("src/lib.rs", 10, 30, 0.91)];
        let md = format_results_markdown("auth", &chunks, true, false);
        assert!(md.contains("line 30"), "full_content should emit all lines");
    }

    #[test]
    fn format_markdown_signals_truncation() {
        let chunks = vec![make_chunk("a.rs", 1, 5, 0.9)];
        let md = format_results_markdown("q", &chunks, false, true);
        assert!(md.contains("truncated"));
    }

    #[test]
    fn always_keeps_at_least_one_result_under_tiny_budget() {
        // Even with max_tokens = 0 we still emit the top result rather
        // than nothing — empty results are useless to the caller.
        let chunks = vec![make_chunk("a.rs", 1, 200, 0.9)];
        let (kept, truncated) = consolidate_results(chunks, 0, false);
        assert_eq!(kept.len(), 1);
        assert!(!truncated);
    }
}
