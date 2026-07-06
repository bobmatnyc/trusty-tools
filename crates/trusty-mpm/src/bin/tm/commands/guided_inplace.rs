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
//! logic. [`plan_inplace`] is the pure decision (env var present AND the
//! fetched record's state is `"stopped"` → [`super::guided_resume::
//! ResumeAction::InPlace`]; otherwise `None`, meaning "fall through to the
//! normal guided flow"). The daemon round-trips ([`fetch_managed_session`],
//! [`reactivate_managed_session`]) and the process-replacing exec
//! ([`exec_claude_in_place`]) are the I/O driver, sequenced by
//! [`run_inplace_relaunch`] as: resolve the `claude` binary + build the resume
//! command (pure, local — can fail on a missing binary before anything is
//! mutated) → reactivate on the daemon (network — a 404/409/unreachable
//! daemon ABORTS the whole in-place path and falls through to the picker,
//! never exec'ing against an unconfirmed record) → exec.
//!
//! Why the ordering matters (safety boundary, code-critic WARN on #2027): a
//! stale/leaked `TM_MANAGED_SESSION_ID` (e.g. inherited into an unrelated
//! subshell) must never cause this process to chdir+exec `claude` into the
//! wrong workspace. Two gates enforce that: (1) [`plan_inplace`] only selects
//! `InPlace` when the fetched record's state is CONFIRMED `"stopped"` — an
//! Active/Errored/Decommissioned/unresolved id all fall through to the
//! ordinary picker; (2) the daemon's own `mark_reactivated` Stopped-only guard
//! is honored by actually checking the reactivate response — a 404/409/network
//! failure aborts rather than proceeding to exec regardless.
//!
//! Test: `plan_inplace_*` cover the pure decision; `reactivate_managed_session`'s
//! success/409/404 outcomes are exercised against a local one-shot `TcpListener`
//! mock (#2027, mirroring `core::sm::providers`' mock convention — no external
//! mock-server crate); the full exec path is not unit-tested — it requires a
//! live daemon and a real `claude` binary, mirroring the rest of the
//! guided-resume I/O surface.

use anyhow::Context as _;

use super::guided_resume::ResumeAction;

/// Per-request timeout for the in-pane reachability probes (GET existence
/// check + POST reactivate), matching the 2s daemon-reachability convention
/// used elsewhere in this binary (`pm_guard.rs`, `misc.rs`'s hook relay).
///
/// Why: this path runs on EVERY bare `tm` invocation inside a managed pane —
/// a hung daemon must not add a long stall before falling through to the
/// ordinary guided default.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

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
/// session, and is it actually `Stopped`?") is I/O; separating the DECISION
/// from the lookup makes the decision itself exhaustively unit-testable.
/// Folding the Stopped-state check in HERE (rather than trusting the daemon's
/// `mark_reactivated` guard alone) closes the hijack window a code-critic WARN
/// flagged on #2027: a resolved-but-`Active`/`Errored`/`Decommissioned` record
/// (e.g. from a leaked/stale env var pointing at some OTHER session) must fall
/// through to the ordinary picker, not attempt an in-place `exec`.
/// What: `Some(ResumeAction::InPlace)` when `env_session_id` is `Some` AND
/// `record_state` is `Some("stopped")`; `None` otherwise — covering "no env
/// var", "env var present but id unknown to the daemon" (`record_state` is
/// `None`), and "id resolves but is NOT `Stopped`" — every one of which must
/// fall through to the ordinary guided picker rather than error.
/// Test: `plan_inplace_selected_when_env_set_and_stopped`,
/// `plan_inplace_none_when_env_absent`,
/// `plan_inplace_none_when_env_set_but_unresolved`,
/// `plan_inplace_none_when_resolved_but_not_stopped`.
pub(crate) fn plan_inplace(
    env_session_id: Option<&str>,
    record_state: Option<&str>,
) -> Option<ResumeAction> {
    (env_session_id.is_some() && record_state == Some("stopped")).then_some(ResumeAction::InPlace)
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
        .timeout(PROBE_TIMEOUT)
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
/// Why: the daemon's `mark_reactivated` is the second half of the
/// Stopped-only safety guard — [`plan_inplace`] already checked the record
/// was `Stopped` at fetch time, but that is a TOCTOU-prone read; the daemon
/// re-validates atomically against its own current state and can legitimately
/// refuse (404 the id vanished, 409 it is no longer `Stopped`, or the daemon
/// may simply be unreachable). This result MUST be honored, not logged and
/// ignored (code-critic WARN on #2027): exec'ing into `claude` after a
/// refused/failed reactivate would let a hijacked/stale env var drive a
/// relaunch the daemon never actually confirmed.
/// What: returns `true` only on an HTTP 2xx response — the daemon record IS
/// now `Active`. Returns `false` on any other outcome (4xx/5xx or a network
/// error), each logged with the concrete reason; the caller ABORTS the
/// in-place path on `false` and falls through to the ordinary guided picker.
/// Test: I/O path; not unit-tested (requires a live daemon).
async fn reactivate_managed_session(client: &reqwest::Client, url: &str, id: &str) -> bool {
    let result = client
        .post(format!("{url}/api/v1/sessions/managed/{id}/reactivate"))
        .timeout(PROBE_TIMEOUT)
        .send()
        .await;
    match result {
        Ok(resp) if resp.status().is_success() => true,
        Ok(resp) => {
            let status = resp.status();
            eprintln!(
                "tm: daemon refused to reactivate session {id} ({status}) — \
                 aborting in-place relaunch, falling back to the guided picker"
            );
            false
        }
        Err(e) => {
            eprintln!(
                "tm: could not reach daemon to reactivate session {id}: {e} — \
                 aborting in-place relaunch, falling back to the guided picker"
            );
            false
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

/// The result of [`run_inplace_relaunch`] — distinguishes a hard failure
/// (surfaced to the operator, non-zero exit) from a safety-gate abort (must
/// fall through to the ordinary picker exactly like an unresolved id).
///
/// Why: the daemon's reactivate response MUST be honored (#2027 code-critic
/// WARN) — a refused/failed reactivate is NOT a hard error to report and
/// exit on, it is "this in-place attempt is not safe to proceed with,"
/// which is exactly the same outcome as [`plan_inplace`] returning `None`.
/// Separating the two outcomes lets [`try_inplace_relaunch`] route a
/// reactivate failure back into the `None`/fall-through path instead of
/// wrapping it in `Some(Err(..))`.
enum InPlaceOutcome {
    /// The exec attempt concluded (never returns on success; carries the
    /// error when `exec` itself failed, or when command resolution failed
    /// before any daemon mutation occurred).
    Result(anyhow::Result<()>),
    /// The daemon refused/failed to confirm reactivation — abort, fall
    /// through to the ordinary guided picker (no exec attempted).
    FallThrough,
}

/// Drive the full in-place relaunch once [`plan_inplace`] selected it.
///
/// Why: separated from [`try_inplace_relaunch`] so the "should we take this
/// path at all" decision and the "how do we execute it" mechanics are
/// distinct, readable steps.
/// What, IN ORDER (#2027 exec-ordering fix): (1) prints a one-line notice;
/// (2) resolves the workdir and builds the resume argv via
/// [`trusty_mpm::runtime::build_inplace_resume_command`] (the SAME
/// `--resume`-existence-check → `--continue`/fresh-spawn fallback logic the
/// tmux-pane resume path uses, #2013) — this is PURE/local (resolving the
/// `claude` binary can fail with `BinaryNotFound`) and runs BEFORE any daemon
/// mutation, so a missing binary never flips the record to a false `Active`;
/// (3) reactivates the record on the daemon (step 3 of #2023 C — see
/// [`reactivate_managed_session`]), immediately before exec — on a
/// refused/failed reactivate this returns [`InPlaceOutcome::FallThrough`]
/// rather than proceeding; (4) execs `claude` in place.
/// Test: I/O path; not unit-tested (requires a live daemon + real `claude`).
async fn run_inplace_relaunch(
    client: &reqwest::Client,
    url: &str,
    id: &str,
    record: trusty_mpm::client::ManagedSessionSummary,
) -> InPlaceOutcome {
    eprintln!("tm: this pane belongs to managed session {id} — relaunching in place…");

    let cwd = match record
        .workspace_path
        .or(record.cwd)
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .context("cannot resolve a working directory for the in-place relaunch")
    {
        Ok(cwd) => cwd,
        Err(e) => return InPlaceOutcome::Result(Err(e)),
    };

    let resume = match trusty_mpm::runtime::build_inplace_resume_command(
        &cwd,
        record.claude_session_id.as_deref(),
    ) {
        Ok(resume) => resume,
        Err(e) => {
            return InPlaceOutcome::Result(Err(anyhow::anyhow!(
                "cannot build in-place relaunch command: {e}"
            )));
        }
    };

    // Reactivate immediately before exec — and HONOR the result (#2027):
    // a refused/failed reactivate aborts rather than proceeding to exec.
    if !reactivate_managed_session(client, url, id).await {
        return InPlaceOutcome::FallThrough;
    }

    let mut cmd = std::process::Command::new(&resume.claude_bin);
    cmd.args(&resume.args)
        .current_dir(&cwd)
        .env_remove("ANTHROPIC_API_KEY");
    if let Some(dir) = &resume.config_dir {
        cmd.env("CLAUDE_CONFIG_DIR", dir);
    }

    InPlaceOutcome::Result(exec_claude_in_place(cmd))
}

/// Top-level entry point: try the in-place relaunch before any other bare-`tm`
/// logic runs (#2023 component C).
///
/// Why: `guided::run_guided_default` must check this FIRST — before project
/// detection, before the picker — because the in-pane case is a completely
/// different situation from "operator ran `tm` from a project directory to
/// pick a session": here the session is already known and already running IN
/// this exact pane.
/// What: `None` — [`MANAGED_SESSION_ID_ENV`] is unset, blank, does not resolve
/// to a known managed session, resolves to a session NOT currently `Stopped`
/// (see [`plan_inplace`]), or the daemon refuses/fails to confirm reactivation
/// (see [`InPlaceOutcome::FallThrough`]) — means "not this path; fall through
/// to the ordinary guided default." `Some(result)` means this function took
/// over: on success it never returns (`claude` replaced this process); on
/// failure it returns `Some(Err(..))` so the caller can surface the error and
/// exit non-zero rather than silently falling through to a picker that would
/// confusingly re-offer this very session.
/// Test: `plan_inplace_*` cover the decision this function delegates to.
pub(crate) async fn try_inplace_relaunch(
    client: &reqwest::Client,
    url: &str,
) -> Option<anyhow::Result<()>> {
    let env_id = read_env_managed_session_id()?;
    let record = fetch_managed_session(client, url, &env_id).await;
    let record_state = record.as_ref().map(|r| r.state.as_str());
    match plan_inplace(Some(&env_id), record_state) {
        Some(ResumeAction::InPlace) => {
            let record = record.expect("plan_inplace only selects InPlace when record is Some");
            match run_inplace_relaunch(client, url, &env_id, record).await {
                InPlaceOutcome::Result(r) => Some(r),
                InPlaceOutcome::FallThrough => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    const TEST_ID: &str = "11111111-2222-3333-4444-555555555555";

    /// Spawn a background HTTP mock that replies `status_line` (+ empty JSON
    /// body) to every connection, counting hits.
    ///
    /// Why: `reactivate_managed_session`'s status-code handling (#2027) needs
    /// a real HTTP round-trip to exercise reqwest's response parsing; this
    /// mirrors `core::sm::providers::test_support`'s "read the full request
    /// before replying" mock convention (that helper is `pub(crate)` to the
    /// `trusty-mpm` LIB crate and unreachable from this `bin/tm` BINARY crate
    /// — a separate compilation unit — hence the small inline copy in
    /// [`read_full_request`] below, rather than pulling in an external
    /// mock-server dependency for one test file).
    /// What: binds an ephemeral port, loops accepting connections, and for
    /// each one increments the returned counter, drains the request, then
    /// replies. Runs until the test's tokio runtime shuts down.
    /// Test: used by `reactivate_*` and the command-build-ordering test.
    async fn spawn_mock(status_line: &'static str) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_task = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                hits_task.fetch_add(1, Ordering::SeqCst);
                read_full_request(&mut sock).await;
                let body = "{}";
                let resp = format!(
                    "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}"), hits)
    }

    /// Read an entire HTTP/1.1 request (headers + any `Content-Length` body)
    /// before the caller writes a response — avoids the connection-reset
    /// flakiness a naive single `read` can cause (mirrors
    /// `core::sm::providers::test_support::read_full_request`, unreachable
    /// from this crate; see [`spawn_mock`]'s doc for why it is duplicated
    /// here rather than shared).
    async fn read_full_request(sock: &mut TcpStream) {
        let mut buf: Vec<u8> = Vec::with_capacity(4096);
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
            match sock.read(&mut chunk).await {
                Ok(0) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => return,
            }
        };
        let content_len: usize = String::from_utf8_lossy(&buf[..header_end])
            .split("\r\n")
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().ok())?
            })
            .unwrap_or(0);
        let want_total = header_end + content_len;
        while buf.len() < want_total {
            match sock.read(&mut chunk).await {
                Ok(0) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => return,
            }
        }
    }

    #[test]
    fn plan_inplace_selected_when_env_set_and_stopped() {
        assert_eq!(
            plan_inplace(Some(TEST_ID), Some("stopped")),
            Some(ResumeAction::InPlace)
        );
    }

    #[test]
    fn plan_inplace_none_when_env_absent() {
        assert_eq!(plan_inplace(None, Some("stopped")), None);
        assert_eq!(plan_inplace(None, None), None);
    }

    #[test]
    fn plan_inplace_none_when_env_set_but_unresolved() {
        // Guard case (#2023 C item 4): a stale/unknown id must fall through to
        // the ordinary guided picker rather than error.
        assert_eq!(plan_inplace(Some(TEST_ID), None), None);
    }

    #[test]
    fn plan_inplace_none_when_resolved_but_not_stopped() {
        // Safety-boundary regression guard (#2027 code-critic WARN): a
        // resolved-but-non-Stopped record (Active/Errored/Decommissioned —
        // e.g. from a leaked/stale TM_MANAGED_SESSION_ID pointing at some
        // OTHER, currently-running session) must NOT select the in-place
        // path; it must fall through to the ordinary guided picker exactly
        // like an unresolved id.
        for state in ["active", "errored", "provisioning", "decommissioned"] {
            assert_eq!(
                plan_inplace(Some(TEST_ID), Some(state)),
                None,
                "state '{state}' must not select InPlace"
            );
        }
    }

    #[tokio::test]
    async fn reactivate_confirms_success_on_2xx() {
        let (url, hits) = spawn_mock("HTTP/1.1 200 OK").await;
        let client = reqwest::Client::new();
        let ok = reactivate_managed_session(&client, &url, TEST_ID).await;
        assert!(ok, "a 2xx response must confirm reactivation");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reactivate_aborts_on_409_conflict() {
        // #2027 HIGH fix: a 409 (session not Stopped on the daemon's own
        // re-check) must NOT be treated as confirmed — the caller must abort
        // rather than proceeding to exec.
        let (url, _hits) = spawn_mock("HTTP/1.1 409 Conflict").await;
        let client = reqwest::Client::new();
        let ok = reactivate_managed_session(&client, &url, TEST_ID).await;
        assert!(!ok, "409 must NOT be treated as a confirmed reactivate");
    }

    #[tokio::test]
    async fn reactivate_aborts_on_404_not_found() {
        let (url, _hits) = spawn_mock("HTTP/1.1 404 Not Found").await;
        let client = reqwest::Client::new();
        let ok = reactivate_managed_session(&client, &url, TEST_ID).await;
        assert!(!ok, "404 must NOT be treated as a confirmed reactivate");
    }

    #[tokio::test]
    async fn reactivate_aborts_on_unreachable_daemon() {
        // Nothing listens on a privileged low port -> immediate connection
        // refused, exercising the network-error branch without waiting out
        // the full PROBE_TIMEOUT.
        let client = reqwest::Client::new();
        let ok = reactivate_managed_session(&client, "http://127.0.0.1:1", TEST_ID).await;
        assert!(
            !ok,
            "an unreachable daemon must NOT be treated as a confirmed reactivate"
        );
    }

    #[tokio::test]
    async fn run_inplace_relaunch_never_reactivates_when_command_build_fails() {
        // #2027 MEDIUM fix: command resolution (resolve `claude` + build the
        // resume argv) must happen BEFORE the daemon is ever asked to
        // reactivate the record, so a missing-binary failure never flips a
        // Stopped record to a false Active.
        //
        // This ordering guarantee can only be exercised end-to-end on a
        // machine where `claude` is NOT resolvable — otherwise
        // build_inplace_resume_command succeeds and this test would be
        // vacuously true. Probe first; skip (don't fail) when claude IS
        // present, mirroring the inverse of the "skip when claude absent"
        // convention used throughout runtime::claude_code's own test suite.
        let tmp = tempfile::tempdir().expect("tempdir");
        if trusty_mpm::runtime::build_inplace_resume_command(tmp.path(), None).is_ok() {
            return;
        }

        let (url, hits) = spawn_mock("HTTP/1.1 200 OK").await;
        let record = trusty_mpm::client::ManagedSessionSummary {
            id: TEST_ID.to_string(),
            name: "tm-test".to_string(),
            state: "stopped".to_string(),
            workspace_path: Some(tmp.path().to_string_lossy().to_string()),
            repo_url: None,
            branch: None,
            created_at: None,
            last_activity_at: None,
            pending_decision: None,
            proposed_default: None,
            source_id: None,
            task: None,
            cwd: None,
            claude_session_id: None,
        };
        let client = reqwest::Client::new();

        let outcome = run_inplace_relaunch(&client, &url, TEST_ID, record).await;

        assert!(
            matches!(outcome, InPlaceOutcome::Result(Err(_))),
            "a command-build failure must surface as a hard error, not FallThrough"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "reactivate must NEVER be called when command resolution fails first (#2027)"
        );
    }
}
