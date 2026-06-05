//! Daemon lifecycle command handlers: start, stop, restart, status probes.
//!
//! Why: daemon lifecycle operations (start/stop/restart, PID discovery,
//! signal helpers, status printing) form a coherent group that benefits from
//! its own file. The heavier "boot and serve" side (`run_daemon` and its
//! private helpers) lives in `daemon_run.rs` to keep both files under the
//! 500-line cap.
//! What: `print_status`, `daemon_healthy`, `start`, `restart`, `stop_daemon`,
//! `cleanup_lock_file`, `find_daemon_pids`, `send_signal`, `pid_alive`.
//! Re-exports `run_daemon` from the sibling `daemon_run` module.
//! Test: `cli_parses_daemon_*` parse tests; the bind/serve and spawn/wait
//! paths are exercised by the daemon e2e suite.

#[path = "daemon_run.rs"]
mod daemon_run;
pub(crate) use daemon_run::run_daemon;

use serde::Deserialize;

use crate::formatters::session::short_id;
use crate::types::SessionRow;

/// Print the daemon health line, session listing, and Telegram-bot note.
///
/// Why: `status` and `start` must show identical state; sharing one printer
/// keeps the two outputs from drifting and adds the Telegram note in one place.
/// What: prints `daemon: ok`, one line per session from `GET /sessions`, and
/// `Telegram bot active` when a bot token is resolvable from the environment or
/// a local `.env.local` / `.env` file.
/// Test: covered indirectly by running `tm status` / `tm start` against a live
/// daemon; Telegram token resolution is tested in `trusty-mpm-telegram`.
pub(crate) async fn print_status(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    println!("daemon: ok");

    #[derive(Deserialize)]
    struct Body {
        sessions: Vec<SessionRow>,
    }
    let body: Body = client
        .get(format!("{url}/sessions"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    for s in &body.sessions {
        let status = s.status.as_str().unwrap_or("unknown");
        println!(
            "{} {} {} ({} delegations)",
            short_id(&s.id),
            status,
            s.workdir,
            s.active_delegations
        );
    }

    if trusty_mpm::telegram::resolve_token("TELEGRAM_BOT_TOKEN").is_some() {
        println!("Telegram bot active");
    }
    Ok(())
}

/// Probe the daemon's `/health` endpoint for liveness.
///
/// Why: both `start` (to decide whether to spawn) and the post-spawn wait loop
/// need a single yes/no liveness check; factoring it out keeps the two call
/// sites identical and the intent obvious.
/// What: issues `GET {url}/health`, returning `true` only on a 2xx response and
/// `false` on any transport error or non-success status.
/// Test: covered indirectly by running `tm start` against a live/dead daemon;
/// the logic mirrors the probe already used by `status`.
pub(crate) async fn daemon_healthy(client: &reqwest::Client, url: &str) -> bool {
    // Verify /health is 200 AND /sessions is 200. Code-intelligence on the same
    // port returns 200 for /health but 404 for /sessions, so checking both
    // discriminates our daemon from other HTTP servers on the same port.
    let health_ok = match client.get(format!("{url}/health")).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => return false,
    };
    if !health_ok {
        return false;
    }
    match client.get(format!("{url}/sessions")).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

/// `start` subcommand — ensure the daemon is running, then show status.
///
/// Why: operators want one command that is safe to run repeatedly — it brings
/// the daemon up if it is down and is a no-op (just status) if it is already
/// up, so `tm start` can sit in shell profiles and setup scripts.
/// What: probes `/health`; if healthy, prints "Daemon already running" plus the
/// same listing as `tm status`. If not, opens `~/.trusty-mpm/daemon.log`, spawns
/// `tm daemon` detached with stdout/stderr appended to that log, polls `/health`
/// for up to 5 seconds, then prints "Starting daemon... done" and the status.
/// Test: `cli_parses_start` covers parsing; the spawn/wait path is exercised by
/// running `tm start` against a clean environment.
pub(crate) async fn start(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    // Prefer the lock file URL — it's the address our daemon actually bound to,
    // not whatever default URL the CLI was given (which may point at a different
    // process on the same port, e.g. code-intelligence on :7880).
    let lock_url = trusty_mpm::core::resolve_daemon_url(None);
    let check_url = if lock_url != trusty_mpm::core::DEFAULT_DAEMON_URL
        || url == trusty_mpm::core::DEFAULT_DAEMON_URL
    {
        lock_url.clone()
    } else {
        url.to_string()
    };
    if daemon_healthy(client, &check_url).await {
        println!("Daemon already running on {check_url}");
        return print_status(client, &check_url).await;
    }

    // Resolve the log file under `~/.trusty-mpm/`, creating the dir if absent.
    let root = trusty_mpm::core::paths::FrameworkPaths::default().root;
    std::fs::create_dir_all(&root)?;
    let log_path = root.join("daemon.log");
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let stderr = stdout.try_clone()?;

    // Spawn `tm daemon` detached, pointed at our own binary so the spawned
    // daemon is always the same build as this CLI. We do NOT pass TRUSTY_MPM_ADDR
    // — the daemon picks its own port (falling back to ephemeral if needed) and
    // records the actual address in the lock file. We discover it from there.
    let lock_path = trusty_mpm::core::lock_file_path();
    // Remove any stale lock file before spawning so we can detect the new write.
    let _ = std::fs::remove_file(&lock_path);
    let exe = std::env::current_exe()?;
    std::process::Command::new(&exe)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(stderr))
        .spawn()?;

    // Poll the lock file for up to 5 seconds. The daemon writes it as soon as
    // it has a bound address — use that URL for the health check.
    print!("Starting daemon... ");
    use std::io::Write as _;
    std::io::stdout().flush().ok();
    let mut healthy = false;
    // Use the lock-file URL if available, fall back to the cli url.
    let mut actual_url = url.to_string();
    for _ in 0..10 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        // Re-resolve: once the lock file appears it wins over the default.
        actual_url = trusty_mpm::core::resolve_daemon_url(None);
        if daemon_healthy(client, &actual_url).await {
            healthy = true;
            break;
        }
    }
    if healthy {
        println!("done");
        if actual_url != url {
            println!("(listening on {actual_url})");
        }
    } else {
        println!("failed");
        println!(
            "daemon did not become healthy within 5s; see {}",
            log_path.display()
        );
        return Ok(());
    }

    print_status(client, &actual_url).await
}

/// `restart` subcommand — stop the running daemon, then start a new one.
///
/// Why: a restart cycle is needed after config changes; running `stop` then
/// `start` manually has a gap where the daemon is unreachable.
/// What: if the daemon is healthy, sends SIGTERM (via pkill) and waits up to
/// 3 s for the port to free, then calls `start`.
/// Test: `cli_parses_start` covers parsing; the spawn/wait path is exercised by
/// running `tm start` against a clean environment.
pub(crate) async fn restart(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    if daemon_healthy(client, url).await {
        print!("Stopping daemon... ");
        use std::io::Write as _;
        std::io::stdout().flush().ok();
        // Kill any running tm/trusty-mpm daemon processes.
        std::process::Command::new("pkill")
            .args(["-f", "tm daemon"])
            .status()
            .ok();
        std::process::Command::new("pkill")
            .args(["-f", "trusty-mpm daemon"])
            .status()
            .ok();
        // Wait until the port is free (up to 3 s).
        for _ in 0..6 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if !daemon_healthy(client, url).await {
                break;
            }
        }
        println!("done");
    }
    start(client, url).await
}

/// `stop` subcommand — terminate every running trusty-mpm daemon.
///
/// Why: pairs with `start` so operators can shut the daemon down without a
/// full `restart` cycle. Mirrors `trusty-search stop` and `trusty-memory
/// stop` so the three daemons share a single mental model. The daemon
/// writes its address into `~/.trusty-mpm/daemon.lock`, but the lock-file
/// PID can go stale (a SIGKILL leaves it behind), so the source of truth
/// is the live process table.
/// What: walks the process table via `sysinfo` for every `trusty-mpm` or
/// `tm` process whose argv contains `daemon`, sends SIGTERM, polls 5 s for
/// them to exit, then SIGKILLs stragglers. Removes the stale lock file
/// when every targeted process has exited.
/// Test: `cli_parses_stop`; the spawn/kill path is exercised by running
/// `tm start` followed by `tm stop` against a clean environment.
pub(crate) async fn stop_daemon() -> anyhow::Result<()> {
    use std::time::{Duration, Instant};

    let targets = find_daemon_pids();
    if targets.is_empty() {
        anyhow::bail!("No daemon running");
    }

    println!(
        "Stopping trusty-mpm daemon ({} process(es): {:?})…",
        targets.len(),
        targets
    );

    // Phase 1: SIGTERM all targets.
    for pid in &targets {
        let _ = send_signal(*pid, "TERM");
    }

    // Phase 2: poll up to 5 s for every targeted PID to exit.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let any_alive = targets.iter().any(|p| pid_alive(*p));
        if !any_alive {
            println!("Daemon stopped");
            cleanup_lock_file();
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
    }

    // Phase 3: SIGKILL anything still alive.
    let stragglers: Vec<u32> = targets.iter().copied().filter(|p| pid_alive(*p)).collect();
    if !stragglers.is_empty() {
        println!(
            "{} process(es) ignored SIGTERM — sending SIGKILL: {:?}",
            stragglers.len(),
            stragglers
        );
        for pid in &stragglers {
            let _ = send_signal(*pid, "KILL");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    if targets.iter().any(|p| pid_alive(*p)) {
        println!("Daemon may still be shutting down");
    } else {
        println!("Daemon stopped");
        cleanup_lock_file();
    }
    Ok(())
}

/// Remove the stale `~/.trusty-mpm/daemon.lock` after a successful stop.
///
/// Why: the daemon writes its lock file on bind and removes it on graceful
/// shutdown, but SIGKILL leaves it behind; the next `tm status` call would
/// then chase a dead address through the discovery timeout.
/// What: best-effort `fs::remove_file` of `lock_file_path()`.
/// Test: covered indirectly by the stop integration path.
pub(crate) fn cleanup_lock_file() {
    let path = trusty_mpm::core::lock_file_path();
    let _ = std::fs::remove_file(&path);
}

/// Walk the process table and return every trusty-mpm daemon PID.
///
/// Why: `tm stop` needs to find the daemon regardless of which binary alias
/// (`trusty-mpm` or `tm`) was used to launch it. Matching argv on `daemon`
/// filters out short-lived CLI invocations (`tm status`, `tm doctor`) whose
/// process names also match.
/// What: refreshes the process list once, matches `name() in {trusty-mpm,
/// tm}` AND `cmd().contains("daemon")`. Excludes the current process.
/// Test: covered indirectly by the stop integration path.
pub(crate) fn find_daemon_pids() -> Vec<u32> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    );
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let me = std::process::id();
    let mut out = Vec::new();
    for (pid, proc_) in sys.processes() {
        let raw = pid.as_u32();
        if raw == me {
            continue;
        }
        let name = proc_.name().to_string_lossy();
        let is_tm_binary = name == "trusty-mpm" || name == "tm";
        if !is_tm_binary {
            continue;
        }
        let is_daemon = proc_.cmd().iter().any(|a| a.to_string_lossy() == "daemon");
        if is_daemon {
            out.push(raw);
        }
    }
    out
}

/// Send a POSIX signal to a PID by shelling out to `/bin/kill`.
///
/// Why: avoid adding a `nix` dependency for the sole purpose of sending
/// SIGTERM / SIGKILL. `kill -SIGNAL pid` is universally available on
/// every Unix the daemon supports (macOS, Linux).
/// What: spawns `kill -<sig> <pid>` and returns an error if the exit
/// status is non-zero.
/// Test: covered indirectly by the stop integration path.
#[cfg(unix)]
pub(crate) fn send_signal(pid: u32, sig: &str) -> std::io::Result<()> {
    let status = std::process::Command::new("kill")
        .arg(format!("-{sig}"))
        .arg(pid.to_string())
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "kill -{sig} {pid} exited {status}"
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn send_signal(_pid: u32, _sig: &str) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "signals unsupported on this platform",
    ))
}

/// Check whether a PID is still alive (Unix only).
///
/// Why: the SIGTERM-then-SIGKILL poll loop needs a portable "is this PID
/// alive?" probe. `kill -0` returns success when the process exists.
/// What: invokes `kill -0 <pid>` via `Command` so no extra dep is needed.
/// Test: covered indirectly by the stop integration path.
#[cfg(unix)]
pub(crate) fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub(crate) fn pid_alive(_pid: u32) -> bool {
    true
}
