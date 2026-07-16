//! Fan-out concurrency bounding for the global (`POST /search`) handler
//! (issue #2845).
//!
//! Why: a single global search fans out one per-index query per registered
//! index. With `join_all` that fan-out was *unbounded* — a request touching
//! ~150+ repo indexes issued ~150 per-index searches near-simultaneously, each
//! one CPU/memory work competing for the same runtime, which produced the
//! long-tail latency and resource pressure behind the `server_busy` storm
//! observed in production 2026-07-16 (see `apply_limiter` in
//! `service::concurrency` for the HTTP-layer admission limiter itself — the
//! per-index searches bounded here are in-process and never traverse it
//! directly). Bounding the fan-out with a small semaphore-style cap lets a
//! large fan-out degrade gracefully — results are still returned, just in
//! bounded waves — and shortens per-request latency, which indirectly
//! relieves pressure on the admission limiter by shrinking the window in
//! which new inbound requests can pile up. This is a single-large-fan-out
//! fix, not a process-global concurrency bound — see the residual-limitation
//! note on `global_search_handler` in `search_global.rs`.
//! What: pure resolution of the effective per-fan-out concurrency cap from
//! (1) a per-request override, (2) the `TRUSTY_SEARCH_FANOUT_CONCURRENCY` env
//! var, or (3) a conservative built-in default. `--serial` (or a request
//! `serial: true`) collapses the cap to 1 for strictly sequential execution.
//! Test: `parse_fanout_env`, `resolve_precedence`, and `serial_forces_one`
//! at the bottom of this file.

/// Env var that overrides the default fan-out concurrency cap.
///
/// Why: operators on memory/CPU-constrained hosts need to tune (or pin) the
/// cap without a code change; the `trusty-search start --fanout-concurrency N`
/// / `--serial` flags translate into this var so the daemon and its self-spawn
/// child agree on a single source of truth.
/// What: parsed as `usize`, clamped to `>= 1`; unset/invalid falls back to
/// [`DEFAULT_FANOUT_CONCURRENCY`].
/// Test: `parse_fanout_env`.
pub(super) const FANOUT_CONCURRENCY_ENV: &str = "TRUSTY_SEARCH_FANOUT_CONCURRENCY";

/// Conservative default cap on concurrently-executing per-index searches
/// within a single fan-out (issue #2845).
///
/// Why: 8 is a conservative resource-saturation cap borrowed from the
/// daemon's own `TRUSTY_MAX_CONCURRENT_REQUESTS` default (see
/// `service::concurrency`) as a reasonable starting point — NOT a
/// by-construction admission bound. Per-index `indexer.search()` calls inside
/// a fan-out are in-process futures; they never traverse the HTTP layer and
/// so never pass through `apply_limiter` / the admission semaphore. The two
/// "8"s govern uncoupled resources (in-process fan-out breadth vs. inbound
/// HTTP request admission). Bounding the fan-out relieves limiter pressure
/// only *indirectly*: fewer concurrent per-index searches means each holds
/// CPU/memory for less overlapping time, which shortens per-request latency
/// and so shrinks the window in which new inbound HTTP requests pile up
/// against the admission queue. It is high enough that small/medium fan-outs
/// run effectively in parallel, and low enough that a 150-index fan-out
/// drains in ~19 bounded waves instead of one thundering herd.
/// What: used when neither a per-request override nor the env var is present.
/// Test: `parse_fanout_env` (unset path), `resolve_precedence`.
pub(super) const DEFAULT_FANOUT_CONCURRENCY: usize = 8;

/// Parse the raw `TRUSTY_SEARCH_FANOUT_CONCURRENCY` value into an effective cap.
///
/// Why: kept pure (takes the raw `Option<String>`) so it is testable without
/// mutating process-global environment state.
/// What: parses as `usize` and clamps to `>= 1`; `None` or an unparseable /
/// zero value yields [`DEFAULT_FANOUT_CONCURRENCY`].
/// Test: `parse_fanout_env`.
pub(super) fn parse_fanout_env(raw: Option<String>) -> usize {
    raw.and_then(|v| v.trim().parse::<usize>().ok())
        .map(|n| n.max(1))
        .unwrap_or(DEFAULT_FANOUT_CONCURRENCY)
}

/// Resolve the effective fan-out concurrency cap for one global search.
///
/// Why: callers (the request handler) need a single number bounding how many
/// per-index searches run at once. Precedence lets an individual caller force
/// serial or a custom cap per request (e.g. a PR-review pipeline on a hot host)
/// without restarting the daemon, while the env/default govern the daemon-wide
/// baseline.
/// What: precedence is `serial` (→ 1) > per-request `max` override (clamped
/// `>= 1`) > `TRUSTY_SEARCH_FANOUT_CONCURRENCY` env > [`DEFAULT_FANOUT_CONCURRENCY`].
/// The env var is read fresh each call (one lookup per fan-out is negligible)
/// so a running daemon honours a live `daemon.env` change.
/// Test: `resolve_precedence`, `serial_forces_one`.
pub(super) fn resolve_fanout_concurrency(serial: bool, request_max: Option<usize>) -> usize {
    if serial {
        return 1;
    }
    if let Some(n) = request_max {
        return n.max(1);
    }
    parse_fanout_env(std::env::var(FANOUT_CONCURRENCY_ENV).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fanout_env() {
        // Unset → default.
        assert_eq!(
            super::parse_fanout_env(None),
            DEFAULT_FANOUT_CONCURRENCY,
            "unset falls back to default"
        );
        // Valid value is honoured.
        assert_eq!(super::parse_fanout_env(Some("4".into())), 4);
        // Zero and negatives / garbage clamp or fall back.
        assert_eq!(
            super::parse_fanout_env(Some("0".into())),
            1,
            "zero clamps to 1"
        );
        assert_eq!(
            super::parse_fanout_env(Some("nonsense".into())),
            DEFAULT_FANOUT_CONCURRENCY,
            "unparseable falls back to default"
        );
        // Whitespace is tolerated.
        assert_eq!(super::parse_fanout_env(Some(" 16 ".into())), 16);
    }

    #[test]
    fn resolve_precedence() {
        // A per-request max override wins over the env/default (env is not set
        // by this test; the override short-circuits before the env read).
        assert_eq!(resolve_fanout_concurrency(false, Some(3)), 3);
        // Override is clamped to >= 1.
        assert_eq!(resolve_fanout_concurrency(false, Some(0)), 1);
    }

    #[test]
    fn serial_forces_one() {
        // `serial` beats any override, however large.
        assert_eq!(resolve_fanout_concurrency(true, Some(99)), 1);
        assert_eq!(resolve_fanout_concurrency(true, None), 1);
    }
}
