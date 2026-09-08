//! Attach to a running `tcode serve` daemon, or START one — and then LEAVE IT
//! RUNNING, whoever started it (#4512; retransported onto the daemon's Unix
//! socket in #6637).
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
//! circumstance — not even one it started itself.
//!
//! What: [`ensure_daemon`] resolves the daemon's socket
//! (`trusty_code::serve::uds::socket_path`) and returns it:
//!
//! * **Live daemon, serving the SAME project** -> attach. Nothing is spawned
//!   and nothing is owned.
//! * **Live daemon, serving a DIFFERENT project** -> hard error naming both
//!   projects (see [`check_binding`]). We neither attach — that would silently
//!   operate against the wrong repository — nor start a competing daemon on a
//!   socket that is already bound.
//! * **Nothing answering** -> spawn `<current_exe> serve --http [--project
//!   <path>]` as a child and wait for the SOCKET to answer. The child handle is
//!   dropped once it is up; the daemon outlives this process.
//!
//! **The `TCODE_DAEMON_URL` branch is gone (#6637).** It existed so an operator
//! could point the TUI at a daemon on another port, and its refuse-to-spawn arm
//! existed so a spawn could not silently ignore that instruction. A socket path
//! is derived from the data directory, so `TRUSTY_DATA_DIR_OVERRIDE` already
//! points both this client and a spawned daemon at the same place — there is no
//! address to name and no instruction to contradict. `--http` stays on the
//! spawn command line only because `trusty-code-gui`'s webview still needs the
//! transient TCP listener; the socket binds either way.
//!
//! The binary is resolved with [`super::tcode_exe::resolve`]
//! (`std::env::current_exe()`), never a bare `tcode` PATH lookup, so a
//! locally built binary spawns ITSELF rather than a stale installed copy.
//! Test: `daemon_autospawn_tests::*` (sibling file, per the 500-SLOC
//! production cap) covers every branch against a stub daemon on a real socket
//! and stub child binaries.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use tokio::process::{Child, Command};
use trusty_code::binding::ProjectBinding;
use trusty_code::tui_client::uds_rpc::UdsRpcClient;
use trusty_common::uds::socket_is_serving;

/// How long a daemon WE spawned gets to answer on its socket before we give up
/// and report the failure.
///
/// Shorter than `daemon_guard::DEFAULT_STARTUP_TIMEOUT` (30s) because this
/// wait sits between the operator pressing Enter and a TUI appearing — 20s
/// of waiting is already the outer edge of tolerable, and a tcode daemon
/// that has not bound its socket by then is not about to.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

/// How long one liveness probe waits for the socket to accept.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Gap between readiness probes while waiting on a spawned daemon.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Filename (under `resolve_data_dir("trusty-code")`) the spawned daemon's
/// stdout/stderr are appended to.
///
/// Why a file rather than inherited fds: the TUI takes over the terminal
/// with an alternate screen immediately after this, so an inherited stderr
/// would scribble daemon log lines across the rendered UI. Null-ing it
/// instead would make a failed startup completely undiagnosable, so we keep
/// the output and name the file in the error message.
const DAEMON_LOG_FILENAME: &str = "tui-spawned-daemon.log";

/// The project binding a live daemon published on `health` (#4512).
///
/// Why: a daemon binds exactly ONE `ProjectBinding` at
/// `trusty_code::serve::build_router` time and keeps it for its whole life.
/// Auto-attach picks a daemon up off a well-known path without the operator
/// choosing one, so a TUI launched in project B would find project A's daemon
/// and drive it — every session, index, and file operation landing in the
/// wrong repository. Making the binding part of the check is what lets a
/// caller refuse.
/// What: the daemon's bound project root, an explicit projectless state, or
/// [`Unreported`](ReportedBinding::Unreported) when `health` answered but
/// carried no usable `binding` field. Those last two are deliberately one
/// variant: both mean "this daemon's project CANNOT be verified", and a caller
/// must treat an unverifiable daemon the same way regardless of why.
/// Test: `daemon_autospawn_tests::reported_binding_parses_every_health_shape`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportedBinding {
    /// The daemon reported it serves no project.
    Projectless,
    /// The daemon reported this canonical project root.
    Bound(PathBuf),
    /// `health` answered, but with no parseable `binding` field.
    Unreported,
}

impl ReportedBinding {
    /// Read a [`ReportedBinding`] out of a `health` result payload.
    ///
    /// Why/What: reuses `ProjectBinding`'s own `Deserialize` (the
    /// `{state, root}` wire shape defined once in `trusty_code::binding`)
    /// rather than re-spelling that shape here, so the reader can never drift
    /// from the writer. A missing or malformed field is
    /// [`Unreported`](ReportedBinding::Unreported), never a guess.
    /// Test: `daemon_autospawn_tests::reported_binding_parses_every_health_shape`.
    pub fn from_health(health: &serde_json::Value) -> Self {
        let Some(raw) = health.get("binding") else {
            return Self::Unreported;
        };
        match serde_json::from_value::<ProjectBinding>(raw.clone()) {
            Ok(binding) => match binding.root() {
                Some(root) => Self::Bound(root.to_path_buf()),
                None => Self::Projectless,
            },
            Err(_) => Self::Unreported,
        }
    }

    /// How to name this binding in an operator-facing message.
    ///
    /// Why: a mismatch error has to print BOTH sides, and "no project" has to
    /// read as a deliberate state rather than as missing information.
    /// Test: `daemon_autospawn_tests::reported_binding_describes_each_state`.
    pub fn describe(&self) -> String {
        match self {
            Self::Projectless => "<projectless>".to_string(),
            Self::Bound(root) => root.display().to_string(),
            Self::Unreported => "<unreported — daemon predates #4512>".to_string(),
        }
    }
}

/// Resolve a live daemon socket for `project`, starting a daemon if none is
/// running.
///
/// Why/What/Test: see module docs — this function IS the policy. `project`
/// mirrors `tcode tui`'s own `--project` (already canonicalized by
/// `cli::tui::resolve_project`): it is forwarded to a spawned daemon so the
/// daemon's binding matches the TUI's, OMITTED when the TUI is projectless (a
/// first-class state, not a degraded one), and checked against the binding an
/// already-running daemon reports.
pub async fn ensure_daemon(project: Option<&Path>) -> Result<PathBuf> {
    let tcode_exe = super::tcode_exe::resolve()?;
    let socket = trusty_code::serve::uds::socket_path()?;
    ensure_daemon_with(project, &tcode_exe, &socket).await
}

/// [`ensure_daemon`] with the binary and socket path injected.
///
/// Why: mirrors `cli_client::StdioRpcClient::spawn`'s established shape — the
/// library half takes explicit paths so it stays testable with a stub, and the
/// `current_exe`/well-known-path policy lives in the one CLI wrapper above.
async fn ensure_daemon_with(
    project: Option<&Path>,
    tcode_exe: &Path,
    socket: &Path,
) -> Result<PathBuf> {
    if socket_is_serving(socket, PROBE_TIMEOUT).await {
        // #4512: a daemon answering is not the same as a daemon serving the
        // project we mean to work in.
        let reported = reported_binding(socket).await;
        check_binding(socket, &reported, project)?;
        return Ok(socket.to_path_buf());
    }
    spawn_and_wait(project, tcode_exe, socket).await
}

/// Ask a live daemon which project it serves.
///
/// A call that fails for any reason answers [`ReportedBinding::Unreported`],
/// which [`check_binding`] refuses — the check fails CLOSED, because an
/// unverified project is how work lands in the wrong repository.
async fn reported_binding(socket: &Path) -> ReportedBinding {
    match UdsRpcClient::new(socket)
        .call("health", serde_json::json!({}))
        .await
    {
        Ok(payload) => ReportedBinding::from_health(&payload),
        Err(_) => ReportedBinding::Unreported,
    }
}

/// Accept a discovered daemon only if it serves the project the client wants.
///
/// Why: a daemon binds exactly one `ProjectBinding` for its whole life, and
/// auto-attach picks daemons up off a well-known path without the operator
/// choosing one. Before #4512 the binding was not even on the wire, so a TUI
/// launched in project B would attach to project A's daemon and every session,
/// index, and file operation would land in the wrong repository — silently.
/// Mismatch is therefore a hard error, and it is deliberately NOT an
/// auto-spawn trigger: the socket is already bound, so "just start our own"
/// would either fail to bind or race the incumbent.
///
/// A projectless client meeting a project-bound daemon (and the reverse) is
/// a MISMATCH, not a compatible pair. Projectless is a deliberate,
/// first-class state — chat/planning with no index, no diff target and no
/// project-scoped memory — so attaching a projectless TUI to a bound daemon
/// would silently grant it a project the operator never named, and attaching a
/// project-bound TUI to a projectless daemon would silently withdraw the
/// indexing and git affordances the operator explicitly asked for. Neither
/// side can be repaired after the fact, because the daemon's binding is
/// fixed at its startup.
///
/// A daemon that reports NO binding ([`ReportedBinding::Unreported`]) is
/// refused as well. It fails CLOSED on purpose: the whole point of the check
/// is that an unverified project is how work lands in the wrong repository,
/// and "old daemon" is not evidence that its project is right. The remedy —
/// restart it — is in the message.
/// What: `Ok(())` when both sides name the same project or both are
/// projectless; otherwise an error naming `socket`, both projects, and the
/// ways forward.
/// Test: `daemon_autospawn_tests::{refuses_a_daemon_bound_to_another_project,
/// refuses_a_project_bound_client_against_a_projectless_daemon,
/// refuses_a_daemon_that_cannot_report_its_binding,
/// attaches_to_a_live_daemon_without_spawning}`.
fn check_binding(socket: &Path, reported: &ReportedBinding, wanted: Option<&Path>) -> Result<()> {
    let wanted_label = wanted
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<projectless>".to_string());
    let socket = socket.display();
    match (reported, wanted) {
        (ReportedBinding::Projectless, None) => Ok(()),
        (ReportedBinding::Bound(root), Some(wanted)) if root == wanted => Ok(()),
        (ReportedBinding::Unreported, _) => Err(anyhow!(
            "the tcode daemon on {socket} does not report which project it serves, \
             so `tcode tui` cannot confirm it is bound to {wanted_label} — and \
             attaching to the wrong project would run every session against the \
             wrong repository. Stop it and let `tcode tui` start a current one."
        )),
        _ => Err(anyhow!(
            "the tcode daemon on {socket} serves a different project than this TUI: \
             daemon = {daemon}, requested = {wanted_label}. `tcode tui` will not \
             attach to it (every session would run against the wrong project) and \
             will not start a competing daemon on a socket that is already bound. \
             Either relaunch this TUI against {daemon}, or stop that daemon and \
             let this one start its own.",
            daemon = reported.describe(),
        )),
    }
}

/// Spawn a daemon and block until its SOCKET answers.
///
/// #6637: readiness is the socket accepting a connection, not `GET /health`
/// answering. The socket is what every client in this crate dials, and it is
/// bound first and fatally by `serve::uds::run_daemon`, so a daemon that
/// answers here is a daemon the TUI can actually drive — whereas a healthy
/// HTTP port proved only that the transient listener came up.
///
/// The `Child` handle is dropped on every path, without a kill: a daemon
/// this process started is a daemon it must not stop (module docs). Even the
/// failure paths leave it alone — a half-started daemon may still be binding
/// its socket, and killing it would be the same "client tears down a shared
/// service" mistake at a worse moment. The error names the log file instead.
async fn spawn_and_wait(
    project: Option<&Path>,
    tcode_exe: &Path,
    socket: &Path,
) -> Result<PathBuf> {
    let log_path = daemon_log_path();
    let mut child = spawn_daemon(tcode_exe, project, log_path.as_deref())?;

    // Race readiness against the child dying: a daemon whose socket is already
    // held exits in milliseconds, and spinning out the full budget to then
    // report a timeout would hide the real cause. Bound to an `Outcome` so the
    // `&mut child` borrow the `wait()` future holds ends with the `select!`.
    let outcome = tokio::select! {
        ready = wait_for_socket(socket) => Outcome::Ready(ready),
        exit = child.wait() => Outcome::Exited(exit.map(|s| s.to_string())),
    };

    match outcome {
        Outcome::Ready(Ok(())) => {
            // Answering AND still running. `try_wait` closes the race where
            // the socket was answered by SOMETHING ELSE while our own child
            // died (e.g. another daemon already held the path).
            if matches!(child.try_wait(), Ok(Some(_))) {
                return Err(exited_early(
                    "it exited during startup",
                    log_path.as_deref(),
                ));
            }
            Ok(socket.to_path_buf())
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

/// Poll until the socket accepts, or [`STARTUP_TIMEOUT`] elapses.
///
/// Test: `daemon_autospawn_tests::daemon_autospawn_waits_on_socket_not_http`.
async fn wait_for_socket(socket: &Path) -> Result<()> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if socket_is_serving(socket, PROBE_TIMEOUT).await {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "tcode tui: the daemon did not answer on {} within {STARTUP_TIMEOUT:?}",
                socket.display()
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Which arm of [`spawn_and_wait`]'s race finished first.
enum Outcome {
    Ready(Result<()>),
    Exited(std::io::Result<String>),
}

/// Build and spawn `<tcode_exe> serve --http [--project <path>]`.
///
/// `--http` is unchanged from before #6637 and is not a contradiction: the
/// persistent-daemon mode binds the socket first and fatally, and the flag now
/// selects only whether the transient TCP listener `trusty-code-gui` still
/// needs comes up beside it. PR 2 drops the flag with the listener.
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
         holding the socket is the usual cause.",
        log_hint(log_path)
    )
}

#[cfg(test)]
#[path = "daemon_autospawn_tests.rs"]
mod daemon_autospawn_tests;
