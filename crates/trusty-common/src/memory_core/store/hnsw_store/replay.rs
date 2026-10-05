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
//! no longer shapes the graph. [`replay`] inserts the first live row serially,
//! so the entry point exists, and the rest on rayon.
//!
//! Guarantee: a parallel build's neighbour lists still depend on thread
//! scheduling, so two opens return the same top k only while the search
//! finds the exact top k both times. That held in every measurement above,
//! but `hnsw_rs` cannot promise it. Owner ruling 25 (#9141) accepted this
//! trade for the open cost: ~0.45 s parallel against ~4.2–5.2 s serial for
//! 6,661 384-dim rows in the dev profile.
//! Test: `reopening_a_palace_answers_every_query_identically`,
//! `replay_leaves_no_point_without_neighbours`.

use hnsw_rs::prelude::{DistCosine, Hnsw};

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

/// Insert `live` into `index`: the first row serially, the rest in parallel.
///
/// Why (#9141): the serial replay was the dominant cost of a cold palace open
/// (2.4 s of a 3 s open at 6,644 vectors). Owner ruling 25 chose the parallel
/// insert over the serial one; see the module header for what that gives up.
/// An `hnsw_rs` 0.3.4 insert that reads no entry point sets one and returns
/// with no neighbours (`hnsw.rs:1083-1098`). In a parallel insert into an
/// empty graph several first inserts can read no entry point at once, and
/// every one but the winner is stored with no edges, so no search reaches it.
/// What: `insert_slice` for the first row sets the entry point; one
/// `parallel_insert_slice` inserts the rest, each of which then links to the
/// graph.
/// Test: `reopening_a_palace_answers_every_query_identically`,
/// `replay_leaves_no_point_without_neighbours`.
pub(super) fn replay(index: &Hnsw<'static, f32, DistCosine>, live: &[(Vec<f32>, usize)]) {
    let Some(((first, first_id), rest)) = live.split_first() else {
        return;
    };
    // See #9141: the entry point exists before any parallel insert reads it.
    index.insert_slice((first.as_slice(), *first_id));
    let refs: Vec<(&[f32], usize)> = rest.iter().map(|(v, id)| (v.as_slice(), *id)).collect();
    index.parallel_insert_slice(&refs);
}
