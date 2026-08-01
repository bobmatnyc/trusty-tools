//! Tests for [`crate::skills::unmanaged`] (issue #4605).
//!
//! Why: the detector decides whether a deployed skill is an unreachable
//! bundled copy or the operator's own work. Getting that wrong in one
//! direction hides the defect; in the other it accuses an operator's skill of
//! being tm's — the precursor to a reconcile touching it. Both directions are
//! pinned here.
//! What: staged skill directories under a temp deploy target, with and without
//! manifest entries, bundled-named and not.
//! Test: this file.

use super::*;
use crate::skills::deployer::deploy_skills;
use std::fs;
use tempfile::TempDir;

/// Build a `bundled` roster from stems.
fn roster(stems: &[&str]) -> BTreeSet<String> {
    stems.iter().map(|s| (*s).to_string()).collect()
}

/// Stage `<dest>/<stem>/SKILL.md` with `body`, bypassing the deployer so the
/// file is untracked — the exact state issue #4605 describes.
fn stage_untracked(dest: &Path, stem: &str, body: &str) {
    let dir = dest.join(stem);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(SKILL_ENTRY_POINT), body).unwrap();
}

#[test]
fn unmanaged_finds_a_bundled_named_untracked_skill() {
    // The #4605 shape: a bundled skill on disk, absent from the manifest.
    let dest = TempDir::new().unwrap();
    stage_untracked(dest.path(), "tm-workflow", "stale body");

    let found = unmanaged_bundled_skills(dest.path(), &roster(&["tm-workflow"]));
    assert_eq!(found.len(), 1, "expected one finding, got {found:?}");
    assert_eq!(found[0].stem, "tm-workflow");
    assert_eq!(found[0].dir, dest.path().join("tm-workflow"));
    assert_eq!(
        found[0].files,
        vec![dest.path().join("tm-workflow").join(SKILL_ENTRY_POINT)]
    );
}

#[test]
fn unmanaged_ignores_a_managed_skill() {
    // A skill the deployer wrote is tracked; it is reachable and not a finding.
    let source = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    fs::write(source.path().join("tm-workflow.md"), "v1").unwrap();
    deploy_skills(source.path(), dest.path()).unwrap();

    assert!(unmanaged_bundled_skills(dest.path(), &roster(&["tm-workflow"])).is_empty());
}

#[test]
fn unmanaged_ignores_an_operator_skill() {
    // A stem matching NOTHING bundled is the operator's own skill. It must
    // never be reported — reporting it is the first step toward a reconcile
    // overwriting work tm never authored.
    let dest = TempDir::new().unwrap();
    stage_untracked(dest.path(), "my-own-skill", "mine");

    assert!(unmanaged_bundled_skills(dest.path(), &roster(&["tm-workflow"])).is_empty());
}

#[test]
fn unmanaged_empty_roster_reports_nothing() {
    // An empty bundled roster means "cannot classify by name". Concluding
    // "nothing is bundled" would condemn every deployed skill at once.
    let dest = TempDir::new().unwrap();
    stage_untracked(dest.path(), "tm-workflow", "stale body");

    assert!(unmanaged_bundled_skills(dest.path(), &BTreeSet::new()).is_empty());
}

#[test]
fn unmanaged_missing_dest_is_empty() {
    // An unprovisioned tier is not a finding, and never an error.
    assert!(
        unmanaged_bundled_skills(Path::new("/nonexistent/skills"), &roster(&["tm"])).is_empty()
    );
}

#[test]
fn unmanaged_ignores_a_directory_without_an_entry_point() {
    // A directory that carries no SKILL.md is not a skill at all.
    let dest = TempDir::new().unwrap();
    fs::create_dir_all(dest.path().join("tm-workflow").join("references")).unwrap();

    assert!(unmanaged_bundled_skills(dest.path(), &roster(&["tm-workflow"])).is_empty());
}

#[test]
fn unmanaged_lists_reference_files() {
    // A multi-file skill's references share the entry point's ownership rule,
    // so they must be enumerated or a reconcile leaves them stale.
    let dest = TempDir::new().unwrap();
    stage_untracked(dest.path(), "tm-workflow", "stale body");
    let refs = dest.path().join("tm-workflow").join("references");
    fs::create_dir_all(&refs).unwrap();
    fs::write(refs.join("b.md"), "b").unwrap();
    fs::write(refs.join("a.md"), "a").unwrap();
    fs::write(refs.join("notes.txt"), "ignored").unwrap();

    let found = unmanaged_bundled_skills(dest.path(), &roster(&["tm-workflow"]));
    assert_eq!(
        found[0].files,
        vec![
            dest.path().join("tm-workflow").join(SKILL_ENTRY_POINT),
            refs.join("a.md"),
            refs.join("b.md"),
        ]
    );
}

#[test]
fn manifest_keys_match_the_deployer_key_shape() {
    // The reconcile must insert the keys `deploy_one_file` looks up, or the
    // next deploy skips the file anyway.
    let dest = TempDir::new().unwrap();
    stage_untracked(dest.path(), "tm-workflow", "stale body");
    let refs = dest.path().join("tm-workflow").join("references");
    fs::create_dir_all(&refs).unwrap();
    fs::write(refs.join("a.md"), "a").unwrap();

    let found = unmanaged_bundled_skills(dest.path(), &roster(&["tm-workflow"]));
    assert_eq!(
        found[0].manifest_keys(),
        vec![
            "tm-workflow".to_string(),
            "tm-workflow/references/a.md".to_string()
        ]
    );
}
