//! `prepare_session` scaffolding-gitignore wiring tests (issue #3427).
//!
//! Why: split out of `tests.rs` to keep it under the test-file SLOC cap,
//! mirroring the sibling-test-module convention this directory uses —
//! `tests_roster.rs`, `tests_launch_trust_3926.rs`. (#7894: the
//! `native_mcp_tests.rs` / `custom_mcp_tests.rs` this used to name went with
//! their modules under ADR-0042.)
//! What: covers the `prepare_session_inner` call into
//! `core::harness_exclude::ensure_scaffold_excluded` — a git-repo
//! `project_dir` gets the paths in `.git/info/exclude` and a tracked
//! `.gitignore` stays as committed (#8758); a non-git one gets no block.
//! Test: this module IS the test suite for that wiring.

use super::tests::EnvVarGuard;
use super::*;

/// Run `git -C dir args`, asserting success; returns stdout.
fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A git repository at `dir` with `.gitignore` committed as `gitignore`.
fn repo_tracking_gitignore(dir: &std::path::Path, gitignore: &str) {
    git(dir, &["init", "-q", "--initial-branch=main"]);
    git(dir, &["config", "user.email", "ci@test.invalid"]);
    git(dir, &["config", "user.name", "CI"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join(".gitignore"), gitignore).unwrap();
    git(dir, &["add", ".gitignore"]);
    git(dir, &["commit", "-q", "-m", "base"]);
}

/// #8758: a launch leaves the tracked `.gitignore` byte-identical and
/// `.gitignore` out of `git status`, even when it holds an old managed block with operator
/// lines inside it and CRLF line endings (the old refresh dropped those lines
/// and converted CRLF to LF). The paths go to the shared `info/exclude`.
#[test]
#[serial_test::serial]
fn launch_leaves_a_tracked_gitignore_untouched() {
    // #3965: `prepare_session` seeds `$HOME/.claude.json` via the REAL
    // process `$HOME` — `#[serial]` + this override keep it off the
    // operator's file.
    let tmp_home = crate::test_support::hermetic_temp_dir();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = crate::test_support::hermetic_temp_dir();
    let project = tmp.path();
    // An outdated block from a pre-#8758 launch: it lacks most managed paths,
    // carries two operator lines between the markers, and uses CRLF.
    let tracked = format!(
        "target/\r\n\r\n{}\r\n.claude/agents/\r\nmy-private-notes/\r\n*.secret\r\n{}\r\n",
        crate::core::scaffold_gitignore::SCAFFOLD_GITIGNORE_BEGIN,
        crate::core::scaffold_gitignore::SCAFFOLD_GITIGNORE_END
    );
    repo_tracking_gitignore(project, &tracked);
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    prepare_session(&fw, project).expect("prep succeeds");

    let after = std::fs::read(project.join(".gitignore")).unwrap();
    assert_eq!(
        after,
        tracked.as_bytes(),
        "launch edited the tracked .gitignore"
    );
    // The launch still writes `CLAUDE.md` and `.claude/` (untracked, not this
    // fix's concern); `.gitignore` must not appear in `git status` at all.
    let status = git(project, &["status", "--porcelain"]);
    assert!(
        !status.contains(".gitignore"),
        ".gitignore is dirty after launch:\n{status}"
    );
    let exclude = std::fs::read_to_string(project.join(".git/info/exclude")).unwrap();
    for path in crate::core::scaffold_gitignore::SCAFFOLD_IGNORED_PATHS {
        assert!(
            exclude.lines().any(|l| l == *path),
            "expected {path} in info/exclude:\n{exclude}"
        );
    }
}

#[test]
#[serial_test::serial]
fn prepare_session_skips_gitignore_when_project_is_not_git_repo() {
    // Issue #3427: the SCAFFOLDING gitignore write is gated on `project_dir`
    // actually being a git working tree — a bare/non-git project must never
    // grow the tm-managed scaffolding block. NOTE: `.gitignore` itself may
    // still exist for an entirely unrelated reason (trusty-search's
    // `colocated_storage::ensure_gitignored` unconditionally adds a
    // `.trusty-search/` entry regardless of git-repo status) — this asserts
    // the ABSENCE of tm's specific managed block, not the absence of the
    // whole file.
    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_gitignores_scaffolding_when_project_is_git_repo` above.
    let tmp_home = crate::test_support::hermetic_temp_dir();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = crate::test_support::hermetic_temp_dir();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    prepare_session(&fw, project).expect("prep succeeds");

    let gitignore = std::fs::read_to_string(project.join(".gitignore")).unwrap_or_default();
    assert!(
        !gitignore.contains(crate::core::scaffold_gitignore::SCAFFOLD_GITIGNORE_BEGIN),
        "the tm scaffolding block must not be written for a non-git project_dir:\n{gitignore}"
    );
}
