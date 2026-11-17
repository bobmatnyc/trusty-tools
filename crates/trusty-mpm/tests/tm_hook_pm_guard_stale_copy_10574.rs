//! End-to-end proof of the #10574 stale-copy guard, fixture F2.
//!
//! Why: only the built binary proves the guard reads `[stale_copy]` from the
//! project's `.trusty-mpm.toml`, records a `Read` in one hook process and
//! judges the `Write` in the next, for the main thread and for a subagent.
//! What: a scratch project with `docs/scenarios.md` v1 registered and
//! `docs/rules.md` locked; the owner's unmarked edit lands between the read
//! and the write. Each variant asserts the deny code on stdout.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_stale_copy_10574::`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::common;

struct Fx {
    _dir: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
}

impl Fx {
    fn new(config: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical");
        let home = root.join("home");
        let project = root.join("proj");
        std::fs::create_dir_all(home.join(".trusty-mpm")).expect("mkdir home");
        std::fs::create_dir_all(project.join("docs")).expect("mkdir docs");
        std::fs::write(project.join(".trusty-mpm.toml"), config).expect("config");
        std::fs::write(project.join("docs/scenarios.md"), "S1\nS4 Result: ok\n").expect("v1");
        std::fs::write(project.join("docs/rules.md"), "R1 critical\n").expect("rules");
        Self {
            _dir: dir,
            home,
            project,
        }
    }

    fn file(&self, rel: &str) -> String {
        self.project.join(rel).display().to_string()
    }

    /// Run the guard on one tool call; `agent` marks a subagent payload.
    fn run(&self, tool: &str, input: serde_json::Value, agent: bool) -> String {
        let mut payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s-10574",
            "cwd": self.project.display().to_string(),
            "tool_name": tool,
            "tool_input": input,
        });
        if agent {
            payload["agent_id"] = serde_json::json!("agent-10574");
        }
        let mut cmd = common::tm_command_in(&self.home);
        cmd.args(["--url", "http://127.0.0.1:1", "hook", "--pm-guard"])
            .current_dir(&self.project)
            .env("CLAUDE_PROJECT_DIR", &self.project)
            .env_remove("TRUSTY_MPM_SESSION_PROFILE")
            .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
            .env_remove("CLAUDE_MPM_SUB_AGENT")
            .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
            .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        let mut child = cmd
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
        let out = child.wait_with_output().expect("wait");
        assert!(out.status.success(), "the guard exits 0: {out:?}");
        String::from_utf8(out.stdout).expect("utf-8")
    }

    fn read(&self, rel: &str, agent: bool) {
        let out = self.run(
            "Read",
            serde_json::json!({ "file_path": self.file(rel) }),
            agent,
        );
        assert!(!out.contains("\"deny\""), "a Read is never denied: {out}");
    }

    fn write(&self, rel: &str, content: &str, agent: bool) -> String {
        let input = serde_json::json!({ "file_path": self.file(rel), "content": content });
        self.run("Write", input, agent)
    }

    fn owner_edit(&self) {
        std::fs::write(
            self.project.join("docs/scenarios.md"),
            "S1\nS4 Result: ok UNMARKED-7F3\n",
        )
        .expect("owner edit");
    }
}

const CONFIG: &str = "[stale_copy]\npaths = [\"docs/*.md\"]\nlocked = [\"docs/rules.md\"]\n";

fn assert_denied(out: &str, code: &str, policy: &str) {
    assert!(out.contains("\"deny\""), "expected a deny: {out}");
    assert!(out.contains(code), "expected {code}: {out}");
    assert!(out.contains(policy), "expected {policy}: {out}");
}

fn path_exists(p: &Path) -> bool {
    p.exists()
}

#[test]
fn f2_in_place_write_after_an_owner_edit_is_denied_for_pm_and_agent() {
    for agent in [false, true] {
        let fx = Fx::new(CONFIG);
        fx.read("docs/scenarios.md", agent);
        assert!(
            path_exists(&fx.project.join(".trusty-mpm/stale-copy")),
            "the Read wrote an observed-version record"
        );
        fx.owner_edit();
        let out = fx.write("docs/scenarios.md", "S1\nv2\n", agent);
        assert_denied(&out, "STALE_INPUT_HASH", "GP-052");
    }
}

#[test]
fn f2_bash_redirect_over_an_owner_edit_is_denied() {
    let fx = Fx::new(CONFIG);
    fx.read("docs/scenarios.md", true);
    fx.owner_edit();
    let out = fx.run(
        "Bash",
        serde_json::json!({ "command": "cat new.md > docs/scenarios.md" }),
        true,
    );
    assert_denied(&out, "STALE_INPUT_HASH", "GP-052");
}

#[test]
fn f2_locked_path_is_denied_whatever_the_hash() {
    let fx = Fx::new(CONFIG);
    fx.read("docs/rules.md", true);
    let out = fx.write("docs/rules.md", "R1 critical\n", true);
    assert_denied(&out, "LOCKED", "GP-053");
}

#[test]
fn f2_late_edit_after_the_last_write_is_detected() {
    let fx = Fx::new(CONFIG);
    fx.read("docs/scenarios.md", true);
    assert!(
        !fx.write("docs/scenarios.md", "S1\nv2\n", true)
            .contains("\"deny\"")
    );
    std::fs::write(fx.project.join("docs/scenarios.md"), "S1\nv2\nlate\n").expect("late");
    let out = fx.write("docs/scenarios.md", "S1\nv3\n", true);
    assert_denied(&out, "STALE_INPUT_HASH", "GP-052");
}

#[test]
fn a_fresh_read_allows_the_write() {
    let fx = Fx::new(CONFIG);
    fx.read("docs/scenarios.md", true);
    let out = fx.write("docs/scenarios.md", "S1\nv2\n", true);
    assert!(!out.contains("\"deny\""), "{out}");
}

#[test]
fn a_malformed_config_leaves_the_guard_inactive() {
    let fx = Fx::new("[stale_copy]\npaths = 3\n");
    let out = fx.write("docs/scenarios.md", "never read", true);
    assert!(!out.contains("STALE_INPUT_HASH"), "{out}");
}
