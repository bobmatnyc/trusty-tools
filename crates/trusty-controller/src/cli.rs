//! CLI argument definitions for `tctl`.
//!
//! Why: Centralising the clap derive structs here keeps `main.rs` as a pure
//! dispatcher and makes the surface testable without spawning a subprocess —
//! clap's `try_parse_from` works entirely in-process.
//!
//! What: Defines `Cli` (the root struct with global flags) and `Commands` (the
//! full Phase-0 subcommand enum), mirroring the design surface in DOC-5 §6.
//! Commands not yet fully implemented return a structured `not-yet-implemented`
//! result rather than panicking (see `commands::not_yet_implemented`).
//!
//! Test: `Cli::try_parse_from(["tctl","version"])` → `Ok(_)`;
//! `Cli::try_parse_from(["tctl","stack","health"])` → `Ok(_)`;
//! bare `tctl` with no subcommand → `Err` (arg_required_else_help = true).

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// `tctl` — thin control plane for the claude-mpm stack.
///
/// Why: Single entry point that installs, upgrades, restarts, and inspects
/// every member of the trusty-* / claude-mpm stack through a uniform contract.
///
/// What: Parses global flags and dispatches to the appropriate subcommand
/// handler. Every subcommand is either backed by a real implementation or
/// returns a structured `not-yet-implemented` stub (Phase 0).
///
/// Test: `tctl --help` prints the full command surface; `tctl version` prints
/// the Phase-0 version envelope; `tctl stack health` returns a stub rollup.
#[derive(Parser, Debug)]
#[command(
    name = "tctl",
    version,
    author,
    propagate_version = true,
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// Scope to act on (DOC-3 §3 default: `all` inside a project dir, else `system`).
    ///
    /// Honoured by scope-bearing commands; inert on `version`, `port`, etc.
    #[arg(long, value_enum, global = true)]
    pub scope: Option<ScopeArg>,

    /// Emit machine-readable JSON to stdout (DOC-5 §4.2).
    ///
    /// Passthrough commands emit the raw DOC-1 envelope; stack commands emit
    /// the DOC-4 rollup struct; `version` emits the capability-discovery object.
    #[arg(long, global = true)]
    pub json: bool,

    /// Per-tool probe deadline in seconds (DOC-4 §1.3: 2 s health / 10 s doctor default).
    #[arg(long, global = true)]
    pub timeout: Option<u64>,

    /// Non-interactive: skip the blast-radius confirmation (DOC-3 §5).
    ///
    /// Required for automation / CI. Non-TTY system-mutating ops without this
    /// flag abort with exit 3 (DOC-5 §3.3 / §4.3).
    #[arg(long, short = 'y', global = true)]
    pub yes: bool,

    /// Override the manifest path (DOC-2 §2 system override > embedded default).
    #[arg(long, global = true)]
    pub manifest: Option<PathBuf>,

    /// Increase output detail.
    ///
    /// On stack verbs: add per-check drill-down (DOC-4 §3.2). On daemon ops:
    /// raise log level on stderr. Repeat for more verbosity.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Commands,
}

/// CLI mirror of the DOC-1 D7 wire scope.
///
/// Why: A thin clap-facing enum avoids pulling `trusty_common::contract::Scope`
/// into clap's derive machinery while keeping the vocabulary identical.
///
/// What: Mirrors `Scope { Project, System, All }` from the DOC-1 contract module
/// (to be wired once that module lands in trusty-common).
///
/// Test: `ScopeArg::from_str("project")` == `Ok(ScopeArg::Project)` via ValueEnum.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum ScopeArg {
    /// Per-project state only (indexes, palaces).
    Project,
    /// Machine-wide daemon layer only.
    System,
    /// Both layers (default inside a project dir).
    All,
}

/// All `tctl` subcommands.
///
/// Why: A single enum over the entire DOC-5 §1.1 command tree lets the
/// dispatcher in `main.rs` be a simple `match` with no ambient state.
///
/// What: Covers the Phase-0 subset (version, stack health/doctor, status,
/// updates, upgrade, install, ensure, start, stop, restart, config, port,
/// doctor --self-check, ui) plus the `external_subcommand` passthrough.
/// Full implementations land in later phases; stubs return structured
/// `NotYetImplemented` results.
///
/// Test: Every variant must be reachable via `Cli::try_parse_from`; see
/// `tests::cli` in `cli.rs`.
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Install the stack (or named members) — system scope. (DOC-8)
    ///
    /// Idempotent: already-installed members are no-ops. Requires cargo and
    /// (for the orchestrator) uv on PATH.
    Install {
        /// Specific member(s) to install; omit for all enabled members.
        members: Vec<String>,
    },

    /// Upgrade the stack (or named members) to the BOM-pinned versions, then restart. (DOC-9)
    ///
    /// `tctl update` is a visible alias. Non-interactive: add `--yes` or `-y`.
    #[command(visible_alias = "update")]
    Upgrade {
        /// Specific member(s) to upgrade; omit for all enabled members.
        members: Vec<String>,

        /// List what would change without actually upgrading (`tctl updates`).
        #[arg(long)]
        check: bool,

        /// Upgrade to the latest published version regardless of the BOM pin.
        #[arg(long)]
        latest: bool,

        /// Do not upgrade `tctl` itself; upgrade other members only.
        #[arg(long)]
        exclude_self: bool,
    },

    /// List available updates + changelog headlines — read-only. (DOC-9 / DOC-2 §5)
    ///
    /// Never mutates. Renders installed vs BOM-pinned version per member and
    /// changelog headlines between them.
    Updates {
        /// Show latest published version (crates.io) instead of the BOM pin.
        #[arg(long)]
        latest: bool,
    },

    /// Idempotent ensure-project pass; no-op when already set up. (DOC-8)
    ///
    /// Patches `.mcp.json`, registers the search index, creates the memory
    /// palace, and (optionally) waits for full readiness.
    Ensure {
        /// Block until the project index reaches `fresh` (for CI / DOC-10).
        #[arg(long)]
        wait: bool,
    },

    /// Start member daemon(s) — system scope.
    Start {
        /// Specific member(s) to start; omit for all daemons.
        members: Vec<String>,
    },

    /// Stop member daemon(s) — system scope.
    Stop {
        /// Specific member(s) to stop; omit for all daemons.
        members: Vec<String>,
    },

    /// Restart all daemons + the controller UI service — system scope. (DOC-5 §7)
    ///
    /// Uses SIGTERM (graceful drain) rather than SIGKILL (#534). Non-interactive:
    /// add `--yes`.
    Restart {
        /// Specific member(s) to restart; omit for all.
        members: Vec<String>,
    },

    /// Stack-wide rollup verbs (DOC-4).
    #[command(subcommand)]
    Stack(StackCmd),

    /// Read-only effective merged config for each member (secrets redacted). (DOC-3 §7)
    Config {
        /// Specific member(s); omit for all.
        members: Vec<String>,
    },

    /// One-line stack summary (verdict + stack version). (DOC-4 sugar)
    Status,

    /// Print the controller's own bound port/address — clean stdout. (DOC-7)
    ///
    /// Output format precedence (highest → lowest):
    ///   1. `--json-port`  →  `{"addr":"…","port":N}` (DOC-7 / trusty-search port --json pattern)
    ///   2. `--addr`       →  `host:port` string
    ///   3. (default)      →  bare integer port
    ///
    /// The global `--json` flag and `--json-port` are *semantically* overlapping
    /// (both produce JSON on stdout) but with different shapes.  When both appear
    /// on the same invocation, `--json-port` takes precedence and `--json` is
    /// ignored for this command.  This is enforced at runtime in `commands::port::run`
    /// rather than by clap because `--json` is a `global = true` flag and clap's
    /// `conflicts_with` does not fire across subcommand boundaries for global flags.
    Port {
        /// Emit host:port instead of the bare port number.
        #[arg(long)]
        addr: bool,

        /// Emit a JSON object `{"addr":"…","port":N}`.
        ///
        /// Precedence over the global `--json` flag: when both are provided,
        /// `--json-port` wins and the port-specific JSON shape is emitted.
        #[arg(long)]
        json_port: bool,
    },

    /// Controller-side conformance self-check of a member. (DOC-6 §8)
    ///
    /// Validates that the named member speaks the DOC-1 contract envelope,
    /// advertises `verbs[]`, and correctly redacts secrets.
    Doctor {
        /// Run the conformance self-check rather than a stack doctor.
        #[arg(long)]
        self_check: bool,

        /// Member to self-check (required when `--self-check` is set).
        member: Option<String>,
    },

    /// Print or open the controller web-UI URL. (DOC-7)
    Ui {
        /// Print the URL without launching a browser.
        #[arg(long)]
        print: bool,
    },

    /// `tctl version` — print tctl's own version + embedded stack_version + contract floor.
    ///
    /// With `--json`: emits the capability-discovery object (DOC-1 D3b / DOC-5 §4.2).
    Version,

    /// Generic passthrough: `tctl <tool> <verb> [args]` — any advertised verb. (DOC-1 D3c)
    ///
    /// The first token is the manifest member id; the remainder is forwarded as
    /// `<binary> <verb> [args] --scope <S> --json`. The controller validates
    /// the member id against the manifest and the verb against the member's
    /// advertised `verbs[]` before invoking.
    #[command(external_subcommand)]
    Passthrough(Vec<String>),
}

/// Stack-wide rollup subcommands (under `tctl stack`).
///
/// Why: `stack` nesting disambiguates the whole-stack rollup from a per-member
/// op reached via the passthrough (DOC-5 §1.4).
///
/// What: `stack health` — fast liveness sweep; `stack doctor` — deep diagnostic
/// sweep. Both render the DOC-4 tools×scope matrix + verdict.
///
/// Test: `tctl stack health` parses to `Commands::Stack(StackCmd::Health)`;
/// `tctl stack doctor` parses to `Commands::Stack(StackCmd::Doctor { member: None })`.
#[derive(Subcommand, Debug)]
pub enum StackCmd {
    /// Fast liveness sweep → tools×scope matrix + verdict. (DOC-4)
    Health,

    /// Deep diagnostic sweep → matrix + drill-down + remediation. (DOC-4)
    Doctor {
        /// Scope the sweep to a single named member.
        member: Option<String>,
    },
}

// ── Unit tests ───────────────────────────────────────────────────────────────
// Tests are in a sibling file to keep this definition file under the 500-line cap.
#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
