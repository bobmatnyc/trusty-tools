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
    Port {
        /// Emit host:port instead of the bare port number.
        #[arg(long)]
        addr: bool,

        /// Emit a JSON object `{"addr":"…","port":N}`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Parse `tctl version` — must succeed and select `Commands::Version`.
    #[test]
    fn parse_version() {
        let cli = Cli::try_parse_from(["tctl", "version"]).expect("version parses");
        assert!(matches!(cli.command, Commands::Version));
    }

    /// Parse `tctl version --json` — global flag propagates into `Cli.json`.
    #[test]
    fn parse_version_json() {
        let cli =
            Cli::try_parse_from(["tctl", "version", "--json"]).expect("version --json parses");
        assert!(cli.json);
        assert!(matches!(cli.command, Commands::Version));
    }

    /// Parse `tctl stack health`.
    #[test]
    fn parse_stack_health() {
        let cli = Cli::try_parse_from(["tctl", "stack", "health"]).expect("stack health parses");
        assert!(matches!(cli.command, Commands::Stack(StackCmd::Health)));
    }

    /// Parse `tctl stack doctor`.
    #[test]
    fn parse_stack_doctor() {
        let cli = Cli::try_parse_from(["tctl", "stack", "doctor"]).expect("stack doctor parses");
        assert!(matches!(
            cli.command,
            Commands::Stack(StackCmd::Doctor { member: None })
        ));
    }

    /// Parse `tctl stack doctor trusty-search`.
    #[test]
    fn parse_stack_doctor_member() {
        let cli = Cli::try_parse_from(["tctl", "stack", "doctor", "trusty-search"])
            .expect("stack doctor <m> parses");
        assert!(matches!(
            cli.command,
            Commands::Stack(StackCmd::Doctor { member: Some(_) })
        ));
    }

    /// Parse `tctl status`.
    #[test]
    fn parse_status() {
        let cli = Cli::try_parse_from(["tctl", "status"]).expect("status parses");
        assert!(matches!(cli.command, Commands::Status));
    }

    /// Parse `tctl updates`.
    #[test]
    fn parse_updates() {
        let cli = Cli::try_parse_from(["tctl", "updates"]).expect("updates parses");
        assert!(matches!(cli.command, Commands::Updates { latest: false }));
    }

    /// Parse `tctl updates --latest`.
    #[test]
    fn parse_updates_latest() {
        let cli =
            Cli::try_parse_from(["tctl", "updates", "--latest"]).expect("updates --latest parses");
        assert!(matches!(cli.command, Commands::Updates { latest: true }));
    }

    /// Parse `tctl upgrade`.
    #[test]
    fn parse_upgrade() {
        let cli = Cli::try_parse_from(["tctl", "upgrade"]).expect("upgrade parses");
        assert!(matches!(cli.command, Commands::Upgrade { .. }));
    }

    /// `tctl update` is a visible alias of `upgrade`.
    #[test]
    fn parse_update_alias() {
        let cli = Cli::try_parse_from(["tctl", "update"]).expect("update alias parses");
        assert!(matches!(cli.command, Commands::Upgrade { .. }));
    }

    /// Parse `tctl upgrade --check`.
    #[test]
    fn parse_upgrade_check() {
        let cli =
            Cli::try_parse_from(["tctl", "upgrade", "--check"]).expect("upgrade --check parses");
        assert!(matches!(cli.command, Commands::Upgrade { check: true, .. }));
    }

    /// Parse `tctl install`.
    #[test]
    fn parse_install() {
        let cli = Cli::try_parse_from(["tctl", "install"]).expect("install parses");
        assert!(matches!(cli.command, Commands::Install { .. }));
    }

    /// Parse `tctl install trusty-search trusty-memory`.
    #[test]
    fn parse_install_members() {
        let cli = Cli::try_parse_from(["tctl", "install", "trusty-search", "trusty-memory"])
            .expect("install members parses");
        if let Commands::Install { members } = &cli.command {
            assert_eq!(members, &["trusty-search", "trusty-memory"]);
        } else {
            panic!("expected Install");
        }
    }

    /// Parse `tctl ensure`.
    #[test]
    fn parse_ensure() {
        let cli = Cli::try_parse_from(["tctl", "ensure"]).expect("ensure parses");
        assert!(matches!(cli.command, Commands::Ensure { wait: false }));
    }

    /// Parse `tctl ensure --wait`.
    #[test]
    fn parse_ensure_wait() {
        let cli = Cli::try_parse_from(["tctl", "ensure", "--wait"]).expect("ensure --wait parses");
        assert!(matches!(cli.command, Commands::Ensure { wait: true }));
    }

    /// Parse `tctl start`.
    #[test]
    fn parse_start() {
        let cli = Cli::try_parse_from(["tctl", "start"]).expect("start parses");
        assert!(matches!(cli.command, Commands::Start { .. }));
    }

    /// Parse `tctl stop`.
    #[test]
    fn parse_stop() {
        let cli = Cli::try_parse_from(["tctl", "stop"]).expect("stop parses");
        assert!(matches!(cli.command, Commands::Stop { .. }));
    }

    /// Parse `tctl restart`.
    #[test]
    fn parse_restart() {
        let cli = Cli::try_parse_from(["tctl", "restart"]).expect("restart parses");
        assert!(matches!(cli.command, Commands::Restart { .. }));
    }

    /// Parse `tctl config`.
    #[test]
    fn parse_config() {
        let cli = Cli::try_parse_from(["tctl", "config"]).expect("config parses");
        assert!(matches!(cli.command, Commands::Config { .. }));
    }

    /// Parse `tctl port`.
    #[test]
    fn parse_port() {
        let cli = Cli::try_parse_from(["tctl", "port"]).expect("port parses");
        assert!(matches!(cli.command, Commands::Port { .. }));
    }

    /// Parse `tctl port --addr`.
    #[test]
    fn parse_port_addr() {
        let cli = Cli::try_parse_from(["tctl", "port", "--addr"]).expect("port --addr parses");
        assert!(matches!(cli.command, Commands::Port { addr: true, .. }));
    }

    /// Parse `tctl doctor --self-check trusty-search`.
    #[test]
    fn parse_doctor_self_check() {
        let cli = Cli::try_parse_from(["tctl", "doctor", "--self-check", "trusty-search"])
            .expect("doctor --self-check parses");
        assert!(matches!(
            cli.command,
            Commands::Doctor {
                self_check: true,
                member: Some(_)
            }
        ));
    }

    /// Parse `tctl ui`.
    #[test]
    fn parse_ui() {
        let cli = Cli::try_parse_from(["tctl", "ui"]).expect("ui parses");
        assert!(matches!(cli.command, Commands::Ui { .. }));
    }

    /// Generic passthrough: `tctl trusty-search doctor`.
    #[test]
    fn parse_passthrough() {
        let cli =
            Cli::try_parse_from(["tctl", "trusty-search", "doctor"]).expect("passthrough parses");
        if let Commands::Passthrough(args) = &cli.command {
            assert_eq!(args.as_slice(), &["trusty-search", "doctor"]);
        } else {
            panic!("expected Passthrough");
        }
    }

    /// Bare `tctl` with no subcommand must fail (arg_required_else_help).
    #[test]
    fn bare_tctl_fails() {
        assert!(Cli::try_parse_from(["tctl"]).is_err());
    }

    /// Global `--scope` propagates to nested subcommands.
    #[test]
    fn global_scope_propagates() {
        let cli = Cli::try_parse_from(["tctl", "--scope", "project", "stack", "health"])
            .expect("scope propagates");
        assert_eq!(cli.scope, Some(ScopeArg::Project));
    }

    /// Global `--timeout` propagates to nested subcommands.
    #[test]
    fn global_timeout_propagates() {
        let cli = Cli::try_parse_from(["tctl", "--timeout", "30", "version"])
            .expect("timeout propagates");
        assert_eq!(cli.timeout, Some(30));
    }

    /// `--yes` / `-y` are equivalent.
    #[test]
    fn yes_short_flag() {
        let long = Cli::try_parse_from(["tctl", "--yes", "restart"]).expect("--yes parses");
        let short = Cli::try_parse_from(["tctl", "-y", "restart"]).expect("-y parses");
        assert!(long.yes);
        assert!(short.yes);
    }
}
