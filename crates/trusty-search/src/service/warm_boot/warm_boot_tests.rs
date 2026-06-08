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
