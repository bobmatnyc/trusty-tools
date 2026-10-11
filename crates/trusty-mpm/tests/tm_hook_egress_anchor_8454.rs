//! The #8454 S2 trust anchors through `tm hook --egress-guard` (the real binary).
//!
//! Why: `[pm_guard] enabled = false` leaves the pm-guard hook — and the #8878
//! trust-anchor floor it runs — out of a session's settings. The egress hook
//! has no off switch, so Architect D1 makes it run that floor too. Only the
//! built binary proves the floor holds there under both bypass variables, for
//! a subagent payload, and with the guard turned off.
//! What: a scratch `$HOME` with `[pm_guard] enabled = false`, the
//! `config.yaml` anchor and a managed `claude-config/settings.json`, and a
//! project whose `.claude/settings.json` carries the egress group. Writes to
//! each anchor, and a Write dropping the project's egress entry, must print one
//! `deny` and exit 0 in every environment; a routine project edit and an
//! unrelated write must print nothing.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_egress_anchor_8454`.

use crate::common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// Every bypass shape: none, each variable, and the subagent marker.
const ENVS: &[&[(&str, &str)]] = &[
    &[],
    &[("TRUSTY_MPM_PM_UNRESTRICTED", "1")],
    &[("TRUSTY_MPM_DISABLE_HOOKS", "1")],
    &[("CLAUDE_MPM_SUB_AGENT", "1")],
    &[
        ("TRUSTY_MPM_PM_UNRESTRICTED", "1"),
        ("TRUSTY_MPM_DISABLE_HOOKS", "1"),
        ("CLAUDE_MPM_SUB_AGENT", "1"),
    ],
];

/// The egress group a tm launch writes.
fn egress_group() -> serde_json::Value {
    serde_json::json!({ "matcher": "", "hooks": [
        { "type": "command", "command": "/usr/local/bin/tm hook --egress-guard", "timeout": 10 }
    ]})
}

/// A scratch home (guard off, anchors written) and a guarded project.
struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical tempdir");
        let home = root.join("home");
        let project = root.join("project");
        let data = root.join("data");
        std::fs::create_dir_all(home.join(".trusty-mpm")).expect("mkdir root");
        std::fs::write(
            home.join(".trusty-mpm/config.toml"),
            "[pm_guard]\nenabled = false\n",
        )
        .expect("write config.toml");
        common::write_disk_threshold(&home, 100);
        let managed = home.join(".trusty-tools/trusty-mpm/claude-config");
        std::fs::create_dir_all(&managed).expect("mkdir managed");
        std::fs::write(managed.join("settings.json"), "{}").expect("write managed");
        std::fs::create_dir_all(project.join(".claude")).expect("mkdir project");
        let settings = serde_json::json!({
            "permissions": { "allow": ["Bash(ls:*)"] },
            "hooks": { "PreToolUse": [egress_group()] },
        });
        std::fs::write(
            project.join(".claude/settings.json"),
            serde_json::to_string_pretty(&settings).expect("json"),
        )
        .expect("write project settings");
        std::fs::create_dir_all(&data).expect("mkdir data");
        Self {
            _dir: dir,
            home,
            project,
            data,
        }
    }

    fn yaml(&self) -> PathBuf {
        self.home.join(".trusty-tools/trusty-mpm/config.yaml")
    }

    fn managed_settings(&self) -> PathBuf {
        self.home
            .join(".trusty-tools/trusty-mpm/claude-config/settings.json")
    }

    fn project_settings(&self) -> PathBuf {
        self.project.join(".claude/settings.json")
    }
}

/// Run `tm hook --egress-guard` over `payload` in `fx`, with `env` applied.
fn run(fx: &Fixture, payload: &serde_json::Value, env: &[(&str, &str)]) -> Output {
    let mut command: Command = common::tm_command_in(&fx.home);
    command
        .args(["--url", "http://127.0.0.1:1", "hook", "--egress-guard"])
        .env("TRUSTY_DATA_DIR_OVERRIDE", &fx.data)
        .env("CLAUDE_PROJECT_DIR", &fx.project)
        .env_remove("TRUSTY_MPM_SESSION_PROFILE")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .current_dir(&fx.project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn tm hook --egress-guard");
    let mut pipe = child.stdin.take().expect("stdin");
    let _ = pipe.write_all(payload.to_string().as_bytes());
    drop(pipe);
    child.wait_with_output().expect("wait for the guard")
}

/// A main-thread `PreToolUse` payload for `tool`.
fn payload(fx: &Fixture, tool: &str, input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "session_id": "s-8454-s2",
        "hook_event_name": "PreToolUse",
        "cwd": fx.project.display().to_string(),
        "tool_name": tool,
        "tool_input": input,
    })
}

fn write(path: &Path, content: &str) -> serde_json::Value {
    serde_json::json!({ "file_path": path.display().to_string(), "content": content })
}

/// Assert one `deny` object on stdout and exit 0.
fn assert_denied(out: &Output, case: &str) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{case}: exit 0, got {:?}; stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("{case}: stdout is not one JSON object ({e}): {stdout:?}"));
    assert_eq!(
        value["hookSpecificOutput"]["permissionDecision"], "deny",
        "{case}: {value}"
    );
    let reason = value["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default();
    assert!(reason.contains("#8454"), "{case}: {reason}");
}

/// Assert nothing on stdout and exit 0 — Claude Code's allow.
fn assert_allowed(out: &Output, case: &str) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{case}: exit 0, got {:?}", out.status);
    assert!(stdout.trim().is_empty(), "{case}: must allow: {stdout}");
}

/// 🔴 #8454 S2: writes to `config.yaml` and the managed settings are denied
/// with the pm-guard off, under both bypass variables, for a subagent marker,
/// and for a subagent payload.
#[test]
fn egress_anchor_holds_with_pm_guard_off_under_bypasses_and_subagents() {
    let fx = Fixture::new();
    for target in [fx.yaml(), fx.managed_settings()] {
        for env in ENVS {
            let main = payload(&fx, "Write", write(&target, "{}"));
            assert_denied(&run(&fx, &main, env), &format!("{target:?} {env:?}"));
            let mut dispatched = main.clone();
            dispatched["agent_id"] = serde_json::json!("agent-1");
            assert_denied(
                &run(&fx, &dispatched, env),
                &format!("{target:?} agent_id {env:?}"),
            );
        }
    }
    let rm = payload(
        &fx,
        "Bash",
        serde_json::json!({ "command": "rm -rf ~/.trusty-tools/trusty-mpm" }),
    );
    assert_denied(&run(&fx, &rm, &[]), "rm -rf of the config dir");
}

/// 🔴 #8454 S2 (Architect D2): dropping the project's egress entry is denied
/// in every environment; a routine edit and an unrelated write are allowed.
#[test]
fn egress_anchor_guards_the_project_settings_entry() {
    let fx = Fixture::new();
    let settings = fx.project_settings();
    for env in ENVS {
        let drop = payload(&fx, "Write", write(&settings, "{}"));
        assert_denied(&run(&fx, &drop, env), &format!("drop {env:?}"));
        let disable = payload(
            &fx,
            "Write",
            write(
                &fx.project.join(".claude/settings.local.json"),
                "{\"disableAllHooks\": true}",
            ),
        );
        assert_denied(&run(&fx, &disable, env), &format!("disable {env:?}"));
    }
    let routine = payload(
        &fx,
        "Edit",
        serde_json::json!({
            "file_path": settings.display().to_string(),
            "old_string": "Bash(ls:*)",
            "new_string": "Bash(pwd)",
        }),
    );
    assert_allowed(&run(&fx, &routine, &[]), "routine edit");
    let unrelated = payload(&fx, "Write", write(&fx.project.join("notes.md"), "x"));
    assert_allowed(&run(&fx, &unrelated, &[]), "unrelated write");
}
