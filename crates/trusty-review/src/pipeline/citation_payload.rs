//! Which backtick spans in a finding's prose quote its cited file (#10964).
//!
//! Why: both citation checks read every backtick span in a finding's
//! description and consequence as code copied from the cited file. A reviewer
//! also backtick-quotes text that is not in the file on purpose: an exploit
//! payload (`' OR '1'='1' --`), an example input, or its own proposed fix
//! (`authed = row is not None`). Both checks withheld true findings for it.
//! What: [`split_backtick_spans`] sorts each closed backtick span into a file
//! quote or an illustration. A span is an illustration only on an explicit
//! cue: a cue phrase ([`BEFORE_CUES`]) ending the prose before it in the same
//! clause, the word `instead` right after it, or text equal to the finding's
//! own `suggested_replacement` or to a span its `suggestion` quotes. Absence
//! from the file never makes a span an illustration; the checks still need a
//! verified file quote to place the finding.
//! Test: the `#10964` sections of `citation_gate_tests.rs` and
//! `citation_check_tests.rs`; `split_backtick_spans_reads_only_explicit_cues`.

use crate::models::Finding;
use crate::pipeline::citation_check::{collect_delimited, normalize};

/// Phrases that, ending the prose before a span, label it as a payload, an
/// example input or a proposed fix. Matched lowercase at a word boundary,
/// with or without a trailing `:` or `,`.
const BEFORE_CUES: &[&str] = &[
    "payload",
    "payloads",
    "attack string",
    "exploit string",
    "input such as",
    "inputs such as",
    "input like",
    "input:",
    "value such as",
    "values such as",
    "e.g.",
    "eg.",
    "for example",
    "for instance",
    "example:",
    "suggest",
    "suggests",
    "suggested",
    "suggestion:",
    "recommend",
    "replace with",
    "replace it with",
    "replace this with",
    "replaced with",
    "change to",
    "change it to",
    "should be",
    "should read",
    "should become",
    "fix:",
    "the fix is",
    "rewrite as",
    "rewrite it as",
];

/// Words that may sit between a cue and its span ("suggest using `x`",
/// "a payload such as `x`"). Any other word ends the cue's reach.
const GAP_WORDS: &[&str] = &[
    "such",
    "as",
    "like",
    "to",
    "use",
    "using",
    "a",
    "an",
    "the",
    "it",
    "with",
    "something",
];

/// The most [`GAP_WORDS`] a cue may reach across.
const MAX_GAP_WORDS: usize = 3;

/// A finding's backtick spans, split by what they quote (#10964).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct BacktickSpans {
    /// Spans the finding presents as code copied from its cited file.
    pub(crate) file: Vec<String>,
    /// Spans the finding labels as a payload, an example input or a fix.
    pub(crate) illustrative: Vec<String>,
}

/// Split the closed backtick spans of `text`, one of `f`'s prose fields with
/// its bracket citations already removed, into file quotes and
/// illustrations. Spans are whitespace-normalized; an unterminated trailing
/// backtick opens no span.
/// Test: `split_backtick_spans_reads_only_explicit_cues`.
pub(crate) fn split_backtick_spans(f: &Finding, text: &str) -> BacktickSpans {
    let fixes = own_fixes(f);
    let segments: Vec<&str> = text.split('`').collect();
    // An even segment count means the last backtick is unmatched.
    let closed = segments.len() - usize::from(segments.len().is_multiple_of(2));
    let mut out = BacktickSpans::default();
    for i in (1..closed).step_by(2) {
        let span = normalize(segments[i]);
        if span.is_empty() {
            continue;
        }
        let after = segments.get(i + 1).copied().unwrap_or("");
        if cued_before(segments[i - 1]) || cued_after(after) || fixes.contains(&span) {
            out.illustrative.push(span);
        } else {
            out.file.push(span);
        }
    }
    out
}

/// The fix text the finding itself proposes: its whole `suggested_replacement`
/// and every backtick span of its `suggestion`.
fn own_fixes(f: &Finding) -> Vec<String> {
    let mut fixes = Vec::new();
    if let Some(replacement) = f.suggested_replacement.as_deref() {
        fixes.push(normalize(replacement));
    }
    collect_delimited(&f.suggestion, '`', &mut fixes);
    fixes.retain(|s| !s.is_empty());
    fixes
}

/// Whether `prose` ends with a cue, reaching back over at most
/// [`MAX_GAP_WORDS`] gap words and never across a sentence break.
fn cued_before(prose: &str) -> bool {
    let lower = prose.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    for gap in 0..=MAX_GAP_WORDS.min(words.len()) {
        let (head, skipped) = words.split_at(words.len() - gap);
        if skipped.iter().any(|w| !GAP_WORDS.contains(w)) {
            return false;
        }
        if ends_with_cue(&head.join(" ")) {
            return true;
        }
    }
    false
}

/// Whether `head` ends with a cue at a word boundary.
fn ends_with_cue(head: &str) -> bool {
    let bare = head.trim_end_matches([':', ',']);
    BEFORE_CUES.iter().any(|cue| {
        [head, bare].iter().any(|h| {
            h.strip_suffix(cue)
                .is_some_and(|rest| !rest.ends_with(|c: char| c.is_alphanumeric()))
        })
    })
}

/// Whether the prose right after a span opens with `instead` ("use `x`
/// instead").
fn cued_after(prose: &str) -> bool {
    let lower = prose.trim_start().to_lowercase();
    lower
        .strip_prefix("instead")
        .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Effort;

    fn spans(text: &str) -> BacktickSpans {
        let f = Finding::new("a.py", "k", text, "", 0.5, Effort::Low);
        split_backtick_spans(&f, text)
    }

    /// #10964: only an explicit cue next to a span labels it; a cue in an
    /// earlier sentence, a cue past a non-gap word, or a span merely absent
    /// from any file stays a file quote.
    #[test]
    fn split_backtick_spans_reads_only_explicit_cues() {
        for text in [
            "a payload such as `x = 1`",
            "an input such as `x = 1`",
            "(e.g. `x = 1`)",
            "Suggest using `x = 1`.",
            "Fix: `x = 1`",
            "It should be `x = 1`.",
            "replace it with `x = 1`",
            "use `x = 1` instead.",
        ] {
            let s = spans(text);
            assert_eq!(s.illustrative, vec!["x = 1".to_string()], "{text}");
            assert!(s.file.is_empty(), "{text}");
        }
        for text in [
            "A payload. `x = 1` runs.",
            "The payload reaches the query via `x = 1`.",
            "`x = 1` runs.",
            "the prefix `x = 1` is instead-of-something",
            "unpayload `x = 1`",
        ] {
            let s = spans(text);
            assert_eq!(s.file, vec!["x = 1".to_string()], "{text}");
            assert!(s.illustrative.is_empty(), "{text}");
        }
    }

    /// #10964: a span equal to the finding's own proposed fix is a fix.
    #[test]
    fn a_span_equal_to_the_findings_own_fix_is_illustrative() {
        let mut f = Finding::new("a.py", "k", "", "use `x = 2`", 0.5, Effort::Low);
        f.suggested_replacement = Some("authed = row is not None".to_string());
        let s = split_backtick_spans(&f, "`authed = row is not None` and `x = 2` and `x = 1`");
        assert_eq!(s.illustrative, vec!["authed = row is not None", "x = 2"]);
        assert_eq!(s.file, vec!["x = 1"]);
    }
}
