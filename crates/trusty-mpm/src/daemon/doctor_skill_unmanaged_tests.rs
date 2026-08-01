//! Tests for the `skill_unmanaged` doctor probe (#4605).
//!
//! Why: the probe exists because every other skill check reports `Ok` on the
//! exact state it detects. Its value is entirely in FIRING — delete the
//! reporting and `unmanaged_bundled_skill_reports_unknown` fails — and in
//! staying silent on a skill the operator authored, which the reconcile it
//! recommends must never touch.
//! What: hermetic `check_skill_unmanaged` cases against a temp framework root
//! with staged skill sources and deploy tiers.
//! Test: this file.

use super::*;
use crate::core::skill_deployer::deploy_skills;

/// A hermetic `FrameworkPaths` whose skill SOURCE dir is inside `base`.
///
/// `trusty_mpm_root` is cleared so `skill_source_dir()` cannot resolve to the
/// real repository's `agents/skills` submodule and leak host state into the
/// assertions (the same guard `doctor_asset_tier`'s tests use).
fn hermetic_paths(base: &Path) -> FrameworkPaths {
    let mut paths = FrameworkPaths::under(base);
    paths.trusty_mpm_root = None;
    paths
}

/// Write `<paths.skill_source_dir()>/<stem>.md`, making `stem` bundled.
fn bundle(paths: &FrameworkPaths, stem: &str, body: &str) {
    let dir = paths.skill_source_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{stem}.md")), body).unwrap();
}

/// Stage `<dir>/<stem>/SKILL.md` without a manifest entry — the #4605 state.
fn stage_untracked(dir: &std::path::Path, stem: &str, body: &str) {
    let skill_dir = dir.join(stem);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), body).unwrap();
}

#[test]
fn unmanaged_bundled_skill_reports_unknown() {
    // The measured #4605 state: a bundled skill deployed to the tm-global
    // roster with no manifest entry. It must never read as healthy.
    let base = tempfile::TempDir::new().unwrap();
    let paths = hermetic_paths(base.path());
    bundle(&paths, "tm-workflow", "current text");
    let tier = paths.agent_deploy_dir().parent().unwrap().join("skills");
    std::fs::create_dir_all(&tier).unwrap();
    stage_untracked(&tier, "tm-workflow", "stale text");

    let check = check_skill_unmanaged(&paths, None);
    assert_eq!(check.status, CheckStatus::Unknown, "{check:?}");
    assert!(check.message.contains("tm-workflow"), "{check:?}");
    assert!(check.message.contains("--reconcile-skills"), "{check:?}");
}

#[test]
fn managed_tier_is_ok() {
    // A skill the deployer wrote is tracked and reachable.
    let base = tempfile::TempDir::new().unwrap();
    let paths = hermetic_paths(base.path());
    bundle(&paths, "tm-workflow", "current text");
    let tier = paths.agent_deploy_dir().parent().unwrap().join("skills");
    deploy_skills(&paths.skill_source_dir(), &tier).unwrap();

    let check = check_skill_unmanaged(&paths, None);
    assert_eq!(check.status, CheckStatus::Ok, "{check:?}");
}

#[test]
fn operator_skill_does_not_fire() {
    // A deployed skill whose stem names nothing bundled is the operator's own.
    // Flagging it would push them toward a reconcile that must not touch it.
    let base = tempfile::TempDir::new().unwrap();
    let paths = hermetic_paths(base.path());
    bundle(&paths, "tm-workflow", "current text");
    let tier = paths.agent_deploy_dir().parent().unwrap().join("skills");
    std::fs::create_dir_all(&tier).unwrap();
    stage_untracked(&tier, "my-own-skill", "mine");

    let check = check_skill_unmanaged(&paths, None);
    assert_eq!(check.status, CheckStatus::Ok, "{check:?}");
}

#[test]
fn project_tier_is_scanned() {
    // Coverage must reach the per-project `.claude/skills` tier too — a
    // managed session reads it and it drifts independently.
    let base = tempfile::TempDir::new().unwrap();
    let project = tempfile::TempDir::new().unwrap();
    let paths = hermetic_paths(base.path());
    bundle(&paths, "tm-workflow", "current text");
    let tier = project.path().join(".claude").join("skills");
    std::fs::create_dir_all(&tier).unwrap();
    stage_untracked(&tier, "tm-workflow", "stale text");

    let check = check_skill_unmanaged(&paths, Some(project.path()));
    assert_eq!(check.status, CheckStatus::Unknown, "{check:?}");
    assert!(check.message.contains("project/tm-workflow"), "{check:?}");
}

#[test]
fn empty_roster_is_not_a_clean_bill() {
    // With no bundled source the probe cannot classify anything. Reporting
    // `Ok` would be the same "unverifiable rendered as clean" defect class.
    let base = tempfile::TempDir::new().unwrap();
    let paths = hermetic_paths(base.path());

    let check = check_skill_unmanaged(&paths, None);
    assert_eq!(check.status, CheckStatus::Unknown, "{check:?}");
    assert!(
        check.message.contains("no bundled skill source"),
        "{check:?}"
    );
}
