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

/// Record `stems` as tm-managed in the project tier's own deploy ledger.
fn mark_managed(project: &Path, stems: &[&str]) {
    use trusty_agents_common::agents::manifest::checksum;
    use trusty_agents_common::skills::manifest::{SkillManifest, SkillManifestEntry};

    let dir = project.join(".claude").join("skills");
    let mut manifest = SkillManifest::load(&dir).expect("fixture: load ledger");
    for stem in stems {
        let content = std::fs::read_to_string(dir.join(stem).join("SKILL.md"))
            .expect("fixture: read the deployed copy");
        manifest.managed.insert(
            (*stem).to_string(),
            SkillManifestEntry {
                checksum: checksum(&content),
                deployed_at: "2026-09-01T00:00:00Z".to_string(),
            },
        );
    }
    manifest.save(&dir).expect("fixture: save ledger");
}

/// The live-verification failure (#6586): a stray the project's own ledger
/// marks MANAGED is exactly what the pre-#6602 deploy left behind, and it is
/// the case the check was blind to.
///
/// Fails before this fix: `check_skill_project_tier` intersected the bundled
/// roster with `list_project_custom_stems`, which by design drops every stem
/// the manifest marks managed — so 51 bundled copies in a real project reported
/// `✅ … holds no bundled skill`.
#[test]
fn project_tier_manifest_managed_copy_is_still_flagged() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-ticketing", "tm-workflow"]);
    project_skill(&project, "tm-ticketing");
    mark_managed(&project, &["tm-ticketing"]);

    let check = check_skill_project_tier(&paths, Some(&project));
    assert_eq!(
        check.status,
        CheckStatus::Warn,
        "a manifest-managed stray is still a stray: {check:?}"
    );
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

/// A project-custom skill under a name the bundled roster does NOT carry is not
/// a stray and must never be flagged (#6586).
#[test]
fn a_user_custom_skill_is_not_flagged() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-ticketing"]);
    project_skill(&project, "our-house-style");
    mark_managed(&project, &[]);

    let check = check_skill_project_tier(&paths, Some(&project));
    assert_eq!(
        check.status,
        CheckStatus::Ok,
        "a non-bundled name is the operator's own work: {check:?}"
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
