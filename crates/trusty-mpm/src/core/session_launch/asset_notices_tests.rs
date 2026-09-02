//! Tests for the launch asset-hygiene lines (#6649).
//!
//! Why: the whole point is that a clean project says NOTHING and an unclean one
//! says exactly one line per kind. Both halves need a test, or the feature
//! degrades into either noise or silence.
//! What: [`super::launch_asset_notices`] and its three private line builders
//! against fixture project trees.
//! Test: this file.

use super::*;
use std::path::PathBuf;
use tempfile::TempDir;
use trusty_agents_common::agents::quarantine_receipt::QuarantinedAgent;

/// A hermetic `FrameworkPaths` rooted inside `base`.
fn hermetic(base: &Path) -> FrameworkPaths {
    let mut fw = FrameworkPaths::under(base);
    fw.trusty_mpm_root = None;
    fw
}

/// Put `stem` in the bundled skill SOURCE so `list_source_stems` carries it.
fn bundle_skill(fw: &FrameworkPaths, stem: &str) {
    let dir = fw.skill_source_dir().join(stem);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
}

/// Deploy `stem` into the PROJECT skill tier, which #6586 says must hold none.
fn stray_skill(project: &Path, stem: &str) {
    let dir = project_skill_tier(project).join(stem);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
}

/// A report standing for a sweep that moved `names`.
fn moved(names: &[&str]) -> QuarantineReport {
    QuarantineReport {
        moved: names
            .iter()
            .map(|n| QuarantinedAgent {
                name: (*n).to_string(),
                from: PathBuf::from(format!("/p/.claude/agents/{n}.md")),
                to: PathBuf::from(format!("/p/.claude/agents/{n}.md.disabled")),
                backup: PathBuf::from(format!("/p/.trusty-mpm/agent-quarantine/{n}.md")),
            })
            .collect(),
        ..QuarantineReport::default()
    }
}

#[test]
fn a_clean_project_produces_no_notice() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = hermetic(home.path());
    bundle_skill(&fw, "tm-workflow");

    let notices = launch_asset_notices(&fw, project.path(), Some(&QuarantineReport::default()));
    assert!(
        notices.is_empty(),
        "a clean launch must add nothing to the terminal: {notices:?}"
    );
}

#[test]
fn a_quarantined_agent_produces_one_line() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = hermetic(home.path());
    bundle_skill(&fw, "tm-workflow");

    let notices = launch_asset_notices(&fw, project.path(), Some(&moved(&["rust-engineer"])));
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].starts_with("agents quarantined 1"),
        "{notices:?}"
    );
    assert!(notices[0].contains("rust-engineer"), "{notices:?}");
}

#[test]
fn findings_are_logged_by_severity() {
    // The helper is a logging sink, so what a test can prove is that it accepts
    // both slices and a formatted scope, and that a clean report is a no-op —
    // no panic, nothing to emit. The severity split (error for a roster gap,
    // warn for an asset notice) is one branch each and is read in the source.
    log_prep_findings(&[], &[], format_args!("clean"));
    log_prep_findings(
        &["agent deploy failed: x".to_string()],
        &["duplicates 1 (qa)".to_string()],
        format_args!("session {} at {}", 7, "/tmp/p"),
    );
}

#[test]
fn a_clean_sweep_produces_no_agent_line() {
    assert!(agent_notice(&QuarantineReport::default()).is_none());
}

#[test]
fn a_project_tier_skill_stray_produces_one_line() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = hermetic(home.path());
    bundle_skill(&fw, "tm-workflow");
    stray_skill(project.path(), "tm-workflow");

    let notices = launch_asset_notices(&fw, project.path(), Some(&QuarantineReport::default()));
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].starts_with("skills stray 1"), "{notices:?}");
    assert!(notices[0].contains("tm-workflow"), "{notices:?}");
    assert!(notices[0].contains("--fix-skills"), "{notices:?}");
}

#[test]
fn a_project_custom_skill_produces_no_line() {
    // #6649 deliverable 4: a project-local skill whose stem is in no roster is
    // not a finding.
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = hermetic(home.path());
    bundle_skill(&fw, "tm-workflow");
    stray_skill(project.path(), "acme-house-style");

    let notices = launch_asset_notices(&fw, project.path(), Some(&QuarantineReport::default()));
    assert!(notices.is_empty(), "{notices:?}");
}

#[test]
fn a_same_stem_duplicate_produces_one_line() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = hermetic(home.path());
    bundle_skill(&fw, "tm-workflow");

    let agents = project_agent_tier(project.path());
    std::fs::create_dir_all(agents.join("version-control")).unwrap();
    std::fs::write(agents.join("version-control.md"), "---\nname: vc\n---\n").unwrap();

    let notices = launch_asset_notices(&fw, project.path(), Some(&QuarantineReport::default()));
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].starts_with("duplicates 1"), "{notices:?}");
    assert!(notices[0].contains("version-control"), "{notices:?}");
}

#[test]
fn an_empty_roster_over_a_populated_tier_reports_undetermined() {
    // #6649 fail-open deliverable: a roster that could not be built must not
    // render as "no strays".
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = hermetic(home.path());
    // Deliberately no `bundle_skill` — the roster is empty.
    stray_skill(project.path(), "tm-workflow");

    let line = skill_notice(&fw, project.path()).expect("an unclassifiable tier reports");
    assert!(line.contains("UNDETERMINED"), "{line}");
}

#[test]
fn an_empty_roster_over_an_empty_tier_is_silent() {
    // The mirror: nothing to classify AND nothing there is genuinely clean.
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = hermetic(home.path());
    std::fs::create_dir_all(project_skill_tier(project.path())).unwrap();

    assert!(skill_notice(&fw, project.path()).is_none());
}

#[test]
#[cfg(unix)]
fn an_unreadable_agent_tier_reports_the_failure() {
    use std::os::unix::fs::PermissionsExt;

    let project = TempDir::new().unwrap();
    let agents = project_agent_tier(project.path());
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o000)).unwrap();
    let mode_took = std::fs::read_dir(&agents).is_err();
    let line = duplicate_notice(project.path());
    std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o700)).unwrap();

    if !mode_took {
        return;
    }
    let line = line.expect("an unreadable tier reports, never reads as clean");
    assert!(line.contains("UNDETERMINED"), "{line}");
}

#[test]
#[cfg(unix)]
fn an_unreadable_skill_tier_reports_the_failure() {
    use std::os::unix::fs::PermissionsExt;

    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = hermetic(home.path());
    bundle_skill(&fw, "tm-workflow");
    let skills = project_skill_tier(project.path());
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::set_permissions(&skills, std::fs::Permissions::from_mode(0o000)).unwrap();
    let mode_took = std::fs::read_dir(&skills).is_err();
    let line = skill_notice(&fw, project.path());
    std::fs::set_permissions(&skills, std::fs::Permissions::from_mode(0o700)).unwrap();

    if !mode_took {
        return;
    }
    let line = line.expect("an unreadable tier reports, never reads as clean");
    assert!(line.contains("UNDETERMINED"), "{line}");
}
