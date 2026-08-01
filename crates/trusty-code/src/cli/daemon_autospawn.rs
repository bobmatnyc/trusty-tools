//! Attach to a running `tcode serve --http` daemon, or START one — with
//! ownership tracked so only a daemon WE started is ever stopped (#4512).
//!
//! Why: `tcode tui` shipped (#4424, PR #4433) refusing to run without a
//! hand-started daemon: discovery failed, the command printed an "actionable
//! message" and exited. DOC-50 §4.1 deferred auto-spawn to Phase 2, but an
//! interactive command that demands the operator go start a background
//! service first is not a shippable first-run experience — the owner
//! directive of 2026-07-31 overturns that deferral. This module is the
//! resulting policy layer, kept OUT of `tui.rs` so the launch path stays a
//! thin "resolve args -> build engine -> run" wiring file (`crate::cli`'s
//! contract) and so the lifetime rules below are stated in one place.
//! What: [`ensure_daemon`] resolves a daemon URL through the UNCHANGED
//! discovery order (`TCODE_DAEMON_URL`, then the `http_addr` file, each
//! liveness-pinged — `tui_client::discovery::lookup_daemon`) and returns a
//! [`DaemonSession`] carrying that URL plus who owns the process:
//!
//! * **Live daemon found** -> [`Ownership::Attached`]. Nothing is spawned,
//!   and [`DaemonSession::shutdown`] is a NO-OP: a daemon we merely attached
//!   to belongs to whoever started it (possibly another TUI, an IDE, or a
//!   long-running dev daemon) and must survive our exit.
//! * **`TCODE_DAEMON_URL` set but unreachable** -> hard error naming that
//!   URL. Auto-spawn deliberately does NOT apply here: the operator named an
//!   address, and starting a daemon at a DIFFERENT address (the default
//!   port) would silently ignore that instruction.
//! * **Discovery file stale or absent** -> spawn
//!   `<current_exe> serve --http [--project <path>]` as a child, wait for it
//!   to answer `GET /health`, and return [`Ownership::Spawned`]. We started
//!   it, so we stop it: [`DaemonSession::shutdown`] sends SIGTERM and waits
//!   out a grace period so axum drains in-flight requests (the workspace's
//!   connection-safe restart convention, #534), escalating to SIGKILL only
//!   if the daemon ignores SIGTERM.
//!
//! The binary is resolved with [`super::tcode_exe::resolve`]
//! (`std::env::current_exe()`), never a bare `tcode` PATH lookup, so a
//! locally built binary spawns ITSELF rather than a stale installed copy.
//! The readiness wait reuses the shared
//! `trusty_common::daemon_guard::spin_until_ready` spinner (issue #985's
//! single tested implementation, already used by trusty-search/-memory/
//! -analyze) instead of a fourth hand-rolled poll loop, raced against
//! `Child::wait` so a daemon that dies on startup (e.g. its port is taken)
//! reports THAT immediately instead of spinning out the whole budget.
//! Test: `daemon_autospawn_tests::*` (sibling file, per the 500-SLOC
//! production cap) covers all four branches against a `wiremock` health
//! endpoint and stub child binaries — attach-without-spawning, spawn,
//! never-kill-what-we-did-not-spawn, and refuse-to-spawn-for-an-explicit-URL.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tokio::process::{Child, Command};
use trusty_code::serve::DEFAULT_HTTP_PORT;
use trusty_code::tui_client::discovery::{DAEMON_URL_ENV, Lookup, Source, lookup_daemon};
use trusty_common::daemon_guard::{DaemonGuardConfig, spin_until_ready};

/// How long a daemon WE spawned gets to become healthy before we give up,
/// stop it, and report the failure.
///
/// Shorter than `daemon_guard::DEFAULT_STARTUP_TIMEOUT` (30s) because this
/// wait sits between the operator pressing Enter and a TUI appearing — 20s
/// of spinner is already the outer edge of tolerable, and a tcode daemon
/// that has not bound its port by then is not about to.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a daemon we own gets to drain and exit after SIGTERM before we
/// escalate to SIGKILL. Matches the sibling sidecar teardown in
/// `trusty-agents`' Tauri shell, whose axum drain is the same shape.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Filename (under `resolve_data_dir("trusty-code")`) the spawned daemon's
/// stdout/stderr are appended to.
///
/// Why a file rather than inherited fds: the TUI takes over the terminal
/// with an alternate screen immediately after this, so an inherited stderr
/// would scribble daemon log lines across the rendered UI. Null-ing it
/// instead (as `daemon_guard::spawn_current_exe` does) would make a
/// failed startup completely undiagnosable, so we keep the output and name
/// the file in the error message.
const DAEMON_LOG_FILENAME: &str = "tui-spawned-daemon.log";

/// Who owns the daemon's process lifetime — the distinction the whole
/// module exists to enforce.
#[derive(Debug)]
pub enum Ownership {
    /// The daemon was already running. We did not start it, so we never
    /// stop it.
    Attached,
    /// We started this daemon, so we are responsible for stopping it.
    Spawned(Child),
}

/// A daemon `tcode tui` is about to drive, plus who owns its lifetime.
#[derive(Debug)]
pub struct DaemonSession {
    /// Base URL (no trailing slash) the engine should target.
    pub url: String,
    ownership: Ownership,
}

impl DaemonSession {
    /// Stop the daemon IF we started it; leave a pre-existing one running.
    ///
    /// Why: killing a daemon we merely attached to would tear the rug out
    /// from under whoever actually started it — another TUI, an editor
    /// integration, or a dev daemon deliberately left running. Ownership is
    /// therefore recorded at attach/spawn time, not inferred here.
    /// What: no-op for [`Ownership::Attached`]; for [`Ownership::Spawned`],
    /// SIGTERM -> wait up to [`SHUTDOWN_GRACE`] for a graceful axum drain ->
    /// SIGKILL + reap only if it ignored SIGTERM. Consumes `self` so a
    /// session cannot be shut down twice.
    /// Test: `daemon_autospawn_tests::{shutdown_stops_a_daemon_we_spawned,
    /// shutdown_leaves_a_pre_existing_daemon_running}`.
    pub async fn shutdown(self) {
        match self.ownership {
            Ownership::Attached => {
                tracing::debug!(
                    url = %self.url,
                    "tcode tui: leaving the pre-existing daemon running (we did not start it)"
                );
            }
            Ownership::Spawned(child) => {
                tracing::debug!(url = %self.url, "tcode tui: stopping the daemon it started");
                terminate(child, SHUTDOWN_GRACE).await;
            }
        }
    }
}

/// Resolve a live daemon URL, starting a daemon if none is running.
///
/// Why/What/Test: see module docs — this function IS the policy.
/// `project` mirrors `tcode tui`'s own `--project`: it is forwarded to a
/// spawned daemon so the daemon's binding matches the TUI's, and OMITTED
/// when the TUI is projectless (a first-class state, not a degraded one).
pub async fn ensure_daemon(
    client: &reqwest::Client,
    project: Option<&Path>,
) -> Result<DaemonSession> {
    let tcode_exe = super::tcode_exe::resolve()?;
    let health_url = format!("http://127.0.0.1:{DEFAULT_HTTP_PORT}");
    ensure_daemon_with(client, project, &tcode_exe, &health_url).await
}

/// [`ensure_daemon`] with the binary and default daemon URL injected.
///
/// Why: mirrors `cli_client::StdioRpcClient::spawn`'s established shape —
/// the library half takes an explicit binary path so it stays testable with
/// a stub, and the `std::env`/well-known-port policy lives in the one CLI
/// wrapper above. Tests drive the real spawn/wait/teardown machinery
/// against a stub child and a `wiremock` health endpoint, with no dependence
/// on the developer's actual port 7882 or `$HOME`.
async fn ensure_daemon_with(
    client: &reqwest::Client,
    project: Option<&Path>,
    tcode_exe: &Path,
    health_base_url: &str,
) -> Result<DaemonSession> {
    match lookup_daemon(client).await {
        Lookup::Live(url) => Ok(DaemonSession {
            url,
            ownership: Ownership::Attached,
        }),
        // An explicit instruction we must not second-guess — see module docs.
        Lookup::Dead {
            url,
            source: Source::Env,
        } => Err(explicit_url_unreachable(&url)),
        Lookup::Dead {
            source: Source::File,
            ..
        }
        | Lookup::Absent => spawn_and_wait(project, tcode_exe, health_base_url).await,
    }
}

/// The error for a `TCODE_DAEMON_URL` that names an unreachable daemon.
///
/// Kept verbose on purpose: the operator has to know both that we refused to
/// spawn and exactly how to change that.
fn explicit_url_unreachable(url: &str) -> anyhow::Error {
    anyhow!(
        "{DAEMON_URL_ENV} is set to {url} but no daemon is responding there. \
         `tcode tui` will not start one for you while {DAEMON_URL_ENV} names an \
         address explicitly — spawning at a different address would ignore that \
         setting. Start `tcode serve --http` at {url}, or unset {DAEMON_URL_ENV} \
         and let `tcode tui` start a daemon itself."
    )
}

/// Spawn a daemon and block until it answers `GET {health_base_url}/health`.
async fn spawn_and_wait(
    project: Option<&Path>,
    tcode_exe: &Path,
    health_base_url: &str,
) -> Result<DaemonSession> {
    let log_path = daemon_log_path();
    let mut child = spawn_daemon(tcode_exe, project, log_path.as_deref())?;
    let config = DaemonGuardConfig {
        startup_timeout: STARTUP_TIMEOUT,
        ..DaemonGuardConfig::new(
            format!("{health_base_url}/health"),
            "the tcode daemon",
            log_hint(log_path.as_deref()),
        )
    };

    // Race readiness against the child dying: a daemon whose port is already
    // taken exits in milliseconds, and spinning out the full budget to then
    // report a timeout would hide the real cause. Bound to an `Outcome` so
    // the `&mut child` borrow the `wait()` future holds ends with the
    // `select!` expression, leaving `child` movable below.
    let outcome = tokio::select! {
        ready = spin_until_ready(&config) => Outcome::Ready(ready),
        exit = child.wait() => Outcome::Exited(exit.map(|s| s.to_string())),
    };

    match outcome {
        Outcome::Ready(Ok(())) => {
            // Healthy AND still running. `try_wait` closes the race where
            // the health URL was answered by SOMETHING ELSE while our own
            // child died (e.g. another daemon already held the port).
            if matches!(child.try_wait(), Ok(Some(_))) {
                return Err(exited_early(
                    "it exited during startup",
                    log_path.as_deref(),
                ));
            }
            Ok(DaemonSession {
                url: health_base_url.to_string(),
                ownership: Ownership::Spawned(child),
            })
        }
        Outcome::Ready(Err(e)) => {
            // Never leak the child we just gave up on.
            terminate(child, SHUTDOWN_GRACE).await;
            Err(e)
        }
        Outcome::Exited(status) => {
            let detail = match status {
                Ok(s) => format!("it exited during startup ({s})"),
                Err(e) => format!("its exit status could not be read ({e})"),
            };
            Err(exited_early(&detail, log_path.as_deref()))
        }
    }
}

/// Which arm of [`spawn_and_wait`]'s race finished first.
enum Outcome {
    Ready(Result<()>),
    Exited(std::io::Result<String>),
}

/// Build and spawn `<tcode_exe> serve --http [--project <path>]`.
///
/// `kill_on_drop(true)` is a BACKSTOP, not the teardown path: a panic or
/// early return that drops the session without calling
/// [`DaemonSession::shutdown`] must not leak a daemon holding the port. The
/// graceful SIGTERM path in [`terminate`] always runs first on the normal
/// exit, reaping the child so this drop hook becomes a no-op.
fn spawn_daemon(
    tcode_exe: &Path,
    project: Option<&Path>,
    log_path: Option<&Path>,
) -> Result<Child> {
    let mut cmd = Command::new(tcode_exe);
    cmd.arg("serve").arg("--http");
    if let Some(project) = project {
        cmd.arg("--project").arg(project);
    }
    cmd.stdin(Stdio::null());
    match log_path.and_then(open_log) {
        Some((out, err)) => {
            cmd.stdout(out).stderr(err);
        }
        None => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    cmd.kill_on_drop(true).spawn().with_context(|| {
        format!(
            "tcode tui: could not start a daemon with `{} serve --http`",
            tcode_exe.display()
        )
    })
}

/// Open (append, creating as needed) two handles onto the daemon log — one
/// each for the child's stdout and stderr. `None` on any failure, which
/// downgrades the spawn to null-ed output rather than failing the launch.
fn open_log(path: &Path) -> Option<(Stdio, Stdio)> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let open = || {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    };
    Some((Stdio::from(open()?), Stdio::from(open()?)))
}

/// `{resolve_data_dir("trusty-code")}/tui-spawned-daemon.log`, or `None` if
/// the data directory cannot be resolved (never fatal — see
/// [`DAEMON_LOG_FILENAME`]).
fn daemon_log_path() -> Option<PathBuf> {
    trusty_common::resolve_data_dir("trusty-code")
        .ok()
        .map(|dir| dir.join(DAEMON_LOG_FILENAME))
}

/// "see <log>" pointer appended to startup failures, or a generic hint when
/// no log file could be opened.
fn log_hint(log_path: Option<&Path>) -> String {
    match log_path {
        Some(path) => format!("see {} for the daemon's own output", path.display()),
        None => "run `tcode serve --http` by hand to see why".to_string(),
    }
}

/// Error for a spawned daemon that died instead of coming up.
fn exited_early(detail: &str, log_path: Option<&Path>) -> anyhow::Error {
    anyhow!(
        "tcode tui: started a daemon but {detail} — {}. A daemon already \
         holding port {DEFAULT_HTTP_PORT} is the usual cause.",
        log_hint(log_path)
    )
}

/// SIGTERM -> grace -> SIGKILL, reaping the child either way.
///
/// Why: SIGTERM lets `tcode serve --http`'s axum server run its graceful
/// shutdown (`trusty_common::shutdown_signal`) and drain in-flight requests,
/// per the workspace's connection-safe restart convention (#534) — a bare
/// kill would drop live SSE subscribers mid-stream. The SIGKILL escalation
/// exists so a wedged daemon still cannot outlive the TUI and keep the port.
/// Mirrors `trusty-agents`' `ui/src-tauri/src/sidecar.rs::terminate_child`
/// (that one is `pub(crate)` inside a Tauri binary crate, so it cannot be
/// imported — only its shape reused).
async fn terminate(mut child: Child, grace: Duration) {
    // Already gone: reap without signalling.
    if let Ok(Some(_)) = child.try_wait() {
        return;
    }

    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: `pid` names a child process we spawned and have not yet
        // reaped, and `SIGTERM` is a valid signal. `kill` has no
        // memory-safety effects — a already-exited pid merely returns ESRCH,
        // which is exactly the case the `try_wait` above handles.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        if tokio::time::timeout(grace, child.wait()).await.is_ok() {
            return;
        }
        tracing::warn!(
            pid,
            ?grace,
            "tcode daemon did not exit on SIGTERM within grace; sending SIGKILL"
        );
    }

    let _ = child.start_kill();
    let _ = child.wait().await;
}

#[cfg(test)]
#[path = "daemon_autospawn_tests.rs"]
mod daemon_autospawn_tests;
