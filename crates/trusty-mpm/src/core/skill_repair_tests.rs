//! Tests for [`super`] — `tm doctor --fix-skills`.
//!
//! Why: the three owner constraints on a fix mode (never silently overwrite a
//! frozen skill, always back up, always verify from disk) are safety rules, so
//! each gets a test that fails if the rule is removed. The deletion boundary is
//! asserted too: nothing this module touches may disappear.
//! What: one test per constraint plus the ordinary repair path.
//! Test: this file.

use super::*;
use crate::core::skill_drift::skill_reference;
use std::collections::BTreeMap;
use std::fs;
use tempfile::TempDir;

/// Build a reference map from literal (stem, content) pairs.
fn reference_of(pairs: &[(&str, &str)]) -> SkillReference {
    SkillReference {
        assets: pairs
            .iter()
            .map(|(s, c)| (s.to_string(), c.to_string()))
            .collect::<BTreeMap<_, _>>(),
        origin: "this binary's embedded bundled assets".to_string(),
    }
}

/// Deploy `stem` into `dest` with `file_content` on disk and the checksum of
/// `manifest_content` recorded.
fn deploy(dest: &Path, stem: &str, file_content: &str, manifest_content: &str) {
    let dir = dest.join(stem);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), file_content).unwrap();

    let mut manifest = SkillManifest::load(dest);
    manifest.managed.insert(
        stem.to_string(),
        SkillManifestEntry {
            checksum: checksum(manifest_content),
            deployed_at: "2026-07-29T00:00:00Z".to_string(),
        },
    );
    manifest.save(dest).unwrap();
}

/// A `FrameworkPaths` rooted entirely under one temp dir, with no submodule.
fn paths_under(tmp: &TempDir) -> FrameworkPaths {
    let mut paths = FrameworkPaths::under(tmp.path());
    paths.trusty_mpm_root = None;
    paths
}

#[test]
fn repair_rewrites_drifted_and_verifies() {
    // The ordinary case: tm still owns the file, so the redeploy is safe. The
    // outcome is only `Repaired` because the content was READ BACK and matched.
    let tmp = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let dest = paths.claude_skills_dir();
    deploy(&dest, "tm-workflow", "v1", "v1");

    let outcomes = repair_skills(
        &reference_of(&[("tm-workflow", "v2")]),
        &paths,
        None,
        false,
        backups.path(),
    );
    let repaired: Vec<&RepairOutcome> = outcomes.iter().filter(|o| o.changed()).collect();
    assert_eq!(repaired.len(), 1, "outcomes: {outcomes:?}");
    assert_eq!(repaired[0].stem, "tm-workflow");

    // Verified independently of the fix's own claim.
    let on_disk = fs::read_to_string(dest.join("tm-workflow").join("SKILL.md")).unwrap();
    assert_eq!(on_disk, "v2");

    // And the manifest was updated, so the file is tm-owned again rather than
    // reading as frozen on the next audit.
    assert!(SkillManifest::load(&dest).checksum_matches("tm-workflow", "v2"));
}

/// CONSTRAINT 1: a frozen skill is NEVER silently overwritten.
#[test]
fn repair_never_touches_a_frozen_skill_by_default() {
    let tmp = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let dest = paths.claude_skills_dir();
    deploy(
        &dest,
        "tm-workflow",
        "hand-edited by the operator",
        "what tm wrote",
    );

    let outcomes = repair_skills(
        &reference_of(&[("tm-workflow", "v2")]),
        &paths,
        None,
        /* include_frozen */ false,
        backups.path(),
    );
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].action, RepairAction::SkippedFrozen);

    // The operator's edit survives, byte for byte.
    let on_disk = fs::read_to_string(dest.join("tm-workflow").join("SKILL.md")).unwrap();
    assert_eq!(on_disk, "hand-edited by the operator");
}

/// CONSTRAINT 1 (opt-in half) + CONSTRAINT 2: with the explicit flag the frozen
/// file IS repaired, and the hand-edited content is backed up first.
#[test]
fn repair_backs_up_before_overwriting() {
    let tmp = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let dest = paths.claude_skills_dir();
    deploy(
        &dest,
        "tm-workflow",
        "hand-edited by the operator",
        "what tm wrote",
    );

    let outcomes = repair_skills(
        &reference_of(&[("tm-workflow", "v2")]),
        &paths,
        None,
        /* include_frozen */ true,
        backups.path(),
    );
    let RepairAction::Repaired { backup } = &outcomes[0].action else {
        panic!("expected a repair, got {:?}", outcomes[0].action);
    };
    let backup = backup.as_ref().expect("an existing file must be backed up");
    assert_eq!(
        fs::read_to_string(backup).unwrap(),
        "hand-edited by the operator",
        "the backup must hold the operator's original content"
    );
    assert_eq!(
        fs::read_to_string(dest.join("tm-workflow").join("SKILL.md")).unwrap(),
        "v2"
    );
}

/// CONSTRAINT 3: the repair verifies from DISK, so a write that did not land is
/// reported as a failure rather than as success.
///
/// Why: reporting success from a write's return value — without re-reading — is
/// the exact failure that produced #4604. Here the skill directory is made
/// unwritable, so the audit still classifies the skill as `Drifted` (the file
/// reads fine) but the write cannot land; the outcome must be `Failed`.
#[cfg(unix)]
#[test]
fn repair_reports_verification_failure() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let dest = paths.claude_skills_dir();
    deploy(&dest, "tm-workflow", "v1", "v1");

    let skill_dir = dest.join("tm-workflow");
    fs::set_permissions(&skill_dir, fs::Permissions::from_mode(0o500)).unwrap();
    // Precondition: the environment must actually be able to deny the write
    // (running as root defeats mode bits). If it cannot, the scenario under
    // test does not exist here, so restore and skip rather than assert a
    // failure that could not occur.
    let deniable = fs::write(skill_dir.join(".probe"), "x").is_err();
    if !deniable {
        let _ = fs::remove_file(skill_dir.join(".probe"));
        fs::set_permissions(&skill_dir, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let outcomes = repair_skills(
        &reference_of(&[("tm-workflow", "v2")]),
        &paths,
        None,
        true,
        backups.path(),
    );
    fs::set_permissions(&skill_dir, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(
        matches!(outcomes[0].action, RepairAction::Failed(_)),
        "got {:?}",
        outcomes[0].action
    );
    assert!(!outcomes[0].changed());
    // And the original content is still there — a failed repair destroys nothing.
    assert_eq!(
        fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
        "v1"
    );
}

/// An unverifiable skill is never repaired — there is nothing to write.
#[test]
fn repair_skips_unverifiable_skills() {
    let tmp = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let dest = paths.claude_skills_dir();
    deploy(
        &dest,
        "my-custom-skill",
        "operator content",
        "operator content",
    );

    let outcomes = repair_skills(
        &reference_of(&[("tm-workflow", "v2")]),
        &paths,
        None,
        true,
        backups.path(),
    );
    assert!(
        matches!(outcomes[0].action, RepairAction::SkippedUnverifiable(_)),
        "got {:?}",
        outcomes[0].action
    );
    assert_eq!(
        fs::read_to_string(dest.join("my-custom-skill").join("SKILL.md")).unwrap(),
        "operator content"
    );
}

/// A fresh skill produces no outcome and no write at all.
#[test]
fn repair_is_a_noop_when_everything_matches() {
    let tmp = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    deploy(&paths.claude_skills_dir(), "tm-workflow", "v2", "v2");

    let outcomes = repair_skills(
        &reference_of(&[("tm-workflow", "v2")]),
        &paths,
        None,
        false,
        backups.path(),
    );
    assert!(outcomes.is_empty(), "outcomes: {outcomes:?}");
    assert!(
        !backups.path().join("operator-home").exists(),
        "a no-op run must not create backup directories"
    );
}

/// HARD BOUNDARY: the repair never removes anything.
///
/// Why: `tm doctor` reports orphaned worktrees and undercounted worktree disk;
/// those stay report-only. On 2026-07-21 an `rm -rf .base/*` orphaned ~70
/// worktrees across concurrent sessions. This asserts the fix path leaves every
/// pre-existing file in place, including files it does not manage.
#[test]
fn repair_deletes_nothing() {
    let tmp = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let dest = paths.claude_skills_dir();
    deploy(&dest, "tm-workflow", "v1", "v1");
    // A bystander file and a bystander directory the repair has no business with.
    fs::write(dest.join("README.md"), "operator note").unwrap();
    fs::create_dir_all(dest.join("unrelated-dir")).unwrap();
    fs::write(dest.join("unrelated-dir").join("keep.txt"), "keep").unwrap();

    repair_skills(
        &reference_of(&[("tm-workflow", "v2")]),
        &paths,
        None,
        true,
        backups.path(),
    );

    assert!(dest.join("README.md").exists());
    assert!(dest.join("unrelated-dir").join("keep.txt").exists());
    assert!(dest.join("tm-workflow").join("SKILL.md").exists());
}

#[test]
fn backup_root_follows_the_remediation_convention() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-08-02T13:45:06Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let root = backup_root_for(Path::new("/home/x"), now);
    assert_eq!(
        root,
        Path::new("/home/x/.trusty-mpm/backup-doctor-remediation-20260802-134506")
    );
}

/// The real embedded reference must be usable by the repair path, not just the
/// literal fixtures the other tests use.
#[test]
fn embedded_reference_is_usable_as_a_repair_source() {
    let reference = skill_reference(None);
    assert!(
        reference.assets.contains_key("tm-workflow"),
        "the binary must embed tm-workflow for --fix-skills to repair it"
    );
    assert!(!reference.assets["tm-workflow"].is_empty());
}
