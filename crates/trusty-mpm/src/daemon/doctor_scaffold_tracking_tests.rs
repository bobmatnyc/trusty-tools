//! Unit tests for [`super`] (`tm doctor` scaffold-tracking probe, issue #3427).
//!
//! Why: split out of `doctor_scaffold_tracking.rs` to mirror the sibling
//! `doctor_hooks_hygiene_tests.rs` test-module convention.
//! What: builds real git-repo fixtures (via the `git` CLI, matching the
//! existing `daemon::managed_routes::inproject::tests` precedent) under
//! [`crate::test_support::hermetic_temp_dir`] — never a bare `TempDir::new()`
//! and never anywhere under the real `~/trusty-mpm-projects` (issue #3450 /
//! umbrella #3451) — and asserts the true-intersection computation: a real
//! collision (tracked + regenerated) is reported, an unrelated tracked
//! project-custom skill is NOT, and a clean repo produces no warning at all.
//! Test: this module IS the test suite for `super`.

use std::path::Path;

use super::*;

fn run_git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

/// `git init` + committer identity, matching the existing
/// `inproject::tests` git-fixture convention.
fn init_git_repo(dir: &Path) {
    run_git(dir, &["init", "-q"]);
    run_git(dir, &["config", "user.email", "test@example.com"]);
    run_git(dir, &["config", "user.name", "Test"]);
}

fn commit_all(dir: &Path, message: &str) {
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-q", "-m", message]);
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Write an agent manifest recording `filenames` as tm-managed, mirroring the
/// on-disk shape `AgentManifest::save` produces (without pulling in the full
/// deployer just to seed a fixture).
fn write_agent_manifest(project: &Path, filenames: &[&str]) {
    let dir = project.join(".claude").join("agents");
    std::fs::create_dir_all(&dir).unwrap();
    let managed: serde_json::Map<String, serde_json::Value> = filenames
        .iter()
        .map(|f| {
            (
                f.to_string(),
                serde_json::json!({
                    "source_chain": [f],
                    "checksum": "deadbeef",
                    "deployed_at": "2026-01-01T00:00:00Z",
                    "origin": "bundled",
                }),
            )
        })
        .collect();
    let manifest = serde_json::json!({ "version": 1, "managed": managed });
    std::fs::write(
        dir.join(crate::core::agent_manifest::MANIFEST_FILE),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

/// Write a skill manifest recording `keys` (bare stems and/or
/// `<stem>/references/<file>` entries) as tm-managed.
fn write_skill_manifest(project: &Path, keys: &[&str]) {
    let dir = project.join(".claude").join("skills");
    std::fs::create_dir_all(&dir).unwrap();
    let managed: serde_json::Map<String, serde_json::Value> = keys
        .iter()
        .map(|k| {
            (
                k.to_string(),
                serde_json::json!({ "checksum": "deadbeef", "deployed_at": "2026-01-01T00:00:00Z" }),
            )
        })
        .collect();
    let manifest = serde_json::json!({ "version": 1, "managed": managed });
    std::fs::write(
        dir.join(crate::core::skill_manifest::SKILL_MANIFEST_FILE),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

#[test]
fn check_ok_when_no_project_dir() {
    let check = check_scaffold_tracking(None);
    assert_eq!(check.status, CheckStatus::Ok);
}

#[test]
fn check_ok_when_not_a_git_repo() {
    let tmp = crate::test_support::hermetic_temp_dir();
    write_skill_manifest(tmp.path(), &["documentation-style"]);
    write(
        tmp.path(),
        ".claude/skills/documentation-style/SKILL.md",
        "content",
    );
    // Deliberately no `git init` — not a git working tree at all.

    let check = check_scaffold_tracking(Some(tmp.path()));
    assert_eq!(check.status, CheckStatus::Ok);
}

#[test]
fn check_ok_on_clean_repo() {
    // A repo that tracks completely unrelated files, and has never had tm
    // scaffolding deployed into it at all, must report Ok — no false
    // positive "your repo is broken" warning on an untouched project.
    let tmp = crate::test_support::hermetic_temp_dir();
    init_git_repo(tmp.path());
    write(tmp.path(), "README.md", "hello");
    commit_all(tmp.path(), "init");

    let check = check_scaffold_tracking(Some(tmp.path()));
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
}

#[test]
fn check_warns_on_true_collision_only() {
    // Reproduces the verified duetto-eve-agents#111 shape: a skill directory
    // (SKILL.md + a reference file) is BOTH tm-managed (manifest says so) AND
    // committed to git — the true collision. Alongside it, plant TWO
    // decoys that must NOT be reported:
    //   1. An untracked-but-managed agent (regenerated, but never committed —
    //      not part of the intersection).
    //   2. A tracked-but-UNMANAGED "project-custom" skill (hand-placed and
    //      intentionally committed, per issue #2816's Project tier — tm never
    //      regenerates it, so it must never be flagged even though it is
    //      tracked in the exact same directory tree).
    let tmp = crate::test_support::hermetic_temp_dir();
    let project = tmp.path();
    init_git_repo(project);

    // The true collision: documentation-style, managed + tracked.
    write_skill_manifest(
        project,
        &[
            "documentation-style",
            "documentation-style/references/one.md",
        ],
    );
    write(
        project,
        ".claude/skills/documentation-style/SKILL.md",
        "entry point",
    );
    write(
        project,
        ".claude/skills/documentation-style/references/one.md",
        "reference",
    );

    // Decoy 2: tracked, hand-placed, NOT in the skill manifest at all.
    write(
        project,
        ".claude/skills/my-project-custom-skill/SKILL.md",
        "hand placed, intentionally committed",
    );

    commit_all(project, "commit scaffolding + custom skill");

    // Decoy 1: managed (per the agent manifest) but never committed — must
    // not appear in a report about TRACKED files.
    write_agent_manifest(project, &["engineer.md"]);
    write(project, ".claude/agents/engineer.md", "agent body");

    let check = check_scaffold_tracking(Some(project));
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(
        check
            .message
            .contains(".claude/skills/documentation-style/SKILL.md"),
        "missing true-collision entry point in: {}",
        check.message
    );
    assert!(
        check
            .message
            .contains(".claude/skills/documentation-style/references/one.md"),
        "missing true-collision reference file in: {}",
        check.message
    );
    assert!(
        !check.message.contains("my-project-custom-skill"),
        "must not flag a tracked-but-unmanaged project-custom skill: {}",
        check.message
    );
    assert!(
        !check.message.contains("engineer.md"),
        "must not flag a managed-but-untracked agent file: {}",
        check.message
    );
}

#[test]
fn regenerated_paths_includes_managed_agent() {
    let tmp = crate::test_support::hermetic_temp_dir();
    write_agent_manifest(tmp.path(), &["engineer.md"]);

    let paths = regenerated_scaffold_paths(tmp.path());
    assert!(paths.contains(".claude/agents/engineer.md"));
}

#[test]
fn regenerated_paths_includes_skill_entry_and_references() {
    let tmp = crate::test_support::hermetic_temp_dir();
    write_skill_manifest(
        tmp.path(),
        &["tm-doctor", "tm-doctor/references/checklist.md"],
    );

    let paths = regenerated_scaffold_paths(tmp.path());
    assert!(paths.contains(".claude/skills/tm-doctor/SKILL.md"));
    assert!(paths.contains(".claude/skills/tm-doctor/references/checklist.md"));
}

#[test]
fn regenerated_paths_includes_all_output_styles() {
    let tmp = crate::test_support::hermetic_temp_dir();

    let paths = regenerated_scaffold_paths(tmp.path());
    for style in crate::core::bundle::OUTPUT_STYLES {
        assert!(
            paths.contains(&format!(".claude/output-styles/{}", style.file_name)),
            "missing bundled style {} in {paths:?}",
            style.file_name
        );
    }
}

#[test]
fn tracked_scaffold_paths_returns_none_outside_git_repo() {
    let tmp = crate::test_support::hermetic_temp_dir();
    assert!(tracked_scaffold_paths(tmp.path()).is_none());
}

#[test]
fn tracked_scaffold_paths_lists_committed_files() {
    let tmp = crate::test_support::hermetic_temp_dir();
    init_git_repo(tmp.path());
    write(tmp.path(), ".claude/skills/foo/SKILL.md", "x");
    write(tmp.path(), "README.md", "unrelated");
    commit_all(tmp.path(), "init");

    let tracked = tracked_scaffold_paths(tmp.path()).expect("must resolve inside a git repo");
    assert!(tracked.contains(".claude/skills/foo/SKILL.md"));
    assert!(
        !tracked.contains("README.md"),
        "pathspec must scope to the three harness subtrees only"
    );
}

#[test]
fn remediation_message_lists_exact_paths() {
    let mut collisions = std::collections::BTreeSet::new();
    collisions.insert(".claude/skills/documentation-style/SKILL.md".to_string());
    let message = remediation_message(&collisions);
    assert!(message.contains("git rm -r --cached"));
    assert!(message.contains(".claude/skills/documentation-style/SKILL.md"));
}
