//! CLI argument definitions for the `trusty-mpm` / `tm` binary.
//!
//! Why: keeping all clap struct/enum definitions in one file makes it easy to
//! audit the full CLI surface without wading through handler logic.
//! What: defines `Cli`, `Command`, and all sub-command enums.
//! Test: each variant is exercised by the `cli_parses_*` unit tests in
//! `tests.rs`.

use std::net::SocketAddr;

use clap::{Parser, Subcommand};

/// Default daemon URL when `--url` / `TRUSTY_MPM_URL` is unset.
///
/// Why (issue #1268): this must be the *same* address the daemon binds to by
/// default, or the client probes a port nothing is listening on and reports
/// "daemon: unreachable". It is re-exported from the single source of truth in
/// `core::discovery` (which also drives the daemon `--addr` default) so the two
/// sides can never drift.
/// What: alias of [`trusty_mpm::core::DEFAULT_DAEMON_URL`].
/// Test: `core::discovery::default_url_matches_addr` proves URL and addr agree.
pub(crate) const DEFAULT_URL: &str = trusty_mpm::core::DEFAULT_DAEMON_URL;

/// Default daemon bind address for the `daemon --addr` flag (issue #1268).
///
/// Why: keeps the daemon's bind default tied to the same constant that drives
/// the client's [`DEFAULT_URL`], so `tm start` and the thin CLI always agree.
/// What: alias of [`trusty_mpm::core::DEFAULT_DAEMON_ADDR`].
/// Test: `core::discovery::default_addr_parses`.
pub(crate) const DEFAULT_ADDR: &str = trusty_mpm::core::DEFAULT_DAEMON_ADDR;

/// trusty-mpm command-line interface.
#[derive(Debug, Parser)]
#[command(name = "trusty-mpm", version, about = "trusty-mpm — unified binary")]
pub(crate) struct Cli {
    /// Base URL of the trusty-mpm daemon (used by the thin CLI subcommands).
    #[arg(long, env = "TRUSTY_MPM_URL", default_value = DEFAULT_URL, global = true)]
    pub(crate) url: String,

    /// Subcommand to run.
    #[command(subcommand)]
    pub(crate) command: Command,
}

/// Top-level CLI subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Show daemon and session status.
    Status,
    /// Start the daemon if not running, or show status if already running.
    Start,
    /// Alias for `start` — start the daemon if not running, no-op if it is.
    ///
    /// Why: matches the trusty-search / trusty-memory CLI surface so users
    /// moving between the three daemons get the same `start` / `serve` /
    /// `stop` triad. With `--stdio` it becomes the MCP stdio bridge (#1221):
    /// a thin proxy that forwards JSON-RPC to the daemon's loopback `POST /rpc`,
    /// mirroring `trusty-memory serve --stdio`. This is the form wired into
    /// `.mcp.json`. Without `--stdio` it behaves like `start`. (The older
    /// `tm daemon --mcp` direct-stdio mode is retained for diagnostics.)
    Serve {
        /// Run as an MCP stdio bridge that proxies to the daemon's `POST /rpc`.
        ///
        /// Why: Claude Code speaks MCP over stdio; the durable daemon speaks
        /// HTTP. This flag selects the bridge that connects the two, auto-starting
        /// the daemon if needed and reconnecting with backoff if it restarts.
        #[arg(long)]
        stdio: bool,
    },
    /// Stop every running trusty-mpm daemon process.
    ///
    /// Why: pairs with `start` so operators can take the daemon down
    /// without a `restart` cycle. Uses `sysinfo` to find the daemon by
    /// name + argv, sends SIGTERM, polls 5s, then SIGKILLs stragglers.
    Stop,
    /// Stop the running daemon and start a fresh one.
    Restart,
    /// Define and manage projects (registered working directories).
    Project {
        /// Project action to perform.
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Define and manage Claude Code sessions within a project.
    Session {
        /// Session action to perform.
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Show the recent hook-event feed.
    Events,
    /// Run a full system diagnostic of the trusty-mpm stack.
    Doctor,
    /// Launch the ratatui multi-session TUI dashboard.
    Tui {
        /// Base URL of the trusty-mpm daemon.
        #[arg(long, env = "TRUSTY_MPM_URL", default_value = DEFAULT_URL)]
        url: String,
        /// Poll interval in milliseconds.
        #[arg(long, default_value_t = 1000)]
        interval_ms: u64,
    },
    /// Launch the coordinator TUI: an input box over a live session list (#1272).
    ///
    /// Why (DOC-13): a Claude-Code-like surface — a text input on top of a live
    /// session list (controller bullet + one row per managed session, the active
    /// row in two columns `[id] │ [summary]`). This is a NEW screen, distinct
    /// from the existing `tm tui` dashboard. It is exposed under its own name
    /// because `coordinator` already names the session-manager chat command
    /// (`tm coordinator` / `tm sm`); a `-tui` suffix avoids that collision.
    /// What: Child #2 polls the daemon's coordinator-context endpoint live on the
    /// `--interval-ms` cadence (self-healing the daemon URL on failure) and maps
    /// each session into the list; slash-command dispatch lands in later children.
    /// `--url` is resolved via `resolve_daemon_url`.
    /// Test: `cli_parses_coordinator_tui` in `tests.rs`.
    CoordinatorTui {
        /// Base URL of the trusty-mpm daemon.
        #[arg(long, env = "TRUSTY_MPM_URL")]
        url: Option<String>,
        /// Poll interval in milliseconds (daemon refresh cadence).
        #[arg(long, default_value_t = 1500)]
        interval_ms: u64,
    },
    /// Launch the Tauri desktop GUI (or open the web build in the browser
    /// when Tauri is unavailable).
    Gui,
    /// Manage the Telegram remote-management bot (pair, status, start, stop).
    Telegram {
        /// Telegram action to perform.
        #[command(subcommand)]
        cmd: TelegramCmd,
    },
    /// Install the bundled framework artifacts to `~/.trusty-mpm/framework/`.
    Install {
        /// Overwrite artifacts that already exist on disk.
        #[arg(long)]
        force: bool,
    },
    /// Handle a Claude Code lifecycle hook (PreToolUse / PostToolUse / Stop).
    ///
    /// Why: `tm install` registers `trusty-mpm hook` as Claude Code's
    /// `PreToolUse`, `PostToolUse`, and `Stop` hook command. Claude Code
    /// invokes this binary on every tool call so the daemon can drive the
    /// circuit breaker, audit log, and dashboard. The handler short-circuits
    /// when `CLAUDE_MPM_SUB_AGENT=1` is set in the environment — that env var
    /// is stamped onto every MPM-spawned sub-agent process to keep nested
    /// agents from generating their own hook traffic.
    /// What: reads minimal context from Claude Code environment variables,
    /// posts a `hook_event` to the running daemon, and exits 0. Daemon
    /// failures and missing env vars degrade silently so a hook firing during
    /// a daemon restart never blocks the user's prompt.
    /// Test: `cli_parses_hook` plus the inline `hook_guard_short_circuits`.
    Hook,
    /// Run the trusty-mpm daemon.
    Daemon {
        /// Address the daemon HTTP API binds to.
        #[arg(long, env = "TRUSTY_MPM_ADDR", default_value = DEFAULT_ADDR)]
        addr: SocketAddr,
        /// Also expose the daemon on the Tailscale interface for remote access.
        #[arg(long, env = "TRUSTY_MPM_TAILSCALE")]
        tailscale: bool,
        /// Run as an MCP server over stdio instead of the HTTP daemon.
        #[arg(long)]
        mcp: bool,
    },
    /// Run the unattended fleet supervisor (24/7 observer + auto-resumer, #1206).
    ///
    /// Why: for overnight / unattended operation the fleet needs an always-on
    /// process that auto-resumes `stopped` sessions, observes session health
    /// without a live caller, surfaces pending decisions, and exposes fleet
    /// metrics — all while making NO autonomy decisions itself.
    /// What: runs the supervisor loop, polling the managed-session store on an
    /// interval. Auto-resume is gated by `TRUSTY_MPM_AUTO_RESUME=1`; the poll
    /// cadence, classification toggle, and metrics address are read from env
    /// (`TRUSTY_MPM_SUPERVISOR_*`) but may be overridden by these flags. Serves
    /// `/metrics` + `/health` for fleet observability.
    /// Test: `cli_parses_supervisor`.
    Supervisor {
        /// Address the supervisor's `/metrics` + `/health` server binds to.
        ///
        /// Overrides `TRUSTY_MPM_SUPERVISOR_ADDR` when supplied.
        #[arg(long, env = "TRUSTY_MPM_SUPERVISOR_ADDR")]
        addr: Option<SocketAddr>,
        /// Poll interval in seconds (overrides `TRUSTY_MPM_SUPERVISOR_INTERVAL`).
        #[arg(long)]
        interval: Option<u64>,
        /// Force auto-resume of `stopped` sessions on (overrides
        /// `TRUSTY_MPM_AUTO_RESUME`). Without this flag (and without the env
        /// var) the supervisor runs observe-only.
        #[arg(long)]
        auto_resume: bool,
        /// Disable idle-session activity classification (no LLM calls).
        #[arg(long)]
        no_classify: bool,
    },
    /// Launch a session with full setup: deploys instructions, agents, and
    /// skills, then starts Claude.
    ///
    /// This runs the full `prepare_session` deployment sequence (instructions,
    /// agents, skills, MCP config) into the project before starting or
    /// attaching to the session in the current terminal (behaves like running
    /// `claude-mpm`).
    Launch {
        /// Project directory to launch in (defaults to the current directory).
        dir: Option<String>,
    },
    /// Start or attach to a session without running the deployment sequence.
    ///
    /// Unlike `launch`, this skips the framework-deployment sequence entirely —
    /// it does not deploy instructions, agents, or skills. It only starts the
    /// tmux-hosted session (idempotent: creates it when absent, attaches when it
    /// already exists) and hands the terminal over to it.
    Connect {
        /// Project directory to connect in (defaults to the current directory).
        dir: Option<String>,
    },
    /// Attach to an existing session by ID, name prefix, or project path.
    /// Opens the TUI focused on the matched session.
    Attach {
        /// Session ID, name prefix, or project directory path.
        target: String,

        /// Print session JSON and exit (no TUI).
        #[arg(long)]
        json: bool,
    },
    /// Inspect or configure the token-use optimizer.
    Optimizer {
        /// Optimizer action to perform.
        #[command(subcommand)]
        action: OptimizerAction,
    },
    /// Inspect the session overseer.
    Overseer {
        /// Overseer action to perform.
        #[command(subcommand)]
        action: OverseerAction,
    },
    /// Send a message to the cross-session coordinator / session manager.
    ///
    /// A message prefixed with `@session:` routes a command at that session;
    /// a plain message is answered by the session manager (or the legacy LLM
    /// chat assistant when the SM is disabled) with full session context.
    ///
    /// DOC-14 D0.2: `tm sm` and `tm session-manager` are visible aliases for
    /// `tm coordinator` — all reach the same code path. `coord` is a hidden alias.
    ///
    /// DOC-14 SM-STDIO (#1291): `tm sm serve --stdio` runs the SM JSON-RPC 2.0
    /// over STDIO adapter — the primary, API-first headless drive surface
    /// (`sm.chat`/`sm.goals.*`/`sm.sessions.*`/`sm.context.get`/`sm.health`). A
    /// plain `tm sm <message>` still chats; the `serve` subcommand is the adapter.
    #[command(
        visible_alias = "sm",
        visible_alias = "session-manager",
        alias = "coord"
    )]
    Coordinator {
        /// The message to send to the coordinator / session manager (omit when
        /// using the `serve` subcommand).
        message: Option<String>,
        /// Optional subcommand: `serve --stdio` runs the SM JSON-RPC stdio adapter.
        #[command(subcommand)]
        action: Option<CoordinatorAction>,
    },

    /// Inspect and probe workspace service daemons.
    ///
    /// Why: agents need a single canonical interface to answer "is trusty-search
    /// running?", "what port is it on?", "is it healthy?" without resorting to
    /// lsof/curl/ps. `tm services` reads the manifest at ~/.claude-mpm/services.yaml
    /// (or the embedded default when the file is absent) and probes each declared
    /// service on demand.
    /// What: eight subcommands (list, status, port, url, health, log, init, restart)
    /// with --json where applicable. Exit codes: 0=running/healthy, 1=down/unhealthy,
    /// 2=unknown service, per the spec.
    /// Test: `cli_parses_services_list`, `cli_parses_services_status`,
    /// `cli_parses_services_port`, `cli_parses_services_url`,
    /// `cli_parses_services_health`, `cli_parses_services_log`,
    /// `cli_parses_services_init`, `cli_parses_services_restart`.
    Services {
        /// Services action to perform.
        #[command(subcommand)]
        action: ServicesAction,
    },

    /// Recover from corrupt or inconsistent deploy state.
    ///
    /// Why: a crash between writing a content file and updating the manifest
    /// can leave stale `.tmp` orphans; a disk-full or interrupted write can
    /// leave the manifest itself corrupt. `tm repair` detects these conditions
    /// and offers targeted remediation without touching user-owned files.
    /// What: `tm repair deploy` removes stale `.tmp` orphans from
    /// `~/.claude/agents/` and `~/.claude/skills/`, validates both manifests,
    /// and prints actionable guidance when corruption is found. A `--force`
    /// flag resets a corrupt agent manifest to empty (which triggers a full
    /// re-deploy on the next `tm install`).
    /// Test: `cli_parses_repair_deploy`.
    Repair {
        /// Repair action to perform.
        #[command(subcommand)]
        action: RepairAction,
    },

    /// Sync and inspect the claude-mpm agent/skill catalog.
    ///
    /// Why: the session-manager MVP deploys agents and skills sourced from the
    /// authoritative claude-mpm repository; `tm catalog` keeps the local cache
    /// current and lets operators inspect what is available.
    /// What: `sync` fetches (or refreshes) the catalog under
    /// `~/.trusty-mpm/catalog/`; `ls` lists the cached agents and skills.
    /// Test: `cli_parses_catalog_sync`, `cli_parses_catalog_ls`.
    Catalog {
        /// Catalog action to perform.
        #[command(subcommand)]
        action: CatalogAction,
    },

    /// One-shot issue → branch → PR → close workflow (#1237).
    ///
    /// Why: packages the manual issue-resolution loop (validate the issue,
    /// branch off the default branch in an isolated worktree, drive an agent to
    /// implement it, post audit comments, open a PR that closes the issue on
    /// merge) into a single invocation. Reuses the session-manager managed-spawn
    /// path and the #842 driver agent/skill.
    /// What: validates `<issue#>` via the selected `[system]` backend (`gh`
    /// default; `jira`/`linear` are not-yet-supported stubs), posts any `--note`
    /// text as issue comments, derives a `<type>/<issue#>-<slug>` branch from the
    /// issue title/labels, and spawns a managed session whose task = "address
    /// issue #<n>: <title>" so the driver implements the change and opens the PR.
    /// Test: `cli_parses_ticket`, `cli_parses_ticket_with_system`,
    /// `cli_parses_ticket_with_notes` in `tests.rs`; orchestration logic in the
    /// `commands::ticket` unit tests.
    Ticket {
        /// Issue reference to resolve (e.g. `1232` or `#1232`).
        issue: String,
        /// Ticket backend: `gh` (default), `jira`, or `linear`.
        #[arg(default_value = "gh", value_enum)]
        system: crate::commands::ticket::system::TicketSystemKind,
        /// Note to post as an issue comment for the audit trail (repeatable).
        #[arg(long = "note", short = 'm')]
        notes: Vec<String>,
        /// Runtime backend for the spawned session: `claude-code` (default) or
        /// `tcode`.
        #[arg(long, default_value = "claude-code", value_enum)]
        runtime: trusty_mpm::runtime::RuntimeKind,
    },

    /// YAML-configurable issue state-management (labels/transitions/assignee).
    ///
    /// Why: externalizes the (formerly hardcoded) issue state machine — the
    /// label set, the allowed transitions, and the assignee model — into a YAML
    /// contract owned by trusty-mpm (#1246). The Unicorn Factory consumes these
    /// verbs by shelling out; the YAML is the portable shared contract. Every
    /// verb maps to a concrete GitHub label/assignee/comment mutation so issue
    /// state stays reconstructable from GitHub artifacts alone.
    /// What: a verb group (`seed-labels`, `transition`, `current`, `states`,
    /// `seed-config`, `repair`) over the selected `[system]` backend (`gh`
    /// default; `jira`/`linear` are not-yet-supported stubs). Config discovery:
    /// `--config` > `./issue-state.yaml` > `~/.trusty-tools/trusty-mpm/
    /// issue-state.yaml` > the embedded Unicorn Factory default.
    /// Test: `cli_parses_issue_*` in `tests.rs`; verb logic in the
    /// `commands::issue` submodule unit tests.
    Issue {
        /// Issue verb to run.
        #[command(subcommand)]
        cmd: IssueCmd,
        /// Ticket backend: `gh` (default), `jira`, or `linear`.
        #[arg(long, default_value = "gh", value_enum, global = true)]
        system: crate::commands::ticket::system::TicketSystemKind,
    },

    /// Watch a board for label-routed issues and dispatch them autonomously.
    ///
    /// Why: `tm ticket <issue#>` executes ONE hand-named issue. `tm watch`
    /// complements it with a board-watch mode: discover every issue carrying a
    /// routing label (default `tm-agent`) and dispatch each into the SAME
    /// managed-session execution path so a team can drop the label on an issue
    /// and have it picked up. Routing is by LABEL, not assignee (no bot account
    /// exists). `poll` runs once (cron-friendly); `listen` polls on an interval
    /// until Ctrl-C.
    /// What: a `poll`/`listen` sub-subcommand group. Both default to DRY-RUN —
    /// they only spawn real work with the explicit `--execute` flag. `<project>`
    /// is an `owner/repo` or a name resolved via the `watch:` config section.
    /// Test: `cli_parses_watch_*` in `tests.rs`; logic in `commands::watch`.
    Watch {
        /// Watch mode to run (`poll` one-shot or `listen` loop).
        #[command(subcommand)]
        cmd: WatchCmd,
    },

    /// Standalone metaharness — PM + sub-agent delegation without the daemon (#1045).
    ///
    /// Why: the M1 POC (issue #1045) builds a self-contained metaharness that
    /// boots without the trusty-mpm daemon or the `claude` CLI, driving PM →
    /// sub-agent delegation in-process via trusty-code (Seam A). The `meta`
    /// command group is the operator entry point for that harness. WI-1 lands
    /// only the bootstrap skeleton: argument parsing, structured logging, and a
    /// placeholder run summary. The live delegation loop, fs/bash tool wiring,
    /// and OpenRouter inference arrive in WI-2..WI-8 and are intentionally
    /// deferred here.
    /// What: a `run` sub-subcommand that accepts `--demo` (run the bundled demo
    /// task) and `--project <PATH>` (the working directory the harness operates
    /// in). For WI-1 the handler validates its arguments, emits a structured
    /// "bootstrap" JSON summary to stdout, and exits 0.
    /// Test: `cli_parses_meta_run`, `cli_parses_meta_run_demo`,
    /// `cli_parses_meta_run_project`, `cli_meta_requires_action` in `tests.rs`.
    Meta {
        /// Metaharness action to run.
        #[command(subcommand)]
        action: MetaAction,
    },
}

/// Verbs for the `tm meta` standalone-metaharness command group (#1045, WI-1).
///
/// Why: `meta` is a group rather than a bare command because future work items
/// will add sibling verbs (e.g. inspecting transcripts under
/// `.trusty-mpm/meta-runs/`); modelling it as a sub-subcommand enum from the
/// start keeps that surface extensible without a breaking CLI change.
/// What: a single `Run` variant carrying the `--demo` flag and the optional
/// `--project <PATH>` working-directory argument.
/// Test: `cli_parses_meta_run`, `cli_parses_meta_run_demo`,
/// `cli_parses_meta_run_project`, `cli_meta_requires_action` in `tests.rs`.
#[derive(Debug, Subcommand)]
pub(crate) enum MetaAction {
    /// Boot the metaharness for a single run (WI-1: bootstrap skeleton only).
    ///
    /// Why: this is the harness's primary entry point — eventually it boots the
    /// PM, delegates to a sub-agent, captures a transcript, and exits. For WI-1
    /// it only validates inputs and prints a bootstrap summary so the scaffold
    /// is exercisable end-to-end before the real runtime lands.
    /// What: `--demo` selects the bundled demo task; `--project <PATH>` sets the
    /// working directory the harness runs against (defaults to the process cwd).
    /// Test: `cli_parses_meta_run*` in `tests.rs`; handler behaviour in the
    /// `commands::meta` unit tests.
    Run {
        /// Run the bundled demo task instead of a user-supplied task.
        #[arg(long)]
        demo: bool,

        /// Working directory the metaharness operates in (defaults to the cwd).
        #[arg(long)]
        project: Option<std::path::PathBuf>,
    },
}

/// Modes for the `tm watch` command group.
///
/// Why: `poll` and `listen` share every flag but differ in lifecycle (one-shot
/// vs loop); a sub-subcommand enum keeps them discoverable and individually
/// parseable while reusing one flag set via the flattened [`WatchArgs`].
/// What: `Poll` (discover + dispatch once, then exit) and `Listen` (repeatedly
/// poll on an interval, processing newly-matched issues, until Ctrl-C).
/// Test: `cli_parses_watch_poll`, `cli_parses_watch_listen` in `tests.rs`.
/// Subcommands for `tm coordinator` / `tm sm` (DOC-14 SM-STDIO #1291).
///
/// Why: `tm sm` chats by default (a plain message), but the SM's PRIMARY,
/// API-first interface is the JSON-RPC over STDIO adapter (§1A.1). Exposing it as
/// a `serve` subcommand under the `sm` alias matches the trusty-* `serve --stdio`
/// convention while leaving `tm sm <message>` for ad-hoc chat.
/// What: one variant, `Serve { stdio }`, mirroring the daemon's `serve --stdio`
/// flag shape. `--stdio` selects the newline-delimited JSON-RPC adapter; without
/// it the subcommand prints guidance (the HTTP/TUI surfaces are separate).
/// Test: `cli_parses_sm_serve_stdio` in `tests.rs`.
#[derive(Debug, Subcommand)]
pub(crate) enum CoordinatorAction {
    /// Run the SM JSON-RPC 2.0 over STDIO adapter (the headless drive surface).
    Serve {
        /// Speak newline-delimited JSON-RPC 2.0 on stdin/stdout (logs to stderr).
        ///
        /// Why: the SM's API-first surface — a parent `claude-mpm`/PM drives every
        /// `sm.*` method headlessly over stdio (§1A.2). Without this flag there is
        /// no other `serve` mode yet, so it is effectively required.
        #[arg(long)]
        stdio: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum WatchCmd {
    /// One-shot: list label-matched issues, dispatch each, then exit.
    Poll {
        /// Shared watch flags (project + label/state/safety/runtime).
        #[command(flatten)]
        args: WatchArgs,
    },
    /// Long-running: poll on `--interval-secs`, dispatching new issues each cycle.
    Listen {
        /// Shared watch flags (project + label/interval/state/safety/runtime).
        #[command(flatten)]
        args: WatchArgs,
    },
}

/// The shared flag set for both `tm watch poll` and `tm watch listen`.
///
/// Why: poll and listen take the same inputs (board, routing label, issue state,
/// the dry-run/execute safety gate, and the spawn runtime), plus listen's poll
/// interval. Flattening one struct into both keeps the surface identical and the
/// dispatch wiring DRY.
/// What: the `<project>` positional (`owner/repo` or a configured name) and the
/// `--label` / `--interval-secs` / `--state` / `--dry-run` / `--execute` /
/// `--runtime` flags. Safety: default is dry-run; `--execute` is required to
/// actually spawn work, and `--dry-run` (the default) always wins if both appear.
/// Test: `cli_parses_watch_*` assert the parsed fields.
#[derive(Debug, clap::Args)]
pub(crate) struct WatchArgs {
    /// Board to watch: an `owner/repo` (e.g. `bobmatnyc/trusty-tools`) OR a
    /// registered project name resolved via the `watch:` config section.
    pub(crate) project: String,

    /// Routing label; only issues carrying it are picked up (default `tm-agent`).
    #[arg(long)]
    pub(crate) label: Option<String>,

    /// `listen`-mode poll interval in seconds (default 60).
    #[arg(
        long = "interval-secs",
        help = "Poll interval in seconds (listen mode only; ignored by poll)"
    )]
    pub(crate) interval_secs: Option<u64>,

    /// Which issues to consider: `open` (default) or `all`.
    #[arg(long, value_enum)]
    pub(crate) state: Option<crate::commands::watch::github::IssueState>,

    /// List matched issues and what WOULD run without spawning anything.
    ///
    /// This is the DEFAULT behaviour; the flag exists to make the safe intent
    /// explicit and, if both `--dry-run` and `--execute` are passed, dry-run wins.
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Actually spawn a managed session per matched issue (the explicit opt-in).
    ///
    /// Without this flag, `tm watch` only describes what it would do. This guard
    /// makes accidental mass-execution against real repos impossible.
    #[arg(long)]
    pub(crate) execute: bool,

    /// Runtime backend for spawned sessions: `claude-code` (default) or `tcode`.
    #[arg(long, default_value = "claude-code", value_enum)]
    pub(crate) runtime: trusty_mpm::runtime::RuntimeKind,
}

/// Verbs for the `tm issue` state-management command group (#1246).
///
/// Why: each operation (seed labels, transition state, inspect, repair) is a
/// distinct, scriptable verb; a sub-subcommand enum keeps them discoverable and
/// individually parseable.
/// What: `SeedLabels` (idempotent create-missing), `Transition` (validated
/// atomic state change), `Current` (read state from labels), `States` (list the
/// model), `SeedConfig` (write the default YAML to disk), `Repair` (resolve a
/// multi-state issue).
/// Test: `cli_parses_issue_*` in `tests.rs`.
#[derive(Debug, Subcommand)]
pub(crate) enum IssueCmd {
    /// Create any missing labels (states + extra families) in the repo.
    SeedLabels {
        /// Explicit path to an issue-state YAML (overrides discovery).
        #[arg(long)]
        config: Option<std::path::PathBuf>,
        /// Print what would be created without creating anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Move an issue to `<to-state>`, validating the edge against the model.
    Transition {
        /// Issue number (e.g. `1232`).
        issue: u64,
        /// Target state name (e.g. `approved`).
        to_state: String,
        /// Explicit path to an issue-state YAML (overrides discovery).
        #[arg(long)]
        config: Option<std::path::PathBuf>,
        /// Optional note appended to the transition audit comment.
        #[arg(long)]
        note: Option<String>,
    },
    /// Report an issue's current state, derived from its labels.
    Current {
        /// Issue number.
        issue: u64,
        /// Explicit path to an issue-state YAML (overrides discovery).
        #[arg(long)]
        config: Option<std::path::PathBuf>,
    },
    /// List the configured states and transitions (reads YAML only).
    States {
        /// Explicit path to an issue-state YAML (overrides discovery).
        #[arg(long)]
        config: Option<std::path::PathBuf>,
    },
    /// Write the embedded default model to the user config path.
    SeedConfig {
        /// Overwrite an existing user config file.
        #[arg(long)]
        force: bool,
    },
    /// Resolve a mid-transition issue carrying multiple state labels.
    Repair {
        /// Issue number.
        issue: u64,
        /// Explicit path to an issue-state YAML (overrides discovery).
        #[arg(long)]
        config: Option<std::path::PathBuf>,
    },
}

/// Actions for the `catalog` subcommand.
///
/// Why: catalog management splits into a remote-sync operation and a local
/// listing; separate sub-actions keep each scriptable.
/// What: `Sync` fetches the catalog (respecting a TTL unless `--force`); `Ls`
/// lists cached agents and skills.
/// Test: `cli_parses_catalog_sync`, `cli_parses_catalog_ls`.
#[derive(Debug, Subcommand)]
pub(crate) enum CatalogAction {
    /// Fetch or refresh the agent/skill catalog from the claude-mpm repo.
    Sync {
        /// Force a fetch even if the cache TTL has not expired.
        #[arg(long)]
        force: bool,
    },
    /// List the cached agents and skills.
    Ls {
        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
}

/// Actions for the `repair` subcommand.
///
/// Why: scoped sub-actions keep `tm repair` extensible — future variants can
/// cover other deploy artefacts without changing the top-level command surface.
/// What: currently only `Deploy` is defined; it covers agent and skill manifests.
/// Test: `cli_parses_repair_deploy`.
#[derive(Debug, Subcommand)]
pub(crate) enum RepairAction {
    /// Repair the agent/skill deploy state in `~/.claude/`.
    ///
    /// Why: a crash during `tm install` or `tm session start` may leave stale
    /// `.tmp` staging files in `~/.claude/agents/` or `~/.claude/skills/`, or
    /// leave either manifest corrupt. This command removes the orphans and
    /// validates both manifests.
    /// What: for each target directory (`~/.claude/agents/`, skill subdirs under
    /// `~/.claude/skills/`), removes `*.tmp` orphans and validates the manifest.
    /// With `--force`, resets a corrupt agent manifest to empty so the next
    /// `tm install` performs a fresh full deploy.
    /// Test: `cli_parses_repair_deploy`.
    Deploy {
        /// Reset a corrupt manifest to empty (triggers full re-deploy on next
        /// `tm install`). Without this flag, a corrupt manifest is reported
        /// but not modified.
        #[arg(long)]
        force: bool,
    },
}

/// Actions for the `telegram` subcommand.
///
/// Why: every Telegram-related operation — pairing, status, and lifecycle —
/// now lives under one `tm telegram <subcommand>` group instead of scattered
/// top-level commands, so the bot's controls are discoverable in one place.
/// What: `Pair` requests a one-time pairing code; `Status` reports the daemon's
/// paired/unpaired state; `Start` runs the bot process in the foreground;
/// `Stop` kills a running bot process.
/// Test: `cli_parses_telegram_pair`, `cli_parses_telegram_status`,
/// `cli_parses_telegram_start`, `cli_parses_telegram_stop`.
#[derive(Debug, Subcommand)]
pub(crate) enum TelegramCmd {
    /// Request a one-time pairing code for the Telegram bot.
    Pair,
    /// Show Telegram bot pairing status.
    Status,
    /// Start the Telegram bot process.
    Start {
        /// Base URL of the trusty-mpm daemon.
        #[arg(long, env = "TRUSTY_MPM_URL", default_value = DEFAULT_URL)]
        url: String,
        /// Telegram bot token. When omitted, resolved from `.env.local` /
        /// `.env` / the `TELEGRAM_BOT_TOKEN` environment variable.
        #[arg(long)]
        token: Option<String>,
        /// Chat id to push unsolicited alerts to.
        #[arg(long)]
        alert_chat_id: Option<i64>,
        /// Restrict the bot to this Telegram user id.
        #[arg(long)]
        allowed_user_id: Option<i64>,
        /// Validate configuration and exit without connecting to Telegram.
        #[arg(long)]
        check: bool,
    },
    /// Stop the Telegram bot process if running.
    Stop,
}

/// Actions for the `project` subcommand.
#[derive(Debug, Subcommand)]
pub(crate) enum ProjectAction {
    /// Register a working directory as a trusty-mpm project.
    Init {
        /// Directory to register (defaults to the cwd).
        #[arg(long)]
        dir: Option<String>,
    },
    /// List all registered projects with their status.
    List,
    /// Show the current project's registered info and config.
    Info {
        /// Project directory (defaults to the cwd).
        #[arg(long)]
        dir: Option<String>,
    },
}

/// Actions for the `session` subcommand.
#[derive(Debug, Subcommand)]
pub(crate) enum SessionAction {
    /// Start a new Claude Code session in the current/specified project.
    Start {
        /// Project directory for the new session (defaults to the cwd).
        #[arg(long)]
        dir: Option<String>,
    },
    /// Stop a session by id or friendly name (managed or project session).
    ///
    /// Managed-aware (#1218): if the id/name is a managed session, this stops its
    /// runtime (workspace preserved, resumable); otherwise it stops the local
    /// project session.
    Stop {
        /// Session id or friendly name (e.g. `tmpm-quiet-falcon`).
        id_or_name: String,
    },
    /// List sessions for the current project.
    List {
        /// Project directory (defaults to the cwd).
        #[arg(long)]
        dir: Option<String>,
    },
    /// Reap dead sessions for the current project.
    Clean {
        /// Project directory (defaults to the cwd).
        #[arg(long)]
        dir: Option<String>,
    },
    /// Show detailed info for a specific session.
    Info {
        /// Session id or friendly name.
        id_or_name: String,
    },
    /// Print the composed launch instructions a session would receive.
    Instructions {
        /// Project directory to compose instructions for (defaults to the cwd).
        #[arg(long)]
        dir: Option<String>,
    },
    /// Show the recent hook-event feed for one session.
    Events {
        /// Session id or friendly name.
        id_or_name: String,
    },
    /// Show every agent's circuit-breaker state.
    Breakers,
    /// Pause a running session, saving state for later resume.
    Pause {
        /// Session id or friendly name.
        id_or_name: String,
        /// Short note about where you left off.
        #[arg(long)]
        note: Option<String>,
    },
    /// Resume a stopped/paused session (managed or project session).
    ///
    /// Managed-aware (#1218): if the id/name is a managed session, this re-spawns
    /// its runtime in the existing workspace; otherwise it resumes the local
    /// paused project session.
    Resume {
        /// Session id or friendly name.
        id_or_name: String,
    },
    /// Send a command to a session's tmux pane.
    Run {
        /// Session id or friendly name.
        id_or_name: String,
        /// Command to send.
        command: String,
        /// Summarize the output before printing (uses the Summarise level).
        #[arg(long)]
        summarize: bool,
    },
    /// Capture the current output of a session's tmux pane.
    Output {
        /// Session id or friendly name.
        id_or_name: String,
        /// Number of lines to capture (default 50).
        #[arg(long, default_value_t = 50)]
        lines: u32,
        /// Summarize the output before printing (uses the Summarise level).
        #[arg(long)]
        summarize: bool,
    },
    /// Spawn a new managed session from a repo + ref (session-manager MVP).
    ///
    /// Why: the session-manager MVP provisions an isolated workspace from a git
    /// repo and starts a harness in it; `tm session new` is the operator-facing
    /// entry point that posts to `POST /api/v1/sessions/managed`.
    /// What: posts repo, ref, task, and an optional name hint to the daemon.
    /// Test: `cli_parses_session_new`.
    New {
        /// Repository URL to provision the session from.
        repo: String,
        /// Git branch or ref to check out.
        #[arg(long, default_value = "main")]
        git_ref: String,
        /// Human-readable task description.
        #[arg(long)]
        task: String,
        /// Optional name hint for the tmux session.
        #[arg(long)]
        name_hint: Option<String>,
        /// Runtime backend for the session: `claude-code` (default) or `tcode`.
        ///
        /// `claude-code` runs Claude Code over OAuth (the default, unchanged
        /// behavior); `tcode` runs trusty-code against the direct Anthropic API
        /// (the `ANTHROPIC_API_KEY` path). Typed as [`RuntimeKind`] (a
        /// `clap::ValueEnum`) so an unsupported value is rejected at parse time
        /// with a "possible values" hint, not silently forwarded to the daemon.
        #[arg(long, default_value = "claude-code", value_enum)]
        runtime: trusty_mpm::runtime::RuntimeKind,
    },
    /// List managed sessions (session-manager MVP).
    ///
    /// Why: operators need to see every managed session and its pending decision.
    /// What: GETs `/api/v1/sessions/managed` and renders a table or JSON.
    /// Test: `cli_parses_session_ls`.
    Ls {
        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Show recent activity for a managed session.
    ///
    /// Why: operators inspect what a session is doing without attaching.
    /// What: GETs `/api/v1/sessions/managed/{id}` and prints its summary.
    /// Test: `cli_parses_session_activity`.
    Activity {
        /// Managed session id.
        id: String,
    },
    /// Inject text into a managed session's pane.
    ///
    /// Why: send a message to the harness without attaching to tmux.
    /// What: POSTs `/api/v1/sessions/managed/{id}/send`.
    /// Test: `cli_parses_session_send`.
    Send {
        /// Managed session id.
        id: String,
        /// Text to inject.
        text: String,
    },
    /// Answer a managed session's pending decision.
    ///
    /// Why: resolve a decision the harness is blocked on.
    /// What: POSTs `/api/v1/sessions/managed/{id}/answer`.
    /// Test: `cli_parses_session_answer`.
    Answer {
        /// Managed session id.
        id: String,
        /// Answer text.
        answer: String,
    },
    /// Print the tmux attach command for a managed session.
    ///
    /// Why: operators need the exact `tmux attach` command to take over a pane.
    /// What: GETs `/api/v1/sessions/managed/{id}/attach-cmd`.
    /// Test: `cli_parses_session_attach`.
    Attach {
        /// Managed session id.
        id: String,
    },
    /// [DEPRECATED] Stop a managed session's runtime — use `stop` instead.
    ///
    /// Why: legacy verbose verb retained for backward compatibility (#1205).
    /// Invoking it prints a deprecation notice and behaves like `stop`
    /// (runtime-stop semantics: workspace preserved, resumable).
    /// What: POSTs `/api/v1/sessions/managed/{id}/runtime-stop` after the notice.
    /// Test: `cli_parses_session_managed_stop`.
    #[command(hide = true)]
    ManagedStop {
        /// Managed session id.
        id: String,
    },
    /// [DEPRECATED] Stop a managed session's runtime — use `stop` instead.
    ///
    /// Why: `runtime-stop` was renamed to `stop` in #1205; the old spelling
    /// still parses but emits a deprecation notice steering operators to `stop`.
    /// What: POSTs `/api/v1/sessions/managed/{id}/runtime-stop` after the notice.
    /// Test: `cli_parses_session_runtime_stop`.
    #[command(hide = true)]
    RuntimeStop {
        /// Managed session id.
        id: String,
    },
    /// [DEPRECATED] Resume a stopped managed session — use `resume` instead.
    ///
    /// Why: `managed-resume` was renamed to `resume` in #1205; the old spelling
    /// still parses but emits a deprecation notice steering operators to `resume`.
    /// What: POSTs `/api/v1/sessions/managed/{id}/resume` after the notice.
    /// Test: `cli_parses_session_managed_resume`.
    #[command(hide = true)]
    ManagedResume {
        /// Managed session id.
        id: String,
    },
    /// Decommission a managed session: stop runtime + remove workspace from disk.
    ///
    /// Why: the ONLY operation that removes the workspace directory. Unlike
    /// `runtime-stop`, decommission is terminal — no further resume is possible.
    /// A tombstone record is kept for `ls` history.
    /// What: POSTs `/api/v1/sessions/managed/{id}/decommission`.
    /// Test: `cli_parses_session_decommission`.
    Decommission {
        /// Managed session id.
        id: String,
    },
    /// Reclaim idle managed sessions: stop idle, decommission done (#1313).
    ///
    /// Why: paused orchestration sessions leave behind idle SM tmux sessions that
    /// consume claude Max rate-limit slots. This enumerates managed sessions,
    /// reads each one's latest activity-monitor verdict, and applies the locked
    /// policy — `idle` → stop (resumable), `done` → decommission, everything else
    /// (`working`/`blocked-on-permission`/`errored`/no-verdict) → leave alone —
    /// reusing the existing stop/decommission operations. No-ops gracefully when
    /// the Session Manager is disabled or the daemon is unreachable.
    /// What: GETs the managed list + each activity verdict, then (unless
    /// `--dry-run`) POSTs `runtime-stop`/`decommission` for the actionable rows.
    /// Test: `cli_parses_session_prune_idle`; policy in `core::sm::prune::tests`;
    /// plan/render in `commands::prune::tests`.
    PruneIdle {
        /// List the candidate sessions, their verdicts, and the action that
        /// WOULD be taken — without stopping or decommissioning anything.
        #[arg(long)]
        dry_run: bool,
        /// Emit the plan as a single JSON object (for programmatic callers such
        /// as the claude-mpm pause skill).
        #[arg(long)]
        json: bool,
    },
}

/// Actions for the `overseer` subcommand.
#[derive(Debug, Subcommand)]
pub(crate) enum OverseerAction {
    /// Show the overseer's enabled status and handler type.
    Status,
}

/// Actions for the `optimizer` subcommand.
#[derive(Debug, Subcommand)]
pub(crate) enum OptimizerAction {
    /// Show current optimizer configuration.
    Status,
    /// Set the default compression level (rewrites the framework policy file).
    Set {
        /// Compression level: off, trim, summarise, caveman.
        #[arg(value_enum)]
        level: CliCompressionLevel,
    },
}

/// Subcommands for `tm services`.
///
/// Why: each subcommand answers exactly one agent question (port? url? healthy?)
/// so the output is scriptable without parsing a full status block.
/// What: eight variants covering list, status, port, url, health, log, init,
/// and restart. Exit codes follow the spec: 0=ok/running, 1=down/unhealthy,
/// 2=unknown service.
/// Test: `cli_parses_services_*` tests in the `#[cfg(test)]` block.
#[derive(Debug, Subcommand)]
pub(crate) enum ServicesAction {
    /// List all declared services with their current status.
    ///
    /// Exit code: always 0 (list never fails; individual services may be down).
    List {
        /// Output as JSON array instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },

    /// Show detailed status for one service.
    ///
    /// Exit code: 0 if running, 1 if down, 2 if service name not in manifest.
    Status {
        /// Service name (e.g. trusty-search).
        name: String,
        /// Output as JSON object.
        #[arg(long)]
        json: bool,
    },

    /// Print the port number for a service (scriptable: PORT=$(tm services port X)).
    ///
    /// Prints just the port number on stdout. Exit code: 0 if port known,
    /// 1 if service is down or port unavailable, 2 if unknown service.
    Port {
        /// Service name.
        name: String,
    },

    /// Print the full base URL for a service (e.g. http://localhost:7878).
    ///
    /// Exit code: 0 if URL known, 1 if service is down, 2 if unknown service.
    Url {
        /// Service name.
        name: String,
    },

    /// Probe the health endpoint and print OK or FAIL.
    ///
    /// Prints "OK" on stdout when healthy; diagnostic detail on stderr when
    /// unhealthy. Exit code: 0 if healthy, 1 if unhealthy or down.
    Health {
        /// Service name.
        name: String,
    },

    /// Print the path to the most-recent log file.
    ///
    /// Scriptable: `tail -f $(tm services log trusty-search)`
    /// Exit code: 0 if log path known and file exists, 1 if not, 2 if unknown.
    Log {
        /// Service name.
        name: String,
    },

    /// Write the default manifest to ~/.claude-mpm/services.yaml.
    ///
    /// Non-destructive: errors if the file already exists. Use --force to
    /// overwrite an existing manifest.
    Init {
        /// Overwrite an existing manifest.
        #[arg(long)]
        force: bool,
    },

    /// Restart a service using its manifest `restart_cmd`.
    ///
    /// Exit code: 0 if restart_cmd succeeded, 1 if restart_cmd absent or failed,
    /// 2 if unknown service.
    Restart {
        /// Service name.
        name: String,
    },
}

/// CLI-friendly compression level (mirrors `CompressionLevel`).
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum CliCompressionLevel {
    /// No compression.
    Off,
    /// Trim large outputs.
    Trim,
    /// Trim + strip ANSI + collapse blanks.
    Summarise,
    /// Drop all content, keep a one-line summary.
    Caveman,
}
