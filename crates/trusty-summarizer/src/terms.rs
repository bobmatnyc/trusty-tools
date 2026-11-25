//! Distinguishing terms per outline section, with no model.
//!
//! Why: step 7. In 86 of 92 strong-reader misses the outline entry covering
//! the answer shared no word with the question's subject: a title (the
//! first words of a 60-line window, or a template heading) does not say what
//! the range holds. Listing the names, numbers, dates and rare terms of each
//! range lets a reader pick the right lines.
//!
//! What: each section's text is scanned for three kinds of candidate:
//! capitalised names (runs of up to three capitalised words), numbers and
//! dates (any token holding a digit, at least three characters long), and
//! other words of at least five letters that are not stop words. A
//! candidate's score is its count in the section times its inverse section
//! frequency (`ln((N + 1) / df)`), times 3 for a name and 2 for a number. A
//! candidate in more than half the sections of a document of four or more
//! sections, or already in the title, is dropped. The best candidates are
//! kept until `max_terms` terms or `max_chars` characters (joined with
//! `", "`) would be passed. The design uses only the document itself; no
//! question or answer key was used to choose the rules or weights.

use std::collections::HashMap;

use crate::segment::Section;
use crate::text::LineIndex;

/// Limits on the terms of one outline entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermLimits {
    /// At most this many terms per entry.
    pub max_terms: usize,
    /// The terms joined with `", "` are at most this many characters.
    pub max_chars: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Name,
    Number,
    Word,
}

impl Kind {
    fn weight(self) -> f64 {
        match self {
            Kind::Name => 3.0,
            Kind::Number => 2.0,
            Kind::Word => 1.0,
        }
    }
}

struct Candidate {
    text: String,
    kind: Kind,
    count: usize,
    first: usize,
}

/// The distinguishing terms of each section, in section order.
///
/// Why: see the module doc.
/// What: returns one vector per section; each holds at most
/// `limits.max_terms` terms whose `", "`-joined length is at most
/// `limits.max_chars` characters, best first. Deterministic.
/// Test: `terms_name_what_each_section_holds`, `terms_respect_limits`
pub fn section_terms(text: &str, sections: &[Section], limits: TermLimits) -> Vec<Vec<String>> {
    let index = LineIndex::new(text);
    let per_section: Vec<Vec<Candidate>> = sections
        .iter()
        .map(|s| {
            let mut body = String::new();
            for n in s.lines.start..=s.lines.end.min(index.line_count()) {
                body.push_str(index.line(n));
                body.push('\n');
            }
            candidates(&body)
        })
        .collect();
    let mut df: HashMap<String, usize> = HashMap::new();
    for cands in &per_section {
        for c in cands {
            *df.entry(c.text.to_lowercase()).or_default() += 1;
        }
    }
    let n = sections.len();
    per_section
        .into_iter()
        .zip(sections)
        .map(|(cands, section)| pick(cands, &section.title, &df, n, limits))
        .collect()
}

fn pick(
    cands: Vec<Candidate>,
    title: &str,
    df: &HashMap<String, usize>,
    n: usize,
    limits: TermLimits,
) -> Vec<String> {
    let title = title.to_lowercase();
    let mut scored: Vec<(f64, usize, Candidate)> = cands
        .into_iter()
        .filter_map(|c| {
            let key = c.text.to_lowercase();
            let d = df.get(&key).copied().unwrap_or(1);
            if n >= 4 && d * 2 > n {
                return None;
            }
            if title.contains(&key) {
                return None;
            }
            let idf = ((n as f64 + 1.0) / d as f64).ln();
            let score = c.count as f64 * idf * c.kind.weight();
            Some((score, c.first, c))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut out: Vec<String> = Vec::new();
    let mut used = 0usize;
    for (_, _, c) in scored {
        if out.len() >= limits.max_terms {
            break;
        }
        let low = c.text.to_lowercase();
        if out.iter().any(|t| t.to_lowercase().contains(&low)) {
            continue;
        }
        let add = c.text.chars().count() + if out.is_empty() { 0 } else { 2 };
        if used + add > limits.max_chars {
            continue;
        }
        used += add;
        out.push(c.text);
    }
    out
}

/// Tokens of a line: runs of letters and digits, keeping `.`, `,`, `/`, `-`
/// and `:` between two digits (`12,345`, `3.5`, `2026-11-14`) and `-`
/// between letters and digits (`SP-800`).
pub(crate) fn tokens(line: &str) -> Vec<(usize, &str)> {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].1.is_alphanumeric() {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i + 1;
        while j < chars.len() {
            let c = chars[j].1;
            if c.is_alphanumeric() {
                j += 1;
                continue;
            }
            let joins = matches!(c, '.' | ',' | '/' | '-' | ':')
                && j + 1 < chars.len()
                && chars[j - 1].1.is_ascii_digit()
                && chars[j + 1].1.is_ascii_digit();
            let hyphen_code = c == '-'
                && j + 1 < chars.len()
                && chars[j + 1].1.is_ascii_digit()
                && chars[j - 1].1.is_uppercase();
            if joins || hyphen_code {
                j += 1;
                continue;
            }
            break;
        }
        let b = chars[start].0;
        let e = chars.get(j).map_or(line.len(), |c| c.0);
        out.push((b, &line[b..e]));
        i = j;
    }
    out
}

fn candidates(body: &str) -> Vec<Candidate> {
    let mut map: HashMap<String, usize> = HashMap::new();
    let mut list: Vec<Candidate> = Vec::new();
    let mut order = 0usize;
    let mut add = |text: String, kind: Kind, list: &mut Vec<Candidate>| {
        order += 1;
        match map.get(&text) {
            Some(&i) => list[i].count += 1,
            None => {
                map.insert(text.clone(), list.len());
                list.push(Candidate {
                    text,
                    kind,
                    count: 1,
                    first: order,
                });
            }
        }
    };
    for line in body.lines() {
        let toks = tokens(line);
        let mut run: Vec<&str> = Vec::new();
        let flush = |run: &mut Vec<&str>,
                     list: &mut Vec<Candidate>,
                     add: &mut dyn FnMut(String, Kind, &mut Vec<Candidate>)| {
            while run.first().is_some_and(|w| is_stop(w)) {
                run.remove(0);
            }
            while run.last().is_some_and(|w| is_stop(w)) {
                run.pop();
            }
            if !run.is_empty() {
                let name = run.join(" ");
                if name.chars().count() >= 3 {
                    add(name, Kind::Name, list);
                }
            }
            run.clear();
        };
        for &(_, tok) in &toks {
            let has_digit = tok.chars().any(|c| c.is_ascii_digit());
            let capital = tok.chars().next().is_some_and(char::is_uppercase)
                && tok.chars().any(char::is_lowercase)
                && !has_digit;
            let acronym = tok.chars().count() >= 2
                && tok
                    .chars()
                    .all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '-')
                && tok.chars().any(char::is_alphabetic);
            if capital || acronym {
                run.push(tok);
                if run.len() == 3 {
                    flush(&mut run, &mut list, &mut add);
                }
                continue;
            }
            flush(&mut run, &mut list, &mut add);
            if has_digit {
                if tok.chars().count() >= 3 {
                    add(tok.to_string(), Kind::Number, &mut list);
                }
            } else if tok.chars().count() >= 5 && !is_stop(tok) {
                add(tok.to_lowercase(), Kind::Word, &mut list);
            }
        }
        flush(&mut run, &mut list, &mut add);
    }
    list
}

/// Common English words that name nothing.
const STOP: &[&str] = &[
    "a",
    "about",
    "above",
    "after",
    "again",
    "all",
    "also",
    "an",
    "and",
    "any",
    "are",
    "as",
    "at",
    "be",
    "because",
    "been",
    "before",
    "being",
    "below",
    "between",
    "both",
    "but",
    "by",
    "can",
    "could",
    "did",
    "do",
    "does",
    "each",
    "either",
    "even",
    "every",
    "for",
    "from",
    "further",
    "had",
    "has",
    "have",
    "having",
    "he",
    "her",
    "here",
    "hers",
    "him",
    "his",
    "how",
    "however",
    "i",
    "if",
    "in",
    "into",
    "is",
    "it",
    "its",
    "just",
    "may",
    "me",
    "might",
    "more",
    "most",
    "much",
    "must",
    "my",
    "no",
    "nor",
    "not",
    "now",
    "of",
    "on",
    "once",
    "one",
    "only",
    "or",
    "other",
    "our",
    "out",
    "over",
    "own",
    "same",
    "shall",
    "she",
    "should",
    "so",
    "some",
    "such",
    "than",
    "that",
    "the",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "through",
    "to",
    "too",
    "under",
    "until",
    "up",
    "upon",
    "very",
    "was",
    "we",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "who",
    "whom",
    "why",
    "will",
    "with",
    "would",
    "yes",
    "yet",
    "you",
    "your",
    "about",
    "against",
    "among",
    "another",
    "around",
    "first",
    "second",
    "third",
    "since",
    "still",
    "though",
    "thus",
    "therefore",
    "within",
    "without",
    "whether",
    "where",
    "never",
    "always",
    "often",
    "rather",
    "quite",
    "though",
    "these",
    "those",
    "there",
    "thing",
    "things",
    "something",
    "nothing",
    "anything",
    "little",
    "great",
    "every",
    "being",
    "which",
    "could",
    "would",
    "should",
    "shall",
    "itself",
    "himself",
    "herself",
    "themselves",
    "yourself",
    "myself",
    "ourselves",
];

fn is_stop(word: &str) -> bool {
    let low = word.to_lowercase();
    STOP.contains(&low.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::{SegmentOptions, segment};

    fn opts() -> SegmentOptions {
        SegmentOptions {
            markdown: true,
            max_section_lines: 60,
            hard_bound: true,
        }
    }

    #[test]
    fn terms_name_what_each_section_holds() {
        // Template headings that name nothing; the bodies differ.
        let text = "# Part\nThe Alder Annex holds 1,240 boxes since 2019-03-04.\n\n\
                    # Part\nCopper wiring passed inspection under Inspector Malone.\n\n\
                    # Part\nThe Birch Hall budget is 52,000 dollars for heating.\n\n\
                    # Part\nNothing notable; the usual weekly heating notes.\n";
        let sections = segment(text, opts());
        let terms = section_terms(
            text,
            &sections,
            TermLimits {
                max_terms: 6,
                max_chars: 100,
            },
        );
        assert_eq!(terms.len(), 4);
        assert!(terms[0].iter().any(|t| t == "Alder Annex"), "{terms:?}");
        assert!(terms[0].iter().any(|t| t == "1,240"), "{terms:?}");
        assert!(terms[0].iter().any(|t| t == "2019-03-04"), "{terms:?}");
        assert!(
            terms[1].iter().any(|t| t == "Inspector Malone"),
            "{terms:?}"
        );
        assert!(terms[2].iter().any(|t| t == "Birch Hall"), "{terms:?}");
        // The title word is not repeated.
        assert!(terms.iter().flatten().all(|t| t != "Part"));
    }

    #[test]
    fn terms_respect_limits() {
        let mut text = String::new();
        for s in 0..10 {
            text.push_str(&format!("# S{s}\n"));
            for w in 0..40 {
                text.push_str(&format!("Zeta{s}x{w} Omega{s}y{w} value {s}{w}00\n"));
            }
            text.push('\n');
        }
        let sections = segment(&text, opts());
        for (max_terms, max_chars) in [(3, 1000), (50, 40), (0, 100)] {
            let terms = section_terms(
                &text,
                &sections,
                TermLimits {
                    max_terms,
                    max_chars,
                },
            );
            for t in &terms {
                assert!(t.len() <= max_terms, "{t:?}");
                assert!(t.join(", ").chars().count() <= max_chars, "{t:?}");
            }
        }
    }

    #[test]
    fn tokens_keep_numbers_and_codes_whole() {
        let got: Vec<&str> = tokens("Pay 12,345.50 on 2026-11-14 per SP-800, rule 3.")
            .into_iter()
            .map(|t| t.1)
            .collect();
        assert_eq!(
            got,
            [
                "Pay",
                "12,345.50",
                "on",
                "2026-11-14",
                "per",
                "SP-800",
                "rule",
                "3"
            ]
        );
    }
}
