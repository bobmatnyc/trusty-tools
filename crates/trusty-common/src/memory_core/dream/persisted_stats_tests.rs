//! #8733: `PersistedDreamStats::save` publishes atomically.
//!
//! Why: a plain `fs::write` truncates `dream_stats.json` before it writes, so a
//! concurrent `load` (the dashboard, the election test's poll) could read an
//! empty or partial file and fail with `EOF while parsing a value`.
//! What: one error-arm test (a failed save leaves the prior snapshot readable)
//! and one bounded reader-vs-writer race.
//! Test: itself.

use super::config::{DreamStats, PersistedDreamStats};
use crate::atomic_file::temp_sibling;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn snapshot(merged: usize) -> PersistedDreamStats {
    PersistedDreamStats {
        last_run_at: chrono::Utc::now(),
        stats: DreamStats {
            merged,
            ..DreamStats::default()
        },
    }
}

/// Why: a save that fails part-way must not cost the operator the last good
/// snapshot. What: seed a snapshot, block the sibling temp path with a
/// directory so the new write cannot be staged, and require `save` to return
/// the error with the prior snapshot still loadable and byte-identical.
#[test]
fn failed_dream_stats_save_keeps_the_prior_snapshot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(PersistedDreamStats::FILE_NAME);
    snapshot(7).save(dir.path()).expect("seed save");
    let before = std::fs::read(&path).expect("read seed");
    std::fs::create_dir(temp_sibling(&path)).expect("block the temp path");

    let err = snapshot(99)
        .save(dir.path())
        .expect_err("save must report the failed staging write");

    assert!(err.to_string().contains("dream_stats.json"), "{err:#}");
    assert_eq!(std::fs::read(&path).expect("read after"), before);
    let loaded = PersistedDreamStats::load(dir.path())
        .expect("prior snapshot still parses")
        .expect("prior snapshot still present");
    assert_eq!(loaded.stats.merged, 7);
}

/// Why: the #8733 flake — a reader polling `dream_stats.json` while it is
/// rewritten saw an empty file. What: one thread saves in a tight loop while
/// this thread loads for a bounded 2 s window; every load must return a
/// complete snapshot, and no staging file may survive the writer.
#[test]
fn concurrent_dream_stats_reader_never_sees_a_partial_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().to_path_buf();
    snapshot(0).save(&data_dir).expect("seed save");

    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (stop, data_dir) = (Arc::clone(&stop), data_dir.clone());
        std::thread::spawn(move || {
            let mut saves = 0usize;
            while !stop.load(Ordering::Relaxed) {
                snapshot(saves).save(&data_dir).expect("writer save");
                saves += 1;
            }
            saves
        })
    };

    let deadline = Instant::now() + Duration::from_secs(2);
    let (mut reads, mut bad, mut first_bad) = (0usize, 0usize, None);
    while Instant::now() < deadline {
        reads += 1;
        match PersistedDreamStats::load(&data_dir) {
            Ok(Some(_)) => {}
            other => {
                bad += 1;
                first_bad.get_or_insert_with(|| format!("{other:?}"));
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    let saves = writer.join().expect("writer thread");

    assert!(saves > 0 && reads > 0, "saves={saves} reads={reads}");
    assert_eq!(
        bad, 0,
        "{bad} of {reads} loads saw an empty or partial dream_stats.json \
         across {saves} saves; first: {first_bad:?}"
    );
    let path = data_dir.join(PersistedDreamStats::FILE_NAME);
    assert!(!temp_sibling(&path).exists(), "a staging file survived");
}
