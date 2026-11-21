//! The evidence test for a closing task's outcome.
//!
//! Why: `datg.py` warns (never refuses) when a task closes with an outcome that
//! cites no link, sha, PR or issue, run id or "verified". Its regex is
//! `https?://|\b[0-9a-f]{7,40}\b|#\d+|\bPR\s*\d+|\brun\s*(?:id\s*)?\d+|\bverified\b`
//! with `re.I`; the crate takes no `regex` dependency, so the six alternatives
//! are matched by hand with Python's `\w`, `\s` and `\d` classes.
//! What: [`has_evidence`].
//! Test: `evidence_alternatives`, `evidence_rejects_near_misses`.

use crate::pystr::{is_digit, is_space, is_word, starts_with_ci};

fn digits_at(t: &[char], i: usize) -> bool {
    t.get(i).is_some_and(|c| is_digit(*c))
}

fn skip_space(t: &[char], mut i: usize) -> usize {
    while t.get(i).is_some_and(|c| is_space(*c)) {
        i += 1;
    }
    i
}

/// True when `outcome` carries an evidence token.
pub fn has_evidence(outcome: &str) -> bool {
    let t: Vec<char> = outcome.chars().collect();
    let boundary = |i: usize| i == 0 || !is_word(t[i - 1]);
    for i in 0..t.len() {
        let rest = &t[i..];
        if starts_with_ci(rest, "http://") || starts_with_ci(rest, "https://") {
            return true;
        }
        if t[i] == '#' && digits_at(&t, i + 1) {
            return true;
        }
        if !boundary(i) {
            continue;
        }
        if starts_with_ci(rest, "pr") && digits_at(&t, skip_space(&t, i + 2)) {
            return true;
        }
        if starts_with_ci(rest, "run") {
            let j = skip_space(&t, i + 3);
            if digits_at(&t, j)
                || (starts_with_ci(&t[j..], "id") && digits_at(&t, skip_space(&t, j + 2)))
            {
                return true;
            }
        }
        if starts_with_ci(rest, "verified") && t.get(i + 8).is_none_or(|c| !is_word(*c)) {
            return true;
        }
        // `\b[0-9a-f]{7,40}\b`: a whole word of 7-40 hex characters.
        let end = (i..t.len()).find(|&k| !is_word(t[k])).unwrap_or(t.len());
        let word = &t[i..end];
        if (7..=40).contains(&word.len()) && word.iter().all(|c| c.is_ascii_hexdigit()) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_alternatives() {
        for ok in [
            "see https://x.y/z",
            "commit 7180713a63 landed",
            "done in #1234",
            "merged PR 12",
            "pr12",
            "run id 99",
            "run 5",
            "runid5",
            "Verified on host",
            "x VERIFIED.",
            "abcdef1",
        ] {
            assert!(has_evidence(ok), "{ok}");
        }
    }

    #[test]
    fn evidence_rejects_near_misses() {
        for no in [
            "",
            "closed",
            "sprint 12",
            "unverified",
            "abcdef",
            "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4",
            "xpr12",
            "# 12",
            "rerun 5",
        ] {
            assert!(!has_evidence(no), "{no}");
        }
    }
}
