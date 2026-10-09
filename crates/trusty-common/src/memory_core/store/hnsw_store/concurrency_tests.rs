//! #9487: concurrent `search` and `upsert` on one store never deadlock.
//!
//! Why: `hnsw_rs`'s whole-index point iterator holds a shared guard on the
//! point table and takes a second shared guard on the same lock when it moves
//! past layer 0. `parking_lot`'s `RwLock` refuses a new shared guard while a
//! writer is queued, and an insert queues for that lock — so a search running
//! the exact scan wedged against any insert that arrived mid-scan, forever.
//! What: one store, pre-filled below the exact-scan threshold, with searcher
//! and upserter threads running a fixed amount of work each. A watchdog fails
//! the test if they have not all finished inside the budget, so a deadlock
//! fails the run instead of hanging it.
//! Test: this file is the test.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::*;
use redb::Database;
use tempfile::tempdir;

/// Seeded unit vector; distinct seeds give distinct, well-spread points.
fn seeded_vec(dim: usize, seed: u64) -> Vec<f32> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let raw: Vec<f32> = (0..dim)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            ((x >> 11) as f64 / (1u64 << 53) as f64) as f32 * 2.0 - 1.0
        })
        .collect();
    let norm: f32 = raw.iter().map(|v| v * v).sum::<f32>().sqrt();
    raw.into_iter().map(|v| v / norm).collect()
}

/// Why (#9487): the issue's first acceptance criterion — a concurrent
/// search+upsert stress run on one `HnswStore` completes with no thread
/// blocked past a bound. On the pre-fix code a searcher wedges in
/// `IterPoint::next` against an upserter queued in `insert_slice`.
/// What: 1,000 pre-filled drawers (exact-scan path), four searcher threads
/// of 150 searches each and two upserter threads of 150 upserts each, half
/// re-upserts of existing uuids. Every thread reports done on a channel; the
/// test fails if the six reports do not all arrive within 60 s. Searches
/// must also keep returning hits, so the run cannot pass by erroring out.
/// Test: this test.
#[test]
fn concurrent_search_and_upsert_never_deadlock() {
    const DIM: usize = 64;
    const PREFILL: u64 = 1_000;
    const SEARCHERS: u64 = 4;
    const UPSERTERS: u64 = 2;
    const ROUNDS: u64 = 150;
    const BUDGET: Duration = Duration::from_secs(60);

    let dir = tempdir().expect("tempdir");
    let db = Arc::new(Database::create(dir.path().join("hnsw.redb")).expect("create db"));
    let store = Arc::new(HnswStore::open(db, DIM).expect("open store"));
    for i in 0..PREFILL {
        store
            .upsert(&format!("pre-{i}"), &seeded_vec(DIM, i))
            .expect("prefill upsert");
    }

    let (done_tx, done_rx) = mpsc::channel::<std::result::Result<String, String>>();
    for s in 0..SEARCHERS {
        let (store, done) = (Arc::clone(&store), done_tx.clone());
        std::thread::spawn(move || {
            let mut outcome = Ok(format!("searcher {s}"));
            for r in 0..ROUNDS {
                match store.search(&seeded_vec(DIM, 10_000 + s * ROUNDS + r), 5) {
                    Ok(hits) if hits.is_empty() => {
                        outcome = Err(format!("searcher {s}: empty result in round {r}"));
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        outcome = Err(format!("searcher {s}: {e}"));
                        break;
                    }
                }
            }
            let _ = done.send(outcome);
        });
    }
    for u in 0..UPSERTERS {
        let (store, done) = (Arc::clone(&store), done_tx.clone());
        std::thread::spawn(move || {
            let mut outcome = Ok(format!("upserter {u}"));
            for r in 0..ROUNDS {
                // Even rounds add a drawer; odd rounds re-upsert a prefilled one
                // (the #5171 shadow path).
                let uuid = if r % 2 == 0 {
                    format!("new-{u}-{r}")
                } else {
                    format!("pre-{}", (u * ROUNDS + r) % PREFILL)
                };
                if let Err(e) = store.upsert(&uuid, &seeded_vec(DIM, 50_000 + u * ROUNDS + r)) {
                    outcome = Err(format!("upserter {u}: {e}"));
                    break;
                }
            }
            let _ = done.send(outcome);
        });
    }
    drop(done_tx);

    let deadline = Instant::now() + BUDGET;
    for finished in 0..(SEARCHERS + UPSERTERS) {
        let left = deadline.saturating_duration_since(Instant::now());
        match done_rx.recv_timeout(left) {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => panic!("#9487: a worker failed: {e}"),
            Err(_) => {
                // The wedged threads still borrow the redb file; leave it.
                std::mem::forget(dir);
                panic!(
                    "#9487: only {finished} of {} search/upsert threads finished in {BUDGET:?} \
                     — the HNSW store deadlocked",
                    SEARCHERS + UPSERTERS
                );
            }
        }
    }
    assert_eq!(
        store.len().expect("len"),
        (PREFILL + UPSERTERS * ROUNDS / 2) as usize
    );
}
