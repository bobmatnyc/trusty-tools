//! #8733: `PersistedDreamStats::save` publishes atomically.
//!
//! Why: a plain `fs::write` truncates `dream_stats.json` before it writes, so a
//! concurrent `load` (the dashboard, the election test's poll) could read an
//! empty or partial file and fail with `EOF while parsing a value`.
//! What: one error-arm test (a failed save leaves the prior snapshot readable)
//! and two bounded races: one writer against a reader, and two writers on one
//! path against a reader.
//! Test: itself.

use super::config::{DreamStats, PersistedDreamStats};
use std::path::Path;
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

/// `dir`'s entries, sorted — shows no staging file survived under any name.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Why: a save that fails part-way must not cost the operator the last good
/// snapshot. What: seed a snapshot, make the data dir read-only so no staging
/// file can be created (whatever its name), and require `save` to return the
/// error with the prior snapshot still loadable and byte-identical. A plain
/// `fs::write` succeeds here — the file itself stays writable — so the test
/// fails against it.
#[cfg(unix)]
#[test]
fn failed_dream_stats_save_keeps_the_prior_snapshot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(PersistedDreamStats::FILE_NAME);
    snapshot(7).save(dir.path()).expect("seed save");
    let before = std::fs::read(&path).expect("read seed");
    let Some(read_only) = crate::atomic_file::test_support::ReadOnlyDir::new(dir.path()) else {
        return;
    };

    let err = snapshot(99)
        .save(dir.path())
        .expect_err("save must report the failed staging write");

    assert!(err.to_string().contains("dream_stats.json"), "{err:#}");
    assert_eq!(std::fs::read(&path).expect("read after"), before);
    let loaded = PersistedDreamStats::load(dir.path())
        .expect("prior snapshot still parses")
        .expect("prior snapshot still present");
    assert_eq!(loaded.stats.merged, 7);
    drop(read_only);
    assert_eq!(entries(dir.path()), [PersistedDreamStats::FILE_NAME]);
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
    assert_eq!(
        entries(&data_dir),
        [PersistedDreamStats::FILE_NAME],
        "a staging file survived"
    );
}

/// Why: #8733 — the idle loop's `dream_cycle` and a `memory.dream_run` RPC
/// can save one palace's snapshot at the same time. With one shared staging
/// name, one writer renamed a file the other was still filling (a reader saw
/// it empty) and the other's rename then failed with `ENOENT`.
/// What: two threads save the same path in a loop while this thread loads for
/// a bounded 2 s; no load may fail, no save may fail, and no staging file may
/// be left behind.
#[test]
fn two_concurrent_dream_stats_writers_never_publish_a_partial_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().to_path_buf();
    snapshot(0).save(&data_dir).expect("seed save");

    let stop = Arc::new(AtomicBool::new(false));
    let writers: Vec<_> = (0..2)
        .map(|_| {
            let (stop, data_dir) = (Arc::clone(&stop), data_dir.clone());
            std::thread::spawn(move || {
                let (mut saves, mut errors, mut first_error) = (0usize, 0usize, None);
                while !stop.load(Ordering::Relaxed) {
                    if let Err(e) = snapshot(saves).save(&data_dir) {
                        errors += 1;
                        first_error.get_or_insert_with(|| format!("{e:#}"));
                    }
                    saves += 1;
                }
                (saves, errors, first_error)
            })
        })
        .collect();

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
    let results: Vec<_> = writers
        .into_iter()
        .map(|w| w.join().expect("writer thread"))
        .collect();

    let saves: usize = results.iter().map(|r| r.0).sum();
    let errors: usize = results.iter().map(|r| r.1).sum();
    let first_error = results.iter().find_map(|r| r.2.clone());
    assert!(saves > 0 && reads > 0, "saves={saves} reads={reads}");
    assert_eq!(
        errors, 0,
        "{errors} of {saves} saves failed; first: {first_error:?}"
    );
    assert_eq!(
        bad, 0,
        "{bad} of {reads} loads saw an empty or partial dream_stats.json \
         across {saves} saves; first: {first_bad:?}"
    );
    assert_eq!(entries(&data_dir), [PersistedDreamStats::FILE_NAME]);
}
