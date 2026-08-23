//! Text primitives the grounding guardrail is built from.
//!
//! Why: [`super::synthesize_grounding`] crossed the 500-SLOC production cap when
//! #6082 lap 8 added component-anchored classification. These are the natural
//! seam: every item here is a pure function over strings, with no knowledge of
//! findings, reports or reachability tiers, and nothing else in the module
//! depends on them beyond calling them.
//! What: sentence splitting and splicing, subject-token extraction,
//! boundary-aware name matching, and the case-insensitive rewrite pass.
//! Test: `synthesize_grounding_tests.rs` exercises each through the guardrail
//! and directly.

use std::collections::BTreeSet;

use super::synthesize_grounding::{
    GENERIC_TOKENS, MIN_TOKEN_LEN, REACHABILITY_REWRITES, REACHABILITY_WORDS,
};

/// Cut `sentence` out of `text`, taking the space that separated it from its
/// neighbour so the remaining prose keeps single spacing.
pub(super) fn remove_sentence(text: &str, sentence: &str) -> String {
    let spaced = format!("{sentence} ");
    if text.contains(&spaced) {
        return text.replace(&spaced, "");
    }
    text.replace(sentence, "")
}

/// The words that identify one finding's subject: its component path segments
/// and its title words, minus the generic ones.
pub(super) fn subject_tokens(title: &str, component: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for raw in component
        .split(['/', '\\', '.', ':'])
        .chain(title.split_whitespace())
    {
        let word: String = raw
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
            .to_lowercase();
        if word.len() < MIN_TOKEN_LEN || GENERIC_TOKENS.contains(&word.as_str()) {
            continue;
        }
        if word.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        out.insert(word);
    }
    out
}

/// True when `haystack` contains `needle` bounded by non-identifier characters.
///
/// Why: a bare `contains` would match `trusty-mpm` inside `trusty-mpm-gui` and
/// blame the wrong crate.
pub(super) fn contains_name(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = haystack.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    haystack.match_indices(needle).any(|(i, _)| {
        let before_ok = i == 0 || !ident(bytes[i - 1]);
        let after = i + needle.len();
        let after_ok = after >= bytes.len() || !ident(bytes[after]);
        before_ok && after_ok
    })
}

/// Apply every known reachability rewrite to one sentence, case-insensitively.
pub(super) fn rewrite_reachability(sentence: &str) -> String {
    let mut out = sentence.to_string();
    for (phrase, replacement) in REACHABILITY_REWRITES {
        out = replace_ignore_case(&out, phrase, replacement);
    }
    out
}

/// Case-insensitive literal replacement preserving everything else verbatim.
pub(super) fn replace_ignore_case(haystack: &str, needle: &str, replacement: &str) -> String {
    let lower = haystack.to_lowercase();
    let mut out = String::with_capacity(haystack.len());
    let mut cursor = 0usize;
    while let Some(rel) = lower[cursor..].find(needle) {
        let at = cursor + rel;
        out.push_str(&haystack[cursor..at]);
        out.push_str(replacement);
        cursor = at + needle.len();
    }
    out.push_str(&haystack[cursor..]);
    out
}

/// True when a sentence asserts reachability beyond this host.
///
/// A plain substring test, not a word-boundary one: every rewrite in
/// [`REACHABILITY_REWRITES`] removes its own reachability word, so any
/// occurrence left belongs to a phrase this module does not know how to
/// correct — including a hyphenated one like `remote-management` or
/// `network-attached`, which a boundary test would miss.
pub(super) fn asserts_reachability(sentence: &str) -> bool {
    let lower = sentence.to_lowercase();
    REACHABILITY_WORDS.iter().any(|w| lower.contains(w))
}

/// Split prose into sentences on `. `, `? ` and `! ` boundaries, keeping each
/// sentence's own text so the caller can splice a correction back in.
pub(super) fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if !matches!(b, b'.' | b'?' | b'!' | b'\n') {
            continue;
        }
        let next_is_break = bytes.get(i + 1).is_none_or(|n| n.is_ascii_whitespace());
        if !next_is_break {
            continue;
        }
        let piece = text[start..=i].trim();
        if !piece.is_empty() {
            out.push(piece);
        }
        start = i + 1;
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}
