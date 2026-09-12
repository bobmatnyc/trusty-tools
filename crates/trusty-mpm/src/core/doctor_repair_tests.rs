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
fn hooks_repair_warns_when_it_removes_the_pm_guard() {
    // #7262: the step text must say what the removal COSTS. An operator who
    // reads "remove tm hook entries under [PreToolUse]" has no way to know PM
    // enforcement just went offline until the next managed launch.
    let tmp = tempfile::tempdir().unwrap();
    let claude = tmp.path().join(".claude");
    fs::create_dir_all(&claude).unwrap();
    let settings =
        crate::core::standalone::hooks::build_tree::tests::incident_settings().to_string();
    fs::write(claude.join("settings.json"), &settings).unwrap();

    for mode in [RepairMode::DryRun, RepairMode::Apply] {
        fs::write(claude.join("settings.json"), &settings).unwrap();
        let steps = repair_hooks_contamination(tmp.path(), mode);
        assert_eq!(steps.len(), 1, "{steps:?}");
        assert!(
            steps[0].what.contains("pm-guard enforcement is absent"),
            "{mode:?} step must name the consequence: {}",
            steps[0].what
        );
        // The clause is an ADDITION, never a replacement.
        assert!(
            steps[0].what.starts_with("remove tm hook entries under ["),
            "{}",
            steps[0].what
        );
    }
}

#[test]
fn hooks_repair_omits_the_pm_guard_clause_for_other_entries() {
    // The mixed fixture's only tm entry is the lifecycle triad, so the guard
    // clause would be false there.
    let tmp = tempfile::tempdir().unwrap();
    write_mixed_settings(tmp.path());

    let steps = repair_hooks_contamination(tmp.path(), RepairMode::DryRun);
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(
        !steps[0].what.contains("pm-guard"),
        "no guard was removed, so nothing may claim one was: {}",
        steps[0].what
    );
}

/// The installed binary the #7262 repoint tests write.
///
/// Why: [`resolve_stable_hook_exe`] reads the host's `PATH`, which would make
/// these assertions depend on whether the machine has `tm` installed. Every test
/// here injects this instead.
const INSTALLED_BIN: &str = "/usr/local/bin/tm";

/// Seed `<project>/.claude/settings.json` with the #7244 incident shape.
///
/// Why (#7262): the ONE fixture the classifier, the cleanup, the doctor probe
/// and now the repair all share — see
/// `core::standalone::hooks::build_tree::tests::incident_settings`.
fn write_build_tree_settings(project: &Path) -> PathBuf {
    let claude = project.join(".claude");
    fs::create_dir_all(&claude).unwrap();
    let path = claude.join("settings.json");
    fs::write(
        &path,
        crate::core::standalone::hooks::build_tree::tests::incident_settings().to_string(),
    )
    .unwrap();
    path
}

#[test]
fn build_tree_repair_repoints_at_the_installed_binary() {
    // #7262 reopened: `hooks_build_tree_binary` had no `--fix` arm at all, so
    // this is the whole point of the change.
    let tmp = tempfile::tempdir().unwrap();
    let path = write_build_tree_settings(tmp.path());

    let steps = repair_build_tree_binary_with(
        std::slice::from_ref(&path),
        Ok(PathBuf::from(INSTALLED_BIN)),
        RepairMode::Apply,
    );

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].check, "hooks_build_tree_binary");
    assert_eq!(steps[0].path, path);
    assert!(steps[0].changed(), "{:?}", steps[0]);
    assert!(
        steps[0]
            .what
            .contains("repoint 8 commands at /usr/local/bin/tm"),
        "{}",
        steps[0].what
    );
    match &steps[0].status {
        StepStatus::Applied { backup } => {
            let backup = backup.as_ref().expect("apply snapshots the file first");
            assert!(backup.exists(), "{}", backup.display());
        }
        other => panic!("expected Applied, got {other:?}"),
    }

    // Detect → repair → clean.
    let after: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        crate::core::standalone::hooks::cleanup::build_tree_hook_commands(&after).is_empty(),
        "{after}"
    );
    assert!(
        crate::core::standalone::hooks::cleanup::build_tree_statusline_command(&after).is_none(),
        "{after}"
    );
}

#[test]
fn build_tree_repair_dry_run_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_build_tree_settings(tmp.path());
    let raw = fs::read_to_string(&path).unwrap();

    let steps = repair_build_tree_binary_with(
        std::slice::from_ref(&path),
        Ok(PathBuf::from(INSTALLED_BIN)),
        RepairMode::DryRun,
    );

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].status, StepStatus::Planned);
    assert!(!steps[0].changed());
    assert_eq!(fs::read_to_string(&path).unwrap(), raw);
}

#[test]
fn build_tree_repair_is_idempotent() {
    // A second `--fix --yes` must plan nothing and rewrite nothing.
    let tmp = tempfile::tempdir().unwrap();
    let path = write_build_tree_settings(tmp.path());
    let files = [path.clone()];

    repair_build_tree_binary_with(&files, Ok(PathBuf::from(INSTALLED_BIN)), RepairMode::Apply);
    let after_first = fs::read_to_string(&path).unwrap();

    let second =
        repair_build_tree_binary_with(&files, Ok(PathBuf::from(INSTALLED_BIN)), RepairMode::Apply);

    assert!(second.is_empty(), "{second:?}");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        after_first,
        "the second pass must be byte-identical"
    );
}

#[test]
fn build_tree_repair_fails_closed_on_unparseable_json() {
    // The error arm: an unparseable file is REPORTED as a failed step, never
    // skipped silently and never rewritten.
    let tmp = tempfile::tempdir().unwrap();
    let claude = tmp.path().join(".claude");
    fs::create_dir_all(&claude).unwrap();
    let path = claude.join("settings.json");
    fs::write(&path, "{ \"hooks\": ").unwrap();

    let steps = repair_build_tree_binary_with(
        std::slice::from_ref(&path),
        Ok(PathBuf::from(INSTALLED_BIN)),
        RepairMode::Apply,
    );

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].check, "hooks_build_tree_binary");
    match &steps[0].status {
        StepStatus::Failed(msg) => assert!(msg.contains("is not valid JSON"), "{msg}"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), "{ \"hooks\": ");
}

#[test]
fn build_tree_repair_refuses_when_no_installed_binary_resolves() {
    // Nothing to repoint TO is a refusal with a reason, not a silent pass.
    let tmp = tempfile::tempdir().unwrap();
    let path = write_build_tree_settings(tmp.path());
    let raw = fs::read_to_string(&path).unwrap();

    let steps = repair_build_tree_binary_with(
        std::slice::from_ref(&path),
        Err(crate::core::standalone::hooks::StableHookExeError::Unresolved),
        RepairMode::Apply,
    );

    assert_eq!(steps.len(), 1, "{steps:?}");
    match &steps[0].status {
        StepStatus::Refused(reason) => {
            assert!(
                reason.contains("no installed tm/trusty-mpm binary"),
                "{reason}"
            );
        }
        other => panic!("expected Refused, got {other:?}"),
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), raw);
}

#[test]
fn build_tree_repair_is_silent_for_a_clean_file() {
    // A machine with no tm on PATH must not grow one refusal line per project.
    let tmp = tempfile::tempdir().unwrap();
    let path = write_mixed_settings(tmp.path());

    for installed in [
        Ok(PathBuf::from(INSTALLED_BIN)),
        Err(crate::core::standalone::hooks::StableHookExeError::Unresolved),
    ] {
        let steps = repair_build_tree_binary_with(
            std::slice::from_ref(&path),
            installed,
            RepairMode::Apply,
        );
        assert!(steps.is_empty(), "{steps:?}");
    }
    assert!(!tmp.path().join(".claude/settings.json.bak").exists());
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

// ── #5866: the output-style repair ──────────────────────────────────────────

/// Deploy every bundled style into `<home>/.claude/output-styles/`.
fn deploy_styles(home: &Path) {
    crate::core::output_style_deployer::deploy_output_styles(&home.join(".claude")).unwrap();
}

/// The path of one bundled style under `<home>/.claude/output-styles/`.
fn style_path(home: &Path, index: usize) -> PathBuf {
    home.join(".claude")
        .join("output-styles")
        .join(crate::core::bundle::OUTPUT_STYLES[index].file_name)
}

/// The remedy string and the repair that honours it must not drift apart.
#[test]
fn output_style_remedy_names_the_fix_command() {
    assert_eq!(OUTPUT_STYLE_REMEDY, "tm doctor --fix --yes");
    assert!(
        !OUTPUT_STYLE_REMEDY.contains("tm install"),
        "#5866: `tm install` has no output-style step"
    );
}

#[test]
fn output_style_repair_is_empty_when_in_sync() {
    let home = tempfile::tempdir().unwrap();
    deploy_styles(home.path());
    assert!(
        repair_output_style(home.path(), None, RepairMode::DryRun).is_empty(),
        "an in-sync tier produces no step, so `--fix` prints nothing about it"
    );
}

#[test]
fn output_style_repair_plans_the_drifted_file() {
    let home = tempfile::tempdir().unwrap();
    deploy_styles(home.path());
    let target = style_path(home.path(), 0);
    fs::write(&target, "stale text").unwrap();

    let steps = repair_output_style(home.path(), None, RepairMode::DryRun);
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].check, "output_style_staleness");
    assert_eq!(steps[0].path, target);
    assert_eq!(steps[0].status, StepStatus::Planned);
}

#[test]
fn output_style_repair_dry_run_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    deploy_styles(home.path());
    let target = style_path(home.path(), 0);
    fs::write(&target, "stale text").unwrap();

    repair_output_style(home.path(), None, RepairMode::DryRun);

    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "stale text",
        "a dry run must leave the file exactly as it found it"
    );
}

/// The repair reports from DISK, and it fixes an absent file too — the state
/// `check_output_style`'s own remedy string pointed at `tm install` for.
#[test]
fn output_style_repair_applies_and_reports_from_disk() {
    let home = tempfile::tempdir().unwrap();
    deploy_styles(home.path());
    let drifted = style_path(home.path(), 0);
    let missing = style_path(home.path(), 1);
    fs::write(&drifted, "stale text").unwrap();
    fs::remove_file(&missing).unwrap();

    let steps = repair_output_style(home.path(), None, RepairMode::Apply);

    assert_eq!(steps.len(), 2, "{steps:?}");
    assert!(
        steps.iter().all(RepairStep::changed),
        "both a drifted and an absent style must be written: {steps:?}"
    );
    assert_eq!(
        fs::read_to_string(&drifted).unwrap(),
        crate::core::bundle::OUTPUT_STYLES[0].content
    );
    assert_eq!(
        fs::read_to_string(&missing).unwrap(),
        crate::core::bundle::OUTPUT_STYLES[1].content
    );
    assert!(
        repair_output_style(home.path(), None, RepairMode::DryRun).is_empty(),
        "the repair must actually clear the finding it reported"
    );
}

/// #7423: a drifted style in the managed `$CLAUDE_CONFIG_DIR` tier is rewritten,
/// and a clean operator-home tier is left alone.
///
/// Why: `tm doctor --fix --yes` on 1.5.29 redeployed `~/.claude/output-styles/`
/// and left `$CLAUDE_CONFIG_DIR/output-styles/trusty-mpm.md` at its old 12,012
/// bytes — the copy every tm-launched session reads. Fails on `origin/main`,
/// whose body hardcoded `home.join(".claude")`.
/// Test: this function IS the test.
#[test]
fn output_style_repair_rewrites_the_managed_tier() {
    let home = tempfile::tempdir().unwrap();
    let managed = tempfile::tempdir().unwrap();
    deploy_styles(home.path());

    let managed_styles = managed.path().join("output-styles");
    fs::create_dir_all(&managed_styles).unwrap();
    for style in crate::core::bundle::OUTPUT_STYLES {
        fs::write(managed_styles.join(style.file_name), style.content).unwrap();
    }
    let stale = managed_styles.join(crate::core::bundle::OUTPUT_STYLES[0].file_name);
    fs::write(&stale, "stale managed copy").unwrap();

    let steps = repair_output_style(home.path(), Some(managed.path()), RepairMode::Apply);

    assert_eq!(steps.len(), 1, "only the managed copy drifted: {steps:?}");
    assert!(steps[0].changed(), "{steps:?}");
    assert!(
        steps[0].what.contains("managed config"),
        "the step must name the tier it wrote: {steps:?}"
    );
    assert_eq!(
        fs::read_to_string(&stale).unwrap(),
        crate::core::bundle::OUTPUT_STYLES[0].content,
        "the managed copy must be rewritten from the bundled asset"
    );
    assert!(
        repair_output_style(home.path(), Some(managed.path()), RepairMode::DryRun).is_empty(),
        "the repair must actually clear the finding it reported"
    );
}

/// A write that succeeded must report as a write, whatever a sibling did.
///
/// Why (#5866): the repair took the deploy's whole-batch `Err` as every step's
/// status, so one unreadable style made a DIFFERENT style read `Failed` after
/// being correctly rewritten to bundled content on disk. That is #5865's
/// complaint — a report that contradicts the disk, leaving an operator re-running
/// `tm doctor --fix` against a status that will not move — reintroduced by the
/// #5866 fix.
/// What: drifts `OUTPUT_STYLES[0]`, makes `OUTPUT_STYLES[1]` unreadable, applies
/// the repair, and asserts the drifted step is `Applied` with bundled content on
/// disk while only the unreadable one is refused.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn one_unreadable_style_does_not_fail_a_sibling_that_was_written() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    deploy_styles(home.path());
    let drifted = style_path(home.path(), 0);
    let blocked = style_path(home.path(), 1);
    fs::write(&drifted, "stale text").unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&blocked).is_ok() {
        eprintln!("skipping: cannot deny read on this platform/privilege level");
        return;
    }

    let steps = repair_output_style(home.path(), None, RepairMode::Apply);
    let _ = fs::set_permissions(&blocked, fs::Permissions::from_mode(0o600));

    let drifted_step = steps
        .iter()
        .find(|s| s.path == drifted)
        .unwrap_or_else(|| panic!("the drifted style must produce a step: {steps:?}"));
    assert_eq!(
        drifted_step.status,
        StepStatus::Applied { backup: None },
        "a style that was rewritten must report as written: {steps:?}"
    );
    assert_eq!(
        fs::read_to_string(&drifted).unwrap(),
        crate::core::bundle::OUTPUT_STYLES[0].content,
        "and the report must match what is on disk"
    );

    let blocked_step = steps
        .iter()
        .find(|s| s.path == blocked)
        .unwrap_or_else(|| panic!("the unreadable style must produce a step: {steps:?}"));
    assert!(
        matches!(blocked_step.status, StepStatus::Refused(_)),
        "the unreadable style owns its own outcome: {steps:?}"
    );
    assert!(
        !steps
            .iter()
            .any(|s| matches!(s.status, StepStatus::Failed(_))),
        "no step may inherit a sibling's failure: {steps:?}"
    );
}

/// Fail-open guard: a read failure is not evidence of staleness, so the repair
/// refuses rather than overwriting a file it could not inspect.
#[cfg(unix)]
#[test]
fn output_style_repair_refuses_an_unreadable_file() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    deploy_styles(home.path());
    let target = style_path(home.path(), 0);
    fs::set_permissions(&target, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&target).is_ok() {
        eprintln!("skipping: cannot deny read on this platform/privilege level");
        return;
    }

    let steps = repair_output_style(home.path(), None, RepairMode::Apply);
    let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o600));

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(
        matches!(steps[0].status, StepStatus::Refused(_)),
        "an unreadable file must be refused, never overwritten: {steps:?}"
    );
    assert!(!steps[0].changed());
}

/// The #7490 incident shape: `SessionStart` wired to the memory hook only,
/// with the PM guard present so the file reads as tm-provisioned.
fn write_missing_group_settings(project: &Path) -> PathBuf {
    let claude = project.join(".claude");
    fs::create_dir_all(&claude).unwrap();
    let path = claude.join("settings.json");
    fs::write(
        &path,
        serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/tm hook --pm-guard" }] }
                ],
                "SessionStart": [
                    { "hooks": [{ "type": "command", "command": "trusty-memory inbox-check" }] }
                ]
            }
        })
        .to_string(),
    )
    .unwrap();
    path
}

/// The `--fix --yes` arm merges the missing lifecycle group back in (#7490).
#[test]
fn missing_group_repair_merges_the_sessionstart_group_back() {
    let project = tempfile::tempdir().unwrap();
    let path = write_missing_group_settings(project.path());

    let steps = repair_missing_hook_group_with(
        project.path(),
        Some(Path::new("/usr/local/bin/tm")),
        RepairMode::Apply,
    );
    assert_eq!(
        steps.len(),
        1,
        "one step for the one gapped file: {steps:?}"
    );
    assert_eq!(steps[0].check, "hooks_missing_tm_group");
    assert!(
        matches!(steps[0].status, StepStatus::Applied { .. }),
        "the merge must apply: {steps:?}"
    );

    let after: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let text = after["hooks"]["SessionStart"].to_string();
    assert!(
        text.contains("/usr/local/bin/tm hook\""),
        "SessionStart must gain the lifecycle entry: {text}"
    );
    assert!(
        text.contains("trusty-memory inbox-check"),
        "the project's own entry must survive: {text}"
    );
}

/// The dry run reports the gap and writes nothing (#7490).
#[test]
fn missing_group_repair_dry_run_changes_nothing() {
    let project = tempfile::tempdir().unwrap();
    let path = write_missing_group_settings(project.path());
    let before = fs::read(&path).unwrap();

    let steps = repair_missing_hook_group(project.path(), RepairMode::DryRun);
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].status, StepStatus::Planned);
    assert!(
        steps[0].what.contains("SessionStart"),
        "the preview must name the event: {}",
        steps[0].what
    );
    assert_eq!(
        before,
        fs::read(&path).unwrap(),
        "a dry run must not touch the file"
    );
}

/// The driver's repair ORDER leaves a healthy project in the state a launch
/// would write (#7490).
///
/// Why: `repair_hooks_contamination` strips every `<exe> hook` entry it finds,
/// which on a tm-provisioned project IS the lifecycle triad. The comments in
/// `core::doctor_repair` and `bin/tm/commands/doctor_repair::run_repairs` both
/// claim the re-merge runs after that strip precisely so the strip cannot undo
/// it — a claim nothing tested. Reversing the two calls, or dropping the
/// second, must fail here.
/// What: seeds a settings file carrying the full triad (produced by the merge
/// itself), then runs the two repairs in Apply mode in the SAME order
/// `run_repairs` uses, and asserts the resulting bytes equal what
/// `ensure_project_hooks` alone produces for the same input.
/// Test: itself.
#[test]
fn contamination_strip_then_missing_group_merge_restores_the_launch_state() {
    let exe = Some(Path::new("/usr/local/bin/tm"));

    // The expected state: one project taken straight to the merged form.
    let expected_project = tempfile::tempdir().unwrap();
    let expected_path = write_missing_group_settings(expected_project.path());
    crate::core::session_launch::ensure_project_hooks(expected_project.path(), exe).unwrap();
    let expected = fs::read(&expected_path).unwrap();

    // The driver's path: identical input, strip first, then re-merge.
    let actual_project = tempfile::tempdir().unwrap();
    let actual_path = write_missing_group_settings(actual_project.path());
    crate::core::session_launch::ensure_project_hooks(actual_project.path(), exe).unwrap();
    let strip = repair_hooks_contamination(actual_project.path(), RepairMode::Apply);
    assert!(
        !strip.is_empty(),
        "the fixture must actually carry entries the strip removes, or this \
         test proves nothing about the ordering: {strip:?}"
    );
    let merge = repair_missing_hook_group_with(actual_project.path(), exe, RepairMode::Apply);
    assert!(
        matches!(
            merge.first().map(|s| &s.status),
            Some(StepStatus::Applied { .. })
        ),
        "the re-merge must run after the strip and apply: {merge:?}"
    );

    assert_eq!(
        String::from_utf8_lossy(&expected),
        String::from_utf8_lossy(&fs::read(&actual_path).unwrap()),
        "strip-then-merge must land on the state a managed launch writes"
    );
}

/// A file with no gap produces no step at all (#7490).
#[test]
fn missing_group_repair_is_silent_for_a_complete_file() {
    let project = tempfile::tempdir().unwrap();
    write_missing_group_settings(project.path());
    crate::core::session_launch::ensure_project_hooks(
        project.path(),
        Some(Path::new("/usr/local/bin/tm")),
    )
    .unwrap();

    let steps = repair_missing_hook_group(project.path(), RepairMode::DryRun);
    assert!(
        steps.is_empty(),
        "a complete file must not produce a repair line: {steps:?}"
    );
}

/// Why (#7617, #5866's lesson): the new `statusline` check's remediation line
/// names `tm doctor --fix`, so the step it names has to exist and has to write.
/// Test: itself.
#[test]
fn statusline_repair_seeds_a_missing_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".claude").join("settings.json");

    let steps = repair_statusline(std::slice::from_ref(&path), RepairMode::Apply);

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].check, "statusline");
    assert!(steps[0].changed(), "{:?}", steps[0].status);
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value["statusLine"]["type"], "command");
}

/// Why (critic MEDIUM 4): the module's rule is "back up before overwriting",
/// and `repair_hooks_contamination` reports the path an operator undoes from.
/// This step claimed `Applied { backup: None }` with no backup anywhere in the
/// chain, so a repoint of the operator's own `~/.claude/settings.json` was
/// unrecoverable.
/// FAILS BEFORE THIS CHANGE: `f8a8a2fb8` reported `backup: None` and wrote no
/// `.bak`.
/// Test: itself.
#[test]
fn statusline_repair_backs_up_before_repointing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "statusLine": {
                "type": "command",
                "command": "/definitely/not/here/tm statusline",
                "padding": 0
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let steps = repair_statusline(std::slice::from_ref(&path), RepairMode::Apply);

    assert_eq!(steps.len(), 1, "{steps:?}");
    let StepStatus::Applied { backup: Some(bak) } = &steps[0].status else {
        panic!("expected a backup path, got {:?}", steps[0].status);
    };
    assert!(
        fs::read_to_string(bak)
            .unwrap()
            .contains("/definitely/not/here/tm statusline"),
        "the backup must carry what was repointed"
    );
}

/// Why: a SEEDED brand-new settings file had no prior bytes, so there is nothing
/// to back up — and reporting a backup that does not exist would send an
/// operator to a path that is not there.
/// Test: itself.
#[test]
fn statusline_repair_reports_no_backup_when_it_created_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".claude").join("settings.json");

    let steps = repair_statusline(std::slice::from_ref(&path), RepairMode::Apply);

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].status, StepStatus::Applied { backup: None });
}

/// Why (the module's rule 1): a bare `--fix` describes and writes nothing.
/// Test: itself.
#[test]
fn statusline_repair_dry_run_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".claude").join("settings.json");

    let steps = repair_statusline(std::slice::from_ref(&path), RepairMode::DryRun);

    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].status, StepStatus::Planned);
    assert!(!path.exists(), "dry run must write nothing");
}

/// Why: a healthy machine must grow no output — a repair line per correct file
/// is noise that trains an operator to skip the whole section.
/// Test: itself.
#[test]
fn statusline_repair_is_silent_for_a_wired_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    // `/bin/sh` exists and is not an ephemeral build path; this test binary's
    // own `current_exe()` is one, so it would read as stale (#2229).
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "statusLine": { "type": "command", "command": "/bin/sh statusline", "padding": 0 }
        }))
        .unwrap(),
    )
    .unwrap();

    let steps = repair_statusline(std::slice::from_ref(&path), RepairMode::Apply);

    assert!(steps.is_empty(), "{steps:?}");
}

/// Why (the module's rule 2, and the Fail-Open Check): a settings file that
/// cannot be parsed is the operator's, and rewriting it from `{}` to fix a
/// status bar would delete every other key in it.
/// Test: itself.
#[test]
fn statusline_repair_refuses_unparseable_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, "{ not json").unwrap();

    let steps = repair_statusline(std::slice::from_ref(&path), RepairMode::Apply);

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(
        matches!(steps[0].status, StepStatus::Refused(_)),
        "{:?}",
        steps[0].status
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
}
