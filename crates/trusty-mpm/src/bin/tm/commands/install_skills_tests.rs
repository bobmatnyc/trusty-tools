//! Tests for `tm install`'s unmanaged-skill reporting (#4605).
//!
//! Why: the reporter is the floor fix — before it, `tm install` mentioned an
//! unreachable bundled skill as neither deployed, skipped, NOR unchanged, and
//! an operator had no signal at all. Delete the reporting and
//! `report_lines_name_the_tier_and_the_skill` fails.
//! What: hermetic `unmanaged_report_lines` cases against a temp framework root.
//! Test: this file.

use super::*;
use trusty_mpm::core::skill_deployer::deploy_skills;

/// A hermetic `FrameworkPaths` whose skill SOURCE dir is inside `base`.
///
/// `trusty_mpm_root` is cleared so `skill_source_dir()` cannot resolve to the
/// real repository's `agents/skills` submodule and leak host state in.
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
fn stage_untracked(dir: &Path, stem: &str, body: &str) {
    let skill_dir = dir.join(stem);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), body).unwrap();
}

#[test]
fn report_lines_name_the_tier_and_the_skill() {
    let base = tempfile::TempDir::new().unwrap();
    let paths = hermetic_paths(base.path());
    bundle(&paths, "tm-workflow", "current text");
    let tier = paths.claude_skills_dir();
    std::fs::create_dir_all(&tier).unwrap();
    stage_untracked(&tier, "tm-workflow", "stale text");

    let lines = unmanaged_report_lines(&paths, None);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("! tm-workflow (untracked at "),
        "{lines:?}"
    );
    assert!(lines[0].contains(&tier.display().to_string()), "{lines:?}");
    assert!(lines[0].contains("--reconcile-skills"), "{lines:?}");
}

#[test]
fn report_lines_are_empty_when_every_skill_is_tracked() {
    let base = tempfile::TempDir::new().unwrap();
    let paths = hermetic_paths(base.path());
    bundle(&paths, "tm-workflow", "current text");
    deploy_skills(&paths.skill_source_dir(), &paths.claude_skills_dir()).unwrap();

    assert!(unmanaged_report_lines(&paths, None).is_empty());
}

#[test]
fn report_lines_ignore_an_operator_skill() {
    // A stem matching nothing bundled is the operator's own — never reported,
    // so nothing ever suggests reconciling it.
    let base = tempfile::TempDir::new().unwrap();
    let paths = hermetic_paths(base.path());
    bundle(&paths, "tm-workflow", "current text");
    let tier = paths.claude_skills_dir();
    std::fs::create_dir_all(&tier).unwrap();
    stage_untracked(&tier, "my-own-skill", "mine");

    assert!(unmanaged_report_lines(&paths, None).is_empty());
}

#[test]
fn backup_root_is_timestamped_under_the_framework_root() {
    let base = tempfile::TempDir::new().unwrap();
    let paths = hermetic_paths(base.path());
    let root = backup_root(&paths);

    assert_eq!(root.parent().unwrap(), paths.root);
    let name = root.file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with(RECONCILE_BACKUP_PREFIX), "{name}");
    assert_eq!(name.len(), RECONCILE_BACKUP_PREFIX.len() + 14, "{name}");
}
