//! Tests for the atomic `goals.json` hot cache (SM-6, DOC-14 §9.4).
//!
//! Why: prove the cache contract the store relies on — the file lives under
//! `<root>/sm/goals.json`, save→load round-trips, save atomically overwrites, a
//! missing file loads as empty (first-touch is not an error), and a malformed file
//! is a hard error. All tests point at a `tempdir` so they never touch the real
//! `~/.trusty-mpm`.

use super::cache::{GOALS_CACHE_FILE, GoalCache, SM_STATE_SUBDIR};
use super::error::SmGoalError;
use super::model::{Goal, GoalStatus};
use chrono::{TimeZone, Utc};
use tempfile::TempDir;

fn ts() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap()
}

fn sample_goals() -> Vec<Goal> {
    let mut a = Goal::new("g-aaaa1111", "first goal", vec!["t1".into()], ts());
    a.status = GoalStatus::InProgress;
    let b = Goal::new("g-bbbb2222", "second goal", vec![], ts());
    vec![a, b]
}

/// Why: the cache path must be exactly `<root>/sm/goals.json` (§9.4) so the TUI
/// and the store agree on where it lives.
/// What: asserts `dir()` ends in `sm` and `path()` is the well-known file.
/// Test: this is the test.
#[test]
fn cache_path_is_under_sm_subdir() {
    let dir = TempDir::new().expect("tempdir");
    let cache = GoalCache::new(dir.path());
    assert!(cache.dir().ends_with(SM_STATE_SUBDIR));
    assert!(
        cache
            .path()
            .ends_with(format!("{SM_STATE_SUBDIR}/{GOALS_CACHE_FILE}"))
    );
}

/// Why: the core round-trip — saved goals must reload byte-identically.
/// What: saves a goal list, loads it, asserts equality.
/// Test: this is the test.
#[test]
fn save_then_load_round_trips() {
    let dir = TempDir::new().expect("tempdir");
    let cache = GoalCache::new(dir.path());
    let goals = sample_goals();

    cache.save(&goals).expect("save");
    let back = cache.load().expect("load");
    assert_eq!(goals, back);
}

/// Why: a never-written cache must load as an empty list, not error — first-touch
/// on a fresh daemon is normal.
/// What: loads from a fresh root, asserts an empty vec.
/// Test: this is the test.
#[test]
fn load_missing_is_empty() {
    let dir = TempDir::new().expect("tempdir");
    let cache = GoalCache::new(dir.path());
    assert!(cache.load().expect("load").is_empty());
}

/// Why: `save` must create the `sm/` subdir lazily (constructing the cache does no
/// I/O), so the daemon need not pre-create it.
/// What: saves into a fresh root and asserts the file now exists.
/// Test: this is the test.
#[test]
fn save_creates_sm_subdir() {
    let dir = TempDir::new().expect("tempdir");
    let cache = GoalCache::new(dir.path());
    assert!(!cache.dir().exists(), "no I/O before save");
    cache.save(&sample_goals()).expect("save");
    assert!(cache.path().exists(), "save must create the cache file");
}

/// Why: re-saving must atomically REPLACE the file (write-tmp-then-rename), leaving
/// exactly the new content and no stray temp files.
/// What: saves twice with different content, asserts the second wins and only the
/// cache file remains in the dir.
/// Test: this is the test.
#[test]
fn save_is_atomic_overwrite() {
    let dir = TempDir::new().expect("tempdir");
    let cache = GoalCache::new(dir.path());

    cache.save(&sample_goals()).expect("first save");
    let smaller = vec![Goal::new("g-cccc3333", "only goal", vec![], ts())];
    cache.save(&smaller).expect("second save");

    let back = cache.load().expect("load");
    assert_eq!(back, smaller, "second save must fully replace the first");

    let entries: Vec<_> = std::fs::read_dir(cache.dir())
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        entries,
        vec![GOALS_CACHE_FILE.to_string()],
        "only the cache file must remain — no leaked temp files; got {entries:?}"
    );
}

/// Why: a present-but-corrupt cache is operator-visible corruption, not silently
/// discarded — it must surface as a hard `Serde` error.
/// What: writes garbage to the cache path then asserts `load` returns
/// `SmGoalError::Serde`.
/// Test: this is the test.
#[test]
fn load_rejects_malformed_file() {
    let dir = TempDir::new().expect("tempdir");
    let cache = GoalCache::new(dir.path());
    std::fs::create_dir_all(cache.dir()).expect("mkdir");
    std::fs::write(cache.path(), b"{ this is not valid json").expect("write garbage");

    match cache.load() {
        Err(SmGoalError::Serde { .. }) => {}
        other => panic!("malformed cache must be a Serde error, got {other:?}"),
    }
}
