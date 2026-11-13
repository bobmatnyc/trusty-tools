//! `tm hook --standing-rules` — the Architect's standing-rules injection (#10960).
//!
//! Why: an Architect session lost the owner's standing rules after each
//! `/clear` and resume. This hook puts the owner-maintained
//! `local/standing-rules.md` into the model's context on `SessionStart` and on
//! every `UserPromptSubmit`.
//! What: [`standing_rules_hook`] reads the payload from stdin and prints the
//! response [`respond`] builds; a warning also goes to stderr.
//!
//! FAIL-OPEN, TOTAL: every arm exits 0 and no response blocks the turn. A
//! session that is not a granted Architect (launch stamp, allowlist and
//! project file, [`session_profile::hook_profile`]) or a sub-agent prints
//! nothing, so a registration that outlived its profile injects nothing.
//! Test: `tests/tm_hook_standing_rules_10960.rs`.

use std::ffi::OsString;
use std::io::Read as _;

use serde_json::Value;
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::session_profile;
use trusty_mpm::core::standing_rules;

/// The most stdin the hook reads; a prompt payload larger than this is not JSON
/// the hook can use, and is treated as unreadable.
const MAX_PAYLOAD_BYTES: u64 = 16 * 1024 * 1024;

/// What the hook prints: the stdout response and the stderr warning.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Response {
    /// The JSON line for stdout, if any.
    pub stdout: Option<String>,
    /// The warning line for stderr, if any.
    pub stderr: Option<String>,
}

/// Run the hook against the process environment and stdin. Always returns.
///
/// Why: see the module doc.
/// What: [`respond`] over `std::env::var_os`, the raw stdin text and
/// [`MpmConfig::load_default`]; prints what it returns.
/// Test: `an_architect_session_gets_the_rules_at_resume_and_per_turn`.
pub(crate) fn standing_rules_hook() {
    let mut raw = String::new();
    let stdin = std::io::stdin()
        .take(MAX_PAYLOAD_BYTES)
        .read_to_string(&mut raw)
        .map(|_| raw);
    let response = respond(
        |name: &str| std::env::var_os(name),
        stdin.ok(),
        MpmConfig::load_default,
    );
    if let Some(line) = response.stderr {
        eprintln!("{line}");
    }
    if let Some(line) = response.stdout {
        println!("{line}");
    }
}

/// The response for one hook call.
///
/// Why: the decision lives apart from the process I/O, so every arm reads in
/// one place.
/// What: a sub-agent (`CLAUDE_MPM_SUB_AGENT`) or a session
/// [`session_profile::hook_profile`] does not resolve to the Architect → an
/// empty [`Response`]; `config` is read only after the stamp matches. An
/// unreadable or non-JSON payload → a stderr warning only. Otherwise
/// [`standing_rules::load`] over `CLAUDE_PROJECT_DIR` and
/// [`standing_rules::hook_output`] for the payload's `hook_event_name`; a
/// `systemMessage` in that output is also returned as the stderr line.
/// Test: `a_pm_session_gets_no_rules`, `an_unreadable_rules_file_warns_and_the_turn_proceeds`,
/// `an_unreadable_payload_warns_and_injects_nothing`.
pub(crate) fn respond(
    env: impl Fn(&str) -> Option<OsString>,
    stdin: Option<String>,
    config: impl FnOnce() -> MpmConfig,
) -> Response {
    if env(trusty_common::claude_config::CLAUDE_MPM_SUB_AGENT_ENV_VAR).is_some() {
        return Response::default();
    }
    let project_env = env(session_profile::PROJECT_DIR_ENV);
    let profile = session_profile::hook_profile(
        env(session_profile::SESSION_PROFILE_ENV),
        project_env.clone(),
        config,
    );
    let Some(project_dir) = session_profile::hook_project_dir(project_env) else {
        return Response::default();
    };
    if !profile.is_architect() {
        return Response::default();
    }
    let Some(payload) = stdin.and_then(|raw| serde_json::from_str::<Value>(&raw).ok()) else {
        return Response {
            stdout: None,
            stderr: Some(
                "tm: standing rules hook got no readable payload; no rules injected".to_owned(),
            ),
        };
    };
    let event = payload["hook_event_name"].as_str().unwrap_or_default();
    let Some(out) = standing_rules::hook_output(event, &standing_rules::load(&project_dir)) else {
        return Response::default();
    };
    Response {
        stderr: out["systemMessage"].as_str().map(str::to_owned),
        stdout: Some(out.to_string()),
    }
}

#[cfg(test)]
#[path = "standing_rules_hook_tests.rs"]
mod tests;
