//! Rank floor for the user-scope rulings leg inside `top_k` (#9143 AC2).
//!
//! Why: `fold_rulings` merges rulings at layer 1 and the caller sorts by score.
//! On a large project palace a ruling that answers the query (about 0.47) loses
//! to many strong project drawers (0.5 to 0.6) and falls out of `top_k` — the
//! live check ranked the Q15 rule 30th. Score alone cannot lift it, so a
//! ruling that answers the query gets a guaranteed rank instead.
//! What: [`ruling_answers_query`] is the relevance condition, chosen at fold
//! time; [`apply_rulings_floor`] runs after the score sort and before the
//! `min_score` filter and `top_k` cut, and lifts each chosen ruling to at most
//! [`RULING_RANK_FLOOR`] plus its order among them. It only moves rulings the
//! fold already admitted, so the `ceil(top_k / 3)` cap still bounds the leg.
//! Test: `tools::recall_rulings_floor_tests`; end to end in
//! `a_ruling_that_answers_the_query_ranks_top_3_in_a_large_noisy_palace`.

use std::collections::HashSet;

use trusty_common::memory_core::retrieval::RecallResult;
use uuid::Uuid;

/// Zero-based rank the best answering ruling is lifted to: rank 3.
///
/// Why: #9143 AC2 asks for the ruling in the top 3. Index 0 is often the
/// project's L0 identity row, and the project's best hit keeps index 1.
pub(crate) const RULING_RANK_FLOOR: usize = 2;

/// Function words dropped before terms are compared.
// #9279: two-letter terms are kept now, so the two-letter function words are
// listed here instead of being dropped by length.
const STOPWORDS: &[&str] = &[
    "about", "after", "all", "also", "and", "any", "are", "but", "can", "could", "did", "does",
    "for", "from", "had", "has", "have", "how", "into", "its", "may", "must", "not", "our",
    "should", "than", "that", "the", "their", "then", "there", "these", "they", "this", "was",
    "were", "what", "when", "where", "which", "who", "why", "will", "with", "would", "you", "your",
    "am", "an", "as", "at", "be", "by", "do", "he", "if", "in", "is", "it", "me", "my", "no", "of",
    "ok", "on", "or", "so", "to", "up", "us", "we",
];

/// The distinct content terms of `text`.
///
/// What: lowercase alphanumeric runs of two or more characters, minus
/// [`STOPWORDS`], with one trailing plural `s` removed from words longer than
/// three bytes (`titles` and `title` are one term; `class` keeps its `ss`).
fn content_terms(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        // #9279: keep two-character ids such as `e1`, `f0` and `fe`.
        .filter(|w| w.chars().count() >= 2)
        .map(str::to_lowercase)
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .map(|w| match w.strip_suffix('s') {
            Some(stem) if w.len() > 3 && !stem.ends_with('s') => stem.to_string(),
            _ => w,
        })
        .collect()
}

/// Whether a ruling's `content` answers `query`.
///
/// Why: the floor overrides score order, so it must fire only for a ruling
/// that is about what was asked; a ruling that merely survived the cap on an
/// unrelated query keeps its score rank. Term coverage is used, not a score
/// threshold, because the fused score's scale depends on the embedder and on
/// whether a BM25 lane ran, and the AC2 ruling scored below every project hit.
/// What: true when the query has content terms and the ruling's content holds
/// at least half of them, and at least two (one, for a one-term query). Tags
/// are not counted: every ruling carries `ruling` or `standing-rule`.
/// Test: `answering_needs_half_the_query_terms_and_at_least_two`,
/// `a_short_id_term_separates_two_rulings`.
pub(crate) fn ruling_answers_query(query: &str, content: &str) -> bool {
    let asked = content_terms(query);
    if asked.is_empty() {
        return false;
    }
    let held = content_terms(content);
    let shared = asked.iter().filter(|t| held.contains(*t)).count();
    shared >= asked.len().min(2) && shared * 2 >= asked.len()
}

/// Lift the answering rulings in `floored` into reserved slots of `top_k`.
///
/// Why: see the module doc. Runs on the score-sorted list, so it overrides
/// the sort only where the sort would bury an answering ruling.
/// What: the i-th ruling of `floored` present in `results`, in list order,
/// moves to `min(current, RULING_RANK_FLOOR + i, top_k - n + i)`, where `n` is
/// how many are present; the last term keeps every one inside `top_k` when
/// `top_k` is small. Nothing is removed, a ruling never moves down, and every
/// other hit keeps its relative order.
/// Test: `a_buried_answering_ruling_is_lifted_to_rank_3`,
/// `a_ruling_already_above_the_floor_stays`,
/// `several_answering_rulings_take_consecutive_slots_inside_a_small_top_k`.
pub(crate) fn apply_rulings_floor(results: &mut Vec<RecallResult>, floored: &[Uuid], top_k: usize) {
    if floored.is_empty() || top_k == 0 {
        return;
    }
    let before = results.len();
    let mut lifted = Vec::new();
    let mut rest = Vec::with_capacity(before);
    for (index, hit) in std::mem::take(results).into_iter().enumerate() {
        if floored.contains(&hit.drawer.id) {
            lifted.push((index, hit));
        } else {
            rest.push(hit);
        }
    }
    let n = lifted.len();
    for (i, (index, hit)) in lifted.into_iter().enumerate() {
        // #9143: reserved slot, never below the hit's own score rank.
        let slot = index
            .min(RULING_RANK_FLOOR + i)
            .min(top_k.saturating_sub(n) + i);
        rest.insert(slot.min(rest.len()), hit);
    }
    *results = rest;
    debug_assert_eq!(results.len(), before, "the floor never adds or drops a hit");
}

#[cfg(test)]
#[path = "recall_rulings_floor_tests.rs"]
mod recall_rulings_floor_tests;
