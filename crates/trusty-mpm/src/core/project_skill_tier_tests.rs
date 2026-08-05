//! Unit tests for [`super::ensure_project_skill_tier`] — the #4880 redeploy
//! trigger.
//!
//! Why a sibling file: `project_skill_tier.rs` is a PRODUCTION file under this
//! repo's 500-SLOC cap; a `*_tests.rs` sibling is classified as a test file
//! (3000-SLOC cap) by `scripts/check_line_cap.sh`, matching
//! `managed_config_tests.rs` and `agent_source_tests.rs`.
//!
//! What: the two triggers (a version bump, a skill-selection change), the no-op
//! when neither moved, the manifest edit that deliberately does NOT trigger, and
//! the two negatives the owner's standing rule demands — a project-custom skill
//! and a checksum-frozen hand edit both survive a version-triggered redeploy.

use super::*;
use crate::core::skill_tiers::{SkillTier, list_project_custom_stems};
use tempfile::TempDir;

/// Two versions standing in for "before" and "after" an upgrade.
const V1: &str = "1.3.5";
const V2: &str = "1.3.6";

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
/// the highest precedence (#4832 moved it into `framework/`), and the reason the
/// stamp still carries a manifest component at all — it can move between
/// releases.
/// What: writes `<harness-root>/.trusty-mpm/framework/manifest.toml`.
fn write_project_manifest(project_dir: &Path, body: &str) {
    let dir = crate::core::harness_root::framework_dir(project_dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("manifest.toml"), body).unwrap();
}

/// The resolved plan for a project, as [`ensure_project_skill_tier`] sees it.
fn plan_for(fw: &FrameworkPaths, project_dir: &Path) -> HarnessPlan {
    let catalog_root = crate::content::catalog_root_for(&fw.root);
    let manifest = resolve_manifest(&ManifestSources::resolve(project_dir, &catalog_root));
    HarnessPlan::from_manifest(&manifest, fw, &catalog_root)
}

/// A temp base, a framework holding one seeded skill, and an empty workspace.
fn fixture(stem: &str, body: &str) -> (TempDir, FrameworkPaths, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let fw = seed_framework(tmp.path(), stem, body);
    let project = tmp.path().join("workspace");
    std::fs::create_dir_all(&project).unwrap();
    (tmp, fw, project)
}

#[test]
fn stamp_is_stable_across_calls() {
    let (_tmp, fw, project) = fixture("probe-skill", "V1");
    let plan = plan_for(&fw, &project);
    assert_eq!(
        project_tier_stamp(V1, &plan),
        project_tier_stamp(V1, &plan),
        "the stamp is a pure function of its inputs"
    );
}

#[test]
fn stamp_changes_with_the_version() {
    let (_tmp, fw, project) = fixture("probe-skill", "V1");
    let plan = plan_for(&fw, &project);
    assert_ne!(
        project_tier_stamp(V1, &plan),
        project_tier_stamp(V2, &plan),
        "a version bump is the primary trigger"
    );
}

#[test]
fn stamp_changes_with_the_skill_selection() {
    let (_tmp, fw, project) = fixture("probe-skill", "V1");
    let before = project_tier_stamp(V1, &plan_for(&fw, &project));

    write_project_manifest(&project, "[skills]\nexclude = [\"something-else\"]\n");
    let after = project_tier_stamp(V1, &plan_for(&fw, &project));

    assert_ne!(
        before, after,
        "an operator editing `[skills]` in the project override must trigger, \
         without waiting for a release"
    );
}

/// The stamp carries the skill SELECTION, not the whole manifest.
///
/// Why: a `[style]` or `[mcp]` edit cannot change which skills this tier should
/// hold, so triggering on it would be a redeploy with nothing to deploy. This is
/// what "keep the manifest identity only if it alters the skill set" means
/// concretely.
#[test]
fn stamp_ignores_manifest_changes_outside_the_skill_selection() {
    let (_tmp, fw, project) = fixture("probe-skill", "V1");
    let before = project_tier_stamp(V1, &plan_for(&fw, &project));

    write_project_manifest(&project, "[style]\nactive = \"probe-style\"\n");
    let after = project_tier_stamp(V1, &plan_for(&fw, &project));

    assert_eq!(
        before, after,
        "a style edit changes no skill, so it must not force a redeploy"
    );
}

/// Trigger 1: a version bump refreshes a stale MANAGED project-tier skill.
///
/// Why: this is the whole defect. `<project>/.claude/skills` outranks the user
/// tier `managed_config` refreshes every run, so a project copy left behind by
/// an older binary silently shadowed the current one — the #4408 shape. The
/// owner's model: "when version is updated, we re-run deployment."
#[test]
fn version_bump_redeploys() {
    let (_tmp, fw, project) = fixture("probe-skill", "V1");
    let deployed = project_skill_dir(&project)
        .join("probe-skill")
        .join("SKILL.md");

    let first = ensure_project_skill_tier_for_version(&fw, &project, V1).unwrap();
    assert!(first.deployed, "the first run has no stamp, so it deploys");
    assert_eq!(std::fs::read_to_string(&deployed).unwrap(), "V1");

    // The upgrade: newer skill text shipped under a newer version.
    reseed(&fw, "probe-skill", "V2-REFRESHED");
    let second = ensure_project_skill_tier_for_version(&fw, &project, V2).unwrap();

    assert!(second.deployed, "a version bump must re-run deployment");
    assert_eq!(
        std::fs::read_to_string(&deployed).unwrap(),
        "V2-REFRESHED",
        "a managed, user-unmodified project-tier skill must be refreshed"
    );
}

/// Trigger 2: a skill-selection change redeploys at the SAME version.
///
/// Why: the operator's `<harness-root>/.trusty-mpm/framework/manifest.toml` is
/// editable between releases and feeds `plan.skill_include`/`skill_exclude`
/// directly (`manifest::apply`). Waiting for a version bump would strand that
/// edit.
#[test]
fn skill_selection_change_redeploys() {
    let (_tmp, fw, project) = fixture("probe-skill", "V1");
    let deployed = project_skill_dir(&project)
        .join("probe-skill")
        .join("SKILL.md");

    ensure_project_skill_tier_for_version(&fw, &project, V1).unwrap();

    reseed(&fw, "probe-skill", "V1-PLUS");
    // Excludes a DIFFERENT stem, so `probe-skill` stays selected — this isolates
    // "the selection moved" from "the selection now drops this skill".
    write_project_manifest(&project, "[skills]\nexclude = [\"something-else\"]\n");
    let report = ensure_project_skill_tier_for_version(&fw, &project, V1).unwrap();

    assert!(
        report.deployed,
        "an edited `[skills]` selection must deploy at the same version"
    );
    assert_eq!(std::fs::read_to_string(&deployed).unwrap(), "V1-PLUS");
}

/// The no-op: same version, same selection, nothing written.
///
/// Why mtime and not just `deployed == false`: "not on every run" is the point
/// of the ruling, and an unconditional redeploy would still leave the content
/// correct while churning every file in the workspace on every resume. This
/// mirrors `skill_source::ensure_skill_source_fresh_is_noop_when_current`.
#[test]
fn unchanged_version_is_a_noop() {
    let (_tmp, fw, project) = fixture("probe-skill", "V1");
    let deployed = project_skill_dir(&project)
        .join("probe-skill")
        .join("SKILL.md");

    ensure_project_skill_tier_for_version(&fw, &project, V1).unwrap();
    let before = std::fs::metadata(&deployed).unwrap().modified().unwrap();

    let second = ensure_project_skill_tier_for_version(&fw, &project, V1).unwrap();

    assert!(
        !second.deployed,
        "a matching stamp must take the no-op path, not a redeploy"
    );
    assert_eq!(second.stats, DeployStats::default());
    let after = std::fs::metadata(&deployed).unwrap().modified().unwrap();
    assert_eq!(before, after, "a no-op must not rewrite a single file");
}

/// Negative 1: a project-custom skill survives a version-triggered redeploy.
///
/// Why: clause 3 of the owner's model — local customizations are tracked in the
/// local manifest and outlive an upgrade. "Re-run deployment on version change"
/// must not become "wipe the user's local copies on upgrade". Two independent
/// mechanisms enforce it and this asserts the stronger one:
/// `tiers::list_project_custom_stems` sees an unmanaged directory, so
/// `plan_skill_tiers` records a Project-over-Bundled shadow and drops the stem
/// from `bundled_deploy` — it never reaches file I/O, which is why it appears in
/// `shadowed` and in NO `stats` vector. `deployer::deploy_one_file`'s
/// unmanaged-target skip is the second line of defense behind it.
#[test]
fn project_custom_skill_is_never_overwritten() {
    let (_tmp, fw, project) = fixture("probe-skill", "BUNDLED");

    // The operator's own file, sharing a stem with a bundled skill so the
    // ownership rule is what is under test rather than mere absence.
    let custom = project_skill_dir(&project)
        .join("probe-skill")
        .join("SKILL.md");
    std::fs::create_dir_all(custom.parent().unwrap()).unwrap();
    let mine = "MY OWN SKILL — never overwrite this\n";
    std::fs::write(&custom, mine).unwrap();

    let first = ensure_project_skill_tier_for_version(&fw, &project, V1).unwrap();
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

    // …and again across the upgrade that re-runs deployment.
    reseed(&fw, "probe-skill", "BUNDLED-V2");
    let second = ensure_project_skill_tier_for_version(&fw, &project, V2).unwrap();

    assert!(second.deployed, "the version bumped, so a deploy ran");
    assert_eq!(
        std::fs::read_to_string(&custom).unwrap(),
        mine,
        "a version bump must not wipe the operator's local copy"
    );
    assert!(
        second.shadowed.iter().any(|s| s.stem == "probe-skill"),
        "…still shadowed on the version-triggered run: {:?}",
        second.shadowed
    );
}

/// Negative 2: a checksum-frozen skill survives a version-triggered redeploy.
///
/// Why: a skill tm DID deploy (so it is in the manifest) but the operator has
/// since hand-edited drifts from its recorded checksum. `deploy_one_file`
/// preserves it. Reconciling today's already-frozen files is a separate one-time
/// migration, not this path's job.
#[test]
fn frozen_skill_is_still_skipped() {
    let (_tmp, fw, project) = fixture("probe-skill", "V1");
    let deployed = project_skill_dir(&project)
        .join("probe-skill")
        .join("SKILL.md");

    // First run makes it MANAGED, then the operator edits it in place.
    ensure_project_skill_tier_for_version(&fw, &project, V1).unwrap();
    let hand_edit = "V1 plus my own notes\n";
    std::fs::write(&deployed, hand_edit).unwrap();

    reseed(&fw, "probe-skill", "V2");
    let report = ensure_project_skill_tier_for_version(&fw, &project, V2).unwrap();

    assert!(report.deployed, "the version bumped, so a deploy ran");
    assert_eq!(
        std::fs::read_to_string(&deployed).unwrap(),
        hand_edit,
        "a checksum-frozen hand edit must survive a version-triggered redeploy"
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

    let report = ensure_project_skill_tier_for_version(&fw, &project, V1).unwrap();

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
    let (_tmp, fw, project) = fixture("probe-skill", "V1");

    ensure_project_skill_tier_for_version(&fw, &project, V1).unwrap();

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
    let (_tmp, fw, project) = fixture("probe-skill", "V1");

    ensure_project_skill_tier_for_version(&fw, &project, V1).unwrap();
    let dest = project_skill_dir(&project);
    assert!(dest.join(PROJECT_TIER_STAMP_FILE).is_file());

    let custom = list_project_custom_stems(&dest).unwrap();
    assert!(
        !custom.iter().any(|s| s.starts_with('.')),
        "the stamp marker must never be mistaken for a project-custom skill: {custom:?}"
    );
}

/// PR #4882 review (MEDIUM): an EXISTING but empty source must refuse, not
/// stamp.
///
/// Why: `skill_source_dir()` prefers the `agents/skills` submodule whenever that
/// path exists, and an unfetched or shallow submodule is an existing-but-empty
/// directory. An `is_dir()`-only guard passes there, writes the stamp as though
/// deployment succeeded, and leaves the project tier stale forever with no
/// signal — the silent-staleness class of #4840 / #4873.
#[test]
fn empty_skill_source_does_not_stamp() {
    let tmp = TempDir::new().unwrap();
    let submodule_root = TempDir::new().unwrap();
    let submodule_skills = submodule_root.path().join("agents").join("skills");
    std::fs::create_dir_all(&submodule_skills).unwrap();

    let mut fw = FrameworkPaths::under(tmp.path());
    fw.trusty_mpm_root = Some(submodule_root.path().to_path_buf());
    assert_eq!(
        fw.skill_source_dir(),
        submodule_skills,
        "fixture guard: the empty submodule must be the resolved source"
    );

    let project = tmp.path().join("workspace");
    std::fs::create_dir_all(&project).unwrap();

    let report = ensure_project_skill_tier_for_version(&fw, &project, V1).unwrap();

    assert!(!report.deployed);
    assert!(
        !project_skill_dir(&project)
            .join(PROJECT_TIER_STAMP_FILE)
            .exists(),
        "an empty source must not be recorded as a successful deploy — the stamp \
         would freeze the project tier stale forever"
    );
}
