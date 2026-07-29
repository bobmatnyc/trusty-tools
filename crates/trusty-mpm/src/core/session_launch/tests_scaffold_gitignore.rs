//! `prepare_session` scaffolding-gitignore wiring tests (issue #3427).
//!
//! Why: split out of `tests.rs` to keep it under the 1500-SLOC test-file cap,
//! mirroring the existing `tests_roster.rs` / `native_mcp_tests.rs` /
//! `custom_mcp_tests.rs` sibling-test-module convention for this directory.
//! What: covers the `prepare_session_inner` call into
//! `core::scaffold_gitignore::ensure_scaffold_gitignored` — a git-repo
//! `project_dir` gets the managed `.gitignore` block, a non-git one does not.
//! Test: this module IS the test suite for that wiring.

use super::tests::EnvVarGuard;
use super::*;

#[test]
#[serial_test::serial]
fn prepare_session_gitignores_scaffolding_when_project_is_git_repo() {
    // Issue #3427 (Part 1 — prevent): a managed workspace deploys agents,
    // skills, and output styles project-locally; when `project_dir` is a git
    // working tree, `prepare_session` must ALSO ensure those paths are
    // gitignored so they never enter this project's history (the
    // precondition for the "would be overwritten by merge" collision).
    // #3965: `prepare_session` seeds `$HOME/.claude.json` via the REAL
    // process `$HOME`, not `fw` — `#[serial]` + the override below keep this
    // test off the operator's real file and off every sibling test doing the
    // same (see `session_launch::tests::prepare_session_writes_claude_md_and_stash`).
    let tmp_home = crate::test_support::hermetic_temp_dir();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = crate::test_support::hermetic_temp_dir();
    let project = tmp.path();
    std::fs::create_dir_all(project.join(".git")).unwrap();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    prepare_session(&fw, project).expect("prep succeeds");

    let gitignore = std::fs::read_to_string(project.join(".gitignore"))
        .expect("prepare_session must write .gitignore for a git-repo project_dir");
    for path in crate::core::scaffold_gitignore::SCAFFOLD_IGNORED_PATHS {
        assert!(
            gitignore.contains(path),
            "expected {path} in .gitignore:\n{gitignore}"
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
