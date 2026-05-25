//! Handler for `trusty-memory start` — boots the HTTP daemon in the background.
//!
//! Why: the three trusty-* daemons (search, memory, mpm) historically had
//! diverging `start` / `serve` / `stop` semantics. As of this change,
//! `trusty-memory start` mirrors `trusty-search start`: it self-spawns a
//! detached copy of the binary with `serve --foreground` so the parent shell
//! gets its prompt back immediately. A second `start` invocation while the
//! daemon is already healthy is a no-op (prints the live address and exits 0)
//! rather than racing a second instance against the dynamic port walker.
//! What: a thin async handler that probes `read_daemon_addr("trusty-memory")`
//! plus an HTTP `/health` check, and on cold start spawns `<this exe> serve
//! --foreground` with stdio piped to `/dev/null`.
//! Test: `cargo run -p trusty-memory -- start` returns immediately; running
//! it twice in a row reports "already running" on the second invocation.

use anyhow::Result;
use colored::Colorize;

/// Path to the trusty-memory daemon's address-discovery file.
///
/// Why: both `handle_start` and the background-mode branch of `serve` need to
/// probe the same file before binding a port, so the path is centralized here
/// rather than re-derived at each call site. Returns `None` when the data dir
/// cannot be resolved (e.g. no $HOME / TRUSTY_DATA_DIR_OVERRIDE) so the
/// fallback path lets normal startup proceed.
/// What: returns `{resolve_data_dir("trusty-memory")}/http_addr`.
/// Test: covered indirectly by the start integration path.
pub(crate) fn addr_file_path() -> Option<std::path::PathBuf> {
    trusty_common::resolve_data_dir("trusty-memory")
        .ok()
        .map(|d| d.join("http_addr"))
}

/// Boot the trusty-memory HTTP daemon in the background.
///
/// Why: a CLI invocation must return control to the shell as soon as the
/// daemon is healthy. Blocking on `run_http_dynamic` (as `serve` historically
/// did) tied the daemon's lifetime to the controlling terminal, which broke
/// shell profiles, tmux panes, and any `make`-driven dev loop. Detaching via
/// self-spawn fixes the lifetime AND keeps the surface identical to
/// `trusty-search start`. The duplicate-instance guard
/// (`check_already_running`) is the load-bearing piece: without it a second
/// invocation would silently spawn a second daemon on the next free port.
/// What: if `trusty_common::check_already_running` reports a healthy daemon,
/// prints its URL and returns. Otherwise spawns `<this exe> serve
/// --foreground` with stdio redirected to `/dev/null` so the child inherits no
/// terminal, and prints the spawned PID. Does NOT wait for the child to finish
/// binding — callers that need health confirmation should run `trusty-memory
/// doctor` next.
/// Test: `cargo run -p trusty-memory -- start` returns within a second; running
/// it twice in a row reports "already running" on the second invocation.
pub async fn handle_start() -> Result<()> {
    if let Some(path) = addr_file_path() {
        if let Some(url) = trusty_common::check_already_running(&path, "/health").await {
            eprintln!(
                "{} trusty-memory is already running at {url}",
                "◉".green()
            );
            return Ok(());
        }
    }

    let exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("could not resolve current_exe: {e}"))?;
    let child = std::process::Command::new(&exe)
        .arg("serve")
        .arg("--foreground")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not spawn detached daemon: {e}"))?;
    let pid = child.id();
    eprintln!(
        "{} Daemon starting in background (pid {pid}). Run `trusty-memory doctor` to verify readiness.",
        "✓".green()
    );
    Ok(())
}
