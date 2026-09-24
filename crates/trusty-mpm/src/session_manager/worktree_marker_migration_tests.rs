//! Unit tests for the one-shot fleet marker migration (#8511).
//! Test: this file IS the test module.

use super::*;
use crate::core::harness_exclude::HARNESS_EXCLUDE_ENTRIES;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

fn exclude_count(fx: &GitWorktreeFixture, entry: &str) -> usize {
    std::fs::read_to_string(fx.repo.join(".git").join("info").join("exclude"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.trim() == entry)
        .count()
}

/// Every tree's legacy marker moves; a second pass moves nothing and adds no
/// duplicate exclude line.
#[test]
fn the_fleet_pass_migrates_every_tree_and_excludes_once() {
    let fx = GitWorktreeFixture::new();
    let trees = [fx.add_worktree("one"), fx.add_worktree("two")];
    for wt in &trees {
        std::fs::write(legacy_sentinel_path(wt), b"{}").expect("write legacy");
    }
    let first = migrate_registered_projects(&fx.repos_root, &[]);
    assert_eq!(
        first,
        CheckoutTally {
            migrated: 2,
            kept: 0
        }
    );
    for wt in &trees {
        assert!(!legacy_sentinel_path(wt).exists());
        let admin = admin_sentinel_path(wt).expect("admin path");
        assert_eq!(std::fs::read(admin).ok(), Some(b"{}".to_vec()));
    }
    let second = migrate_registered_projects(&fx.repos_root, &[]);
    assert_eq!(second, CheckoutTally::default());
    for entry in HARNESS_EXCLUDE_ENTRIES {
        assert_eq!(exclude_count(&fx, entry), 1, "{entry}");
    }
}

/// A dry run plans and writes nothing; apply performs the plan; a repaired
/// fleet produces no steps.
#[test]
fn the_doctor_repair_previews_then_applies() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("doctor");
    std::fs::write(legacy_sentinel_path(&wt), b"{}").expect("write legacy");

    let plan = repair_worktree_markers(&fx.repos_root, &[], RepairMode::DryRun);
    assert_eq!(plan.len(), 2, "{plan:?}");
    assert!(plan.iter().all(|s| s.status == StepStatus::Planned));
    assert!(
        legacy_sentinel_path(&wt).exists(),
        "a dry run moved the marker"
    );
    assert_eq!(exclude_count(&fx, HARNESS_EXCLUDE_ENTRIES[0]), 0);

    let applied = repair_worktree_markers(&fx.repos_root, &[], RepairMode::Apply);
    assert_eq!(applied.len(), 2, "{applied:?}");
    assert!(applied.iter().all(RepairStep::changed), "{applied:?}");
    assert!(!legacy_sentinel_path(&wt).exists());

    assert!(repair_worktree_markers(&fx.repos_root, &[], RepairMode::Apply).is_empty());
}
