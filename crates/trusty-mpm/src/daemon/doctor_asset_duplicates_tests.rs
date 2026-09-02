//! Tests for the `asset_duplicates` doctor probe (#6649).
//!
//! Why: the probe exists to fire on the one collision no tier-vs-tier check can
//! see, and to stay silent on a project's own assets. Both halves are pinned
//! here; delete the check and `a_file_beside_a_directory_warns` fails.
//! What: end-to-end `check_asset_duplicates` cases against temp trees, plus the
//! pure `verdict` branches.
//! Test: this file.

use super::*;
use tempfile::TempDir;

/// A hermetic `FrameworkPaths` rooted inside `base`.
fn hermetic_paths(base: &Path) -> FrameworkPaths {
    let mut paths = FrameworkPaths::under(base);
    paths.trusty_mpm_root = None;
    paths
}

/// Write `<project>/.claude/agents/<name>`.
fn place_agent(project: &Path, name: &str) {
    let dir = project.join(".claude").join("agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), "---\nname: x\n---\n").unwrap();
}

/// Build a scan result for the pure `verdict` tests.
fn hit(label: &'static str, dir: &Path, stems: &[&str]) -> TierScan {
    TierScan {
        label,
        dir: dir.to_path_buf(),
        outcome: Ok(stems
            .iter()
            .map(|s| DuplicateStem {
                stem: (*s).to_string(),
                paths: vec![dir.join(s), dir.join(format!("{s}.md"))],
            })
            .collect()),
    }
}

#[test]
fn a_file_beside_a_directory_warns() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    place_agent(project.path(), "version-control.md");
    std::fs::create_dir(
        project
            .path()
            .join(".claude")
            .join("agents")
            .join("version-control"),
    )
    .unwrap();

    let check = check_asset_duplicates(&hermetic_paths(home.path()), Some(project.path()));
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(
        check.message.contains("version-control"),
        "the colliding name must be named: {}",
        check.message
    );
}

#[test]
fn a_clean_machine_is_ok() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    place_agent(project.path(), "version-control.md");
    place_agent(project.path(), "engineer.md");

    let check = check_asset_duplicates(&hermetic_paths(home.path()), Some(project.path()));
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
}

#[test]
fn a_project_custom_asset_does_not_fire() {
    // #6649 deliverable 4: an asset whose stem is in no roster and collides with
    // nothing is not a finding. This probe never consults a roster at all, which
    // is what makes that structural rather than a rule to maintain.
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    place_agent(project.path(), "acme-internal-reviewer.md");
    let skills = project.path().join(".claude").join("skills");
    std::fs::create_dir_all(skills.join("acme-house-style")).unwrap();
    std::fs::write(skills.join("acme-house-style").join("SKILL.md"), "x").unwrap();

    let check = check_asset_duplicates(&hermetic_paths(home.path()), Some(project.path()));
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
}

#[test]
fn the_probe_removes_nothing() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    place_agent(project.path(), "qa.md");
    let dir = project.path().join(".claude").join("agents").join("qa");
    std::fs::create_dir(&dir).unwrap();

    let check = check_asset_duplicates(&hermetic_paths(home.path()), Some(project.path()));
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(dir.is_dir(), "the directory must survive the probe");
    assert!(
        project
            .path()
            .join(".claude")
            .join("agents")
            .join("qa.md")
            .is_file(),
        "the file must survive the probe"
    );
    assert!(
        !check.message.contains("--fix"),
        "this probe names no repair command: {}",
        check.message
    );
}

#[test]
fn an_unscannable_tier_is_unknown_not_ok() {
    let dir = PathBuf::from("/tmp/locked-tier");
    let scans = vec![TierScan {
        label: "project agents",
        dir,
        outcome: Err("cannot scan asset tier /tmp/locked-tier: denied".to_string()),
    }];
    let check = verdict(&scans);
    assert_eq!(check.status, CheckStatus::Unknown, "{}", check.message);
    assert!(check.message.contains("UNDETERMINED"), "{}", check.message);
}

#[test]
fn a_real_duplicate_outranks_an_unscannable_tier() {
    let tmp = TempDir::new().unwrap();
    let scans = vec![
        hit("project agents", tmp.path(), &["qa"]),
        TierScan {
            label: "user skills",
            dir: PathBuf::from("/tmp/locked-tier"),
            outcome: Err("cannot scan asset tier /tmp/locked-tier: denied".to_string()),
        },
    ];
    let check = verdict(&scans);
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(check.message.contains("qa"), "{}", check.message);
    assert!(
        check.message.contains("UNDETERMINED"),
        "the tier it could not read is still named: {}",
        check.message
    );
}
