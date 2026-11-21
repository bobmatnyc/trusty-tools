//! HNSW graph construction for `HnswStore::open` (#9141).
//!
//! Why: an open rebuilds the in-memory graph from the `VECTORS` rows, and two
//! opens of the same rows ranked recalls differently — a query's correct
//! drawer ranked 4 on one open and fell out of the top 10 on another.
//!
//! Root cause: `hnsw_rs` 0.3.4 draws each point's layer from a `StdRng` seeded
//! with `StdRng::from_os_rng()` (`hnsw.rs:328`). The seed is private, so a
//! 16-layer graph got a new hierarchy on every open, and its search missed a
//! different set of true neighbours each time. Evidence, four builds per row,
//! 50 queries through `graph_nearest`, k = 10:
//!
//! | data | 16 layers, parallel | 16 layers, serial | 1 layer, parallel | 1 layer, serial |
//! |---|---|---|---|---|
//! | 6,000 clustered, 384-dim | 22/50 differ | 9/50 | not run | 0/50 |
//! | 4,500 clustered, 384-dim | 17/50 | 41/50 | 0/50 | 0/50 |
//! | 4,500 isotropic, 16-dim | 16/50 | 14/50 | 0/50 | 0/50 |
//! | 4,500 isotropic, 64-dim | 21/50 | 20/50 | 0/50 | 0/50 |
//! | 30,000 isotropic, 16-dim | 20/50 | 16/50 | 0/50 | 0/50 |
//!
//! The 16-layer builds also missed 9–124 of 2,000 true top-10 neighbours; the
//! 1-layer builds missed none.
//!
//! What: [`new_index`] builds a SINGLE-layer graph. With `max_layer = 1`,
//! `LayerGenerator::generate` maps every draw to layer 0, so the OS-seeded RNG
//! no longer shapes the graph. [`replay`] inserts every live row serially
//! (#10682: a parallel insert can deadlock inside `hnsw_rs`).
//!
//! Guarantee (#10682): the replay is serial, so two opens of the same rows
//! build the same graph. Owner ruling 25 (#9141) had chosen a parallel replay
//! for its open cost (~0.45 s parallel against ~4.2–5.2 s serial for 6,661
//! 384-dim rows in the dev profile); that replay could deadlock inside
//! `hnsw_rs`, so the serial cost is the price of an open that always ends.
//! #9280: at or below `EXHAUSTIVE_SCAN_MAX_POINTS` the exact scan answers
//! instead of the graph.
//! Test: `reopening_a_palace_answers_every_query_identically`,
//! `replay_leaves_no_point_without_neighbours`.

use hnsw_rs::prelude::{DistCosine, Hnsw};

use super::quiet_insert::insert_quietly;
use super::{HNSW_EF_CONSTRUCTION, HNSW_INITIAL_CAPACITY, HNSW_MAX_NB_CONNECTION};

/// Layer count of every graph this store builds.
///
/// Why (#9141): `hnsw_rs` assigns layers from an RNG seeded by OS entropy, and
/// one layer is the only setting that makes the draw irrelevant. With
/// `ef_search` at 1024 from `graph_arm`, the layer-0 search dominates query
/// cost: the probe in the module header measured 1.5–2.0 ms per query at 6,000
/// 384-dim points with either layer count.
/// What: passed as `max_layer` to `Hnsw::new`.
/// Test: `reopening_a_palace_answers_every_query_identically`.
pub(super) const HNSW_LAYERS: usize = 1;

/// An empty graph with the store's construction parameters.
///
/// What: `Hnsw::new` with [`HNSW_LAYERS`] layers.
/// Test: `reopening_a_palace_answers_every_query_identically`.
pub(super) fn new_index() -> Hnsw<'static, f32, DistCosine> {
    Hnsw::<f32, DistCosine>::new(
        HNSW_MAX_NB_CONNECTION,
        HNSW_INITIAL_CAPACITY,
        HNSW_LAYERS,
        HNSW_EF_CONSTRUCTION,
        DistCosine,
    )
}

/// Insert `live` into `index`, one row at a time on the calling thread.
///
/// Why (#10682): `hnsw_rs` 0.3.4 `reverse_update_neighborhood_simple`
/// (`hnsw.rs:1237-1241`) holds `new_point.neighbours.read()` while it takes
/// `q.neighbours.write()`. Two parallel inserts that each found the other as a
/// neighbour each hold their own read lock and wait on the other's write lock,
/// so `parallel_insert_slice` can deadlock and hang `HnswStore::open`. The
/// lock order lives inside `hnsw_rs`, so the only order this crate controls is
/// "one insert at a time". This reverses owner ruling 25's parallel replay
/// (#9141): a correct open beats a fast open that can hang forever.
/// #9141: the graph stays single-layer, so a serial replay is also
/// deterministic: the same rows give the same neighbour lists on every open.
/// What: every row goes through [`super::quiet_insert::insert_quietly`], which
/// also silences the 50,000th-insert print (#9187). Fails only when stdout
/// cannot be silenced, and then the graph is partial.
/// Test: `replay_builds_the_same_graph_every_time`,
/// `replay_of_mutual_neighbours_finishes_within_bound`,
/// `reopening_a_palace_answers_every_query_identically`,
/// `no_hnsw_insert_writes_to_stdout`.
pub(super) fn replay(
    index: &Hnsw<'static, f32, DistCosine>,
    live: &[(Vec<f32>, usize)],
) -> std::io::Result<()> {
    // #10682: serial, so no two inserts ever hold `hnsw_rs` neighbour locks at once.
    for (vector, id) in live {
        insert_quietly(index, vector, *id)?;
    }
    Ok(())
}
