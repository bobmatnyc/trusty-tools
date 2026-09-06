//! Live-background-session liveness probe and the `attach` relaunch shape (#6863).
//!
//! Why: `spawn_resume` chose `--resume <id>` from a filesystem check alone
//! (`session_id_exists` — is `<projects_dir>/<encoded-cwd>/<id>.jsonl` a regular
//! file?). That file exists for a FINISHED conversation and for one Claude Code
//! is still running as a background job, and the two need opposite commands. Sent
//! `--resume` for a live background session, `claude` prints
//!
//! ```text
//! Session <uuid> is running as a background session (<short-id>).
//! Run `claude attach <short-id>` … or `claude stop <short-id>` first
//! ```
//!
//! and exits 0. The pane is left at a bare shell, and the reap loop marks the
//! record Stopped about a minute later. `claude attach <short-id>` opens that
//! same background session in the pane with its conversation intact, which is
//! what the operator asked for.
//!
//! What: [`query_registry`] runs `claude agents --json` — Claude Code's own
//! machine-readable list of live sessions — under a hard time cap;
//! [`attach_id_in_registry`] answers "is this `sessionId` a live background
//! entry, and what is its short id?"; [`attach_command`] builds the `attach`
//! pane command; [`relaunch_command`] makes the choice and is the single seam
//! `spawn_resume` calls.
//!
//! **Why a registry read and not the refusal text.** `claude_code_exit_hint`
//! (#6766) rejected parsing that message as an undocumented UI surface a
//! reworded release would silently take with it. The same objection applies
//! here, so the decision is made BEFORE the launch, against a `--json` surface
//! that exists to be scripted against.
//!
//! **Every failure falls open to `--resume`.** A missing `claude`, a non-zero
//! exit, a timeout, or unparsable JSON all resolve to "not live", which is
//! exactly the pre-#6863 behavior. The probe can only ever turn a refused
//! relaunch into a working one; it can never turn a working one into a failure.
//!
//! `claude stop <short-id>` is never issued — it is destructive to in-flight
//! work.
//!
//! Test: `relaunch_attaches_to_a_live_background_session`,
//! `relaunch_resumes_a_session_absent_from_the_registry`,
//! `relaunch_resumes_when_the_registry_call_fails`,
//! `registry_sample_parses_every_entry_shape`,
//! `query_registry_kills_and_reaps_a_wedged_probe` — in the sibling `_tests.rs`.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;
use tracing::debug;

use super::claude_code_gh_env;
use super::{
    cd_and_group, env_bin_prefix, exit_dispatch_suffix, launch_clock_prefix,
    session_id_export_prefix,
};

/// Ceiling on the `claude agents --json` wait. Resume is interactive, so the
/// probe must never be the reason a pane sits blank; three seconds is far above
/// the ~200 ms the command actually takes and still imperceptible when the
/// binary hangs.
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(3);

/// The `kind` a background session carries. An `interactive` entry is a live
/// TTY Claude Code, which `attach` cannot take over and which carries no `id`.
const BACKGROUND_KIND: &str = "background";

/// `state` values that mean the background session has FINISHED. Anything else
/// (`working`, `blocked`, or a state a future release adds) is live, so an
/// unrecognised state attaches rather than colliding with a refusal.
const TERMINAL_STATES: [&str; 2] = ["done", "failed"];

/// One entry of the `claude agents --json` array.
///
/// Why: only four of the emitted fields decide anything, and every one of them
/// is `Option` because the shape varies by entry — an `interactive` session
/// carries neither `id` nor `state`, and `pid`/`status` appear only while a
/// process is attached to the entry. Deserializing into required fields would
/// make one such entry poison the whole array.
/// What: `id` is the short id `claude attach` takes; `sessionId` is the full
/// UUID a tm record stores as its `claude_session_id`.
/// Test: `registry_sample_parses_every_entry_shape`.
#[derive(Debug, Deserialize)]
struct AgentEntry {
    id: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    kind: Option<String>,
    state: Option<String>,
}

/// The pane-command inputs `--resume` and `attach` share.
///
/// Why: [`relaunch_command`] picks between two builders that need almost the
/// same eight values; passing them as one borrowed struct keeps the seam a
/// single testable call rather than a twelve-argument function.
/// What: plain borrows, no ownership and no I/O. `claude_bin` is already
/// disclaim-wrapped by the caller — the field is the exact string that reaches
/// the pane.
/// Test: `attach_command_carries_the_resume_prefixes`.
pub(super) struct RelaunchInputs<'a> {
    pub cwd: &'a Path,
    pub claude_bin: &'a str,
    pub config_dir: Option<&'a Path>,
    pub session_id: &'a str,
    pub prompt_file: Option<&'a Path>,
    pub oauth_token: Option<&'a str>,
    pub gh_env_file: Option<&'a Path>,
    pub mcp_env: &'a [(String, String)],
}

/// Read Claude Code's live-session registry, or say why it could not be read.
///
/// Why: this is the only I/O in the module, kept behind its own function so
/// every decision above it is a pure function of a `&str` and can be unit
/// tested without a real `claude` on the machine (#4255 — a test must never
/// touch the operator's real session state).
/// What: spawns `<claude_bin> agents --json` through
/// [`crate::core::spawn_disclaim::disclaimed_stdout_command_with_timeout`] —
/// stdout piped, stderr to `/dev/null`, TCC-disclaimed on macOS — wrapped in
/// [`trusty_common::spawn_retry::retry_on_etxtbsy`], the workspace's one
/// exec-retry policy (only the exec step can raise `ETXTBSY`, so no retry ever
/// re-runs a probe that started). That helper drains stdout off-thread, so a
/// child that outgrew the pipe buffer cannot deadlock the caller, and it
/// SIGKILLs and reaps a child that outlives [`REGISTRY_TIMEOUT`] — a probe
/// process never survives the deadline (#6863; the same guarantee #5969 bought
/// the `claude --version` probe).
///
/// `CLAUDE_CONFIG_DIR` is exported to match the spawn the pane will make.
/// Claude Code 2.1.261 keeps this registry machine-global — verified: the same
/// eleven entries come back with and without the managed config dir set — so
/// today it changes nothing; it is set so a release that DOES scope the
/// registry by config home is read against the right one.
///
/// # Errors
///
/// A spawn failure (no `claude` on PATH), a non-zero exit, or the timeout, each
/// rendered as a one-line reason for the caller's debug log. Every one of them
/// means the same thing to [`relaunch_command`]: fall back to `--resume`.
///
/// Test: `query_registry_kills_and_reaps_a_wedged_probe` proves the deadline
/// leaves no child behind. The live shape it returns is pinned by
/// `registry_sample_parses_every_entry_shape`, which parses a capture of this
/// command's real output; the decision above it is the seam tests inject AROUND.
pub(super) fn query_registry(
    claude_bin: &str,
    config_dir: Option<&Path>,
) -> Result<String, String> {
    // #6863: rebuilt per attempt because the bounded-wait helper consumes the
    // `Command`; an ETXTBSY retry never re-runs a probe that already started.
    let build = || {
        let mut cmd = Command::new(claude_bin);
        cmd.arg("agents").arg("--json").stdin(Stdio::null());
        if let Some(dir) = config_dir {
            cmd.env("CLAUDE_CONFIG_DIR", dir);
        }
        cmd
    };
    let out = trusty_common::spawn_retry::retry_on_etxtbsy(|| {
        crate::core::spawn_disclaim::disclaimed_stdout_command_with_timeout(
            build(),
            REGISTRY_TIMEOUT,
        )
    })
    .map_err(|e| {
        if e.kind() == io::ErrorKind::TimedOut {
            format!("`claude agents --json` did not answer within {REGISTRY_TIMEOUT:?}")
        } else {
            format!("could not run `{claude_bin} agents --json`: {e}")
        }
    })?;
    if !out.status.success() {
        return Err(format!("`claude agents --json` exited {}", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Find the short id to `attach` for a stored `claude_session_id`, if that
/// session is a live background one.
///
/// Why: `claude attach` takes the SHORT id, while a tm record stores the full
/// UUID, so the registry is both the liveness answer and the id translation.
/// What: parses the array and returns the `id` of the first entry whose
/// `sessionId` matches, whose `kind` is [`BACKGROUND_KIND`], and whose `state`
/// is not one of [`TERMINAL_STATES`]. Unparsable JSON, no match, an
/// `interactive` entry, or a finished one all return `None`.
/// Test: `attach_id_found_for_a_live_background_entry`,
/// `attach_id_absent_for_an_unlisted_session`,
/// `attach_id_absent_for_a_finished_background_entry`,
/// `attach_id_absent_for_an_interactive_entry`,
/// `attach_id_absent_for_unparsable_json`.
pub(super) fn attach_id_in_registry(
    registry_json: &str,
    claude_session_id: &str,
) -> Option<String> {
    let entries: Vec<AgentEntry> = serde_json::from_str(registry_json).ok()?;
    entries.into_iter().find_map(|entry| {
        let live = entry.session_id.as_deref() == Some(claude_session_id)
            && entry.kind.as_deref() == Some(BACKGROUND_KIND)
            && !entry
                .state
                .as_deref()
                .is_some_and(|s| TERMINAL_STATES.contains(&s));
        if live { entry.id } else { None }
    })
}

/// Build the `claude attach <short-id>` pane command.
///
/// Why: re-entering a live background session must otherwise look exactly like
/// a resume — same `cd`, same `TM_MANAGED_SESSION_ID` export, same launch clock
/// and exit dispatch (#6766), same `gh` identity file (#3025), same env scrub
/// and `CLAUDE_CONFIG_DIR` (DOC-34), same disclaim-exec wrapper (#2997) — or
/// the attached session loses whatever the differing piece carried.
/// What: [`super::resume_command`]'s prefix chain with `attach <attach_id>` in
/// place of the flags.
///
/// `prompt_file` is deliberately unused: `claude attach <id>` accepts no
/// options (`claude attach --help` documents the bare form only), and the
/// system prompt it would carry is already part of the conversation being
/// re-entered. Passing it would make `claude` reject the invocation.
/// Test: `attach_command_carries_the_resume_prefixes`,
/// `attach_command_omits_flags_attach_cannot_take`.
pub(super) fn attach_command(inputs: &RelaunchInputs<'_>, attach_id: &str) -> String {
    let body = format!(
        "{}{}{}{} attach {attach_id}{}",
        session_id_export_prefix(inputs.session_id),
        launch_clock_prefix(),
        claude_code_gh_env::gh_env_source_prefix(inputs.gh_env_file),
        env_bin_prefix(
            inputs.claude_bin,
            inputs.config_dir,
            inputs.oauth_token,
            inputs.mcp_env,
        ),
        exit_dispatch_suffix(),
    );
    cd_and_group(inputs.cwd, &body)
}

/// Choose and build the pane command for a resume: `attach`, `--resume`, or a
/// fresh launch.
///
/// Why: the choice and both builders belong at one seam so `spawn_resume` holds
/// no policy of its own, and so a test can pin the whole decision by handing in
/// a registry string instead of running a real `claude` (the injected-I/O
/// pattern `session_id_exists`/`session_id_exists_in` already uses in this
/// module).
/// What: a `claude_session_id` that [`attach_id_in_registry`] reports live
/// yields [`attach_command`]; otherwise the pre-#6863 path runs unchanged —
/// [`super::resume_command`] with `effective_id` (the caller's
/// `session_id_exists`-filtered id), which is `--resume <uuid>` when that id
/// survived and a fresh launch when it did not. `probe` returns `Err` whenever
/// [`query_registry`] could not answer; the reason is logged at debug and the
/// fallback is taken.
///
/// `probe` is a closure, not an already-read string, so the registry read
/// happens only on the one path that can use it. There is nothing to look up
/// without a `claude_session_id`, and running it anyway shelled out to the
/// operator's real `claude` from every fresh-launch test (#4255, and ~20 s per
/// test — the reason this is lazy).
/// Test: `relaunch_attaches_to_a_live_background_session`,
/// `relaunch_resumes_a_session_absent_from_the_registry`,
/// `relaunch_resumes_when_the_registry_call_fails`,
/// `relaunch_starts_fresh_without_a_usable_id`,
/// `relaunch_never_probes_the_registry_without_a_session_id`.
pub(super) fn relaunch_command(
    inputs: &RelaunchInputs<'_>,
    claude_session_id: Option<&str>,
    effective_id: Option<&str>,
    probe: impl FnOnce() -> Result<String, String>,
) -> String {
    // #6863: no stored id means no registry lookup — the probe must not run.
    let attach_id = match claude_session_id.map(|id| (id, probe())) {
        Some((id, Ok(json))) => attach_id_in_registry(&json, id),
        Some((_, Err(reason))) => {
            debug!(
                reason = %reason,
                "claude agents --json unavailable; falling back to --resume (#6863)"
            );
            None
        }
        None => None,
    };
    match attach_id {
        Some(short_id) => {
            debug!(
                attach_id = %short_id,
                "stored claude_session_id is a live background session; attaching (#6863)"
            );
            attach_command(inputs, &short_id)
        }
        None => super::resume_command(
            inputs.cwd,
            inputs.claude_bin,
            inputs.config_dir,
            effective_id,
            inputs.session_id,
            inputs.prompt_file,
            inputs.oauth_token,
            inputs.gh_env_file,
            inputs.mcp_env,
        ),
    }
}

#[cfg(test)]
#[path = "claude_code_agents_tests.rs"]
mod tests;
