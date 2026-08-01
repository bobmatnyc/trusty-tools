//! Detection of externally-changed assistant homes (#4325).
//!
//! Why: #4325 makes external modification EXPECTED and requires DETECTION plus
//! a remedy the concierge can narrate. Every test here is one thing a user can
//! actually do to their own directory.
//! What: healthy baseline, then each condition — home deleted, entry deleted,
//! entry replaced by the wrong kind, config corrupted, instructions emptied —
//! and the narration seam that carries them.
//! Test: this module IS the test surface.

use super::home_tests::temp_home;
use crate::assistants::{HomeIssueKind, inspect};

#[test]
fn a_freshly_ensured_home_is_healthy() {
    let (_tmp, home) = temp_home();
    home.ensure().unwrap();
    let health = inspect(&home);
    assert!(health.is_healthy(), "issues: {:?}", health.issues);
    assert_eq!(health.narration(), None);
}

/// Why: "you have no home directory" is ONE fact. Reporting it five times (once
/// per missing entry) is the raw dump #4325 rules out.
#[test]
fn a_missing_home_is_one_finding_not_five() {
    let (_tmp, home) = temp_home();
    let health = inspect(&home);
    assert_eq!(health.issues.len(), 1, "issues: {:?}", health.issues);
    assert_eq!(health.issues[0].kind, HomeIssueKind::HomeMissing);
    assert!(!health.issues[0].remedy.is_empty());
}

#[test]
fn reports_a_deleted_layout_directory() {
    let (_tmp, home) = temp_home();
    home.ensure().unwrap();
    std::fs::remove_dir_all(home.okg_dir()).unwrap();

    let health = inspect(&home);
    assert_eq!(health.issues.len(), 1, "issues: {:?}", health.issues);
    let issue = &health.issues[0];
    assert_eq!(issue.kind, HomeIssueKind::Missing);
    assert_eq!(issue.entry, "okg");
    assert_eq!(issue.path, home.okg_dir());
}

#[test]
fn reports_a_deleted_instructions_file() {
    let (_tmp, home) = temp_home();
    home.ensure().unwrap();
    std::fs::remove_file(home.instructions_path()).unwrap();

    let health = inspect(&home);
    assert_eq!(health.issues.len(), 1, "issues: {:?}", health.issues);
    assert_eq!(health.issues[0].kind, HomeIssueKind::Missing);
    assert_eq!(health.issues[0].entry, "instructions.md");
}

/// Why: a user who replaces `okg/` with a note file has not "deleted" it — the
/// remedy differs, so the finding must too.
#[test]
fn reports_an_entry_of_the_wrong_kind() {
    let (_tmp, home) = temp_home();
    home.ensure().unwrap();
    std::fs::remove_dir_all(home.agents_dir()).unwrap();
    std::fs::write(home.agents_dir(), "not a directory").unwrap();

    let health = inspect(&home);
    let issue = health
        .issues
        .iter()
        .find(|i| i.entry == "agents")
        .expect("agents finding");
    assert_eq!(issue.kind, HomeIssueKind::NotADirectory);
    assert!(issue.detail.contains("not a directory"), "was: {issue}");
}

#[test]
fn reports_malformed_config() {
    let (_tmp, home) = temp_home();
    home.ensure().unwrap();
    std::fs::write(home.config_path(), "id = \"izzie\"\nthis is not toml [[[\n").unwrap();

    let health = inspect(&home);
    let issue = health
        .issues
        .iter()
        .find(|i| i.entry == "config.toml")
        .expect("config finding");
    assert_eq!(issue.kind, HomeIssueKind::Malformed);
    assert!(issue.detail.contains("not valid TOML"), "was: {issue}");
    assert!(!issue.remedy.is_empty());
    // One line, not a multi-line parser dump.
    assert!(!issue.detail.contains('\n'), "was: {issue}");
}

/// Why: a hand-added key is a user personalising their own file, not a defect.
/// Calling it malformed would train users to distrust the detector.
#[test]
fn tolerates_unknown_config_keys() {
    let (_tmp, home) = temp_home();
    home.ensure().unwrap();
    std::fs::write(
        home.config_path(),
        "id = \"izzie\"\nfavourite_colour = \"green\"\n",
    )
    .unwrap();
    assert!(inspect(&home).is_healthy());
}

#[test]
fn reports_emptied_instructions() {
    let (_tmp, home) = temp_home();
    home.ensure().unwrap();
    std::fs::write(home.instructions_path(), "   \n").unwrap();

    let health = inspect(&home);
    let issue = health
        .issues
        .iter()
        .find(|i| i.entry == "instructions.md")
        .expect("instructions finding");
    assert_eq!(issue.kind, HomeIssueKind::Malformed);
    assert!(issue.detail.contains("empty"), "was: {issue}");
}

/// Why: the narration seam is what #4320's conversational surface consumes —
/// every finding must reach it with its remedy attached.
#[test]
fn narration_names_every_issue() {
    let (_tmp, home) = temp_home();
    home.ensure().unwrap();
    std::fs::remove_dir_all(home.okg_dir()).unwrap();
    std::fs::remove_file(home.config_path()).unwrap();

    let health = inspect(&home);
    let narration = health.narration().expect("unhealthy home narrates");
    assert!(narration.contains(&home.path().display().to_string()));
    for issue in &health.issues {
        assert!(
            narration.contains(&issue.remedy),
            "narration dropped a remedy: {narration}"
        );
    }
    assert_eq!(HomeIssueKind::Missing.describe(), "missing");
    assert_eq!(HomeIssueKind::Unreadable.describe(), "unreadable");
    assert_eq!(HomeIssueKind::NotAFile.describe(), "not a file");
    assert_eq!(
        HomeIssueKind::HomeMissing.describe(),
        "the home directory is missing"
    );
    assert_eq!(HomeIssueKind::Malformed.describe(), "malformed");
    assert_eq!(HomeIssueKind::NotADirectory.describe(), "not a directory");
}
