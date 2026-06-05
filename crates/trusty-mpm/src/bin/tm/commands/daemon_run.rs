//! Daemon HTTP-server entry point and its private helpers.
//!
//! Why: `run_daemon` and its three private helpers (`wait_for_shutdown_signal`,
//! `spawn_telegram_bot`, `get_tailscale_ip`) are a coherent "boot and serve"
//! group; extracting them keeps `daemon.rs` under the 500-line cap while
//! co-locating the helpers that are only meaningful together.
//! What: `run_daemon` (the `daemon` subcommand handler) plus private helpers.
//! Test: `cli_parses_daemon_*` parse tests; the bind/serve path is exercised
//! by the daemon e2e suite.

use std::net::SocketAddr;

/// `daemon` subcommand — run the HTTP daemon (or MCP server) with auto port
/// selection, lock-file service discovery, and optional Tailscale exposure.
///
/// Why: the daemon must start even when the configured port is busy (auto
/// fallback to an ephemeral port), publish its real address so clients can
/// find it (lock file), and optionally be reachable over Tailscale.
/// What: in MCP mode delegates straight to `run_mcp`; otherwise binds `addr`
/// (falling back to `127.0.0.1:0` on `AddrInUse`), optionally binds a second
/// Tailscale listener, writes the lock file, registers a Ctrl-C handler that
/// removes the lock, then serves the API on the primary listener.
/// Test: `cli_parses_daemon_*` cover flag parsing; the bind/serve path is
/// exercised by the daemon e2e suite.
pub(crate) async fn run_daemon(addr: SocketAddr, tailscale: bool, mcp: bool) -> anyhow::Result<()> {
    use std::io::ErrorKind;

    let state = trusty_mpm::daemon::DaemonState::shared();
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
    let recorded_url = trusty_mpm::core::resolve_daemon_url(None);
    if recorded_url != trusty_mpm::core::DEFAULT_DAEMON_URL
        && trusty_common::probe_health(&recorded_url, "/health").await
    {
        eprintln!("trusty-mpm daemon is already running at {recorded_url}");
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

    // Optional Tailscale second listener.
    let tailscale_url = if tailscale {
        match get_tailscale_ip() {
            Some(ts_ip) => {
                let ts_addr = format!("{ts_ip}:{}", actual_addr.port());
                match tokio::net::TcpListener::bind(&ts_addr).await {
                    Ok(ts_listener) => {
                        let ts_url = format!("http://{ts_addr}");
                        tracing::info!("Tailscale listener on {ts_url}");
                        // Spawn a second server sharing daemon state.
                        trusty_mpm::daemon::spawn_secondary_listener(
                            trusty_mpm::daemon::DaemonState::shared(),
                            ts_listener,
                        );
                        Some(ts_url)
                    }
                    Err(e) => {
                        tracing::warn!("failed to bind Tailscale address {ts_addr}: {e}");
                        None
                    }
                }
            }
            None => {
                tracing::warn!("--tailscale requested but no Tailscale IP found");
                None
            }
        }
    } else {
        None
    };

    // Write lock file so clients can discover us.
    trusty_mpm::daemon::lock::write_lock(&base_url, tailscale_url.as_deref());

    // Clean up the lock file on shutdown for BOTH Ctrl-C (SIGINT) and SIGTERM.
    // `tm restart` stops the old daemon with `pkill`, which sends SIGTERM — if we
    // only trapped SIGINT the lock file would leak with a dead PID, and the next
    // client's `resolve_daemon_url` would fall back to the default port (often
    // occupied by an unrelated process) and report "daemon unreachable".
    tokio::spawn(async {
        wait_for_shutdown_signal().await;
        trusty_mpm::daemon::lock::remove_lock();
        std::process::exit(0);
    });

    // Auto-start the Telegram bot alongside the daemon when a bot token is
    // configured. `resolve_token` honours `.env.local` → `.env` → the process
    // environment, so a single dotenv file configures both the daemon and the
    // bot. Without a token the daemon runs normally; only a warning is logged.
    spawn_telegram_bot(&base_url);

    trusty_mpm::daemon::serve_http(state, listener).await
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

/// Spawn the Telegram bot as a background task when a token is configured.
///
/// Why: an operator who has set `TELEGRAM_BOT_TOKEN` expects the bot to come up
/// with the daemon — not as a separate process they must remember to start.
/// What: resolves the token via `trusty_mpm::telegram::resolve_token` (which
/// reads `.env.local`, then `.env`, then the environment). When a token is
/// found the bot's `run` is spawned on a tokio task pointed at `base_url`; when
/// absent a single warning is logged and the daemon continues.
/// Test: token resolution is covered by `trusty-mpm-telegram`'s
/// `resolve_token_*` tests; the spawn path is exercised by running the daemon.
fn spawn_telegram_bot(base_url: &str) {
    match trusty_mpm::telegram::resolve_token("TELEGRAM_BOT_TOKEN") {
        Some(token) => {
            tracing::info!("TELEGRAM_BOT_TOKEN found — starting Telegram bot");
            let url = base_url.to_string();
            tokio::spawn(async move {
                let options = trusty_mpm::telegram::BotOptions::default();
                if let Err(e) = trusty_mpm::telegram::run(url, Some(token), false, options).await {
                    tracing::warn!("Telegram bot exited: {e}");
                }
            });
        }
        None => {
            tracing::warn!(
                "TELEGRAM_BOT_TOKEN not set — Telegram bot not started \
                 (set it in .env.local to enable)"
            );
        }
    }
}

/// Detect the Tailscale IPv4 address by running `tailscale ip -4`.
///
/// Why: Tailscale's IP changes per device; we can't hardcode it. The CLI
/// is the most reliable cross-platform way to query it without adding a
/// Tailscale SDK dependency.
/// What: Spawns `tailscale ip -4`, trims the output, returns it if it
/// looks like an IP address. Returns `None` on any error or if Tailscale
/// is not installed.
/// Test: Hard to test without Tailscale installed; the unit test below
/// checks the happy-path string parsing.
fn get_tailscale_ip() -> Option<String> {
    let output = std::process::Command::new("tailscale")
        .args(["ip", "-4"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let ip = String::from_utf8(output.stdout).ok()?;
    let ip = ip.trim().to_string();
    // Basic sanity check: must look like an IPv4 or Tailscale CGNAT address.
    if ip.contains('.') && !ip.is_empty() {
        Some(ip)
    } else {
        None
    }
}
