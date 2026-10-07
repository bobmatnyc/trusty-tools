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
//! An id-shaped token that is common among the candidates counts for neither
//! boost.
//! Test: `ruling_e1_ranks_its_drawer_first_in_l2_and_l3`,
//! `a_common_short_word_keeps_similarity_order`,
//! `a_common_short_word_stays_below_the_relevance_floor`,
//! `l2_returns_relevant_drawer`, `l2_rank_trace_emits_one_event_per_candidate`.

use std::collections::HashSet;

use uuid::Uuid;

use super::handle::PalaceHandle;
use super::id_tokens::{id_token_boost, query_id_tokens, rare_id_tokens};
use super::layers::{RANK_TRACE_TARGET, rank_score, uuid_prefix_eq};
use super::scope::scope_admits;
use super::types::RecallResult;
use crate::memory_core::decay::DecayConfig;
use crate::memory_core::palace::Drawer;
use crate::memory_core::store::vector::VectorHit;

/// Closet tag boost: a query token names a closet keyword holding the drawer.
const CLOSET_BOOST: f32 = 0.15;

/// Score `hits` for layer `layer` (2 or 3), unsorted and untruncated.
///
/// What: drops hits with no drawer row (possible during partial loads), hits
/// outside `allowed`, and expired drawers (#4885). Layer 2 emits one
/// [`RANK_TRACE_TARGET`] event per scored candidate (#4904).
pub(super) fn score_candidates(
    handle: &PalaceHandle,
    hits: &[VectorHit],
    allowed: &Option<HashSet<Uuid>>,
    query_tokens: &[String],
    layer: u8,
) -> Vec<RecallResult> {
    let drawers = handle.drawers.read();
    let closets = handle.closets.read();
    let now = chrono::Utc::now();
    let candidates: Vec<(f32, &Drawer)> = hits
        .iter()
        .filter_map(|hit| {
            let drawer = drawers
                .iter()
                .find(|d| uuid_prefix_eq(d.id, hit.drawer_id))?;
            // #4885: the vector index keeps an expired drawer's embedding until
            // a sweep compacts it, so a hit can land on a drawer past its TTL.
            (scope_admits(allowed, drawer.room_id) && !drawer.is_expired_at(now))
                .then_some((hit.score, drawer))
        })
        .collect();

    // #9279: an id-shaped word most candidates hold ("pm", "pr") is common
    // here, so it earns neither the id boost nor the closet boost.
    let id_tokens = query_id_tokens(query_tokens);
    let contents: Vec<&str> = candidates.iter().map(|(_, d)| d.content()).collect();
    let rare = rare_id_tokens(&id_tokens, &contents);
    let closet_tokens: Vec<&str> = query_tokens
        .iter()
        .map(String::as_str)
        .filter(|t| !id_tokens.contains(t) || rare.contains(t))
        .collect();

    candidates
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
        .collect()
}
