//! Tests for the project-tier bundled-skill probe (#6586).
//!
//! Why: the probe's whole job is to make a duplication visible that the deploy
//! fix can no longer create but also cannot clean up, so every test drives real
//! directories rather than a stubbed scan — a mocked lister would test the mock.
//! What: the warn path, the two clean paths that must NOT fire, and the two
//! undeterminable paths that must never render as healthy.
//! Test: this file IS the test module.

use super::*;

/// Build a `FrameworkPaths` rooted in `base` with a bundled roster holding
/// `stems`, and return the project directory to probe.
fn fixture(base: &Path, stems: &[&str]) -> (FrameworkPaths, std::path::PathBuf) {
    let paths = FrameworkPaths::under(base);
    let source = paths.skill_source_dir();
    std::fs::create_dir_all(&source).expect("fixture: bundled source dir");
    for stem in stems {
        std::fs::write(source.join(format!("{stem}.md")), "# bundled\n")
            .expect("fixture: write bundled skill");
    }
    let project = base.join("project");
    std::fs::create_dir_all(&project).expect("fixture: project dir");
    (paths, project)
}

/// Put a directory-shaped skill named `stem` in the project's own tier.
fn project_skill(project: &Path, stem: &str) {
    let dir = project.join(".claude").join("skills").join(stem);
    std::fs::create_dir_all(&dir).expect("fixture: project skill dir");
    std::fs::write(dir.join("SKILL.md"), "# project copy\n").expect("fixture: write SKILL.md");
}

/// The issue's second acceptance: a stray project-tier copy of a bundled skill
/// is flagged, by name, with the remediation (#6586).
///
/// Fails before #6586: the check does not exist, so `tm doctor` reports nothing
/// about the 21 byte-identical `tm-*` duplicates the issue counted.
#[test]
fn project_tier_bundled_copy_warns() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-ticketing", "tm-workflow"]);
    project_skill(&project, "tm-ticketing");

    let check = check_skill_project_tier(&paths, Some(&project));
    assert_eq!(check.status, CheckStatus::Warn, "{check:?}");
    assert!(
        check.message.contains("tm-ticketing"),
        "the duplicate must be named: {}",
        check.message
    );
    assert!(
        check.message.contains("tm doctor --fix-skills"),
        "and the remediation given: {}",
        check.message
    );
}

/// The probe must not delete anything it reports (#6586).
///
/// A read-only diagnostic cannot tell a leftover tm deployment from a
/// project-custom skill the operator wrote under a bundled name, and the second
/// is real work.
#[test]
fn the_probe_removes_nothing_it_reports() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-ticketing"]);
    project_skill(&project, "tm-ticketing");
    let file = project
        .join(".claude")
        .join("skills")
        .join("tm-ticketing")
        .join("SKILL.md");

    let check = check_skill_project_tier(&paths, Some(&project));
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(
        file.exists(),
        "the probe must leave the operator's file alone"
    );
}

/// A project tier holding only project-custom skills is clean — the tier is not
/// retired, only bundled names in it are (#6586).
#[test]
fn project_custom_only_tier_is_ok() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-ticketing"]);
    project_skill(&project, "our-house-style");

    let check = check_skill_project_tier(&paths, Some(&project));
    assert_eq!(check.status, CheckStatus::Ok, "{check:?}");
}

#[test]
fn project_tier_without_bundled_names_is_ok() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-ticketing"]);

    let check = check_skill_project_tier(&paths, Some(&project));
    assert_eq!(
        check.status,
        CheckStatus::Ok,
        "an absent project tier holds no duplicate: {check:?}"
    );
}

/// Neither unverifiable state may render as healthy — the #4605 rule, kept.
#[test]
fn unverifiable_states_are_unknown_not_ok() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-ticketing"]);
    assert_eq!(
        check_skill_project_tier(&paths, None).status,
        CheckStatus::Unknown,
        "no project in scope classifies nothing"
    );

    let bare = tempfile::tempdir().expect("tempdir");
    let empty_roster = FrameworkPaths::under(bare.path());
    assert_eq!(
        check_skill_project_tier(&empty_roster, Some(&project)).status,
        CheckStatus::Unknown,
        "an empty bundled roster classifies nothing"
    );
}
