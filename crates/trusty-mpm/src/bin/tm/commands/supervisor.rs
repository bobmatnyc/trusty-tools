//! `supervisor` subcommand — run the unattended 24/7 fleet supervisor (#1206).
//!
//! Why: for overnight / unattended operation the managed-session fleet needs an
//! always-on process that auto-resumes `stopped` sessions, observes health
//! without a live caller, surfaces pending decisions, and exposes fleet metrics —
//! all while making NO autonomy decisions. This handler is the operator entry
//! point (`tm supervisor`) that wires the real session manager + activity monitor
//! and runs the loop until the process is signalled to stop.
//! What: builds a [`SessionManager`] over the real tmux driver (falling back to a
//! no-op driver when tmux is absent), resolves the [`SupervisorConfig`] from env
//! with CLI overrides, spawns the `/metrics` + `/health` server, and runs the
//! supervisor loop.
//! Test: `cli_parses_supervisor` covers flag parsing; the loop logic is unit-
//! tested in `trusty_mpm::supervisor::tests`.

use std::net::SocketAddr;
use std::sync::Arc;

use trusty_mpm::activity::monitor::{ActivityMonitor, OpenRouterClassifier};
use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::session_manager::real_tmux::NoopTmuxDriver;
use trusty_mpm::session_manager::{ManagedTmuxDriver, RealTmuxDriver, SessionManager};
use trusty_mpm::supervisor::config::{DEFAULT_LLM_MODEL, ENV_LLM_MODEL};
use trusty_mpm::supervisor::{Supervisor, SupervisorConfig};

/// Validate the optional `--interval` CLI override and apply it to a config.
///
/// Why: previously a `--interval 0` was silently filtered to the default, so an
/// operator who fat-fingered a zero got no feedback and a cadence they did not
/// ask for. Rejecting zero with an explicit error gives immediate, actionable
/// feedback instead of a surprising silent fallback.
/// What: when `interval` is `Some(0)` returns an error; when `Some(n)` with
/// `n > 0` sets `cfg.interval` to `n` seconds; when `None` leaves `cfg` untouched
/// (the env-derived / default cadence stands).
/// Test: `interval_zero_is_rejected`, `interval_positive_overrides`,
/// `interval_none_keeps_config` in `super::tests`.
pub(crate) fn apply_interval_override(
    cfg: &mut SupervisorConfig,
    interval: Option<u64>,
) -> anyhow::Result<()> {
    match interval {
        Some(0) => anyhow::bail!(
            "--interval must be greater than 0 seconds (got 0); omit the flag to use the default"
        ),
        Some(secs) => {
            cfg.interval = std::time::Duration::from_secs(secs);
            Ok(())
        }
        None => Ok(()),
    }
}

/// Run the supervisor loop with config resolved from env + CLI overrides.
///
/// Why: separating config resolution + wiring from the loop itself keeps the
/// handler readable and lets the CLI flags cleanly override the env defaults.
/// What: loads the managed-session store under `~/.trusty-mpm/session-manager`,
/// builds the activity monitor unless `--no-classify` (or the env toggle) disables
/// it, spawns the metrics server, and runs [`Supervisor::run`]. CLI flags take
/// precedence over `TRUSTY_MPM_SUPERVISOR_*` / `TRUSTY_MPM_AUTO_RESUME`.
/// Test: `cli_parses_supervisor`; loop behavior in `supervisor::tests`.
pub(crate) async fn run_supervisor(
    addr: Option<SocketAddr>,
    interval: Option<u64>,
    auto_resume: bool,
    no_classify: bool,
) -> anyhow::Result<()> {
    // Resolve config from env, then apply CLI overrides (CLI wins).
    let mut cfg = SupervisorConfig::from_env();
    if let Some(a) = addr {
        cfg.metrics_addr = a;
    }
    // A zero interval is rejected outright (immediate feedback) rather than
    // silently falling back to the default.
    apply_interval_override(&mut cfg, interval)?;
    if auto_resume {
        cfg.auto_resume = true;
    }
    if no_classify {
        cfg.classify_idle = false;
    }

    // Build the session manager over the real tmux driver (or a no-op fallback).
    let data_dir = FrameworkPaths::default().root.join("session-manager");
    std::fs::create_dir_all(&data_dir)?;
    let tmux: Arc<dyn ManagedTmuxDriver> = match RealTmuxDriver::discover() {
        Ok(d) => Arc::new(d),
        Err(e) => {
            tracing::warn!("tmux unavailable for supervisor: {e}; using no-op driver");
            Arc::new(NoopTmuxDriver)
        }
    };
    let mgr = Arc::new(SessionManager::new(&data_dir, tmux).await?);

    // Build the activity monitor unless classification is disabled.
    let monitor = if cfg.classify_idle {
        let model = std::env::var(ENV_LLM_MODEL).unwrap_or_else(|_| DEFAULT_LLM_MODEL.to_owned());
        Some(ActivityMonitor::new(OpenRouterClassifier::new(), model))
    } else {
        None
    };

    // Bind the /metrics + /health listener BEFORE the loop so a port collision
    // fails fast (propagated out of run_supervisor) instead of leaving the
    // supervisor running for hours with no metrics endpoint.
    let handle = trusty_mpm::supervisor::new_handle();
    let metrics_addr = cfg.metrics_addr;
    let listener = trusty_mpm::supervisor::http::bind(metrics_addr).await?;

    // Now that the port is confirmed free, serve on the bound listener. We keep
    // the JoinHandle and select on it inside the loop so a later serve failure
    // surfaces rather than being swallowed by a detached task.
    let server_handle = handle.clone();
    let mut server_task = tokio::spawn(async move {
        trusty_mpm::supervisor::http::serve_on(listener, server_handle).await
    });

    tracing::info!(
        addr = %metrics_addr,
        interval_secs = cfg.interval.as_secs(),
        auto_resume = cfg.auto_resume,
        classify_idle = cfg.classify_idle,
        "starting unattended supervisor"
    );

    let supervisor = Supervisor::new(mgr, cfg, monitor);
    // Race the supervisor loop against the metrics server task: if the server
    // dies (serve error or panic), abort the supervisor and propagate the error
    // instead of running on without observability.
    tokio::select! {
        loop_result = supervisor.run(handle) => loop_result,
        server_result = &mut server_task => {
            match server_result {
                Ok(Ok(())) => {
                    anyhow::bail!("supervisor metrics server exited unexpectedly")
                }
                Ok(Err(e)) => Err(e.context("supervisor metrics server failed")),
                Err(join_err) => {
                    Err(anyhow::anyhow!("supervisor metrics server task panicked: {join_err}"))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: a zero interval is a fat-finger that previously vanished into the
    /// default; it must now surface as an error so the operator notices.
    /// What: asserts `apply_interval_override(_, Some(0))` is an `Err` and that
    /// the config interval is left unchanged.
    /// Test: this test.
    #[test]
    fn interval_zero_is_rejected() {
        let mut cfg = SupervisorConfig::default();
        let before = cfg.interval;
        let result = apply_interval_override(&mut cfg, Some(0));
        assert!(result.is_err(), "--interval 0 must be rejected");
        assert_eq!(
            cfg.interval, before,
            "rejected interval must not mutate cfg"
        );
    }

    /// Why: a positive override is the intended path and must replace the
    /// env/default cadence.
    /// What: asserts `Some(n)` sets `cfg.interval` to `n` seconds.
    /// Test: this test.
    #[test]
    fn interval_positive_overrides() {
        let mut cfg = SupervisorConfig::default();
        apply_interval_override(&mut cfg, Some(45)).expect("positive interval is accepted");
        assert_eq!(cfg.interval, std::time::Duration::from_secs(45));
    }

    /// Why: omitting `--interval` must leave the env-derived / default cadence in
    /// place rather than zeroing it.
    /// What: asserts `None` leaves `cfg.interval` untouched.
    /// Test: this test.
    #[test]
    fn interval_none_keeps_config() {
        let mut cfg = SupervisorConfig::default();
        let before = cfg.interval;
        apply_interval_override(&mut cfg, None).expect("absent interval is a no-op");
        assert_eq!(cfg.interval, before);
    }
}
