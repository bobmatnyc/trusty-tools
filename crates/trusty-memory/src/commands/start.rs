//! Handler for `trusty-memory start` — boots the HTTP daemon in the background.
//!
//! Why: the three trusty-* daemons (search, memory, mpm) historically had
//! diverging `start` / `serve` / `stop` semantics. `trusty-memory start`
//! mirrors `trusty-search start`: it spawns a detached `serve --foreground`.
//! A second `start` while the daemon is already healthy is a no-op (prints the
//! live address and exits 0) rather than racing a second instance against the
//! dynamic port walker.
//! What: probes `read_daemon_addr("trusty-memory")` plus an HTTP `/health`
//! check, and on cold start goes through the shared single-flight start
//! (#5267) so a concurrent `serve --stdio` bridge cannot start a second daemon.
//! Test: `spawn_args_never_bind_port_zero`,
//! `start_lock_lives_beside_the_addr_file`; cross-process exclusion in
//! `crates/trusty-common/tests/single_flight_exclusion.rs`.

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

/// Path to the exclusive lock that serialises daemon starts (#5267).
///
/// Why: `handle_start` and the `serve --stdio` bridge both start the daemon.
/// They must contend for the SAME lock file or the exclusion is only within each
/// path and a `start` racing a bridge still yields two daemons — #1152's failure
/// mode. One derivation, used by both, is what makes that impossible.
/// What: returns `{resolve_data_dir("trusty-memory")}/start.lock`, alongside the
/// `http_addr` file so both live under the same (test-overridable) data dir.
/// Returns `None` when the data dir cannot be resolved.
/// Test: `start_lock_lives_beside_the_addr_file`. The exclusion this path
/// depends on is proven cross-crate in
/// crates/trusty-common/tests/single_flight_exclusion.rs.
pub(crate) fn start_lock_path() -> Option<std::path::PathBuf> {
    trusty_common::resolve_data_dir("trusty-memory")
        .ok()
        .map(|d| d.join("start.lock"))
}

/// Build the `DaemonBridgeConfig` describing how to start the canonical daemon.
///
/// Why (#1152, #5267): the daemon must be started ONE way, from one place. The
/// spawn args are `serve --foreground` with **no `--http`** — that is
/// load-bearing. #1152's outage came from `--http 127.0.0.1:0`, whose
/// OS-assigned random port always differed from the canonical one and always
/// overwrote `http_addr`, so each spawned daemon stole address ownership from
/// the launchd daemon and squatted redb's write lock. Bare `serve --foreground`
/// takes the dynamic walker starting at the canonical 7070 instead.
/// What: returns the config shared by `handle_start` and the stdio bridge.
/// Test: `spawn_args_never_bind_port_zero`.
pub(crate) fn daemon_start_config() -> trusty_common::mcp::DaemonBridgeConfig {
    trusty_common::mcp::DaemonBridgeConfig {
        service_name: "trusty-memory".to_string(),
        // NEVER `--http 127.0.0.1:0` — see the doc comment above (#1152).
        spawn_args: vec!["serve".to_string(), "--foreground".to_string()],
        health_path: "/health".to_string(),
        base_url_fn: Box::new(crate::commands::daemon_guard::daemon_base_url),
        startup_timeout: None,
        poll_interval: None,
        // #5267: the single-flight path supplies the exclusion `no_spawn` was
        // standing in for, so starting is safe again.
        no_spawn: false,
        no_spawn_hint: None,
    }
}

/// Boot the trusty-memory HTTP daemon in the background.
///
/// Why: the daemon must outlive the invoking shell, so it runs detached rather
/// than tied to the controlling terminal (which broke shell profiles, tmux
/// panes, and `make`-driven dev loops). Since #5267 this also waits for the
/// daemon to answer `/health` before returning: the previous fire-and-forget
/// return released the start lock before the port was bound, which let the next
/// contender re-probe a dead address and start a second daemon (#1152).
/// What: if `trusty_common::check_already_running` reports a healthy daemon,
/// prints its URL and returns without taking the lock. Otherwise delegates to
/// [`trusty_common::mcp::ensure_daemon_up_single_flight`], which starts
/// `serve --foreground` at most once across all processes and waits for it to
/// become ready. Fails closed if it never does.
/// Test: `spawn_args_never_bind_port_zero`,
/// `start_config_relies_on_the_single_flight_lock`; end-to-end exclusion in
/// `crates/trusty-common/tests/single_flight_exclusion.rs`.
pub async fn handle_start() -> Result<()> {
    if let Some(path) = addr_file_path() {
        if let Some(url) = trusty_common::check_already_running(&path, "/health").await {
            eprintln!("{} trusty-memory is already running at {url}", "◉".green());
            return Ok(());
        }
    }

    // #5267: start under the shared single-flight lock so a concurrent `serve
    // --stdio` bridge cannot start a second daemon. Waiting for readiness here
    // (rather than returning at spawn time, as this used to) is what keeps the
    // exclusion honest — releasing the lock before the daemon binds would let
    // the next contender re-probe a not-yet-listening port and start another.
    let lock_path = start_lock_path()
        .ok_or_else(|| anyhow::anyhow!("could not resolve the trusty-memory data directory"))?;
    let config = daemon_start_config();
    let url = trusty_common::mcp::ensure_daemon_up_single_flight(&config, &lock_path).await?;
    eprintln!("{} trusty-memory daemon ready at {url}", "✓".green());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `--http 127.0.0.1:0` is the exact spawn shape that caused #1152. An
    /// OS-assigned random port always differs from the canonical one and always
    /// overwrites `http_addr`, so each spawned daemon stole address ownership
    /// from the launchd daemon and squatted redb's write lock. This asserts the
    /// shape can never come back by accident.
    /// What: inspects the shared start config's spawn args.
    /// Test: itself.
    #[test]
    fn spawn_args_never_bind_port_zero() {
        let config = daemon_start_config();
        let joined = config.spawn_args.join(" ");
        assert!(
            !joined.contains(":0"),
            "spawn args must never bind port 0 (#1152); got: {joined}"
        );
        assert!(
            !config.spawn_args.iter().any(|a| a == "--http"),
            "the canonical daemon takes the dynamic walker from 7070, not an \
             explicit --http (#1152); got: {joined}"
        );
        assert_eq!(
            config.spawn_args,
            vec!["serve".to_string(), "--foreground".to_string()],
            "the canonical daemon command must stay `serve --foreground`"
        );
    }

    /// Why: `handle_start` and the stdio bridge must contend for the SAME lock
    /// file. If they derived different paths the exclusion would hold only
    /// within each path, and a `start` racing a bridge would still yield two
    /// daemons — #1152's failure mode through a side door.
    /// What: asserts the lock path is derived from the data dir and sits beside
    /// `http_addr`, so both callers resolve it identically.
    /// Test: itself.
    #[test]
    fn start_lock_lives_beside_the_addr_file() {
        let (Some(lock), Some(addr)) = (start_lock_path(), addr_file_path()) else {
            return; // No resolvable data dir in this environment.
        };
        assert_eq!(
            lock.parent(),
            addr.parent(),
            "the start lock must live in the same data dir as http_addr"
        );
        assert_eq!(lock.file_name().unwrap(), "start.lock");
    }

    /// Why: the bridge must never be able to spawn without exclusion. `no_spawn`
    /// is off now, so the lock is the ONLY thing preventing #1152 — this pins
    /// the pairing so a future edit cannot turn spawning back on without it.
    /// Test: itself.
    #[test]
    fn start_config_relies_on_the_single_flight_lock() {
        let config = daemon_start_config();
        assert!(
            !config.no_spawn,
            "start-if-not-running requires spawning to be permitted (#5267)"
        );
        assert!(
            start_lock_path().is_some(),
            "a spawning config must have a lock path to serialise on"
        );
    }
}
