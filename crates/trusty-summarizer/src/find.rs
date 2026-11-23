//! Where a term or phrase occurs in a document's text, with no model.
//!
//! Why: step 7. Of 92 arm C misses by strong readers, 65 requested lines but
//! never reached the range holding the answer. A reader that can ask where
//! the question's subject occurs can go straight to those lines.
//!
//! What: the query and every line are cut into words (letters and digits;
//! `12,345`, `3.5` and `2026-11-14` stay one word), lowercased, with
//! thousands separators removed and a light English suffix strip (`-s`,
//! `-es`, `-ies`, `-ing`, `-ed`, `'s`), so `Annexes`, `annex` and `annex's`
//! match and `17` matches `17` but not `170`. Hits come in three ranks,
//! best first (B2), each hit tagged with its rank as `match`:
//!
//! 1. `phrase`: the query's words in order, consecutive, possibly running
//!    over a line break; the hit is the line holding the first word.
//! 2. `all_terms`: the query's content words (stop words dropped) all
//!    within a window of three lines; the hit is the window's first line.
//! 3. `some_terms`: for a query of two or more content words, a line holding at
//!    least half of its content words, more words first. A lower share is
//!    no hit, so a query whose subject is absent still answers `none`.
//!
//! A line inside a better hit is not a hit again. Within a rank, hits are in
//! line order. `mode` is the best rank found, or `none` with no hit; the
//! result still counts the lines holding each term.
//!
//! The output is bounded: at most `max_hits` hits, each with `context`
//! lines before and after, each line cut to `max_line_chars` characters
//! (0: whole lines);
//! `total` counts every hit before the cut.

use serde::{Deserialize, Serialize};

use crate::segment::truncate_chars;
use crate::terms::tokens;
use crate::text::{LineIndex, Span};

/// Bounds on a find result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindOptions {
    /// At most this many hits are returned.
    pub max_hits: usize,
    /// Lines of context before and after each hit.
    pub context: usize,
    /// Each returned line is cut to this many characters; 0 keeps whole
    /// lines (B1).
    pub max_line_chars: usize,
}

/// How the hits were found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    /// The words in order.
    Phrase,
    /// Every content word within three lines.
    AllTerms,
    /// Some of the content words on one line.
    SomeTerms,
    /// Nothing matched.
    None,
}

/// One hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    /// The line that matched.
    pub line: usize,
    /// How this hit matched (B2).
    #[serde(rename = "match")]
    pub matched: MatchMode,
    /// The lines shown, `[start, end]`.
    pub lines: Span,
    /// The shown lines, each `"<n>: <text>"`, joined with `\n`.
    pub text: String,
}

/// Lines holding one query term.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermCount {
    /// The term as normalised.
    pub term: String,
    /// Lines that hold it.
    pub lines: usize,
}

/// The answer to one find request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindResult {
    /// The query as given.
    pub query: String,
    /// How the hits were found.
    pub mode: MatchMode,
    /// Hits before the `max_hits` cut.
    pub total: usize,
    /// The hits, best rank first, then line order; at most `max_hits`.
    pub hits: Vec<Hit>,
    /// Per content term, how many lines hold it.
    pub terms: Vec<TermCount>,
}

/// The normalised form of one word (see the module doc).
pub(crate) fn normalise(word: &str) -> String {
    let mut w: String = word
        .chars()
        .filter(|c| *c != ',')
        .flat_map(char::to_lowercase)
        .collect();
    if w.chars().any(|c| c.is_ascii_digit()) {
        return w;
    }
    for suffix in ["'s", "’s"] {
        if let Some(s) = w.strip_suffix(suffix) {
            w = s.to_string();
        }
    }
    let n = w.chars().count();
    if n > 4 && w.ends_with("ies") {
        w.truncate(w.len() - 3);
        w.push('y');
    } else if n > 4
        && (w.ends_with("sses")
            || w.ends_with("xes")
            || w.ends_with("ches")
            || w.ends_with("shes")
            || w.ends_with("zes"))
    {
        w.truncate(w.len() - 2);
    } else if n > 5 && w.ends_with("ing") {
        w.truncate(w.len() - 3);
    } else if n > 4 && w.ends_with("ed") {
        w.truncate(w.len() - 2);
    } else if n > 3
        && w.ends_with('s')
        && !w.ends_with("ss")
        && !w.ends_with("us")
        && !w.ends_with("is")
    {
        w.truncate(w.len() - 1);
    }
    w
}

fn words(line: &str) -> Vec<String> {
    tokens(line)
        .into_iter()
        .flat_map(|(_, t)| {
            // `SP-800` is one token for the outline; for matching, its
            // parts must also match `SP 800`.
            if t.contains('-') && t.chars().any(char::is_alphabetic) {
                t.split('-').map(normalise).collect::<Vec<_>>()
            } else {
                vec![normalise(t)]
            }
        })
        .filter(|w| !w.is_empty())
        .collect()
}

const FIND_STOP: &[&str] = &[
    "a", "an", "and", "the", "of", "in", "on", "at", "to", "for", "by", "with", "or", "is", "are",
    "was", "were", "be", "as", "from", "that", "this", "it", "its",
];

/// Finds `query` in `text`.
///
/// Why: see the module doc.
/// What: returns phrase hits, then all-terms hits, then some-terms hits
/// ordered by how many content words they hold, or mode `none` with no
/// hit. Postconditions: `hits.len() <= max_hits`,
/// `hits.len() <= total`, every hit's `lines` lies inside the document,
/// every shown line is at most `max_line_chars` characters (whole when 0,
/// a cut line ending in `…`), and an empty
/// or all-punctuation query gives mode `none`.
/// Test: `workers/summarizer-convert/tests/worker.rs` (find_matches_across_formats),
/// `phrase_matches_across_line_breaks_and_inflections`,
/// `numbers_match_whole_and_ignore_separators`, `no_match_is_explicit`,
/// `output_is_bounded`, `width_zero_keeps_whole_lines`,
/// `phrase_hits_rank_above_partial_hits_by_coverage`
pub fn find(text: &str, query: &str, opts: FindOptions) -> FindResult {
    let index = LineIndex::new(text);
    let n_lines = index.line_count();
    let q: Vec<String> = words(query);
    let content: Vec<String> = {
        let c: Vec<String> = q
            .iter()
            .filter(|w| !FIND_STOP.contains(&w.as_str()))
            .cloned()
            .collect();
        if c.is_empty() { q.clone() } else { c }
    };
    let line_words: Vec<Vec<String>> = (1..=n_lines).map(|n| words(index.line(n))).collect();
    let terms = content
        .iter()
        .map(|t| TermCount {
            term: t.clone(),
            lines: line_words.iter().filter(|w| w.contains(t)).count(),
        })
        .collect();

    let found = ranked_hits(&line_words, &q, &content);
    let mode = found.first().map_or(MatchMode::None, |&(_, m)| m);
    let total = found.len();
    let hits = found
        .into_iter()
        .take(opts.max_hits)
        .map(|(line, matched)| {
            let start = line.saturating_sub(opts.context).max(1);
            let end = (line + opts.context).min(n_lines);
            let text = (start..=end)
                .map(|k| {
                    let l = index.line(k).trim_end();
                    // B1: 0 asks for the whole line.
                    if opts.max_line_chars == 0 {
                        format!("{k}: {l}")
                    } else {
                        format!("{k}: {}", truncate_chars(l, opts.max_line_chars))
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            Hit {
                line,
                matched,
                lines: Span::new(start, end),
                text,
            }
        })
        .collect();
    FindResult {
        query: query.to_string(),
        mode,
        total,
        hits,
        terms,
    }
}

const WINDOW: usize = 3;

/// Every hit, best rank first (see the module doc).
fn ranked_hits(
    line_words: &[Vec<String>],
    q: &[String],
    content: &[String],
) -> Vec<(usize, MatchMode)> {
    if q.is_empty() {
        return Vec::new();
    }
    let mut covered = vec![false; line_words.len() + 1];
    let mut out: Vec<(usize, MatchMode)> = Vec::new();
    for (line, last) in phrase_hits(line_words, q) {
        // A phrase run over a line break covers its last line too.
        covered[line..=last].iter_mut().for_each(|c| *c = true);
        out.push((line, MatchMode::Phrase));
    }
    if content.len() > 1 {
        for line in window_hits(line_words, content, WINDOW) {
            let end = (line + WINDOW - 1).min(line_words.len());
            if (line..=end).any(|l| covered[l]) {
                continue;
            }
            covered[line..=end].iter_mut().for_each(|c| *c = true);
            out.push((line, MatchMode::AllTerms));
        }
    }
    // Partial hits need two or more content words; "the ctx" has one.
    // Partial hits need two or more content words; "the ctx" has one.
    if content.len() > 1 {
        let mut partial: Vec<(usize, usize)> = line_words
            .iter()
            .enumerate()
            .filter(|(i, _)| !covered[i + 1])
            .map(|(i, ws)| (i + 1, content.iter().filter(|t| ws.contains(t)).count()))
            .filter(|&(_, n)| n > 0 && 2 * n >= content.len())
            .collect();
        partial.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        out.extend(partial.into_iter().map(|(l, _)| (l, MatchMode::SomeTerms)));
    }
    out
}

/// Lines (1-based) where the words of `q` start, consecutive in the flat
/// word stream, one hit per line, each with the line of its last word.
fn phrase_hits(line_words: &[Vec<String>], q: &[String]) -> Vec<(usize, usize)> {
    let flat: Vec<(usize, &str)> = line_words
        .iter()
        .enumerate()
        .flat_map(|(i, ws)| ws.iter().map(move |w| (i + 1, w.as_str())))
        .collect();
    let mut out: Vec<(usize, usize)> = Vec::new();
    if flat.len() < q.len() {
        return out;
    }
    for i in 0..=flat.len() - q.len() {
        if q.iter().enumerate().all(|(k, w)| flat[i + k].1 == w) {
            let (line, last) = (flat[i].0, flat[i + q.len() - 1].0);
            match out.last_mut() {
                Some(h) if h.0 == line => h.1 = h.1.max(last),
                _ => out.push((line, last)),
            }
        }
    }
    out
}

/// First lines of non-overlapping `window`-line windows that hold every
/// word of `terms`.
fn window_hits(line_words: &[Vec<String>], terms: &[String], window: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < line_words.len() {
        if !terms.iter().any(|t| line_words[i].contains(t)) {
            i += 1;
            continue;
        }
        let end = (i + window).min(line_words.len());
        let all = terms
            .iter()
            .all(|t| line_words[i..end].iter().any(|ws| ws.contains(t)));
        if all {
            out.push(i + 1);
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> FindOptions {
        FindOptions {
            max_hits: 20,
            context: 1,
            max_line_chars: 160,
        }
    }

    #[test]
    fn phrase_matches_across_line_breaks_and_inflections() {
        let text = "intro\nThe Alder\nAnnexes hold boxes.\nother\nalder annex's roof\n";
        let r = find(text, "Alder annex", opts());
        assert_eq!(r.mode, MatchMode::Phrase);
        let lines: Vec<usize> = r.hits.iter().map(|h| h.line).collect();
        assert_eq!(lines, [2, 5]);
        assert_eq!(r.hits[0].lines, Span::new(1, 3));
        assert!(r.hits[0].text.starts_with("1: intro\n2: The Alder"));
        let r = find("they were studying hard\n", "studies", opts());
        assert_eq!(r.mode, MatchMode::Phrase);
        let r = find("the parties agreed\n", "party", opts());
        assert_eq!(r.hits.len(), 1);
    }

    #[test]
    fn all_terms_fall_back_within_three_lines() {
        let text = "budget for Birch\nhall is set\nnothing\nnothing\nBirch only\n";
        let r = find(text, "Birch Hall budget", opts());
        assert_eq!(r.mode, MatchMode::AllTerms);
        // Line 5 holds one word of three: under half, no hit.
        assert_eq!(r.hits.iter().map(|h| h.line).collect::<Vec<_>>(), [1]);
    }

    #[test]
    fn phrase_hits_rank_above_partial_hits_by_coverage() {
        let text = "cycle one\nnothing\nnothing\nnothing\nctx alone\nnothing\nnothing\n\
                    log at ctx cycle path\nnothing\nnothing\nnothing\ncycle and ctx\n";
        let o = FindOptions {
            max_hits: 1,
            ..opts()
        };
        let r = find(text, "ctx-cycle", o);
        assert_eq!(r.mode, MatchMode::Phrase);
        assert_eq!(r.hits[0].line, 8, "the phrase hit comes first");
        let r = find(text, "ctx-cycle", opts());
        let ranked: Vec<(usize, MatchMode)> = r.hits.iter().map(|h| (h.line, h.matched)).collect();
        assert_eq!(
            ranked,
            [
                (8, MatchMode::Phrase),
                (12, MatchMode::AllTerms),
                (1, MatchMode::SomeTerms),
                (5, MatchMode::SomeTerms),
            ]
        );
        // More content words first among partial hits; under half is none.
        let r = find(
            "alder birch\nnothing\nnothing\nnothing\nalder birch cedar\nnothing\nnothing\nnothing\ndome\n",
            "alder birch cedar dome",
            opts(),
        );
        assert_eq!(r.mode, MatchMode::SomeTerms);
        assert_eq!(r.hits.iter().map(|h| h.line).collect::<Vec<_>>(), [5, 1]);
    }

    #[test]
    fn one_content_word_gives_no_partial_hits() {
        // "the" is a stop word: one content word, so no partial flood.
        let text = "ctx alone\nnothing\nthe ctx here\nnothing\nctx again\n";
        let r = find(text, "the ctx", opts());
        assert_eq!(r.mode, MatchMode::Phrase);
        assert_eq!(r.hits.iter().map(|h| h.line).collect::<Vec<_>>(), [3]);
        let r = find(text, "the ctx", opts());
        assert!(r.hits.iter().all(|h| h.matched != MatchMode::SomeTerms));
        let r = find("ctx alone\nnothing\n", "the ctx", opts());
        assert_eq!(r.mode, MatchMode::None);
        assert_eq!(r.total, 0);
    }

    #[test]
    fn numbers_match_whole_and_ignore_separators() {
        let text = "annex 17 here\nannex 170 there\ntotal 12345 units\nrate 3.5 pct\n";
        let r = find(text, "17", opts());
        assert_eq!(r.hits.iter().map(|h| h.line).collect::<Vec<_>>(), [1]);
        let r = find(text, "12,345", opts());
        assert_eq!(r.hits.iter().map(|h| h.line).collect::<Vec<_>>(), [3]);
        let r = find(text, "3.5", opts());
        assert_eq!(r.hits.iter().map(|h| h.line).collect::<Vec<_>>(), [4]);
    }

    #[test]
    fn no_match_is_explicit() {
        let r = find("alpha beta\n", "gamma delta", opts());
        assert_eq!(r.mode, MatchMode::None);
        assert_eq!(r.total, 0);
        assert!(r.hits.is_empty());
        assert_eq!(r.terms.len(), 2);
        assert!(r.terms.iter().all(|t| t.lines == 0));
        assert_eq!(find("alpha\n", "  ?! ", opts()).mode, MatchMode::None);
        assert_eq!(find("", "alpha", opts()).mode, MatchMode::None);
    }

    #[test]
    fn width_zero_keeps_whole_lines() {
        let line = format!("start {} answer", "x".repeat(300));
        let text = format!("{line}\n");
        let whole = FindOptions {
            max_line_chars: 0,
            ..opts()
        };
        let r = find(&text, "answer", whole);
        assert_eq!(r.hits[0].text, format!("1: {line}"));
        let r = find(&text, "answer", opts());
        assert!(r.hits[0].text.ends_with('…'));
        assert_eq!(r.hits[0].text.chars().count(), "1: ".len() + 160);
    }

    #[test]
    fn output_is_bounded() {
        let long = "x".repeat(500);
        let text: String = (0..1000).map(|i| format!("key {i} {long}\n")).collect();
        let o = FindOptions {
            max_hits: 5,
            context: 2,
            max_line_chars: 40,
        };
        let r = find(&text, "key", o);
        assert_eq!(r.total, 1000);
        assert_eq!(r.hits.len(), 5);
        for h in &r.hits {
            assert!(h.lines.len() <= 5);
            for l in h.text.lines() {
                let body = l.split_once(": ").unwrap().1;
                assert!(body.chars().count() <= 40);
            }
        }
    }
}
