//! Attach to a running `tcode serve --http` daemon, or START one — and
//! then LEAVE IT RUNNING, whoever started it (#4512).
//!
//! Why: `tcode tui` shipped (#4424, PR #4433) refusing to run without a
//! hand-started daemon: discovery failed, the command printed an "actionable
//! message" and exited. DOC-50 §4.1 deferred auto-spawn to Phase 2, but an
//! interactive command that demands the operator go start a background
//! service first is not a shippable first-run experience — the owner
//! directive of 2026-07-31 overturns that deferral. This module is the
//! resulting policy layer, kept OUT of `tui.rs` so the launch path stays a
//! thin "resolve args -> build engine -> run" wiring file (`crate::cli`'s
//! contract) and so the lifetime and binding rules below are stated in one
//! place.
//!
//! **The TUI never stops the daemon.** Per the owner directive of
//! 2026-08-01, the tcode daemon is the process that owns PM lifecycle, agent
//! dispatch, and inter-agent communication; a TUI is one of possibly several
//! CLIs/TUIs *attached* to it. Quitting a client must therefore never end
//! live PM or agent work, so this module signals the daemon on exit under NO
//! circumstance — not even one it started itself. A daemon `tcode tui`
//! spawned simply keeps running afterwards, exactly like one started by
//! hand. Quiescence-gated idle exit (a daemon that stops itself once it has
//! no attached clients AND no active PM/agent sessions) is separate
//! follow-up work, deliberately not implemented here: getting it wrong in
//! either direction destroys work or leaks processes, and it needs its own
//! lease/refcount design rather than a client-side kill.
//!
//! What: [`ensure_daemon`] resolves a daemon URL through the UNCHANGED
//! discovery order (`TCODE_DAEMON_URL`, then the `http_addr` file, each
//! liveness-pinged — `tui_client::discovery::lookup_daemon`) and returns the
//! base URL to drive:
//!
//! * **Live daemon found, serving the SAME project** -> attach. Nothing is
//!   spawned and nothing is owned.
//! * **Live daemon found, serving a DIFFERENT project** -> hard error naming
//!   both projects (see [`check_binding`]). We neither attach — that would
//!   silently operate against the wrong repository — nor start a competing
//!   daemon on a port that is already taken.
//! * **`TCODE_DAEMON_URL` set but unreachable** -> hard error naming that
//!   URL. Auto-spawn deliberately does NOT apply here: the operator named an
//!   address, and starting a daemon at a DIFFERENT address (the default
//!   port) would silently ignore that instruction.
//! * **Discovery file stale or absent** -> spawn
//!   `<current_exe> serve --http [--project <path>]` as a child and wait for
//!   it to answer `GET /health`. The child handle is dropped once it is
//!   healthy; the daemon outlives this process.
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
//! production cap) covers every branch against a `wiremock` health endpoint
//! and stub child binaries — attach-without-spawning, spawn, refuse-for-an-
//! explicit-URL, refuse-on-a-binding-mismatch, and the universal
//! never-signal-the-daemon-on-exit rule.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tokio::process::{Child, Command};
use trusty_code::serve::DEFAULT_HTTP_PORT;
use trusty_code::tui_client::discovery::{
    DAEMON_URL_ENV, Lookup, ReportedBinding, Source, lookup_daemon,
};
use trusty_common::daemon_guard::{DaemonGuardConfig, spin_until_ready};

/// How long a daemon WE spawned gets to become healthy before we give up and
/// report the failure.
///
/// Shorter than `daemon_guard::DEFAULT_STARTUP_TIMEOUT` (30s) because this
/// wait sits between the operator pressing Enter and a TUI appearing — 20s
/// of spinner is already the outer edge of tolerable, and a tcode daemon
/// that has not bound its port by then is not about to.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

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

/// Resolve a live daemon URL for `project`, starting a daemon if none is
/// running.
///
/// Why/What/Test: see module docs — this function IS the policy.
/// `project` mirrors `tcode tui`'s own `--project` (already canonicalized by
/// `cli::tui::resolve_project`): it is forwarded to a spawned daemon so the
/// daemon's binding matches the TUI's, OMITTED when the TUI is projectless (a
/// first-class state, not a degraded one), and — since #4512 — checked
/// against the binding an already-running daemon reports.
pub async fn ensure_daemon(client: &reqwest::Client, project: Option<&Path>) -> Result<String> {
    let tcode_exe = super::tcode_exe::resolve()?;
    let health_url = format!("http://127.0.0.1:{DEFAULT_HTTP_PORT}");
    ensure_daemon_with(client, project, &tcode_exe, &health_url).await
}

/// [`ensure_daemon`] with the binary and default daemon URL injected.
///
/// Why: mirrors `cli_client::StdioRpcClient::spawn`'s established shape —
/// the library half takes an explicit binary path so it stays testable with
/// a stub, and the `std::env`/well-known-port policy lives in the one CLI
/// wrapper above. Tests drive the real spawn/wait machinery against a stub
/// child and a `wiremock` health endpoint, with no dependence on the
/// developer's actual port 7882 or `$HOME`.
async fn ensure_daemon_with(
    client: &reqwest::Client,
    project: Option<&Path>,
    tcode_exe: &Path,
    health_base_url: &str,
) -> Result<String> {
    match lookup_daemon(client).await {
        // #4512: a daemon answering is not the same as a daemon serving the
        // project we mean to work in.
        Lookup::Live { url, binding } => check_binding(&url, &binding, project).map(|()| url),
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

/// Accept a discovered daemon only if it serves the project the client wants.
///
/// Why: a daemon binds exactly one `ProjectBinding` for its whole life
/// (`serve::build_router`), and auto-attach picks daemons up off a
/// well-known port without the operator choosing one. Before #4512 the
/// binding was not even on the wire, so a TUI launched in project B would
/// attach to project A's daemon and every session, index, and file operation
/// would land in the wrong repository — silently. Mismatch is therefore a
/// hard error, and it is deliberately NOT an auto-spawn trigger: the port is
/// already taken, so "just start our own" would either fail to bind or race
/// the incumbent.
///
/// A projectless client meeting a project-bound daemon (and the reverse) is
/// a MISMATCH, not a compatible pair. Projectless is a deliberate,
/// first-class state — chat/planning with no index, no diff target and no
/// project-scoped memory — so attaching a projectless TUI to a bound daemon
/// would silently grant it a project the operator never named (exactly the
/// implicit-binding bug `ProjectBinding` exists to prevent), and attaching a
/// project-bound TUI to a projectless daemon would silently withdraw the
/// indexing and git affordances the operator explicitly asked for. Neither
/// side can be repaired after the fact, because the daemon's binding is
/// fixed at its startup.
///
/// A daemon that reports NO binding ([`ReportedBinding::Unreported`], i.e. a
/// build older than #4512) is refused as well. It fails CLOSED on purpose:
/// the whole point of the check is that an unverified project is how work
/// lands in the wrong repository, and "old daemon" is not evidence that its
/// project is right. The remedy — restart it — is in the message.
/// What: `Ok(())` when both sides name the same project or both are
/// projectless; otherwise an error naming `url`, both projects, and the ways
/// forward.
/// Test: `daemon_autospawn_tests::{refuses_a_daemon_bound_to_another_project,
/// refuses_a_project_bound_client_against_a_projectless_daemon,
/// refuses_a_daemon_that_cannot_report_its_binding,
/// attaches_to_a_live_daemon_without_spawning}`.
fn check_binding(url: &str, reported: &ReportedBinding, wanted: Option<&Path>) -> Result<()> {
    let wanted_label = wanted
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<projectless>".to_string());
    match (reported, wanted) {
        (ReportedBinding::Projectless, None) => Ok(()),
        (ReportedBinding::Bound(root), Some(wanted)) if root == wanted => Ok(()),
        (ReportedBinding::Unreported, _) => Err(anyhow!(
            "the tcode daemon at {url} does not report which project it serves, \
             so `tcode tui` cannot confirm it is bound to {wanted_label} — and \
             attaching to the wrong project would run every session against the \
             wrong repository. That daemon predates the binding check (#4512): \
             stop it and let `tcode tui` start a current one, or restart it from \
             this build of `tcode serve --http`."
        )),
        _ => Err(anyhow!(
            "the tcode daemon at {url} serves a different project than this TUI: \
             daemon = {daemon}, requested = {wanted_label}. `tcode tui` will not \
             attach to it (every session would run against the wrong project) and \
             will not start a competing daemon on a port that is already in use. \
             Either relaunch this TUI against {daemon}, or stop that daemon and \
             let this one start its own, or point `{DAEMON_URL_ENV}` at a daemon \
             serving {wanted_label}.",
            daemon = reported.describe(),
        )),
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
///
/// The `Child` handle is dropped on every path, without a kill: a daemon
/// this process started is a daemon it must not stop (module docs). Even the
/// failure paths leave it alone — a half-started daemon may still be binding
/// its port, and killing it would be the same "client tears down a shared
/// service" mistake at a worse moment. The error names the log file instead.
async fn spawn_and_wait(
    project: Option<&Path>,
    tcode_exe: &Path,
    health_base_url: &str,
) -> Result<String> {
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
    // `select!` expression.
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
            Ok(health_base_url.to_string())
        }
        Outcome::Ready(Err(e)) => Err(e),
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
/// There is deliberately no `kill_on_drop(true)`: the spawned daemon must
/// survive this process, so the one thing the `Child` handle must NOT do is
/// signal it when the TUI's stack unwinds (module docs, owner directive
/// 2026-08-01).
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
    cmd.spawn().with_context(|| {
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

#[cfg(test)]
#[path = "daemon_autospawn_tests.rs"]
mod daemon_autospawn_tests;
