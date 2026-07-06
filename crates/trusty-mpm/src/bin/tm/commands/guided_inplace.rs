//! Bare-`tm` in-pane relaunch of the CURRENT managed session (#2023 component C).
//!
//! Why: components A and B (already landed) leave a managed pane alive as a
//! bare shell when its `claude` process exits, with `TM_MANAGED_SESSION_ID`
//! exported into that shell's live environment. If the operator then types
//! bare `tm` from inside THAT pane, the ordinary guided flow would be wrong on
//! two counts: (1) it would show a picker/attach flow for a session the
//! operator is already sitting inside, and (2) if it happened to select
//! "restart", the daemon's `/resume` route unconditionally kills any surviving
//! tmux session and creates a fresh one — which would tear down the very pane
//! the operator is running `tm` from. This module detects that specific
//! situation and relaunches `claude` directly back into the CURRENT process's
//! pane via `exec`, bypassing the daemon kill+recreate path entirely.
//!
//! What: [`try_inplace_relaunch`] is the top-level entry point `guided::
//! run_guided_default` calls FIRST, before any project detection or picker
//! logic. [`plan_inplace`] is the pure decision (env var present AND resolves
//! to a known managed-session record → [`super::guided_resume::ResumeAction::
//! InPlace`]; otherwise `None`, meaning "fall through to the normal guided
//! flow"). The daemon round-trips ([`fetch_managed_session`],
//! [`reactivate_managed_session`]) and the process-replacing exec
//! ([`exec_claude_in_place`]) are the I/O driver.
//!
//! Test: `plan_inplace_*` cover the pure decision; the I/O path (HTTP calls,
//! `exec`) is not unit-tested here — it requires a live daemon and a real
//! `claude` binary, mirroring the rest of the guided-resume I/O surface.

use anyhow::Context as _;

use super::guided_resume::ResumeAction;

/// Environment variable exported into a managed pane's shell (#2023 B) that
/// identifies which managed session bare `tm`, run after `claude` exits,
/// belongs to.
const MANAGED_SESSION_ID_ENV: &str = "TM_MANAGED_SESSION_ID";

/// Read [`MANAGED_SESSION_ID_ENV`] from the environment, treating a blank
/// value the same as absent.
///
/// Why: an exported-but-empty env var (never expected in practice, but cheap
/// to guard) must not be mistaken for a real session id.
/// What: `std::env::var` filtered to non-blank (after trim).
/// Test: exercised indirectly via [`plan_inplace`]'s tests (which take the
/// already-read `Option<&str>` directly, keeping the env read itself outside
/// the pure/testable seam).
fn read_env_managed_session_id() -> Option<String> {
    std::env::var(MANAGED_SESSION_ID_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
}

/// Decide whether bare `tm`'s in-pane environment signals an in-place
/// relaunch (#2023 component C).
///
/// Why: the actual daemon lookup ("does this id resolve to a known managed
/// session?") is I/O; separating the DECISION from the lookup makes the
/// decision itself exhaustively unit-testable.
/// What: `Some(ResumeAction::InPlace)` when `env_session_id` is `Some` AND
/// `resolves` is `true`; `None` otherwise — covering both "no env var" and
/// "env var present but stale/unknown to the daemon" (guard case), each of
/// which must fall through to the ordinary guided picker rather than error.
/// Test: `plan_inplace_selected_when_env_set_and_resolves`,
/// `plan_inplace_none_when_env_absent`,
/// `plan_inplace_none_when_env_set_but_unresolved`.
pub(crate) fn plan_inplace(env_session_id: Option<&str>, resolves: bool) -> Option<ResumeAction> {
    (env_session_id.is_some() && resolves).then_some(ResumeAction::InPlace)
}

/// GET `/api/v1/sessions/managed/{id}` — resolve the env-supplied id to a record.
///
/// Why: the in-place path must confirm the daemon still knows this session
/// (the env var can go stale — e.g. the record was decommissioned after the
/// pane was left alive) before reactivating/relaunching it.
/// What: `Some(summary)` on HTTP 2xx, `None` on any failure (network error,
/// 404, non-2xx, or an undeserializable body) — every failure mode folds into
/// the same "fall through to the picker" outcome via [`plan_inplace`].
/// Test: I/O path; not unit-tested (requires a live daemon).
async fn fetch_managed_session(
    client: &reqwest::Client,
    url: &str,
    id: &str,
) -> Option<trusty_mpm::client::ManagedSessionSummary> {
    let resp = client
        .get(format!("{url}/api/v1/sessions/managed/{id}"))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json().await.ok()
}

/// POST `/api/v1/sessions/managed/{id}/reactivate` — flip Stopped -> Active
/// in place, with no tmux mutation (#2023 C step 3).
///
/// Why: the daemon record must reflect that the session is running again
/// BEFORE this process execs into `claude` — otherwise `tm session ls` would
/// keep showing the session as `Stopped` even though it is live.
/// What: best-effort — a failure here is logged but does NOT block the
/// relaunch (the operator's priority is getting `claude` back, not a
/// perfectly synchronized daemon record; a stale `Stopped` record self-heals
/// on the next reap tick or explicit `tm session info`).
/// Test: I/O path; not unit-tested (requires a live daemon).
async fn reactivate_managed_session(client: &reqwest::Client, url: &str, id: &str) {
    let result = client
        .post(format!("{url}/api/v1/sessions/managed/{id}/reactivate"))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await;
    match result {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => {
            let status = resp.status();
            eprintln!(
                "tm: warning: daemon returned {status} reactivating session {id} \
                 (non-fatal — relaunching anyway)"
            );
        }
        Err(e) => {
            eprintln!(
                "tm: warning: could not reach daemon to reactivate session {id}: {e} \
                 (non-fatal — relaunching anyway)"
            );
        }
    }
}

/// Replace the CURRENT process image with `cmd` (Unix `exec`), never returning
/// on success.
///
/// Why: the in-place relaunch must put `claude` directly into THIS process's
/// controlling terminal/pane — spawning a child and waiting would leave the
/// `tm` process (and an extra layer of signal/Ctrl-C indirection) sitting
/// between the pane and `claude`, unlike every other managed-session launch
/// path where the daemon's tmux pane runs `claude` as the pane's direct child.
/// What: Unix — `std::os::unix::process::CommandExt::exec` replaces the
/// process image; a return from `exec` is always a failure (it never returns
/// on success), so the error is propagated as `Err`. Non-Unix — no `exec`
/// syscall exists; falls back to spawn+wait and exits this process with the
/// child's status code, which is observably equivalent from the terminal's
/// perspective (just one extra, short-lived process in between).
/// Test: not unit-tested — replacing/exiting the test process is not
/// observable from within the test itself.
#[cfg(unix)]
fn exec_claude_in_place(mut cmd: std::process::Command) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt as _;
    let err = cmd.exec();
    Err(anyhow::anyhow!("failed to exec claude in place: {err}"))
}

/// Non-Unix fallback for [`exec_claude_in_place`] (see its doc for rationale).
#[cfg(not(unix))]
fn exec_claude_in_place(mut cmd: std::process::Command) -> anyhow::Result<()> {
    let status = cmd.status().context("failed to run claude")?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Drive the full in-place relaunch once [`plan_inplace`] selected it.
///
/// Why: separated from [`try_inplace_relaunch`] so the "should we take this
/// path at all" decision and the "how do we execute it" mechanics are
/// distinct, readable steps.
/// What: (1) prints a one-line notice; (2) best-effort reactivates the record
/// on the daemon (step 3 of #2023 C — see [`reactivate_managed_session`]);
/// (3) resolves the workdir (`workspace_path` → `cwd` → the CLI's own current
/// directory, since the pane's shell is already rooted at the workspace); (4)
/// builds the resume argv via [`trusty_mpm::runtime::build_inplace_resume_command`]
/// (the SAME `--resume`-existence-check → `--continue`/fresh-spawn fallback
/// logic the tmux-pane resume path uses, #2013); (5) execs `claude` in place.
/// Test: I/O path; not unit-tested (requires a live daemon + real `claude`).
async fn run_inplace_relaunch(
    client: &reqwest::Client,
    url: &str,
    id: &str,
    record: trusty_mpm::client::ManagedSessionSummary,
) -> anyhow::Result<()> {
    eprintln!("tm: this pane belongs to managed session {id} — relaunching in place…");

    reactivate_managed_session(client, url, id).await;

    let cwd = record
        .workspace_path
        .or(record.cwd)
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .context("cannot resolve a working directory for the in-place relaunch")?;

    let resume = trusty_mpm::runtime::build_inplace_resume_command(
        &cwd,
        record.claude_session_id.as_deref(),
    )
    .map_err(|e| anyhow::anyhow!("cannot build in-place relaunch command: {e}"))?;

    let mut cmd = std::process::Command::new(&resume.claude_bin);
    cmd.args(&resume.args)
        .current_dir(&cwd)
        .env_remove("ANTHROPIC_API_KEY");
    if let Some(dir) = &resume.config_dir {
        cmd.env("CLAUDE_CONFIG_DIR", dir);
    }

    exec_claude_in_place(cmd)
}

/// Top-level entry point: try the in-place relaunch before any other bare-`tm`
/// logic runs (#2023 component C).
///
/// Why: `guided::run_guided_default` must check this FIRST — before project
/// detection, before the picker — because the in-pane case is a completely
/// different situation from "operator ran `tm` from a project directory to
/// pick a session": here the session is already known and already running IN
/// this exact pane.
/// What: `None` — [`MANAGED_SESSION_ID_ENV`] is unset, blank, or does not
/// resolve to a known managed session on the daemon — means "not this path;
/// fall through to the ordinary guided default." `Some(result)` means this
/// function took over: on success it never returns (`claude` replaced this
/// process); on failure it returns `Some(Err(..))` so the caller can surface
/// the error and exit non-zero rather than silently falling through to a
/// picker that would confusingly re-offer this very session.
/// Test: `plan_inplace_*` cover the decision this function delegates to.
pub(crate) async fn try_inplace_relaunch(
    client: &reqwest::Client,
    url: &str,
) -> Option<anyhow::Result<()>> {
    let env_id = read_env_managed_session_id()?;
    let record = fetch_managed_session(client, url, &env_id).await;
    match plan_inplace(Some(&env_id), record.is_some()) {
        Some(ResumeAction::InPlace) => {
            Some(run_inplace_relaunch(client, url, &env_id, record?).await)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_inplace_selected_when_env_set_and_resolves() {
        assert_eq!(
            plan_inplace(Some("11111111-2222-3333-4444-555555555555"), true),
            Some(ResumeAction::InPlace)
        );
    }

    #[test]
    fn plan_inplace_none_when_env_absent() {
        assert_eq!(plan_inplace(None, true), None);
        assert_eq!(plan_inplace(None, false), None);
    }

    #[test]
    fn plan_inplace_none_when_env_set_but_unresolved() {
        // Guard case (#2023 C item 4): a stale/unknown id must fall through to
        // the ordinary guided picker rather than error.
        assert_eq!(
            plan_inplace(Some("11111111-2222-3333-4444-555555555555"), false),
            None
        );
    }
}
