//! End-to-end proof that `tm hook --pm-guard` refuses an agent's HEAD switch in
//! a dirty main checkout (#8572).
//!
//! Why: the reported incident was a `version-control` dispatch running
//! `git checkout <branch>` in the shared main checkout over the operator's
//! uncommitted edit. The unit rows in `pm_guard_bash::head_switch` prove the
//! policy with an injected dirty-state probe; only the real binary proves the
//! probe itself — a real `git status --porcelain` against a real repository —
//! and that the rule fires ahead of the subagent exemptions.
//! What: builds a real repository per test, spawns the built `tm` against an
//! unreachable daemon URL, and asserts ALLOW (empty stdout) or DENY (one JSON
//! line carrying `permissionDecision: "deny"`).
//! Test: `cargo test -p trusty-mpm --test tm_hook_pm_guard_head_switch`.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Nothing listens on port 1, so a deny's best-effort audit POST fails fast.
const UNREACHABLE_DAEMON: &str = "http://127.0.0.1:1";

/// The agent marker a native Task/Agent dispatch stamps on its payload.
const AGENT: &str = r#""agent_id":"agent-8572","agent_type":"version-control","#;

/// Run `tm hook --pm-guard` over one payload, from `cwd`, and return stdout.
fn run_pm_guard(stdin_json: &str, cwd: &Path) -> String {
    let home = tempfile::tempdir().expect("home");
    common::write_disk_threshold(home.path(), 100);
    let mut child = common::tm_command_in(home.path())
        .args(["--url", UNREACHABLE_DAEMON, "hook", "--pm-guard"])
        .current_dir(cwd)
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
        .env_remove("TM_MANAGED_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `tm hook --pm-guard`");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin_json.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "tm hook --pm-guard must exit 0: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    common::assert_pm_guard_refusals_prefixed(&stdout);
    stdout
}

/// A `PreToolUse` Bash payload for `command` in `cwd`, with `extra` spliced in.
fn payload(command: &str, cwd: &Path, extra: &str) -> String {
    let base = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "cwd": cwd.display().to_string(),
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let spliced = format!("{{{extra}{}", &base[1..]);
    serde_json::from_str::<serde_json::Value>(&spliced).expect("spliced payload parses");
    spliced
}

/// Run git in `dir` with a fixed identity and assert it succeeded.
fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .status()
        .expect("git")
        .success();
    assert!(ok, "git {args:?}");
}

/// A real main checkout with one commit and a `feat/x` branch, clean.
fn main_checkout() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    git(&repo, &["init", "-q", "-b", "main", "."]);
    std::fs::write(repo.join("a.txt"), "a").expect("write");
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    git(&repo, &["branch", "feat/x"]);
    (dir, repo)
}

/// Assert `stdout` is exactly one deny line naming #8572.
fn assert_head_switch_denied(stdout: &str) {
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "a deny prints one JSON line: {stdout:?}");
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("deny is JSON");
    assert_eq!(
        parsed["hookSpecificOutput"]["permissionDecision"], "deny",
        "{stdout}"
    );
    assert!(stdout.contains("#8572"), "{stdout}");
}

#[test]
fn pm_guard_refuses_an_agent_branch_switch_in_a_dirty_main_checkout() {
    // The incident: the operator's edit is in the tree, an agent switches.
    let (_dir, repo) = main_checkout();
    std::fs::write(repo.join("a.txt"), "operator edit").expect("edit");
    for command in [
        "git checkout feat/x",
        "git switch feat/x",
        "git checkout -b fix/y",
        "git stash && git checkout feat/x",
    ] {
        let stdout = run_pm_guard(&payload(command, &repo, AGENT), &repo);
        assert_head_switch_denied(&stdout);
        assert!(stdout.contains(&repo.display().to_string()), "{stdout}");
        assert!(stdout.contains("uncommitted work"), "{stdout}");
    }
}

#[test]
fn pm_guard_refuses_a_branch_switch_when_git_status_fails() {
    // #8572 fail-closed arm: a corrupt index makes `git status` exit non-zero,
    // and an unreadable state must refuse, never read as clean.
    let (_dir, repo) = main_checkout();
    std::fs::write(repo.join(".git/index"), "not an index").expect("corrupt index");
    let stdout = run_pm_guard(&payload("git checkout feat/x", &repo, AGENT), &repo);
    assert_head_switch_denied(&stdout);
    assert!(stdout.contains("could not be checked"), "{stdout}");
}

#[test]
fn pm_guard_allows_an_agent_branch_switch_in_a_clean_main_checkout() {
    // #5356: a clean checkout keeps the pre-#8572 answer.
    let (_dir, repo) = main_checkout();
    let stdout = run_pm_guard(&payload("git checkout feat/x", &repo, AGENT), &repo);
    assert_eq!(
        stdout.trim(),
        "",
        "clean main checkout must allow: {stdout}"
    );
}

#[test]
fn pm_guard_allows_the_pm_branch_switch_in_a_dirty_main_checkout() {
    // The rule binds dispatched agents; the PM's payload carries no agent_id.
    let (_dir, repo) = main_checkout();
    std::fs::write(repo.join("a.txt"), "operator edit").expect("edit");
    let stdout = run_pm_guard(&payload("git checkout -b docs/x", &repo, ""), &repo);
    assert_eq!(stdout.trim(), "", "the PM must be allowed: {stdout}");
}

#[test]
fn pm_guard_allows_a_path_restore_in_the_agents_own_worktree() {
    // #8579: `git checkout <sha> -- <path>` inside the agent's own worktree
    // stays allowed, even while the main checkout is dirty.
    let (_dir, repo) = main_checkout();
    let wt = repo.join(".claude/worktrees/agent-8572");
    git(
        &repo,
        &["worktree", "add", "-q", &wt.display().to_string(), "feat/x"],
    );
    std::fs::write(repo.join("a.txt"), "operator edit").expect("edit");
    std::fs::write(wt.join("a.txt"), "agent edit").expect("edit");
    for command in ["git checkout HEAD -- a.txt", "git checkout -b fix/z"] {
        let stdout = run_pm_guard(&payload(command, &wt, AGENT), &wt);
        assert_eq!(stdout.trim(), "", "`{command}` in own worktree: {stdout}");
    }
}

#[test]
fn pm_guard_refuses_an_unresolved_directory_switch_from_the_agents_worktree() {
    // #8572 review: from the agent's own worktree, `-C $MAIN` resolved to
    // `<worktree>/$MAIN`, read as the worktree, and was allowed; the shell
    // then ran the switch in the main checkout.
    let (_dir, repo) = main_checkout();
    let wt = repo.join(".claude/worktrees/agent-8572");
    git(
        &repo,
        &["worktree", "add", "-q", &wt.display().to_string(), "feat/x"],
    );
    for command in [
        "git -C $MAIN checkout feat/x",
        "cd $MAIN && git switch main",
    ] {
        let stdout = run_pm_guard(&payload(command, &wt, AGENT), &wt);
        assert_head_switch_denied(&stdout);
        assert!(stdout.contains("$MAIN"), "{stdout}");
    }
}
