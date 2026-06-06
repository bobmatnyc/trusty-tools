//! CLI subcommand dispatch for memory and code search.
//!
//! Why: Agents and humans need to query local memory/code stores without
//! running the full PM loop. This module parses argv and invokes the
//! appropriate read path on the local redb/usearch store.
//! What: Exposes `run_search_command` which handles `memory search`,
//! `memory run`, and `code search` subcommands. Also exports the
//! `did_you_mean` helper used by the top-level dispatcher and nested
//! subcommand handlers to surface friendly typo suggestions.
//! Test: See `search_cmd::tests` for arg-parsing and formatter unit tests,
//! and the `tests` module at the bottom of this file for `did_you_mean`.

pub mod memories_cmd;
pub mod search_cmd;

pub use memories_cmd::run_memories_command;
pub use search_cmd::run_search_command;

/// Find the closest match to `input` from `candidates` using Levenshtein
/// edit distance. Returns `Some(candidate)` when the best match is within
/// `max_distance` edits; `None` when all candidates are too far.
///
/// Why: Surfaces friendly "did you mean X?" suggestions when the user
/// miskeys a subcommand without pulling in a fuzzy-match dependency.
/// What: Quadratic-space Levenshtein via a flat 2-row rolling buffer.
/// Test: `did_you_mean_finds_close_match`, `did_you_mean_rejects_far_input`
pub fn did_you_mean<'a>(
    input: &str,
    candidates: &[&'a str],
    max_distance: usize,
) -> Option<&'a str> {
    let input = input.to_lowercase();
    let mut best: Option<(&str, usize)> = None;
    for &cand in candidates {
        let d = levenshtein(&input, &cand.to_lowercase());
        if d <= max_distance && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((cand, d));
        }
    }
    best.map(|(c, _)| c)
}

/// Standard Levenshtein edit distance between two strings.
///
/// Why: Powers `did_you_mean` without a dependency. Kept private since the
/// public surface intentionally only exposes the suggestion helper.
/// What: Iterative DP with two rolling rows; O(m*n) time, O(n) space.
/// Test: `levenshtein_exact_match_is_zero`, `levenshtein_one_edit`.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            curr[j] = if a[i - 1] == b[j - 1] {
                prev[j - 1]
            } else {
                1 + prev[j - 1].min(prev[j]).min(curr[j - 1])
            };
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn did_you_mean_finds_close_match() {
        let candidates = &["memory", "agents", "skills", "debug"];
        assert_eq!(did_you_mean("memori", candidates, 2), Some("memory"));
        assert_eq!(did_you_mean("agentss", candidates, 2), Some("agents"));
        assert_eq!(did_you_mean("skilss", candidates, 2), Some("skills"));
    }

    #[test]
    fn did_you_mean_rejects_far_input() {
        let candidates = &["memory", "agents", "skills"];
        assert_eq!(did_you_mean("xyz", candidates, 2), None);
    }

    #[test]
    fn levenshtein_exact_match_is_zero() {
        assert_eq!(levenshtein("memory", "memory"), 0);
    }

    #[test]
    fn levenshtein_one_edit() {
        assert_eq!(levenshtein("memori", "memory"), 1);
    }
}
