//! `tm il config` — the IL schedule's verbs (#11001, ADR-0077 D3).
//!
//! What: [`IlAction`] (one group, `config`) and [`IlConfigAction`]: `show`,
//! `enroll`, `remove`, `set`, `reset`.
//! Test: `cli_parses_every_il_config_verb`.

use std::path::PathBuf;

use clap::Subcommand;

/// One `tm il` group.
#[derive(Debug, Subcommand)]
pub(crate) enum IlAction {
    /// Show and edit the IL schedule, `<architect>/local/il-schedule.json`.
    Config {
        /// The Architect directory (default: the recorded Architect).
        #[arg(long, global = true)]
        dir: Option<String>,
        /// The verb.
        #[command(subcommand)]
        action: IlConfigAction,
    },
}

/// One `tm il config` verb; the CLI is the schedule's only writer.
#[derive(Debug, Subcommand)]
pub(crate) enum IlConfigAction {
    /// Print the schedule; the built-in preset when the file is absent.
    Show,
    /// Enroll a session for IL clearing.
    Enroll {
        /// The tmux session name.
        session: String,
        /// The project directory holding `.trusty-mpm/datg/`.
        #[arg(long)]
        project: PathBuf,
        /// The IL id code, 2-8 letters or digits.
        #[arg(long)]
        code: Option<String>,
    },
    /// Remove an enrolled session.
    Remove {
        /// The tmux session name.
        session: String,
    },
    /// Set `thresholds.<key>`, or a session field with `--session`.
    ///
    /// Session fields: project, code, signals (comma list of S1-S5),
    /// state_path, thresholds.<key>.
    Set {
        /// The key, e.g. `thresholds.floor_ctx` or `signals`.
        key: String,
        /// The new value.
        value: String,
        /// Edit this enrolled session instead of the global thresholds.
        #[arg(long)]
        session: Option<String>,
    },
    /// Restore the built-in preset; an edited file needs `--force`.
    Reset {
        /// Replace a file that differs from the preset.
        #[arg(long)]
        force: bool,
    },
}
