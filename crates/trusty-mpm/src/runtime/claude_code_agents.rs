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
//! `registry_sample_parses_every_entry_shape` — in the sibling `_tests.rs`.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
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
/// What: spawns `<claude_bin> agents --json` with stdin and stderr null-ed and
/// stdout piped, through [`trusty_common::spawn_retry::retry_on_etxtbsy`] (the
/// workspace's one exec-retry policy), then waits on a thread-and-channel
/// handoff capped at [`REGISTRY_TIMEOUT`]. The handoff, rather than a bare
/// `wait_with_output`, is what makes the cap real: a reader thread keeps
/// draining the pipe, so a child that outgrew the pipe buffer cannot deadlock
/// the caller, and `recv_timeout` returns on the deadline regardless.
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
/// Test: not unit tested — it is the seam tests inject AROUND. The live shape
/// it returns is pinned by `registry_sample_parses_every_entry_shape`, which
/// parses a capture of this command's real output.
pub(super) fn query_registry(
    claude_bin: &str,
    config_dir: Option<&Path>,
) -> Result<String, String> {
    let mut cmd = Command::new(claude_bin);
    cmd.arg("agents")
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(dir) = config_dir {
        cmd.env("CLAUDE_CONFIG_DIR", dir);
    }
    let child = trusty_common::spawn_retry::retry_on_etxtbsy(|| cmd.spawn())
        .map_err(|e| format!("could not spawn `{claude_bin} agents --json`: {e}"))?;

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(REGISTRY_TIMEOUT) {
        Ok(Ok(out)) if out.status.success() => {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        }
        Ok(Ok(out)) => Err(format!("`claude agents --json` exited {}", out.status)),
        Ok(Err(e)) => Err(format!(
            "`claude agents --json` could not be waited on: {e}"
        )),
        Err(_) => Err(format!(
            "`claude agents --json` did not answer within {REGISTRY_TIMEOUT:?}"
        )),
    }
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
/// survived and a fresh launch when it did not. `registry` is `Err` whenever
/// [`query_registry`] could not answer; the reason is logged at debug and the
/// fallback is taken.
/// Test: `relaunch_attaches_to_a_live_background_session`,
/// `relaunch_resumes_a_session_absent_from_the_registry`,
/// `relaunch_resumes_when_the_registry_call_fails`,
/// `relaunch_starts_fresh_without_a_usable_id`.
pub(super) fn relaunch_command(
    inputs: &RelaunchInputs<'_>,
    claude_session_id: Option<&str>,
    effective_id: Option<&str>,
    registry: Result<&str, &str>,
) -> String {
    let attach_id = match (claude_session_id, registry) {
        (Some(id), Ok(json)) => attach_id_in_registry(json, id),
        (Some(_), Err(reason)) => {
            debug!(
                reason = %reason,
                "claude agents --json unavailable; falling back to --resume (#6863)"
            );
            None
        }
        (None, _) => None,
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
