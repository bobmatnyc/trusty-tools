//! `tm divert` command group — bulk-read diversion to a cheap worker (#6887).
//!
//! Why: `tm hook --divert-check` blocks an oversized read and names a command
//! for the agent to run instead. This is that command's argument surface.
//! What: [`DivertAction`] — currently just `bulk-read`.
//! Test: `cli_parses_divert_bulk_read` in `tests.rs`.

use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum DivertAction {
    /// Read files on a cheap worker model and print only the answer.
    ///
    /// The session's own context never sees the file bytes — that is the whole
    /// saving. The worker model and provider come from the manifest's
    /// `[divert]` section, exported into the session as `TRUSTY_DIVERT_*`;
    /// credentials are resolved from the environment by this process, never
    /// written into `.claude/settings.json`.
    ///
    /// When no provider is reachable the command prints `divert: fall-through`
    /// and exits 3, which is the signal to re-read the file directly with
    /// `offset`/`limit` instead.
    #[command(name = "bulk-read")]
    BulkRead {
        /// Files to read. At least one.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// The question to answer about them. Defaults to a general summary.
        #[arg(long)]
        prompt: Option<String>,
        /// Cap on the worker's answer length.
        #[arg(long, default_value_t = 2048)]
        max_tokens: u32,
    },
}
