//! End-to-end proof that the trust-anchor floor binds the bypasses (#8878 D8).
//!
//! Why: owner ruling 2026-09-29 — a write to `~/.trusty-mpm/config.toml` is
//! denied for every session but the Architect's main thread, and the deny
//! holds under `TRUSTY_MPM_PM_UNRESTRICTED` and `TRUSTY_MPM_DISABLE_HOOKS`,
//! which return before every other rule. Only the built binary proves the
//! stdin read moved ahead of them.
//! What: spawns `tm hook --pm-guard` with a scratch `$HOME` holding the anchor
//! and an unreachable daemon, and checks each bypass, an unreadable payload
//! under each bypass, and the Architect's main thread and subagent.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_trust_anchor_8878::`.

use crate::common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// The bypass variables, and `None` for no bypass.
const BYPASSES: [Option<(&str, &str)>; 3] = [
    None,
    Some(("TRUSTY_MPM_PM_UNRESTRICTED", "1")),
    Some(("TRUSTY_MPM_DISABLE_HOOKS", "1")),
];

/// A scratch home with the anchor, and an Architect project allow-listed in it.
struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical tempdir");
        let home = root.join("home");
        let project = root.join("architect");
        std::fs::create_dir_all(home.join(".trusty-mpm")).expect("mkdir root");
        std::fs::create_dir_all(&project).expect("mkdir project");
        std::fs::write(
            project.join(".trusty-mpm.toml"),
            "profile = \"supervisor\"\n",
        )
        .expect("write profile");
        std::fs::write(
            home.join(".trusty-mpm/config.toml"),
            format!(
                "[supervisor]\nprojects = [{:?}]\n",
                project.display().to_string()
            ),
        )
        .expect("write anchor");
        common::write_disk_threshold(&home, 100);
        Self {
            _dir: dir,
            home,
            project,
        }
    }

    fn anchor(&self) -> PathBuf {
        self.home.join(".trusty-mpm/config.toml")
    }
}

/// Run the guard with raw `stdin`, the named env pairs, and `stamp`.
fn run(fx: &Fixture, stdin: &str, env: &[(&str, &str)], stamp: Option<&str>) -> String {
    let mut cmd = common::tm_command_in(&fx.home);
    cmd.args(["--url", "http://127.0.0.1:1", "hook", "--pm-guard"])
        .current_dir(&fx.project)
        .env("CLAUDE_PROJECT_DIR", &fx.project)
        .env_remove("TRUSTY_MPM_SESSION_PROFILE")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT");
    for (k, v) in env {
        cmd.env(k, v);
    }
    if let Some(stamp) = stamp {
        cmd.env("TRUSTY_MPM_SESSION_PROFILE", stamp);
    }
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
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "the guard exits 0: {out:?}");
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

/// A main-thread `Write` payload targeting `path`.
fn write_payload(fx: &Fixture, path: &Path) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s-1",
        "cwd": fx.project.display().to_string(),
        "tool_name": "Write",
        "tool_input": { "file_path": path.display().to_string(), "content": "x" },
    })
}

#[test]
fn the_floor_denies_an_anchor_write_under_each_bypass() {
    let fx = Fixture::new();
    let stdin = write_payload(&fx, &fx.anchor()).to_string();
    for bypass in BYPASSES {
        let env: Vec<(&str, &str)> = bypass.into_iter().collect();
        let out = run(&fx, &stdin, &env, None);
        assert!(out.contains("\"deny\""), "{bypass:?}: {out}");
        assert!(out.contains("#8878"), "{bypass:?}: {out}");
    }
    // A bypass still lifts every other rule: a write elsewhere passes.
    let other = write_payload(&fx, &fx.project.join("notes.md")).to_string();
    for bypass in BYPASSES.into_iter().flatten() {
        assert_eq!(run(&fx, &other, &[bypass], None).trim(), "", "{bypass:?}");
    }
}

#[test]
fn an_unreadable_payload_denies_under_each_bypass() {
    let fx = Fixture::new();
    for bypass in BYPASSES.into_iter().flatten() {
        for stdin in ["", "not json", "[1]"] {
            let out = run(&fx, stdin, &[bypass], None);
            assert!(out.contains("\"deny\""), "{bypass:?} {stdin:?}: {out}");
        }
    }
}

#[test]
fn the_architect_main_thread_writes_and_its_subagent_does_not() {
    let fx = Fixture::new();
    let main = write_payload(&fx, &fx.anchor());
    assert_eq!(
        run(&fx, &main.to_string(), &[], Some("supervisor")).trim(),
        ""
    );
    let mut sub = main.clone();
    sub["agent_id"] = serde_json::json!("agent-7");
    let out = run(&fx, &sub.to_string(), &[], Some("supervisor"));
    assert!(out.contains("#8878"), "{out}");
    // A PM stamp in the same directory is not the Architect.
    let out = run(&fx, &main.to_string(), &[], Some("pm"));
    assert!(out.contains("#8878"), "{out}");
}
