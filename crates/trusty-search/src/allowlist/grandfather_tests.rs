//! Unit tests for the #767 first-run grandfather pass.
//!
//! Why: this pass is the difference between switching on default-deny and
//! silently un-indexing a working install. Its two dangerous failure modes are
//! opposite: seeding too little (breaks the operator's setup) and seeding too
//! much (launders a sensitive root, or resurrects one the operator removed).
//! Both are asserted here.
//! What: fixtures write an `indexes.toml` in the daemon's registry format and
//! run the pass over injected paths.
//! Test: this file.

use std::path::{Path, PathBuf};

use super::sources::AllowlistPaths;
use super::{grandfather_existing_indexes, AllowlistConfig};
use crate::service::persistence::PersistedIndex;

/// A root that survives the hard denylist — see `sources_tests::safe_root`.
fn safe_root(name: &str) -> PathBuf {
    let base = dirs::home_dir()
        .expect("HOME required")
        .join(".trusty-search-grandfather-tests");
    let dir = base.join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test root");
    std::fs::canonicalize(&dir).expect("canonicalize test root")
}

fn fixture(dir: &Path) -> AllowlistPaths {
    AllowlistPaths::default()
        .with_allowlist(dir.join("allowlist.toml"))
        .with_project_paths(dir.join("projects.json"))
}

/// Write an `indexes.toml` in the daemon registry format.
fn write_registry(path: &Path, roots: &[(&str, &Path)]) {
    let entries: Vec<PersistedIndex> = roots
        .iter()
        .map(|(id, root)| PersistedIndex::new(*id, *root))
        .collect();
    crate::service::persistence::save_index_registry_at(path, &entries).expect("write registry");
}

/// The roots the daemon is already serving are carried into a fresh allowlist.
///
/// Why: this is the whole point — the 2026-08 box had seven registered roots
/// and no allowlist file, so without this the gate would have stopped indexing
/// all seven on the next restart.
#[test]
fn grandfather_seeds_registered_roots() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let registry = dir.path().join("indexes.toml");
    let a = safe_root("seed-a");
    let b = safe_root("seed-b");
    write_registry(&registry, &[("a", &a), ("b", &b)]);

    let outcome = grandfather_existing_indexes(&paths, &registry).expect("grandfather");
    assert_eq!(outcome.seeded, vec![a.clone(), b.clone()]);
    assert!(outcome.denied.is_empty());

    let cfg = AllowlistConfig::load_from(&paths.allowlist_file()).expect("load");
    assert!(cfg.contains(&a) && cfg.contains(&b), "{cfg:?}");
}

/// A registered root that the hard denylist refuses is NOT carried over.
///
/// Why: grandfathering preserves a working setup; it must not launder a
/// sensitive root that predates the gate into a standing approval.
#[test]
fn grandfather_skips_denied_roots() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let registry = dir.path().join("indexes.toml");
    let safe = safe_root("seed-safe");
    let sensitive = dirs::home_dir().expect("home").join(".ssh");
    write_registry(&registry, &[("safe", &safe), ("ssh", &sensitive)]);

    let outcome = grandfather_existing_indexes(&paths, &registry).expect("grandfather");
    assert_eq!(outcome.seeded, vec![safe.clone()]);
    assert_eq!(outcome.denied.len(), 1, "{outcome:?}");
    assert_eq!(outcome.denied[0].0, super::canonicalise(&sensitive));

    let cfg = AllowlistConfig::load_from(&paths.allowlist_file()).expect("load");
    assert!(
        !cfg.contains(&sensitive),
        "sensitive root must not be seeded"
    );
}

/// An existing allowlist is never rewritten — including one the operator
/// emptied deliberately. Re-seeding would resurrect roots they just removed.
#[test]
fn grandfather_noop_when_allowlist_exists() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let registry = dir.path().join("indexes.toml");
    let root = safe_root("seed-existing");
    write_registry(&registry, &[("a", &root)]);
    AllowlistConfig::default()
        .save_to(&paths.allowlist_file())
        .expect("write empty allowlist");

    let outcome = grandfather_existing_indexes(&paths, &registry).expect("grandfather");
    assert!(outcome.skipped_existing);
    assert!(outcome.seeded.is_empty());

    let cfg = AllowlistConfig::load_from(&paths.allowlist_file()).expect("load");
    assert!(
        cfg.entries.is_empty(),
        "an emptied allowlist must stay empty: {cfg:?}"
    );
}

/// A fresh install writes no file at all — default-deny with nothing to
/// grandfather is already the correct state.
#[test]
fn grandfather_noop_on_fresh_install() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let registry = dir.path().join("indexes.toml");

    let outcome = grandfather_existing_indexes(&paths, &registry).expect("grandfather");
    assert!(outcome.seeded.is_empty() && !outcome.skipped_existing);
    assert!(
        !paths.allowlist_file().exists(),
        "no allowlist file should be created for a fresh install"
    );
}

/// A root the project registry already approves is not duplicated into
/// `allowlist.toml` — that approval has its own lifecycle via `tm`.
#[test]
fn grandfather_skips_roots_the_project_registry_already_approves() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let registry = dir.path().join("indexes.toml");
    let project = safe_root("seed-project");
    let plain = safe_root("seed-plain");
    write_registry(&registry, &[("p", &project), ("q", &plain)]);
    std::fs::write(
        paths.project_paths_file(),
        serde_json::to_string(&[serde_json::json!({"alias":"p","path": &project})]).expect("json"),
    )
    .expect("write projects");

    let outcome = grandfather_existing_indexes(&paths, &registry).expect("grandfather");
    assert_eq!(outcome.seeded, vec![plain]);
}

// ── one-time-ness and concurrency (#767, finding 4) ──────────────────────────

/// Deleting `allowlist.toml` must NOT re-seed on the next start.
///
/// Why: deleting that file is a plausible "reset to default-deny" gesture. The
/// pass keyed only on the file's absence, so the next start would re-seed every
/// registered root as a standing approval — undoing exactly what the operator
/// just did. The durable stamp is what makes the pass one-time in fact.
#[test]
fn grandfather_does_not_reseed_after_the_allowlist_is_deleted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let registry = dir.path().join("indexes.toml");
    let root = safe_root("reseed");
    write_registry(&registry, &[("a", &root)]);

    let first = grandfather_existing_indexes(&paths, &registry).expect("first pass");
    assert_eq!(first.seeded, vec![root.clone()]);

    // The operator resets to default-deny.
    std::fs::remove_file(paths.allowlist_file()).expect("delete allowlist");

    let second = grandfather_existing_indexes(&paths, &registry).expect("second pass");
    assert!(second.skipped_existing, "{second:?}");
    assert!(second.seeded.is_empty(), "{second:?}");
    assert!(
        !paths.allowlist_file().exists(),
        "a deleted allowlist must stay deleted"
    );
}

/// A fresh install stamps too, so a later registry gaining entries plus a
/// missing allowlist cannot resurrect a seed pass.
#[test]
fn grandfather_stamps_even_on_a_fresh_install() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let registry = dir.path().join("indexes.toml");

    let first = grandfather_existing_indexes(&paths, &registry).expect("first pass");
    assert!(first.seeded.is_empty() && !first.skipped_existing);

    let root = safe_root("fresh-stamped");
    write_registry(&registry, &[("a", &root)]);
    let second = grandfather_existing_indexes(&paths, &registry).expect("second pass");
    assert!(
        second.skipped_existing,
        "the stamp must block a later seed: {second:?}"
    );
}

/// An allowlist created concurrently wins; the seed is discarded rather than
/// clobbering it.
///
/// Why: `exists()` then `save_to` is a check-then-write, and `save_to` renames
/// over whatever is there — so an `index add` landing inside that window would
/// be silently lost. `create_new(true)` inverts which side loses.
#[test]
fn grandfather_yields_to_a_concurrently_created_allowlist() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let registry = dir.path().join("indexes.toml");
    let seeded_root = safe_root("concurrent-seed");
    let raced_root = safe_root("concurrent-add");
    write_registry(&registry, &[("a", &seeded_root)]);

    // Stand in for the racing writer: the file exists by the time the pass
    // would write it. The `exists()` pre-check and the write are the two ends
    // of the window; this asserts the WRITE end refuses.
    let mut raced = AllowlistConfig::default();
    raced.upsert(super::AllowlistEntry {
        path: raced_root.clone(),
        name: None,
        exclude: Vec::new(),
        extensions: Vec::new(),
        skip_kg: false,
    });
    raced
        .save_to(&paths.allowlist_file())
        .expect("racing write");

    let outcome = grandfather_existing_indexes(&paths, &registry).expect("pass");
    assert!(outcome.seeded.is_empty(), "{outcome:?}");

    let cfg = AllowlistConfig::load_from(&paths.allowlist_file()).expect("load");
    assert!(
        cfg.contains(&raced_root),
        "the racing approval must survive"
    );
    assert!(
        !cfg.contains(&seeded_root),
        "the seed must not clobber the racing writer"
    );
}
