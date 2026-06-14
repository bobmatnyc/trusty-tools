//! Claude Code configuration analyzer (I/O side).
//!
//! Why: `trusty-mpm-core::claude_config` defines the pure data model and path
//! resolution; the daemon owns the filesystem reads, the recommendation logic,
//! and the apply/restart actions. Keeping the I/O here preserves `core`'s
//! purity while still letting trusty-mpm inspect and improve a project's
//! Claude Code setup.
//! What: [`ClaudeConfigAnalyzer`] reads + merges the settings files, produces
//! [`ConfigRecommendation`]s, and applies them; [`ClaudeCodeRestarter`] finds
//! running `claude` processes and restarts Claude Code inside a tmux session;
//! [`ConfigCheckpointer`] snapshots and restores config files;
//! [`ProfileDeployer`] deploys named configuration presets.
//! Test: `cargo test -p trusty-mpm-daemon claude_config` covers reading,
//! analysis, and apply against temp directories (no real `~/.claude` touched).

mod analyzer;
mod checkpointer;
mod profile;
mod restarter;

#[cfg(test)]
mod tests;

pub use analyzer::ClaudeConfigAnalyzer;
pub use checkpointer::ConfigCheckpointer;
pub use profile::ProfileDeployer;
pub use restarter::ClaudeCodeRestarter;
