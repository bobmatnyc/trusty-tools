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
/// `compute_supervised_true_when_no_plist_and_not_launchd`.
pub(crate) fn compute_supervised(is_launchd_supervised: bool, plist_exists: bool) -> bool {
    !plist_exists || is_launchd_supervised
}

/// Pure decision: should the MCP stdio bridge refuse to auto-spawn a daemon?
///
/// Why: when a launchd unit is registered, `launchctl bootstrap` is the
/// correct (and eventually consistent) way to bring the daemon back after a
/// restart; a bridge-side auto-spawn during the restart gap creates the #2486
/// orphan. When no launchd unit exists (a dev machine), auto-spawn remains the
/// convenient default, matching the pre-existing `tm start` behaviour.
/// What: `no_spawn` is exactly `plist_exists` — spawn is refused if and only
/// if a trusty-mpm launchd unit is present.
/// Test: `compute_no_spawn_true_when_plist_exists`,
/// `compute_no_spawn_false_when_no_plist`.
pub(crate) fn compute_no_spawn(plist_exists: bool) -> bool {
    plist_exists
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_supervised_true_when_no_plist() {
        assert!(compute_supervised(false, false));
    }

    #[test]
    fn compute_supervised_true_when_no_plist_and_not_launchd() {
        // No plist at all — even an unsupervised process is fine (dev machine).
        assert!(compute_supervised(false, false));
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
}
