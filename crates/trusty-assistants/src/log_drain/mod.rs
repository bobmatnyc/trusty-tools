//! `trusty-assistants`' adoption of `trusty_common::log_drain::run_once` (#6537,
//! Phase 5 of epic #6533).
//!
//! Why: trusty-mpm's own daemon already periodically uploads its logs
//! (#6535, `trusty_mpm::core::trusty_tools_config::log_drain` +
//! `trusty_mpm::daemon::log_drain`). `trusty-assistants` already has a file
//! appender — `service::daemon_log_path` (#4111), the daemon's own stderr
//! redirected to `~/Library/Logs/trusty-agents/daemon-<hash>-<port>.log` —
//! but nothing collects it. This module is the same shape as trusty-mpm's
//! Phase 3 pair, scoped down to what a single-project daemon actually needs:
//! ONE destination, ONE source (this daemon's own log directory), and the
//! identity of the project this process is bound to — trusty-mpm's
//! `sources[]` grouping (#6657) exists for a host draining several projects
//! at once, which one `trusty-assistants` daemon never does.
//!
//! What: [`LogDrainConfig`] is the `[log_drain]` section of
//! `~/.trusty-agents/config.toml` (same field names as trusty-mpm's
//! `log_drain:` section); [`resolve_log_drain`] turns it into a runnable
//! [`LogDrainSetting`]. #11632: the scheduler that ran it lived in the cut
//! `--api` daemon; the section still parses so a config round trip keeps it.
//!
//! Test: `resolve::tests`.
//!
//! [`LogDrainConfig`]: crate::log_drain::LogDrainConfig
//! [`resolve_log_drain`]: crate::log_drain::resolve_log_drain
//! [`LogDrainSetting`]: crate::log_drain::LogDrainSetting

mod resolve;

pub use resolve::{
    LogDrainConfig, LogDrainConfigError, LogDrainSetting, ResolvedLogDrain, resolve_log_drain,
};
