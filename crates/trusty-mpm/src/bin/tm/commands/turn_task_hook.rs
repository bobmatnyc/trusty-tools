//! `tm hook --turn-task` — the PM's per-turn Intent Ledger task stamp (#11398).
//!
//! Why: the Tasks view books a turn to a task by a `Task: <id>` line in the
//! transcript. The owner ruled that the PM harness stamps the current task into
//! every turn, read from what the PM set through datg.
//! What: [`turn_task_hook`] reads the `UserPromptSubmit` payload from stdin and
//! prints the `additionalContext` response [`respond`] builds, if any.
//!
//! FAIL-OPEN, SILENT, READ-ONLY: every arm exits 0 with no stderr, no response
//! blocks the turn, and datg state is only read. A sub-agent (payload
//! `agent_id` or `CLAUDE_MPM_SUB_AGENT`) or an Architect session prints nothing.
//! Test: `tests/tm_hook_turn_task_11398.rs`, `turn_task_hook_tests.rs`.

use std::ffi::OsString;
use std::io::Read as _;

use serde_json::Value;
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::session_profile;
use trusty_mpm::core::turn_task_stamp;

/// The most stdin the hook reads; a larger payload is treated as unreadable.
const MAX_PAYLOAD_BYTES: u64 = 16 * 1024 * 1024;

/// Run the hook against the process environment and stdin. Always returns.
///
/// Test: `a_pm_turn_is_stamped_with_the_current_open_task`.
pub(crate) fn turn_task_hook() {
    let mut raw = String::new();
    let stdin = std::io::stdin()
        .take(MAX_PAYLOAD_BYTES)
        .read_to_string(&mut raw)
        .map(|_| raw);
    let env = |name: &str| std::env::var_os(name);
    let tmux = || env("TMUX_PANE").and_then(|pane| turn_task_stamp::tmux_session(&pane));
    if let Some(line) = respond(env, stdin.ok(), MpmConfig::load_default, tmux) {
        println!("{line}");
    }
}

/// The stdout line for one hook call, or `None` to print nothing.
///
/// Why: the decision lives apart from the process I/O, so every arm is
/// testable without a binary.
/// What: `None` for a sub-agent (`CLAUDE_MPM_SUB_AGENT` or a payload
/// `agent_id`), an unreadable payload, an event other than
/// `UserPromptSubmit`, no `CLAUDE_PROJECT_DIR`, an Architect session
/// or no stamp ([`turn_task_stamp::resolve_stamp`]: the dbatg store over
/// `DBATG_SESSION` and `tmux`, else the datg store over `DATG_SESSION`).
/// Otherwise the JSON of [`turn_task_stamp::hook_output`].
/// Test: `each_fail_open_arm_prints_nothing`, `a_pm_payload_is_stamped`.
pub(crate) fn respond(
    env: impl Fn(&str) -> Option<OsString>,
    stdin: Option<String>,
    config: impl FnOnce() -> MpmConfig,
    tmux: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if env(trusty_common::claude_config::CLAUDE_MPM_SUB_AGENT_ENV_VAR).is_some() {
        return None;
    }
    let payload: Value = serde_json::from_str(&stdin?).ok()?;
    if payload["hook_event_name"].as_str() != Some(turn_task_stamp::TURN_TASK_EVENT)
        || !payload["agent_id"].is_null()
    {
        return None;
    }
    let project_env = env(session_profile::PROJECT_DIR_ENV);
    let project = session_profile::hook_project_dir(project_env.clone())?;
    let profile = session_profile::hook_profile(
        env(session_profile::SESSION_PROFILE_ENV),
        project_env,
        config,
    );
    if profile.is_architect() {
        return None;
    }
    // #11398: dbatg (the installed writer) first, datg.py's store as fallback.
    let dbatg_session = env("DBATG_SESSION");
    let datg_session = env("DATG_SESSION");
    let stamp = turn_task_stamp::resolve_stamp(
        &project,
        dbatg_session.as_deref(),
        datg_session.as_deref(),
        tmux,
    )?;
    Some(turn_task_stamp::hook_output(&stamp).to_string())
}

#[cfg(test)]
#[path = "turn_task_hook_tests.rs"]
mod tests;
