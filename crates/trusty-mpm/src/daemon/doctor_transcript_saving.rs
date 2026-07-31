//! Doctor probe: would a managed spawn have Claude Code transcript saving
//! DISABLED? (issue #4467)
//!
//! Why: this defect went unnoticed until Claude Code started printing a warning
//! about it. Nothing in tm reported it, and the symptom — "the session had no
//! history to resume from" — only surfaces after the session is already gone.
//! That is the same shape as #4451: a silent failure with no check capable of
//! failing, which `agent_reachability` was added for. This probe is that check.
//!
//! What it asserts is the invariant every launch-line builder encodes: each one
//! must UNSET [`crate::core::claude_env_scrub::TRANSCRIPT_SUPPRESSING_MARKER`],
//! and none may unset a
//! [`crate::core::claude_env_scrub::DELIBERATE_SPAWN_ENV`] member. Both
//! directions matter and they fail in opposite ways — under-scrub costs the
//! session its transcripts (#4467), over-scrub costs it the entire bundled agent
//! roster (#4451, via #4455) or the #2246 OAuth token.
//!
//! It covers EVERY launch line the library owns, not just the daemon one — see
//! [`launch_lines`]. The first version of this check parsed only
//! `env_bin_prefix` and therefore reported `Ok` while three other builders still
//! leaked; a check that returns `Ok` for a leaking path is worse than no check,
//! because it actively certifies the broken state.
//!
//! Both sides are read from PRODUCTION code, not restated: shell-string builders
//! are parsed with [`crate::core::claude_env_scrub::parse_env_unset_vars`], and
//! `Command` builders are read through `get_envs()`. Drop the scrub from any
//! builder and this check fails — which a check that compared the marker constant
//! against itself could never do.
//!
//! Deliberately STATIC rather than spawning a probe `claude -p` and inspecting
//! the transcript directory afterwards: a live spawn costs tens of seconds,
//! needs network and auth, and would make `tm doctor` slow and flaky on the one
//! command operators reach for when things are already broken. The static form
//! catches the whole failure class at zero cost. The set of markers live in the
//! current environment is reported as CONTEXT in the message, never as the
//! pass/fail condition — `tm doctor` run from a plain shell has no markers set,
//! and the check must still be meaningful there.
//!
//! Test: `crates/trusty-mpm/src/daemon/doctor_transcript_saving_tests.rs`.

use crate::core::claude_env_scrub::{
    DELIBERATE_SPAWN_ENV, TRANSCRIPT_SUPPRESSING_MARKER, markers_present_in_env,
    parse_env_unset_vars,
};
use crate::core::doctor::{CheckStatus, DoctorCheck};

/// Name of this check as it appears in `tm doctor` output.
const CHECK_NAME: &str = "transcript_saving";

/// A representative config dir for the probe.
///
/// Why: `env_bin_prefix` emits `CLAUDE_CONFIG_DIR` as an ASSIGNMENT, so passing
/// one is what lets the over-scrub branch see whether the variable was (wrongly)
/// added to the UNSET list instead. A bare `None` would make that branch
/// unreachable.
/// What: a fixed non-existent path — nothing is read from disk.
/// Test: exercised via [`launch_lines`].
const PROBE_CONFIG_DIR: &str = "/managed/claude-config";

/// A representative OAuth token for the probe.
///
/// Why: `CLAUDE_CODE_OAUTH_TOKEN` is the other [`DELIBERATE_SPAWN_ENV`] member,
/// and it only appears on the emitted prefix when a token is supplied. Passing
/// `None` would make an over-scrub of it invisible to this check.
/// What: an obvious non-credential placeholder.
/// Test: `over_scrub_of_the_oauth_token_is_detected`.
const PROBE_TOKEN: &str = "probe-token-not-a-credential";

/// Every launch-line builder that spawns a `claude`, paired with the variables it
/// unsets.
///
/// Why (review follow-up on #4467): the first version of this check parsed only
/// `runtime::claude_code::env_bin_prefix`, so it reported `Ok` while
/// `model_inject::build_claude_command` (`tm launch` / `tm connect` /
/// delegations), `standalone::run::build_launch_command` (`tm run`) and
/// `daemon::spawn_command::relaunch_command` all still leaked. A check that
/// returns `Ok` for a leaking path is worse than no check: it actively certifies
/// the broken state. Enumerating the builders here — and reading each one's real
/// output — is what makes the verdict cover what its message claims.
/// What: `(label, unset-variable-names)` per builder. Shell-string builders are
/// parsed with [`parse_env_unset_vars`]; `Command` builders report their
/// `env_remove`d keys, which `get_envs()` surfaces as `(key, None)`.
///
/// `bin/tm`'s `build_inplace_exec_command` is deliberately absent: it lives in
/// the `tm` BINARY crate, which this library cannot reference. It is covered by
/// its own `inplace_exec_command_scrubs_inherited_session_markers` test, and the
/// message below names the gap rather than implying coverage.
/// Test: `every_production_launch_line_preserves_transcript_saving`,
/// `launch_lines_covers_every_builder`.
fn launch_lines() -> Vec<(&'static str, Vec<String>)> {
    let config_dir = std::path::PathBuf::from(PROBE_CONFIG_DIR);

    // Shell-string builders: parse the `-u` operands out of the `env` prefix.
    let prefix =
        crate::runtime::env_bin_prefix("claude", Some(&config_dir), Some(PROBE_TOKEN));
    let launch_line = crate::core::model_inject::build_claude_command(None, None);
    let relaunch_line = crate::daemon::spawn_command::relaunch_command();

    // `Command` builders: an `env_remove` shows up as a `None` value.
    let run_cmd = crate::core::standalone::run::build_launch_command(
        std::path::Path::new("/probe/repo"),
        &config_dir,
        None,
    );
    let stream_cmd =
        crate::control::backend::stream_json::build_claude_command(std::path::Path::new("/probe"), None);

    vec![
        (
            "runtime::claude_code::env_bin_prefix (daemon spawn + resume)",
            owned(parse_env_unset_vars(&prefix)),
        ),
        (
            "core::model_inject::build_claude_command (tm launch / connect / delegate)",
            owned(parse_env_unset_vars(&launch_line)),
        ),
        (
            "daemon::spawn_command::relaunch_command (pane relaunch)",
            owned(parse_env_unset_vars(&relaunch_line)),
        ),
        (
            "core::standalone::run::build_launch_command (tm run)",
            removed_env_keys(&run_cmd),
        ),
        (
            "control::backend::stream_json::build_claude_command (headless)",
            removed_env_keys(&stream_cmd),
        ),
    ]
}

/// Borrow-to-owned helper so both builder shapes share one verdict type.
///
/// Why: [`parse_env_unset_vars`] borrows from a local `String`, which cannot
/// escape [`launch_lines`]; owning the names keeps the two shapes uniform without
/// leaking lifetimes into the verdict signature.
/// What: copies each `&str` into a `String`.
/// Test: exercised via [`launch_lines`].
fn owned(names: Vec<&str>) -> Vec<String> {
    names.into_iter().map(str::to_owned).collect()
}

/// The variables a [`std::process::Command`] explicitly REMOVES from its child env.
///
/// Why: the `Command`-based launch paths express the scrub as `env_remove` rather
/// than an `env -u` flag, so the check needs the equivalent view of them. Reading
/// `get_envs()` — rather than trusting that `scrub_command` was called — is what
/// keeps this probe reading production behaviour.
/// What: keys whose value is `None` (an `env_remove`), lossily stringified.
/// Test: exercised via [`launch_lines`]; `tm run` coverage is asserted by
/// `every_production_launch_line_preserves_transcript_saving`.
fn removed_env_keys(cmd: &std::process::Command) -> Vec<String> {
    cmd.get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .collect()
}

/// Probe whether EVERY `tm` launch line preserves Claude Code transcript saving.
///
/// Why: see the module doc. Hard `Fail`, not `Warn`: when it trips, sessions on
/// the offending path are unrecoverable — no native `--resume`, no `--continue`,
/// no `/rewind`, and absent from `--resume` listings — so a session that dies
/// loses everything since tm's last summary-only snapshot. That is data loss, not
/// a configuration preference.
/// What: runs [`verdict`] over every [`launch_lines`] entry and returns the FIRST
/// failure, so one leaking builder cannot be averaged away by four correct ones.
/// Test: `every_production_launch_line_preserves_transcript_saving`.
pub(super) fn check_transcript_saving() -> DoctorCheck {
    let present = markers_present_in_env();
    let lines = launch_lines();
    let covered = lines.len();
    for (label, unset) in &lines {
        let names: Vec<&str> = unset.iter().map(String::as_str).collect();
        let check = verdict(label, &names, &present);
        if check.status != CheckStatus::Ok {
            return check;
        }
    }
    let context = if present.is_empty() {
        "no inherited markers in this environment".to_owned()
    } else {
        format!(
            "inherited here and scrubbed: {} — this `tm` is itself running with the leak \
             present",
            present.join(", ")
        )
    };
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Ok,
        format!(
            "all {covered} library launch line(s) unset `{TRANSCRIPT_SUPPRESSING_MARKER}` while \
             keeping [{}], so transcript saving and native --resume stay available; {context} \
             (the bare-`tm` in-place relaunch lives in the `tm` binary and is covered by its \
             own test, not by this check)",
            DELIBERATE_SPAWN_ENV.join(", ")
        ),
    )
}

/// Pure verdict over ONE launch line's unset list.
///
/// Why: separated from [`check_transcript_saving`] so BOTH failure branches are
/// directly testable. The real launch lines come from production code, so a test
/// that could only call the wrapper would exercise the happy path and silently
/// lose the regression guard — the same blind spot #4467 itself is. `path` is
/// threaded through so a failure names WHICH builder leaks; a message that said
/// only "a managed spawn" would leave an operator grepping five call sites.
/// What: `Fail` when `unset` contains ANY [`DELIBERATE_SPAWN_ENV`] member (that
/// variable is set on purpose — unsetting `CLAUDE_CONFIG_DIR` restores #4451),
/// then `Fail` when it omits [`TRANSCRIPT_SUPPRESSING_MARKER`] (transcripts would
/// be lost). `Ok` otherwise. `present` is context for the message only.
///
/// Iterating `DELIBERATE_SPAWN_ENV` rather than testing the literal
/// `"CLAUDE_CONFIG_DIR"` is what makes an over-scrub of the OTHER member
/// (`CLAUDE_CODE_OAUTH_TOKEN`) visible too — the earlier literal form could not
/// see it.
/// Test: `ok_when_the_marker_is_scrubbed`,
/// `fails_when_the_suppressing_marker_is_not_scrubbed`,
/// `fails_when_the_scrub_would_take_the_config_dir`,
/// `over_scrub_of_the_oauth_token_is_detected`,
/// `failure_is_a_hard_fail_not_a_warn`,
/// `failure_names_the_leaking_path`,
/// `ok_message_reports_markers_live_in_the_environment`,
/// `config_dir_over_scrub_takes_precedence_in_the_message`.
fn verdict(path: &str, unset: &[&str], present: &[&'static str]) -> DoctorCheck {
    if let Some(deliberate) = DELIBERATE_SPAWN_ENV
        .iter()
        .find(|name| unset.contains(*name))
    {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!(
                "`{path}` would UNSET `{deliberate}`, which a managed spawn sets on \
                 PURPOSE. For CLAUDE_CONFIG_DIR that relocation is what puts the bundled \
                 agent roster in the `user` settings tier the spawn's `--setting-sources` \
                 flag loads (#4455), so unsetting it restores issue #4451: every \
                 delegation degrades to `general-purpose` with `Agent type '<name>' not \
                 found`. For CLAUDE_CODE_OAUTH_TOKEN it restores the #2246 Keychain login \
                 loop. Remove `{deliberate}` from \
                 `core::claude_env_scrub::INHERITED_SESSION_MARKERS`."
            ),
        );
    }
    if !unset.contains(&TRANSCRIPT_SUPPRESSING_MARKER) {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!(
                "`{path}` does NOT unset `{TRANSCRIPT_SUPPRESSING_MARKER}`, so when `tm` is \
                 invoked from inside a Claude Code session the `claude` it spawns inherits \
                 it and turns session persistence OFF (\"Transcript saving is off — \
                 inherited {TRANSCRIPT_SUPPRESSING_MARKER} marker\"). Sessions on that path \
                 then have no native --resume/--continue/rewind recovery and never appear \
                 in `--resume`, so if one dies everything since tm's last snapshot is lost \
                 (issue #4467). Scrub it via `core::claude_env_scrub`. Currently unsets: \
                 [{}].",
                unset.join(", ")
            ),
        );
    }
    let context = if present.is_empty() {
        "no inherited markers in this environment".to_owned()
    } else {
        format!(
            "inherited here and scrubbed: {} — this `tm` is itself running with the leak \
             present",
            present.join(", ")
        )
    };
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Ok,
        format!(
            "`{path}` unsets [{}] while keeping [{}], so transcript saving and native \
             --resume stay available; {context}",
            unset.join(", "),
            DELIBERATE_SPAWN_ENV.join(", ")
        ),
    )
}

#[cfg(test)]
#[path = "doctor_transcript_saving_tests.rs"]
mod tests;
