//! Exact short-id token boost for L2/L3 recall (#9279).
//!
//! Why: a query that names an id, such as "ruling e1", has to recall the drawer
//! holding that id. The embedding gives a two-character token almost no
//! weight, and the closet tag boost cannot separate the drawers either: every
//! ruling matches "ruling" and earns the same +0.15. In the live supervisor
//! palace the E1 ruling scored 0.208 and ranked 12th under five other rulings
//! at 0.234 to 0.298.
//! What: [`query_id_tokens`] picks the id-shaped tokens out of a query,
//! [`rare_id_tokens`] keeps those few candidates hold, and [`id_token_boost`]
//! adds [`ID_TOKEN_BOOST`] to a candidate whose content holds a rare one as a
//! whole token. A query with no rare id token gets no boost on any candidate,
//! so its ranking is unchanged.
//! Test: `ruling_e1_ranks_its_drawer_first_in_l2_and_l3`,
//! `a_query_without_an_id_token_keeps_its_order`,
//! `a_common_short_word_keeps_similarity_order`,
//! `a_common_short_word_stays_below_the_relevance_floor`, `id_token_shape`,
//! `id_token_boost_matches_whole_normalized_tokens`,
//! `an_id_most_candidates_hold_is_not_rare`.

use crate::memory_core::dream::normalize_keyword;

/// Score added to a candidate whose content holds a query id token (#9279).
///
/// Why: it must clear the vector gap the embedding leaves for an id (0.09 in
/// the live case) with margin, and it sits above the 0.15 closet boost so an
/// exact id match outweighs a topical one. Architect decision, 2026-10-06, on
/// the E11 design.
pub(super) const ID_TOKEN_BOOST: f32 = 0.3;

/// Longest token, in characters, that counts as an id.
const MAX_ID_CHARS: usize = 3;

/// Whether a normalized keyword is shaped like an id.
///
/// What: two characters (`e1`, `v2`, `42`, `fe`), or three characters with a
/// digit (`e12`, `q15`). A three-letter word such as `fix` or `bug` is not an
/// id, so ordinary queries never earn the boost. Stop words are already gone,
/// because callers pass `extract_keywords` output.
pub(super) fn is_id_token(token: &str) -> bool {
    match token.chars().count() {
        2 => true,
        MAX_ID_CHARS => token.chars().any(char::is_numeric),
        _ => false,
    }
}

/// The id-shaped tokens of a query's keyword list.
pub(super) fn query_id_tokens(query_tokens: &[String]) -> Vec<&str> {
    query_tokens
        .iter()
        .map(String::as_str)
        .filter(|t| is_id_token(t))
        .collect()
}

/// Fewest candidates allowed to hold a rare id, whatever the candidate count.
const MIN_HOLDERS: usize = 2;

/// A rare id is held by at most one candidate in this many.
const HOLDER_SHARE: usize = 5;

/// Most candidates that may hold an id token while it still counts as rare.
///
/// Why (#9279 review): a two-character word such as `pm`, `ci` or `pr` is
/// id-shaped but common. Boosting it +0.3 reordered ordinary queries and lifted
/// a 0.05-similarity drawer over the 0.35 hook relevance floor. A real id
/// names one drawer or a few; a common word sits in many of the candidates.
/// What: `max(2, candidates / 5)`. Fifteen candidates allow three holders, so
/// the `e1` that one ruling holds stays rare, and the `pm` that most hold does
/// not.
pub(super) fn max_holders(candidates: usize) -> usize {
    (candidates / HOLDER_SHARE).max(MIN_HOLDERS)
}

/// The query id tokens held by at most [`max_holders`] of `contents`.
pub(super) fn rare_id_tokens<'a>(id_tokens: &[&'a str], contents: &[&str]) -> Vec<&'a str> {
    let limit = max_holders(contents.len());
    id_tokens
        .iter()
        .copied()
        .filter(|id| {
            let holders = contents
                .iter()
                .filter(|c| holds_any(c, &[*id]))
                .take(limit + 1);
            holders.count() <= limit
        })
        .collect()
}

/// Whether `content` holds one of `ids` as a whole token.
///
/// What: normalizes each whitespace-delimited word the way the closet index
/// does, so `#42`, `E1:` and `(v2)` match `42`, `e1` and `v2`. Reads the
/// content, not the closet index, because closets start empty when a palace
/// opens and fill only on the next write or dream cycle.
fn holds_any(content: &str, ids: &[&str]) -> bool {
    if ids.is_empty() {
        return false;
    }
    // Whitespace split only: `E1/E2` and `E1's` normalize to `e1e2` and `e1s`.
    content.split_whitespace().any(|raw| {
        // Skip long words before allocating: no id is longer than three chars.
        if raw.chars().filter(|c| c.is_alphanumeric()).count() > MAX_ID_CHARS {
            return false;
        }
        let token = normalize_keyword(raw);
        ids.contains(&token.as_str())
    })
}

/// [`ID_TOKEN_BOOST`] when `content` holds one of `id_tokens`, else `0.0`.
/// Callers pass only the rare tokens ([`rare_id_tokens`]).
pub(super) fn id_token_boost(id_tokens: &[&str], content: &str) -> f32 {
    if holds_any(content, id_tokens) {
        ID_TOKEN_BOOST
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the boost must fire on ids and never on ordinary short words.
    /// What: two characters, or three with a digit, is an id; nothing else is.
    #[test]
    fn id_token_shape() {
        for id in ["e1", "fe", "42", "v2", "e12", "q15"] {
            assert!(is_id_token(id), "{id} is an id");
        }
        for word in ["fix", "bug", "cap", "ruling", "x", "e123"] {
            assert!(!is_id_token(word), "{word} is not an id");
        }
    }

    /// Why: `#42` and `E1:` must match the ids `42` and `e1`, and a longer
    /// word that merely contains an id must not.
    /// What: content-side normalization matches the closet form.
    #[test]
    fn id_token_boost_matches_whole_normalized_tokens() {
        assert_eq!(id_token_boost(&["42"], "see issue #42."), ID_TOKEN_BOOST);
        assert_eq!(id_token_boost(&["e1"], "Ruling E1: hold"), ID_TOKEN_BOOST);
        assert_eq!(id_token_boost(&["e1"], "ruling e12 and be1"), 0.0);
        assert_eq!(id_token_boost(&[], "ruling e1"), 0.0);
    }

    /// Why (#9279 review): `pm` in most candidates is a word, not an id.
    /// What: one holder of fifteen is rare; four of fifteen is not (limit 3).
    #[test]
    fn an_id_most_candidates_hold_is_not_rare() {
        let mut contents = vec!["ruling e1: hold"; 1];
        contents.extend(["the PM files it"; 4]);
        contents.extend(["ruling e2: other"; 10]);
        assert_eq!(max_holders(contents.len()), 3);
        assert_eq!(rare_id_tokens(&["e1", "pm"], &contents), vec!["e1"]);
        assert_eq!(max_holders(4), MIN_HOLDERS);
    }
}
