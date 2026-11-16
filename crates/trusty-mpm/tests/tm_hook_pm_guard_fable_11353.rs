//! #11353, end to end: GP-051 refuses a PM dispatch of a capped role on Fable.
//!
//! Why: the cap only matters if the built hook applies it on the live
//! `Agent` path, after the dispatch roster check that gates it.
//! What: spawns `tm hook --pm-guard` with a scratch `$HOME` (disk threshold
//! 100%, repository content staged), a working directory whose
//! `.claude/agents/` deploys `qa` and `rust-engineer`, and an unreachable
//! daemon. A `qa` dispatch with `model: "fable"` is denied naming GP-051; a
//! `rust-engineer` dispatch with the same model is allowed.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_fable_11353::`.

use crate::common;

use std::io::Write;
use std::process::Stdio;

use serde_json::json;
use trusty_mpm::core::guard_policy;

/// Run the guard on an `Agent` dispatch of `agent` asking for `model`.
fn dispatch(agent: &str, model: &str) -> String {
    let home = tempfile::tempdir().expect("home");
    let work = tempfile::tempdir().expect("work");
    common::write_disk_threshold(home.path(), 100);
    common::stage_repo_content(home.path());
    let agents = work.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents).expect("mkdir agents");
    std::fs::write(agents.join("qa.md"), "---\nname: qa\nrole: qa\n---\nbody\n").expect("qa");
    std::fs::write(
        agents.join("rust-engineer.md"),
        "---\nname: rust-engineer\nrole: engineer\n---\nbody\n",
    )
    .expect("rust-engineer");
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s-11353",
        "cwd": work.path().display().to_string(),
        "tool_name": "Agent",
        "tool_input": { "subagent_type": agent, "model": model, "prompt": "go" },
    })
    .to_string();
    let mut child = common::tm_command_in(home.path())
        .args(["--url", "http://127.0.0.1:1", "hook", "--pm-guard"])
        .current_dir(work.path())
        .env_remove("CLAUDE_CODE_SUBAGENT_MODEL")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("TRUSTY_MPM_SESSION_PROFILE")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `tm hook --pm-guard`");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "the guard exits 0: {out:?}");
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

#[test]
fn a_qa_dispatch_on_fable_is_denied_naming_gp_051() {
    let stdout = dispatch("qa", "fable");
    assert!(stdout.contains("\"deny\""), "{stdout}");
    assert!(stdout.contains(&guard_policy::FABLE_CAP.tag()), "{stdout}");
}

#[test]
fn a_rust_engineer_dispatch_on_fable_is_allowed() {
    assert_eq!(dispatch("rust-engineer", "fable").trim(), "");
}
