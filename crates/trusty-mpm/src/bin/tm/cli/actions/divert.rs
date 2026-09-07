//! `tm divert` command group — bulk-read diversion to a cheap worker (#6887).
//!
//! Why: `tm hook --divert-check` blocks an oversized read and names a command
//! for the agent to run instead. This is that command's argument surface.
//! What: [`DivertAction`] — currently just `bulk-read`.
//! Test: `cli_parses_divert_bulk_read` in `tests_behavior_a.rs`.

use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum DivertAction {
    /// Read files on a cheap worker model and print only the answer.
    ///
    /// The session's own context never sees the file bytes — that is the whole
    /// saving. The worker is headless Claude Code Haiku (`claude -p`) running
    /// under this session's own login; the model comes from the manifest's
    /// `[divert] worker_model`, exported as `TRUSTY_DIVERT_WORKER_MODEL`. No
    /// credential is read or written by this command.
    ///
    /// When no worker answers the command prints `divert: fall-through` and
    /// exits 3, which is the signal to re-read the file directly with
    /// `offset`/`limit` instead.
    #[command(name = "bulk-read")]
    BulkRead {
        /// Files to read. At least one.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// The question to answer about them. Defaults to a general summary.
        #[arg(long)]
        prompt: Option<String>,
        /// Seconds to wait for the worker before falling through.
        #[arg(
            long,
            default_value_t = crate::commands::divert_worker::DEFAULT_TIMEOUT_SECS
        )]
        timeout_secs: u64,
    },
}
