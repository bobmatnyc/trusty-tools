//! Tests for the scaffolding path list (#3427, #7762, #7932, #8533, #8758).
//!
//! Split out with `#[path]` so `scaffold_gitignore.rs` stays small. The paths
//! reach git through `.git/info/exclude` now, so the behaviour tests ask git
//! itself, with that file written by the launch's own function.

use super::*;
use std::path::Path;

/// A REAL repository, for the tests that ask git itself about ignore status.
fn real_git_repo(dir: &Path) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["init", "-q"])
        .output()
        .expect("git must be runnable — this crate shells out to it everywhere");
    assert!(out.status.success(), "git init failed: {out:?}");
}

/// `git check-ignore` on a path, with the operator's global excludes muted.
fn is_ignored(repo: &Path, relative: &str) -> bool {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "core.excludesFile=/dev/null",
            "check-ignore",
            "--no-index",
            "-q",
            "--",
            relative,
        ])
        .output()
        .expect("git check-ignore must run");
    match out.status.code() {
        Some(0) => true,
        Some(1) => false,
        other => panic!(
            "git check-ignore exited {other:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

#[test]
fn block_covers_session_output_but_not_project_config() {
    // #4832: `tm` writes per-session output into `.trusty-mpm/` on every
    // launch, so the list must name it — but must NOT swallow the
    // operator-authored `framework/manifest.toml` or `config.toml`.
    for covered in [
        ".trusty-mpm/sessions/",
        ".trusty-mpm/logs/",
        ".trusty-mpm/last-instructions.md",
    ] {
        assert!(
            SCAFFOLD_IGNORED_PATHS.contains(&covered),
            "missing {covered} in {SCAFFOLD_IGNORED_PATHS:?}"
        );
    }
    for spared in [
        ".claude/",
        ".claude/skills/",
        ".trusty-mpm/",
        ".trusty-mpm/framework/",
        ".trusty-mpm/config.toml",
        // #5207: the committed project config MUST stay trackable — being
        // reviewable in a PR is the entire reason it exists.
        crate::core::project_config::PROJECT_CONFIG_FILE,
    ] {
        assert!(
            !SCAFFOLD_IGNORED_PATHS.contains(&spared),
            "{spared} is operator config and must stay trackable"
        );
    }
}

/// The two lock entries are spelled by `settings_lock`, not by hand (#7762).
///
/// Why: the sidecar name is decided in `settings_lock::lock_sidecar` and
/// written here as a literal. This is what stops the two drifting — if the
/// sidecar suffix ever changes, this fails instead of the operator's
/// `git status` quietly growing an untracked file.
#[test]
fn lock_entries_match_the_settings_lock_sidecar() {
    for settings in [".claude/settings.json", ".claude/settings.local.json"] {
        let sidecar = crate::core::settings_lock::lock_sidecar(Path::new(settings));
        let expected = sidecar.to_str().expect("a UTF-8 fixture path");
        assert!(
            SCAFFOLD_IGNORED_PATHS.contains(&expected),
            "{expected} must be in SCAFFOLD_IGNORED_PATHS: {SCAFFOLD_IGNORED_PATHS:?}"
        );
    }
    // The guarded files themselves stay trackable — only the lock artifact
    // is ignored (issue #3427's "Important Note").
    for spared in [".claude/settings.json", ".claude/settings.local.json"] {
        assert!(
            !SCAFFOLD_IGNORED_PATHS.contains(&spared),
            "{spared} is project config and must stay trackable"
        );
    }
}

/// A tracked skill re-included in `.gitignore` stays visible (#7932, #8758).
///
/// Why: the skills entry is a glob, not a directory pattern, because a
/// directory-level exclude stops git descending and kills the
/// `!/.claude/skills/<name>/` negation a project writes. `.gitignore` outranks
/// `info/exclude`, so the negation still wins. Git is the oracle: a string
/// assertion cannot prove a negation reaches the file.
#[test]
fn a_tracked_skill_survives_the_exclude_entries() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let repo = tmp.path();
    real_git_repo(repo);
    std::fs::write(repo.join(".gitignore"), "!/.claude/skills/keeper/\n").unwrap();
    crate::core::harness_exclude::ensure_scaffold_excluded(repo).unwrap();

    assert!(!is_ignored(repo, ".claude/skills/keeper/SKILL.md"));
    // The skills the harness deploys stay ignored — the negation is not a
    // blanket re-include.
    assert!(is_ignored(repo, ".claude/skills/deployed/SKILL.md"));
}

/// Every bundled style file tm deploys into the project is ignored, the
/// generated composites too, and a project's own style is not (#8533).
#[test]
fn a_project_style_stays_trackable_and_generated_styles_are_ignored() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let repo = tmp.path();
    real_git_repo(repo);
    crate::core::harness_exclude::ensure_scaffold_excluded(repo).unwrap();

    assert!(!is_ignored(repo, ".claude/output-styles/fleet-voice.md"));
    assert!(is_ignored(
        repo,
        ".claude/output-styles/fleet-voice.tm-floor.md"
    ));
    for style in crate::core::bundle::OUTPUT_STYLES {
        let path = format!(".claude/output-styles/{}", style.file_name);
        assert!(is_ignored(repo, &path), "{path} must be ignored");
    }
}
