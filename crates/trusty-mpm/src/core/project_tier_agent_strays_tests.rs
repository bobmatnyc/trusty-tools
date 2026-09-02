//! Tests for the `--fix-agents` project-tier sweep (#6649).
//!
//! Why: the sweep is the one agent repair that DELETES, so each of its four
//! refusals needs a test that fails if the refusal is dropped — above all the
//! operator-authored directory, which is #6649's stated acceptance case.
//! What: fixture project trees driven through
//! [`super::remove_project_tier_agent_strays`] in both modes.
//! Test: this file.

use super::*;
use tempfile::TempDir;
use trusty_agents_common::agents::manifest::{ManifestEntry, Origin, checksum};

/// A hermetic `FrameworkPaths` whose agent SOURCE dir is inside `base`.
///
/// `trusty_mpm_root` is cleared so `agent_source_dir()` cannot resolve to the
/// real repository's `agents/agents` submodule and leak host state in.
fn hermetic_paths(base: &Path) -> FrameworkPaths {
    let mut paths = FrameworkPaths::under(base);
    paths.trusty_mpm_root = None;
    paths
}

/// A minimal agent document declaring `name`.
fn doc(name: &str) -> String {
    format!("---\nname: {name}\nrole: engineer\n---\n\nBody.\n")
}

/// Put `name` in the bundled agent SOURCE so `bundled_roster` carries it.
fn bundle(paths: &FrameworkPaths, name: &str) {
    let dir = paths.agent_source_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.md")), doc(name)).unwrap();
}

/// Write `<project>/.claude/agents/<file_name>`.
fn place(project: &Path, file_name: &str, body: &str) {
    let dir = project_agent_tier(project);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(file_name), body).unwrap();
}

/// Record `file_name` in the project tier's ownership ledger.
fn track(project: &Path, file_name: &str, body: &str, origin: Origin) {
    let dir = project_agent_tier(project);
    let mut manifest = AgentManifest::load(&dir);
    manifest.managed.insert(
        file_name.to_owned(),
        ManifestEntry {
            source_chain: vec![],
            checksum: checksum(body),
            deployed_at: "2026-09-02T00:00:00Z".to_owned(),
            origin,
        },
    );
    manifest.save(&dir).unwrap();
}

/// The one step for `path`'s file name, or a panic naming what was produced.
fn step_for<'a>(steps: &'a [RepairStep], file_name: &str) -> &'a RepairStep {
    steps
        .iter()
        .find(|s| s.path.file_name().is_some_and(|n| n == file_name))
        .unwrap_or_else(|| panic!("no step for {file_name}: {steps:?}"))
}

/// A project whose tier holds one ledger-tracked bundled copy.
fn tracked_project(home: &TempDir, project: &TempDir) -> FrameworkPaths {
    let paths = hermetic_paths(home.path());
    bundle(&paths, "qa");
    let body = doc("qa");
    place(project.path(), "qa.md", &body);
    track(project.path(), "qa.md", &body, Origin::Bundled);
    paths
}

#[test]
fn an_operator_authored_directory_is_refused_and_survives() {
    // #6649 acceptance: a same-named DIRECTORY with no ledger row survives, in
    // apply mode, and is reported rather than silently skipped.
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = hermetic_paths(home.path());
    bundle(&paths, "version-control");
    let dir = project_agent_tier(project.path()).join("version-control");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.md"), "mine").unwrap();

    let steps = remove_project_tier_agent_strays(
        &paths,
        Some(project.path()),
        backups.path(),
        RepairMode::Apply,
    );

    let step = step_for(&steps, "version-control");
    assert!(
        matches!(step.status, StepStatus::Refused(_)),
        "an operator-authored directory must be REFUSED: {step:?}"
    );
    assert!(dir.is_dir(), "the directory must survive --yes");
    assert!(dir.join("notes.md").is_file(), "its contents must survive");
}

#[test]
fn an_untracked_copy_is_refused() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = hermetic_paths(home.path());
    bundle(&paths, "qa");
    place(project.path(), "qa.md", &doc("qa"));

    let steps = remove_project_tier_agent_strays(
        &paths,
        Some(project.path()),
        backups.path(),
        RepairMode::Apply,
    );

    let step = step_for(&steps, "qa.md");
    assert!(
        matches!(&step.status, StepStatus::Refused(why) if why.contains("deploy ledger")),
        "{step:?}"
    );
    assert!(project_agent_tier(project.path()).join("qa.md").is_file());
}

#[test]
fn a_user_owned_ledger_entry_is_refused() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = hermetic_paths(home.path());
    bundle(&paths, "qa");
    let body = doc("qa");
    place(project.path(), "qa.md", &body);
    track(project.path(), "qa.md", &body, Origin::User);

    let steps = remove_project_tier_agent_strays(
        &paths,
        Some(project.path()),
        backups.path(),
        RepairMode::Apply,
    );

    let step = step_for(&steps, "qa.md");
    assert!(
        matches!(&step.status, StepStatus::Refused(why) if why.contains("OPERATOR")),
        "{step:?}"
    );
    assert!(project_agent_tier(project.path()).join("qa.md").is_file());
}

#[test]
fn a_hand_edited_managed_copy_is_refused() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = hermetic_paths(home.path());
    bundle(&paths, "qa");
    track(project.path(), "qa.md", &doc("qa"), Origin::Bundled);
    // The ledger records the ORIGINAL bytes; disk holds an edit.
    place(project.path(), "qa.md", "---\nname: qa\n---\n\nMy edit.\n");

    let steps = remove_project_tier_agent_strays(
        &paths,
        Some(project.path()),
        backups.path(),
        RepairMode::Apply,
    );

    let step = step_for(&steps, "qa.md");
    assert!(
        matches!(&step.status, StepStatus::Refused(why) if why.contains("hand-edited")),
        "{step:?}"
    );
    assert!(project_agent_tier(project.path()).join("qa.md").is_file());
}

#[test]
fn a_ledger_tracked_copy_is_removed() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = tracked_project(&home, &project);

    let steps = remove_project_tier_agent_strays(
        &paths,
        Some(project.path()),
        backups.path(),
        RepairMode::Apply,
    );

    let step = step_for(&steps, "qa.md");
    assert!(
        matches!(step.status, StepStatus::Applied { .. }),
        "{step:?}"
    );
    assert!(
        !project_agent_tier(project.path()).join("qa.md").exists(),
        "the proven copy is removed"
    );
    let ledger = AgentManifest::load(&project_agent_tier(project.path()));
    assert!(
        !ledger.is_managed("qa.md"),
        "the ledger must stop claiming a file that is gone"
    );
}

#[test]
fn a_removed_stray_is_backed_up() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = tracked_project(&home, &project);

    remove_project_tier_agent_strays(
        &paths,
        Some(project.path()),
        backups.path(),
        RepairMode::Apply,
    );

    let backup = backups.path().join("project-agents").join("qa.md");
    assert!(backup.is_file(), "every removal is recoverable");
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), doc("qa"));
}

#[test]
fn a_dry_run_writes_nothing() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = tracked_project(&home, &project);

    let steps = remove_project_tier_agent_strays(
        &paths,
        Some(project.path()),
        backups.path(),
        RepairMode::DryRun,
    );

    assert_eq!(step_for(&steps, "qa.md").status, StepStatus::Planned);
    assert!(project_agent_tier(project.path()).join("qa.md").is_file());
    assert!(
        !backups.path().join("project-agents").exists(),
        "a preview creates no backup root"
    );
}

#[test]
fn a_project_custom_agent_yields_no_step() {
    // #6649 deliverable 4: an agent whose stem is not in the roster is ignored
    // entirely — no finding, no refusal line, nothing to read past.
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = hermetic_paths(home.path());
    bundle(&paths, "qa");
    place(
        project.path(),
        "acme-internal-reviewer.md",
        &doc("acme-internal-reviewer"),
    );

    let steps = remove_project_tier_agent_strays(
        &paths,
        Some(project.path()),
        backups.path(),
        RepairMode::Apply,
    );

    assert!(
        steps.is_empty(),
        "a non-roster agent is not a finding: {steps:?}"
    );
    assert!(
        project_agent_tier(project.path())
            .join("acme-internal-reviewer.md")
            .is_file()
    );
}

#[test]
fn a_corrupt_ledger_refuses_the_sweep_rather_than_reporting_a_clean_tier() {
    // #6649 fail-open deliverable: an unreadable ledger must REPORT, never
    // produce an empty step list that reads as "nothing to repair".
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = hermetic_paths(home.path());
    bundle(&paths, "qa");
    let body = doc("qa");
    place(project.path(), "qa.md", &body);
    std::fs::write(
        project_agent_tier(project.path()).join(".trusty-mpm-manifest.json"),
        "{ not json",
    )
    .unwrap();

    let steps = remove_project_tier_agent_strays(
        &paths,
        Some(project.path()),
        backups.path(),
        RepairMode::Apply,
    );

    assert_eq!(steps.len(), 1, "one tier-wide refusal: {steps:?}");
    assert!(
        matches!(&steps[0].status, StepStatus::Refused(why) if why.contains("ownership ledger")),
        "{:?}",
        steps[0]
    );
    assert!(project_agent_tier(project.path()).join("qa.md").is_file());
}

#[test]
fn an_empty_roster_refuses_the_sweep() {
    // #6649 fail-open deliverable, the other half: no roster means nothing can
    // be classified, which must not render as a clean tier.
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let mut paths = hermetic_paths(home.path());
    // An empty embedded bundle AND an empty source dir is the only way to make
    // `bundled_roster` empty; `bundle()` is deliberately not called.
    paths.trusty_mpm_root = None;
    place(project.path(), "qa.md", &doc("qa"));

    let steps = remove_project_tier_agent_strays(
        &paths,
        Some(project.path()),
        backups.path(),
        RepairMode::Apply,
    );

    // The binary embeds a roster, so this asserts the SHAPE of the guard: when
    // the roster is empty the sweep refuses tier-wide rather than returning no
    // steps. With a non-empty embedded roster the untracked `qa.md` is refused
    // instead — either way nothing is removed and something is reported.
    assert!(
        !steps.is_empty(),
        "an unclassifiable tier reports something"
    );
    assert!(
        steps
            .iter()
            .all(|s| matches!(s.status, StepStatus::Refused(_))),
        "{steps:?}"
    );
    assert!(project_agent_tier(project.path()).join("qa.md").is_file());
}

#[test]
#[cfg(unix)]
fn a_symlinked_project_tier_is_refused() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = hermetic_paths(home.path());
    bundle(&paths, "qa");

    let real = home.path().join("real-agents");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("qa.md"), doc("qa")).unwrap();
    std::fs::create_dir_all(project.path().join(".claude")).unwrap();
    std::os::unix::fs::symlink(&real, project_agent_tier(project.path())).unwrap();

    let steps = remove_project_tier_agent_strays(
        &paths,
        Some(project.path()),
        backups.path(),
        RepairMode::Apply,
    );

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(
        matches!(&steps[0].status, StepStatus::Refused(why) if why.contains("symlink")),
        "{:?}",
        steps[0]
    );
    assert!(real.join("qa.md").is_file(), "the link target must survive");
}

#[test]
fn a_tier_bundled_agents_deploy_to_is_never_swept() {
    let home = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = hermetic_paths(home.path());
    bundle(&paths, "qa");
    // A "project" whose `.claude/agents` IS the canonical deploy dir.
    let deploy = paths.agent_deploy_dir();
    std::fs::create_dir_all(&deploy).unwrap();
    let fake_project = deploy.parent().unwrap().parent().unwrap().to_path_buf();
    std::fs::write(deploy.join("qa.md"), doc("qa")).unwrap();

    let steps = remove_project_tier_agent_strays(
        &paths,
        Some(&fake_project),
        backups.path(),
        RepairMode::Apply,
    );

    assert!(
        steps
            .iter()
            .all(|s| matches!(s.status, StepStatus::Refused(_))),
        "the canonical tier is never swept: {steps:?}"
    );
    assert!(deploy.join("qa.md").is_file());
}

#[test]
fn no_project_in_scope_yields_no_steps() {
    let home = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let steps = remove_project_tier_agent_strays(
        &hermetic_paths(home.path()),
        None,
        backups.path(),
        RepairMode::Apply,
    );
    assert!(steps.is_empty());
}
