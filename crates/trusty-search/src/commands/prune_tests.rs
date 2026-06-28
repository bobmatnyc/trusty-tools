//! Tests for `commands/prune.rs` (issue #1782).
//!
//! Why: the prune command deletes index data; correctness of the protection,
//!      eligibility, and deletion logic must be verified by unit tests that
//!      don't touch the real registry or real disk.
//! What: drives `classify_entry` and `handle_prune_at` with injected tempfile
//!       paths, a deterministic `now_unix`, and a no-op `size_fn` so tests are
//!       fast, isolated, and fully offline.
//! Test: this file.

use super::*;
use crate::config::AutoPruneConfig;
use crate::service::persistence::{save_index_registry_at, PersistedIndex};
use std::path::PathBuf;
use tempfile::tempdir;

/// Seconds per day (86400) exposed for readability in test arithmetic.
const DAY: u64 = 86_400;

fn make_entry(id: &str, last_queried: Option<u64>, last_indexed: Option<u64>) -> PersistedIndex {
    PersistedIndex {
        id: id.to_string(),
        root_path: PathBuf::from(format!("/tmp/{id}")),
        last_queried_unix: last_queried,
        last_indexed_unix: last_indexed,
        ..Default::default()
    }
}

fn default_cfg() -> AutoPruneConfig {
    AutoPruneConfig {
        enabled: false,
        max_idle_days: 30,
        protected_indexes: vec![],
    }
}

fn no_size(_id: &str) -> Option<u64> {
    None
}

fn write_registry(path: &Path, entries: &[PersistedIndex]) {
    save_index_registry_at(path, entries).unwrap();
}

// ── classify_entry unit tests ────────────────────────────────────────────────

/// Why: pins the eligibility boundary at exactly `max_idle_days`.
/// What: an entry idle for exactly 30 days is eligible; 29 days is not.
/// Test: this test.
#[test]
fn prune_eligibility_boundary() {
    let cfg = default_cfg();
    let now: u64 = 1_000 * DAY;

    // Idle for exactly 30 days → eligible.
    let e30 = make_entry("a", Some(now - 30 * DAY), None);
    let d30 = classify_entry(&e30, &cfg, now, no_size);
    assert!(
        matches!(d30, PruneDecision::Eligible { idle_days: 30, .. }),
        "30-day idle should be eligible: {d30:?}"
    );

    // Idle for 29 days → recent.
    let e29 = make_entry("b", Some(now - 29 * DAY), None);
    let d29 = classify_entry(&e29, &cfg, now, no_size);
    assert!(
        matches!(d29, PruneDecision::Recent { idle_days: 29 }),
        "29-day idle should be recent: {d29:?}"
    );
}

/// Why: an index with no timestamps should never be auto-pruned.
/// What: entry with both fields None → NotTracked (not Eligible).
/// Test: this test.
#[test]
fn prune_not_tracked_is_not_eligible() {
    let cfg = default_cfg();
    let e = make_entry("x", None, None);
    let d = classify_entry(&e, &cfg, 1_000 * DAY, no_size);
    assert_eq!(d, PruneDecision::NotTracked);
}

/// Why: when `last_queried_unix` is absent but `last_indexed_unix` is set,
///      the indexed timestamp should be used as the fallback activity anchor.
/// What: entry with queried=None, indexed=old → should be Eligible.
/// Test: this test.
#[test]
fn prune_falls_back_to_last_indexed() {
    let cfg = default_cfg();
    let now: u64 = 1_000 * DAY;
    let e = make_entry("y", None, Some(now - 31 * DAY));
    let d = classify_entry(&e, &cfg, now, no_size);
    assert!(
        matches!(d, PruneDecision::Eligible { idle_days: 31, .. }),
        "should be eligible via indexed fallback: {d:?}"
    );
}

/// Why: protected indexes must never be classified as Eligible regardless
///      of how old they are.
/// What: entry whose id is in `protected_indexes` → Protected.
/// Test: this test.
#[test]
fn prune_protected_is_never_eligible() {
    let cfg = AutoPruneConfig {
        protected_indexes: vec!["critical".into()],
        ..default_cfg()
    };
    let now: u64 = 1_000 * DAY;
    // Even if idle for 1000 days.
    let e = make_entry("critical", Some(now - 1_000 * DAY), None);
    let d = classify_entry(&e, &cfg, now, no_size);
    assert_eq!(d, PruneDecision::Protected);
}

// ── handle_prune_at integration tests ────────────────────────────────────────

/// Why: dry-run must NOT modify the registry even when entries are eligible.
/// What: write an eligible entry, run without --apply, reload and assert
///       the entry is still present.
/// Test: this test.
#[test]
fn prune_dry_run_lists_but_does_not_delete() {
    let tmp = tempdir().unwrap();
    let toml = tmp.path().join("indexes.toml");
    let now = 1_000 * DAY;
    let entry = make_entry("old-proj", Some(now - 60 * DAY), None);
    write_registry(&toml, &[entry]);

    handle_prune_at(
        &toml,
        /*apply=*/ false,
        /*yes=*/ true,
        /*max_idle_days_override=*/ Some(30),
        default_cfg(),
        /*interactive=*/ false,
        no_size,
        now,
    )
    .unwrap();

    let after = load_index_registry_at(&toml).unwrap();
    assert_eq!(after.len(), 1, "dry-run must leave registry unchanged");
    assert_eq!(after[0].id, "old-proj");
}

/// Why: --apply must delete only indexes that are both eligible AND not
///      protected; recent and untracked ones must be preserved.
/// What: write three entries (old eligible, recent, untracked), run with
///       --apply --yes, reload, assert only the eligible one is removed.
/// Test: this test.
#[test]
fn prune_apply_deletes_only_eligible() {
    let tmp = tempdir().unwrap();
    let toml = tmp.path().join("indexes.toml");
    let now = 1_000 * DAY;

    let old = make_entry("old", Some(now - 60 * DAY), None);
    let fresh = make_entry("fresh", Some(now - 5 * DAY), None);
    let untracked = make_entry("untracked", None, None);
    write_registry(&toml, &[old, fresh, untracked]);

    handle_prune_at(
        &toml,
        /*apply=*/ true,
        /*yes=*/ true,
        /*max_idle_days_override=*/ Some(30),
        default_cfg(),
        /*interactive=*/ false,
        no_size,
        now,
    )
    .unwrap();

    let after = load_index_registry_at(&toml).unwrap();
    assert_eq!(
        after.len(),
        2,
        "only the eligible old entry must be removed"
    );
    let ids: Vec<&str> = after.iter().map(|e| e.id.as_str()).collect();
    assert!(ids.contains(&"fresh"), "fresh must survive");
    assert!(ids.contains(&"untracked"), "untracked must survive");
    assert!(!ids.contains(&"old"), "old must be removed");
}

/// Why: protected entries must survive even when idle beyond the threshold.
/// What: inject a cfg with `critical` in protected_indexes, write an old
///       `critical` entry, run --apply, assert the entry REMAINS in the
///       registry.
/// Test: this test.
#[test]
fn prune_apply_skips_protected() {
    let tmp = tempdir().unwrap();
    let toml = tmp.path().join("indexes.toml");
    let now = 1_000 * DAY;

    let e = make_entry("critical", Some(now - 999 * DAY), None);
    write_registry(&toml, &[e]);

    // Inject a cfg with "critical" as a protected index.
    let cfg = AutoPruneConfig {
        protected_indexes: vec!["critical".into()],
        ..default_cfg()
    };

    handle_prune_at(
        &toml,
        /*apply=*/ true,
        /*yes=*/ true,
        /*max_idle_days_override=*/ Some(30),
        cfg,
        /*interactive=*/ false,
        no_size,
        now,
    )
    .unwrap();

    let after = load_index_registry_at(&toml).unwrap();
    assert_eq!(
        after.len(),
        1,
        "protected entry must survive --apply: {after:?}"
    );
    assert_eq!(after[0].id, "critical", "the protected entry must remain");
}

/// Why: when both an eligible and a protected entry coexist, only the eligible
///      one must be removed.
/// What: write `critical` (protected) and `old` (eligible), run --apply with
///       cfg protecting `critical`, assert `old` is gone and `critical` survives.
/// Test: this test.
#[test]
fn prune_apply_removes_eligible_preserves_protected() {
    let tmp = tempdir().unwrap();
    let toml = tmp.path().join("indexes.toml");
    let now = 1_000 * DAY;

    let protected = make_entry("critical", Some(now - 999 * DAY), None);
    let old = make_entry("old", Some(now - 60 * DAY), None);
    write_registry(&toml, &[protected, old]);

    let cfg = AutoPruneConfig {
        protected_indexes: vec!["critical".into()],
        ..default_cfg()
    };

    handle_prune_at(
        &toml,
        /*apply=*/ true,
        /*yes=*/ true,
        /*max_idle_days_override=*/ Some(30),
        cfg,
        /*interactive=*/ false,
        no_size,
        now,
    )
    .unwrap();

    let after = load_index_registry_at(&toml).unwrap();
    assert_eq!(after.len(), 1, "exactly one entry must remain");
    assert_eq!(after[0].id, "critical", "critical must be preserved");
}

/// Why: an empty registry must be handled gracefully.
/// What: call handle_prune_at on an empty file, assert it returns Ok.
/// Test: this test.
#[test]
fn prune_empty_registry_is_noop() {
    let tmp = tempdir().unwrap();
    let toml = tmp.path().join("indexes.toml");
    let result = handle_prune_at(
        &toml,
        false,
        true,
        None,
        default_cfg(),
        false,
        no_size,
        1_000 * DAY,
    );
    assert!(result.is_ok(), "empty registry must not error");
}

/// Why: colocated indexes store data at `<root_path>/.trusty-search/`, NOT at
///      the global `<data_dir>/indexes/<id>/`. Deleting a colocated index must
///      remove the colocated dir, not the (absent) global dir.
/// What: create a tempdir as the fake root_path, create the `.trusty-search/`
///       subdir inside it, write a colocated registry entry, run --apply, assert
///       the `.trusty-search/` dir is gone and the entry is out of the registry.
/// Test: this test.
#[test]
fn prune_colocated_deletion_removes_colocated_dir() {
    use crate::service::colocated_storage::COLOCATED_DIR_NAME;

    let tmp = tempdir().unwrap();
    let toml = tmp.path().join("indexes.toml");

    // Fake project root with an existing colocated storage dir.
    let root = tmp.path().join("myproject");
    let colocated_dir = root.join(COLOCATED_DIR_NAME);
    std::fs::create_dir_all(&colocated_dir).unwrap();
    assert!(colocated_dir.exists(), "setup: colocated dir must exist");

    let now = 1_000 * DAY;
    let entry = PersistedIndex {
        id: "myproject".to_string(),
        root_path: root.clone(),
        last_queried_unix: Some(now - 60 * DAY),
        colocated: true,
        ..Default::default()
    };
    write_registry(&toml, &[entry]);

    handle_prune_at(
        &toml,
        /*apply=*/ true,
        /*yes=*/ true,
        /*max_idle_days_override=*/ Some(30),
        default_cfg(),
        /*interactive=*/ false,
        no_size,
        now,
    )
    .unwrap();

    // Registry entry must be gone.
    let after = load_index_registry_at(&toml).unwrap();
    assert_eq!(after.len(), 0, "registry entry must be removed");

    // Colocated data dir must be deleted.
    assert!(
        !colocated_dir.exists(),
        "colocated .trusty-search/ dir must be removed by --apply"
    );
}

/// Why: `format_bytes` must produce human-readable output at each scale.
/// What: checks the formatting for B, KB, MB, GB boundaries.
/// Test: this test.
#[test]
fn format_bytes_display_cases() {
    assert_eq!(format_bytes(512), "512 B");
    assert_eq!(format_bytes(1_024), "1 KB");
    assert_eq!(format_bytes(1_536), "2 KB");
    assert_eq!(format_bytes(1_048_576), "1.0 MB");
    assert_eq!(format_bytes(52_428_800), "50.0 MB");
    assert_eq!(format_bytes(1_073_741_824), "1.0 GB");
}
