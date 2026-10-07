//! HNSW search-beam tuning keyed on index size (#9414).
//!
//! Why: usearch does not persist `expansion_search` in a snapshot; the store
//! re-applies it from code at creation and on every `load_from`. A single
//! 50K-key step left every index at usearch's default beam of 64, and on a
//! 150K-key index that beam missed the true nearest neighbour (#9414). One
//! table here keeps the creation, load and requantize paths from drifting.
//! What: [`hnsw_tuning`] maps a key count to usearch graph options, and
//! [`search_count`] is the query-time candidate floor that gives a store built
//! in this process the same beam a reloaded one gets.
//! Test: `super::tests_9414`, and the `--include-ignored` recall test
//! `hnsw_recall_9414::recall_holds_on_a_150k_clustered_index_after_reload`.

/// Key count above which the large-index tuning and the query floor apply.
/// At or below it the store keeps usearch's library defaults.
pub(super) const LARGE_INDEX_KEYS: usize = 50_000;

/// usearch graph options derived from an index's key count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HnswTuning {
    /// Graph degree `M`; `0` selects the library default (16). Build-time only:
    /// `view()`/`load()` take it from the snapshot header.
    pub connectivity: usize,
    /// Beam used while inserting; `0` selects the library default (128).
    pub expansion_add: usize,
    /// Beam used while searching; `0` selects the library default (64).
    pub expansion_search: usize,
}

/// Why: the search beam must grow with the graph, or recall falls as an index
/// grows (#9414). The steps come from the recall sweep recorded in the #9414
/// PR (dim 32, M 16, f16, after a real save and reload): at 150K keys a beam
/// of 64 gave recall@10 0.978 and two rank-1 self-query misses in 200 on a
/// clustered corpus; 256 gave 0.999 and none. On isotropic data 512 was the
/// smallest beam reaching 0.99 at 300K keys.
/// What: `(0, 0, 0)` (library defaults) up to [`LARGE_INDEX_KEYS`]; above it
/// connectivity 32 and expansion_add 128 as before, with expansion_search 256
/// up to 250K keys and 512 beyond.
/// Test: `super::tests_9414::ef_scales_with_index_size`.
pub(super) fn hnsw_tuning(expected_keys: usize) -> HnswTuning {
    if expected_keys <= LARGE_INDEX_KEYS {
        return HnswTuning {
            connectivity: 0,
            expansion_add: 0,
            expansion_search: 0,
        };
    }
    let expansion_search = if expected_keys <= 250_000 { 256 } else { 512 };
    HnswTuning {
        connectivity: 32,
        expansion_add: 128,
        expansion_search,
    }
}

/// Why: a store built in this process (`UsearchStore::new`) keeps the beam it
/// was created with (64) however large it grows, and changing a live index's
/// beam under the shared read lock would be a data race (#9414). usearch
/// searches with `max(expansion_search, count)`, so asking for more results is
/// a lock-free way to widen the beam for one query.
/// What: the number of results to request from usearch for a caller asking
/// for `top_k` on an index holding `live_keys` keys: `top_k` at or below
/// [`LARGE_INDEX_KEYS`], else at least the [`hnsw_tuning`] beam for that size.
/// The caller truncates the hits back to `top_k`.
/// Test: `super::tests_9414::search_count_floors_only_large_indexes`,
/// `super::tests_9414::search_truncates_to_top_k_above_the_floor`.
pub(super) fn search_count(top_k: usize, live_keys: usize) -> usize {
    if live_keys <= LARGE_INDEX_KEYS {
        return top_k;
    }
    top_k.max(hnsw_tuning(live_keys).expansion_search)
}
