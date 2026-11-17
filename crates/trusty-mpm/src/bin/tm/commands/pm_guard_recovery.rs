//! The fixed recovery allowlist a failing `tm hook --pm-guard` still lets
//! through (#10641, Architect blocking finding on PR #10644).
//!
//! Why: with `onFailure: "block"` and nothing else, a guard that stalls,
//! panics or errors refuses every tool, including the `tm` reinstall that
//! would fix it, and locks the session out. Writes must stay refused, so the
//! escape is a short fixed list that cannot carry another command (ruling 263).
//! What: [`recovery_allows`] answers for one payload: a read-only tool whose
//! call the normal path's secret gate allows, or a Bash command that is
//! exactly `tm doctor`, `tm install ...` or `cargo install trusty-mpm ...`
//! written as one plain command ([`is_recovery_command`]). [`failure_verdict`]
//! turns a guard failure into `None` (allow) for those and a policy-tagged
//! deny carrying [`RECOVERY_NOTE`] for everything else.
//! Test: `pm_guard_recovery_tests.rs`.

use std::path::PathBuf;

use serde_json::Value;
use trusty_mpm::core::guard_policy::GuardPolicy;

use crate::commands::pm_guard_architect_envfile::evaluate_secret_file_read_gated;
use crate::commands::pm_guard_floor::ArchitectGate;
use crate::commands::pm_guard_response::build_pm_guard_deny_response;

/// Tools that print no file the guard protects and change nothing on disk.
///
/// Why: the read-only half of the recovery list. Each call still passes the
/// normal path's secret gate (`evaluate_secret_file_read_gated`).
pub(crate) const READ_ONLY_TOOLS: &[&str] = &["Read", "Grep", "Glob", "LS", "NotebookRead"];

/// The text every failure-arm refusal carries (#10641).
pub(crate) const RECOVERY_NOTE: &str = "While the guard is failing, only read-only tools (Read, \
     Grep, Glob, LS, NotebookRead, never on a secret file) and exactly `tm doctor`, `tm install \
     ...` and `cargo install trusty-mpm --version <v> --locked` run, each as one plain command \
     with no `;`, `&&`, `|`, `$(`, redirect, env prefix or wrapper. To clear a stuck session \
     from outside it, run `tm doctor` in a terminal, reinstall with `cargo install trusty-mpm \
     --version <v> --locked`, or set `[pm_guard] enabled = false` in \
     `~/.trusty-mpm/config.toml` and relaunch the session.";

/// Whether `c` may appear in a recovery command.
///
/// Why (ruling 263): an allowlist over characters, not a denylist of
/// metacharacters, so a shell construct nobody listed cannot slip through.
/// Quotes, `$`, backticks, `;`, `&`, `|`, `<`, `>`, `(`, `)`, globs, `~`,
/// `\`, tabs and newlines are all outside it.
fn is_plain(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, ' ' | '-' | '.' | '_' | '/' | ':' | '@' | '+' | '=' | ',')
}

/// Whether the `cargo install trusty-mpm` arguments are the CLAUDE.md form.
///
/// What: only `--locked`, `--force`, `--version <v>` and `--version=<v>`; a
/// `--path`, `--git`, `--root` or any other word refuses, so the reinstall
/// cannot point at an arbitrary source.
fn cargo_args_ok(args: &[&str]) -> bool {
    let version = |v: &str| !v.is_empty() && !v.starts_with('-');
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let ok = match *arg {
            "--locked" | "--force" => true,
            "--version" => rest.next().is_some_and(|v| version(v)),
            other => other.strip_prefix("--version=").is_some_and(version),
        };
        if !ok {
            return false;
        }
    }
    true
}

/// Whether `command` is one recovery command, written as one plain command.
///
/// Why (ruling 263): the list must not launder another command. A command
/// qualifies only when every character [`is_plain`] allows and its first
/// word is the program itself — no env prefix (`FOO=1 tm doctor`) and no
/// wrapper (`timeout 5 cargo install ...`).
/// What: exactly `tm doctor`; `tm install` with any plain arguments;
/// `cargo install trusty-mpm` with [`cargo_args_ok`] arguments.
/// Test: `only_plain_recovery_commands_qualify`, `laundering_shapes_fall_back_to_deny`.
pub(crate) fn is_recovery_command(command: &str) -> bool {
    if !command.chars().all(is_plain) {
        return false;
    }
    let words: Vec<&str> = command.split(' ').filter(|w| !w.is_empty()).collect();
    match words.as_slice() {
        ["tm", "doctor"] => true,
        ["tm", "install", ..] => true,
        ["cargo", "install", "trusty-mpm", rest @ ..] => cargo_args_ok(rest),
        _ => false,
    }
}

/// Whether a failing guard may let this call through (#10641).
///
/// What: a [`READ_ONLY_TOOLS`] call that the normal path's secret gate
/// (`evaluate_secret_file_read_gated`: file names, the #8523 plist content
/// rule, kuzu stores) allows, or a Bash call whose `command`
/// [`is_recovery_command`] accepts. Anything else, a payload with no tool
/// name included, is `false`.
/// Test: `read_only_tools_pass_unless_they_name_a_secret`,
/// `a_credential_plist_is_refused_under_failure`,
/// `only_plain_recovery_commands_qualify`, `writes_never_qualify`.
pub(crate) fn recovery_allows(payload: &Value) -> bool {
    let Some(tool) = payload.get("tool_name").and_then(Value::as_str) else {
        return false;
    };
    let input = payload.get("tool_input");
    if READ_ONLY_TOOLS.contains(&tool) {
        let cwd = payload
            .get("cwd")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        // #10641: the normal path's whole secret gate (file names, the #8523
        // plist content rule, kuzu stores), so no secret rule drifts out of
        // this arm. For a read-only tool it never asks the Architect identity.
        let gate = ArchitectGate::ambient(payload);
        return evaluate_secret_file_read_gated(tool, input, &cwd, &gate).is_none();
    }
    tool == "Bash"
        && input
            .and_then(|i| i.get("command"))
            .and_then(Value::as_str)
            .is_some_and(is_recovery_command)
}

/// The verdict for a guard that failed (`how`) on `payload` (#10641).
///
/// Why: one answer for the panic, error and timeout arms, so they cannot
/// drift. A payload the guard never read gets no allowlist.
/// What: `None` (allow, nothing printed) when [`recovery_allows`] accepts the
/// payload; otherwise the `policy` deny naming `how` and [`RECOVERY_NOTE`].
/// Test: `a_failure_denies_a_write_and_allows_a_recovery_command`.
pub(crate) fn failure_verdict(
    policy: &GuardPolicy,
    how: &str,
    payload: Option<&Value>,
) -> Option<Value> {
    if payload.is_some_and(recovery_allows) {
        return None;
    }
    let reason = format!(
        "tm hook --pm-guard could not decide this tool call because {how}, so it refuses it \
         (#10600). {RECOVERY_NOTE}"
    );
    Some(build_pm_guard_deny_response(policy, &reason))
}

#[cfg(test)]
#[path = "pm_guard_recovery_tests.rs"]
mod tests;
