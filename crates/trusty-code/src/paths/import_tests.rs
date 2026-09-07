//! Unit tests for the `.claude/` → `.trusty-code/` import (#5426).
//!
//! Why: the import's guarantees — deterministic, non-overwriting, reversible,
//! and refusing four classes of unsafe source — are the ones a project owner
//! bets their repository on.
//! What: plan determinism, the empty case, the four refusals, and an applied run
//! that creates exactly the planned files and nothing else.
//! Test: this file IS the test module.

use super::*;

use crate::paths::TRUSTY_CODE_DIRNAME;

/// Write `<root>/.claude/<relative>`, creating parents.
fn write_claude(root: &Path, relative: &str, body: &str) -> PathBuf {
    let path = root.join(CLAUDE_COMPAT_DIRNAME).join(relative);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, body).expect("write");
    path
}

/// Every file created under `<root>/.trusty-code`, sorted.
fn native_tree(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(&root.join(TRUSTY_CODE_DIRNAME), &mut out);
    out.sort();
    out
}

/// The plan is sorted by target and stable across runs.
///
/// Why: `--dry-run` is only trustworthy if the real run does the same work in
/// the same order; a plan whose order came from `read_dir` would vary by
/// filesystem.
/// What: stages agents, a nested skill, and settings; asserts the plan is
/// sorted, covers every file, and is byte-identical across two calls.
/// Test: this function IS the test.
#[test]
fn plan_is_sorted_and_deterministic() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_claude(tmp.path(), "agents/pm.md", "# pm");
    write_claude(tmp.path(), "agents/engineer.md", "# engineer");
    write_claude(tmp.path(), "skills/demo/SKILL.md", "# demo");
    write_claude(tmp.path(), "skills/demo/references/deep.md", "# deep");
    write_claude(
        tmp.path(),
        "settings.json",
        r#"{"code_harness":{"mode":"parity"}}"#,
    );

    let plan = plan_import(tmp.path());
    let again = plan_import(tmp.path());

    assert_eq!(plan, again, "the plan must be a function of the tree alone");
    assert_eq!(plan.entries.len(), 5);
    assert!(
        plan.entries.windows(2).all(|w| w[0].to <= w[1].to),
        "entries must be sorted by target path"
    );
    assert_eq!(plan.to_copy().count(), 5);
    assert!(
        plan.entries
            .iter()
            .all(|e| e.to.starts_with(tmp.path().join(TRUSTY_CODE_DIRNAME))),
        "every target must land beneath .trusty-code/"
    );
}

/// A project with no `.claude/` plans nothing and errors on nothing.
#[test]
fn missing_claude_dir_yields_empty_plan() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert_eq!(plan_import(tmp.path()), ImportPlan::default());
}

/// Applying a plan creates exactly its `Copy` targets and nothing else.
///
/// Why: reversibility means the report is a complete inventory — anything
/// created but unreported could not be undone.
/// What: applies a staged plan and asserts the on-disk `.trusty-code/` tree
/// equals the reported `created` list, with identical file contents.
/// Test: this function IS the test.
#[test]
fn apply_creates_only_the_planned_files() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_claude(tmp.path(), "agents/pm.md", "# pm");
    write_claude(tmp.path(), "skills/demo/SKILL.md", "# demo");

    let plan = plan_import(tmp.path());
    let report = apply_import(&plan);

    assert!(report.refused.is_empty(), "refused: {:?}", report.refused);
    assert_eq!(report.created, native_tree(tmp.path()));
    assert_eq!(report.created.len(), 2);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join(TRUSTY_CODE_DIRNAME).join("agents/pm.md"))
            .expect("read"),
        "# pm"
    );
}

/// A second import overwrites nothing and reports every skip.
///
/// Why: the non-overwrite rule has to hold for files THIS import created too,
/// or re-running it would silently discard edits made after the first run.
/// What: imports twice; the second plan copies nothing and refuses both files.
/// Test: this function IS the test.
#[test]
fn apply_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_claude(tmp.path(), "agents/pm.md", "# pm");
    apply_import(&plan_import(tmp.path()));

    // Edit the imported copy: a re-import must not clobber it.
    let target = tmp.path().join(TRUSTY_CODE_DIRNAME).join("agents/pm.md");
    std::fs::write(&target, "# edited by the project owner").expect("write");

    let second = plan_import(tmp.path());
    let report = apply_import(&second);

    assert_eq!(second.to_copy().count(), 0);
    assert!(report.created.is_empty());
    assert_eq!(report.refused.len(), 1);
    assert!(report.refused[0].1.contains("already exists"));
    assert_eq!(
        std::fs::read_to_string(&target).expect("read"),
        "# edited by the project owner"
    );
}

/// An existing target is never overwritten, even on a first run.
#[test]
fn existing_target_is_never_overwritten() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_claude(tmp.path(), "agents/pm.md", "# from claude");
    let target = tmp.path().join(TRUSTY_CODE_DIRNAME).join("agents/pm.md");
    std::fs::create_dir_all(target.parent().expect("parent")).expect("mkdir");
    std::fs::write(&target, "# authored natively").expect("write");

    let plan = plan_import(tmp.path());

    assert_eq!(plan.entries.len(), 1);
    assert!(matches!(plan.entries[0].action, ImportAction::Refuse(_)));
    assert_eq!(
        std::fs::read_to_string(&target).expect("read"),
        "# authored natively"
    );
}

/// A source carrying the executable bit is refused.
///
/// Why: an executable imported into `.trusty-code/` inherits the trust every
/// later consumer of that tree extends to it — provenance this import cannot
/// vouch for.
/// What: chmods a staged agent file to `0o755` and asserts the refusal.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn executable_source_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().expect("tempdir");
    let src = write_claude(tmp.path(), "agents/hook.md", "#!/bin/sh\nrm -rf /\n");
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let plan = plan_import(tmp.path());
    let report = apply_import(&plan);

    assert_eq!(plan.to_copy().count(), 0);
    assert!(report.created.is_empty());
    assert_eq!(report.refused.len(), 1);
    assert!(
        report.refused[0].1.contains("executable"),
        "reason was: {}",
        report.refused[0].1
    );
}

/// A symlink pointing out of `.claude/` is refused.
///
/// Why: `.claude/agents/leak.md -> ~/.ssh/id_rsa` would otherwise be copied into
/// a directory the project commits.
/// What: symlinks a file outside the project into `.claude/agents/` and asserts
/// nothing is copied.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn symlink_escaping_claude_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("tempdir");
    let secret = outside.path().join("id_rsa");
    std::fs::write(&secret, "PRIVATE KEY").expect("write");

    let agents = tmp.path().join(CLAUDE_COMPAT_DIRNAME).join("agents");
    std::fs::create_dir_all(&agents).expect("mkdir");
    std::os::unix::fs::symlink(&secret, agents.join("leak.md")).expect("symlink");

    let plan = plan_import(tmp.path());
    let report = apply_import(&plan);

    assert_eq!(plan.entries.len(), 1);
    assert_eq!(plan.to_copy().count(), 0);
    assert!(report.created.is_empty());
    assert!(
        report.refused[0].1.contains("symlink"),
        "reason was: {}",
        report.refused[0].1
    );
    assert!(
        !tmp.path()
            .join(TRUSTY_CODE_DIRNAME)
            .join("agents/leak.md")
            .exists()
    );
}

/// A settings file carrying a credential is refused.
///
/// Why: `.trusty-code/` is project-owned and conventionally committed; a
/// credential copied there is a leak with a long tail.
/// What: stages `.claude/settings.json` with an `OPENROUTER_API_KEY` and asserts
/// the refusal names the offending key.
/// Test: this function IS the test.
#[test]
fn secret_bearing_settings_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_claude(
        tmp.path(),
        "settings.json",
        r#"{"env":{"OPENROUTER_API_KEY":"sk-live-xyz"}}"#,
    );

    let plan = plan_import(tmp.path());
    let report = apply_import(&plan);

    assert!(report.created.is_empty());
    assert_eq!(report.refused.len(), 1);
    assert!(
        report.refused[0].1.contains("OPENROUTER_API_KEY"),
        "reason was: {}",
        report.refused[0].1
    );
    assert!(
        !tmp.path()
            .join(TRUSTY_CODE_DIRNAME)
            .join("settings.json")
            .exists()
    );
}

/// Settings that will not parse are refused rather than assumed clean.
///
/// Why: an unparseable file is not evidence of the absence of a secret.
/// What: stages malformed JSON and asserts nothing is copied.
/// Test: this function IS the test.
#[test]
fn unparseable_settings_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_claude(tmp.path(), "settings.json", "{ not json");

    let plan = plan_import(tmp.path());

    assert_eq!(plan.to_copy().count(), 0);
    assert!(matches!(plan.entries[0].action, ImportAction::Refuse(_)));
}

/// An unreadable subtree loses its own files, not the whole import.
///
/// Why: the Fail-Open Check applied to the import walk — one bad directory must
/// not strand the agents that were readable.
/// What: chmods `.claude/skills` to `0o000` and asserts the agents still import.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn unreadable_subtree_is_skipped_with_a_warning() {
    use std::os::unix::fs::PermissionsExt;

    // SAFETY: `geteuid` takes no arguments, touches no memory, and cannot fail.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    write_claude(tmp.path(), "agents/pm.md", "# pm");
    let skills = write_claude(tmp.path(), "skills/demo/SKILL.md", "# demo")
        .parent()
        .expect("parent")
        .parent()
        .expect("skills")
        .to_path_buf();
    std::fs::set_permissions(&skills, std::fs::Permissions::from_mode(0o000)).expect("chmod");

    let plan = plan_import(tmp.path());

    std::fs::set_permissions(&skills, std::fs::Permissions::from_mode(0o755)).expect("restore");

    assert_eq!(plan.to_copy().count(), 1, "the readable agent must survive");
    assert!(plan.entries[0].to.ends_with("agents/pm.md"));
}
