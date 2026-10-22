//! `tm architect dashboard` — the Architect dashboard store's verbs (#10063).
//!
//! Why: DOC-82 LDG-3.3 — the prototype's `dashboard-db.py` verbs with its
//! arguments, over the same bodies as `mpm.architect.dashboard.*`.
//! What: [`DashboardArgs`] (store selection) and [`DashboardVerb`]: `get`,
//! `list`, `set`, `update`, `delete`, `batch`, `pruned`, `validate`, `fsck`,
//! `tally`, `footer`, `ids`.
//! Test: `cli_parses_architect_dashboard_verbs`.

use std::path::PathBuf;

use clap::{Args, Subcommand};

/// `tm architect dashboard` arguments.
#[derive(Debug, Args)]
pub(crate) struct DashboardArgs {
    /// Store root (default: `DASHBOARD_ROOT`, else
    /// `<Architect dir>/local/owner-queue`).
    #[arg(long, global = true)]
    pub root: Option<PathBuf>,
    /// `dir` or `jsonl` (default: `DASHBOARD_FORMAT`, else `dir`).
    #[arg(long, global = true)]
    pub format: Option<String>,
    /// The `by` of change and prune lines (default: `DASHBOARD_ACTOR`, else
    /// `architect`).
    #[arg(long, global = true)]
    pub actor: Option<String>,
    /// The prune log (default: `<root>/../owner-queue-archive/pruned.jsonl`).
    #[arg(long, global = true)]
    pub prune_log: Option<PathBuf>,
    /// The verb.
    #[command(subcommand)]
    pub verb: DashboardVerb,
}

/// One `tm architect dashboard` verb.
#[derive(Debug, Subcommand)]
pub(crate) enum DashboardVerb {
    /// Print one document as `{id, version, data}`.
    Get {
        /// Collection name.
        collection: String,
        /// Doc id.
        id: String,
    },
    /// Print a collection's documents, in id order.
    List {
        /// Collection name.
        collection: String,
        /// Keep documents with this `status`.
        #[arg(long)]
        status: Option<String>,
    },
    /// Replace or create a document; an existing one needs `--if-version`.
    Set {
        /// Collection name.
        collection: String,
        /// Doc id.
        id: String,
        /// The body: a file, `-` for stdin, or a literal starting `{`.
        #[arg(long, value_name = "FILE_OR_STRING")]
        json: String,
        /// Expected version; 0 means "must not exist". A stale one exits 1.
        #[arg(long)]
        if_version: Option<u64>,
    },
    /// Merge keys into an existing document.
    Update {
        /// Collection name.
        collection: String,
        /// Doc id.
        id: String,
        /// The keys: a file, `-` for stdin, or a literal starting `{`.
        #[arg(long, value_name = "FILE_OR_STRING")]
        json: String,
        /// Expected version. A stale one exits 1.
        #[arg(long)]
        if_version: Option<u64>,
    },
    /// Delete a document; its record goes to the prune log first.
    Delete {
        /// Collection name.
        collection: String,
        /// Doc id.
        id: String,
        /// Expected version. A stale one exits 1.
        #[arg(long)]
        if_version: Option<u64>,
    },
    /// Apply a JSON list of ops (or `{"ops": [...]}`), all or none.
    Batch {
        /// The ops file, or `-` for stdin.
        file: String,
    },
    /// Print prune-log lines, newest last.
    Pruned {
        /// Only this doc id.
        #[arg(long)]
        id: Option<String>,
        /// Only this collection.
        #[arg(long)]
        collection: Option<String>,
    },
    /// Check every document against the schemas; exit 1 on an error.
    Validate {
        /// The schema directory (default: `DASHBOARD_SCHEMA_DIR`, else
        /// `<Architect dir>/schemas/dashboard`).
        #[arg(long)]
        schemas: Option<PathBuf>,
    },
    /// Check the files against the change log and references; exit 1 on a problem.
    Fsck,
    /// Print the open-item counts line, the only source for counts.
    Tally {
        /// Print the counts as JSON.
        #[arg(long = "json")]
        as_json: bool,
    },
    /// Print the one-line reply footer, built from `tally`.
    Footer {
        /// Print the footer and counts as JSON.
        #[arg(long = "json")]
        as_json: bool,
        /// The loopback URL (default: `DASHBOARD_LOCAL_URL`, else
        /// `http://127.0.0.1:7890/`).
        #[arg(long)]
        local: Option<String>,
        /// The tailnet URL (default: `DASHBOARD_TAILNET_URL`; required).
        #[arg(long)]
        tailnet: Option<String>,
    },
    /// Print the next free id of a prefix, the free count and retire candidates.
    Ids {
        /// One capital letter, e.g. `D`.
        prefix: String,
    },
}
