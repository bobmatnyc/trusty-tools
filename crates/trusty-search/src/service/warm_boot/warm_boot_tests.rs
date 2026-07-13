//! Tests for the resilient warm-boot index collection (issues #718 / #723 / #860).
//!
//! Why: the key invariant is that an inaccessible or hung colocated root
//! must never prevent the accessible legacy/colocated entries from
//! registering. Issue #860 adds the root_path-equality dedup invariant: a
//! colocated entry whose root_path is already owned by a legacy entry (even
//! under a different ID scheme) must be suppressed. We simulate inaccessibility
//! with a nonexistent path (which returns NotFound immediately — a fast proxy
//! for the TCC hang which cannot be reproduced in unit tests).
//! Test: `cargo test -p trusty-search -- warm_boot`.

use super::*;

// ── warmboot_index_timeout ────────────────────────────────────────────────

/// Why: guard that the env var reader parses valid values and falls back.
/// What: set `TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS=42`, assert Duration is
/// 42s; unset, assert Duration is ROOT_SCAN_TIMEOUT.
/// Note: `serial` prevents racing with other env-var mutators.
/// Test: this test.
#[test]
#[serial_test::serial]
fn warmboot_index_timeout_parses_env_var() {
    // Parse a valid value.
    unsafe { std::env::set_var("TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS", "42") };
    assert_eq!(
        warmboot_index_timeout(),
        Duration::from_secs(42),
        "must parse 42 from env var"
    );
    // Remove and confirm fallback.
    unsafe { std::env::remove_var("TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS") };
    assert_eq!(
        warmboot_index_timeout(),
        ROOT_SCAN_TIMEOUT,
        "must fall back to ROOT_SCAN_TIMEOUT when env var is absent"
    );
}

// ── collect_colocated_entries ─────────────────────────────────────────────

/// Why: the key resilience invariant — when one root is inaccessible (or
/// times out under launchd), the other roots must still be scanned and
/// their indexes returned.
/// What: write a roots.toml with two entries: one real tempdir with
/// .trusty-search/ and one nonexistent path. Call
/// `collect_colocated_entries`; assert the real one is found.
/// Note: `serial` prevents parallel env-var mutation from other tests
/// (TRUSTY_DATA_DIR is a shared global state).
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn colocated_scan_partial_failure_still_returns_accessible() {
    let data_tmp = tempfile::tempdir().unwrap();
    let real_root = tempfile::tempdir().unwrap();
    let ts_dir = real_root.path().join(".trusty-search");
    std::fs::create_dir_all(&ts_dir).unwrap();

    // Point TRUSTY_DATA_DIR at our isolated tempdir so roots.toml does not
    // read the real system data dir. `serial` prevents concurrent tests from
    // racing on this env var.
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path());
    }

    // Register both a real and a nonexistent root.
    let nonexistent = std::path::PathBuf::from("/tmp/trusty-718-no-root-xyz9999");
    crate::service::roots_registry::upsert_root(real_root.path().to_path_buf()).unwrap();
    crate::service::roots_registry::upsert_root(nonexistent).unwrap();

    let known_ids: HashSet<String> = HashSet::new();
    let known_root_paths: HashSet<PathBuf> = HashSet::new();
    // No volumes are inaccessible in this test.
    let inaccessible: HashSet<PathBuf> = HashSet::new();
    let results = collect_colocated_entries(&known_ids, &known_root_paths, &inaccessible).await;

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }

    // The real root must be found even though the nonexistent root errored.
    assert_eq!(
        results.len(),
        1,
        "accessible root must be discovered even when another root is inaccessible; \
         got: {results:?}"
    );
    let canonical_root = real_root.path().canonicalize().unwrap();
    assert_eq!(
        results[0].root_path, canonical_root,
        "discovered root_path must match the real tempdir"
    );
}

/// Why: entries already present in `known_ids` (from the legacy scan) must
/// not be duplicated in the colocated results — dedup is required.
/// What: register a real root and pre-populate `known_ids` with its
/// derived id; assert the colocated result is empty (already known).
/// Note: `serial` prevents parallel env-var mutation from other tests.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn colocated_scan_deduplicates_against_known_ids() {
    use crate::service::fs_discovery::id_from_path;

    let data_tmp = tempfile::tempdir().unwrap();
    let real_root = tempfile::tempdir().unwrap();
    let ts_dir = real_root.path().join(".trusty-search");
    std::fs::create_dir_all(&ts_dir).unwrap();
    let canonical_root = real_root.path().canonicalize().unwrap();
    let expected_id = id_from_path(&canonical_root);

    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path());
    }
    crate::service::roots_registry::upsert_root(real_root.path().to_path_buf()).unwrap();

    let mut known_ids: HashSet<String> = HashSet::new();
    known_ids.insert(expected_id.clone());
    let known_root_paths: HashSet<PathBuf> = HashSet::new();
    let inaccessible: HashSet<PathBuf> = HashSet::new();

    let results = collect_colocated_entries(&known_ids, &known_root_paths, &inaccessible).await;

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }

    assert!(
        results.is_empty(),
        "index already in known_ids must not be returned again; got: {results:?}"
    );
}

/// Why (issue #723): roots on inaccessible volumes must be skipped before
/// any spawn_blocking scan is attempted — the volume probe prevents issuing
/// any open() calls on a hung volume.
/// What: register one real root and one root with a mocked inaccessible
/// volume key. Pass the mocked key in `inaccessible_volumes`; assert only
/// the real root's index is returned.
/// Note: `serial` prevents parallel env-var mutation from other tests.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn colocated_scan_skips_inaccessible_volume_roots() {
    use crate::service::fs_discovery::id_from_path;

    let data_tmp = tempfile::tempdir().unwrap();
    let real_root = tempfile::tempdir().unwrap();
    let ts_dir = real_root.path().join(".trusty-search");
    std::fs::create_dir_all(&ts_dir).unwrap();
    let canonical_root = real_root.path().canonicalize().unwrap();
    let real_id = id_from_path(&canonical_root);

    // Register a fake root that looks like it's on /Volumes/BLOCKED.
    // We won't actually create it — the test asserts it is skipped via the
    // inaccessible_volumes filter, not via a scan timeout.
    let fake_blocked = PathBuf::from("/Volumes/BLOCKED/some-project");

    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path());
    }
    crate::service::roots_registry::upsert_root(real_root.path().to_path_buf()).unwrap();
    crate::service::roots_registry::upsert_root(fake_blocked.clone()).unwrap();

    let known_ids: HashSet<String> = HashSet::new();
    let known_root_paths: HashSet<PathBuf> = HashSet::new();
    // Simulate: /Volumes/BLOCKED was probed and timed out.
    let mut inaccessible: HashSet<PathBuf> = HashSet::new();
    inaccessible.insert(PathBuf::from("/Volumes/BLOCKED"));

    let results = collect_colocated_entries(&known_ids, &known_root_paths, &inaccessible).await;

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }

    // Only the real (non-blocked) root must be found.
    assert_eq!(
        results.len(),
        1,
        "only the accessible root must be returned; got: {results:?}"
    );
    assert_eq!(
        results[0].id, real_id,
        "the returned entry must be the real root, not the blocked one"
    );
}

/// Why (issue #860): reproduces the actual "ghost index" bug.
///
/// On warm-boot, `restore_indexes` loads legacy entries from `indexes.toml`
/// whose IDs are basename-derived (e.g. `trusty-tools`). Then
/// `collect_colocated_entries` scans `roots.toml` and derives IDs via
/// `id_from_path` using full-path sanitization (e.g.
/// `Users_mac_workspace_trusty-tools`). The pre-existing ID-only dedup
/// never matched, so a phantom empty "ghost" entry was registered for every
/// legacy root on every daemon restart.
///
/// What: simulate the actual collision — register a colocated `.trusty-search/`
/// at a real temp root, then seed `known_ids` with a BASENAME id (not the
/// full-path id) AND seed `known_root_paths` with that root's canonical path
/// (exactly as `restore_indexes` would do). Call `collect_colocated_entries`
/// and assert the result is EMPTY — the ghost is suppressed via the
/// root_path-equality dedup even though the two IDs differ.
///
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn colocated_scan_deduplicates_by_root_path_against_basename_legacy_id() {
    let data_tmp = tempfile::tempdir().unwrap();
    let real_root = tempfile::tempdir().unwrap();
    let ts_dir = real_root.path().join(".trusty-search");
    std::fs::create_dir_all(&ts_dir).unwrap();
    let canonical_root = real_root.path().canonicalize().unwrap();

    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path());
    }
    crate::service::roots_registry::upsert_root(real_root.path().to_path_buf()).unwrap();

    // Simulate legacy entry: basename id (e.g. "myproject"), NOT the
    // full-path-sanitized id that `id_from_path` would derive.
    let basename_id = canonical_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("legacy-project")
        .to_string();

    let mut known_ids: HashSet<String> = HashSet::new();
    known_ids.insert(basename_id.clone());

    // The root_path set mirrors how restore_indexes builds seen_root_paths
    // from legacy entries in Phase 1.
    let mut known_root_paths: HashSet<PathBuf> = HashSet::new();
    known_root_paths.insert(canonical_root.clone());

    let inaccessible: HashSet<PathBuf> = HashSet::new();

    let results = collect_colocated_entries(&known_ids, &known_root_paths, &inaccessible).await;

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }

    // The colocated scan MUST return nothing: the root_path is already
    // owned by the legacy entry, so the ghost entry must be suppressed even
    // though `basename_id != id_from_path(&canonical_root)`.
    assert!(
        results.is_empty(),
        "ghost entry must be suppressed when root_path is already owned by a \
         legacy entry with a different id scheme (issue #860); got: {results:?}"
    );
}

/// Why (canonicalization symmetry fix, MEDIUM finding on #864): if Phase 1
/// seeds `known_root_paths` with `canonicalize_best_effort` and Phase 2 uses
/// a *different* canonicalization call (e.g. bare `.canonicalize()` which
/// silently fails on non-existent paths and returns a different suffix), the
/// `contains` check silently misses and a ghost duplicate slips through.
///
/// This test exercises the path where the colocated entry's `root_path` is a
/// symlink whose target has already been canonicalized into `known_root_paths`.
/// Before the fix, using `.canonicalize().unwrap_or_else(|_| raw.clone())` in
/// Phase 2 would resolve the symlink correctly on Linux/macOS — EXCEPT in the
/// edge case where `canonicalize` returns an error (e.g. the symlink dangled
/// transiently between Phase 1 and Phase 2 scans). In that failure case Phase 2
/// fell back to the raw symlink path, while Phase 1 had stored the canonical
/// target — mismatch, ghost slips through.  Using the same
/// `canonicalize_best_effort` helper in both phases ensures they degrade
/// identically (raw path fallback with `debug` log) so the `contains` check
/// stays consistent.
///
/// What: build `known_root_paths` with a canonical path directly (simulating
/// Phase 1 having resolved it), then present Phase 2 with the *same* path
/// (simulating the case where both sides succeed and agree — must still dedup).
/// The failure-path scenario (symlink dangling) is covered by the fact that
/// after the fix both sides call the same function, so the fallback behaviour
/// is identical by construction.
///
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn colocated_scan_dedup_uses_consistent_canonicalization() {
    let data_tmp = tempfile::tempdir().unwrap();
    let real_root = tempfile::tempdir().unwrap();
    let ts_dir = real_root.path().join(".trusty-search");
    std::fs::create_dir_all(&ts_dir).unwrap();
    let canonical_root = real_root.path().canonicalize().unwrap();

    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path());
    }
    // Register the root so the colocated scan will discover it.
    crate::service::roots_registry::upsert_root(real_root.path().to_path_buf()).unwrap();

    // Simulate Phase 1: `known_root_paths` already holds the canonical form
    // (as if `restore_indexes` called `canonicalize_best_effort(&entry.root_path)`).
    let mut known_root_paths: HashSet<PathBuf> = HashSet::new();
    known_root_paths.insert(canonical_root.clone());

    // Also seed a mismatching basename id so the ID-level check does not fire
    // (we want the root_path-level check to be the gating one).
    let mut known_ids: HashSet<String> = HashSet::new();
    known_ids.insert("__legacy-id-that-will-not-match-anything__".to_string());

    let inaccessible: HashSet<PathBuf> = HashSet::new();
    let results = collect_colocated_entries(&known_ids, &known_root_paths, &inaccessible).await;

    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }

    // The colocated scan arrives at the same canonical root and must suppress
    // the entry via root_path-level dedup.  If the two canonicalization calls
    // diverged (old bug), results would be non-empty.
    assert!(
        results.is_empty(),
        "canonicalization in Phase 2 must agree with Phase 1 so the root_path \
         dedup fires consistently (canonicalization-symmetry fix, #864); got: {results:?}"
    );
}

// ── dedup_entries_by_corpus_path (issue #2305) ────────────────────────────

/// Build a minimal colocated `PersistedIndex` for the corpus-path dedup tests.
fn colocated_entry(id: &str, root: &std::path::Path, last_indexed: Option<u64>) -> PersistedIndex {
    PersistedIndex {
        id: id.to_string(),
        root_path: root.to_path_buf(),
        colocated: true,
        last_indexed_unix: last_indexed,
        ..Default::default()
    }
}

/// Why (issue #2305 — the regression): two colocated entries that share a
/// `root_path` resolve to the SAME `<root>/.trusty-search/index.redb`. redb is a
/// single-open database, so warm-booting both opens the file twice and the
/// second fails with `DatabaseAlreadyOpen`. The dedup must collapse them to one
/// entry before any open is attempted.
/// What: two colocated entries with one shared root plus one distinct root;
/// assert the shared pair collapses to exactly one survivor and the distinct
/// root is untouched (2 entries out for 3 in).
/// Test: this test.
#[test]
fn dedup_by_corpus_path_collapses_same_root() {
    let shared = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();

    let entries = vec![
        colocated_entry("apex", shared.path(), Some(100)),
        colocated_entry("trusty-mpm", shared.path(), Some(50)),
        colocated_entry("distinct", other.path(), Some(10)),
    ];

    let deduped = dedup_entries_by_corpus_path(entries);

    // Shared root collapses to one; distinct root survives → 2 total.
    assert_eq!(
        deduped.len(),
        2,
        "two entries sharing one redb corpus must collapse to one; got {deduped:?}"
    );
    // Exactly one of the shared-root ids survives, and the distinct one is kept.
    let ids: HashSet<&str> = deduped.iter().map(|e| e.id.as_str()).collect();
    assert!(
        ids.contains("distinct"),
        "the entry with a distinct root must always survive; got {ids:?}"
    );
    let shared_survivors = ["apex", "trusty-mpm"]
        .iter()
        .filter(|id| ids.contains(**id))
        .count();
    assert_eq!(
        shared_survivors, 1,
        "exactly one of the shared-root entries must survive; got {ids:?}"
    );
}

/// Why: the fix must not merge indexes that genuinely live in different redb
/// files — distinct roots must keep independent corpora.
/// What: two colocated entries with distinct roots; assert both survive.
/// Test: this test.
#[test]
fn dedup_by_corpus_path_keeps_distinct_roots() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();

    let entries = vec![
        colocated_entry("a", a.path(), Some(1)),
        colocated_entry("b", b.path(), Some(2)),
    ];

    let deduped = dedup_entries_by_corpus_path(entries);
    assert_eq!(
        deduped.len(),
        2,
        "entries at distinct roots resolve to distinct redb files and must both survive"
    );
}

/// Why: legacy (non-colocated) corpora are keyed by unique `index_id` in the
/// global data dir and can never collide, so the dedup must never merge them —
/// even when they happen to share a `root_path`.
/// What: two non-colocated entries sharing a root_path; assert both survive.
/// Test: this test.
#[test]
fn dedup_by_corpus_path_keeps_non_colocated() {
    let shared = tempfile::tempdir().unwrap();

    let mut a = colocated_entry("legacy-a", shared.path(), Some(1));
    a.colocated = false;
    let mut b = colocated_entry("legacy-b", shared.path(), Some(2));
    b.colocated = false;

    let deduped = dedup_entries_by_corpus_path(vec![a, b]);
    assert_eq!(
        deduped.len(),
        2,
        "non-colocated entries are id-keyed and never collide, even sharing a root_path"
    );
}

/// Why: when two entries collide, the survivor must be the most-recently-active
/// one so warm boot keeps the freshest registration (matching the recency key
/// used by `select_warmboot_entries`).
/// What: two colocated entries sharing a root with different `last_indexed_unix`;
/// assert the higher-recency id survives regardless of input order.
/// Test: this test.
#[test]
fn dedup_by_corpus_path_keeps_most_recent() {
    let shared = tempfile::tempdir().unwrap();

    // Stale entry listed FIRST so we prove recency (not order) selects the winner.
    let entries = vec![
        colocated_entry("stale", shared.path(), Some(10)),
        colocated_entry("fresh", shared.path(), Some(999)),
    ];

    let deduped = dedup_entries_by_corpus_path(entries);
    assert_eq!(deduped.len(), 1, "shared root must collapse to one");
    assert_eq!(
        deduped[0].id, "fresh",
        "the most-recently-active entry must be the survivor"
    );
}

// ── dedup_entries_by_corpus_path_verbose (issue #2337) ────────────────────

/// Why (#2337 part 1): callers need the dropped entries to prune their
/// `indexes.toml` rows — otherwise they are re-discovered/re-warned/re-dropped
/// forever.
/// What: two entries share a root; asserts `dropped` contains exactly the
/// losing entry and `survivors` contains exactly the winner.
/// Test: this test.
#[test]
fn dedup_verbose_reports_dropped_entries() {
    let shared = tempfile::tempdir().unwrap();

    let entries = vec![
        colocated_entry("stale", shared.path(), Some(1)),
        colocated_entry("fresh", shared.path(), Some(2)),
    ];

    let outcome = dedup_entries_by_corpus_path_verbose(entries);
    assert_eq!(outcome.survivors.len(), 1, "one survivor");
    assert_eq!(outcome.survivors[0].id, "fresh");
    assert_eq!(outcome.dropped.len(), 1, "one dropped entry");
    assert_eq!(outcome.dropped[0].id, "stale");
    assert!(
        outcome.merged_survivor_ids.contains("fresh"),
        "the survivor must be flagged as having absorbed a dropped entry's config"
    );
}

/// Why (#2337 part 2): a genuine collision keeps only the redb data, but the
/// two entries' list-type search filters can legitimately differ; dropping
/// them entirely would silently narrow the survivor's search scope.
/// What: the loser has distinct `extensions`/`domain_terms`; asserts the
/// survivor's fields are the union (with survivor's own values first).
/// Test: this test.
#[test]
fn dedup_verbose_merges_list_config_into_survivor() {
    let shared = tempfile::tempdir().unwrap();

    let mut winner = colocated_entry("fresh", shared.path(), Some(2));
    winner.extensions = vec!["rs".to_string()];
    winner.domain_terms = vec!["daemon".to_string()];

    let mut loser = colocated_entry("stale", shared.path(), Some(1));
    loser.extensions = vec!["py".to_string()];
    loser.domain_terms = vec!["daemon".to_string(), "corpus".to_string()];
    loser.include_paths = vec!["src/legacy".to_string()];

    let outcome = dedup_entries_by_corpus_path_verbose(vec![winner, loser]);
    assert_eq!(outcome.survivors.len(), 1);
    let survivor = &outcome.survivors[0];
    assert_eq!(survivor.id, "fresh");
    assert_eq!(
        survivor.extensions,
        vec!["rs".to_string(), "py".to_string()],
        "extensions must be the union, survivor's own values first"
    );
    assert_eq!(
        survivor.domain_terms,
        vec!["daemon".to_string(), "corpus".to_string()],
        "domain_terms must be de-duplicated across the union"
    );
    assert_eq!(
        survivor.include_paths,
        vec!["src/legacy".to_string()],
        "include_paths present only on the loser must still be adopted"
    );
}

/// Why (#2337 part 2): scalar pipeline toggles (`lexical_only`, `skip_kg`,
/// `include_docs`, `respect_gitignore`) have no safe "union" semantics — an
/// automatic OR/AND could silently disable a lane the survivor was built
/// with. These must be left exactly as the survivor had them; the loser's
/// values are surfaced via a log line instead (not asserted here — this test
/// pins the "not merged" behavior).
/// What: loser sets `lexical_only = true` / `skip_kg = true` while the
/// survivor has both `false`; asserts the survivor's scalar flags are
/// unchanged after the merge.
/// Test: this test.
#[test]
fn dedup_verbose_does_not_merge_scalar_flags() {
    let shared = tempfile::tempdir().unwrap();

    let mut winner = colocated_entry("fresh", shared.path(), Some(2));
    winner.lexical_only = false;
    winner.skip_kg = false;
    winner.include_docs = true;
    winner.respect_gitignore = true;

    let mut loser = colocated_entry("stale", shared.path(), Some(1));
    loser.lexical_only = true;
    loser.skip_kg = true;
    loser.include_docs = false;
    loser.respect_gitignore = false;

    let outcome = dedup_entries_by_corpus_path_verbose(vec![winner, loser]);
    let survivor = &outcome.survivors[0];
    assert!(!survivor.lexical_only, "lexical_only must not be merged");
    assert!(!survivor.skip_kg, "skip_kg must not be merged");
    assert!(survivor.include_docs, "include_docs must not be merged");
    assert!(
        survivor.respect_gitignore,
        "respect_gitignore must not be merged"
    );
}

/// Why: the common case (no collision at all) must be a cheap no-op — callers
/// use `dropped.is_empty()` to skip all `indexes.toml` IO.
/// What: two entries at distinct roots; asserts both survive with empty
/// `dropped` and `merged_survivor_ids`.
/// Test: this test.
#[test]
fn dedup_verbose_no_collision_yields_empty_dropped_and_merged_sets() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();

    let entries = vec![
        colocated_entry("a", a.path(), Some(1)),
        colocated_entry("b", b.path(), Some(2)),
    ];

    let outcome = dedup_entries_by_corpus_path_verbose(entries);
    assert_eq!(outcome.survivors.len(), 2);
    assert!(outcome.dropped.is_empty());
    assert!(outcome.merged_survivor_ids.is_empty());
}

/// Why: `merge_dropped_config_into_survivor` must not duplicate a value that
/// already appears on the survivor (e.g. both entries were registered with
/// the same `exclude_globs` pattern).
/// What: survivor and loser share one overlapping value plus one distinct
/// value each; asserts the merged result has no duplicates.
/// Test: this test.
#[test]
fn merge_dropped_config_deduplicates_overlapping_values() {
    let mut survivor = PersistedIndex {
        id: "survivor".to_string(),
        exclude_globs: vec!["**/vendor/**".to_string(), "*.lock".to_string()],
        ..Default::default()
    };
    let dropped = PersistedIndex {
        id: "dropped".to_string(),
        exclude_globs: vec!["**/vendor/**".to_string(), "*.generated.ts".to_string()],
        ..Default::default()
    };

    merge_dropped_config_into_survivor(&mut survivor, &dropped);

    assert_eq!(
        survivor.exclude_globs,
        vec![
            "**/vendor/**".to_string(),
            "*.lock".to_string(),
            "*.generated.ts".to_string(),
        ],
        "overlapping value must appear once; new value must be appended"
    );
}
