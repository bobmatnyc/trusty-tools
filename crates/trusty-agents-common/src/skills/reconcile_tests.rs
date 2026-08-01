//! Tests for [`crate::skills::reconcile`] (issue #4605).
//!
//! Why: adoption is the one path that writes over a file tm cannot prove it
//! authored. Every guarantee that makes that acceptable — the roster is the
//! only admission test, a backup exists before ownership is recorded, and the
//! adoption actually unsticks the deploy — is pinned here rather than
//! documented.
//! What: staged untracked skills under temp deploy targets, adopted, then
//! re-deployed to prove the end-to-end refresh.
//! Test: this file.

use super::*;
use crate::skills::deployer::deploy_skills;
use crate::skills::tiers::list_project_custom_stems;
use std::fs;
use tempfile::TempDir;

/// Build a `bundled` roster from stems.
fn roster(stems: &[&str]) -> BTreeSet<String> {
    stems.iter().map(|s| (*s).to_string()).collect()
}

/// Stage `<dest>/<stem>/SKILL.md` with `body`, bypassing the deployer so the
/// file is untracked — the #4605 state.
fn stage_untracked(dest: &Path, stem: &str, body: &str) -> PathBuf {
    let dir = dest.join(stem);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("SKILL.md");
    fs::write(&path, body).unwrap();
    path
}

#[test]
fn adopt_registers_the_skill_and_its_references() {
    let dest = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    stage_untracked(dest.path(), "tm-workflow", "stale");
    let refs = dest.path().join("tm-workflow").join("references");
    fs::create_dir_all(&refs).unwrap();
    fs::write(refs.join("a.md"), "old ref").unwrap();

    let adopted =
        adopt_unmanaged_bundled_skills(dest.path(), &roster(&["tm-workflow"]), backups.path())
            .unwrap();

    assert_eq!(adopted.len(), 1);
    assert_eq!(
        adopted[0].adopted_keys,
        vec![
            "tm-workflow".to_string(),
            "tm-workflow/references/a.md".to_string()
        ]
    );
    let manifest = SkillManifest::load(dest.path());
    assert!(manifest.is_managed("tm-workflow"));
    assert!(manifest.is_managed("tm-workflow/references/a.md"));
    // The recorded checksum is of the content ON DISK, so the deployer sees
    // "managed and unmodified" rather than "the user edited it".
    assert!(manifest.checksum_matches("tm-workflow", "stale"));
}

#[test]
fn adopt_backs_up_before_recording() {
    let dest = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    let staged = stage_untracked(dest.path(), "tm-workflow", "irreplaceable text");

    let adopted =
        adopt_unmanaged_bundled_skills(dest.path(), &roster(&["tm-workflow"]), backups.path())
            .unwrap();

    let copy = adopted[0].backup_dir.join("SKILL.md");
    assert!(copy.is_file(), "no backup at {}", copy.display());
    assert_eq!(fs::read_to_string(&copy).unwrap(), "irreplaceable text");
    // The original is untouched by adoption itself — only the manifest changed.
    assert_eq!(fs::read_to_string(&staged).unwrap(), "irreplaceable text");
}

#[test]
fn adopt_writes_the_backup_ledger() {
    let dest = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    stage_untracked(dest.path(), "tm-workflow", "stale");

    adopt_unmanaged_bundled_skills(dest.path(), &roster(&["tm-workflow"]), backups.path()).unwrap();

    let log = fs::read_to_string(backups.path().join(BACKUP_LEDGER_FILE)).unwrap();
    assert!(log.starts_with("BACKUP: "), "unexpected ledger: {log}");
    assert!(log.contains("tm-workflow/SKILL.md"), "ledger: {log}");
}

#[test]
fn adopt_leaves_an_operator_skill_alone() {
    // A stem matching nothing bundled is the operator's. Never backed up,
    // never adopted, never touched.
    let dest = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    stage_untracked(dest.path(), "my-own-skill", "mine");

    let adopted =
        adopt_unmanaged_bundled_skills(dest.path(), &roster(&["tm-workflow"]), backups.path())
            .unwrap();

    assert!(adopted.is_empty());
    assert!(!SkillManifest::load(dest.path()).is_managed("my-own-skill"));
    assert_eq!(
        fs::read_to_string(dest.path().join("my-own-skill").join("SKILL.md")).unwrap(),
        "mine"
    );
    assert!(!backups.path().join(BACKUP_LEDGER_FILE).exists());
}

#[test]
fn adopt_no_findings_writes_nothing() {
    // Nothing in scope must leave the target byte-identical — in particular it
    // must not create a manifest where none existed.
    let dest = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();

    let adopted =
        adopt_unmanaged_bundled_skills(dest.path(), &roster(&["tm-workflow"]), backups.path())
            .unwrap();

    assert!(adopted.is_empty());
    assert!(
        !dest
            .path()
            .join(crate::skills::manifest::SKILL_MANIFEST_FILE)
            .exists()
    );
}

#[test]
fn adopt_then_deploy_refreshes_a_stale_skill() {
    // The end-to-end claim of issue #4605: before adoption the tier planner
    // classifies the stem project-custom and no deploy reaches it; after
    // adoption the ordinary deploy refreshes it.
    let source = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    fs::write(source.path().join("tm-workflow.md"), "current bundled text").unwrap();
    let deployed = stage_untracked(dest.path(), "tm-workflow", "stale text");

    // Before: untracked -> classified project-custom -> excluded from deploy.
    assert!(
        list_project_custom_stems(dest.path())
            .unwrap()
            .contains("tm-workflow")
    );
    deploy_skills(source.path(), dest.path()).unwrap();
    assert_eq!(
        fs::read_to_string(&deployed).unwrap(),
        "stale text",
        "a plain deploy must not touch an untracked file"
    );

    adopt_unmanaged_bundled_skills(dest.path(), &roster(&["tm-workflow"]), backups.path()).unwrap();

    // After: no longer project-custom, and the ordinary deploy refreshes it.
    assert!(
        !list_project_custom_stems(dest.path())
            .unwrap()
            .contains("tm-workflow")
    );
    let stats = deploy_skills(source.path(), dest.path()).unwrap();
    assert!(stats.deployed.contains(&"tm-workflow".to_string()));
    assert_eq!(
        fs::read_to_string(&deployed).unwrap(),
        "current bundled text"
    );
}

#[test]
fn preview_matches_what_adoption_touches() {
    let dest = TempDir::new().unwrap();
    let backups = TempDir::new().unwrap();
    stage_untracked(dest.path(), "tm-workflow", "stale");

    let preview = preview_unmanaged_bundled_skills(dest.path(), &roster(&["tm-workflow"]));
    let adopted =
        adopt_unmanaged_bundled_skills(dest.path(), &roster(&["tm-workflow"]), backups.path())
            .unwrap();

    let previewed: Vec<String> = preview.iter().map(|s| s.stem.clone()).collect();
    let touched: Vec<String> = adopted.iter().map(|s| s.stem.clone()).collect();
    assert_eq!(previewed, touched);
    assert_eq!(preview[0].manifest_keys(), adopted[0].adopted_keys);
}

#[test]
fn backup_target_mirrors_the_absolute_path() {
    // Two tiers can hold a same-named skill; a flat backup dir would let the
    // second clobber the first.
    let root = Path::new("/tmp/backups");
    assert_eq!(
        backup_target(root, Path::new("/Users/x/.claude/skills/tm-x")),
        Path::new("/tmp/backups/Users/x/.claude/skills/tm-x")
    );
    assert_ne!(
        backup_target(root, Path::new("/Users/x/.claude/skills/tm-x")),
        backup_target(root, Path::new("/Users/x/cfg/skills/tm-x"))
    );
}
