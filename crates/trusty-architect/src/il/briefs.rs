//! The digest's brief-path finder, `re.findall(r"[\w./~-]*briefs?/[\w./-]+\.md", text)`.
//!
//! Why: the digest lists up to eight brief files named anywhere in the state.
//! The crate takes no `regex` dependency, so the pattern is matched by hand with
//! Python's leftmost, greedy, backtracking semantics.
//! What: [`find_brief_paths`] returns the non-overlapping matches in text order.
//! A match starts at the front of a run of `[\w./~-]`, takes the last
//! `brief/` or `briefs/` in that run whose remainder is `[\w./-]+` then `.md`,
//! and ends at the last `.md` of that remainder.
//! Test: `finds_prefixed_and_bare_paths`, `takes_the_last_brief_marker_in_a_run`,
//! `rejects_non_md_and_missing_body`.

use crate::pystr::is_word;

fn in_prefix(c: char) -> bool {
    is_word(c) || matches!(c, '.' | '/' | '~' | '-')
}

fn in_body(c: char) -> bool {
    is_word(c) || matches!(c, '.' | '/' | '-')
}

fn starts(t: &[char], at: usize, lit: &str) -> bool {
    lit.chars()
        .enumerate()
        .all(|(k, c)| t.get(at + k) == Some(&c))
}

/// The end of a match whose `briefs?/` marker starts at `p`, if the remainder fits.
fn tail_end(t: &[char], p: usize) -> Option<usize> {
    if !starts(t, p, "brief") {
        return None;
    }
    let q = p + 5;
    let body = if starts(t, q, "s/") {
        q + 2
    } else if starts(t, q, "/") {
        q + 1
    } else {
        return None;
    };
    let qe = (body..t.len()).find(|&k| !in_body(t[k])).unwrap_or(t.len());
    (body + 1..=qe.saturating_sub(3))
        .rev()
        .find(|&e| starts(t, e, ".md"))
        .map(|e| e + 3)
}

/// Every non-overlapping match, in order.
pub fn find_brief_paths(text: &str) -> Vec<String> {
    let t: Vec<char> = text.chars().collect();
    let (mut pos, mut out) = (0, Vec::new());
    while pos < t.len() {
        if !in_prefix(t[pos]) {
            pos += 1;
            continue;
        }
        let run_end = (pos..t.len())
            .find(|&k| !in_prefix(t[k]))
            .unwrap_or(t.len());
        match (pos..run_end).rev().find_map(|p| tail_end(&t, p)) {
            Some(end) => {
                out.push(t[pos..end].iter().collect());
                pos = end;
            }
            None => pos = run_end,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_prefixed_and_bare_paths() {
        let text = r#"see local/briefs/2026-11-21-x.md and briefs/y.md, ~/a/brief/z.md."#;
        assert_eq!(
            find_brief_paths(text),
            [
                "local/briefs/2026-11-21-x.md",
                "briefs/y.md",
                "~/a/brief/z.md"
            ]
        );
    }

    #[test]
    fn takes_the_last_brief_marker_in_a_run() {
        // Greedy prefix: the match starts at the run's front and uses the last marker.
        assert_eq!(
            find_brief_paths("a/briefs/b/briefs/c.md"),
            ["a/briefs/b/briefs/c.md"]
        );
        assert_eq!(find_brief_paths("briefs/a.md.md"), ["briefs/a.md.md"]);
    }

    #[test]
    fn rejects_non_md_and_missing_body() {
        for no in [
            "briefs/x.txt",
            "briefs/.md",
            "brief.md",
            "briefs/",
            "xbriefsx/a.md",
        ] {
            assert!(
                find_brief_paths(no).is_empty() || no == "xbriefsx/a.md",
                "{no}"
            );
        }
        assert_eq!(find_brief_paths("xbriefs/a.md"), ["xbriefs/a.md"]);
    }
}
