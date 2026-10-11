//! End-to-end proof that `tm hook --pm-guard` refuses a copy-verb or `sed -i`
//! write into a main checkout from an agent standing in its worktree (#7228).
//!
//! Why: the incident was an agent in its own worktree reaching back into the
//! shared main checkout by absolute path. `cp`, `mv`, `install`, `rsync`,
//! `dd of=` and `sed -i` named that destination as an ordinary argument, so
//! the write boundary read no target, and Guard 4's subagent exemption let
//! the command through. Only the real binary proves the boundary now runs
//! ahead of that exemption for these verbs.
//! What: builds a real repository with a linked-worktree-shaped directory per
//! test, spawns the built `tm` against an unreachable daemon URL with an agent
//! payload, and asserts DENY (one JSON line naming ADR-0044) or ALLOW (empty
//! stdout).
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_absolute_write_7228::`.

use crate::common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Nothing listens on port 1, so a deny's best-effort audit POST fails fast.
const UNREACHABLE_DAEMON: &str = "http://127.0.0.1:1";

/// The agent marker a native Task/Agent dispatch stamps on its payload.
const AGENT: &str = r#""agent_id":"agent-7228","agent_type":"rust-engineer","#;

/// A main checkout `repo/`, its worktree, and a scratch `$HOME`.
struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    main: PathBuf,
    worktree: PathBuf,
    home: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical tempdir");
    let main = root.join("repo");
    std::fs::create_dir_all(main.join("src")).expect("mkdir src");
    let ok = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main", "."])
        .current_dir(&main)
        .status()
        .expect("git init")
        .success();
    assert!(ok, "git init");
    let worktree = main.join(".claude/worktrees/agent-7228");
    std::fs::create_dir_all(worktree.join("src")).expect("mkdir worktree");
    std::fs::write(worktree.join(".git"), "gitdir: /elsewhere").expect("write .git");
    std::fs::create_dir_all(root.join("repo-other/src")).expect("mkdir sibling");
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".trusty-mpm")).expect("mkdir home");
    common::write_disk_threshold(&home, 100);
    Fixture {
        _dir: dir,
        root,
        main,
        worktree,
        home,
    }
}

/// Run `tm hook --pm-guard` over `command` from `cwd`; `extra` is spliced
/// into the payload. Returns stdout.
fn run(fx: &Fixture, command: &str, cwd: &Path, extra: &str) -> String {
    let base = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "cwd": cwd.display().to_string(),
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let stdin = format!("{{{extra}{}", &base[1..]);
    let mut child = common::tm_command_in(&fx.home)
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
        .write_all(stdin.as_bytes())
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

/// Assert `stdout` is one deny line carrying `marker`.
fn assert_denied(stdout: &str, marker: &str, command: &str) {
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "`{command}` must print one deny: {stdout:?}"
    );
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("deny is JSON");
    assert_eq!(
        parsed["hookSpecificOutput"]["permissionDecision"], "deny",
        "`{command}`: {stdout}"
    );
    assert!(stdout.contains(marker), "`{command}`: {stdout}");
}

#[test]
fn pm_guard_denies_cp_into_main_checkout_from_a_worktree_agent() {
    let fx = fixture();
    let main = fx.main.display();
    for command in [
        format!("cp src/lib.rs {main}/src/lib.rs"),
        format!("cp src/lib.rs {main}/src/"),
        format!("cp src/lib.rs {main}/src"),
        format!("cp -t {main}/src src/lib.rs"),
    ] {
        let stdout = run(&fx, &command, &fx.worktree, AGENT);
        assert_denied(&stdout, "ADR-0044", &command);
    }
}

#[test]
fn pm_guard_denies_every_tier_a_verb_into_main_checkout_from_a_worktree_agent() {
    let fx = fixture();
    let main = fx.main.display();
    for command in [
        format!("mv src/lib.rs {main}/src/lib.rs"),
        format!("install -m 0644 src/lib.rs {main}/src/lib.rs"),
        format!("rsync -a src/lib.rs {main}/src/"),
        format!("dd if=src/lib.rs of={main}/src/lib.rs"),
        format!("sed -i 's/a/b/' {main}/src/lib.rs"),
        format!("sed -i '' 's/a/b/' {main}/src/lib.rs"),
        format!("sed -i.bak 's/a/b/' {main}/src/lib.rs"),
    ] {
        let stdout = run(&fx, &command, &fx.worktree, AGENT);
        assert_denied(&stdout, "ADR-0044", &command);
    }
}

// #7228 fail closed: an agent's `cp` that does not lex cannot be placed.
#[test]
fn pm_guard_denies_an_unlexable_cp_from_a_worktree_agent() {
    let fx = fixture();
    let command = format!("cp src/lib.rs '{}/src/lib.rs", fx.main.display());
    let stdout = run(&fx, &command, &fx.worktree, AGENT);
    assert_denied(&stdout, "#8730", &command);
}

#[test]
fn pm_guard_allows_tier_a_verbs_that_write_outside_main_checkout_source() {
    let fx = fixture();
    let (main, wt, root) = (fx.main.display(), fx.worktree.display(), fx.root.display());
    let clone = fx.root.join("scratchpad/clone");
    std::fs::create_dir_all(clone.join(".git")).expect("mkdir clone");
    let (clone, home) = (clone.display(), fx.home.display());
    // A symlink outside the main checkout that resolves into the worktree.
    let link = fx.root.join("wt-link");
    std::os::unix::fs::symlink(&fx.worktree, &link).expect("symlink");
    let link = link.display();
    // Scratchpad clones under /tmp and /private/tmp (#7778 exempts only a
    // clone under a `scratchpad` segment; a bare /tmp clone is a checkout).
    let (_tmp_dir, tmp) = scratch_clone("/tmp");
    let (_private_dir, private) = scratch_clone("/private/tmp");
    let (tmp, private) = (tmp.display(), private.display());
    for command in [
        format!("cp {main}/src/lib.rs {wt}/src/lib.rs"),
        format!("cp {main}/src/lib.rs src/lib.rs"),
        format!("cp {main}/src/lib.rs ./src/"),
        format!("cp {main}/src/lib.rs /tmp/7228-probe/lib.rs"),
        format!("cp {main}/src/lib.rs /private/tmp/7228-probe/lib.rs"),
        format!("cp {main}/src/lib.rs {clone}/src/lib.rs"),
        format!("cp notes.md {main}/docs/notes.md"),
        format!("sed -i '' 's/a/b/' {main}/README.md"),
        format!("cp notes.toml {main}/config/app.toml"),
        format!("cp {main}/src/lib.rs {link}/src/lib.rs"),
        format!("cp {main}/src/lib.rs {tmp}/src/lib.rs"),
        format!("rsync -a {main}/src/ {private}/src/"),
        format!("cp src/lib.rs {root}/repo-other/src/lib.rs"),
        format!("cp src/lib.rs {home}/.trusty-mpm/probe.rs"),
        "sed -i '' 's/a/b/' src/lib.rs".to_string(),
    ] {
        let stdout = run(&fx, &command, &fx.worktree, AGENT);
        assert_eq!(stdout, "", "`{command}` must stay allowed");
    }
}

/// A clone (a `.git` directory) at `<base>/<tmp>/scratchpad/clone`; the
/// returned guard removes it.
fn scratch_clone(base: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("tm-7228-")
        .tempdir_in(base)
        .expect("scratch dir");
    let clone = dir.path().join("scratchpad/clone");
    std::fs::create_dir_all(clone.join(".git")).expect("mkdir .git");
    std::fs::create_dir_all(clone.join("src")).expect("mkdir src");
    (dir, clone)
}

// #7905 composes with #7228: an operator-listed documents repo has no source
// for the boundary to protect, so a copy verb into it is not refused.
#[test]
fn pm_guard_allows_a_copy_into_a_listed_documents_repo() {
    let fx = fixture();
    std::fs::write(
        fx.home.join(".trusty-mpm/config.toml"),
        format!(
            "[pm_guard]\ndocuments_repos = [{:?}]\n",
            fx.main.display().to_string()
        ),
    )
    .expect("write config");
    let command = format!("cp src/lib.rs {}/src/lib.rs", fx.main.display());
    let stdout = run(&fx, &command, &fx.worktree, AGENT);
    assert_eq!(stdout, "", "`{command}` lands in a documents repo");
}
