//! Daemon entry point and its private helpers.
//!
//! Why: `run_daemon` and its private helpers (`wait_for_shutdown_signal`,
//! `spawn_telegram_bot`, `tailscale_deprecated_error`) are a coherent "boot
//! and serve" group; extracting them keeps `daemon.rs` under the 500-line cap
//! while co-locating the helpers that are only meaningful together.
//! What: `run_daemon` (the `daemon` subcommand handler) plus private helpers.
//! Test: `cli_parses_daemon_*` parse tests; `run_daemon_rejects_tailscale_flag`
//! covers the deprecation error; the #4397 bare-CLI guard's decision table is
//! covered hermetically by `commands::launchd_probe::tests::
//! compute_daemon_refuse_*` (`apply_supervision_signal` itself probes real
//! launchd/filesystem state, so it is not hermetically fakeable here); the
//! bind/serve path is exercised by `tests/daemon_no_tcp_6288_s3.rs`.

/// What an already-running daemon is reported as, wherever that is reported.
///
/// Why (#6869): `tm daemon`'s two guard arms printed `… already running at
/// http://127.0.0.1:7880` and `tm start` printed `Daemon already running on
/// <url>`, which put the daemon's port in front of the operator on paths where
/// it changes nothing they can act on. The URL is a discovery detail —
/// whatever `resolve_daemon_url` happened to return, or whatever `--addr`
/// asked for — and the move to a Unix socket (#6288) will make printing one
/// wrong. It is demoted to `tracing::debug!` at each site.
/// What: one fixed line. `pub(super)` so `daemon::start` renders the SAME
/// string rather than keeping a second copy that could drift.
/// Test: `already_running_notice_names_no_address`,
/// `start_already_running_line_names_no_address`.
pub(super) const ALREADY_RUNNING_NOTICE: &str = "the trusty-mpm daemon is already running";

/// `daemon` subcommand — run the daemon on its Unix socket (or the MCP server)
/// with lock-file service discovery.
///
/// Why: ADR-0032 leaves `trusty-console` as the only TCP surface; since #6288
/// S3a the daemon binds no TCP port and serves its socket only. The lock file
/// tells clients which process is the daemon and which socket it serves.
/// What: rejects `--tailscale` up front with an actionable error (see
/// [`tailscale_deprecated_error`]); refuses to start when a launchd unit is
/// registered but this process isn't launchd-supervised and `--force` was
/// not passed (issue #4397, see [`apply_supervision_signal`] and
/// [`unsupervised_daemon_error`]); in MCP mode delegates straight to
/// `run_mcp`; otherwise logs a deprecation line for a set `addr` (ignored),
/// removes a stale `http_addr` file, binds the socket and then writes the
/// lock (refusing, non-zero, when another daemon serves the socket),
/// registers a signal handler that removes the lock, then serves the socket.
/// A serve failure removes the lock and returns the error.
/// Test: `cli_parses_daemon_*` cover flag parsing; `run_daemon_rejects_
/// tailscale_flag` covers the deprecation error; `sandbox` (#9121) is
/// covered by `daemon_sandbox_tests.rs`; the #4397 guard's decision
/// table is covered hermetically by `commands::launchd_probe::tests::
/// compute_daemon_refuse_*`; the bind/serve path by
/// `a_sandbox_daemon_holds_no_tcp_listener_and_publishes_no_address`,
/// `a_busy_addr_flag_is_ignored_and_logged_once` and
/// `a_second_daemon_on_the_same_socket_exits_non_zero_and_leaves_the_lock`.
pub(crate) async fn run_daemon(
    addr: Option<String>,
    tailscale: bool,
    mcp: bool,
    force: bool,
    sandbox: bool,
) -> anyhow::Result<()> {
    // ADR-0011 loopback-only doctrine (#3330): the secondary Tailscale
    // listener has been removed. Fail loudly and immediately rather than
    // silently ignoring the flag — old invocations (scripts, launchd plists,
    // muscle memory) need a clear signal, not a quietly-degraded daemon.
    if tailscale {
        return Err(tailscale_deprecated_error());
    }
    // #9121: refuse an unisolated sandbox, and latch, before any credential
    // read, state construction or bind.
    if sandbox {
        crate::commands::daemon_sandbox::enter()?;
    }

    // Anchor cwd to a stable directory so that git subprocesses spawned later
    // never fail with "fatal: Unable to read current working directory" when the
    // inherited cwd has been deleted (e.g. a /private/tmp session dir cleaned up
    // by the OS). This is a best-effort set: we ignore the result so a read-only
    // filesystem cannot prevent the daemon from starting.
    // Why: daemon cwd is inherited by every subprocess (git clone, git worktree).
    // What: chdir to home on startup; fall back to "/" if home is unavailable.
    // Test: side-effect-only; verified via the managed-spawn integration suite
    //       and the git-clone defensive current_dir() added at each call site.
    if let Some(home) = dirs::home_dir() {
        let _ = std::env::set_current_dir(&home);
    } else {
        let _ = std::env::set_current_dir("/");
    }

    // #9274: the daemon owns the SM and portfolio palaces; it opens them as writer.
    let state = trusty_mpm::daemon::DaemonState::shared();

    // #2486: compute the launchd-supervision signal once, up front, so both
    // the MCP-mode early return and the HTTP-serving path below carry it on
    // `GET /health`. See `crate::commands::launchd_probe` for the full
    // restart-race rationale and the pure decision table.
    let supervised = apply_supervision_signal(&state);

    // #7822: publish the build THIS process runs — the semver `/health` reports
    // cannot distinguish two builds cut under the same version.
    // #7873: the id is compiled in, so it describes this process's own build no
    // matter when it is read, and is the same value the `tm` bin running
    // `tm doctor` carries. The startup call site stays only because that is
    // where every other `/health` field is recorded.
    state.set_build_identity(trusty_mpm::core::build_identity::build_id());

    // #4230: record the opt-in so `/health` can distinguish a deliberate
    // unsupervised run from an unwanted orphan. Without it `tm doctor` would call
    // a `--force` daemon an orphan — the very escape hatch #4397's refusal
    // recommends.
    state.set_unsupervised_forced(force);

    // #4397: the #2486 guard previously existed only on the MCP-bridge's
    // `no_spawn` path (`serve_stdio.rs`) — a bare `tm daemon` invocation
    // walked straight past the same hazard. Refuse here too unless the
    // operator explicitly opts in with `--force`.
    if crate::commands::launchd_probe::compute_daemon_refuse(supervised, force) {
        return Err(unsupervised_daemon_error());
    }

    if mcp {
        return trusty_mpm::daemon::run_mcp(state).await;
    }

    // #6288 S3a: the daemon serves its socket only. The two TCP `/health`
    // probes that guarded against a duplicate are gone with the port; the
    // socket bind below is the one guard — it refuses when another trusty-mpm
    // answers the path, and the process then exits non-zero having published
    // nothing (`bind_socket_then_publish`).
    if let Some(notice) = addr_deprecation_notice(addr.as_deref()) {
        tracing::warn!("{notice}");
    }
    // #6288 S3a: an older daemon's `http_addr` names a port nothing serves now.
    // Removing it advances no state, so a failure is only a warning.
    if let Err(e) = trusty_common::remove_daemon_addr("trusty-mpm") {
        tracing::warn!("failed to remove a stale trusty-mpm http_addr file: {e:#}");
    }

    // #6288: bind the RPC socket and only then write the lock file, which is
    // what clients discover us by. See [`bind_socket_then_publish`] for why the
    // order is the point.
    let socket_path = trusty_mpm::daemon::socket::socket_path()?;
    let lock_path = trusty_mpm::core::discovery::lock_file_path();
    let rpc = bind_socket_then_publish(&socket_path, &lock_path).await?;

    // Auto-start the Telegram bot alongside the daemon when a bot token is
    // configured. `resolve_token` honours `.env.local` → `.env` → the process
    // environment, so a single dotenv file configures both the daemon and the
    // bot. Without a token the daemon runs normally; only a warning is logged.
    // The returned token lets the shutdown handler stop the supervised bot
    // promptly so it never blocks (or outlives) graceful shutdown (#1499).
    // #9121: a sandbox starts no channel poller.
    // #6288: the in-process bot reaches this daemon over its socket, not TCP.
    let bot_url = trusty_mpm::core::socket_locator(&socket_path);
    let bot_shutdown = crate::commands::daemon_sandbox::gate_channel_pollers(sandbox, || {
        spawn_telegram_bot(&bot_url)
    });

    // Clean up the lock file on shutdown for BOTH Ctrl-C (SIGINT) and SIGTERM.
    // `tm restart` stops the old daemon with `pkill`, which sends SIGTERM — if we
    // only trapped SIGINT the lock file would leak with a dead PID, and the next
    // client's `resolve_daemon_url` would fall back to the default port (often
    // occupied by an unrelated process) and report "daemon unreachable".
    // #6849: this handler exits before the serve drain, so it flushes the bus
    // producer itself (bounded at 1 s; a no-op with no bus attached).
    let action_events_state = std::sync::Arc::clone(&state);
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        // Stop the supervised Telegram bot first so it does not log a spurious
        // restart while the process is on its way down.
        if let Some(token) = bot_shutdown {
            token.cancel();
        }
        trusty_mpm::daemon::action_events::before_signal_exit(
            &action_events_state,
            trusty_mpm::daemon::lock::remove_lock,
        )
        .await;
        // See #6288: this exit almost certainly wins the race against
        // `serve_with_shutdown`'s graceful drain, so neither the session reap
        // (#1455) nor the socket unlink is guaranteed to run on a real SIGTERM.
        // The socket is self-healing — `daemon::socket::bind` reclaims a file
        // nobody is serving. S3a leaves the race as it was.
        std::process::exit(0);
    });

    let served = trusty_mpm::daemon::serve(state, rpc).await;
    unpublish_if_failed(served, &lock_path)
}

/// The deprecation line for `--addr` / `TRUSTY_MPM_ADDR`, when either was set.
///
/// Why (#6288 S3a, owner ruling Q1): the daemon binds no TCP port, but the host
/// launchd plist still passes `--addr 127.0.0.1:7880` and restarts a failed
/// daemon every 10 s. A hard error would crash-loop it, so the value is
/// accepted, ignored and named once in the log. Removal is due in 2.0.0.
/// What: `Some(line)` naming the ignored value when `addr` is set, else `None`.
/// Test: `addr_deprecation_notice_names_the_ignored_value`;
/// `a_busy_addr_flag_is_ignored_and_logged_once` proves it is logged once.
fn addr_deprecation_notice(addr: Option<&str>) -> Option<String> {
    addr.map(|value| {
        format!(
            "--addr / TRUSTY_MPM_ADDR is deprecated and ignored (got {value:?}): the \
             daemon serves its Unix socket only (#6288, ADR-0032); the flag is \
             removed in 2.0.0"
        )
    })
}

/// Remove this daemon's lock when serving failed, and pass the result through.
///
/// Why (#6288 S3a, the Fail-Open Check): a failed serve makes `run_daemon`
/// return an error and the process exit non-zero. The lock written at bind
/// time would otherwise outlive it and name a daemon that serves nothing.
/// What: on `Err`, removes the lock at `lock_path` if it names this process;
/// on `Ok`, leaves it — the shutdown handler owns that path.
/// Test: `unpublish_if_failed_removes_the_lock_on_a_serve_error`,
/// `unpublish_if_failed_keeps_the_lock_on_a_clean_stop`.
fn unpublish_if_failed(
    served: anyhow::Result<()>,
    lock_path: &std::path::Path,
) -> anyhow::Result<()> {
    if served.is_err() {
        trusty_mpm::core::daemon_identity::remove_lock_owned_by_at(
            lock_path,
            &[std::process::id()],
        );
    }
    served
}

/// Bind the RPC socket, then publish the daemon's identity — in that order.
///
/// Why the order is the whole function (#6288): the lock file names a daemon
/// that is about to serve, and a lock written before a bind that then fails
/// would name a process that aborted; the next client would resolve a daemon
/// nothing answers. #6288 S3a: the socket is the only listener and this bind
/// is also the duplicate guard — another live trusty-mpm on the path refuses
/// it, and nothing is published.
///
/// What: [`trusty_mpm::daemon::socket::bind`], then
/// [`trusty_mpm::core::daemon_identity::write_lock_at`]. `lock_path` is a
/// parameter so the test can assert against a temp path instead of the
/// operator's real `~/.trusty-mpm/daemon.lock`.
///
/// # Errors
///
/// When the socket cannot be bound — including when another trusty-mpm is
/// already serving it. Nothing is published on that path.
///
/// Test: `bind_socket_then_publish_writes_no_lock_when_the_socket_is_taken`,
/// `bind_socket_then_publish_writes_the_lock_when_the_bind_succeeds`.
async fn bind_socket_then_publish(
    socket: &std::path::Path,
    lock_path: &std::path::Path,
) -> anyhow::Result<trusty_mpm::daemon::socket::BoundSocket> {
    let rpc = trusty_mpm::daemon::socket::bind(socket).await?;
    trusty_mpm::core::daemon_identity::write_lock_at(lock_path, &socket.display().to_string());
    Ok(rpc)
}

/// Block until the process receives a shutdown signal (SIGINT or SIGTERM).
///
/// Why: the daemon must remove its lock file on every graceful stop, not just
/// Ctrl-C. `tm restart` stops the old daemon with `pkill`, which delivers
/// SIGTERM; trapping only SIGINT would leak a stale lock file and break daemon
/// discovery for the next client.
/// What: races a `ctrl_c()` future against a Unix SIGTERM stream; on non-Unix
/// platforms (no SIGTERM) it just awaits Ctrl-C.
/// Test: covered indirectly by `tm restart` — the new daemon binds cleanly and
/// the lock file reflects its address rather than the killed daemon's.
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("failed to register SIGTERM handler: {e}");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Compute and store the launchd-supervision signal on `state` (issue #2486).
///
/// Why: a green `/health` alone does not prove launchd owns this process —
/// during a `bootout → cargo install → bootstrap` restart, a racing MCP stdio
/// bridge can auto-spawn an orphan daemon that answers `/health` 200 but is
/// missing the plist's `EnvironmentVariables` and runs with `cwd=$HOME`.
/// `crate::commands::launchd_probe::compute_supervised` combines "is THIS
/// process launchd-supervised" with "does a trusty-mpm launchd unit exist at
/// all" into the single safe/hazardous signal `GET /health` exposes.
/// What: probes for a trusty-mpm launchd plist, combines it with
/// `trusty_common::update::is_launchd_supervised()`, stores the result on
/// `state` via [`trusty_mpm::daemon::DaemonState::set_supervised`], logs a
/// `tracing::warn!` exactly once at startup when the result is hazardous, and
/// returns the `supervised` bool so the caller can also gate startup on it
/// (issue #4397).
/// Test: the pure decision table is covered by
/// `crate::commands::launchd_probe::tests`; this wrapper is side-effect-only
/// (env/filesystem probes + a state mutation) and is exercised via the daemon
/// e2e suite's boot path.
fn apply_supervision_signal(state: &trusty_mpm::daemon::DaemonState) -> bool {
    use trusty_common::supervision::{LaunchdSupervision, launchd_supervision};

    // #5623: fail-closed — an unreadable `~/Library/LaunchAgents` now counts as
    // "launchd may own this", so `/health` reports the hazard instead of a
    // `supervised: true` it never established.
    let plist_exists = crate::commands::launchd_probe::launchd_may_own_daemon();
    // #4469: ONE authoritative query, whose three-state answer is published on
    // `/health` alongside the bool it collapses into. Without the third state a
    // launchctl that could not be asked reads as `supervised: false`, and
    // `tm doctor`'s orphan verdict escalates that to a `kill -TERM`
    // recommendation against a daemon whose state is merely unknown.
    let answer = launchd_supervision();
    let discriminant = match &answer {
        LaunchdSupervision::Supervised(_) => "supervised",
        LaunchdSupervision::NotSupervised => "not_supervised",
        LaunchdSupervision::Unknown(_) => "unknown",
    };
    state.set_launchd_supervision(discriminant);
    if let LaunchdSupervision::Unknown(why) = &answer {
        tracing::warn!(
            reason = %why,
            "could not determine launchd supervision authoritatively — /health will \
             report launchd_supervision=unknown (issue #4469)"
        );
    }

    let is_launchd = answer.is_supervised();
    let supervised = crate::commands::launchd_probe::compute_supervised(is_launchd, plist_exists);
    state.set_supervised(supervised);
    if !supervised {
        // #4868: name the labels from the registry, not from literals. A hint
        // that prescribes a unit the host does not have is #2827 exactly.
        tracing::warn!(
            "unsupervised trusty-mpm daemon running alongside a launchd unit — \
             likely spawned by a client during a restart race; kill it and use \
             `launchctl kickstart -k gui/$(id -u)/{supervisor}` (or `{daemon}`) \
             to let launchd own the daemon.",
            supervisor = trusty_common::launchd_labels::MPM_SUPERVISOR,
            daemon = trusty_common::launchd_labels::MPM,
        );
    }
    supervised
}

/// Build the error returned when `run_daemon` refuses to start on the
/// bare-CLI path because a launchd unit is registered but this process is not
/// launchd-supervised (issue #4397, the bare-CLI counterpart of #2486's
/// MCP-bridge `no_spawn` guard).
///
/// Why: silently starting anyway (the pre-#4397 behaviour) creates exactly
/// the orphan-daemon hazard #2486 was meant to prevent — a duplicate,
/// unsupervised daemon that can hold a stale binary indefinitely (observed:
/// PID 35895 serving a stale 1.2.3 image since 2026-07-29 while the on-disk
/// binary was 1.3.0). The error names both remedies: let launchd own the
/// daemon via `launchctl kickstart`/`bootstrap`, or explicitly opt in with
/// `--force` when the operator really does want an unsupervised run (e.g.
/// local dev alongside a supervised instance on a different port).
/// What: returns an `anyhow::Error` with the actionable message; kept as a
/// separate function so the message text has exactly one source of truth for
/// both the runtime call site and any future caller.
/// Test: `unsupervised_daemon_error_names_remedies` below.
fn unsupervised_daemon_error() -> anyhow::Error {
    // #4868: labels come from the canonical registry so this remedy always
    // names units that exist.
    anyhow::anyhow!(
        "refusing to start: a trusty-mpm launchd unit is registered but this \
         process was not started by launchd (issue #2486/#4397) — starting \
         anyway would create a duplicate, unsupervised daemon that can hold a \
         stale binary indefinitely. Use \
         `launchctl kickstart -k gui/$(id -u)/{supervisor}` (or `{daemon}`) to \
         let launchd own the daemon, or pass `tm daemon --force` to start \
         unsupervised anyway.",
        supervisor = trusty_common::launchd_labels::MPM_SUPERVISOR,
        daemon = trusty_common::launchd_labels::MPM,
    )
}

/// Spawn the SUPERVISED Telegram bot as a background task when a token is
/// configured.
///
/// Why: an operator who has set `TELEGRAM_BOT_TOKEN` expects the bot to come up
/// with the daemon — not as a separate process they must remember to start —
/// AND to stay up. The previous bare spawn logged once and ended on the first
/// error (a 409 long-poll conflict, a network blip), so the Telegram surface
/// went silently dark until a full daemon restart (#1499). Wrapping the run loop
/// in the supervisor makes a transient failure self-heal with bounded
/// exponential backoff, makes a permanent misconfiguration back off and give up
/// loudly instead of tight-looping, and keeps the bot shutdown-safe.
/// What: resolves the token via `trusty_mpm::telegram::resolve_token` (which
/// reads `.env.local`, then `.env`, then the environment). When a token is found
/// the supervised bot loop ([`trusty_mpm::telegram::run_supervised`]) is spawned
/// on a tokio task pointed at `base_url`, cancellable via the returned
/// `CancellationToken` so graceful shutdown can stop it; when absent a single
/// warning is logged and the daemon continues. Returns the token (or `None` when
/// no bot was started) so the caller can cancel it on shutdown.
/// Test: token resolution is covered by `trusty-mpm-telegram`'s
/// `resolve_token_*` tests; the restart/give-up/cancellation logic is covered by
/// `telegram::supervisor::tests`; the spawn path is exercised by running the
/// daemon.
fn spawn_telegram_bot(base_url: &str) -> Option<tokio_util::sync::CancellationToken> {
    match trusty_mpm::telegram::resolve_token("TELEGRAM_BOT_TOKEN") {
        Some(token) => {
            tracing::info!("TELEGRAM_BOT_TOKEN found — starting supervised Telegram bot");
            let url = base_url.to_string();
            let shutdown = tokio_util::sync::CancellationToken::new();
            let bot_shutdown = shutdown.clone();
            tokio::spawn(async move {
                let options = trusty_mpm::telegram::BotOptions::default();
                trusty_mpm::telegram::run_supervised(url, Some(token), options, bot_shutdown).await;
            });
            Some(shutdown)
        }
        None => {
            tracing::warn!(
                "TELEGRAM_BOT_TOKEN not set — Telegram bot not started \
                 (set it in .env.local to enable)"
            );
            None
        }
    }
}

/// Build the hard-deprecation error returned when `tm daemon --tailscale` is
/// invoked.
///
/// Why: issue #3330 (part of epic #3328) removed the daemon's secondary
/// Tailscale listener under ADR-0011's loopback-only doctrine — non-loopback
/// ingress is exclusively `trusty-console`'s job. Old invocations (scripts,
/// launchd plists, muscle memory) must fail loudly with a pointer to the
/// replacement, not silently degrade to a loopback-only daemon that looks
/// like it started fine.
/// What: returns an `anyhow::Error` with the actionable message; kept as a
/// separate function so the message text has exactly one source of truth for
/// both the runtime call site and its test.
/// Test: `run_daemon_rejects_tailscale_flag` below.
fn tailscale_deprecated_error() -> anyhow::Error {
    anyhow::anyhow!(
        "tm daemon --tailscale has been removed: the daemon binds loopback \
         only now (ADR-0011 loopback-only doctrine, issue #3330). Use \
         `trusty-console --tailscale` (or its Funnel mode) for non-loopback \
         ingress instead."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#6869): the guard's line is the one place `tm daemon` still spoke a
    /// port to the operator. Pinning the constant is what keeps a later edit
    /// from quietly interpolating a URL back into it.
    /// What: asserts the notice carries no port, no loopback host, no scheme,
    /// and still names the daemon.
    /// Test: this is the test.
    #[test]
    fn already_running_notice_names_no_address() {
        for needle in ["7880", "127.0.0.1", "http://", ":"] {
            assert!(
                !ALREADY_RUNNING_NOTICE.contains(needle),
                "the startup guard must not print {needle:?}: {ALREADY_RUNNING_NOTICE}"
            );
        }
        assert!(
            ALREADY_RUNNING_NOTICE.contains("trusty-mpm daemon is already running"),
            "the operator must still be told what happened: {ALREADY_RUNNING_NOTICE}"
        );
    }

    /// The regression test for the stale lock #6288 slice 1 would otherwise
    /// have introduced.
    ///
    /// Why: `write_lock` used to be the step right after the only bind, so a
    /// written lock always named a daemon holding every listener it needed.
    /// The UDS bind is a second fallible step, and `run_daemon` has no cleanup
    /// path on its error return — so a lock written before that bind would
    /// outlive the aborted process and send the next client to a daemon that
    /// never started.
    /// What: holds the socket with a live listener, so the bind is refused,
    /// then asserts the lock file was never created. Swapping the two
    /// statements in `bind_socket_then_publish` makes this fail.
    /// Test: this function IS the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn bind_socket_then_publish_writes_no_lock_when_the_socket_is_taken() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let socket = tmp.path().join("sockets").join("trusty-mpm.sock");
        let lock_path = tmp.path().join("daemon.lock");

        // An incumbent daemon: a real listener the takeover must refuse.
        let incumbent = trusty_mpm::daemon::socket::bind(&socket)
            .await
            .expect("the incumbent binds a fresh path");

        let err = bind_socket_then_publish(&socket, &lock_path)
            .await
            .expect_err("a bind against a live socket must fail");
        assert!(
            format!("{err:#}").contains(&socket.display().to_string()),
            "the error must name the path: {err:#}"
        );
        assert!(
            !lock_path.exists(),
            "no lock file may name a daemon that failed to start: {}",
            lock_path.display()
        );

        drop(incumbent);
    }

    /// The other half: a successful bind DOES publish, and the record names
    /// the socket and no TCP address (#6288 S3a).
    ///
    /// Why: an ordering test that only proves the failure path would pass if
    /// the write were deleted outright.
    /// Test: this function IS the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn bind_socket_then_publish_writes_the_lock_when_the_bind_succeeds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let socket = tmp.path().join("sockets").join("trusty-mpm.sock");
        let lock_path = tmp.path().join("daemon.lock");

        let rpc = bind_socket_then_publish(&socket, &lock_path)
            .await
            .expect("a fresh socket binds");

        let lock = trusty_mpm::core::daemon_identity::read_lock_at(&lock_path)
            .expect("the lock must be written and readable");
        assert_eq!(lock.addr, "", "#6288 S3a: no TCP address is advertised");
        assert_eq!(lock.socket_path, socket.display().to_string());
        assert_eq!(lock.pid, std::process::id());

        drop(rpc);
    }

    /// #6288 S3a, the Fail-Open Check: a serve error removes this daemon's
    /// lock, so no record outlives a process that exits non-zero.
    /// Test: this function IS the test.
    #[test]
    fn unpublish_if_failed_removes_the_lock_on_a_serve_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let lock_path = tmp.path().join("daemon.lock");
        trusty_mpm::core::daemon_identity::write_lock_at(&lock_path, "/s.sock");

        let err = unpublish_if_failed(Err(anyhow::anyhow!("accept loop died")), &lock_path)
            .expect_err("the serve error passes through");
        assert!(format!("{err:#}").contains("accept loop died"), "{err:#}");
        assert!(!lock_path.exists(), "the lock must not outlive the failure");
    }

    /// The clean stop leaves the lock to the shutdown handler.
    /// Test: this function IS the test.
    #[test]
    fn unpublish_if_failed_keeps_the_lock_on_a_clean_stop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let lock_path = tmp.path().join("daemon.lock");
        trusty_mpm::core::daemon_identity::write_lock_at(&lock_path, "/s.sock");

        unpublish_if_failed(Ok(()), &lock_path).expect("a clean stop is Ok");
        assert!(lock_path.exists());
    }

    /// #6288 S3a (owner ruling Q1): a set `--addr` is named, as ignored, in
    /// one line; an unset one produces none.
    /// Test: this function IS the test.
    #[test]
    fn addr_deprecation_notice_names_the_ignored_value() {
        assert_eq!(addr_deprecation_notice(None), None);
        let line = addr_deprecation_notice(Some("127.0.0.1:7880")).expect("a notice");
        assert!(
            line.contains("deprecated and ignored"),
            "the line says the value is ignored: {line}"
        );
        assert!(line.contains("127.0.0.1:7880"), "names the value: {line}");
        assert!(line.contains("2.0.0"), "names the removal release: {line}");
    }

    /// `run_daemon` must refuse `--tailscale` before doing anything else —
    /// no bind, no chdir, no lock file — so the deprecation error is fast
    /// and side-effect free to test.
    #[tokio::test]
    async fn run_daemon_rejects_tailscale_flag() {
        let err = run_daemon(None, true, false, false, false)
            .await
            .expect_err("--tailscale must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("trusty-console"), "message was: {msg}");
        assert!(msg.contains("ADR-0011"), "message was: {msg}");
        assert!(msg.contains("#3330"), "message was: {msg}");
    }

    /// Issue #4397: the bare-CLI refusal message must name both remedies —
    /// the `launchctl kickstart` recipe and the `--force` opt-in — so an
    /// operator hitting it has an immediate next step either way.
    #[test]
    fn unsupervised_daemon_error_names_remedies() {
        let msg = unsupervised_daemon_error().to_string();
        assert!(msg.contains("launchctl kickstart"), "message was: {msg}");
        assert!(msg.contains("com.trusty.mpm"), "message was: {msg}");
        assert!(msg.contains("--force"), "message was: {msg}");
        assert!(msg.contains("#2486"), "message was: {msg}");
        assert!(msg.contains("#4397"), "message was: {msg}");
    }
}
