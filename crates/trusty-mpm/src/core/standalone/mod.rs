//! Standalone managed `tm` driver — alias registry, global config, load, and run.
//!
//! Why: trusty-mpm's existing session launch path mutates the global `~/.claude`
//! on every invocation, making it unsafe to run alongside the user's own
//! claude-mpm setup. This module adds a `register/load/run` lifecycle that
//! operates entirely under `~/.trusty-mpm/` — never touching `~/.claude/` or
//! `~/.claude.json` — so the driver can replace claude-mpm for daily work
//! without collision (DOC-24).
//! What: four submodules — `registry` (alias→URL persistence), `global_config`
//! (ensure the one tm-global config dir exists), `load` (clone + prepare a
//! managed workspace), and `run` (launch claude with CLAUDE_CONFIG_DIR isolation).
//! Test: unit tests in each submodule's `#[cfg(test)]` block.

pub mod global_config;
pub mod load;
pub mod registry;
pub mod run;
