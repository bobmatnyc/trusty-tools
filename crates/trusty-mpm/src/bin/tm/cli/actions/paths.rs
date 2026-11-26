//! `tm paths show|set` — the install's host paths (#11821 P6b).
//!
//! What: [`PathsAction`]: `show` prints the resolved paths and their sources;
//! `set` writes `~/.trusty-tools/install.toml`, owner-only.
//! Test: `cli_parses_paths_show_and_set`.

use std::path::PathBuf;

use clap::Subcommand;

/// One `tm paths` verb.
#[derive(Debug, Subcommand)]
pub(crate) enum PathsAction {
    /// Print projects_root and config_dir with their source.
    ///
    /// Sources: env, install-file, config-template or default. A broken
    /// install.toml prints its error and exits non-zero.
    Show,
    /// Write install.toml: absolute paths only, from an operator terminal.
    ///
    /// Refused inside a Claude Code session or agent, and while any tm
    /// session is live. Nothing is moved: move the data yourself.
    Set {
        /// The root managed projects are cloned under.
        #[arg(long)]
        projects_root: Option<PathBuf>,
        /// The directory holding `.trusty-tools` and `.trusty-mpm`.
        #[arg(long)]
        config_dir: Option<PathBuf>,
    },
}
