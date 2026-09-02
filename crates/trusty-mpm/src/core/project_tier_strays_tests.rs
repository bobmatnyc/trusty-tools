//! Tests for the project-tier stray sweep (#6586).
//!
//! Why: the sweep DELETES, which every other repair in this crate refuses to
//! do, so each test drives real directories and asserts against disk — the
//! evidence rule that licenses the deletion is only meaningful if the tests
//! read the same disk the operator would.
//! What: the removal and its ledger update, the two refusals that protect the
//! operator's own work, the managed-tier boundary, and the dry run.
//! Test: this file IS the test module.

use super::*;
use crate::core::skill_manifest::SkillManifestEntry;
use trusty_agents_common::agents::manifest::checksum;

/// Bundled roster under `base`, plus the project directory to sweep.
fn fixture(base: &Path, stems: &[&str]) -> (FrameworkPaths, PathBuf) {
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

/// `<project>/.claude/skills`.
fn tier(project: &Path) -> PathBuf {
    project.join(".claude").join("skills")
}

/// Deploy a directory-shaped skill into the project tier and return its body.
fn project_skill(project: &Path, stem: &str) -> String {
    let dir = tier(project).join(stem);
    std::fs::create_dir_all(dir.join("references")).expect("fixture: project skill dir");
    let body = format!("# {stem}\n");
    std::fs::write(dir.join("SKILL.md"), &body).expect("fixture: write SKILL.md");
    std::fs::write(dir.join("references").join("extra.md"), "# extra\n")
        .expect("fixture: write reference");
    body
}

/// Record `stem` in the tier's ledger with the checksum of `content`.
fn record(project: &Path, stem: &str, content: &str) {
    let dir = tier(project);
    let mut manifest = SkillManifest::load(&dir).expect("fixture: load ledger");
    manifest.managed.insert(
        stem.to_string(),
        SkillManifestEntry {
            checksum: checksum(content),
            deployed_at: "2026-09-01T00:00:00Z".to_string(),
        },
    );
    manifest.managed.insert(
        format!("{stem}/references/extra.md"),
        SkillManifestEntry {
            checksum: checksum("# extra\n"),
            deployed_at: "2026-09-01T00:00:00Z".to_string(),
        },
    );
    manifest.save(&dir).expect("fixture: save ledger");
}

/// The backup root a test run writes under.
fn backups(base: &Path) -> PathBuf {
    base.join("backup-doctor-remediation-test")
}

/// The acceptance (#6586): a manifest-managed stray is removed, the operator's
/// own skill stays, and the ledger no longer claims the removed stem.
#[test]
fn a_managed_stray_is_removed_and_a_custom_skill_is_kept() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-ticketing", "tm-workflow"]);
    let body = project_skill(&project, "tm-ticketing");
    project_skill(&project, "our-house-style");
    record(&project, "tm-ticketing", &body);

    let steps = remove_project_tier_strays(
        &paths,
        Some(&project),
        false,
        &backups(tmp.path()),
        RepairMode::Apply,
    );

    assert_eq!(
        steps.len(),
        1,
        "only the bundled name is a stray: {steps:?}"
    );
    assert!(
        matches!(steps[0].status, StepStatus::Applied { .. }),
        "{steps:?}"
    );
    assert!(
        !tier(&project).join("tm-ticketing").exists(),
        "the stray must be gone from the project tier"
    );
    assert!(
        tier(&project)
            .join("our-house-style")
            .join("SKILL.md")
            .is_file(),
        "the operator's own skill must survive the sweep"
    );

    let manifest = SkillManifest::load(&tier(&project)).expect("ledger");
    assert!(
        !manifest.is_managed("tm-ticketing"),
        "the removed stem must be dropped from the ledger: {:?}",
        manifest.managed.keys().collect::<Vec<_>>()
    );
    assert!(
        !manifest.is_managed("tm-ticketing/references/extra.md"),
        "and so must its reference files: {:?}",
        manifest.managed.keys().collect::<Vec<_>>()
    );
}

/// The whole skill subtree is recoverable, not just its entry point.
#[test]
fn a_removed_stray_is_backed_up_whole() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-ticketing"]);
    let body = project_skill(&project, "tm-ticketing");
    record(&project, "tm-ticketing", &body);
    let root = backups(tmp.path());

    let steps = remove_project_tier_strays(&paths, Some(&project), false, &root, RepairMode::Apply);

    let StepStatus::Applied { backup: Some(path) } = &steps[0].status else {
        panic!("expected an applied removal with a backup: {steps:?}");
    };
    assert_eq!(path, &root.join("project").join("tm-ticketing"));
    assert_eq!(
        std::fs::read_to_string(path.join("SKILL.md")).expect("backed-up entry point"),
        body
    );
    assert_eq!(
        std::fs::read_to_string(path.join("references").join("extra.md"))
            .expect("backed-up reference file"),
        "# extra\n"
    );
}

/// A bundled-named directory tm never recorded may be the operator's own work.
#[test]
fn an_unrecorded_bundled_name_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-ticketing"]);
    project_skill(&project, "tm-ticketing");

    let steps = remove_project_tier_strays(
        &paths,
        Some(&project),
        true,
        &backups(tmp.path()),
        RepairMode::Apply,
    );

    assert!(
        matches!(steps[0].status, StepStatus::Refused(_)),
        "an unrecorded copy is never removed, even with --include-frozen: {steps:?}"
    );
    assert!(
        tier(&project)
            .join("tm-ticketing")
            .join("SKILL.md")
            .is_file()
    );
}

/// A hand-edit is deliberate work under any name.
#[test]
fn a_hand_edited_stray_is_refused_without_include_frozen() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-ticketing"]);
    let body = project_skill(&project, "tm-ticketing");
    record(&project, "tm-ticketing", &body);
    let entry = tier(&project).join("tm-ticketing").join("SKILL.md");
    std::fs::write(&entry, "# hand-edited\n").expect("hand-edit the deployed copy");

    let steps = remove_project_tier_strays(
        &paths,
        Some(&project),
        false,
        &backups(tmp.path()),
        RepairMode::Apply,
    );
    assert!(
        matches!(steps[0].status, StepStatus::Refused(_)),
        "{steps:?}"
    );
    assert!(entry.is_file(), "the edit must survive");

    let steps = remove_project_tier_strays(
        &paths,
        Some(&project),
        true,
        &backups(tmp.path()),
        RepairMode::Apply,
    );
    assert!(
        matches!(steps[0].status, StepStatus::Applied { .. }),
        "--include-frozen is the opt-in that removes it: {steps:?}"
    );
    assert!(!entry.exists());
}

/// A dry run reports the same set it would remove, and writes nothing.
#[test]
fn a_dry_run_removes_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-ticketing"]);
    let body = project_skill(&project, "tm-ticketing");
    record(&project, "tm-ticketing", &body);
    let root = backups(tmp.path());

    let steps =
        remove_project_tier_strays(&paths, Some(&project), false, &root, RepairMode::DryRun);

    assert!(matches!(steps[0].status, StepStatus::Planned), "{steps:?}");
    assert!(
        tier(&project)
            .join("tm-ticketing")
            .join("SKILL.md")
            .is_file()
    );
    assert!(!root.exists(), "a dry run writes no backup either");
    assert!(
        SkillManifest::load(&tier(&project))
            .expect("ledger")
            .is_managed("tm-ticketing"),
        "and leaves the ledger alone"
    );
}

/// A tier bundled skills are DEPLOYED to is never swept — removing them there
/// would run the #6586 fix backwards. A managed-workspace `FrameworkPaths`
/// resolves `claude_skills_dir` onto the project's own `.claude/skills`, which
/// is exactly the collision the guard exists for.
#[test]
fn a_tier_bundled_skills_deploy_to_is_never_swept() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let paths = FrameworkPaths::for_managed_project(tmp.path().join(".trusty-mpm"), &project);
    let source = paths.skill_source_dir();
    std::fs::create_dir_all(&source).expect("bundled source dir");
    std::fs::write(source.join("tm-ticketing.md"), "# bundled\n").expect("write bundled skill");
    assert_eq!(
        paths.claude_skills_dir(),
        tier(&project),
        "fixture must collide the two tiers"
    );

    let dir = tier(&project).join("tm-ticketing");
    std::fs::create_dir_all(&dir).expect("deployed skill dir");
    std::fs::write(dir.join("SKILL.md"), "# bundled\n").expect("write deployed skill");

    let steps = remove_project_tier_strays(
        &paths,
        Some(&project),
        true,
        &backups(tmp.path()),
        RepairMode::Apply,
    );

    assert!(
        matches!(steps[0].status, StepStatus::Refused(_)),
        "{steps:?}"
    );
    assert!(
        dir.join("SKILL.md").is_file(),
        "the bundled roster must survive"
    );
}

/// No project in scope and no tier on disk are both "nothing to do", not a
/// finding.
#[test]
fn nothing_to_sweep_reports_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-ticketing"]);
    let root = backups(tmp.path());

    assert!(remove_project_tier_strays(&paths, None, false, &root, RepairMode::Apply).is_empty());
    assert!(
        remove_project_tier_strays(&paths, Some(&project), false, &root, RepairMode::Apply)
            .is_empty(),
        "an unprovisioned project tier holds no stray"
    );
}
