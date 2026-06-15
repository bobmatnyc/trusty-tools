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
use trusty_mpm::supervisor::{Supervisor, SupervisorConfig};

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
    if let Some(secs) = interval.filter(|s| *s > 0) {
        cfg.interval = std::time::Duration::from_secs(secs);
    }
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
        let model =
            std::env::var("TRUSTY_LLM_MODEL").unwrap_or_else(|_| "openai/gpt-4o-mini".to_owned());
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
