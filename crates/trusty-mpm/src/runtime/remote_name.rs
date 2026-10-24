//! A managed session's remote name is its tmux session name (#9025).
//!
//! Why: owner rule (2026-10-01): the name a tm-managed Claude Code session
//! shows peers — the Remote Control session name and the `ListAgents` name —
//! must equal its tmux session name, with no manual `/rename`. Without a name
//! Claude Code generates one (`trusty-tools-a7`), so the operator could not
//! tell which tmux session a remote entry belonged to.
//! What: [`ensure_name_arg`] is the one argv rule. [`named_after_session`]
//! applies it to a launch spec at the two spec delivery points,
//! `managed_launch::deliver_in` (the daemon's spawn and resume) and
//! `cli_launch::send_spec_launch_with` (the CLI and fleet launches); the
//! bare-`tm` in-place relaunch, which execs `claude` without a spec, applies
//! it through `InPlaceResumeCommand::named_after_session`. A launch path that
//! builds its own argv outside those three must call the rule itself. A later
//! `tmux rename-session` is not followed live; [`remote_name_drifts`] finds
//! the mismatch from Claude Code's own registry, and the `session_names`
//! doctor row reports it.
//! Test: `remote_name_tests.rs`.

use std::collections::HashMap;

use serde::Deserialize;

use super::launch_spec::LaunchSpec;

/// The Claude Code flag that sets a session's display name.
pub const NAME_FLAG: &str = "--name";

/// `spec` with `--name <session>` as its first argument.
///
/// Why: see the module doc; the tmux name is only known where the spec is
/// delivered, so the choke points call this rather than each builder.
/// What: a copy of `spec`. Unchanged when its argv is `attach <id>` —
/// `claude attach` accepts no options, and the background session it re-enters
/// keeps the name it was launched with — or when it already names itself
/// (`--name`/`-n`). Otherwise `--name <session>` is inserted at the front, so
/// it can never be read as the value of a trailing optional flag.
/// Test: `a_spec_is_named_after_its_session`,
/// `an_attach_or_an_already_named_spec_is_left_alone`.
pub fn named_after_session(spec: &LaunchSpec, session: &str) -> LaunchSpec {
    let mut named = spec.clone();
    ensure_name_arg(&mut named.args, session);
    named
}

/// Put `--name <session>` at the front of `args` unless they already name it.
///
/// Why: the one argv rule, shared by [`named_after_session`] and the bare-`tm`
/// in-place relaunch, which builds a bare argv rather than a [`LaunchSpec`].
/// What: leaves `args` unchanged when they are `attach <id>` or already carry
/// `--name`/`-n`; otherwise inserts the pair at index 0.
/// Test: `a_spec_is_named_after_its_session`,
/// `an_attach_or_an_already_named_spec_is_left_alone`.
pub fn ensure_name_arg(args: &mut Vec<String>, session: &str) {
    let attach = args.first().is_some_and(|a| a == "attach");
    let has_name = args.iter().any(|a| a == NAME_FLAG || a == "-n");
    if !attach && !has_name {
        // #9025: the remote name equals the tmux session name.
        args.splice(0..0, [NAME_FLAG.to_owned(), session.to_owned()]);
    }
}

/// One managed session whose remote name is not its tmux session name.
///
/// What: the tmux session, the `claude` pid running in it, and the name
/// Claude Code's registry reports for that pid.
/// Test: `a_renamed_tmux_session_is_a_remote_name_drift`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteNameDrift {
    /// The live tmux session name.
    pub session: String,
    /// The `claude` process in that session.
    pub pid: u32,
    /// The name peers see for it.
    pub remote: String,
}

/// One `claude agents --json` entry; only the fields this check reads.
#[derive(Deserialize)]
struct RegistryEntry {
    pid: Option<u32>,
    name: Option<String>,
}

/// Every session in `sessions` whose `claude` reports another remote name.
///
/// Why: a `tmux rename-session` after launch leaves the remote name behind;
/// this is how `tm doctor` sees it (closure condition 2 of #9025).
/// What: `claude_pid` maps a session to the `claude` in it; a session without
/// one is skipped. `registry` is read only when some session has a `claude`,
/// so a host with none never runs `claude agents --json`. A pid the registry
/// does not list, or lists with no name, is skipped: nothing proves a
/// mismatch.
///
/// # Errors
///
/// The `registry` reason, or a parse failure of its JSON.
/// Test: `a_renamed_tmux_session_is_a_remote_name_drift`,
/// `the_registry_is_not_read_without_a_claude`,
/// `an_unreadable_registry_is_an_error`.
pub fn remote_name_drifts(
    sessions: &[String],
    claude_pid: impl Fn(&str) -> Option<u32>,
    registry: impl FnOnce() -> Result<String, String>,
) -> Result<Vec<RemoteNameDrift>, String> {
    let running: Vec<(&String, u32)> = sessions
        .iter()
        .filter_map(|s| claude_pid(s).map(|pid| (s, pid)))
        .collect();
    if running.is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<RegistryEntry> = serde_json::from_str(&registry()?)
        .map_err(|e| format!("`claude agents --json` was not readable: {e}"))?;
    let by_pid: HashMap<u32, String> = entries
        .into_iter()
        .filter_map(|e| Some((e.pid?, e.name?)))
        .collect();
    Ok(running
        .into_iter()
        .filter_map(|(session, pid)| {
            let remote = by_pid.get(&pid)?;
            (remote != session).then(|| RemoteNameDrift {
                session: session.clone(),
                pid,
                remote: remote.clone(),
            })
        })
        .collect())
}

/// Claude Code's live-session registry, read through the resolved `claude`.
///
/// What: [`super::claude_code::read_agent_registry`]; `Err` says why it could
/// not be read.
/// Test: covered by the pure [`remote_name_drifts`] tests; this is the I/O.
pub fn read_registry() -> Result<String, String> {
    super::claude_code::read_agent_registry()
}

#[cfg(test)]
#[path = "remote_name_tests.rs"]
mod tests;
