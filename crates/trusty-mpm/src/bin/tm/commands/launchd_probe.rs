//! Launchd-unit probing shared by daemon startup and the MCP stdio bridge (#2486).
//!
//! Why: during the documented `launchctl bootout → cargo install → launchctl
//! bootstrap` restart, trusty-console's auto-reconnect can race the restart —
//! it spawns a fresh `tm serve --stdio` bridge child whose startup auto-spawns
//! an ORPHAN daemon before launchd has a chance to relaunch the supervised one.
//! The orphan answers `/health` 200 (so a green health check is NOT sufficient
//! evidence launchd won) but runs without the plist's `EnvironmentVariables`
//! (`TELEGRAM_BOT_TOKEN`, `OPENROUTER_API_KEY`) and with `cwd=$HOME`, silently
//! breaking features like the Telegram poller. Both the daemon (to report
//! `supervised` on `/health`) and the stdio bridge (to decide whether
//! auto-spawn is safe) need the same "does a trusty-mpm launchd unit exist"
//! signal, so it is centralised here instead of duplicated.
//!
//! What: [`mpm_launchd_plist_exists`] probes the two plist paths a trusty-mpm
//! launchd unit could be registered under. [`compute_supervised`] and
//! [`compute_no_spawn`] are pure functions over that boolean (plus, for
//! supervision, [`trusty_common::update::is_launchd_supervised`]) so the
//! decision logic is unit-testable without touching the filesystem or launchd.
//!
//! Test: `compute_supervised_*` and `compute_no_spawn_*` below exercise every
//! combination of the pure decision tables. `mpm_launchd_plist_exists` itself
//! is a thin `Path::exists` probe and is not unit-tested (no hermetic way to
//! fake `$HOME/Library/LaunchAgents` without real filesystem I/O).

/// Candidate launchd plist paths for a trusty-mpm daemon unit.
///
/// Why: the supervisor installer (`trusty-installer`) uses the label
/// `com.trusty.mpm.supervisor`, but operators may also run a plain
/// `com.trusty.mpm` unit; there is no single canonical in-repo daemon plist,
/// so both are treated as "a trusty-mpm launchd unit exists".
/// What: returns the two absolute paths under `~/Library/LaunchAgents/`,
/// falling back to an empty list when `$HOME` cannot be resolved (e.g. a
/// stripped-down CI sandbox) — treated the same as "no plist" by callers.
/// Test: covered indirectly via `mpm_launchd_plist_exists`.
fn launchd_plist_paths() -> Vec<std::path::PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let agents = home.join("Library/LaunchAgents");
    vec![
        agents.join("com.trusty.mpm.plist"),
        agents.join("com.trusty.mpm.supervisor.plist"),
    ]
}

/// Returns `true` when a trusty-mpm launchd unit is registered on this host.
///
/// Why: the presence of a plist is the operator's declared intent that
/// launchd should own the daemon's lifecycle; both the health-supervision
/// signal and the bridge's no-spawn decision key off this fact.
/// What: `true` when either candidate path in [`launchd_plist_paths`] exists.
/// Test: exercised manually — see module doc; the pure decisions that consume
/// this boolean are unit-tested below.
pub(crate) fn mpm_launchd_plist_exists() -> bool {
    launchd_plist_paths().iter().any(|p| p.exists())
}

/// Pure decision: is this daemon process in a hazardous unsupervised state?
///
/// Why: factored out of any filesystem/env access so the four-quadrant truth
/// table is testable without launchd, a plist, or environment variables.
/// What: `true` (safe) when no launchd unit exists at all (a dev-run daemon
/// with no plist is fine) OR this process itself is launchd-supervised.
/// `false` (hazardous) exactly when a launchd unit exists but this process
/// was NOT launched by launchd — the #2486 orphan-daemon signature.
/// Test: `compute_supervised_true_when_no_plist`,
/// `compute_supervised_true_when_plist_and_launchd`,
/// `compute_supervised_false_when_plist_without_launchd`,
/// `compute_supervised_true_when_launchd_and_no_plist` (all four quadrants of
/// the (`is_launchd_supervised`, `plist_exists`) truth table are covered).
pub(crate) fn compute_supervised(is_launchd_supervised: bool, plist_exists: bool) -> bool {
    !plist_exists || is_launchd_supervised
}

/// Pure decision: should a CLIENT-side auto-spawn of the daemon be refused?
///
/// Why: when a launchd unit is registered, `launchctl bootstrap`/`kickstart` is
/// the correct (and eventually consistent) way to bring the daemon back after a
/// restart; a client-side auto-spawn during the restart gap creates the #2486
/// orphan. When no launchd unit exists (a dev machine), auto-spawn remains the
/// convenient default.
///
/// #4230: this decision is deliberately keyed on `plist_exists` ALONE — no
/// supervision heuristic. It is consumed by every client that would spawn a
/// daemon subprocess: the MCP stdio bridge (`serve_stdio`, the original #2486
/// call site) and `tm start`/`tm restart` (`commands::daemon::start`, the path
/// that actually produced the #4230 orphan — it spawned a detached bare
/// `tm daemon` with no launchd awareness whatsoever). Keeping it heuristic-free
/// matters: the child-side #4397 guard folds in
/// [`trusty_common::update::is_launchd_supervised`], whose `XPC_SERVICE_NAME`
/// prong can report `true` for a non-launchd child spawned from a context where
/// `TERM_PROGRAM` is unset, so the callee guard alone is not a reliable last
/// line of defence.
/// What: `no_spawn` is exactly `plist_exists` — spawn is refused if and only
/// if a trusty-mpm launchd unit is present.
/// Test: `compute_no_spawn_true_when_plist_exists`,
/// `compute_no_spawn_false_when_no_plist`.
pub(crate) fn compute_no_spawn(plist_exists: bool) -> bool {
    plist_exists
}

/// Operator guidance printed when `tm start`/`tm restart` refuses to spawn a
/// daemon because launchd owns it (issue #4230).
///
/// Why: the pre-#4230 `commands::daemon::start` spawned a detached bare
/// `tm daemon` unconditionally, with no launchd check at all — that is the path
/// that produced the #4230 orphan (PID 98606, PPID 1, `cwd=$HOME`, serving a
/// stale 1.0.2 image on :7880 for two days while launchd's `com.trusty.mpm`
/// reported `not running`). Once the child-side #4397 guard landed, the same
/// invocation instead spawns a child that dies in `~/.trusty-mpm/daemon.log`
/// and `tm start` reports only "daemon did not become healthy within 5s" — the
/// operator is told the daemon is broken, not that they used the wrong verb.
/// This message names the right verb up front.
/// What: a fixed string naming the `launchctl kickstart` restart recipe, the
/// `tm daemon --force` opt-in, and the issue; single source of truth for the
/// call site and its test.
/// Test: `cli_spawn_refusal_names_kickstart_and_force`.
pub(crate) fn cli_spawn_refusal_hint() -> String {
    "refusing to spawn: a trusty-mpm launchd unit is registered, so launchd owns \
     the daemon's lifecycle — spawning one here would create a duplicate, \
     unsupervised daemon that can seize the port and serve a stale binary \
     indefinitely (issue #2486/#4230). Restart it with \
     `launchctl kickstart -k gui/$(id -u)/com.trusty.mpm` (or \
     `com.trusty.mpm.supervisor`), then re-run `tm status`. To start an \
     unsupervised daemon on purpose, run `tm daemon --force`."
        .to_string()
}

/// Pure decision: should the bare `tm daemon` CLI path refuse to start?
///
/// Why (issue #4397): the #2486 guard was wired only into the MCP-bridge's
/// `no_spawn` path (`compute_no_spawn`, consulted from `serve_stdio.rs`).
/// `run_daemon` (the bare-CLI path, invoked directly or by a launchd plist)
/// never consulted any guard, so `tm daemon` run by hand walked straight past
/// the same hazard: a launchd unit is registered, but THIS process was not
/// started by launchd — the orphan-daemon signature `compute_supervised`
/// already detects. `compute_no_spawn` itself is unsuitable here because it
/// only takes `plist_exists` — applied directly to `run_daemon` it would also
/// refuse the legitimate case where launchd itself is the one invoking
/// `tm daemon`. This function instead keys off `supervised` (which already
/// folds in `is_launchd_supervised`), plus an explicit opt-in.
/// What: `true` (refuse) exactly when `!supervised && !force` — i.e. a
/// launchd unit exists, this process isn't the one launchd started, and the
/// operator did not pass `--force`.
/// Test: `compute_daemon_refuse_true_when_unsupervised_and_not_forced`,
/// `compute_daemon_refuse_false_when_supervised`,
/// `compute_daemon_refuse_false_when_forced`.
pub(crate) fn compute_daemon_refuse(supervised: bool, force: bool) -> bool {
    !supervised && !force
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_supervised_true_when_no_plist() {
        assert!(compute_supervised(false, false));
    }

    #[test]
    fn compute_supervised_true_when_launchd_and_no_plist() {
        // Launchd-supervised with no plist registered (e.g. probed before the
        // plist path convention existed, or a unit under a third path) — still
        // safe: `is_launchd_supervised` alone is sufficient.
        assert!(compute_supervised(true, false));
    }

    #[test]
    fn compute_supervised_true_when_plist_and_launchd() {
        assert!(compute_supervised(true, true));
    }

    #[test]
    fn compute_supervised_false_when_plist_without_launchd() {
        // The hazardous #2486 state: a unit is registered but this process
        // was not launched by launchd.
        assert!(!compute_supervised(false, true));
    }

    #[test]
    fn compute_no_spawn_true_when_plist_exists() {
        assert!(compute_no_spawn(true));
    }

    #[test]
    fn compute_no_spawn_false_when_no_plist() {
        assert!(!compute_no_spawn(false));
    }

    /// #4230: the `tm start` refusal must name the launchctl verb that
    /// actually works and the `--force` opt-in, so an operator hitting it has
    /// an immediate next step either way.
    #[test]
    fn cli_spawn_refusal_names_kickstart_and_force() {
        let msg = cli_spawn_refusal_hint();
        assert!(msg.contains("launchctl kickstart"), "message was: {msg}");
        assert!(msg.contains("com.trusty.mpm"), "message was: {msg}");
        assert!(msg.contains("tm daemon --force"), "message was: {msg}");
        assert!(msg.contains("#4230"), "message was: {msg}");
    }

    #[test]
    fn compute_daemon_refuse_true_when_unsupervised_and_not_forced() {
        assert!(compute_daemon_refuse(false, false));
    }

    #[test]
    fn compute_daemon_refuse_false_when_supervised() {
        // Supervised (either no plist, or launchd-supervised) always proceeds,
        // regardless of `force`.
        assert!(!compute_daemon_refuse(true, false));
        assert!(!compute_daemon_refuse(true, true));
    }

    #[test]
    fn compute_daemon_refuse_false_when_forced() {
        // The hazardous state, but the operator explicitly opted in.
        assert!(!compute_daemon_refuse(false, true));
    }
}
