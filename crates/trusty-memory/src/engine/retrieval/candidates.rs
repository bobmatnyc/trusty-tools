//! L2/L3 candidate scoring, shared by both layers (#9279).
//!
//! Why: the id-token rarity rule must see every candidate's content before it
//! scores any of them, and L2 and L3 each ran their own copy of the scoring
//! loop. One two-pass function keeps both layers on one formula, the drift
//! #3274 and #4904 each had to repair.
//! What: [`score_candidates`] joins vector hits to live drawers inside the
//! scope, works out which query id tokens are rare among those drawers
//! ([`rare_id_tokens`]), then scores each drawer with [`rank_score`]:
//! similarity tilted by importance, plus the closet boost, plus the id boost.
//! A common id earns no id boost. A two-character token earns the closet boost
//! only as a rare id; longer tokens keep the closet boost as before.
//! #9390: [`search_live_candidates`] runs the vector search and refills past
//! expired drawers under [`super::refill::next_refill_window`].
//! Test: `ruling_e1_ranks_its_drawer_first_in_l2_and_l3`,
//! `l2_and_l3_refill_past_expired_drawers`,
//! `a_common_short_word_keeps_similarity_order`,
//! `a_common_short_word_stays_below_the_relevance_floor`,
//! `a_rare_two_letter_word_stays_below_the_relevance_floor`,
//! `a_common_three_char_id_keeps_the_closet_boost`,
//! `l2_returns_relevant_drawer`, `l2_rank_trace_emits_one_event_per_candidate`.

use std::collections::HashSet;

use uuid::Uuid;

use super::handle::PalaceHandle;
use super::id_tokens::{id_token_boost, query_id_tokens, rare_id_tokens};
use super::layers::{RANK_TRACE_TARGET, rank_score, uuid_prefix_eq};
use super::refill::next_refill_window;
use super::scope::scope_admits;
use super::types::RecallResult;
use crate::engine::decay::DecayConfig;
use crate::engine::dream::MIN_KEYWORD_CHARS;
use crate::engine::palace::Drawer;
use crate::engine::store::vector::{VectorHit, VectorStore};

/// Closet tag boost: a query token names a closet keyword holding the drawer.
const CLOSET_BOOST: f32 = 0.15;

/// Search the vector store and score the live candidates for `layer`.
///
/// Why (#9390): ADR-0028 D6 keeps expired Tier C slots, and [`score_candidates`]
/// drops them only after the fetch, so a pile of them could empty the window.
/// What: searches `fetch` hits, scores them, and while expired hits were
/// dropped, fewer than `target` live candidates survived and the store
/// returned a full window, searches again with the window
/// [`next_refill_window`] gives. Unsorted and untruncated, as
/// [`score_candidates`]. With no expired hit this is one search, as before.
/// Test: `l2_and_l3_refill_past_expired_drawers`.
pub(super) async fn search_live_candidates(
    handle: &PalaceHandle,
    query_vec: &[f32],
    fetch: usize,
    target: usize,
    allowed: &Option<HashSet<Uuid>>,
    query_tokens: &[String],
    layer: u8,
) -> anyhow::Result<Vec<RecallResult>> {
    let mut fetch = fetch;
    loop {
        let hits = handle.vector_store.search(query_vec, fetch).await?;
        let (results, expired) = score_candidates(handle, &hits, allowed, query_tokens, layer);
        match next_refill_window(fetch, hits.len(), results.len(), expired, target) {
            Some(next) => fetch = next,
            None => return Ok(results),
        }
    }
}

/// Score `hits` for layer `layer` (2 or 3), unsorted and untruncated.
///
/// What: drops hits with no drawer row (possible during partial loads), hits
/// outside `allowed`, and expired drawers (#4885). Layer 2 emits one
/// [`RANK_TRACE_TARGET`] event per scored candidate (#4904). Also returns how
/// many in-scope hits were dropped as expired, for the #9390 refill.
fn score_candidates(
    handle: &PalaceHandle,
    hits: &[VectorHit],
    allowed: &Option<HashSet<Uuid>>,
    query_tokens: &[String],
    layer: u8,
) -> (Vec<RecallResult>, usize) {
    let drawers = handle.drawers.read();
    let closets = handle.closets.read();
    let now = chrono::Utc::now();
    let mut expired = 0;
    let candidates: Vec<(f32, &Drawer)> = hits
        .iter()
        .filter_map(|hit| {
            let drawer = drawers
                .iter()
                .find(|d| uuid_prefix_eq(d.id, hit.drawer_id))?;
            if !scope_admits(allowed, drawer.room_id) {
                return None;
            }
            // #4885: the vector index keeps an expired drawer's embedding until
            // a sweep compacts it, so a hit can land on a drawer past its TTL.
            if drawer.is_expired_at(now) {
                expired += 1;
                return None;
            }
            Some((hit.score, drawer))
        })
        .collect();

    // #9279: an id most candidates hold is common here and earns no id boost.
    let id_tokens = query_id_tokens(query_tokens);
    let contents: Vec<&str> = candidates.iter().map(|(_, d)| d.content()).collect();
    let rare = rare_id_tokens(&id_tokens, &contents);
    // #9279: a token of three or more characters keeps the closet boost it had
    // before (`k8s`, `ec2`, common or not); a two-character token, which the
    // closets did not hold before, earns it only as a rare id.
    let closet_tokens: Vec<&str> = query_tokens
        .iter()
        .map(String::as_str)
        .filter(|t| t.chars().count() > MIN_KEYWORD_CHARS || rare.contains(t))
        .collect();

    let results = candidates
        .into_iter()
        .map(|(similarity, drawer)| {
            let age_days = DecayConfig::age_days(drawer.created_at);
            let boost = drawer.accumulated_boost(&handle.decay_config);
            let eff_importance =
                handle
                    .decay_config
                    .effective_importance(drawer.importance, age_days, boost);
            let in_closet = closet_tokens.iter().any(|tok| {
                closets
                    .get(*tok)
                    .is_some_and(|ids| ids.contains(&drawer.id))
            });
            let tag_boost = if in_closet { CLOSET_BOOST } else { 0.0 };
            let id_boost = id_token_boost(&rare, drawer.content());
            // #4904: importance tilts the similarity, it does not multiply it.
            let score = rank_score(similarity, eff_importance, tag_boost + id_boost);
            if layer == 2 {
                // #4904: a missed fact may be absent, ranked below the cut, or
                // never queried for; only the pre-truncation list tells which.
                tracing::debug!(
                    target: RANK_TRACE_TARGET,
                    drawer_id = %drawer.id,
                    similarity,
                    importance = drawer.importance,
                    eff_importance,
                    tag_boost,
                    id_boost,
                    score,
                    "l2 candidate"
                );
            }
            RecallResult {
                drawer: drawer.clone(),
                score,
                layer,
            }
        })
        .collect();
    (results, expired)
}
