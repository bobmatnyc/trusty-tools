//! Tests for [`super`] — the `tm doctor --fix` repair driver (issue #4948).
//!
//! Why: this command deletes and rewrites operator state, so each of its three
//! safety rules gets a test that fails if the rule is removed — dry run writes
//! nothing, a file tm does not own is refused, and `legacy_sources` findings
//! are never deleted. The "it actually repairs" tests are the easy half; the
//! refusals are the half worth having.
//! What: per repair, one apply test and one dry-run test, plus the ownership
//! refusals.
//! Test: this file.

use super::*;
use std::fs;

/// A settings file carrying one tm hook entry and one foreign (claude-mpm) one.
///
/// Why: the interesting case is the mixed file — the repair must strip tm's
/// entry and leave the other harness's alone. A tm-only fixture cannot show
/// that.
fn write_mixed_settings(project: &Path) -> PathBuf {
    let claude = project.join(".claude");
    fs::create_dir_all(&claude).unwrap();
    let path = claude.join("settings.json");
    fs::write(
        &path,
        serde_json::json!({
            "model": "opus",
            "hooks": {
                "PreToolUse": [
                    { "hooks": [{ "type": "command", "command": "tm hook" }] },
                    { "hooks": [{ "type": "command", "command": "claude-mpm hook" }] }
                ]
            }
        })
        .to_string(),
    )
    .unwrap();
    path
}

/// Create a real, minimal git repo in a hermetic temp dir.
///
/// Why: `push_guard` resolves its hooks directory by shelling out to `git
/// rev-parse --git-common-dir`, so a plain temp directory cannot exercise it.
/// Mirrors `core::push_guard`'s own helper, including the `None` return when
/// git is unavailable.
fn temp_repo() -> Option<(tempfile::TempDir, PathBuf)> {
    let dir = crate::test_support::hermetic_temp_dir();
    let path = dir.path().to_path_buf();
    let ok = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .arg(&path)
        .status()
        .ok()?;
    ok.success().then_some((dir, path))
}

#[test]
fn mode_defaults_to_dry_run() {
    // The whole safety posture rests on this one conversion: a `--fix` with no
    // `--yes` must never reach a write path.
    assert_eq!(RepairMode::from_apply_flag(false), RepairMode::DryRun);
    assert_eq!(RepairMode::from_apply_flag(true), RepairMode::Apply);
}

#[test]
fn hooks_repair_applies_and_backs_up() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_mixed_settings(tmp.path());

    let steps = repair_hooks_contamination(tmp.path(), RepairMode::Apply);
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].check, "hooks_contamination");
    assert_eq!(steps[0].path, path);
    assert!(steps[0].changed(), "{:?}", steps[0]);

    // Verified from disk, not from the repair's own claim.
    let after: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let groups = after["hooks"]["PreToolUse"].as_array().unwrap();
    let commands: Vec<&str> = groups
        .iter()
        .flat_map(|g| g["hooks"].as_array().unwrap())
        .map(|e| e["command"].as_str().unwrap())
        .collect();
    assert_eq!(
        commands,
        vec!["claude-mpm hook"],
        "only tm's own entry may be removed"
    );
    assert_eq!(after["model"], "opus", "unrelated keys must survive");

    // The backup the step reports must exist and hold the pre-repair bytes.
    let StepStatus::Applied { backup: Some(bak) } = &steps[0].status else {
        panic!("expected a backup path, got {:?}", steps[0].status);
    };
    assert!(
        fs::read_to_string(bak).unwrap().contains("tm hook"),
        "the backup must carry what was removed"
    );
}

#[test]
fn hooks_repair_dry_run_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_mixed_settings(tmp.path());
    let before = fs::read(&path).unwrap();

    let steps = repair_hooks_contamination(tmp.path(), RepairMode::DryRun);
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].status, StepStatus::Planned);
    assert!(!steps[0].changed());
    assert!(
        steps[0].what.contains("PreToolUse"),
        "the preview must name what it would remove: {}",
        steps[0].what
    );

    assert_eq!(fs::read(&path).unwrap(), before, "dry run rewrote the file");
    // And it must not have left a backup behind either — a backup is a write.
    let stray: Vec<_> = fs::read_dir(tmp.path().join(".claude"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(".bak"))
        .collect();
    assert!(stray.is_empty(), "dry run wrote a backup: {stray:?}");
}

#[test]
fn hooks_repair_leaves_foreign_entries_alone() {
    // A project carrying ONLY another harness's hooks has no tm contamination,
    // so the repair must produce no step at all — never a step that would
    // remove somebody else's configuration.
    let tmp = tempfile::tempdir().unwrap();
    let claude = tmp.path().join(".claude");
    fs::create_dir_all(&claude).unwrap();
    let path = claude.join("settings.json");
    let body = serde_json::json!({
        "hooks": { "Stop": [{ "hooks": [{ "command": "claude-mpm hook" }] }] }
    })
    .to_string();
    fs::write(&path, &body).unwrap();

    let steps = repair_hooks_contamination(tmp.path(), RepairMode::Apply);
    assert!(steps.is_empty(), "{steps:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), body);
}

#[test]
fn push_guard_repair_installs_when_missing() {
    let Some((_dir, repo)) = temp_repo() else {
        return;
    };
    let steps = repair_push_guard(&repo, RepairMode::Apply);
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].check, "push_guard");
    assert!(steps[0].changed(), "{:?}", steps[0]);
    assert!(
        fs::read_to_string(&steps[0].path)
            .unwrap()
            .contains(crate::core::push_guard::HOOK_MARKER),
        "the guard must actually be on disk at the reported path"
    );

    // Idempotent: a second run has nothing to report.
    assert!(repair_push_guard(&repo, RepairMode::Apply).is_empty());
}

#[test]
fn push_guard_repair_dry_run_writes_nothing() {
    let Some((_dir, repo)) = temp_repo() else {
        return;
    };
    let steps = repair_push_guard(&repo, RepairMode::DryRun);
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].status, StepStatus::Planned);
    assert!(
        !steps[0].path.exists(),
        "dry run installed the hook at {}",
        steps[0].path.display()
    );
}

#[test]
fn push_guard_repair_refuses_a_foreign_hook() {
    // The ownership rule: a `pre-push` tm did not write is never overwritten,
    // in either mode. This is the same judgement `inspect_pre_push_guard`
    // makes for the read-only doctor check, which is why there is one of it.
    let Some((_dir, repo)) = temp_repo() else {
        return;
    };
    let hooks = crate::core::push_guard::effective_hooks_dir(&repo).unwrap();
    fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("pre-push");
    fs::write(&hook, "#!/bin/sh\n# somebody else's hook\n").unwrap();

    for mode in [RepairMode::DryRun, RepairMode::Apply] {
        let steps = repair_push_guard(&repo, mode);
        assert_eq!(steps.len(), 1, "{steps:?}");
        assert!(
            matches!(steps[0].status, StepStatus::Refused(_)),
            "{mode:?} must refuse a foreign hook, got {:?}",
            steps[0].status
        );
    }
    assert_eq!(
        fs::read_to_string(&hook).unwrap(),
        "#!/bin/sh\n# somebody else's hook\n",
        "the foreign hook was modified"
    );
}

#[test]
fn legacy_sources_are_refused_never_deleted() {
    // The check whose obvious repair must not ship. Every finding is reported
    // with its path and a reason, and every file survives — including one
    // hand-edited copy, which is precisely the case a name-based delete could
    // not have distinguished.
    let tmp = tempfile::tempdir().unwrap();
    let skills = tmp.path().join(".claude").join("skills");
    for stem in ["tm-workflow", "tm-doctor"] {
        let dir = skills.join(stem);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), "hand-edited by the operator").unwrap();
    }
    fs::create_dir_all(tmp.path().join(".trusty-mpm").join("claude-config")).unwrap();

    let steps = refuse_legacy_sources(tmp.path());
    assert_eq!(steps.len(), 3, "{steps:?}");
    for step in &steps {
        assert_eq!(step.check, "legacy_sources");
        assert!(
            matches!(step.status, StepStatus::Refused(_)),
            "every legacy_sources finding must be refused, got {:?}",
            step.status
        );
        assert!(!step.changed());
        assert!(step.path.exists(), "{} was deleted", step.path.display());
    }
    assert!(
        skills.join("tm-workflow").join("SKILL.md").is_file(),
        "the hand-edited copy must survive verbatim"
    );
}

#[test]
fn legacy_sources_ignores_a_foreign_skill() {
    // Only `tm-*` entries are trusty-mpm's. An unrelated user skill in the
    // same directory must not even be mentioned.
    let tmp = tempfile::tempdir().unwrap();
    let skills = tmp.path().join(".claude").join("skills");
    fs::create_dir_all(skills.join("my-own-skill")).unwrap();

    assert!(refuse_legacy_sources(tmp.path()).is_empty());
}

#[test]
fn legacy_sources_is_empty_on_a_clean_home() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(refuse_legacy_sources(tmp.path()).is_empty());
}
