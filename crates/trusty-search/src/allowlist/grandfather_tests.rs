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

/// Stand in for a prior boot that already ran the pass (#5926).
fn write_stamp_for(paths: &AllowlistPaths) {
    let stamp = super::grandfather::stamp_path(&paths.allowlist_file());
    if let Some(dir) = stamp.parent() {
        std::fs::create_dir_all(dir).expect("create stamp dir");
    }
    std::fs::write(&stamp, b"test").expect("write stamp");
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

/// Once the pass has run, an emptied allowlist stays empty. Re-seeding would
/// resurrect roots the operator just removed.
///
/// #5926: this used to seed the fixture with a bare allowlist file and no
/// stamp, which asserted the defect — the pass keyed on the file's existence,
/// so any pre-gate `allowlist.toml` blocked the migration and warm-boot then
/// dropped every registered root the file did not happen to list. The stamp is
/// what makes a removal a DECISION, so the fixture writes it.
#[test]
fn grandfather_noop_once_the_stamp_exists() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let registry = dir.path().join("indexes.toml");
    let root = safe_root("seed-existing");
    write_registry(&registry, &[("a", &root)]);
    AllowlistConfig::default()
        .save_to(&paths.allowlist_file())
        .expect("write empty allowlist");
    write_stamp_for(&paths);

    let outcome = grandfather_existing_indexes(&paths, &registry).expect("grandfather");
    assert!(outcome.skipped_already_done);
    assert!(outcome.seeded.is_empty());

    let cfg = AllowlistConfig::load_from(&paths.allowlist_file()).expect("load");
    assert!(
        cfg.entries.is_empty(),
        "an emptied allowlist must stay empty: {cfg:?}"
    );
}

/// The #5926 regression: a PARTIAL pre-upgrade `allowlist.toml` must not block
/// the migration.
///
/// Why: on the reporting box the file already held ~24 hand-added entries while
/// `indexes.toml` held 121 registrations, and the pass ran only when the file
/// was absent. It skipped, and warm-boot's `retain_approved_entries` then
/// excluded the 103 roots nothing had approved — `warm-boot DEGRADED: only
/// 11/37 indexes loaded` with `skipped_tcc: 0`. Against the pre-fix code this
/// test fails with `skipped_already_done: true` and an empty `seeded`.
#[test]
fn grandfather_seeds_roots_missing_from_a_partial_allowlist() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let registry = dir.path().join("indexes.toml");
    let known = safe_root("partial-known");
    let missing_a = safe_root("partial-missing-a");
    let missing_b = safe_root("partial-missing-b");
    write_registry(
        &registry,
        &[("known", &known), ("a", &missing_a), ("b", &missing_b)],
    );
    // The operator's pre-gate file: one of the three roots, added by hand.
    let mut existing = AllowlistConfig::default();
    existing.upsert(super::AllowlistEntry {
        path: known.clone(),
        name: None,
        exclude: Vec::new(),
        extensions: Vec::new(),
        skip_kg: false,
    });
    existing
        .save_to(&paths.allowlist_file())
        .expect("write partial allowlist");

    let outcome = grandfather_existing_indexes(&paths, &registry).expect("grandfather");
    assert_eq!(
        outcome.seeded,
        vec![missing_a.clone(), missing_b.clone()],
        "a partial pre-gate allowlist must be completed, not treated as curated: {outcome:?}"
    );

    let cfg = AllowlistConfig::load_from(&paths.allowlist_file()).expect("load");
    for root in [&known, &missing_a, &missing_b] {
        assert!(
            cfg.contains(root),
            "every registered root must survive the upgrade: {root:?} missing from {cfg:?}"
        );
    }
}

/// A root removed AFTER the pass ran stays removed on the next boot.
///
/// Why: this is the other half of #5926 and the constraint the fix must not
/// break. "Never approved because the gate is new" and "explicitly de-approved"
/// must not collapse into one another — the stamp is the only thing separating
/// them, so a second pass over a pruned file must add nothing back.
#[test]
fn grandfather_does_not_resurrect_a_root_removed_after_the_pass_ran() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let registry = dir.path().join("indexes.toml");
    let kept = safe_root("deapprove-kept");
    let removed = safe_root("deapprove-removed");
    write_registry(&registry, &[("kept", &kept), ("removed", &removed)]);

    let first = grandfather_existing_indexes(&paths, &registry).expect("first pass");
    assert_eq!(first.seeded, vec![kept.clone(), removed.clone()]);

    // The operator prunes one root — a deliberate de-approval, under a gate
    // that has already had its turn.
    crate::allowlist::remove_from_allowlist(&removed, Some(&paths.allowlist_file()))
        .expect("remove");

    let second = grandfather_existing_indexes(&paths, &registry).expect("second pass");
    assert!(second.skipped_already_done, "{second:?}");
    let cfg = AllowlistConfig::load_from(&paths.allowlist_file()).expect("load");
    assert!(cfg.contains(&kept));
    assert!(
        !cfg.contains(&removed),
        "a deliberate de-approval must survive the next boot: {cfg:?}"
    );
}

/// An `allowlist.toml` that does not parse is left alone and does NOT burn the
/// one-time pass.
///
/// Why: the merge path is a read-modify-write, so a file it cannot read is a
/// file it must not overwrite — writing the seed on top would discard every
/// approval the operator had. Warm-boot separately keeps every entry while the
/// allowlist is unreadable, so nothing is un-indexed while this waits.
#[test]
fn grandfather_leaves_an_unparseable_allowlist_alone_and_retries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let registry = dir.path().join("indexes.toml");
    let root = safe_root("corrupt-allowlist");
    write_registry(&registry, &[("a", &root)]);
    std::fs::write(paths.allowlist_file(), "not toml [[[").expect("write");

    let first = grandfather_existing_indexes(&paths, &registry).expect("first pass");
    assert!(
        first.seeded.is_empty() && !first.skipped_already_done,
        "{first:?}"
    );
    assert_eq!(
        std::fs::read_to_string(paths.allowlist_file()).expect("read"),
        "not toml [[[",
        "the operator's file must be untouched"
    );

    // They fix the syntax; the pass must still be available.
    AllowlistConfig::default()
        .save_to(&paths.allowlist_file())
        .expect("rewrite");
    let second = grandfather_existing_indexes(&paths, &registry).expect("second pass");
    assert_eq!(
        second.seeded,
        vec![root],
        "the pass must retry after an unparseable file, not be permanently burned"
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
    assert!(outcome.seeded.is_empty() && !outcome.skipped_already_done);
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
    assert!(second.skipped_already_done, "{second:?}");
    assert!(second.seeded.is_empty(), "{second:?}");
    assert!(
        !paths.allowlist_file().exists(),
        "a deleted allowlist must stay deleted"
    );
}

/// An UNREADABLE registry must not burn the one-time pass.
///
/// Why: the stamp means "the pass has had its turn", and a boot whose registry
/// could not be read never got one — a misdirected `TRUSTY_DATA_DIR` is the
/// likely cause and it is transient. Stamping there would make the next GOOD
/// boot grandfather nothing, and warm-boot would then silently drop every
/// previously-served root. The pass must retry.
#[test]
fn grandfather_does_not_stamp_when_the_registry_cannot_be_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let registry = dir.path().join("indexes.toml");
    // Not valid TOML — `load_index_registry_at` returns Err rather than empty.
    std::fs::write(&registry, "this is not toml [[[").expect("write");

    let first = grandfather_existing_indexes(&paths, &registry).expect("first pass");
    assert!(
        first.seeded.is_empty() && !first.skipped_already_done,
        "{first:?}"
    );
    assert!(
        !paths.allowlist_file().exists(),
        "nothing to write when the registry is unreadable"
    );

    // The registry becomes readable — the pass must still be available.
    let root = safe_root("retry-after-unreadable");
    write_registry(&registry, &[("a", &root)]);
    let second = grandfather_existing_indexes(&paths, &registry).expect("second pass");
    assert_eq!(
        second.seeded,
        vec![root],
        "the pass must retry after a failed read, not be permanently burned"
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
    assert!(first.seeded.is_empty() && !first.skipped_already_done);

    let root = safe_root("fresh-stamped");
    write_registry(&registry, &[("a", &root)]);
    let second = grandfather_existing_indexes(&paths, &registry).expect("second pass");
    assert!(
        second.skipped_already_done,
        "the stamp must block a later seed: {second:?}"
    );
}

/// A concurrently created allowlist and the seed both survive.
///
/// Why: `exists()` then `save_to` is a check-then-write, and `save_to` renames
/// over whatever is there — so an `index add` landing inside that window would
/// be silently lost. `create_new(true)` detects the race.
///
/// #5926: on detection the pass now MERGES rather than discarding the seed.
/// Discarding was safe while the pass only ran on a fresh install; now the seed
/// IS the migration, so throwing it away costs exactly the registered indexes
/// this pass exists to keep, and one `index add` in the window would do it.
#[test]
fn grandfather_merges_with_a_concurrently_created_allowlist() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let registry = dir.path().join("indexes.toml");
    let seeded_root = safe_root("concurrent-seed");
    let raced_root = safe_root("concurrent-add");
    write_registry(&registry, &[("a", &seeded_root)]);

    // Stand in for the racing writer: the file exists by the time the pass
    // would write it. The `exists()` pre-check and the write are the two ends
    // of the window; this asserts what the WRITE end does.
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
    assert_eq!(outcome.seeded, vec![seeded_root.clone()], "{outcome:?}");

    let cfg = AllowlistConfig::load_from(&paths.allowlist_file()).expect("load");
    assert!(
        cfg.contains(&raced_root),
        "the racing approval must survive: {cfg:?}"
    );
    assert!(
        cfg.contains(&seeded_root),
        "the seed must survive too: {cfg:?}"
    );
}

/// `create_new_toml` is what detects the race above — it must refuse an
/// existing file rather than renaming over it.
///
/// Why: the test above exercises the merge that follows detection, and would
/// still pass if `create_new_toml` silently overwrote instead. This pins the
/// detection itself.
#[test]
fn create_new_toml_refuses_an_existing_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("allowlist.toml");
    std::fs::write(&path, "# someone else's file\n").expect("write");

    let err = super::grandfather::create_new_toml(&path, &AllowlistConfig::default())
        .expect_err("must refuse");
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err:?}");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "# someone else's file\n",
        "the existing file must be untouched"
    );
}
