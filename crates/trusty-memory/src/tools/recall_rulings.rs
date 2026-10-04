//! User-scope rulings in every project palace's recall at L1 (#9143).
//!
//! Why: an owner ruling stored in one palace could not be recalled from
//! another. The review's Q15 (the symptom-title rule) lives in `supervisor`;
//! asked in trusty-tools, plain `memory_recall` never saw it, and
//! `memory_recall_all` ranked it below five unrelated apex drawers. Standing
//! rules are user-scope by nature — they constrain how work is done in every
//! project — so a project recall must reach them without fanning out over the
//! whole estate, which costs seconds (#9138 O3).
//! What: [`rulings_palaces_from_env`] reads the operator's list of user-scope
//! palaces once, at `AppState` construction. [`merge_user_rulings`] runs an L2
//! search against each listed palace, keeps only drawers that carry a ruling
//! tag ([`super::recall_rank::is_ruling`]), and merges them into the project
//! recall as layer-1 hits with their query similarity as score. A non-ruling
//! drawer from a listed palace never enters (#9143 criterion 3). An empty list
//! — the default — costs nothing.
//! Test: `tests/recall_temporal_rank.rs`
//! (`a_ruling_in_the_user_scope_palace_is_recalled_from_a_project_palace`);
//! `rulings_palace_list_parses_commas_and_blanks` below.

use crate::AppState;
use trusty_common::memory_core::embed::Embedder;
use trusty_common::memory_core::registry::PalaceRegistry;
use trusty_common::memory_core::retrieval::{
    expand_query, retrieve_l2_scoped, PalaceHandle, RecallResult, RecallScope,
};

use super::helpers::open_palace_handle;
use super::recall_rank::is_ruling;

/// Environment variable naming the user-scope rulings palaces.
///
/// What: a comma-separated list of palace ids, e.g. `supervisor`. Unset or
/// blank means no user-scope leg.
pub const RULINGS_PALACES_ENV: &str = "TRUSTY_MEMORY_RULINGS_PALACES";

/// Candidates fetched from each rulings palace before the ruling filter.
///
/// Why: a rulings palace also holds non-ruling drawers, and the filter runs
/// after the vector search, so asking for `top_k` alone could return none.
/// What: `max(top_k * 4, 32)`; a rulings palace is small (tens to hundreds
/// of drawers), so the wider HNSW search is cheap.
fn rulings_window(top_k: usize) -> usize {
    top_k.saturating_mul(4).max(32)
}

/// Parse a comma-separated palace list; blanks and surrounding space dropped.
pub(crate) fn parse_rulings_palaces(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The user-scope rulings palaces configured in [`RULINGS_PALACES_ENV`].
///
/// Why: read once per `AppState`, like `PalaceRegistry::from_env`, so the
/// recall hot path never touches the environment.
/// What: the parsed list, or empty when the variable is unset or not UTF-8.
pub(crate) fn rulings_palaces_from_env() -> Vec<String> {
    std::env::var(RULINGS_PALACES_ENV)
        .map(|raw| parse_rulings_palaces(&raw))
        .unwrap_or_default()
}

impl AppState {
    /// Replace the user-scope rulings palaces (#9143).
    ///
    /// Why: tests and embedders need the list without writing the process
    /// environment, which every other test in the binary would see.
    #[must_use]
    pub fn with_rulings_palaces(mut self, palaces: Vec<String>) -> Self {
        self.rulings_palaces = std::sync::Arc::new(palaces);
        self
    }
}

/// Merge ruling-tagged drawers from the user-scope palaces into `results`.
///
/// Why: see the module doc. Layer 1 marks them as grounding rather than a
/// project search hit, matching #9143's "at L1". Their score stays the query
/// similarity, so an off-topic ruling competes for a `top_k` slot on the same
/// scale as every other hit instead of claiming one. `min_score` is applied
/// here because `apply_score_floor` exempts layer 1 by design.
/// What: for each configured palace other than `target`, opens it (an absent
/// palace is skipped at debug level; any other open or search failure is
/// logged at warn and skipped, so the project recall still answers), runs
/// [`retrieve_l2_scoped`] over the expanded query with [`rulings_window`]
/// candidates, keeps ruling-tagged drawers at or above `min_score` that are
/// not already in `results`, and appends them as layer 1. The caller re-sorts.
/// The query embedding repeats the project leg's text, so the shared
/// embedder's LRU cache answers it.
/// Test: `a_ruling_in_the_user_scope_palace_is_recalled_from_a_project_palace`.
pub(crate) async fn merge_user_rulings(
    state: &AppState,
    target: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    top_k: usize,
    min_score: Option<f32>,
    results: &mut Vec<RecallResult>,
) {
    if state.rulings_palaces.is_empty() || top_k == 0 {
        return;
    }
    let expanded = expand_query(query);
    for palace in state.rulings_palaces.iter() {
        if palace == target.id.as_str() {
            continue;
        }
        let handle = match open_palace_handle(state, palace) {
            Ok(h) => h,
            Err(e) if PalaceRegistry::open_error_is_absent(&e) => {
                tracing::debug!(palace = %palace, "#9143: rulings palace absent; skipped");
                continue;
            }
            Err(e) => {
                tracing::warn!(palace = %palace, "#9143: rulings palace unreadable: {e:#}");
                continue;
            }
        };
        if handle.id == target.id {
            continue; // an alias of the project palace itself
        }
        let hits = match retrieve_l2_scoped(
            &handle,
            embedder,
            &expanded,
            &RecallScope::All,
            rulings_window(top_k),
        )
        .await
        {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(palace = %palace, "#9143: rulings search failed: {e:#}");
                continue;
            }
        };
        for mut hit in hits {
            let admitted = is_ruling(&hit.drawer)
                && min_score.is_none_or(|floor| hit.score >= floor)
                && !results.iter().any(|r| r.drawer.id == hit.drawer.id);
            if admitted {
                hit.layer = 1;
                results.push(hit);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rulings_palace_list_parses_commas_and_blanks() {
        assert_eq!(
            parse_rulings_palaces(" supervisor , ,owner "),
            ["supervisor", "owner"]
        );
        assert!(parse_rulings_palaces("").is_empty());
        assert_eq!(rulings_window(3), 32);
        assert_eq!(rulings_window(20), 80);
    }
}
