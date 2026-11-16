//! #11353, end to end: GP-051 refuses a PM dispatch of a capped role on Fable.
//!
//! Why: the cap only matters if the built hook applies it on the live
//! `Agent` path, after the dispatch roster check that gates it.
//! What: spawns `tm hook --pm-guard` with a scratch `$HOME` (disk threshold
//! 100%, repository content staged), a working directory whose
//! `.claude/agents/` deploys `qa` and `rust-engineer`, and an unreachable
//! daemon. A `qa` dispatch with `model: "fable"` is denied naming GP-051; a
//! `rust-engineer` dispatch with the same model is allowed. A `qa` dispatch
//! with no `model` inherits the managed launch's `TRUSTY_MPM_LAUNCH_MODEL`
//! stamp: admitted on Opus, denied on Fable, denied with no stamp.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_fable_11353::`.

use crate::common;

use std::io::Write;
use std::process::Stdio;

use serde_json::json;
use trusty_mpm::core::guard_policy;

/// Run the guard on an `Agent` dispatch of `agent` asking for `model`.
fn dispatch(agent: &str, model: &str) -> String {
    dispatch_with(agent, Some(model), None)
}

/// [`dispatch`] with an optional `model` and launch-model stamp.
fn dispatch_with(agent: &str, model: Option<&str>, launch_model: Option<&str>) -> String {
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
    let mut input = json!({ "subagent_type": agent, "prompt": "go" });
    if let Some(model) = model {
        input["model"] = json!(model);
    }
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s-11353",
        "cwd": work.path().display().to_string(),
        "tool_name": "Agent",
        "tool_input": input,
    })
    .to_string();
    let mut command = common::tm_command_in(home.path());
    match launch_model {
        Some(m) => command.env("TRUSTY_MPM_LAUNCH_MODEL", m),
        None => command.env_remove("TRUSTY_MPM_LAUNCH_MODEL"),
    };
    let mut child = command
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

/// #11353: a managed launch's model stamp names the parent an `inherit`
/// dispatch runs on, before any statusline render records it.
#[test]
fn an_inherit_dispatch_follows_the_launch_model_stamp() {
    let opus = dispatch_with("qa", None, Some("claude-opus-5-5"));
    assert_eq!(opus.trim(), "", "a non-Fable launch admits qa: {opus}");
    let fable = dispatch_with("qa", None, Some("claude-fable-5-2"));
    assert!(fable.contains("\"deny\""), "{fable}");
    assert!(fable.contains(&guard_policy::FABLE_CAP.tag()), "{fable}");
    let unmanaged = dispatch_with("qa", None, None);
    assert!(
        unmanaged.contains("\"deny\""),
        "no stamp fails closed: {unmanaged}"
    );
}
