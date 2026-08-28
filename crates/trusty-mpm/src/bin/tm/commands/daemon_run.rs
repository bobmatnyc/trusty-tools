//! Daemon HTTP-server entry point and its private helpers.
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
//! launchd/filesystem state, so it is not hermetically fakeable here — see
//! the daemon e2e suite for the wired-up behaviour); the bind/serve path is
//! likewise exercised by the daemon e2e suite.

use std::net::SocketAddr;

/// `daemon` subcommand — run the HTTP daemon (or MCP server) with auto port
/// selection and lock-file service discovery.
///
/// Why: the daemon must start even when the configured port is busy (auto
/// fallback to an ephemeral port) and publish its real address so clients can
/// find it (lock file). Per ADR-0011's loopback-only doctrine (issue #3330,
/// part of epic #3328), the daemon itself never exposes a non-loopback
/// listener — `trusty-console` is the sole HTTP surface reachable off-host,
/// via its own `--tailscale`/Funnel modes.
/// What: rejects `--tailscale` up front with an actionable error (see
/// [`tailscale_deprecated_error`]); refuses to start when a launchd unit is
/// registered but this process isn't launchd-supervised and `--force` was
/// not passed (issue #4397, see [`apply_supervision_signal`] and
/// [`unsupervised_daemon_error`]); in MCP mode delegates straight to
/// `run_mcp`; otherwise binds `addr` (falling back to `127.0.0.1:0` on
/// `AddrInUse`), writes the lock file, registers a Ctrl-C handler that
/// removes the lock, then serves the API on the primary (loopback) listener.
/// Test: `cli_parses_daemon_*` cover flag parsing; `run_daemon_rejects_
/// tailscale_flag` covers the deprecation error; the #4397 guard's decision
/// table is covered hermetically by `commands::launchd_probe::tests::
/// compute_daemon_refuse_*`; the bind/serve path is exercised by the daemon
/// e2e suite.
pub(crate) async fn run_daemon(
    addr: SocketAddr,
    tailscale: bool,
    mcp: bool,
    force: bool,
) -> anyhow::Result<()> {
    use std::io::ErrorKind;

    // ADR-0011 loopback-only doctrine (#3330): the secondary Tailscale
    // listener has been removed. Fail loudly and immediately rather than
    // silently ignoring the flag — old invocations (scripts, launchd plists,
    // muscle memory) need a clear signal, not a quietly-degraded daemon.
    if tailscale {
        return Err(tailscale_deprecated_error());
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

    let state = trusty_mpm::daemon::DaemonState::shared();

    // #2486: compute the launchd-supervision signal once, up front, so both
    // the MCP-mode early return and the HTTP-serving path below carry it on
    // `GET /health`. See `crate::commands::launchd_probe` for the full
    // restart-race rationale and the pure decision table.
    let supervised = apply_supervision_signal(&state);

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

    // Refuse to start a second instance: read the lock-file address, probe
    // `/health`, and bail out cleanly when an existing daemon answers. Without
    // this guard, the `AddrInUse` fallback below would auto-pick an ephemeral
    // port and silently spawn a duplicate daemon that splits traffic with the
    // original. `resolve_daemon_url` already validates the recorded PID is
    // alive (and clears stale lock files), so a `None`-ish result here means
    // either no lock exists or the recorded daemon is dead — proceed normally.
    //
    // INTENTIONALLY kept on the sync direct-lock resolver (#1849 Phase 2): the
    // daemon is starting up here; the console gateway isn't relevant — and its
    // async probe would require bootstrapping a reqwest::Client before the bind
    // address is even known. Bootstrap circularity means only the lock file
    // is authoritative at this point.
    let recorded_url = trusty_mpm::core::resolve_daemon_url(None);
    if recorded_url != trusty_mpm::core::DEFAULT_DAEMON_URL
        && trusty_common::probe_health(&recorded_url, "/health").await
    {
        eprintln!("trusty-mpm daemon is already running at {recorded_url}");
        return Ok(());
    }

    // #1731: the lock file now has to carry the trusty-mpm product magic to be
    // trusted, so a daemon started by a pre-#1731 binary is invisible to the
    // check above and the `AddrInUse` fallback below would quietly spawn a
    // duplicate on an ephemeral port. Ask the address this invocation intends
    // to bind whether a daemon already answers there. Scoped to `addr`, not the
    // default, so `--addr` still starts a second daemon on a free port.
    let intended_url = format!("http://{addr}");
    if trusty_common::probe_health(&intended_url, "/health").await {
        eprintln!("trusty-mpm daemon is already running at {intended_url}");
        return Ok(());
    }

    // Auto port selection: try configured address; fall back to ephemeral.
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == ErrorKind::AddrInUse => {
            tracing::warn!("port {} is busy — selecting an ephemeral port", addr.port());
            tokio::net::TcpListener::bind("127.0.0.1:0").await?
        }
        Err(e) => return Err(e.into()),
    };
    let actual_addr = listener.local_addr()?;
    let base_url = format!("http://{actual_addr}");
    tracing::info!("daemon listening on {base_url}");

    // #6288: bind the RPC socket and only then write the lock file, which is
    // what clients discover us by (backward-compat for existing clients that
    // parse the TOML lock directly, e.g. the old MpmConnector). See
    // [`bind_socket_then_publish`] for why the order is the point.
    let socket_path = trusty_mpm::daemon::socket::socket_path()?;
    let rpc = bind_socket_then_publish(
        &base_url,
        &socket_path,
        &trusty_mpm::core::discovery::lock_file_path(),
    )
    .await?;
    // Also write the standard trusty-common http_addr file so the console's
    // reverse proxy can discover the daemon address via the shared
    // `read_daemon_addr` helper (#1849 Phase 1).
    // Convention: store bare `host:port` (no http:// scheme) so that
    // `detect_service` in trusty-console can TCP-probe it directly.
    if let Err(e) = trusty_common::write_daemon_addr("trusty-mpm", &actual_addr.to_string()) {
        tracing::warn!("failed to write trusty-mpm http_addr discovery file: {e:#}");
    }

    // Auto-start the Telegram bot alongside the daemon when a bot token is
    // configured. `resolve_token` honours `.env.local` → `.env` → the process
    // environment, so a single dotenv file configures both the daemon and the
    // bot. Without a token the daemon runs normally; only a warning is logged.
    // The returned token lets the shutdown handler stop the supervised bot
    // promptly so it never blocks (or outlives) graceful shutdown (#1499).
    let bot_shutdown = spawn_telegram_bot(&base_url);

    // Clean up the lock file on shutdown for BOTH Ctrl-C (SIGINT) and SIGTERM.
    // `tm restart` stops the old daemon with `pkill`, which sends SIGTERM — if we
    // only trapped SIGINT the lock file would leak with a dead PID, and the next
    // client's `resolve_daemon_url` would fall back to the default port (often
    // occupied by an unrelated process) and report "daemon unreachable".
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        // Stop the supervised Telegram bot first so it does not log a spurious
        // restart while the process is on its way down.
        if let Some(token) = bot_shutdown {
            token.cancel();
        }
        trusty_mpm::daemon::lock::remove_lock();
        // Remove the standard http_addr file so the console stops advertising
        // this (now-dead) daemon address (#1849 Phase 1).
        if let Err(e) = trusty_common::remove_daemon_addr("trusty-mpm") {
            tracing::warn!("failed to remove trusty-mpm http_addr discovery file: {e:#}");
        }
        // See #6288: this exit almost certainly wins the race against
        // `serve_with_shutdown`'s graceful drain, so neither the session reap
        // (#1455) nor the socket unlink is guaranteed to run on a real SIGTERM.
        // The socket is self-healing — `daemon::socket::bind` reclaims a file
        // nobody is serving — so slice 1 leaves the race alone; slice 7, which
        // moves the clients onto the socket, is where it gets fixed.
        std::process::exit(0);
    });

    trusty_mpm::daemon::serve_http(state, listener, rpc).await
}

/// Bind the RPC socket, then publish the daemon's identity — in that order.
///
/// Why the order is the whole function (#6288): the lock file names a daemon
/// that is about to serve, and `run_daemon` has no cleanup path on its error
/// return. Before this slice there was one bind and it happened first, so a
/// written lock always named a daemon that had taken every listener it needed.
/// The socket bind is a second, fallible step; writing the lock between the two
/// would leave a record naming a process that then aborts, and the next client
/// would resolve a daemon URL nothing answers.
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
    base_url: &str,
    socket: &std::path::Path,
    lock_path: &std::path::Path,
) -> anyhow::Result<trusty_mpm::daemon::socket::BoundSocket> {
    let rpc = trusty_mpm::daemon::socket::bind(socket).await?;
    trusty_mpm::core::daemon_identity::write_lock_at(
        lock_path,
        base_url,
        &socket.display().to_string(),
    );
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

        let err = bind_socket_then_publish("http://127.0.0.1:7880", &socket, &lock_path)
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

    /// The other half: a successful bind DOES publish, and the record carries
    /// both endpoints.
    ///
    /// Why: an ordering test that only proves the failure path would pass if
    /// the write were deleted outright.
    /// Test: this function IS the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn bind_socket_then_publish_writes_the_lock_when_the_bind_succeeds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let socket = tmp.path().join("sockets").join("trusty-mpm.sock");
        let lock_path = tmp.path().join("daemon.lock");

        let rpc = bind_socket_then_publish("http://127.0.0.1:7880", &socket, &lock_path)
            .await
            .expect("a fresh socket binds");

        let lock = trusty_mpm::core::daemon_identity::read_lock_at(&lock_path)
            .expect("the lock must be written and readable");
        assert_eq!(lock.addr, "http://127.0.0.1:7880");
        assert_eq!(lock.socket_path, socket.display().to_string());
        assert_eq!(lock.pid, std::process::id());

        drop(rpc);
    }

    /// `run_daemon` must refuse `--tailscale` before doing anything else —
    /// no bind, no chdir, no lock file — so the deprecation error is fast
    /// and side-effect free to test.
    #[tokio::test]
    async fn run_daemon_rejects_tailscale_flag() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let err = run_daemon(addr, true, false, false)
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
