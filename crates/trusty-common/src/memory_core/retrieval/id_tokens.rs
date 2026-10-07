//! Exact short-id token boost for L2/L3 recall (#9279).
//!
//! Why: a query that names an id, such as "ruling e1", has to recall the drawer
//! holding that id. The embedding gives a two-character token almost no
//! weight, and the closet tag boost cannot separate the drawers either: every
//! ruling matches "ruling" and earns the same +0.15. In the live supervisor
//! palace the E1 ruling scored 0.208 and ranked 12th under five other rulings
//! at 0.234 to 0.298.
//! What: [`query_id_tokens`] picks the id-shaped tokens out of a query, and
//! [`id_token_boost`] adds [`ID_TOKEN_BOOST`] to a candidate whose content
//! holds one of them as a whole token. A query with no id-shaped token gets no
//! boost on any candidate, so its ranking is unchanged.
//! Test: `ruling_e1_ranks_its_drawer_first_in_l2_and_l3`,
//! `a_query_without_an_id_token_keeps_its_order`, `id_token_shape`,
//! `id_token_boost_matches_whole_normalized_tokens`.

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

/// [`ID_TOKEN_BOOST`] when `content` holds one of `id_tokens` as a whole
/// token, else `0.0`.
///
/// What: normalizes each whitespace-delimited word the way the closet index
/// does, so `#42`, `E1:` and `(v2)` match `42`, `e1` and `v2`. Reads the
/// content, not the closet index, because closets start empty when a palace
/// opens and fill only on the next write or dream cycle.
pub(super) fn id_token_boost(id_tokens: &[&str], content: &str) -> f32 {
    if id_tokens.is_empty() {
        return 0.0;
    }
    let hit = content.split_whitespace().any(|raw| {
        // Skip long words before allocating: no id is longer than three chars.
        if raw.chars().filter(|c| c.is_alphanumeric()).count() > MAX_ID_CHARS {
            return false;
        }
        let token = normalize_keyword(raw);
        id_tokens.contains(&token.as_str())
    });
    if hit { ID_TOKEN_BOOST } else { 0.0 }
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
}
