//! Per-palace `trusty-bm25-daemon` spawn supervisor (issue #193).
//!
//! Why: trusty-memory ships the `trusty-bm25-daemon` binary alongside its own
//! but never actually spawned it, so operators who set `TRUSTY_BM25_DAEMON=1`
//! had to babysit one daemon per palace by hand. This module makes BM25 a
//! single-process concern: on first BM25 use for a palace, discover the binary,
//! spawn a child with the right `--palace` + `--data-dir`, poll the socket until
//! it serves, and own the child for the rest of the daemon's life.
//!
//! What: a thin BM25-shaped face over
//! [`trusty_common::uds::supervisor::UdsServiceSupervisor`] (#5089 step 2). The
//! state machine — spawn-gate serialisation, socket adoption, the LRU cap, the
//! RSS ceiling, socket-backed liveness, the doomed queue, SIGTERM→SIGKILL — is
//! shared now, so `trusty-console` inherits the same hardening for
//! `trusty-review` / `trusty-analyze` (ADR-0034 §1) instead of re-earning it.
//! What stays here is what is genuinely BM25's: the env-var knobs, the socket
//! path convention, the daemon's argv, and — see [`BM25_TIMEOUTS`] — the two
//! timing numbers that are statements about `trusty-bm25-daemon` rather than
//! about supervision.
//!
//! Test: unit tests in `bm25_supervisor_tests.rs` cover the env knobs, the
//! external-mode opt-out, socket adoption, idempotent shutdown, and the
//! patience-vs-flush relationship. `tests/bm25_supervisor_concurrency.rs` drives
//! real daemon children through the double-spawn, aggregate-cap, dead-child,
//! unserved-socket and evicted-live-child-flush paths. The shared machinery's
//! own unit coverage lives in `trusty-common`'s `uds/supervisor/tests.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

use trusty_common::bm25_client::{locate_bm25_daemon_binary, socket_path_for_palace};
use trusty_common::uds::supervisor::{
    ServiceTimeouts, SpawnSpec, SupervisorConfig, UdsServiceSupervisor,
};

/// Environment variable that disables spawn supervision entirely.
///
/// Why: operators who manage `trusty-bm25-daemon` themselves (launchd plist,
/// systemd unit, docker sidecar) must be able to opt the in-process supervisor
/// out so two daemons never fight over one socket.
/// What: `TRUSTY_BM25_EXTERNAL`. Any value other than `"1"` is treated as unset.
/// Test: `external_mode_skips_spawn`.
pub const ENV_EXTERNAL_BM25: &str = "TRUSTY_BM25_EXTERNAL";

/// Environment variable that overrides the cap on concurrently-live daemons.
///
/// Why (#2845): `ensure_running` is called once per palace, and one
/// `memory_recall_all` touches every palace on disk — ~99 on this host. Without
/// a cap the supervisor would hold 99 child processes for the rest of the
/// trusty-memory daemon's life. Drawer distribution is heavily skewed, so the
/// working set is a handful of palaces and a small cap costs almost nothing.
/// What: `TRUSTY_BM25_MAX_DAEMONS`, parsed as `usize`; values below 1 and
/// unparseable values fall back to [`DEFAULT_MAX_LIVE_DAEMONS`].
/// Test: `max_live_daemons_honours_env_override`.
pub const ENV_MAX_DAEMONS: &str = "TRUSTY_BM25_MAX_DAEMONS";

/// Environment variable that overrides the per-daemon RSS ceiling, in MB.
///
/// Why (#2846): trusty-search declared an `rss_limit_mb` and never compared it
/// against anything; the process grew to 2.2x that limit and was OOM-killed. A
/// BM25 daemon's memory scales linearly with its palace's drawer text because
/// `PalaceBm25Index` retains every document's full text, so an unbounded palace
/// is an unbounded daemon.
/// What: `TRUSTY_BM25_RSS_LIMIT_MB`. `0` disables enforcement; unparseable
/// values fall back to [`DEFAULT_RSS_LIMIT_MB`].
/// Test: `rss_limit_honours_env_override`.
pub const ENV_RSS_LIMIT_MB: &str = "TRUSTY_BM25_RSS_LIMIT_MB";

/// Default cap on concurrently-live BM25 daemons.
///
/// Why: three covers the realistic working set — the palace you are in, the one
/// you just cross-referenced, and one in flight — while keeping the worst case
/// at three subprocesses instead of ninety-nine.
/// What: `3`.
/// Test: `default_cap_is_three`.
pub const DEFAULT_MAX_LIVE_DAEMONS: usize = 3;

/// Default per-daemon RSS ceiling in megabytes.
///
/// Why: the whole drawer corpus across ~99 palaces is single-digit MB of text,
/// so a single daemon holding more than 512 MB is not holding drawers — it is
/// leaking, and #2846 is the record of what happens when nobody notices.
/// What: `512`.
/// Test: `rss_limit_honours_env_override`.
pub const DEFAULT_RSS_LIMIT_MB: u64 = 512;

/// Upper bound on how long `ensure_running` waits for a freshly-spawned daemon
/// to bind and accept.
///
/// Why: BM25's bind step is fast — the snapshot load is the slowest part and
/// runs on a tempdir-sized fixture in tests — so 3 s is comfortably more than
/// the observed worst case while still failing fast on a misconfigured spawn.
/// The 10x gap against the embedder supervisor's 30 s default is entirely
/// explained by BM25 having no model to load, which is exactly why this number
/// is declared here rather than in the shared supervisor.
const SPAWN_PROBE_TIMEOUT: Duration = Duration::from_millis(3000);

/// How long the supervisor waits after SIGTERM before escalating to SIGKILL.
///
/// Why: strictly greater than the daemon's own `SHUTDOWN_FLUSH_TIMEOUT` (2 s),
/// because the daemon needs signal delivery, the flush itself, socket cleanup
/// and exit inside this window. At an equal budget the SIGKILL lands mid-flush
/// and the open write window is lost.
/// Test: `sigterm_patience_exceeds_the_daemon_flush_budget`.
const SIGTERM_PATIENCE: Duration = Duration::from_secs(5);

/// `trusty-bm25-daemon`'s timing budget, and the compile-time guard on it.
///
/// 🔴 This `const` item is what replaces the old
/// `const _: () = assert!(SIGTERM_PATIENCE_SECS > …SHUTDOWN_FLUSH_TIMEOUT…)`
/// (#5085). That assertion could not cross into `trusty-common` — a shared
/// supervisor cannot name any particular daemon's flush budget without depending
/// on it — so the check moved into [`ServiceTimeouts::new`], which is a
/// `const fn`. Evaluating it in a `const` item runs the assert at compile time,
/// exactly as before, and binds it to the value actually handed to the
/// supervisor rather than to a free-standing constant a refactor could leave
/// behind. Lower `SIGTERM_PATIENCE` to 2 s, or raise the daemon's
/// `SHUTDOWN_FLUSH_TIMEOUT` past it, and this line fails the build.
///
/// The daemon's budget is imported, never restated: a hardcoded copy stays equal
/// to itself while the real value drifts, so it could not detect the drift it
/// exists to name.
/// Test: `sigterm_patience_exceeds_the_daemon_flush_budget` pins the margin;
/// the strict inequality is the compiler's job.
const BM25_TIMEOUTS: ServiceTimeouts = ServiceTimeouts::new(
    SPAWN_PROBE_TIMEOUT,
    trusty_bm25_daemon::SHUTDOWN_FLUSH_TIMEOUT,
    SIGTERM_PATIENCE,
);

/// Supervisor that owns BM25 daemon subprocesses, one per palace.
///
/// Why: trusty-memory wants the BM25 lane to be zero-touch — set
/// `TRUSTY_BM25_DAEMON=1` and recall just gets a lexical boost. Owning the
/// children here means the trusty-memory daemon's lifetime IS the BM25 daemons'
/// lifetime.
/// What: a [`UdsServiceSupervisor`] keyed by palace id, plus BM25's socket-path
/// and argv conventions. Every method is `&self` so the supervisor can live
/// behind an `Arc`.
/// Test: `bm25_supervisor_tests.rs` and `tests/bm25_supervisor_concurrency.rs`.
#[derive(Debug)]
pub struct Bm25Supervisor {
    inner: UdsServiceSupervisor,
}

impl Bm25Supervisor {
    /// Construct an empty supervisor with limits read from the environment.
    ///
    /// Why: resolving the limits here — rather than on each `ensure_running` —
    /// means a mid-flight env mutation cannot make the cap wobble between two
    /// concurrent calls.
    /// Test: `default_cap_is_three`, `max_live_daemons_honours_env_override`.
    pub fn new() -> Self {
        Self::with_limits(max_live_from_env(), rss_limit_from_env())
    }

    /// Construct a supervisor with explicit limits, bypassing the environment.
    ///
    /// Why: the limit tests must pin a cap of 1 or an RSS ceiling of 1 MB
    /// without mutating process-global env vars that sibling tests race against.
    /// What: `max_live` is clamped to at least 1 — a cap of zero would reap every
    /// daemon the instant it spawned. `rss_limit_mb: None` disables RSS
    /// enforcement; `Some(n)` reaps any child measuring at or above `n` MB.
    /// Test: `cap_is_clamped_to_at_least_one`.
    pub fn with_limits(max_live: usize, rss_limit_mb: Option<u64>) -> Self {
        Self {
            inner: UdsServiceSupervisor::new(
                SupervisorConfig::new("trusty-bm25-daemon", max_live, BM25_TIMEOUTS)
                    .with_rss_limit_mb(rss_limit_mb)
                    .with_external_env(ENV_EXTERNAL_BM25),
            ),
        }
    }

    /// Cap on concurrently-live daemons this supervisor enforces.
    pub fn max_live(&self) -> usize {
        self.inner.max_live()
    }

    /// Per-daemon RSS ceiling in MB; `None` means enforcement is off.
    pub fn rss_limit_mb(&self) -> Option<u64> {
        self.inner.rss_limit_mb()
    }

    /// How many children have been reaped by the cap or the RSS limit.
    ///
    /// Why: "the cap is configured" and "the cap did something" are different
    /// claims, and #2846 is the record of a limit that only ever made the first.
    /// `shutdown` does not increment it.
    /// Test: `a_concurrent_fanout_never_exceeds_the_cap`.
    pub fn reaped_count(&self) -> u64 {
        self.inner.reaped_count()
    }

    /// How many daemon processes this supervisor has launched.
    ///
    /// Why: this is the only externally-visible difference between "spawns are
    /// serialised" and "spawns race" — a double spawn leaves the map holding one
    /// entry either way, because the loser's daemon fails its bind and dies.
    /// Test: `concurrent_callers_for_one_palace_spawn_exactly_one_daemon`.
    pub fn spawned_count(&self) -> u64 {
        self.inner.spawned_count()
    }

    /// Number of palaces currently being supervised.
    pub async fn supervised_count(&self) -> usize {
        self.inner.supervised_count().await
    }

    /// Ensure a `trusty-bm25-daemon` is running for `palace` and return the
    /// socket path the caller should connect to.
    ///
    /// Why: callers want one function that handles every state a per-palace
    /// daemon can be in without reimplementing probe-and-spawn. Returning the
    /// socket path rather than a `Bm25Client` keeps the supervisor free of the
    /// BM25 wire protocol.
    /// What: resolves BM25's canonical socket path, then defers to
    /// [`UdsServiceSupervisor::ensure_running`]. The daemon binary is located
    /// lazily, inside the spawn-spec closure, so the external-mode, fast and
    /// adoption paths never require it to be installed.
    /// Test: `external_mode_skips_spawn`, `already_running_skips_spawn`, and the
    /// concurrency suite.
    pub async fn ensure_running(&self, palace: &str, data_dir: &Path) -> Result<PathBuf> {
        let socket_path = socket_path_for_palace(palace);
        let data_dir = data_dir.to_path_buf();
        self.inner
            .ensure_running(palace, &socket_path, || {
                let binary = locate_bm25_daemon_binary()?;
                Ok(SpawnSpec::new(binary)
                    .arg("--palace")
                    .arg(palace)
                    .arg("--data-dir")
                    .arg(&data_dir)
                    .create_dir(&data_dir))
            })
            .await
            .with_context(|| format!("ensure trusty-bm25-daemon is running for palace {palace}"))
    }

    /// Graceful shutdown: SIGTERM all owned daemons, reap them, and clean up
    /// their sockets.
    ///
    /// Why: trusty-memory's normal exit is a SIGTERM from launchd or a ctrl-c.
    /// Relying on `kill_on_drop` instead would SIGKILL each daemon, skipping its
    /// own socket cleanup and leaving the BM25 snapshot half-flushed.
    /// Test: `shutdown_with_no_children_is_noop`, and the e2e test's assertion
    /// that the child is reaped and the socket file removed.
    pub async fn shutdown(&self) {
        self.inner.shutdown().await;
    }
}

impl Default for Bm25Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve the live-daemon cap from [`ENV_MAX_DAEMONS`].
///
/// Why: an operator who fans out wider than the default working set needs a
/// knob, and a knob that silently ignores a typo is worse than no knob.
/// What: parses as `usize`; `0` and unparseable values fall back to
/// [`DEFAULT_MAX_LIVE_DAEMONS`].
/// Test: `max_live_daemons_honours_env_override`.
fn max_live_from_env() -> usize {
    match std::env::var(ENV_MAX_DAEMONS) {
        Ok(raw) => match raw.trim().parse::<usize>() {
            Ok(n) if n >= 1 => n,
            _ => {
                tracing::warn!(
                    "{ENV_MAX_DAEMONS}={raw:?} is not a positive integer — \
                     using default {DEFAULT_MAX_LIVE_DAEMONS}"
                );
                DEFAULT_MAX_LIVE_DAEMONS
            }
        },
        Err(_) => DEFAULT_MAX_LIVE_DAEMONS,
    }
}

/// Resolve the per-daemon RSS ceiling from [`ENV_RSS_LIMIT_MB`].
///
/// Why: same knob-with-a-typo argument as [`max_live_from_env`], plus one
/// specific to this limit — `0` must be an explicit, documented way to turn
/// enforcement off, not an accident of parsing.
/// What: parses as `u64`; `0` maps to `None` (enforcement off); unparseable
/// values fall back to [`DEFAULT_RSS_LIMIT_MB`].
/// Test: `rss_limit_honours_env_override`.
fn rss_limit_from_env() -> Option<u64> {
    match std::env::var(ENV_RSS_LIMIT_MB) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(_) => {
                tracing::warn!(
                    "{ENV_RSS_LIMIT_MB}={raw:?} is not an integer — \
                     using default {DEFAULT_RSS_LIMIT_MB}"
                );
                Some(DEFAULT_RSS_LIMIT_MB)
            }
        },
        Err(_) => Some(DEFAULT_RSS_LIMIT_MB),
    }
}

#[cfg(test)]
#[path = "bm25_supervisor_tests.rs"]
mod tests;
