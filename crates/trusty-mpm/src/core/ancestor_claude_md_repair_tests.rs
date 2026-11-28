//! Tests for the ancestor memory-file repairs (#7673).
//!
//! Split out with `#[path]` so `ancestor_claude_md_repair.rs` stays under the
//! 500-SLOC production cap.

use super::*;
use crate::core::instruction_pipeline::CLAUDE_MD_STUB;
use tempfile::TempDir;

/// `<tmp>/ancestor/project`, canonicalised.
// #8838: `project` is a registered project, so the nearest-boundary walk stops
// there whatever a shared temp root above the fixture holds.
fn fixture() -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let ancestor = root.join("ancestor");
    let project = ancestor.join("project");
    std::fs::create_dir_all(project.join(".trusty-mpm")).unwrap();
    (tmp, ancestor, project)
}

fn settings_of(project: &Path) -> PathBuf {
    project.join(".claude").join("settings.local.json")
}

#[test]
fn the_check_name_matches_the_doctor_row() {
    assert_eq!(ANCESTOR_CHECK, "ancestor_claude_md");
}

/// FAILS BEFORE THIS CHANGE: `--fix` had no step for this finding at all.
#[test]
fn a_seed_ancestor_is_renamed_aside() {
    let (_tmp, ancestor, project) = fixture();
    let seed = ancestor.join("CLAUDE.md");
    std::fs::write(&seed, CLAUDE_MD_STUB).unwrap();
    // The project's own CLAUDE.md must be untouched by every repair here.
    let own = project.join("CLAUDE.md");
    std::fs::write(&own, CLAUDE_MD_STUB).unwrap();

    let steps = repair_ancestor_claude_md(&project, None, None, "20260912", RepairMode::Apply);

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(
        matches!(steps[0].status, StepStatus::Applied { .. }),
        "{steps:?}"
    );
    assert!(!seed.exists(), "the seed is renamed out of the loaded name");
    assert_eq!(
        std::fs::read_to_string(ancestor.join("CLAUDE.md.stale-seed-20260912")).unwrap(),
        CLAUDE_MD_STUB
    );
    assert_eq!(
        std::fs::read_to_string(&own).unwrap(),
        CLAUDE_MD_STUB,
        "a file INSIDE the project root is never touched"
    );
}

/// FAILS BEFORE THIS CHANGE: there was no `claudeMdExcludes` repair, and the
/// briefed rename would have moved a file carrying the operator's own notes.
#[test]
fn a_content_ancestor_is_excluded_not_renamed() {
    let (_tmp, ancestor, project) = fixture();
    let notes = ancestor.join("CLAUDE.md");
    std::fs::write(&notes, "# Monorepo\n\nAll packages use pnpm.\n").unwrap();

    let steps = repair_ancestor_claude_md(&project, None, None, "20260912", RepairMode::Apply);

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(
        matches!(steps[0].status, StepStatus::Applied { .. }),
        "{steps:?}"
    );
    assert!(notes.is_file(), "a file with real content is never renamed");
    // #12077 row 13: on this project's launch-settings record.
    assert_eq!(
        recorded_excludes(&project),
        serde_json::json!([notes.display().to_string()])
    );
}

#[test]
fn dry_run_writes_nothing() {
    let (_tmp, ancestor, project) = fixture();
    let seed = ancestor.join("CLAUDE.md");
    std::fs::write(&seed, CLAUDE_MD_STUB).unwrap();
    std::fs::write(ancestor.join("CLAUDE.local.md"), "# notes\n").unwrap();
    let managed = ancestor.parent().unwrap().join("claude-config");

    let steps = repair_ancestor_claude_md(
        &project,
        None,
        Some(&managed),
        "20260912",
        RepairMode::DryRun,
    );

    assert_eq!(steps.len(), 2, "{steps:?}");
    assert!(
        steps.iter().all(|s| s.status == StepStatus::Planned),
        "{steps:?}"
    );
    assert!(
        seed.is_file(),
        "dry run leaves the seed exactly where it was"
    );
    assert!(
        !settings_of(&project).exists() && !managed.join("settings.json").exists(),
        "dry run seeds no settings file"
    );
    assert!(
        !crate::core::session_launch::launch_keys_record_path(&project).exists(),
        "dry run writes no launch-settings record"
    );
}

#[test]
fn an_already_excluded_ancestor_yields_no_step() {
    let (_tmp, ancestor, project) = fixture();
    let notes = ancestor.join("CLAUDE.md");
    std::fs::write(&notes, "# Monorepo\n").unwrap();
    let settings = settings_of(&project);
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        serde_json::json!({ "claudeMdExcludes": [notes.display().to_string()] }).to_string(),
    )
    .unwrap();

    let steps = repair_ancestor_claude_md(&project, None, None, "20260912", RepairMode::Apply);

    assert!(steps.is_empty(), "{steps:?}");
}

/// `git -C <dir> init -q`, reporting whether git was available at all.
fn git_init(dir: &Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["init", "-q"])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// #7673 round 2: `--fix` run from a subdirectory of a git repository must not
/// exclude the repository's OWN root `CLAUDE.md` — the nearest boundary is the
/// repository root, so that file is never a finding and nothing is written.
#[test]
fn no_exclude_is_written_for_the_git_roots_own_claude_md_from_a_nested_project_root() {
    let tmp = TempDir::new().unwrap();
    let repo = std::fs::canonicalize(tmp.path()).unwrap().join("repo");
    let nested = repo.join("crates").join("thing");
    std::fs::create_dir_all(&nested).unwrap();
    if !git_init(&repo) {
        eprintln!("#7673 tests: git unavailable, skipping");
        return;
    }
    std::fs::write(repo.join("CLAUDE.md"), "# Root project\n\nUse cargo.\n").unwrap();

    let steps = repair_ancestor_claude_md(&nested, None, None, "20260912", RepairMode::Apply);

    assert!(steps.is_empty(), "{steps:?}");
    assert!(
        !settings_of(&nested).exists(),
        "no exclude may be written at all, let alone at the nested directory"
    );
    assert!(
        !settings_of(&repo).exists(),
        "the root's own CLAUDE.md is the project's, not a finding — nothing to exclude"
    );
}

/// FAILS AGAINST 89f6204e4 (#7673): `--fix` run from a linked worktree nested
/// inside its main checkout treated the checkout's own `CLAUDE.md` as a stray
/// and wrote an exclude for it into the worktree's settings. The worktree
/// resolves to its main checkout, so there is no finding and nothing is written.
#[test]
fn fix_from_a_nested_linked_worktree_writes_no_exclude() {
    let tmp = TempDir::new().unwrap();
    let repo = std::fs::canonicalize(tmp.path()).unwrap().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |dir: &Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "git {args:?}: {stderr}");
    };
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "T"]);
    std::fs::write(repo.join("CLAUDE.md"), "# Repo\n\nUse cargo.\n").unwrap();
    git(&repo, &["add", "CLAUDE.md"]);
    git(&repo, &["commit", "-qm", "init"]);
    let wt = repo.join(".claude").join("worktrees").join("wt");
    std::fs::create_dir_all(wt.parent().unwrap()).unwrap();
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
    );

    let steps = repair_ancestor_claude_md(&wt, None, None, "20260912", RepairMode::Apply);

    assert!(steps.is_empty(), "{steps:?}");
    assert!(
        !settings_of(&wt).exists(),
        "no exclude in the worktree's settings"
    );
    assert!(
        !settings_of(&repo).exists(),
        "no exclude in the checkout's settings"
    );
}

/// Regression case 6 — FAILS AGAINST ROUND 3: under a dotfiles `$HOME` repo, a
/// marker project resolved to `$HOME`, so `--fix` found nothing to repair. The
/// exclude for `$HOME/CLAUDE.md` must land on the RESOLVED root's
/// launch-settings record (#12077 row 13) — `myproject`, the nearest boundary —
/// and not on the subdirectory `--fix` ran from, nor at `$HOME`.
#[test]
fn the_exclude_from_a_subdirectory_lands_on_the_resolved_roots_record() {
    let tmp = TempDir::new().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap().join("home");
    let project = home.join("projects").join("myproject");
    let cwd = project.join("src").join("deep");
    std::fs::create_dir_all(project.join(".trusty-mpm")).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    if !git_init(&home) {
        eprintln!("#7673 tests: git unavailable, skipping");
        return;
    }
    let file = home.join("CLAUDE.md");
    std::fs::write(&file, "# Dotfiles\n\nMy shell notes.\n").unwrap();

    let managed = tmp.path().join("claude-config");

    let steps =
        repair_ancestor_claude_md(&cwd, None, Some(&managed), "20260912", RepairMode::Apply);

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(
        matches!(steps[0].status, StepStatus::Applied { .. }),
        "{steps:?}"
    );
    // #12077 row 13: the resolved root's record, never a settings file.
    assert!(
        !settings_of(&project).exists(),
        "never the project's settings"
    );
    assert!(
        !managed.join("settings.json").exists(),
        "never tm's managed dir"
    );
    assert_eq!(
        recorded_excludes(&project),
        serde_json::json!([file.display().to_string()])
    );
    assert_eq!(
        recorded_excludes(&cwd),
        serde_json::Value::Null,
        "never the cwd's record"
    );
    assert!(!settings_of(&cwd).exists(), "never the cwd's settings");
    assert!(!settings_of(&home).exists(), "never $HOME's settings");
    assert!(file.is_file(), "a content file is never renamed");
}

/// FAILS AGAINST ROUND 3: a project root that does not exist was scanned as if
/// it did, and `--fix` acted on the would-be ancestors of a typo. It is one
/// `Failed` step naming the path, and nothing is touched.
#[test]
fn a_missing_project_root_is_a_failed_step() {
    let (_tmp, ancestor, _project) = fixture();
    std::fs::write(ancestor.join("CLAUDE.md"), CLAUDE_MD_STUB).unwrap();
    let missing = ancestor.join("does-not-exist");

    let steps = repair_ancestor_claude_md(&missing, None, None, "20260912", RepairMode::Apply);

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(
        matches!(&steps[0].status, StepStatus::Failed(why) if why.contains("does-not-exist")),
        "{steps:?}"
    );
    assert!(ancestor.join("CLAUDE.md").is_file(), "nothing is renamed");
}

/// A second `--fix` on the same day must not overwrite the first run's rescue
/// copy, which would destroy the only remaining bytes of the original.
#[test]
fn a_rename_target_that_exists_is_refused() {
    let (_tmp, ancestor, project) = fixture();
    std::fs::write(ancestor.join("CLAUDE.md"), CLAUDE_MD_STUB).unwrap();
    std::fs::write(
        ancestor.join("CLAUDE.md.stale-seed-20260912"),
        "an earlier rescue copy\n",
    )
    .unwrap();

    let steps = repair_ancestor_claude_md(&project, None, None, "20260912", RepairMode::Apply);

    assert_eq!(steps.len(), 1);
    assert!(
        matches!(steps[0].status, StepStatus::Refused(_)),
        "{steps:?}"
    );
    assert_eq!(
        std::fs::read_to_string(ancestor.join("CLAUDE.md.stale-seed-20260912")).unwrap(),
        "an earlier rescue copy\n"
    );
}

/// The `claudeMdExcludes` the launch-settings record of `project` carries.
fn recorded_excludes(project: &Path) -> serde_json::Value {
    crate::core::session_launch::read_launch_keys(project)
        .unwrap()
        .and_then(|keys| keys.get("claudeMdExcludes").cloned())
        .unwrap_or(serde_json::Value::Null)
}

/// #12077 row 13: the exclude goes on this project's launch-settings record,
/// which the launch passes as `--settings`; tm's managed config dir, shared by
/// every tm session, is not written.
#[test]
fn the_exclude_lands_on_the_launch_settings_record_12077() {
    let (tmp, ancestor, project) = fixture();
    let notes = ancestor.join("CLAUDE.md");
    std::fs::write(&notes, "# Monorepo\n\nAll packages use pnpm.\n").unwrap();
    let managed = tmp.path().join("claude-config");
    std::fs::create_dir_all(&managed).unwrap();

    let steps = repair_ancestor_claude_md(
        &project,
        None,
        Some(&managed),
        "20260912",
        RepairMode::Apply,
    );

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(
        matches!(steps[0].status, StepStatus::Applied { .. }),
        "{steps:?}"
    );
    assert_eq!(
        recorded_excludes(&project),
        serde_json::json!([notes.display().to_string()])
    );
    assert!(
        !managed.join("settings.json").exists(),
        "the managed config dir is shared by every tm session; not written"
    );
    assert!(!settings_of(&project).exists(), "no project settings file");
}

/// #12077 row 13: the record is tm's own, so the exclude needs no managed
/// config dir.
#[test]
fn the_exclude_needs_no_managed_config_dir_12077() {
    let (_tmp, ancestor, project) = fixture();
    let notes = ancestor.join("CLAUDE.md");
    std::fs::write(&notes, "# Monorepo\n").unwrap();

    let steps = repair_ancestor_claude_md(&project, None, None, "20260912", RepairMode::Apply);

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(
        matches!(steps[0].status, StepStatus::Applied { .. }),
        "{steps:?}"
    );
    assert_eq!(
        recorded_excludes(&project),
        serde_json::json!([notes.display().to_string()])
    );
}

/// #12077 row 13: an exclude on the record counts as excluded, so a second
/// `--fix` has nothing to do.
#[test]
fn a_recorded_exclude_yields_no_step_12077() {
    let (_tmp, ancestor, project) = fixture();
    let notes = ancestor.join("CLAUDE.md");
    std::fs::write(&notes, "# Monorepo\n").unwrap();
    crate::core::session_launch::set_launch_key(
        &project,
        "claudeMdExcludes",
        serde_json::json!([notes.display().to_string()]),
    )
    .unwrap();

    let steps = repair_ancestor_claude_md(&project, None, None, "20260912", RepairMode::Apply);

    assert!(steps.is_empty(), "{steps:?}");
}

/// #12077 row 13, Fail-Open Check: a record that cannot be read fails the
/// step and stays byte-identical; nothing else is written in its place.
#[test]
fn an_unreadable_launch_record_fails_the_exclude_12077() {
    let (tmp, ancestor, project) = fixture();
    std::fs::write(ancestor.join("CLAUDE.md"), "# Monorepo\n").unwrap();
    let managed = tmp.path().join("claude-config");
    std::fs::create_dir_all(&managed).unwrap();
    let record = crate::core::session_launch::launch_keys_record_path(&project);
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    std::fs::write(&record, "{ not json").unwrap();

    let steps = repair_ancestor_claude_md(
        &project,
        None,
        Some(&managed),
        "20260912",
        RepairMode::Apply,
    );

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(
        matches!(&steps[0].status, StepStatus::Failed(why) if why.contains("not valid")),
        "{steps:?}"
    );
    assert_eq!(std::fs::read_to_string(&record).unwrap(), "{ not json");
    assert!(!managed.join("settings.json").exists());
}
