//! Tests for the tm-managed `.gitignore` block (#3427, #7762, #7932, #7875).
//!
//! Split out with `#[path]` so `scaffold_gitignore.rs` stays under the
//! 500-SLOC production cap.

use super::*;

fn init_git_repo(dir: &Path) {
    std::fs::create_dir_all(dir.join(".git")).unwrap();
}

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

/// How many rules in `content` normalize to `rule` (slash-insensitive, #7875).
fn rule_count(content: &str, rule: &str) -> usize {
    content
        .lines()
        .filter(|line| normalized_rule(line) == Some(rule))
        .count()
}

#[test]
fn writes_block_to_fresh_gitignore() {
    let tmp = crate::test_support::hermetic_temp_dir();
    init_git_repo(tmp.path());

    let wrote = ensure_scaffold_gitignored(tmp.path()).unwrap();
    assert!(wrote, "first call must write the block");

    let content = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    assert!(content.contains(SCAFFOLD_GITIGNORE_BEGIN));
    assert!(content.contains(SCAFFOLD_GITIGNORE_END));
    for path in SCAFFOLD_IGNORED_PATHS {
        assert!(
            content.contains(path),
            "missing {path} in written .gitignore:\n{content}"
        );
    }
    // Never blanket-ignore `.claude/` itself — settings.json / .mcp.json
    // must remain trackable.
    assert!(!content.lines().any(|l| l.trim() == ".claude/"));
    // #7932: nor the skills DIRECTORY, which would block descent and kill
    // every tracked-skill negation a project writes below the block.
    assert!(!content.lines().any(|l| l.trim() == ".claude/skills/"));
}

#[test]
fn block_covers_session_output_but_not_project_config() {
    // #4832: `tm` writes per-session output into `.trusty-mpm/` on every
    // launch, so the block must name it — but must NOT swallow the
    // operator-authored `framework/manifest.toml` or `config.toml`.
    let tmp = crate::test_support::hermetic_temp_dir();
    init_git_repo(tmp.path());
    ensure_scaffold_gitignored(tmp.path()).unwrap();
    let content = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();

    for covered in [
        ".trusty-mpm/sessions/",
        ".trusty-mpm/logs/",
        ".trusty-mpm/last-instructions.md",
    ] {
        assert!(
            content.lines().any(|l| l.trim() == covered),
            "missing {covered} in:\n{content}"
        );
    }
    for spared in [
        ".trusty-mpm/",
        ".trusty-mpm/framework/",
        ".trusty-mpm/config.toml",
        // #5207: the committed project config MUST stay trackable — being
        // reviewable in a PR is the entire reason it exists. Scaffolding an
        // ignore rule over it would silently defeat the feature.
        crate::core::project_config::PROJECT_CONFIG_FILE,
    ] {
        assert!(
            !content.lines().any(|l| l.trim() == spared),
            "{spared} is operator config and must stay trackable"
        );
    }
}

#[test]
fn idempotent_on_repeat_call() {
    let tmp = crate::test_support::hermetic_temp_dir();
    init_git_repo(tmp.path());

    assert!(ensure_scaffold_gitignored(tmp.path()).unwrap());
    let first = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();

    // A second (and third) call must be a no-op — no duplicated block.
    assert!(!ensure_scaffold_gitignored(tmp.path()).unwrap());
    assert!(!ensure_scaffold_gitignored(tmp.path()).unwrap());
    let second = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();

    assert_eq!(first, second, "repeat calls must not modify the file");
    assert_eq!(
        second.matches(SCAFFOLD_GITIGNORE_BEGIN).count(),
        1,
        "the begin marker must appear exactly once"
    );
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

/// A project scaffolded before #7762 gains the new entries on the next pass.
///
/// Why: the begin marker alone used to be the whole idempotency check, so an
/// existing block never gained an entry added later — the fix would have
/// reached only projects that had never run tm.
#[test]
fn an_existing_block_gains_newly_managed_paths() {
    let tmp = crate::test_support::hermetic_temp_dir();
    init_git_repo(tmp.path());
    let gitignore_path = tmp.path().join(".gitignore");
    // A pre-#7762 block: the markers plus the paths managed at that time.
    std::fs::write(
        &gitignore_path,
        format!(
            "node_modules/\n\n{SCAFFOLD_GITIGNORE_BEGIN}\n\
             .claude/agents/\n.claude/skills/\n.claude/output-styles/\n\
             {SCAFFOLD_GITIGNORE_END}\ncustom-tail/\n"
        ),
    )
    .unwrap();

    let refreshed = ensure_scaffold_gitignored(tmp.path()).unwrap();

    assert!(refreshed, "a stale block must be refreshed");
    let content = std::fs::read_to_string(&gitignore_path).unwrap();
    for managed in SCAFFOLD_IGNORED_PATHS {
        assert!(
            content.lines().any(|l| l.trim() == *managed),
            "missing {managed} after the refresh:\n{content}"
        );
    }
    assert_eq!(
        content.matches(SCAFFOLD_GITIGNORE_BEGIN).count(),
        1,
        "the refresh must replace the block, not add a second one"
    );
    assert!(
        content.starts_with("node_modules/\n"),
        "content before the block must survive:\n{content}"
    );
    assert!(
        content.ends_with("custom-tail/\n"),
        "content after the block must survive:\n{content}"
    );
    // And the refresh is itself idempotent.
    assert!(!ensure_scaffold_gitignored(tmp.path()).unwrap());
}

/// A block whose end marker was hand-deleted is not rewritten.
///
/// Why: the refresh needs both markers to know where the managed lines stop.
/// Guessing would delete whatever the operator wrote below the block.
#[test]
fn a_block_with_no_end_marker_is_left_alone() {
    let tmp = crate::test_support::hermetic_temp_dir();
    init_git_repo(tmp.path());
    let gitignore_path = tmp.path().join(".gitignore");
    let mangled = format!("{SCAFFOLD_GITIGNORE_BEGIN}\n.claude/agents/\nmine/\n");
    std::fs::write(&gitignore_path, &mangled).unwrap();

    let changed = ensure_scaffold_gitignored(tmp.path()).unwrap();

    assert!(!changed);
    assert_eq!(
        std::fs::read_to_string(&gitignore_path).unwrap(),
        mangled,
        "an operator-edited block must be left byte-for-byte alone"
    );
}

#[test]
fn preserves_unrelated_existing_content() {
    let tmp = crate::test_support::hermetic_temp_dir();
    init_git_repo(tmp.path());
    let gitignore_path = tmp.path().join(".gitignore");
    std::fs::write(&gitignore_path, "node_modules/\ntarget/\n").unwrap();

    ensure_scaffold_gitignored(tmp.path()).unwrap();

    let content = std::fs::read_to_string(&gitignore_path).unwrap();
    assert!(content.starts_with("node_modules/\ntarget/\n"));
    assert!(content.contains(SCAFFOLD_GITIGNORE_BEGIN));
}

#[test]
fn appends_newline_when_existing_file_lacks_trailing_newline() {
    let tmp = crate::test_support::hermetic_temp_dir();
    init_git_repo(tmp.path());
    let gitignore_path = tmp.path().join(".gitignore");
    // Deliberately no trailing newline.
    std::fs::write(&gitignore_path, "node_modules/").unwrap();

    ensure_scaffold_gitignored(tmp.path()).unwrap();

    let content = std::fs::read_to_string(&gitignore_path).unwrap();
    assert!(content.starts_with("node_modules/\n"));
    assert!(
        !content.contains("node_modules/#"),
        "must not glue lines together"
    );
}

#[test]
fn noop_when_not_a_git_repo() {
    let tmp = crate::test_support::hermetic_temp_dir();
    // No `.git` created.

    let wrote = ensure_scaffold_gitignored(tmp.path()).unwrap();
    assert!(!wrote, "must not write when project_dir is not a git repo");
    assert!(!tmp.path().join(".gitignore").exists());
}

/// A tracked skill stays tracked across a refresh (#7932).
///
/// Why: the generator used to emit `.claude/skills/`, and a directory-level
/// exclude stops git descending — so the `!/.claude/skills/<name>/` negation
/// PR #7916 added below the block was dead the moment the block regenerated,
/// and the two tracked skills went ignored on every launch. Git itself is the
/// oracle here: a string assertion on the rendered block cannot prove that a
/// negation still reaches the file.
#[test]
fn a_tracked_skill_survives_a_refresh_of_the_block() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let repo = tmp.path();
    real_git_repo(repo);
    std::fs::create_dir_all(repo.join(".claude/skills/keeper")).unwrap();
    std::fs::write(repo.join(".claude/skills/keeper/SKILL.md"), "# keeper\n").unwrap();
    std::fs::create_dir_all(repo.join(".claude/skills/deployed")).unwrap();
    std::fs::write(repo.join(".claude/skills/deployed/SKILL.md"), "# gen\n").unwrap();

    // The shape this repo had: a stale directory-form block, with the tracked
    // skill re-included below it.
    std::fs::write(
        repo.join(".gitignore"),
        format!(
            "{SCAFFOLD_GITIGNORE_BEGIN}\n\
             .claude/agents/\n.claude/skills/\n.claude/output-styles/\n\
             {SCAFFOLD_GITIGNORE_END}\n!/.claude/skills/keeper/\n"
        ),
    )
    .unwrap();

    assert!(ensure_scaffold_gitignored(repo).unwrap(), "must refresh");

    assert!(
        !is_ignored(repo, ".claude/skills/keeper/SKILL.md"),
        "the negation must survive the refresh:\n{}",
        std::fs::read_to_string(repo.join(".gitignore")).unwrap()
    );
    // The un-negated skills the harness deploys stay ignored — the narrowing
    // must not become a blanket re-include.
    assert!(is_ignored(repo, ".claude/skills/deployed/SKILL.md"));
}

/// An entry the project already spells outside the block is not re-added
/// (#7875).
///
/// Why: `/.claude/settings.json.lock` (PR #7860) and the block's
/// `.claude/settings.json.lock` are the same rule with different anchoring.
/// The old idempotency check looked only inside the block, so every launch
/// appended both lock entries again and the checkout never converged.
#[test]
fn an_entry_spelled_outside_the_block_is_not_duplicated() {
    let tmp = crate::test_support::hermetic_temp_dir();
    init_git_repo(tmp.path());
    let gitignore_path = tmp.path().join(".gitignore");
    std::fs::write(
        &gitignore_path,
        format!(
            "/.claude/settings.json.lock\n/.claude/settings.local.json.lock\n\n\
             {SCAFFOLD_GITIGNORE_BEGIN}\n\
             .claude/agents/\n.claude/skills/\n.claude/output-styles/\n\
             {SCAFFOLD_GITIGNORE_END}\n"
        ),
    )
    .unwrap();

    ensure_scaffold_gitignored(tmp.path()).unwrap();

    let content = std::fs::read_to_string(&gitignore_path).unwrap();
    for rule in [
        ".claude/settings.json.lock",
        ".claude/settings.local.json.lock",
    ] {
        assert_eq!(
            rule_count(&content, rule),
            1,
            "{rule} must appear exactly once in:\n{content}"
        );
    }
    // Converged: nothing left to write on the next launch.
    assert!(
        !ensure_scaffold_gitignored(tmp.path()).unwrap(),
        "a deduped block must be stable across launches"
    );
}

/// A negation is never coverage, and is never rewritten (#7932, #7875).
///
/// Why: treating `!/.claude/skills/keeper/` as "this path is already handled"
/// would drop the managed rule it depends on. And the operator's negations
/// live outside the block by design — the refresh carries them through
/// verbatim, exactly once.
#[test]
fn a_negation_outside_the_block_is_not_treated_as_coverage() {
    assert_eq!(normalized_rule("!/.claude/skills/keeper/"), None);
    assert_eq!(normalized_rule("# a comment"), None);
    assert_eq!(normalized_rule("   "), None);
    assert_eq!(
        normalized_rule("  /.claude/settings.json.lock  "),
        Some(".claude/settings.json.lock"),
        "the anchoring slash is the only difference that must be normalized"
    );

    let tmp = crate::test_support::hermetic_temp_dir();
    init_git_repo(tmp.path());
    let gitignore_path = tmp.path().join(".gitignore");
    let negation = "!/.claude/skills/keeper/";
    std::fs::write(
        &gitignore_path,
        format!(
            "{SCAFFOLD_GITIGNORE_BEGIN}\n.claude/agents/\n{SCAFFOLD_GITIGNORE_END}\n{negation}\n"
        ),
    )
    .unwrap();

    ensure_scaffold_gitignored(tmp.path()).unwrap();

    let content = std::fs::read_to_string(&gitignore_path).unwrap();
    assert!(
        content.lines().any(|l| l == ".claude/skills/*"),
        "the managed glob must still be emitted:\n{content}"
    );
    assert_eq!(
        content.lines().filter(|l| *l == negation).count(),
        1,
        "the negation must survive once, unrewritten:\n{content}"
    );
    assert!(content.ends_with(&format!("{negation}\n")));
}

/// This repo's own committed block is what the generator would write.
///
/// Why (#7932): the block is regenerated on every launch, so any drift between
/// the constant and the committed file makes the main checkout dirty at every
/// launch — which is how the tracked-skill narrowing got reverted in the first
/// place. This test is the gate that keeps the two in step: change
/// `SCAFFOLD_IGNORED_PATHS` and you must update `.gitignore` in the same PR.
#[test]
fn this_repos_committed_block_matches_the_generator() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let gitignore = std::fs::read_to_string(repo_root.join(".gitignore"))
        .expect("the workspace root .gitignore");
    let lines: Vec<&str> = gitignore.lines().collect();
    let (begin, end) =
        block_range(&lines).expect("this repo's .gitignore carries the managed block");
    let committed: String = lines[begin..=end].iter().flat_map(|l| [*l, "\n"]).collect();

    assert_eq!(
        committed,
        render_block_for(&gitignore),
        "the committed block has drifted from the generator — a launch would rewrite it"
    );
}

/// A failed rewrite leaves the operator's file intact and reports the error.
///
/// Why: the refresh replaces the whole file. A non-atomic write that fails
/// part-way would leave a truncated `.gitignore` — the one file whose loss
/// un-ignores every harness artifact at once. The write stages to a sibling
/// temp file and renames, so a failure is total or nothing, and never `Ok`.
#[cfg(unix)]
#[test]
fn a_failed_write_leaves_the_gitignore_intact_and_reports_the_error() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = crate::test_support::hermetic_temp_dir();
    init_git_repo(tmp.path());
    let gitignore_path = tmp.path().join(".gitignore");
    let stale = format!("{SCAFFOLD_GITIGNORE_BEGIN}\n.claude/agents/\n{SCAFFOLD_GITIGNORE_END}\n");
    std::fs::write(&gitignore_path, &stale).unwrap();

    let original = std::fs::metadata(tmp.path()).unwrap().permissions();
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    // Running as root (or on a filesystem that ignores mode bits) the failure
    // cannot be provoked at all — skip rather than assert a false property.
    let probe = tmp.path().join(".write-probe");
    if std::fs::write(&probe, b"x").is_ok() {
        let _ = std::fs::remove_file(&probe);
        std::fs::set_permissions(tmp.path(), original).unwrap();
        return;
    }

    let result = ensure_scaffold_gitignored(tmp.path());
    std::fs::set_permissions(tmp.path(), original).unwrap();

    assert!(
        result.is_err(),
        "a failed write must never report success: {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&gitignore_path).unwrap(),
        stale,
        "the operator's file must survive byte-for-byte"
    );
    let strays: Vec<String> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(strays.is_empty(), "scratch files left behind: {strays:?}");
}
