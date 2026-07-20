//! CLI argument definitions for `trusty-installer` (alias: `tctl`).
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
//! Test: `Cli::try_parse_from(["trusty-installer","version"])` → `Ok(_)`;
//! `Cli::try_parse_from(["trusty-installer","stack","health"])` → `Ok(_)`;
//! bare `trusty-installer` with no subcommand → `Err` (arg_required_else_help = true).
//! The `tctl` alias binary resolves to the same binary and accepts the same args.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// `trusty-installer` — thin control plane for the claude-mpm stack.
///
/// Why: Single entry point that installs, upgrades, restarts, and inspects
/// every member of the trusty-* / claude-mpm stack through a uniform contract.
///
/// What: Parses global flags and dispatches to the appropriate subcommand
/// handler. Every subcommand is either backed by a real implementation or
/// returns a structured `not-yet-implemented` stub (Phase 0). Also available
/// as `tctl` (transitional alias — ADR-0013 / SPEC-INSTALLER-01 Phase 1).
///
/// Test: `trusty-installer --help` prints the full command surface;
/// `trusty-installer version` prints the Phase-0 version envelope;
/// `trusty-installer stack health` returns a stub rollup.
#[derive(Parser, Debug)]
#[command(
    name = "trusty-installer",
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

/// CLI value for `trusty-installer up --analyze-core` (DOC-12 §3.5).
///
/// Why: A clap-facing enum keeps the `--analyze-core blocking|background|skip`
/// surface declarative while the orchestrator works in terms of
/// `commands::up::config::AnalyzeMode`; `commands::up_dispatch` bridges the two.
///
/// What: Mirrors the §3.5 tri-state.
///
/// Test: `cli_tests::parse_up_analyze_core` round-trips each value.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum AnalyzeCoreArg {
    /// Wait for the core-analysis snapshot before proceeding.
    Blocking,
    /// Run the snapshot in the background, non-gating (default).
    Background,
    /// Omit the boot-analysis pass entirely.
    Skip,
}

/// CLI value for `trusty-installer sign <target>` (#2558).
///
/// Why: A clap-facing enum keeps the `sign` surface declarative and
/// self-documenting (`--help` lists the two valid targets) while
/// `commands::sign::run` and `commands::macos_signing` work in terms of the
/// plain `&str` set names (`SEARCH_SET` / `MPM_SET`) so that module stays
/// clap-independent.
///
/// What: `Search` → the `trusty-search` + `trusty-embedderd` set; `Mpm` → the
/// `trusty-mpm` set (owner-authorized scope extension, #2558).
///
/// Test: `cli_tests::parse_sign` round-trips each value.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum SignTargetArg {
    /// `trusty-search` + its bundled `trusty-embedderd`.
    #[value(name = "trusty-search")]
    Search,
    /// `trusty-mpm`.
    #[value(name = "trusty-mpm")]
    Mpm,
}

impl SignTargetArg {
    /// The plain set name `commands::macos_signing` expects.
    ///
    /// Why: Bridges the clap enum to the plain-`&str` set vocabulary used by
    /// `macos_signing::binaries_for_set` / `sign_set_strict`, so that module
    /// has no clap dependency.
    /// What: `Search` → `"trusty-search"`, `Mpm` → `"trusty-mpm"`.
    /// Test: `cli_tests::parse_sign` (indirectly, via the round-trip).
    pub fn as_set_name(self) -> &'static str {
        match self {
            SignTargetArg::Search => "trusty-search",
            SignTargetArg::Mpm => "trusty-mpm",
        }
    }
}

/// All `trusty-installer` subcommands.
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
    /// `trusty-installer up` — boot the whole stack: always-on core → analyze → console → opt-in mpm. (DOC-12)
    ///
    /// The first-loaded control plane's boot orchestrator. Idempotent and
    /// restart-safe: a second `up` on a healthy stack is an all-no-op. Brings
    /// memory + search to healthy (gated), runs a background boot analysis,
    /// starts trusty-console (printing its URL), and — when opted in via
    /// `--with-mpm` / config / interactive-yes — starts trusty-mpm and ensures
    /// Claude Code is present and at latest (prompting before any upgrade).
    Up {
        /// Force the opt-in orchestrator (trusty-mpm) ON (§4).
        #[arg(long, conflicts_with = "no_mpm")]
        with_mpm: bool,

        /// Force the opt-in orchestrator (trusty-mpm) OFF (§4).
        #[arg(long)]
        no_mpm: bool,

        /// Boot-analysis mode over the core crates (§3.5). Default: background.
        #[arg(long, value_enum)]
        analyze_core: Option<AnalyzeCoreArg>,

        /// Block STAGE 5 ensure-project until the index reaches `fresh` (§3.5).
        #[arg(long)]
        wait: bool,

        /// Skip the Claude Code upgrade step even when stale (§6).
        #[arg(long)]
        skip_claude_upgrade: bool,
    },

    /// Install the stack (or named members) — system scope. (DOC-8)
    ///
    /// Idempotent: already-installed members are no-ops. Requires cargo and
    /// (for the orchestrator) uv on PATH.
    Install {
        /// Specific member(s) to install; omit for all enabled members.
        members: Vec<String>,

        /// Install binaries only — skip the post-install launchd service
        /// bootstrap (#2556). Also settable via `TCTL_NO_SERVICE_BOOTSTRAP`.
        #[arg(long)]
        no_service: bool,

        /// Preview what would be installed (members, binaries, planned service
        /// actions) without installing anything (#2112). Exits 0. Safe to run
        /// in any context — TTY, non-TTY, or `--json` — and never prompts.
        /// Always bypasses the interactive component picker for determinism:
        /// with no members named, previews the FULL stable set (not a
        /// TTY-driven subset) — same as omitting `--dry-run` in a non-TTY
        /// context.
        #[arg(long)]
        dry_run: bool,

        /// Allow the trusty-mpm supervisor bootstrap to replace an
        /// already-registered launchd supervisor with an older-or-equal
        /// version (#3527 downgrade guard override). Has no effect on any
        /// other member or check.
        #[arg(long)]
        force: bool,

        /// Skip the post-install `ensure` + `stack health` verify tail
        /// (#2560). The install itself (binaries + service bootstrap) still
        /// runs; only the final automated verification pass is skipped.
        #[arg(long)]
        no_verify: bool,
    },

    /// Upgrade the stack (or named members) to the BOM-pinned versions, then restart. (DOC-9)
    ///
    /// `trusty-installer update` is a visible alias. Non-interactive: add `--yes` or `-y`.
    #[command(visible_alias = "update")]
    Upgrade {
        /// Specific member(s) to upgrade; omit for all enabled members.
        members: Vec<String>,

        /// List what would change without actually upgrading (`trusty-installer updates`).
        #[arg(long)]
        check: bool,

        /// Upgrade to the latest published version regardless of the BOM pin.
        #[arg(long)]
        latest: bool,

        /// Do not upgrade `trusty-installer` itself; upgrade other members only.
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

    /// Restart all daemons + the installer UI service — system scope. (DOC-5 §7)
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

    /// Read-only effective merged config for each member (secrets redacted).
    /// (DOC-3 §7) Also hosts the `keys` sub-feature (#2405).
    Config(ConfigArgs),

    /// One-line stack summary (verdict + stack version). (DOC-4 sugar)
    Status,

    /// Print a member daemon's bound port/address — clean stdout. (DOC-7)
    ///
    /// `trusty-installer` does not host its own HTTP server; this reports the bound
    /// address of a managed daemon read from its `http_addr` discovery file (via
    /// `trusty_common::read_daemon_addr`). Defaults to `trusty-search` when no
    /// member is named.
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
        /// Member daemon whose port to report (defaults to `trusty-search`).
        member: Option<String>,

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

    /// Installer-side conformance self-check of a member. (DOC-6 §8)
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

    /// Print or open the installer web-UI URL. (DOC-7)
    Ui {
        /// Print the URL without launching a browser.
        #[arg(long)]
        print: bool,
    },

    /// `trusty-installer version` — print own version + embedded stack_version + contract floor.
    ///
    /// With `--json`: emits the capability-discovery object (DOC-1 D3b / DOC-5 §4.2).
    Version,

    /// Update `trusty-installer` itself to the latest published version.
    ///
    /// Attempts a prebuilt download; falls back to `cargo install --locked` when
    /// no prebuilt is available. Prints a restart hint on success — does NOT
    /// re-exec the updated binary. Use `--yes` to skip the crates.io version check.
    SelfUpdate,

    /// Developer-ID codesign a signable binary set — macOS only. (#2558)
    ///
    /// The single source of truth for the FDA/App-Data-TCC persistent-signing
    /// story: `tctl install` already runs this automatically (fail-soft) after
    /// `trusty-search` / `trusty-mpm` land, but this standalone verb lets an
    /// operator (re-)sign binaries built from local source
    /// (`cargo install --path …`) directly, and is what
    /// `scripts/install-trusty-search-signed.sh` shells out to instead of
    /// duplicating `codesign` flags in bash. Hard-fails (non-zero exit) when
    /// no Developer ID Application certificate is available or signing fails —
    /// unlike the fail-soft `tctl install` hook, this command is the operator
    /// explicitly asking for signing to happen.
    ///
    /// The raw identity-probe output and which identity was selected can be
    /// printed via the global `-v`/`--verbose` flag (#2939) — a diagnostic aid
    /// for "no identity found" failures despite a valid keychain cert.
    Sign {
        /// Which signable set to sign.
        #[arg(value_enum)]
        target: SignTargetArg,

        /// Directory containing the binaries (defaults to `$CARGO_HOME/bin` /
        /// `~/.cargo/bin`).
        #[arg(long)]
        dir: Option<PathBuf>,
    },

    /// Generic passthrough: `trusty-installer <tool> <verb> [args]` — any advertised verb. (DOC-1 D3c)
    ///
    /// The first token is the manifest member id; the remainder is forwarded as
    /// `<binary> <verb> [args] --scope <S> --json`. The installer validates
    /// the member id against the manifest and the verb against the member's
    /// advertised `verbs[]` before invoking.
    #[command(external_subcommand)]
    Passthrough(Vec<String>),
}

/// Stack-wide rollup subcommands (under `trusty-installer stack`).
///
/// Why: `stack` nesting disambiguates the whole-stack rollup from a per-member
/// op reached via the passthrough (DOC-5 §1.4).
///
/// What: `stack health` — fast liveness sweep; `stack doctor` — deep diagnostic
/// sweep. Both render the DOC-4 tools×scope matrix + verdict.
///
/// Test: `trusty-installer stack health` parses to `Commands::Stack(StackCmd::Health)`;
/// `trusty-installer stack doctor` parses to `Commands::Stack(StackCmd::Doctor { member: None })`.
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

/// Args for `trusty-installer config` (DOC-3 §7 + #2405).
///
/// Why: `config` already means "read-only effective merged config for each
/// stack member" here — a different domain than the universal
/// inference-provider credential CLI (epic #2400). Rather than mount the
/// whole-grammar `ConfigCommand` (which would collide on the `config` node),
/// this nests only the embeddable `keys` feature as an optional subcommand
/// alongside the pre-existing bare `members` positional, so both grammars
/// coexist: `trusty-installer config [members...]` (unchanged) and
/// `trusty-installer config keys <verb>` (new).
/// What: `members` is the pre-existing bare-positional member filter;
/// `action` is `Some(ConfigSubcommand::Keys(..))` only when the first token
/// after `config` is literally `keys` — clap's derive tries subcommand
/// matching before falling back to positional collection, and no stack
/// member is named `keys`, so the two grammars never collide in practice.
/// Test: `cli_tests::parse_config` (bare + members unchanged),
/// `cli_tests::parse_config_keys` (new `keys` nesting).
#[derive(Debug, clap::Args)]
pub struct ConfigArgs {
    /// Specific member(s); omit for all. Not used together with `keys`.
    pub members: Vec<String>,

    #[command(subcommand)]
    pub action: Option<ConfigSubcommand>,
}

/// The `trusty-installer config` sub-feature layer (#2405).
///
/// Why: keeps room for future `config` sub-features without touching
/// [`ConfigArgs`]'s bare-members grammar.
/// What: today only `keys` — the universal inference-provider credential CLI
/// (`set`/`list`/`test`/`unset`), nested via trusty-common's embeddable
/// `ConfigKeysCommand`.
/// Test: `cli_tests::parse_config_keys`.
#[derive(Debug, Subcommand)]
pub enum ConfigSubcommand {
    /// Manage inference provider configuration (API keys) — the universal
    /// `config keys set/list/test/unset` surface shared by every trusty-*
    /// binary (epic #2400 Wave 1, #2405).
    Keys(trusty_common::inference::config::ConfigKeysCommand),
}

// ── Unit tests ───────────────────────────────────────────────────────────────
// Tests are in a sibling file to keep this definition file under the 500-line cap.
#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
