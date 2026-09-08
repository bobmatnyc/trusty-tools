//! Tracing and logging initialisation for tcode.
//!
//! Why: All tcode binaries and the library's test harness need consistent
//! tracing initialisation that writes to stderr (never stdout — stdout is the
//! API transport channel). Centralising it here prevents duplicated setup and
//! ensures the `try_init` pattern is used everywhere so test binaries remain
//! idempotent.
//! What: `init_tracing` initialises the global tracing subscriber from the
//! `RUST_LOG` env var. `init_tracing_for_test` is a lightweight variant for
//! test binaries that silently drops duplicate init errors.
//! Test: `init_tracing_for_test_is_idempotent` calls the function twice and
//! asserts no panic.
//!
//! ## Log-level convention (#2857)
//!
//! trusty-code makes control-flow decisions that change a run's outcome —
//! caps tripping, retries, delegation refusals, fallbacks, truncations,
//! skips, gate interceptions. Every such decision point MUST log at the
//! level below when it fires; the organising principle is:
//!
//! - **`warn!`** — the decision silently changes the run's outcome (a cap
//!   trips, a retry budget is exhausted, a gate refuses/intercepts an
//!   action, a floor is exceeded). Minimum level for anything that could
//!   otherwise only be diagnosed via cross-run forensics (the #2852 class
//!   of bug this ticket exists to prevent).
//! - **`info!`** — a degradation the model or user acts on, but the run
//!   proceeds normally (a fallback lane serves results, a redundant
//!   re-run is suppressed, a budget truncation drops lowest-scored
//!   content, cadence compression fires on schedule).
//! - **`debug!`** — detail useful for tracing a run's mechanics but not
//!   itself a decision that changes the outcome (byte-level truncation of
//!   a single tool's output, already surfaced in-band to the model).
//! - **`error!`** — reserved for regression signals: a safeguard fired
//!   under conditions where it should have been structurally unreachable
//!   (e.g. threshold compaction firing while the cadence compressor is
//!   supposed to keep the transcript under budget at all times).
//!
//! All logs go to **stderr** via `tracing` (never stdout — see the module
//! doc above) and are filtered by `RUST_LOG`, falling back to `info` when it
//! is unset or invalid.
//!
//! **Logs vs. events:** structured [`crate::events`] are telemetry for the
//! UI (a typed, machine-consumed fact stream); `tracing` logs here are for
//! an operator reading stderr while debugging a run. Both may describe the
//! same underlying fact, but one must never substitute for the other — an
//! event emission does not excuse a silent decision point from also logging.

use std::path::PathBuf;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer as _;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// Build the tracing filter, falling back to [`DEFAULT_LOG_LEVEL`].
///
/// Why: `EnvFilter::from_default_env` yields an EMPTY directive set when
/// `RUST_LOG` is unset or malformed, which filters everything below `error`
/// and silences the `warn!` decision points this module's convention requires.
/// What: parses `RUST_LOG`; on any parse failure builds a filter from
/// [`DEFAULT_LOG_LEVEL`] instead.
/// Test: `tests/logging_e2e.rs`.
fn env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_LEVEL))
}

/// Initialise the global tracing subscriber.
///
/// Why: All daemons and CLI entry points call this once at startup to ensure
/// log output is consistently routed to stderr with the `RUST_LOG` filter.
/// What: Installs a stderr-bound fmt subscriber with [`env_filter`].
/// Panics if called twice (use `init_tracing_for_test` in test binaries).
/// Test: Called in `main.rs`; correctness is verified by observing log output.
pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(env_filter())
        .with_writer(std::io::stderr)
        .init();
}

/// Initialise the global tracing subscriber, silently ignoring duplicate inits.
///
/// Why: Test binaries may link multiple test modules that all call setup code.
/// `try_init` returns an error on the second call instead of panicking, so
/// test runs remain stable regardless of execution order.
/// What: Calls `try_init()`; swallows the error if already initialised.
/// Test: `init_tracing_for_test_is_idempotent`.
pub fn init_tracing_for_test() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter())
        .with_writer(std::io::stderr)
        .try_init();
}

/// Log level used when no `RUST_LOG` env var is set.
///
/// Why: Documents and centralises the default so operators know what to expect
/// without reading source.
/// What: A static string literal used by [`env_filter`] as its real fallback.
/// Test: `tests/logging_e2e.rs` covers unset and invalid `RUST_LOG` values.
pub const DEFAULT_LOG_LEVEL: &str = "info";

/// Basename `tracing_appender::rolling::daily` rotates:
/// `tcode.log.YYYY-MM-DD` (#6537).
pub const FILE_LOG_BASENAME: &str = "tcode.log";

/// Directory tcode's own rotating file log is written to: `~/.trusty-code/logs`.
///
/// Why (#6537): before this, `init_tracing` wrote to stderr only, which
/// nothing durable ever collects — the epic's whole point (cloud log drain,
/// #6533) needs a file to drain. This mirrors trusty-mpm's own
/// `~/.trusty-mpm/logs` layout, under the private-state directory
/// [`crate::paths::private_state`] already established rather than a new
/// root.
/// What: `crate::paths::private_state::private_state_dir().join("logs")`.
/// Test: `tests::file_log_dir_is_under_private_state`.
pub fn file_log_dir() -> PathBuf {
    crate::paths::private_state::private_state_dir().join("logs")
}

/// Initialise tracing for `tcode serve`: stderr (unchanged, MCP-framing-safe)
/// PLUS a daily-rotating file layer under [`file_log_dir`] (#6537).
///
/// Why: only the long-running `serve` daemon accumulates enough log volume to
/// be worth draining — mirrors trusty-mpm's own daemon/CLI split
/// (`bin/tm/tracing_setup.rs`), where a short-lived CLI invocation gets the
/// lighter [`init_tracing`] instead. A file-directory creation failure (a
/// read-only home, a full disk) degrades to stderr-only rather than failing
/// daemon startup. A prior version created the directory with a raw
/// `create_dir_all`, which applies the process umask and commonly leaves
/// `0755`/`0644` — code-review fix (#6537): this now routes through the same
/// private-state hardening every other writer under `~/.trusty-code` uses
/// (#6999), so the directory is `0700` and each rolled file `0600`.
/// What: builds a `tracing_subscriber::registry()` with a stderr `fmt` layer
/// and, when the log directory is creatable, a non-blocking daily file `fmt`
/// layer (`with_ansi(false)`, since a rotated file is read by tooling, not a
/// terminal). The directory is created and tightened via
/// [`crate::paths::private_state::ensure_private_state_dir`] (the
/// `~/.trusty-code` root) then
/// [`crate::paths::private_state::ensure_dir`] (the `logs` subdirectory
/// itself); [`chmod_log_files_owner_only`] then tightens whichever file
/// `tracing_appender::rolling::daily` just opened for today. Returns the
/// file layer's `WorkerGuard` — the caller MUST hold it for the process
/// lifetime; dropping it early silently discards buffered log records.
/// Test: `tests::file_log_dir_is_under_private_state` covers the path
/// computation this function depends on; installing a second GLOBAL
/// subscriber in-process (this function's own behavior) cannot itself run in
/// the shared `--lib` test binary alongside `begin_capture`'s `try_init` —
/// see that test's own doc comment. The directory/file mode guarantee is
/// covered by `tests/logging_e2e.rs::file_log_dir_and_current_file_are_owner_only`
/// (a subprocess with `HOME` pointed at a tempdir, for the same reason).
pub fn init_tracing_with_file_log() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(env_filter());

    let log_dir = file_log_dir();
    let create_result = crate::paths::private_state::ensure_private_state_dir()
        .and_then(|_| crate::paths::private_state::ensure_dir(&log_dir));
    let (file_layer, guard) = match create_result {
        Ok(()) => {
            let appender = tracing_appender::rolling::daily(&log_dir, FILE_LOG_BASENAME);
            chmod_log_files_owner_only(&log_dir);
            let (non_blocking, guard) = tracing_appender::non_blocking(appender);
            let layer = tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_filter(env_filter());
            (Some(layer), Some(guard))
        }
        Err(e) => {
            eprintln!(
                "tcode: cannot create log directory {}: {e} (stderr-only for this run)",
                log_dir.display()
            );
            (None, None)
        }
    };

    tracing_subscriber::registry()
        .with(stderr_layer)
        .with(file_layer)
        .init();

    guard
}

/// Tighten every regular file already in `log_dir` to owner-only (#6537).
///
/// Why: `tracing_appender::rolling::daily` opens today's file the moment the
/// appender is constructed — before this function runs — with whatever mode
/// the process umask leaves (commonly `0644`). Chmod'ing every entry
/// currently in the directory, rather than computing tomorrow's filename
/// itself, stays correct regardless of `tracing_appender`'s internal naming.
/// KNOWN LIMITATION: a file this appender creates on a LATER day's rotation,
/// after this function has already returned, is never chmod'd by it — this
/// daemon is expected to restart at least daily in practice, and each
/// restart re-tightens whatever file is current at that point.
/// What: on Unix, chmod every regular file directly under `log_dir` to
/// `0600`, logging a `warn` (never failing) on any entry it cannot stat or
/// chmod. No-op on non-Unix targets, which have no comparable mode bits.
/// Test: `tests/logging_e2e.rs::file_log_dir_and_current_file_are_owner_only`.
#[cfg(unix)]
fn chmod_log_files_owner_only(log_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "could not tighten a tcode log file to 0600; its contents may \
                 be readable by other users on this machine"
            );
        }
    }
}

#[cfg(not(unix))]
fn chmod_log_files_owner_only(_log_dir: &std::path::Path) {}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `init_tracing_for_test` must be idempotent across multiple calls.
    ///
    /// Why: Test binaries are often multi-crate and may call init from several
    /// setup functions; a panic on re-init would break the test suite. (#2857)
    /// This is the ONLY other site in this crate's `--lib` test binary that
    /// installs a GLOBAL tracing subscriber via `try_init`. Calling
    /// `crate::test_support::begin_capture()` first guarantees the #2857
    /// capture module's own `set_global_default` call always wins that
    /// one-shot race — this test's own `try_init()` then simply no-ops
    /// (already documented/expected behaviour), regardless of which test the
    /// harness happens to schedule first.
    /// What: Calls `init_tracing_for_test` twice; asserts no panic.
    /// Test: This test.
    #[test]
    fn init_tracing_for_test_is_idempotent() {
        crate::test_support::begin_capture();
        init_tracing_for_test();
        init_tracing_for_test(); // second call must not panic
    }

    /// The default log level constant is non-empty.
    ///
    /// Why: Guard against accidental empty string.
    /// What: Asserts `DEFAULT_LOG_LEVEL` is non-empty.
    /// Test: This test.
    #[test]
    fn default_log_level_is_non_empty() {
        assert!(!DEFAULT_LOG_LEVEL.is_empty());
    }

    /// `file_log_dir` sits under the crate's own private-state directory,
    /// never a bespoke root (#6537).
    ///
    /// Why: `init_tracing_with_file_log` installs a GLOBAL subscriber, so it
    /// cannot itself be called from this shared test binary without racing
    /// `begin_capture`'s own `try_init` — this test covers the path
    /// computation, which is what the log-drain source (`crate::log_drain`)
    /// actually depends on.
    #[test]
    fn file_log_dir_is_under_private_state() {
        let dir = file_log_dir();
        assert!(dir.ends_with("logs"), "{}", dir.display());
        assert!(
            dir.to_string_lossy().contains(".trusty-code"),
            "{}",
            dir.display()
        );
    }
}
