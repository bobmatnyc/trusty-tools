//! Text primitives the grounding guardrail is built from.
//!
//! Why: [`super::synthesize_grounding`] crossed the 500-SLOC production cap when
//! #6082 lap 8 added component-anchored classification. These are the natural
//! seam: every item here is a pure function over strings, with no knowledge of
//! findings, reports or reachability tiers, and nothing else in the module
//! depends on them beyond calling them.
//! What: sentence splitting and splicing, subject-token extraction,
//! boundary-aware name matching, the case-insensitive rewrite pass, and the
//! grammar check that decides whether a rewritten sentence may ship.
//! Test: `synthesize_grounding_tests.rs` exercises each through the guardrail
//! and directly.

use std::collections::BTreeSet;

use super::synthesize_grounding::{
    GENERIC_TOKENS, MIN_TOKEN_LEN, REACHABILITY_REWRITES, REACHABILITY_WORDS,
};

/// The nouns and verbs a reachability word must be near to be making a CLAIM
/// about the finding's surface (#6082 lap 9).
///
/// Why: tier 3 in [`super::synthesize_grounding`] has no evidence either way, so
/// it withholds rather than corrects, and withholding a field the reader would
/// have kept is a real cost. What separates "a critical remote-execution risk"
/// from "a transient network error" is not the word `network` — both carry one —
/// but whether that word is attributing access, attack, execution or exposure to
/// the surface. These are the words that attribute it.
///
/// What: matched as whole tokens, because a prefix match on `access` would take
/// `accessory` with it. The reachability word itself is matched by PREFIX
/// instead — see [`asserts_reachability_claim`] — so `remotely`, `networked` and
/// a hyphenated `remote-execution` all count.
pub(super) const CLAIM_WORDS: &[&str] = &[
    "access",
    "accessed",
    "accessible",
    "attack",
    "attacker",
    "attackers",
    "attacks",
    "breach",
    "compromise",
    "compromised",
    "control",
    "controlled",
    "escalation",
    "execute",
    "executed",
    "executes",
    "execution",
    "exploit",
    "exploitable",
    "exploitation",
    "exploited",
    "expose",
    "exposed",
    "exposes",
    "exposure",
    "hijack",
    "hijacked",
    "injection",
    "intrusion",
    "rce",
    "reach",
    "reachable",
    "reached",
    "reaching",
    "takeover",
    "tamper",
    "unauthenticated",
    "unauthorised",
    "unauthorized",
];

/// How many tokens either side of a reachability word [`CLAIM_WORDS`] is
/// searched over.
///
/// Three covers every claim shape the graded reports produced — the compound
/// (`remote-execution`), the modifier (`network-reachable attacker`) and the
/// short prepositional phrase (`exploitable over the network`) — while leaving a
/// claim word in a different clause of the same sentence out of reach, which is
/// what keeps "the network error path raises maintenance cost and the risk that
/// a new endpoint mishandles an error status" a clean sentence.
const CLAIM_WINDOW: usize = 3;

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

/// Determiners a replacement may open with.
///
/// A replacement starting with one of these cannot follow an article: splicing
/// `any local process` in where `remote attacker` stood leaves `a any local
/// process` unless the article in front is consumed with the phrase (#6082 lap
/// 10).
const LEADING_DETERMINERS: &[&str] = &["a", "an", "the", "any", "every", "each", "some", "no"];

/// The articles [`splice_ignore_case`] consumes ahead of such a replacement.
const CONSUMED_ARTICLES: &[&str] = &["a", "an", "the"];

/// Contrast joiners a rewritten sentence is read across for a self-contrast.
const CONTRAST_MARKERS: &[&str] = &[" rather than ", " instead of "];

/// Phrases that place a surface on this host.
///
/// Read only on a sentence the rewrite pass changed, and only to ask whether
/// both sides of a contrast landed in the same class.
const HOST_LOCAL_PHRASES: &[&str] = &[
    "on the host",
    "on this host",
    "any local process",
    "local process",
    "local-process",
    "host-local",
    "localhost",
    "loopback",
    "local socket",
    "local-socket",
];

/// Apply every known reachability rewrite to one sentence, case-insensitively.
pub(super) fn rewrite_reachability(sentence: &str) -> String {
    let mut out = sentence.to_string();
    for (phrase, replacement) in REACHABILITY_REWRITES {
        out = splice_ignore_case(&out, phrase, replacement);
    }
    out
}

/// The grammatical debris a rewrite left behind, as the reason to withhold.
///
/// Why: [`rewrite_reachability`] substitutes a phrase into a sentence it did not
/// write, and the graded lap-10 report shipped the result — "reachable by any
/// process on the host rather than a any local process". Two things went wrong
/// at once: the replacement doubled the article in front of it, and the
/// corrected clause contrasted host-local reach with host-local reach, which
/// tells a reader nothing. Neither is repairable from here, so a tripped check
/// falls back to the withhold path the module already uses for a claim it cannot
/// correct — a visible withholding reads better than shipped nonsense.
/// What: `Some(reason)` for two adjacent determiners, or for a contrast whose
/// two sides carry the same reachability class. `None` when the sentence reads
/// cleanly. Run on the REWRITTEN sentence only, so prose the module left alone
/// is never judged by it.
/// Test: `synthesize_grounding_tests.rs::{a_rewrite_never_ships_a_doubled_article,
/// a_rewrite_that_contrasts_a_class_with_itself_is_withheld}`.
pub(super) fn grammar_debris(sentence: &str) -> Option<String> {
    if let Some(pair) = doubled_determiner(sentence) {
        return Some(format!("the correction left '{pair}' in the sentence"));
    }
    if contrasts_one_class_with_itself(sentence) {
        return Some(
            "the correction left both sides of a contrast describing the same reachability"
                .to_string(),
        );
    }
    None
}

/// The first pair of adjacent determiners in a sentence, lower-cased.
fn doubled_determiner(sentence: &str) -> Option<String> {
    let raw: Vec<&str> = sentence.split_whitespace().collect();
    raw.windows(2).find_map(|pair| {
        // Punctuation between the two words means they are not one noun phrase.
        if !pair[0].ends_with(|c: char| c.is_ascii_alphanumeric()) {
            return None;
        }
        let (first, second) = (word_of(pair[0])?, word_of(pair[1])?);
        let is_det = |w: &String| LEADING_DETERMINERS.contains(&w.as_str());
        (is_det(&first) && is_det(&second)).then(|| format!("{first} {second}"))
    })
}

/// True when a contrast in `sentence` has the same reachability class on both
/// sides, so it draws no distinction.
fn contrasts_one_class_with_itself(sentence: &str) -> bool {
    let lower = sentence.to_lowercase();
    CONTRAST_MARKERS.iter().any(|marker| {
        let Some(at) = lower.find(marker) else {
            return false;
        };
        let before = reach_class(&lower[..at]);
        before.is_some() && before == reach_class(&lower[at + marker.len()..])
    })
}

/// Which reachability class a clause describes, if it names one at all.
///
/// `Some(true)` is beyond this host and `Some(false)` is on it; a reachability
/// word wins, because a clause carrying one is making the wider claim.
fn reach_class(clause: &str) -> Option<bool> {
    if REACHABILITY_WORDS.iter().any(|w| clause.contains(w)) {
        return Some(true);
    }
    HOST_LOCAL_PHRASES
        .iter()
        .any(|p| clause.contains(p))
        .then_some(false)
}

/// One whitespace-separated word reduced to its lower-cased letters, or `None`
/// when nothing is left.
fn word_of(raw: &str) -> Option<String> {
    let word: String = raw
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_lowercase();
    (!word.is_empty()).then_some(word)
}

/// Replace `needle` with `replacement`, consuming an article the replacement's
/// own determiner would double.
///
/// Why: the rewrite table's replacements were written as standalone noun
/// phrases, and the sentences they land in already carry an article — so
/// `by a remote attacker` became `by a any local process` (#6082 lap 10).
/// What: [`replace_ignore_case`], plus: when the replacement opens with a
/// [`LEADING_DETERMINERS`] word, any [`CONSUMED_ARTICLES`] word immediately
/// before the match is dropped, and its capitalization carries onto the
/// replacement so a sentence-initial article does not lower-case the sentence.
/// Test: `synthesize_grounding_tests.rs::a_rewrite_consumes_the_article_its_replacement_doubles`.
fn splice_ignore_case(haystack: &str, needle: &str, replacement: &str) -> String {
    let doubles = replacement
        .split_whitespace()
        .next()
        .and_then(word_of)
        .is_some_and(|w| LEADING_DETERMINERS.contains(&w.as_str()));
    if !doubles {
        return replace_ignore_case(haystack, needle, replacement);
    }
    let lower = haystack.to_lowercase();
    let mut out = String::with_capacity(haystack.len());
    let mut cursor = 0usize;
    while let Some(rel) = lower[cursor..].find(needle) {
        let at = cursor + rel;
        out.push_str(&haystack[cursor..at]);
        match take_trailing_article(&mut out) {
            true => out.push_str(&capitalize(replacement)),
            false => out.push_str(replacement),
        }
        cursor = at + needle.len();
    }
    out.push_str(&haystack[cursor..]);
    out
}

/// Drop a trailing article from `out`; `true` when the one dropped was
/// capitalized, so the caller capitalizes what replaces it.
fn take_trailing_article(out: &mut String) -> bool {
    let trimmed = out.trim_end();
    if trimmed.len() == out.len() {
        return false; // the match abuts the previous word: no article to take
    }
    let Some(last) = trimmed.split_whitespace().next_back() else {
        return false;
    };
    if !CONSUMED_ARTICLES.contains(&last.to_lowercase().as_str()) {
        return false;
    }
    let capitalized = last.starts_with(|c: char| c.is_ascii_uppercase());
    out.truncate(trimmed.len() - last.len());
    capitalized
}

/// Upper-case the first character of `text`.
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
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

/// True when a sentence uses a reachability word to CLAIM reach, rather than
/// merely mentioning a network (#6082 lap 9).
///
/// Why: [`asserts_reachability`] is the right trigger where the report has
/// evidence the surface is host-local — every occurrence there contradicts the
/// data. Tier 3 has no evidence either way, so it must tell a claim from a
/// mention, or every finding whose prose says "a transient network error" loses
/// its field. The lap-9 report shipped the opposite failure: tier 3 asked
/// whether [`REACHABILITY_REWRITES`] could rewrite the sentence, and
/// `remote-execution` matches no pattern, so §5.1 item 3's business impact went
/// out claiming remote code execution on a surface with no evidence of reach.
///
/// What: splits the sentence on non-alphanumerics, so a hyphenated compound
/// becomes two tokens and `remote-execution` reads as `remote` beside
/// `execution`. A token counts as a reachability word when it STARTS with one,
/// which admits `remotely`, `networked` and `networking` without a stem table.
/// It is a claim when a [`CLAIM_WORDS`] token sits within [`CLAIM_WINDOW`]
/// tokens of it.
///
/// Test: `synthesize_grounding_tests.rs::{an_unevidenced_component_rejects_a_hyphenated_reach_compound,
/// an_unevidenced_component_rejects_reachability_word_variants,
/// an_unevidenced_component_keeps_a_non_reach_mention_of_the_network}`.
pub(super) fn asserts_reachability_claim(sentence: &str) -> bool {
    let words: Vec<String> = sentence
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    words.iter().enumerate().any(|(i, word)| {
        if !REACHABILITY_WORDS.iter().any(|r| word.starts_with(r)) {
            return false;
        }
        let from = i.saturating_sub(CLAIM_WINDOW);
        let to = (i + CLAIM_WINDOW + 1).min(words.len());
        words[from..to]
            .iter()
            .any(|near| CLAIM_WORDS.contains(&near.as_str()))
    })
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
