//! Unit tests for [`super::ensure_project_skill_tier`] — the #4880
//! project-manifest trigger.
//!
//! Why a sibling file: `project_skill_tier.rs` is a PRODUCTION file under this
//! repo's 500-SLOC cap; a `*_tests.rs` sibling is classified as a test file
//! (3000-SLOC cap) by `scripts/check_line_cap.sh`, matching
//! `managed_config_tests.rs` and `agent_source_tests.rs`.
//!
//! What: the manifest-change trigger, the unchanged-manifest no-op, and the two
//! negatives the owner's standing rule demands — a project-custom skill and a
//! checksum-frozen hand edit are both left alone whether or not the manifest
//! moved.
//!
//! A NOTE ON THE FIXTURE. [`super::project_tier_stamp`] hashes the resolved
//! manifest plus `skill_source::skill_bundle_stamp()`, and that second component
//! fingerprints the COMPILED-IN bundle — not the seeded `fw.skills` directory
//! these tests write. So editing a seeded source file does not move the stamp
//! here, which is precisely what makes the trigger assertions unambiguous: every
//! redeploy below happens because the MANIFEST changed. In production the source
//! directory is materialized from that same compiled bundle
//! (`ensure_skill_source_fresh`), so its content cannot move without the bundle
//! stamp moving with it.

use super::*;
use crate::core::skill_tiers::{SkillTier, list_project_custom_stems};
use tempfile::TempDir;

/// A framework layout whose bundled skill source holds one seeded skill.
///
/// Why: `FrameworkPaths::under` keeps every path inside the temp dir, so these
/// tests never touch the real `~/.trusty-mpm` or `~/.claude`.
/// What: creates `fw.skills` and writes `<stem>.md` with `body`.
fn seed_framework(base: &Path, stem: &str, body: &str) -> FrameworkPaths {
    let fw = FrameworkPaths::under(base);
    std::fs::create_dir_all(&fw.skills).unwrap();
    std::fs::write(fw.skills.join(format!("{stem}.md")), body).unwrap();
    fw
}

/// Overwrite the bundled source copy of a seeded skill.
fn reseed(fw: &FrameworkPaths, stem: &str, body: &str) {
    std::fs::write(fw.skills.join(format!("{stem}.md")), body).unwrap();
}

/// Write a project-override manifest layer for `project_dir`.
///
/// Why: this is the operator-editable layer `ManifestSources::resolve` reads at
/// the highest precedence (#4832 moved it into `framework/`). Writing it is the
/// literal "the project manifest was updated" event this module triggers on.
/// What: writes `<harness-root>/.trusty-mpm/framework/manifest.toml`.
fn write_project_manifest(project_dir: &Path, body: &str) {
    let dir = crate::core::harness_root::framework_dir(project_dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("manifest.toml"), body).unwrap();
}

/// The resolved manifest for a project, as [`ensure_project_skill_tier`] sees it.
fn resolved(fw: &FrameworkPaths, project_dir: &Path) -> HarnessManifest {
    let catalog_root = crate::content::catalog_root_for(&fw.root);
    resolve_manifest(&ManifestSources::resolve(project_dir, &catalog_root))
}

#[test]
fn stamp_is_stable_across_calls() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path(), "probe-skill", "V1");
    let project = tmp.path().join("workspace");
    std::fs::create_dir_all(&project).unwrap();

    let manifest = resolved(&fw, &project);
    assert_eq!(
        project_tier_stamp(&manifest).unwrap(),
        project_tier_stamp(&manifest).unwrap(),
        "the stamp is a pure function of its inputs"
    );
}

#[test]
fn stamp_changes_with_the_manifest() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path(), "probe-skill", "V1");
    let project = tmp.path().join("workspace");
    std::fs::create_dir_all(&project).unwrap();

    let before = project_tier_stamp(&resolved(&fw, &project)).unwrap();
    write_project_manifest(&project, "[style]\nactive = \"probe-style\"\n");
    let after = project_tier_stamp(&resolved(&fw, &project)).unwrap();

    assert_ne!(
        before, after,
        "an edited project-override layer must move the stamp"
    );
}

/// Test 1: a manifest change refreshes a stale MANAGED project-tier skill.
///
/// Why: this is the whole defect. `<project>/.claude/skills` outranks the user
/// tier `managed_config` refreshes every run, so a project copy left behind by
/// an older binary silently shadowed the current one — the #4408 shape.
#[test]
fn manifest_change_refreshes_a_stale_skill() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path(), "probe-skill", "V1");
    let project = tmp.path().join("workspace");
    std::fs::create_dir_all(&project).unwrap();
    let deployed = project_skill_dir(&project)
        .join("probe-skill")
        .join("SKILL.md");

    let first = ensure_project_skill_tier(&fw, &project).unwrap();
    assert!(first.deployed, "the first run has no stamp, so it deploys");
    assert_eq!(std::fs::read_to_string(&deployed).unwrap(), "V1");

    // The binary now ships newer text AND the project manifest moved.
    reseed(&fw, "probe-skill", "V2-REFRESHED");
    write_project_manifest(&project, "[style]\nactive = \"probe-style\"\n");

    let second = ensure_project_skill_tier(&fw, &project).unwrap();

    assert!(second.deployed, "an updated project manifest must deploy");
    assert_eq!(
        std::fs::read_to_string(&deployed).unwrap(),
        "V2-REFRESHED",
        "a managed, user-unmodified project-tier skill must be refreshed"
    );
}

/// Test 2: an unchanged manifest performs no rewrite.
///
/// Why mtime and not just `deployed == false`: "cheap on relaunch" is the
/// property the owner's ruling asks for, and an unconditional redeploy would
/// still leave the content correct while churning every file in the workspace on
/// every resume. This mirrors
/// `skill_source::ensure_skill_source_fresh_is_noop_when_current`.
#[test]
fn unchanged_manifest_is_a_noop() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path(), "probe-skill", "V1");
    let project = tmp.path().join("workspace");
    std::fs::create_dir_all(&project).unwrap();
    let deployed = project_skill_dir(&project)
        .join("probe-skill")
        .join("SKILL.md");

    ensure_project_skill_tier(&fw, &project).unwrap();
    let before = std::fs::metadata(&deployed).unwrap().modified().unwrap();

    let second = ensure_project_skill_tier(&fw, &project).unwrap();

    assert!(
        !second.deployed,
        "a matching stamp must take the no-op path, not a redeploy"
    );
    assert_eq!(second.stats, DeployStats::default());
    let after = std::fs::metadata(&deployed).unwrap().modified().unwrap();
    assert_eq!(before, after, "a no-op must not rewrite a single file");
}

/// Test 3 (negative): a project-custom skill is never overwritten — manifest
/// change or not.
///
/// Why: the owner's standing rule. Two independent mechanisms enforce it and
/// this asserts the stronger one. `tiers::list_project_custom_stems` sees an
/// unmanaged directory (absent from `.trusty-mpm-skills-manifest.json`), so
/// `plan_skill_tiers` records a Project-over-Bundled shadow and drops the stem
/// from `bundled_deploy` — it never reaches file I/O at all, which is why it
/// shows up in `shadowed` and in NO `stats` vector.
/// `deployer::deploy_one_file`'s unmanaged-target skip is the second line of
/// defense behind it. Deploying more often must weaken neither, so the
/// assertions run on BOTH the first deploy and one triggered by a manifest
/// change.
#[test]
fn project_custom_skill_is_never_overwritten() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path(), "probe-skill", "BUNDLED");
    let project = tmp.path().join("workspace");
    std::fs::create_dir_all(&project).unwrap();

    // The operator's own file, sharing a stem with a bundled skill so the
    // ownership rule is what is under test rather than mere absence.
    let custom = project_skill_dir(&project)
        .join("probe-skill")
        .join("SKILL.md");
    std::fs::create_dir_all(custom.parent().unwrap()).unwrap();
    let mine = "MY OWN SKILL — never overwrite this\n";
    std::fs::write(&custom, mine).unwrap();

    let first = ensure_project_skill_tier(&fw, &project).unwrap();
    assert_eq!(std::fs::read_to_string(&custom).unwrap(), mine);
    assert!(
        first.shadowed.iter().any(|s| s.stem == "probe-skill"
            && s.winner == SkillTier::Project
            && s.loser == SkillTier::Bundled),
        "the hand-placed copy must win precedence outright: {:?}",
        first.shadowed
    );
    assert!(
        !first.stats.deployed.iter().any(|s| s == "probe-skill"),
        "a shadowed stem must never be written: {:?}",
        first.stats
    );

    // …and again once the project manifest moves.
    reseed(&fw, "probe-skill", "BUNDLED-V2");
    write_project_manifest(&project, "[style]\nactive = \"probe-style\"\n");
    let second = ensure_project_skill_tier(&fw, &project).unwrap();

    assert!(second.deployed, "the manifest moved, so a deploy ran");
    assert_eq!(
        std::fs::read_to_string(&custom).unwrap(),
        mine,
        "\"deploy on manifest update\" must never become \"overwrite user edits \
         on manifest update\""
    );
    assert!(
        !second.stats.deployed.iter().any(|s| s == "probe-skill"),
        "…and still not written on the manifest-triggered run: {:?}",
        second.stats
    );
}

/// Test 4 (negative): a checksum-frozen skill is still skipped.
///
/// Why: a skill tm DID deploy (so it is in the manifest) but the operator has
/// since hand-edited drifts from its recorded checksum. `deploy_one_file`
/// preserves it, and reconciling those is a separate, owner-gated action
/// (`tm doctor --fix-skills --include-frozen`) explicitly out of this issue's
/// scope.
#[test]
fn frozen_skill_is_still_skipped() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path(), "probe-skill", "V1");
    let project = tmp.path().join("workspace");
    std::fs::create_dir_all(&project).unwrap();
    let deployed = project_skill_dir(&project)
        .join("probe-skill")
        .join("SKILL.md");

    // First run makes it MANAGED, then the operator edits it in place.
    ensure_project_skill_tier(&fw, &project).unwrap();
    let hand_edit = "V1 plus my own notes\n";
    std::fs::write(&deployed, hand_edit).unwrap();

    reseed(&fw, "probe-skill", "V2");
    write_project_manifest(&project, "[style]\nactive = \"probe-style\"\n");
    let report = ensure_project_skill_tier(&fw, &project).unwrap();

    assert!(report.deployed, "the manifest moved, so a deploy ran");
    assert_eq!(
        std::fs::read_to_string(&deployed).unwrap(),
        hand_edit,
        "a checksum-frozen hand edit must survive a manifest-triggered deploy"
    );
    assert!(
        report.stats.skipped.iter().any(|s| s == "probe-skill"),
        "the frozen skill must be reported as skipped: {:?}",
        report.stats
    );
}

#[test]
fn missing_skill_source_does_not_stamp() {
    let tmp = TempDir::new().unwrap();
    let mut fw = FrameworkPaths::under(tmp.path());
    fw.trusty_mpm_root = None;
    let project = tmp.path().join("workspace");
    std::fs::create_dir_all(&project).unwrap();

    let report = ensure_project_skill_tier(&fw, &project).unwrap();

    assert!(!report.deployed);
    assert!(
        !project_skill_dir(&project)
            .join(PROJECT_TIER_STAMP_FILE)
            .exists(),
        "an empty deploy must not be recorded as current — the next run retries"
    );
}

#[test]
fn deploys_into_the_workspace_not_the_framework_home() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path(), "probe-skill", "V1");
    let project = tmp.path().join("workspace");
    std::fs::create_dir_all(&project).unwrap();

    ensure_project_skill_tier(&fw, &project).unwrap();

    assert!(
        project_skill_dir(&project)
            .join("probe-skill")
            .join("SKILL.md")
            .is_file()
    );
    assert!(
        !fw.claude_skills_dir().join("probe-skill").exists(),
        "a home-scoped `fw` must not redirect this deploy into ~/.claude/skills"
    );
}

#[test]
fn stamp_file_is_ignored_by_the_tier_planner() {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path(), "probe-skill", "V1");
    let project = tmp.path().join("workspace");
    std::fs::create_dir_all(&project).unwrap();

    ensure_project_skill_tier(&fw, &project).unwrap();
    let dest = project_skill_dir(&project);
    assert!(dest.join(PROJECT_TIER_STAMP_FILE).is_file());

    let custom = list_project_custom_stems(&dest).unwrap();
    assert!(
        !custom.iter().any(|s| s.starts_with('.')),
        "the stamp marker must never be mistaken for a project-custom skill: {custom:?}"
    );
}
