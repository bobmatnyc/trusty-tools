//! #9275 PR B: `HnswStore::search` counts `top_k` in drawers, not chunk rows.
//!
//! Why: a chunked drawer holds one row per chunk under `{drawer}` and
//! `{drawer}#n`. A search that stops after `k` ROWS returns one drawer when
//! that drawer's chunks are the nearest rows, and `candidates.rs` reads a
//! short answer as "index exhausted".
//! What: rows are written with the raw `upsert` primitive; each test checks
//! the number of distinct drawers a search returns, on both search arms.
//! Test: this file IS the tests.

use super::*;
use std::collections::HashSet;

/// The drawer a row key belongs to: the text before `#`.
fn drawer_of(key: &str) -> &str {
    key.split('#').next().unwrap_or(key)
}

fn distinct_drawers(hits: &[(String, f32)]) -> HashSet<&str> {
    hits.iter().map(|(k, _)| drawer_of(k)).collect()
}

/// Write drawer `id` as `chunks` rows: the bare key, then `#1`, `#2`, ...
fn chunked(store: &HnswStore, id: &str, vectors: &[Vec<f32>]) {
    for (n, v) in vectors.iter().enumerate() {
        let key = if n == 0 {
            id.to_string()
        } else {
            format!("{id}#{n}")
        };
        store.upsert(&key, v).expect("raw upsert");
    }
}

/// Every key in the store, sorted.
fn sorted_keys(store: &HnswStore) -> Vec<String> {
    let mut keys = store.all_keys().expect("all_keys");
    keys.sort();
    keys
}

/// #9275: `replace_group` takes a drawer key, never a row key. An empty key or
/// one holding `#` is an error, and nothing is written — no rows that the
/// same call's surplus sweep then deletes behind an `Ok`.
#[test]
fn replace_group_rejects_an_empty_or_hash_drawer_key() {
    let (_dir, store) = open_store(8);
    let rows = vec![planar_vec(8, 0.0), planar_vec(8, 10.0)];
    for drawer in ["x#2", ""] {
        let result = store.replace_group(drawer, &rows);
        assert!(result.is_err(), "drawer {drawer:?}: {result:?}");
        assert_eq!(sorted_keys(&store), Vec::<String>::new(), "{drawer:?}");
    }
}

/// #9275: `delete_group("{d}#2")` must not delete the whole of drawer `d`, and
/// an empty key names no drawer. Both are errors and delete nothing.
#[test]
fn delete_group_rejects_an_empty_or_hash_drawer_key() {
    let (_dir, store) = open_store(8);
    let d = Uuid::new_v4().to_string();
    let rows = [planar_vec(8, 0.0), planar_vec(8, 5.0), planar_vec(8, 9.0)];
    chunked(&store, &d, &rows);
    let before = sorted_keys(&store);
    assert_eq!(before.len(), 3);
    for drawer in [format!("{d}#2"), String::new()] {
        let result = store.delete_group(&drawer);
        assert!(result.is_err(), "drawer {drawer:?}: {result:?}");
        assert_eq!(sorted_keys(&store), before, "{drawer:?} deleted rows");
    }
}

/// AC-B3: five drawers of four chunks, `k = 3` returns three distinct drawers,
/// each once, best first — not the nearest drawer's three nearest chunks.
#[test]
fn top_k_counts_drawers_not_chunks() {
    let (_dir, store) = open_store(8);
    let ids: Vec<String> = (0..5).map(|_| Uuid::new_v4().to_string()).collect();
    for (j, id) in ids.iter().enumerate() {
        let rows: Vec<Vec<f32>> = (0..4)
            .map(|c| planar_vec(8, j as f32 * 10.0 + c as f32 * 0.5))
            .collect();
        chunked(&store, id, &rows);
    }

    let hits = store.search(&planar_vec(8, 0.0), 3).unwrap();
    let got: Vec<&str> = hits.iter().map(|(k, _)| drawer_of(k)).collect();
    assert_eq!(
        got,
        vec![ids[0].as_str(), ids[1].as_str(), ids[2].as_str()],
        "three drawers, nearest first, each once: {hits:?}"
    );
}

/// AC-B4 invariant: fewer than `k` drawers come back only when the index holds
/// fewer than `k` drawers — and then every one of them, once.
#[test]
fn search_returns_short_only_when_the_index_is_exhausted() {
    let (_dir, store) = open_store(8);
    let ids: Vec<String> = (0..3).map(|_| Uuid::new_v4().to_string()).collect();
    for (j, id) in ids.iter().enumerate() {
        let rows: Vec<Vec<f32>> = (0..4)
            .map(|c| planar_vec(8, j as f32 * 20.0 + c as f32))
            .collect();
        chunked(&store, id, &rows);
    }

    let hits = store.search(&planar_vec(8, 0.0), 10).unwrap();
    assert_eq!(hits.len(), 3, "three drawers, one hit each: {hits:?}");
    assert_eq!(distinct_drawers(&hits).len(), 3);
}

/// AC-B4, graph arm: above the exhaustive threshold, a query whose nearest rows
/// are all chunks of one drawer still fills `k` with distinct drawers. The
/// graph arm must keep widening on drawers, not stop on its row count.
#[test]
fn search_widens_until_k_drawers_in_the_graph_arm() {
    let dim = 32;
    let k = 5usize;
    let fill_n = exhaustive::EXHAUSTIVE_SCAN_MAX_POINTS + 10;
    let fill: Vec<Vec<f32>> = (0..fill_n)
        .map(|i| spread_vec(dim, 600_000 + i as u64))
        .collect();
    let (_dir, seed) = open_store(dim);
    seed_rows(&seed.db, &fill);
    let store = HnswStore::open(Arc::clone(&seed.db), dim).expect("reopen");

    let query = spread_vec(dim, 777_777);
    let mix = |a: &[f32], b: &[f32], w: f32| -> Vec<f32> {
        let raw: Vec<f32> = a
            .iter()
            .zip(b)
            .map(|(x, y)| x * (1.0 - w) + y * w)
            .collect();
        let norm: f32 = raw.iter().map(|x| x * x).sum::<f32>().sqrt();
        raw.into_iter().map(|x| x / norm).collect()
    };
    // Twelve drawers near the query; each drawer's eight chunks sit within a
    // tiny jitter of its centre, so the nearest rows are one drawer's chunks.
    let ids: Vec<String> = (0..12).map(|_| Uuid::new_v4().to_string()).collect();
    for (j, id) in ids.iter().enumerate() {
        let centre = mix(&query, &spread_vec(dim, 900_000 + j as u64), 0.05);
        let rows: Vec<Vec<f32>> = (0..8)
            .map(|c| {
                let jitter = spread_vec(dim, 950_000 + (j * 8 + c) as u64);
                mix(&centre, &jitter, 0.002)
            })
            .collect();
        chunked(&store, id, &rows);
    }
    assert!(
        store.len().unwrap() > exhaustive::EXHAUSTIVE_SCAN_MAX_POINTS,
        "this test is about the graph arm; it must be the arm selected"
    );

    let hits = store.search(&query, k).unwrap();
    let drawers = distinct_drawers(&hits);
    assert_eq!(hits.len(), k, "k counts drawers: {hits:?}");
    assert_eq!(drawers.len(), k, "each drawer once: {hits:?}");
    assert!(
        drawers.iter().all(|d| ids.iter().any(|id| id == d)),
        "the five nearest drawers are chunked drawers, not filler: {hits:?}"
    );
}
