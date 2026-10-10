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
//! What: [`ensure_daemon`] picks between two sockets — the shared one
//! (`trusty_code::serve::uds::socket_path`) and this project's own sibling
//! (`trusty_code::serve::uds::project_socket_path`, #4600) — and returns the
//! one to drive. It never attaches to a socket whose daemon has not reported
//! this TUI's own project ([`check_binding`]):
//!
//! * **Own socket live** -> attach if it serves the SAME project; otherwise a
//!   hard error naming both projects. Probed first, so a daemon an earlier
//!   launch started there is reused rather than duplicated.
//! * **Shared socket live, SAME project** -> attach. Nothing is spawned.
//! * **Shared socket live, DIFFERENT project** -> do not attach, and do not
//!   compete for that socket: start this project's daemon on its own socket
//!   (`serve --project-socket`). Before #4600 this was a hard error, so a
//!   second project could not run at all.
//! * **Shared socket live, project UNREPORTED** (no binding, or `health`
//!   failed) -> hard error naming that socket. It may already serve this
//!   project, so neither an attach nor a second daemon is safe.
//! * **Nothing answering** -> start the daemon on the shared socket. If that
//!   start fails, or another daemon answers first (two TUIs starting at once),
//!   the shared socket is probed again and decided by the three rows above.
//!
//! A started daemon is `<current_exe> serve --http --port 0 [--project-socket]
//! [--project <path>]`, a child in its own session; `--port 0` because a fixed
//! port made the second project's daemon die on bind (#4600). The child handle
//! is dropped once its socket answers AND reports this project; the daemon
//! outlives this process, and a terminal hangup never reaches it.
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

/// [`ensure_daemon`] with the binary and the shared socket path injected.
///
/// Why: mirrors `cli_client::StdioRpcClient::spawn`'s established shape — the
/// library half takes explicit paths so it stays testable with a stub, and the
/// `current_exe`/well-known-path policy lives in the one CLI wrapper above.
/// What: the decision table in the module docs; `socket` is the shared path
/// and this project's own path is derived from it.
/// Test: `daemon_autospawn_tests::{a_second_project_starts_its_own_daemon_beside_the_first,
/// a_second_project_reuses_its_own_running_daemon,
/// refuses_another_projects_daemon_on_this_projects_own_socket,
/// refuses_an_unverifiable_shared_daemon_without_spawning,
/// a_cold_start_race_lost_to_another_project_starts_our_own_daemon}`.
async fn ensure_daemon_with(
    project: Option<&Path>,
    tcode_exe: &Path,
    socket: &Path,
) -> Result<PathBuf> {
    // #4600: this project's own socket first, so a daemon an earlier launch
    // put there is reused instead of a second one starting on the shared path.
    let own = trusty_code::serve::uds::project_socket_path(socket, project);
    if socket_is_serving(&own, PROBE_TIMEOUT).await {
        let reported = reported_binding(&own).await;
        check_binding(&own, &reported, project)?;
        return Ok(own);
    }
    if !socket_is_serving(socket, PROBE_TIMEOUT).await {
        let spawned = spawn_and_wait(project, tcode_exe, socket, false).await;
        // #4600: cold-start race. Another TUI's daemon can take the shared
        // socket between our probe and our child's bind, so whatever answers
        // there now decides — ours or not. Only a silent socket keeps the
        // spawn's own error.
        if let Err(e) = spawned
            && !socket_is_serving(socket, PROBE_TIMEOUT).await
        {
            return Err(e);
        }
    }
    use_shared_socket(project, tcode_exe, socket, &own).await
}

/// Decide what to do about a LIVE shared socket (#4600).
///
/// Why: since #4600 a project can run on its own socket, but only a daemon
/// PROVEN to serve another project may be passed over. An unreported one may
/// already serve this project, and a second daemon per project is never
/// started.
/// What: same project -> the shared socket; a different project -> start this
/// project's daemon on `own` and verify its binding (fails closed); unreported
/// or failed `health` -> [`unverified_shared_daemon`].
/// Test: `daemon_autospawn_tests::{refuses_an_unverifiable_shared_daemon_without_spawning,
/// a_cold_start_race_lost_to_another_project_starts_our_own_daemon,
/// a_cold_start_race_lost_to_an_unverifiable_daemon_is_refused,
/// a_cold_start_race_lost_to_our_own_project_attaches}`.
async fn use_shared_socket(
    project: Option<&Path>,
    tcode_exe: &Path,
    shared: &Path,
    own: &Path,
) -> Result<PathBuf> {
    // #4512: a daemon answering is not the same as a daemon serving the
    // project we mean to work in.
    let reported = reported_binding(shared).await;
    match binding_fit(&reported, project) {
        BindingFit::Same => Ok(shared.to_path_buf()),
        BindingFit::Other => {
            spawn_and_wait(project, tcode_exe, own, true).await?;
            // #4600: verify before attaching — the own socket's answer need
            // not have come from our child.
            let reported = reported_binding(own).await;
            check_binding(own, &reported, project)?;
            Ok(own.to_path_buf())
        }
        BindingFit::Unverified => Err(unverified_shared_daemon(shared, project)),
    }
}

/// How a live daemon's reported binding relates to the project wanted (#4600).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindingFit {
    /// Both name the same project, or both are projectless.
    Same,
    /// The daemon reported a different project (projectless counts as one).
    Other,
    /// The daemon reported no project, or its `health` call failed.
    Unverified,
}

/// Classify `reported` against `wanted`; see [`check_binding`] for why
/// projectless and bound never fit each other.
fn binding_fit(reported: &ReportedBinding, wanted: Option<&Path>) -> BindingFit {
    match (reported, wanted) {
        (ReportedBinding::Projectless, None) => BindingFit::Same,
        (ReportedBinding::Bound(root), Some(wanted)) if root == wanted => BindingFit::Same,
        (ReportedBinding::Unreported, _) => BindingFit::Unverified,
        _ => BindingFit::Other,
    }
}

/// The refusal for a shared-socket daemon whose project cannot be verified.
///
/// Why (#4600): that daemon may already serve this project, so starting a
/// daemon beside it could give the project two. It may also be another
/// project's live work, which this TUI must not ask anyone to end (owner
/// directive 2026-08-01), so the remedy is a retry or a daemon on this
/// project's own socket, never a stop.
/// Test: `daemon_autospawn_tests::refuses_an_unverifiable_shared_daemon_without_spawning`.
fn unverified_shared_daemon(socket: &Path, wanted: Option<&Path>) -> anyhow::Error {
    let (wanted_label, project_arg) = match wanted {
        Some(root) => (
            root.display().to_string(),
            format!(" --project {}", root.display()),
        ),
        None => ("<projectless>".to_string(), String::new()),
    };
    anyhow!(
        "the tcode daemon on {socket} answered but did not report which project it \
         serves (it predates #4512, or its `health` call failed), so `tcode tui` \
         cannot tell whether it already serves {wanted_label}. It will not attach to \
         it, and will not start a second daemon for this project beside it. If it is \
         still starting up, run `tcode tui` again in a moment. If you know it serves \
         another project, start this project's own daemon with `tcode serve --http \
         --port 0 --project-socket{project_arg}`, then run `tcode tui` again.",
        socket = socket.display(),
    )
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
/// #8205 made this reachable without any flag at all: a bare `tcode tui` in a
/// repository now WANTS that repository, so an operator who never typed
/// `--project` can meet this refusal against a projectless daemon. The message
/// therefore names the flag that resolves it — see [`rematch_hint`].
/// Test: `daemon_autospawn_tests::{refuses_another_projects_daemon_on_this_projects_own_socket,
/// refuses_a_project_bound_client_against_a_projectless_daemon,
/// refuses_a_daemon_that_cannot_report_its_binding,
/// refusal_against_a_projectless_daemon_names_the_projectless_flag,
/// attaches_to_a_live_daemon_without_spawning}`.
fn check_binding(socket: &Path, reported: &ReportedBinding, wanted: Option<&Path>) -> Result<()> {
    let wanted_label = wanted
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<projectless>".to_string());
    let socket = socket.display();
    match binding_fit(reported, wanted) {
        BindingFit::Same => Ok(()),
        BindingFit::Unverified => Err(anyhow!(
            "the tcode daemon on {socket} does not report which project it serves, \
             so `tcode tui` cannot confirm it is bound to {wanted_label} — and \
             attaching to the wrong project would run every session against the \
             wrong repository. Stop it and let `tcode tui` start a current one."
        )),
        BindingFit::Other => Err(anyhow!(
            "the tcode daemon on {socket} serves a different project than this TUI: \
             daemon = {daemon}, requested = {wanted_label}. `tcode tui` will not \
             attach to it (every session would run against the wrong project) and \
             will not start a competing daemon on a socket that is already bound. \
             Either relaunch this TUI to match it ({rematch}), or stop that daemon \
             and let this one start its own.",
            daemon = reported.describe(),
            rematch = rematch_hint(reported),
        )),
    }
}

/// The exact command that relaunches this TUI to match `reported` (#8205).
///
/// Why: "relaunch this TUI against <daemon>" told an operator what to achieve
/// but not how, and since #8205 a bare `tcode tui` picks its own project from
/// the current directory — so "just run it again" reproduces the refusal. The
/// two mismatch classes need two different flags, and neither is guessable
/// from the old message.
/// What: `tcode tui --projectless` for a projectless daemon, `tcode tui
/// --project <root>` for a bound one. An [`ReportedBinding::Unreported`]
/// daemon reaches its own arm of `check_binding` and never this one, so it
/// falls back to the generic phrasing rather than naming a flag that would
/// not help.
/// Test: `daemon_autospawn_tests::refusal_against_a_projectless_daemon_names_the_projectless_flag`,
/// `daemon_autospawn_tests::refuses_another_projects_daemon_on_this_projects_own_socket`.
fn rematch_hint(reported: &ReportedBinding) -> String {
    match reported {
        ReportedBinding::Projectless => "run `tcode tui --projectless`".to_string(),
        ReportedBinding::Bound(root) => {
            format!("run `tcode tui --project {}`", root.display())
        }
        ReportedBinding::Unreported => "relaunch it against that daemon's project".to_string(),
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
///
/// #4600: `Ok` means only that the socket answers while our child runs, not
/// that our child answered. Two TUIs can both find the shared socket free and
/// both spawn; the loser's child is still starting when the winner's socket
/// answers. Every caller therefore checks the binding before attaching.
/// `project_socket` selects `serve --project-socket` (see [`spawn_daemon`]).
/// Test: `daemon_autospawn_tests::a_cold_start_race_lost_to_another_project_starts_our_own_daemon`.
async fn spawn_and_wait(
    project: Option<&Path>,
    tcode_exe: &Path,
    socket: &Path,
    project_socket: bool,
) -> Result<()> {
    let log_path = daemon_log_path();
    let mut child = spawn_daemon(tcode_exe, project, project_socket, log_path.as_deref())?;

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
            Ok(())
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

/// Build and spawn `<tcode_exe> serve --http --port 0 [--project-socket]
/// [--project <path>]`.
///
/// `--http` is unchanged from before #6637 and is not a contradiction: the
/// persistent-daemon mode binds the socket first and fatally, and the flag now
/// selects only whether the transient TCP listener comes up beside it. PR 2
/// drops the flag with the listener.
///
/// `--port 0` (#4600): that listener used to take the fixed default port, so
/// a second project's daemon — or any daemon while a stray test daemon held
/// the port — died on bind even though its socket was free. No in-tree client
/// dials the listener any more (`trusty-code-gui` bridges the socket), so an
/// OS-assigned port costs nothing. `project_socket` adds `--project-socket`,
/// which makes the daemon bind its project's own socket.
///
/// There is deliberately no `kill_on_drop(true)`: the spawned daemon must
/// survive this process, so the one thing the `Child` handle must NOT do is
/// signal it when the TUI's stack unwinds (module docs, owner directive
/// 2026-08-01). For the same reason it starts in its own session
/// (`daemon_guard::start_in_new_session`, #8783): otherwise closing the
/// terminal SIGHUPs the TUI's foreground group and the daemon with it.
///
/// Test: `daemon_autospawn_tests::the_spawned_daemon_leads_its_own_session`,
/// `daemon_autospawn_tests::a_second_project_starts_its_own_daemon_beside_the_first`.
fn spawn_daemon(
    tcode_exe: &Path,
    project: Option<&Path>,
    project_socket: bool,
    log_path: Option<&Path>,
) -> Result<Child> {
    let mut cmd = std::process::Command::new(tcode_exe);
    cmd.arg("serve").arg("--http").arg("--port").arg("0");
    if project_socket {
        cmd.arg("--project-socket");
    }
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
    trusty_common::daemon_guard::start_in_new_session(&mut cmd);
    Command::from(cmd).spawn().with_context(|| {
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
