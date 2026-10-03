//! End-to-end proof of the operator-listed checkout rules (#8524, #7905).
//!
//! Why: the unit rows in `pm_guard_bash::operator_checkouts` inject the
//! allowlist and the content probe. Only the real binary proves the list is
//! read from `~/.trusty-mpm/config.toml`, that the probe is a real
//! `git diff --quiet`, and that the destructive, commit and write rules all
//! consult it.
//! What: a real repository per test, the built `tm` against an unreachable
//! daemon, and a scratch `$HOME` whose config lists (or does not list) it.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_operator_checkouts::`.

use crate::common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Nothing listens on port 1, so the daemon is unreachable.
const UNREACHABLE_DAEMON: &str = "http://127.0.0.1:1";

/// Run `tm hook --pm-guard` over `payload` from `cwd` with `home`; `true` = deny.
fn denied(home: &Path, cwd: &Path, payload: serde_json::Value) -> bool {
    common::write_disk_threshold(home, 100);
    let mut child = common::tm_command_in(home)
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
        .write_all(payload.to_string().as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success(), "tm hook --pm-guard must exit 0");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    common::assert_pm_guard_refusals_prefixed(&stdout);
    stdout.contains("\"deny\"")
}

/// A `Bash` payload from the PM.
fn bash(command: &str, cwd: &Path) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "cwd": cwd.display().to_string(),
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
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

/// A main checkout on `main` with one commit, plus a scratch home.
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    std::fs::create_dir_all(home.join(".trusty-mpm")).expect("mkdir home");
    git(&repo, &["init", "-q", "-b", "main", "."]);
    std::fs::write(repo.join("config.yaml"), "a: 1\n").expect("write");
    git(&repo, &["add", "config.yaml"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    (dir, repo, home)
}

/// List `repo` under `key` in `home`'s operator config.
fn list(home: &Path, key: &str, repo: &Path) {
    let body = format!("[pm_guard]\n{key} = [{:?}]\n", repo.display().to_string());
    std::fs::write(home.join(".trusty-mpm/config.toml"), body).expect("config");
}

/// 🔴 REGRESSION (#8524): `git reset --keep <rev>` in a listed cron-host
/// runtime checkout whose content equals `<rev>` is allowed. Denied on
/// origin/main. Unlisted, a content drift, and `--hard` all stay denied.
#[test]
fn pm_guard_allows_reset_keep_in_a_listed_runtime_checkout() {
    let (_dir, repo, home) = fixture();
    // `drift` carries the same content under a new commit; `other` differs.
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "same content"],
    );
    git(&repo, &["branch", "drift"]);
    git(&repo, &["reset", "-q", "--soft", "HEAD~1"]);
    git(&repo, &["checkout", "-q", "-b", "other"]);
    std::fs::write(repo.join("config.yaml"), "a: 2\n").expect("write");
    git(&repo, &["commit", "-q", "-am", "other"]);
    git(&repo, &["checkout", "-q", "main"]);

    let keep = bash("git reset --keep drift", &repo);
    assert!(denied(&home, &repo, keep.clone()), "unlisted stays denied");
    list(&home, "runtime_checkouts", &repo);
    assert!(
        !denied(&home, &repo, keep),
        "listed with equal content is allowed"
    );
    assert!(denied(&home, &repo, bash("git reset --keep other", &repo)));
    assert!(denied(&home, &repo, bash("git reset --hard drift", &repo)));
    assert!(denied(
        &home,
        &repo,
        bash("git reset --keep no-such-ref", &repo)
    ));
}

/// 🔴 REGRESSION (#7905): a listed documents repo commits and writes a
/// tracked `.py` from its main checkout. Denied on origin/main; the same
/// repo unlisted stays denied.
#[test]
fn pm_guard_documents_repo_commits_and_writes_a_script() {
    let (_dir, repo, home) = fixture();
    std::fs::write(repo.join("make-graphics.py"), "print(1)\n").expect("write");
    git(&repo, &["add", "make-graphics.py"]);
    let commit = bash("git commit -m \"archive: move\" -m \"body\"", &repo);
    let write = serde_json::json!({
        "agent_id": "agent-7905",
        "agent_type": "engineer",
        "hook_event_name": "PreToolUse",
        "cwd": repo.display().to_string(),
        "tool_name": "Write",
        "tool_input": {
            "file_path": repo.join("archive/make-graphics.py").display().to_string(),
            "content": "print(1)\n",
        },
    });
    assert!(
        denied(&home, &repo, commit.clone()),
        "unlisted commit stays denied"
    );
    assert!(
        denied(&home, &repo, write.clone()),
        "unlisted write stays denied"
    );
    list(&home, "documents_repos", &repo);
    assert!(!denied(&home, &repo, commit), "listed commit is allowed");
    assert!(!denied(&home, &repo, write), "listed write is allowed");
}
