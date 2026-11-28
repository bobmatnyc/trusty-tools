//! Issue #11891 — the re-link sweep tells duplicate ties from real strands.
//!
//! Why: with more identical vectors than the sweep's 10-hit search width, a
//! self-search cannot return every duplicate, so the pre-fix sweep reported
//! the extras stranded on every pass and never converged (hypothesis H2).
//! What: the seeded #9450 churn strands some distinct survivors; 25 identical
//! vectors are then added. After `relink_stranded`, no key may read as
//! stranded, and each distinct strand must find itself again.
//! Test: this module. Run with `cargo test -p trusty-search --lib relink_dup_11891`.

use super::relink_9450_tests::{damaged_index, BLOBS};
use super::usearch_relink::{find_stranded, relink_stranded};

/// Keys in the churned base index: enough for the seeded churn to strand.
const BASE: usize = 8_000;

/// Identical vectors added after the churn, more than the 10-hit sweep width.
const DUPLICATES: usize = 25;

/// #11891: duplicate ties count as reachable; a real strand is still re-linked.
/// Pre-fix, the `after` assertion fails with the duplicates still reported.
#[test]
fn duplicate_ties_are_reachable_and_real_strands_still_relink() {
    let (index, mut vectors, mut keys) = damaged_index(BASE, 9450);
    index
        .reserve(index.size() + DUPLICATES)
        .expect("reserve for the duplicates");
    let dup = vec![0.5f32; BLOBS.dim];
    for k in 0..DUPLICATES {
        let key = (BASE + k) as u64;
        index.add(key, &dup).expect("add duplicate");
        while vectors.len() <= key as usize {
            vectors.push(Vec::new());
        }
        vectors[key as usize] = dup.clone();
        keys.push(key);
    }
    let is_dup = |key: u64| key >= BASE as u64;
    let read = |i: usize, buf: &mut [f32]| {
        buf.copy_from_slice(&vectors[keys[i] as usize]);
        Ok(Some(keys[i]))
    };
    let n = keys.len();

    let before = find_stranded(&index, n, BLOBS.dim, 4, &read).expect("find");
    let real: Vec<u64> = before
        .iter()
        .map(|&(_, k)| k)
        .filter(|&k| !is_dup(k))
        .collect();
    eprintln!(
        "#11891: before the sweep {} stranded ({} distinct, {} duplicates)",
        before.len(),
        real.len(),
        before.len() - real.len()
    );
    assert!(
        !real.is_empty(),
        "the churn must strand at least one distinct vector"
    );

    let relinked =
        relink_stranded(&index, n, BLOBS.dim, 4, "dup test", "test", &read).expect("relink");
    let after = find_stranded(&index, n, BLOBS.dim, 4, &read).expect("find");
    let dups_after = after.iter().filter(|&&(_, k)| is_dup(k)).count();
    assert!(
        after.is_empty(),
        "{} keys still stranded after {relinked} re-links ({dups_after} duplicates): {:?}",
        after.len(),
        &after[..after.len().min(10)]
    );
    for key in real {
        let hits = index
            .search(&vectors[key as usize], 10)
            .expect("self-search");
        assert!(
            hits.keys.contains(&key),
            "distinct strand {key} must find itself after the sweep"
        );
    }
    assert_eq!(index.size(), n, "re-linking must not add or lose a key");
}
