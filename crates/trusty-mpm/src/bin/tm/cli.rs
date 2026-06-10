//! CLI argument definitions for the `trusty-mpm` / `tm` binary.
//!
//! Why: keeping all clap struct/enum definitions in one file makes it easy to
//! audit the full CLI surface without wading through handler logic.
//! What: defines `Cli`, `Command`, and all sub-command enums.
//! Test: each variant is exercised by the `cli_parses_*` unit tests in
//! `tests.rs`.

use std::net::SocketAddr;

use clap::{Parser, Subcommand};

/// Default daemon address when `--url` / `TRUSTY_MPM_URL` is unset.
pub(crate) const DEFAULT_URL: &str = "http://127.0.0.1:7880";

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
    /// `stop` triad. `tm serve` does NOT speak MCP stdio — pass
    /// `tm daemon --mcp` for the MCP-on-stdin/stdout mode.
    Serve,
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
        #[arg(long, env = "TRUSTY_MPM_ADDR", default_value = "127.0.0.1:7880")]
        addr: SocketAddr,
        /// Also expose the daemon on the Tailscale interface for remote access.
        #[arg(long, env = "TRUSTY_MPM_TAILSCALE")]
        tailscale: bool,
        /// Run as an MCP server over stdio instead of the HTTP daemon.
        #[arg(long)]
        mcp: bool,
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
    /// Send a message to the cross-session coordinator and print its reply.
    ///
    /// A message prefixed with `@session:` routes a command at that session;
    /// a plain message is answered by the LLM with full session context.
    #[command(alias = "coord")]
    Coordinator {
        /// The message to send to the coordinator.
        message: String,
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
    /// Stop a session by id or friendly name.
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
    /// Resume a paused session.
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
