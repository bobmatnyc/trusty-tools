//! `trusty-agents`' adoption of `trusty_common::log_drain::run_once` (#6537,
//! Phase 5 of epic #6533).
//!
//! Why: trusty-mpm's own daemon already periodically uploads its logs
//! (#6535, `trusty_mpm::core::trusty_tools_config::log_drain` +
//! `trusty_mpm::daemon::log_drain`). `trusty-agents` already has a file
//! appender — `service::daemon_log_path` (#4111), the daemon's own stderr
//! redirected to `~/Library/Logs/trusty-agents/daemon-<hash>-<port>.log` —
//! but nothing collects it. This module is the same shape as trusty-mpm's
//! Phase 3 pair, scoped down to what a single-project daemon actually needs:
//! ONE destination, ONE source (this daemon's own log directory), and the
//! identity of the project this process is bound to — trusty-mpm's
//! `sources[]` grouping (#6657) exists for a host draining several projects
//! at once, which one `trusty-agents` daemon never does.
//!
//! What: [`LogDrainConfig`] is the `[log_drain]` section of
//! `~/.trusty-agents/config.toml` (same field names as trusty-mpm's
//! `log_drain:` section); [`resolve_log_drain`] turns it into a runnable
//! [`LogDrainSetting`]; [`spawn`] starts the interval loop from
//! `api::server::routes::serve_with_config`, beside `listeners::poll::spawn_listeners`.
//! The drain CORE — `collect`, the manifest, `put`, `run_once` — is
//! `trusty_common::log_drain`; nothing here reimplements it.
//!
//! Test: `resolve::tests`, `scheduler::tests`.

mod resolve;
mod scheduler;

pub use resolve::{
    LogDrainConfig, LogDrainConfigError, LogDrainSetting, ResolvedLogDrain, resolve_log_drain,
};
pub use scheduler::spawn;
