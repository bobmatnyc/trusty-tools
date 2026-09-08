//! `tcode`'s adoption of `trusty_common::log_drain::run_once` (#6537, Phase 5
//! of epic #6533).
//!
//! Why: trusty-mpm's own daemon already periodically uploads its logs
//! (#6535, `trusty_mpm::core::trusty_tools_config::log_drain` +
//! `trusty_mpm::daemon::log_drain`); `tcode serve` writes logs nobody
//! collects — before #6537 it had no file log at all (see
//! `crate::logging::file_log_dir`). This module is the same shape as
//! trusty-mpm's Phase 3 pair, scoped down to what a single-project daemon
//! actually needs: ONE destination, ONE source (tcode's own file log), and
//! the identity of the project `tcode serve` is bound to — trusty-mpm's
//! `sources[]` grouping (#6657) exists for a host draining several projects
//! at once, which `tcode serve` never does.
//!
//! What: [`config::LogDrainConfig`] is the on-disk shape
//! (`~/.trusty-code/log_drain.yaml`), using the same field names as
//! trusty-mpm's `log_drain:` section; [`resolve::resolve_log_drain`] turns it
//! into a runnable [`resolve::LogDrainSetting`]; [`scheduler::spawn`] starts
//! the interval loop from `serve::build_router` when the plan resolves
//! enabled. The drain CORE — `collect`, the manifest, `put`, `run_once` — is
//! `trusty_common::log_drain`; nothing here reimplements it.
//!
//! Test: `config::tests`, `resolve::tests`, `scheduler::tests`.

mod config;
mod resolve;
mod scheduler;

pub use config::LogDrainConfig;
pub use resolve::{LogDrainConfigError, LogDrainSetting, ResolvedLogDrain, resolve_log_drain};
pub use scheduler::spawn;
