//! `tm dbatg` — the PM ledger writer's CLI (#9758).
//!
//! Why: DOC-82 LDG-3.3 — the prototype's `datg.py` verbs with the same
//! arguments, stdout and exit codes, served by the daemon.
//! What: [`DbatgArgs`] (store selection, request id) and [`DbatgVerb`]:
//! `init`, `add`, `update`, `close`, `batch`, `prune`, `show`.
//! Test: `cli_parses_every_dbatg_verb`.

use std::path::PathBuf;

use clap::{Args, Subcommand};

/// `tm dbatg` arguments.
#[derive(Debug, Args)]
pub(crate) struct DbatgArgs {
    /// Project directory (default: the current directory); a worktree maps
    /// to its main checkout.
    #[arg(long, global = true)]
    pub project: Option<PathBuf>,
    /// The session whose store to use (default: `$DBATG_SESSION`, else the
    /// tmux session of `$TMUX_PANE`).
    #[arg(long, global = true)]
    pub session: Option<String>,
    /// Retry key: a batch sent again with the same id is applied once.
    #[arg(long, global = true)]
    pub request_id: Option<String>,
    /// The verb.
    #[command(subcommand)]
    pub verb: DbatgVerb,
}

/// One `tm dbatg` verb.
#[derive(Debug, Subcommand)]
pub(crate) enum DbatgVerb {
    /// Create this session's store with an id code (2-8 chars).
    Init {
        /// The id namespace, e.g. `th`.
        #[arg(long)]
        code: String,
    },
    /// Add a goal (G), decision (D), ask (A) or task (T).
    Add {
        /// G, D, A or T.
        kind: String,
        /// One line, at most 120 characters.
        #[arg(long)]
        title: String,
        /// The active goal it serves (D, A, T).
        #[arg(long)]
        goal: Option<String>,
        /// Goal rank (G).
        #[arg(long)]
        rank: Option<u32>,
        /// The finish line (G).
        #[arg(long)]
        finish: Option<String>,
        /// The Architect goal it maps to (G).
        #[arg(long)]
        maps: Option<String>,
        /// Comma list; a trailing `*` marks the recommendation (D, A).
        #[arg(long)]
        opts: Option<String>,
        /// Who a task is with (T).
        #[arg(long)]
        to: Option<String>,
        /// An explicit id.
        #[arg(long)]
        id: Option<String>,
    },
    /// Change a record.
    Update {
        /// The record id.
        id: String,
        /// One line, at most 120 characters.
        #[arg(long)]
        title: Option<String>,
        /// A status valid for the record kind.
        #[arg(long)]
        status: Option<String>,
        /// The owner's answer (D, A).
        #[arg(long)]
        answer: Option<String>,
        /// The verified outcome.
        #[arg(long)]
        outcome: Option<String>,
        /// `architect` to escalate, empty to clear (D, A).
        #[arg(long)]
        esc: Option<String>,
        /// Who a task is with (T).
        #[arg(long)]
        to: Option<String>,
        /// The log note.
        #[arg(long)]
        note: Option<String>,
        /// Goal rank (G).
        #[arg(long)]
        rank: Option<u32>,
    },
    /// Close a D, A or T with a verified outcome; it moves to `archive.jsonl`.
    Close {
        /// The record id.
        id: String,
        /// The verified outcome.
        #[arg(long)]
        outcome: String,
    },
    /// Apply a JSON list of ops, all or none: one write per turn.
    Batch {
        /// Read the ops from this file instead of stdin.
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Archive done items and tasks, and answered items older than 24 h.
    Prune {
        /// Name what would be archived; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Age, in hours, after which an answered item is archived.
        #[arg(long, default_value_t = 24)]
        answered_hours: u32,
        /// Archive an answered item only when it has an outcome.
        #[arg(long)]
        require_outcome: bool,
    },
    /// Print the store, or with `--brief` the one-line summary.
    Show {
        /// The summary line only.
        #[arg(long)]
        brief: bool,
    },
}
