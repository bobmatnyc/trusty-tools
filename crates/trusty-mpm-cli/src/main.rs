//! trusty-mpm — the single unified binary.
//!
//! Why: `cargo install trusty-mpm` should install exactly one binary that
//! covers every mode — the resident daemon, the MCP server, the ratatui
//! dashboard, the Telegram bot, and the thin HTTP CLI. One binary keeps the
//! install story simple and the modes discoverable via `--help`.
//! What: parses subcommands and routes to the embedded library crates —
//! `trusty_mpm_daemon`, `trusty_mpm_tui`, `trusty_mpm_telegram`,
//! `trusty_mpm_gui` — or, for the
//! thin CLI subcommands, drives the daemon's HTTP API with an async `reqwest`
//! client.
//! Test: `cargo run -p trusty-mpm-cli -- status` prints daemon/session state;
//! handler and parsing logic are covered by `cargo test -p trusty-mpm-cli`.

use std::net::SocketAddr;

use clap::{Parser, Subcommand};
use serde::Deserialize;

/// Default daemon address when `--url` / `TRUSTY_MPM_URL` is unset.
const DEFAULT_URL: &str = "http://127.0.0.1:7880";

/// trusty-mpm command-line interface.
#[derive(Debug, Parser)]
#[command(name = "trusty-mpm", version, about = "trusty-mpm — unified binary")]
struct Cli {
    /// Base URL of the trusty-mpm daemon (used by the thin CLI subcommands).
    #[arg(long, env = "TRUSTY_MPM_URL", default_value = DEFAULT_URL, global = true)]
    url: String,

    /// Subcommand to run.
    #[command(subcommand)]
    command: Command,
}

/// Top-level CLI subcommands.
#[derive(Debug, Subcommand)]
enum Command {
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
enum TelegramCmd {
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
enum ProjectAction {
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
enum SessionAction {
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
enum OverseerAction {
    /// Show the overseer's enabled status and handler type.
    Status,
}

/// Actions for the `optimizer` subcommand.
#[derive(Debug, Subcommand)]
enum OptimizerAction {
    /// Show current optimizer configuration.
    Status,
    /// Set the default compression level (rewrites the framework policy file).
    Set {
        /// Compression level: off, trim, summarise, caveman.
        #[arg(value_enum)]
        level: CliCompressionLevel,
    },
}

/// CLI-friendly compression level (mirrors `CompressionLevel`).
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum CliCompressionLevel {
    /// No compression.
    Off,
    /// Trim large outputs.
    Trim,
    /// Trim + strip ANSI + collapse blanks.
    Summarise,
    /// Drop all content, keep a one-line summary.
    Caveman,
}

/// One session row as returned by `GET /sessions`.
#[derive(Debug, Deserialize)]
struct SessionRow {
    /// Session id (a `SessionId` newtype: `{"0": "<uuid>"}`).
    id: serde_json::Value,
    /// Working directory.
    workdir: String,
    /// Lifecycle status string.
    status: serde_json::Value,
    /// Number of active delegations.
    #[serde(default)]
    active_delegations: u32,
}

/// One project row as returned by `GET /projects`.
#[derive(Debug, Deserialize)]
struct ProjectRow {
    /// Absolute project path.
    path: std::path::PathBuf,
    /// Human-readable project name.
    name: String,
}

/// One event row as returned by `GET /events`.
#[derive(Debug, Deserialize)]
struct EventRow {
    /// Originating session (`SessionId` newtype JSON).
    session: serde_json::Value,
    /// Claude Code wire event name.
    event: String,
    /// RFC3339 timestamp the daemon received the event.
    at: String,
    /// Raw event payload (shape varies per event).
    #[serde(default)]
    payload: serde_json::Value,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Long-running modes need tracing on stderr (the daemon's MCP mode speaks
    // JSON-RPC on stdout, so all logs must stay off stdout).
    if matches!(cli.command, Command::Daemon { .. }) {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "info".into()),
            )
            .with_writer(std::io::stderr)
            .init();
    }

    let client = reqwest::Client::new();
    // Resolve the daemon URL once: explicit --url/TRUSTY_MPM_URL wins, then
    // lock file (daemon may bind to an ephemeral port), then default.
    let url = trusty_mpm_core::resolve_daemon_url(Some(&cli.url));
    match cli.command {
        Command::Status => status(&client, &url).await,
        Command::Start => start(&client, &url).await,
        Command::Serve => start(&client, &url).await,
        Command::Stop => stop_daemon().await,
        Command::Restart => restart(&client, &url).await,
        Command::Project { action } => project(&client, &url, action).await,
        Command::Session { action } => session(&client, &url, action).await,
        Command::Events => events(&client, &url).await,
        Command::Doctor => doctor(&url).await,
        Command::Tui {
            url: tui_url,
            interval_ms,
        } => {
            let resolved = trusty_mpm_core::resolve_daemon_url(Some(&tui_url));
            trusty_mpm_tui::run(resolved, interval_ms).await
        }
        Command::Gui => {
            #[cfg(feature = "gui")]
            {
                trusty_mpm_gui::run();
                Ok(())
            }
            #[cfg(not(feature = "gui"))]
            {
                anyhow::bail!(
                    "this build was compiled without GUI support (the `gui` feature is disabled)"
                )
            }
        }
        Command::Telegram { cmd } => telegram(&url, cmd).await,
        Command::Install { force } => install(force),
        Command::Hook => hook(&client, &url).await,
        Command::Daemon {
            addr,
            tailscale,
            mcp,
        } => run_daemon(addr, tailscale, mcp).await,
        Command::Launch { dir } => launch(&client, &url, dir).await,
        Command::Connect { dir } => connect(&client, &url, dir).await,
        Command::Attach { target, json } => attach_cmd(&client, &url, &target, json).await,
        Command::Optimizer { action } => optimizer(&client, &url, action).await,
        Command::Overseer { action } => overseer(&client, &url, action).await,
        Command::Coordinator { message } => coordinator(&url, message).await,
    }
}

/// `coordinator` / `coord` subcommand — message the cross-session coordinator.
///
/// Why: a scriptable, one-shot entry point to the coordinator so Telegram, cron
/// jobs, and shell scripts can ask "what is happening across my sessions?" or
/// route a `@session:` command without the TUI.
/// What: dispatches a [`TrustyCommand::CoordinatorChat`] through the shared
/// [`CommandExecutor`] and prints the reply (or the routed command's output);
/// a daemon/LLM failure becomes a non-zero-exit error line.
/// Test: covered by the executor's coordinator wire-shape tests.
async fn coordinator(url: &str, message: String) -> anyhow::Result<()> {
    use trusty_mpm_client::{CommandExecutor, CommandResult, TrustyCommand};
    let executor = CommandExecutor::new(url.to_string());
    match executor
        .execute(TrustyCommand::CoordinatorChat { message })
        .await
    {
        CommandResult::ChatReply { reply } => println!("{reply}"),
        CommandResult::Error(msg) => {
            eprintln!("coordinator: {msg}");
            std::process::exit(1);
        }
        other => eprintln!("unexpected coordinator result: {other:?}"),
    }
    Ok(())
}

/// `telegram` subcommand — manage the Telegram bot (pair, status, start, stop).
///
/// Why: every Telegram operation belongs under one discoverable command group;
/// this dispatcher routes the [`TelegramCmd`] variants to their handlers.
/// What: `Pair` requests a pairing code, `Status` queries the daemon's pairing
/// state, `Start` runs the bot process in the foreground, `Stop` kills it.
/// Test: variant routing is covered by the `cli_parses_telegram_*` tests; the
/// handlers themselves are exercised against a live daemon.
async fn telegram(url: &str, cmd: TelegramCmd) -> anyhow::Result<()> {
    match cmd {
        TelegramCmd::Pair => telegram_pair(url).await,
        TelegramCmd::Status => telegram_status(url).await,
        TelegramCmd::Start {
            url: start_url,
            token,
            alert_chat_id,
            allowed_user_id,
            check,
        } => {
            let token = token.or_else(|| trusty_mpm_telegram::resolve_token("TELEGRAM_BOT_TOKEN"));
            let options = trusty_mpm_telegram::BotOptions {
                allowed_user_id,
                alert_chat_id,
            };
            trusty_mpm_telegram::run(start_url, token, check, options).await
        }
        TelegramCmd::Stop => telegram_stop(),
    }
}

/// `telegram pair` — request a Telegram-bot pairing code.
///
/// Why: pairing the Telegram bot to this daemon needs an out-of-band shared
/// secret; `tm telegram pair` asks the local daemon for a short code and prints
/// both the `/pair` command and a `t.me` deep link the operator can use.
/// What: calls `POST /pair/request` via the shared [`CommandExecutor`] and
/// prints the code, its TTL, the `/pair <code>` command, and the deep link.
/// Test: covered by the executor's `pair_request_returns_code` test.
async fn telegram_pair(url: &str) -> anyhow::Result<()> {
    use trusty_mpm_client::{CommandExecutor, CommandResult};
    let executor = CommandExecutor::new(url.to_string());
    match executor.pair_request().await {
        CommandResult::PairCode {
            code,
            expires_in_seconds,
        } => {
            println!("Pairing code: {code}");
            println!("Expires in: {} minutes", expires_in_seconds / 60);
            println!();
            println!("In Telegram, send to your bot:");
            println!("  /pair {code}");
            println!();
            println!("Or click: https://t.me/trusty_mpm_bot?start={code}");
        }
        CommandResult::Error(msg) => eprintln!("pairing failed: {msg}"),
        other => eprintln!("unexpected pairing result: {other:?}"),
    }
    Ok(())
}

/// `telegram status` — show the daemon's Telegram pairing state.
///
/// Why: operators need to know whether a Telegram chat is already paired with
/// the daemon — and which one — without digging through logs.
/// What: calls `GET /pair/status` via the shared [`DaemonClient`] and prints
/// `paired` plus the registered `chat_id`, or `unpaired` when no chat is bound.
/// Test: covered by the client's `pair_status_deserializes` test.
async fn telegram_status(url: &str) -> anyhow::Result<()> {
    use trusty_mpm_client::DaemonClient;
    let client = DaemonClient::new(url.to_string());
    match client.pair_status().await {
        Ok(status) if status.paired => match status.chat_id {
            Some(chat_id) => println!("Telegram: paired (chat_id: {chat_id})"),
            None => println!("Telegram: paired"),
        },
        Ok(_) => println!("Telegram: unpaired — run `tm telegram pair` to begin"),
        Err(e) => eprintln!("daemon unreachable: {e}"),
    }
    Ok(())
}

/// `telegram stop` — kill the Telegram bot process if one is running.
///
/// Why: the bot may be running standalone (`tm telegram start`) or alongside
/// the daemon; `tm telegram stop` gives the operator a single way to take it
/// down without hunting for the PID.
/// What: runs `pkill -f "trusty-mpm telegram start"` to terminate any
/// foreground bot process; reports whether a process was killed.
/// Test: exercised manually — process management is not unit-testable here.
fn telegram_stop() -> anyhow::Result<()> {
    let status = std::process::Command::new("pkill")
        .args(["-f", "telegram start"])
        .status();
    match status {
        Ok(s) if s.success() => println!("Telegram bot stopped"),
        Ok(_) => println!("no Telegram bot process found"),
        Err(e) => eprintln!("failed to stop Telegram bot: {e}"),
    }
    Ok(())
}

/// `install` subcommand — deploy the bundled framework artifacts and wire
/// MPM lifecycle hooks into every Claude Code settings file.
///
/// Why: a fresh machine has no `~/.trusty-mpm/framework/`; `trusty-mpm install`
/// writes the compile-time-embedded artifacts (optimizer policy, framework
/// instructions, placeholder agent/skill) so the daemon has a working policy
/// and launchers have instructions to point sessions at. It also installs
/// the MPM `PreToolUse` / `PostToolUse` / `Stop` hooks into every Claude
/// Code settings file on the machine so the daemon receives the lifecycle
/// events that drive its circuit breaker, audit log, and dashboard. All
/// edits are idempotent.
/// What: resolves [`FrameworkPaths::default`] and delegates to
/// [`install_to`], deploys composed agents, then calls
/// [`install_claude_hooks`] to merge the MPM hook block into every
/// discovered settings file.
/// Test: `install_writes_all_artifacts`, `install_skips_existing_without_force`,
/// `install_claude_hooks_is_idempotent`.
fn install(force: bool) -> anyhow::Result<()> {
    let paths = trusty_mpm_core::paths::FrameworkPaths::default();
    let report = install_to(&paths, force)?;
    println!(
        "Installing trusty-mpm framework artifacts to {}",
        paths.framework.display()
    );
    for line in &report {
        println!("  {line}");
    }
    println!(
        "Composing agents into {}",
        paths.claude_agents_dir().display()
    );
    let deploy = trusty_mpm_core::agent_deployer::deploy_agents(
        &paths.agent_source_dir(),
        &paths.claude_agents_dir(),
    )?;
    for line in deploy_report_lines(&deploy, &paths.agent_source_dir()) {
        println!("  {line}");
    }

    // Wire the MPM lifecycle hooks into every Claude settings file on the
    // machine. Per-file failures are non-fatal so one bad file does not sink
    // the whole install.
    if let Err(e) = install_claude_hooks() {
        eprintln!("warning: failed to install Claude Code hooks: {e:#}");
    }

    println!("Framework installed. Run `trusty-mpm daemon` to start.");
    Ok(())
}

/// Idempotently install the MPM `PreToolUse` / `PostToolUse` / `Stop` hook
/// block into every Claude Code settings file on the machine.
///
/// Why: without these hooks the daemon never receives Claude Code lifecycle
/// events, so the circuit breaker, audit log, and dashboard all sit blind.
/// `tm install` is the canonical point to wire them up — running it twice
/// must produce identical files (idempotency requirement).
/// What: discovers every `.claude/settings*.json` under `$HOME` via
/// [`trusty_common::claude_config::discover_claude_settings`], loads each
/// one, deep-merges the MPM hook block using
/// [`trusty_common::claude_config::merge_hook_entries`], and writes the
/// result back atomically when it differs from disk. Falls back to creating
/// `~/.claude/settings.json` when no settings files exist. Returns a count
/// of files modified for the caller to report.
/// Test: `install_claude_hooks_is_idempotent`,
/// `install_claude_hooks_creates_fallback`.
fn install_claude_hooks() -> anyhow::Result<usize> {
    use colored::Colorize;
    use trusty_common::claude_config::{
        default_settings_max_depth, discover_claude_settings, merge_hook_entries, write_json_atomic,
    };

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
    println!(
        "Wiring MPM hooks into Claude Code settings under {}…",
        home.display()
    );

    let additions = mpm_hook_additions();
    let files = discover_claude_settings(&home, default_settings_max_depth());

    let target_files: Vec<std::path::PathBuf> = if files.is_empty() {
        let fallback = home.join(".claude").join("settings.json");
        println!("  no settings files found; creating {}", fallback.display());
        vec![fallback]
    } else {
        files
    };

    let mut changed = 0usize;
    for path in &target_files {
        let original: serde_json::Value = match std::fs::read_to_string(path) {
            Ok(s) if s.trim().is_empty() => serde_json::Value::Object(serde_json::Map::new()),
            Ok(s) => match serde_json::from_str(&s) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!(
                        "  {} {} {}",
                        "✗".red(),
                        path.display(),
                        format!("(parse error: {e})").red()
                    );
                    continue;
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                serde_json::Value::Object(serde_json::Map::new())
            }
            Err(e) => {
                eprintln!(
                    "  {} {} {}",
                    "✗".red(),
                    path.display(),
                    format!("(read error: {e})").red()
                );
                continue;
            }
        };
        let merged = merge_hook_entries(&original, &additions);
        if merged == original {
            println!(
                "  {} {} {}",
                "↻".cyan(),
                path.display().to_string().dimmed(),
                "(already configured)".dimmed()
            );
            continue;
        }
        match write_json_atomic(path, &merged) {
            Ok(()) => {
                changed += 1;
                println!("  {} {}", "✓".green(), path.display());
            }
            Err(e) => {
                eprintln!(
                    "  {} {} {}",
                    "✗".red(),
                    path.display(),
                    format!("({e})").red()
                );
            }
        }
    }

    if changed > 0 {
        println!(
            "  installed MPM hooks in {} settings file{}.",
            changed,
            if changed == 1 { "" } else { "s" }
        );
    }
    Ok(changed)
}

/// Build the MPM lifecycle hook additions JSON block.
///
/// Why: every call site (the install handler and its unit test) needs the
/// exact same shape so [`merge_hook_entries`] can dedup by deep equality;
/// centralising the literal avoids two slightly-different copies racing in
/// the idempotency check.
/// What: returns a JSON object with `PreToolUse`, `PostToolUse`, and `Stop`
/// arrays, each carrying one `{matcher: "*", hooks: [{type: "command",
/// command: "trusty-mpm hook", timeout: ...}]}` block. `PostToolUse` runs
/// asynchronously (Claude Code does not block on its completion) because
/// the daemon may take longer to ingest tool results; `PreToolUse` and
/// `Stop` keep the default synchronous behaviour with short timeouts.
/// Test: covered indirectly by `install_claude_hooks_is_idempotent`.
fn mpm_hook_additions() -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": "trusty-mpm hook",
                    "timeout": 5
                }]
            }],
            "PostToolUse": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": "trusty-mpm hook",
                    "timeout": 60,
                    "async": true
                }]
            }],
            "Stop": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": "trusty-mpm hook",
                    "timeout": 5
                }]
            }]
        }
    })
}

/// Render per-file status lines for an agent [`DeployResult`].
///
/// Why: `install` and `session start` both print agent deploy results; one
/// formatter keeps the output identical and the call sites small.
/// What: a `✓ <file> (composed: a → b → c)` line per deployed agent, a
/// `~ <file> (skipped — user-modified)` line per skipped one, and a `=` line
/// per unchanged one; the chain comes from the agent's resolved source chain.
/// Test: covered indirectly by `install_writes_all_artifacts`.
fn deploy_report_lines(
    deploy: &trusty_mpm_core::agent_deployer::DeployResult,
    source_dir: &std::path::Path,
) -> Vec<String> {
    let mut lines = Vec::new();
    for file in &deploy.deployed {
        let name = file.trim_end_matches(".md");
        let chain = trusty_mpm_core::agent_builder::source_chain(name, source_dir)
            .map(|c| c.join(" \u{2192} "))
            .unwrap_or_else(|_| name.to_string());
        lines.push(format!("\u{2713} {file} (composed: {chain})"));
    }
    for file in &deploy.skipped {
        lines.push(format!("~ {file} (skipped \u{2014} user-modified)"));
    }
    for file in &deploy.unchanged {
        lines.push(format!("= {file} (unchanged)"));
    }
    lines
}

/// Write every bundled artifact under `paths`, returning a per-file report.
///
/// Why: separating the filesystem work from argument parsing and stdout makes
/// the installer unit-testable against a `tempfile::TempDir`.
/// What: for each [`trusty_mpm_core::bundle::ALL`] artifact, creates parent
/// directories and writes the file; an existing file is skipped unless `force`.
/// Returns one human-readable status line per artifact.
/// Test: `install_writes_all_artifacts`, `install_skips_existing_without_force`.
fn install_to(
    paths: &trusty_mpm_core::paths::FrameworkPaths,
    force: bool,
) -> anyhow::Result<Vec<String>> {
    let mut report = Vec::new();
    for artifact in trusty_mpm_core::bundle::ALL {
        let dest = paths.framework.join(artifact.rel_path);
        if dest.exists() && !force {
            report.push(format!("- {} (exists, skipped)", artifact.rel_path));
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, artifact.contents)?;
        report.push(format!("\u{2713} {}", artifact.rel_path));
    }
    Ok(report)
}

/// Environment variable set by every MPM-spawned sub-agent process so its
/// nested Claude Code session can suppress hook traffic.
///
/// Why: a sub-agent is already running inside an MPM-supervised context;
/// re-emitting hook events from inside it just doubles every tool call in
/// the daemon's audit log without adding signal. Stamping the env var on
/// the spawn side and gating the hook handler on the same var keeps the
/// suppression cheap, explicit, and process-local. Re-exporting the shared
/// constant from `trusty_common::claude_config` ensures the spawn site
/// (`open-mpm`) and this consumer never drift apart on the literal name.
/// What: thin alias for
/// [`trusty_common::claude_config::CLAUDE_MPM_SUB_AGENT_ENV_VAR`]. Presence
/// is what matters; the canonical value used by spawn helpers is `"1"`.
const SUB_AGENT_ENV: &str = trusty_common::claude_config::CLAUDE_MPM_SUB_AGENT_ENV_VAR;

/// `hook` subcommand — handle a Claude Code lifecycle hook event.
///
/// Why: Claude Code invokes the configured hook command on every PreToolUse /
/// PostToolUse / Stop event. The handler posts a minimal `hook_event` to the
/// daemon so the circuit breaker, audit log, and dashboard can react. To keep
/// nested MPM sub-agents from doubling every tool call in the audit feed, the
/// very first thing the handler does is check `CLAUDE_MPM_SUB_AGENT`; when
/// that env var is present the handler exits 0 immediately without contacting
/// the daemon.
/// What: reads the event name from `CLAUDE_HOOK_EVENT` and the session id
/// from `CLAUDE_SESSION_ID` (both populated by Claude Code; missing values
/// degrade to placeholders so the daemon still gets a record). POSTs to
/// `<url>/hooks` with a JSON body and a 2-second timeout. Every failure path
/// returns `Ok(())` — failing the hook would block the user's prompt.
/// Test: `cli_parses_hook` covers parse routing; the guard branch is
/// exercised via the inline `hook_guard_short_circuits` test.
async fn hook(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    // Guard FIRST so no I/O happens inside MPM-spawned sub-agents.
    if std::env::var_os(SUB_AGENT_ENV).is_some() {
        return Ok(());
    }

    // Pull the minimal context Claude Code stamps into the environment. Both
    // values may be absent in test harnesses or future Claude Code versions;
    // degrade to placeholders so the daemon still gets a record.
    let event = std::env::var("CLAUDE_HOOK_EVENT").unwrap_or_else(|_| "Unknown".to_string());
    let session_id = std::env::var("CLAUDE_SESSION_ID").unwrap_or_default();

    let body = serde_json::json!({
        "session_id": session_id,
        "event": event,
        "payload": {}
    });

    // Best-effort POST — any failure (daemon down, network blip, malformed
    // url) becomes a silent Ok(()) so Claude Code never sees a non-zero exit.
    let req = client
        .post(format!("{url}/hooks"))
        .timeout(std::time::Duration::from_secs(2))
        .json(&body)
        .send();
    let _ = req.await;
    Ok(())
}

/// Resolve a `--dir` option to an absolute path, defaulting to the cwd.
///
/// Why: `project` and `session` subcommands all accept an optional directory;
/// centralizing the "default to cwd" rule keeps the handlers uniform.
/// What: returns `dir` as a `PathBuf` when given, otherwise the process cwd.
/// Test: covered indirectly by the project/session handler integration tests.
fn resolve_dir(dir: Option<String>) -> anyhow::Result<std::path::PathBuf> {
    match dir {
        Some(d) => Ok(std::path::PathBuf::from(d)),
        None => Ok(std::env::current_dir()?),
    }
}

/// `project` subcommand — define and manage trusty-mpm projects.
///
/// Why: a project is a registered working directory; operators need shell
/// commands to register one, list all, and inspect the current one without
/// hand-crafting HTTP requests.
/// What: `Init` registers the directory (`POST /projects`) and scaffolds a
/// local `.trusty-mpm/`; `List` prints `GET /projects`; `Info` prints the
/// current directory's project via `GET /projects/current`.
/// Test: `cli_parses_project_init`, `cli_parses_project_list`,
/// `cli_parses_project_info`, `project_init_scaffolds_dotdir`.
async fn project(client: &reqwest::Client, url: &str, action: ProjectAction) -> anyhow::Result<()> {
    match action {
        ProjectAction::Init { dir } => {
            let path = resolve_dir(dir)?;
            let body: serde_json::Value = client
                .post(format!("{url}/projects"))
                .json(&serde_json::json!({ "path": path }))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let report = scaffold_project_dir(&path)?;
            for line in &report {
                println!("  {line}");
            }
            let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            println!("registered project '{name}' at {}", path.display());
        }
        ProjectAction::List => {
            #[derive(Deserialize)]
            struct Body {
                projects: Vec<ProjectRow>,
            }
            let body: Body = client
                .get(format!("{url}/projects"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            if body.projects.is_empty() {
                println!("no projects registered");
            }
            for p in &body.projects {
                println!("{} {}", p.name, p.path.display());
            }
        }
        ProjectAction::Info { dir } => {
            let path = resolve_dir(dir)?;
            let resp = client
                .get(format!("{url}/projects/current"))
                .query(&[("path", path.to_string_lossy().as_ref())])
                .send()
                .await?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                println!("{} is not a registered project", path.display());
            } else {
                let body: serde_json::Value = resp.error_for_status()?.json().await?;
                println!("{}", serde_json::to_string_pretty(&body)?);
            }
        }
    }
    Ok(())
}

/// Scaffold `<project>/.trusty-mpm/` with a config skeleton and `sessions/`.
///
/// Why: `project init` must give the operator an editable, version-controllable
/// project config; doing it in a testable helper keeps it covered without a
/// live daemon.
/// What: creates `.trusty-mpm/sessions/` and writes `config.toml` (only when
/// absent — never clobbering an edited config); returns a per-path report.
/// Test: `project_init_scaffolds_dotdir`, `project_init_keeps_existing_config`.
fn scaffold_project_dir(project: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let mut report = Vec::new();
    let dotdir = project.join(".trusty-mpm");
    let sessions = dotdir.join("sessions");
    std::fs::create_dir_all(&sessions)?;
    report.push(format!("\u{2713} {}", sessions.display()));

    let config = dotdir.join("config.toml");
    if config.exists() {
        report.push(format!("- {} (exists, skipped)", config.display()));
    } else {
        let name = trusty_mpm_core::project::name_from_path(project);
        let contents = format!(
            "# trusty-mpm project configuration\n\
             # Generated by: trusty-mpm project init\n\n\
             [project]\nname = \"{name}\"\n\n\
             [agents]\n\
             # Additional agent sources for this project\n\
             # sources = [\"https://example.com/agents\"]\n\n\
             [skills]\n\
             # Additional skill sources for this project\n\
             # sources = []\n"
        );
        std::fs::write(&config, contents)?;
        report.push(format!("\u{2713} {}", config.display()));
    }
    Ok(report)
}

/// `session` subcommand — define and manage sessions within a project.
///
/// Why: a session is a Claude Code instance; operators start, stop, list,
/// reap, and inspect them per project from the shell.
/// What: `Start` posts `POST /sessions` with the project path; `Stop` and
/// `Info` resolve a session by id or friendly name; `List` and `Clean` scope
/// to the project directory.
/// Test: `cli_parses_session_start`, `cli_parses_session_stop`,
/// `cli_parses_session_list`, `cli_parses_session_clean`,
/// `cli_parses_session_info`.
async fn session(client: &reqwest::Client, url: &str, action: SessionAction) -> anyhow::Result<()> {
    match action {
        SessionAction::Start { dir } => {
            let path = resolve_dir(dir)?;
            // Prepare the custom instructions Claude Code reads at startup:
            // deploy composed agents to `~/.claude/agents/` and merge the
            // project CLAUDE.md. This shared prep is what makes a plain
            // `claude` process behave as a trusty-mpm session.
            let fw = trusty_mpm_core::paths::FrameworkPaths::default();
            match trusty_mpm_core::session_launch::prepare_session(&fw, &path) {
                Ok(report) => {
                    println!(
                        "Agents: {} deployed, {} skipped, {} unchanged",
                        report.deploy.deployed.len(),
                        report.deploy.skipped.len(),
                        report.deploy.unchanged.len(),
                    );
                    if report.instructions.claude_md_created {
                        println!("  Created CLAUDE.md stub in {}", path.display());
                    }
                    println!(
                        "Instructions: {} agents in delegation authority",
                        report.instructions.agent_count
                    );
                    println!(
                        "  Merged instructions written to {}",
                        report.stash.display()
                    );
                }
                Err(err) => eprintln!("warning: session preparation failed: {err}"),
            }

            #[derive(Deserialize)]
            struct Body {
                #[serde(default)]
                name: String,
            }
            let body: Body = client
                .post(format!("{url}/sessions"))
                .json(&serde_json::json!({
                    "project": path,
                    "project_path": path,
                }))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            // The daemon only registers session state now — it no longer
            // spawns the tmux host (that caused session proliferation). The
            // CLI owns the actual launch: create a detached tmux session in
            // the project directory and start `claude` in it.
            let workdir = path.to_string_lossy().to_string();
            let new_session = std::process::Command::new("tmux")
                .args(["new-session", "-d", "-s", &body.name, "-c", &workdir])
                .status();
            match new_session {
                Ok(status) if status.success() => {
                    let send = std::process::Command::new("tmux")
                        .args(["send-keys", "-t", &body.name, "claude", "Enter"])
                        .status();
                    match send {
                        Ok(s) if s.success() => {
                            println!("started session {} (tmux + claude)", body.name);
                        }
                        Ok(_) | Err(_) => {
                            eprintln!(
                                "warning: tmux session {} created but failed to start claude",
                                body.name
                            );
                            println!("started session {}", body.name);
                        }
                    }
                }
                Ok(_) | Err(_) => {
                    eprintln!(
                        "warning: failed to create tmux session {}; run `claude` manually in {}",
                        body.name, workdir
                    );
                    println!("started session {}", body.name);
                }
            }
        }
        SessionAction::Stop { id_or_name } => {
            let resp = client
                .delete(format!("{url}/sessions/{id_or_name}"))
                .send()
                .await?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                println!("not found");
            } else {
                resp.error_for_status()?;
                println!("stopped {id_or_name}");
            }
        }
        SessionAction::List { dir } => {
            let path = resolve_dir(dir)?;
            #[derive(Deserialize)]
            struct Body {
                sessions: Vec<SessionRow>,
            }
            let body: Body = client
                .get(format!("{url}/sessions"))
                .query(&[("project", path.to_string_lossy().as_ref())])
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            if body.sessions.is_empty() {
                println!("no sessions for {}", path.display());
            }
            for s in &body.sessions {
                let status = s.status.as_str().unwrap_or("unknown");
                println!("{} {} {}", short_id(&s.id), status, s.workdir);
            }
        }
        SessionAction::Clean { dir } => {
            // `dir` is accepted for symmetry; the daemon reaps globally.
            let _ = resolve_dir(dir)?;
            let body: serde_json::Value = client
                .delete(format!("{url}/sessions/dead"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let removed = body.get("removed").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("reaped {removed} dead session(s)");
        }
        SessionAction::Info { id_or_name } => {
            #[derive(Deserialize)]
            struct Body {
                sessions: Vec<serde_json::Value>,
            }
            let body: Body = client
                .get(format!("{url}/sessions"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let found = body.sessions.iter().find(|s| {
                let id_match = s
                    .get("id")
                    .and_then(|v| v.get("0"))
                    .and_then(|v| v.as_str())
                    == Some(id_or_name.as_str());
                let name_match =
                    s.get("tmux_name").and_then(|v| v.as_str()) == Some(id_or_name.as_str());
                id_match || name_match
            });
            match found {
                Some(s) => println!("{}", serde_json::to_string_pretty(s)?),
                None => println!("session '{id_or_name}' not found"),
            }
        }
        SessionAction::Instructions { dir } => {
            // Pure local computation — no daemon round-trip needed.
            let path = resolve_dir(dir)?;
            let fw = trusty_mpm_core::paths::FrameworkPaths::default();
            let (output, _stash) = compose_session_instructions(&fw, &path)?;
            print!("{}", output.merged);
        }
        SessionAction::Events { id_or_name } => {
            let id = match resolve_session_id(client, url, &id_or_name).await? {
                Some(id) => id,
                None => {
                    println!("session '{id_or_name}' not found");
                    return Ok(());
                }
            };
            #[derive(Deserialize)]
            struct Body {
                events: Vec<EventRow>,
            }
            let body: Body = client
                .get(format!("{url}/sessions/{id}/events/poll"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            if body.events.is_empty() {
                println!("no events for session {id_or_name}");
            }
            for e in &body.events {
                println!("{} {} {}", e.at, e.event, event_summary(&e.payload));
            }
        }
        SessionAction::Breakers => {
            #[derive(Deserialize)]
            struct Row {
                agent: String,
                breaker: serde_json::Value,
            }
            #[derive(Deserialize)]
            struct Body {
                breakers: Vec<Row>,
            }
            let body: Body = client
                .get(format!("{url}/breakers"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            if body.breakers.is_empty() {
                println!("no circuit breakers");
            } else {
                println!("{:<24} {:<12} FAILURES", "AGENT", "STATE");
                for r in &body.breakers {
                    let state = r
                        .breaker
                        .get("state")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let failures = r
                        .breaker
                        .get("consecutive_failures")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    println!("{:<24} {:<12} {}", r.agent, state, failures);
                }
            }
        }
        SessionAction::Pause { id_or_name, note } => {
            let resp = client
                .post(format!("{url}/sessions/{id_or_name}/pause"))
                .json(&serde_json::json!({ "summary": note }))
                .send()
                .await?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                println!("session '{id_or_name}' not found");
            } else {
                let body: serde_json::Value = resp.error_for_status()?.json().await?;
                let summary = body.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                println!("paused {id_or_name}: {summary}");
            }
        }
        SessionAction::Resume { id_or_name } => {
            let resp = client
                .post(format!("{url}/sessions/{id_or_name}/resume"))
                .send()
                .await?;
            match resp.status() {
                reqwest::StatusCode::NOT_FOUND => {
                    println!("session '{id_or_name}' not found");
                }
                reqwest::StatusCode::CONFLICT => {
                    println!("session '{id_or_name}' is not paused");
                }
                _ => {
                    resp.error_for_status()?;
                    println!("resumed {id_or_name}");
                }
            }
        }
        SessionAction::Run {
            id_or_name,
            command,
            summarize,
        } => {
            let mut req = client.post(format!("{url}/sessions/{id_or_name}/command"));
            if summarize {
                req = req.query(&[("compress", "summarise")]);
            }
            let resp = req
                .json(&serde_json::json!({ "command": command }))
                .send()
                .await?;
            match resp.status() {
                reqwest::StatusCode::NOT_FOUND => {
                    println!("session '{id_or_name}' not found");
                }
                reqwest::StatusCode::CONFLICT => {
                    println!("session '{id_or_name}' is stopped");
                }
                _ => {
                    let body: serde_json::Value = resp.error_for_status()?.json().await?;
                    let output = body.get("output").and_then(|v| v.as_str()).unwrap_or("");
                    print!("{output}");
                    print_compression_stats(&body);
                }
            }
        }
        SessionAction::Output {
            id_or_name,
            lines,
            summarize,
        } => {
            let mut query: Vec<(&str, String)> = vec![("lines", lines.to_string())];
            if summarize {
                query.push(("compress", "summarise".to_string()));
            }
            let resp = client
                .get(format!("{url}/sessions/{id_or_name}/output"))
                .query(&query)
                .send()
                .await?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                println!("session '{id_or_name}' not found");
            } else {
                let body: serde_json::Value = resp.error_for_status()?.json().await?;
                let output = body.get("output").and_then(|v| v.as_str()).unwrap_or("");
                print!("{output}");
                print_compression_stats(&body);
            }
        }
    }
    Ok(())
}

/// `launch` subcommand — launch a configured claude session and attach to it.
///
/// Why: `tm launch` should reproduce the `claude-mpm` experience — one command
/// that prepares the framework, registers the session, starts `claude` in a
/// tmux host, and hands the current terminal over to it. The user sees a
/// summary banner, then `claude` itself.
/// What: resolves `dir`, runs `prepare_session`, registers the session with the
/// daemon (`POST /sessions`, falling back to a generated name when the daemon
/// is unreachable), writes the system-prompt file, prints the banner, creates a
/// detached tmux session running `claude`, and `attach`es to it (blocking until
/// the user detaches/exits).
/// Test: `cli_parses_launch`, `cli_parses_launch_with_dir`.
async fn launch(client: &reqwest::Client, url: &str, dir: Option<String>) -> anyhow::Result<()> {
    // 1. Resolve the target directory (absolute, so the banner is unambiguous).
    //    `resolve_dir` already defaults to the process cwd when `dir` is None.
    let path = resolve_dir(dir)?;
    let path = path.canonicalize().unwrap_or(path);
    let workdir = path.to_string_lossy().to_string();

    // 1b. If a session already exists for this directory and its tmux session
    //     is still alive, reconnect to it instead of launching a new one. A
    //     daemon that is unreachable, or a stale record with no live tmux
    //     session, simply falls through to the normal launch sequence below.
    if let Some(existing) = find_existing_session(client, url, &workdir).await
        && !existing.is_empty()
        && tmux_has_session(&existing)
    {
        print_launch_banner_reconnecting(&workdir, &existing);
        let status = std::process::Command::new("tmux")
            .args(["attach-session", "-t", &existing])
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux attach-session exited with failure");
        }
        return Ok(());
    }

    // 2. Prepare the framework: deploy composed agents and merge CLAUDE.md so a
    //    plain `claude` process behaves as a trusty-mpm PM session. A prep
    //    failure is logged but not fatal — the session can still launch.
    let fw = trusty_mpm_core::paths::FrameworkPaths::default();
    if let Err(err) = trusty_mpm_core::session_launch::prepare_session(&fw, &path) {
        eprintln!("warning: session preparation failed: {err}");
    }

    // 3. Register the session with the daemon. The tmux name is derived from
    //    the project folder (`tmpm-<folder>`); we send it so the daemon's
    //    registry stays consistent with the tmux session we create below. When
    //    the daemon is unreachable we still launch under the same folder name.
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        name: String,
        /// The registered session's id; `None` when the daemon is unreachable.
        id: Option<trusty_mpm_core::session::SessionId>,
    }
    let folder_name = fallback_session_name(&path);
    let (tmux_name, session_id) = match client
        .post(format!("{url}/sessions"))
        .json(&serde_json::json!({
            "project": path,
            "project_path": path,
            "name": folder_name,
        }))
        .send()
        .await
    {
        Ok(resp) => match resp.error_for_status() {
            Ok(resp) => match resp.json::<Body>().await {
                Ok(body) if !body.name.is_empty() => (body.name, body.id),
                Ok(body) => (folder_name.clone(), body.id),
                _ => (folder_name.clone(), None),
            },
            Err(err) => {
                eprintln!("warning: daemon rejected session registration: {err}");
                (folder_name.clone(), None)
            }
        },
        Err(err) => {
            eprintln!("warning: daemon unreachable ({err}); launching without registration");
            (folder_name.clone(), None)
        }
    };

    // 4. Regenerate `~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md` from
    //    the bundled assets so the on-disk system prompt always reflects the
    //    current trusty-mpm build. A failure is logged but not fatal —
    //    `build_system_prompt` regenerates lazily if the file is missing.
    let instructions_path = match trusty_mpm_core::instruction_pipeline::install_system_prompt() {
        Ok(path) => Some(path),
        Err(err) => {
            eprintln!("warning: failed to install system prompt: {err}");
            None
        }
    };

    // Build the `--append-system-prompt` text (read from the file installed
    // above) and write it to a temp file so `claude` reads it at startup.
    let (claude_cmd, prompt_path) = match trusty_mpm_core::session_launch::build_system_prompt() {
        Some(prompt) => {
            let file = std::env::temp_dir().join(format!(
                "trusty-mpm-system-prompt-{}.txt",
                trusty_mpm_core::session::SessionId::new().0
            ));
            match std::fs::write(&file, &prompt) {
                Ok(()) => (
                    format!("claude --append-system-prompt-file {}", file.display()),
                    Some(file),
                ),
                Err(err) => {
                    eprintln!("warning: failed to write system prompt file: {err}");
                    ("claude".to_string(), None)
                }
            }
        }
        None => ("claude".to_string(), None),
    };

    // 5. Print the summary banner. The "Prompt:" line shows the canonical
    //    `~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md` source path
    //    when it was installed, not the per-session temp copy.
    print_launch_banner(
        &workdir,
        &tmux_name,
        instructions_path.as_deref().or(prompt_path.as_deref()),
    );

    // 6. Create a detached tmux session in the project directory.
    let new_session = std::process::Command::new("tmux")
        .args(["new-session", "-d", "-s", &tmux_name, "-c", &workdir])
        .status();
    if !matches!(new_session, Ok(s) if s.success()) {
        anyhow::bail!("failed to create tmux session {tmux_name} in {workdir}");
    }

    // 7. Start `claude` inside the tmux session.
    let send = std::process::Command::new("tmux")
        .args(["send-keys", "-t", &tmux_name, &claude_cmd, "Enter"])
        .status();
    if !matches!(send, Ok(s) if s.success()) {
        anyhow::bail!("tmux session {tmux_name} created but failed to start claude");
    }

    // 7b. Find the claude process PID inside the tmux pane and report it to
    //     the daemon so it can monitor process liveness. `claude` takes 1-3 s
    //     to start after send-keys, so this retries for up to ~5 s.
    if let Some(session_id) = session_id {
        let claude_pid = trusty_mpm_core::process::find_claude_pid_in_tmux(
            &tmux_name,
            10,                                    // up to 10 attempts
            std::time::Duration::from_millis(500), // 500 ms between attempts
        );
        if let Some(pid) = claude_pid {
            // PATCH /sessions/{id}/pid — tell the daemon the real process PID.
            // Best-effort; failure is logged but does not abort launch.
            let _ = client
                .patch(format!("{url}/sessions/{}/pid", session_id.0))
                .json(&serde_json::json!({ "pid": pid }))
                .send()
                .await;
            tracing::info!(
                "claude process PID {pid} registered for session {}",
                session_id.0
            );
        } else {
            tracing::warn!("could not find claude PID for session {tmux_name} after retries");
        }
    }

    // 8. Attach to the session — this takes over the current terminal and
    //    blocks until the user detaches or exits claude.
    let status = std::process::Command::new("tmux")
        .args(["attach-session", "-t", &tmux_name])
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux attach-session exited with failure");
    }
    Ok(())
}

/// `connect` subcommand — start or attach to a session without deployment.
///
/// Why: `tm connect` is the lightweight sibling of `tm launch`. Where `launch`
/// runs the full framework-deployment sequence (`prepare_session` +
/// `install_system_prompt`) before bringing the session up, `connect`
/// deliberately skips all deployment — it assumes the framework is already in
/// place (or that the operator does not want it touched) and only ensures the
/// tmux-hosted session is running, then attaches.
/// What: resolves `dir`, reconnects to a live session for the directory when
/// one exists, otherwise registers the session via
/// `POST /api/v1/sessions/connect`, creates the tmux host idempotently
/// (`tmux new-session -A`), starts `claude` only when the session is freshly
/// created, and `attach`es to it. No agents, skills, instructions, or
/// system-prompt files are written.
/// Test: `cli_parses_connect`, `cli_parses_connect_with_dir`.
async fn connect(client: &reqwest::Client, url: &str, dir: Option<String>) -> anyhow::Result<()> {
    // 1. Resolve the target directory (absolute, so the banner is unambiguous).
    let path = resolve_dir(dir)?;
    let path = path.canonicalize().unwrap_or(path);
    let workdir = path.to_string_lossy().to_string();

    // 1b. Reconnect to an existing live session for this directory if one
    //     exists — `connect` is idempotent by design.
    if let Some(existing) = find_existing_session(client, url, &workdir).await
        && !existing.is_empty()
        && tmux_has_session(&existing)
    {
        print_launch_banner_reconnecting(&workdir, &existing);
        let status = std::process::Command::new("tmux")
            .args(["attach-session", "-t", &existing])
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux attach-session exited with failure");
        }
        return Ok(());
    }

    // 2. Register the session with the daemon via the connect endpoint. No
    //    `prepare_session` and no `install_system_prompt` — `connect` skips the
    //    entire deployment sequence. When the daemon is unreachable we still
    //    bring the session up under the folder-derived name.
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        name: String,
    }
    let folder_name = fallback_session_name(&path);
    let tmux_name = match client
        .post(format!("{url}/api/v1/sessions/connect"))
        .json(&serde_json::json!({
            "project": path,
            "project_path": path,
            "name": folder_name,
        }))
        .send()
        .await
    {
        Ok(resp) => match resp.error_for_status() {
            Ok(resp) => match resp.json::<Body>().await {
                Ok(body) if !body.name.is_empty() => body.name,
                _ => folder_name.clone(),
            },
            Err(err) => {
                eprintln!("warning: daemon rejected session registration: {err}");
                folder_name.clone()
            }
        },
        Err(err) => {
            eprintln!("warning: daemon unreachable ({err}); connecting without registration");
            folder_name.clone()
        }
    };

    // 3. Print the summary banner. The "Prompt:" line is "(default)" — `connect`
    //    writes no system-prompt file.
    print_launch_banner(&workdir, &tmux_name, None);

    // 4. Create the tmux host idempotently. `new-session -A` attaches to an
    //    existing session and creates a detached one (`-d`) otherwise; the
    //    `has-session` probe tells us which happened so `claude` is started
    //    only for a freshly-created session.
    let already_running = tmux_has_session(&tmux_name);
    let new_session = std::process::Command::new("tmux")
        .args(["new-session", "-A", "-d", "-s", &tmux_name, "-c", &workdir])
        .status();
    if !matches!(new_session, Ok(s) if s.success()) {
        anyhow::bail!("failed to create tmux session {tmux_name} in {workdir}");
    }

    // 5. Start bare `claude` inside a freshly-created session. `connect` does
    //    not compose a `--append-system-prompt` — it does no deployment.
    if !already_running {
        let send = std::process::Command::new("tmux")
            .args(["send-keys", "-t", &tmux_name, "claude", "Enter"])
            .status();
        if !matches!(send, Ok(s) if s.success()) {
            anyhow::bail!("tmux session {tmux_name} created but failed to start claude");
        }
    }

    // 6. Attach — takes over the current terminal until the user detaches.
    let status = std::process::Command::new("tmux")
        .args(["attach-session", "-t", &tmux_name])
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux attach-session exited with failure");
    }
    Ok(())
}

/// Compute the fallback `tmpm-<folder>` session name for a project directory.
///
/// Why: when the daemon is unreachable `tm launch` still needs a tmux session
/// name; deriving it from the project folder keeps the offline name identical
/// to the one the daemon would assign for the same directory.
/// What: returns `name_from_dir(path)` (`tmpm-<sanitized-folder>`).
/// Test: `fallback_session_name_has_tmpm_prefix`,
/// `fallback_session_name_uses_folder`.
fn fallback_session_name(path: &std::path::Path) -> String {
    trusty_mpm_core::names::name_from_dir(path)
}

/// Left indent applied to every line of the full-screen launch banner.
const BANNER_INDENT: &str = "   ";

/// Width of the session-info separator line drawn in the launch banner.
const BANNER_SEPARATOR_WIDTH: usize = 53;

/// The ASCII-art robot mascot drawn at the top of the launch banner.
///
/// Why: a recognizable centerpiece gives `tm launch` the same "the tool has
/// taken over the terminal" feel as claude-mpm's startup screen.
/// What: a multi-line string-art robot; each line is printed verbatim with the
/// shared [`BANNER_INDENT`].
const BANNER_ROBOT: &[&str] = &[
    "                                             ",
    "                    .}##-                    ",
    "                    }#+#}                    ",
    "                   .}#-#}.                   ",
    "               ^]}#}##+##}#}]^               ",
    "            ^}}]<^++]#<#]++^^<}}<            ",
    "          <}]^++++++<#}#<++---+^]}]          ",
    "        ^}]+--++++++<###<+---+---+<}^        ",
    "       <}<+---++---+<###<-+--------^}<       ",
    "      -}<+-+^<<^+---+}##+---+^^<+---^}+      ",
    "     .]}++<}}<<]}]+-+}##--+]}]<<]}<--]].     ",
    "   ^#}#]^]].     ^#^-}##-^#<      ]]+]#}#^   ",
    "   }}]#]<#+       ]]+}#}-]}       -#<]#<}}   ",
    " ]##}<#]^}^      .}<-<#<.<}-      ^}^<#<}##] ",
    "<}^}}<#]-^}}-   <#<--<#<--<#<   -}}^-<}^}}+}<",
    "-#]}}^#]---^]##}^---.^#^-..-^}##]+..-]#^}}]#-",
    "  +#}^#]-----.----....+..............<#^]#+  ",
    "   }}^#]-----..--....................<}^}}   ",
    "   }}<#]--...--........   ........ ..<#^}}   ",
    "   ]}<#]-....--.........    .........<#^}]   ",
    "   ^}}#]^^++--------------.-...---++^]#]}^   ",
    "    }}]]}}}}##}}}}}}}}}}}}}}}}}##}}]]]]}}    ",
    "    }}+-----<#+     .   .     -#<-.----}]    ",
    "    -#<-----<}+  ..           -}<-...-<#-    ",
    "     .}}<+--<#+..             -}<-.-^}}.     ",
    "        ^}#}##}}}}}}}}}}}}}}}}}##}#}^        ",
    "                                             ",
];

/// The block-character "TRUSTY" wordmark drawn below the robot.
///
/// Why: a large title makes the banner read as a deliberate splash screen
/// rather than a stray log line.
/// What: six rows of unicode block-drawing glyphs plus a spaced-out subtitle.
const BANNER_TITLE: &[&str] = &[
    "████████╗██████╗ ██╗   ██╗███████╗████████╗██╗   ██╗",
    "╚══██╔══╝██╔══██╗██║   ██║██╔════╝╚══██╔══╝╚██╗ ██╔╝",
    "   ██║   ██████╔╝██║   ██║███████╗   ██║    ╚████╔╝ ",
    "   ██║   ██╔══██╗██║   ██║╚════██║   ██║     ╚██╔╝  ",
    "   ██║   ██║  ██║╚██████╔╝███████║   ██║      ██║   ",
    "   ╚═╝   ╚═╝  ╚═╝ ╚═════╝ ╚══════╝   ╚═╝      ╚═╝   ",
    "             M U L T I - A G E N T   P M",
];

/// Query the terminal width in columns, falling back to 80 when unknown.
///
/// Why: the launch banner clears the screen and the caller may want the width
/// for future centering; a robust width probe avoids panics on pipes/CI.
/// What: issues a `TIOCGWINSZ` ioctl on stdout, then falls back to the
/// `$COLUMNS` environment variable, then to a hard-coded 80.
/// Test: `terminal_width_is_positive` asserts the result is always > 0.
fn terminal_width() -> usize {
    // SAFETY: `winsize` is a plain-old-data struct; `ioctl` only writes into it
    // and we check the return code before reading the result.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &raw mut ws) == 0 && ws.ws_col > 0 {
            return ws.ws_col as usize;
        }
    }
    if let Ok(cols) = std::env::var("COLUMNS")
        && let Ok(n) = cols.parse::<usize>()
        && n > 0
    {
        return n;
    }
    80
}

/// Render the full-screen `tm launch` banner into a single string.
///
/// Why: keeping the banner pure (string in, string out) makes it trivially
/// testable and lets [`print_launch_banner`] stay a thin print wrapper.
/// What: builds the cleared-screen escape sequence, the ASCII robot, the
/// "TRUSTY" wordmark, and an indented session-info block. When
/// `reconnect_session` is `Some`, a `Status:` row is added and the closing
/// action line reads "Reconnecting..." instead of "Launching claude...".
/// Test: `launch_banner_contains_session_fields`,
/// `launch_banner_marks_reconnect`.
fn render_launch_banner(
    workdir: &str,
    tmux_name: &str,
    prompt_path: Option<&std::path::Path>,
    reconnect_session: Option<&str>,
) -> String {
    let mut out = String::new();
    // Clear the screen and home the cursor so the banner owns the terminal.
    out.push_str("\x1B[2J\x1B[1;1H");

    out.push('\n');
    for line in BANNER_ROBOT {
        out.push_str(BANNER_INDENT);
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    for line in BANNER_TITLE {
        out.push_str(BANNER_INDENT);
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');

    let separator = "─".repeat(BANNER_SEPARATOR_WIDTH);
    let field =
        |label: &str, value: &str| -> String { format!("{BANNER_INDENT}{label:<9}:  {value}\n") };

    let memory = detect_memory();
    let search = detect_tool("trusty-search");
    let prompt = match prompt_path {
        Some(p) => p.display().to_string(),
        None => "(default)".to_string(),
    };

    out.push_str(BANNER_INDENT);
    out.push_str(&separator);
    out.push('\n');
    out.push_str(&field("Project", workdir));
    out.push_str(&field("Session", tmux_name));
    if let Some(session) = reconnect_session {
        out.push_str(&field(
            "Status",
            &format!("↩  reconnecting to existing session ({session})"),
        ));
    } else {
        out.push_str(&field("Memory", &format!("{memory}  ✓")));
        out.push_str(&field("Search", &format!("{search}  ✓")));
        out.push_str(&field("Prompt", &prompt));
    }
    out.push_str(BANNER_INDENT);
    out.push_str(&separator);
    out.push('\n');
    out.push('\n');

    let action = if reconnect_session.is_some() {
        "Reconnecting..."
    } else {
        "Launching claude..."
    };
    out.push_str(BANNER_INDENT);
    out.push_str(action);
    out.push('\n');
    out
}

/// Print the full-screen `tm launch` banner, then pause briefly.
///
/// Why: `tm launch` should give the operator a readable splash screen before
/// the terminal is taken over by `claude`/`tmux`.
/// What: clears the screen, prints the ASCII robot, the "TRUSTY" wordmark, and
/// the indented session-info block, queries the terminal width once (kept for
/// future centering), then sleeps one second so the banner is legible.
/// Test: `launch_banner_does_not_panic`.
fn print_launch_banner(workdir: &str, tmux_name: &str, prompt_path: Option<&std::path::Path>) {
    let _ = terminal_width();
    print!(
        "{}",
        render_launch_banner(workdir, tmux_name, prompt_path, None)
    );
    let _ = std::io::Write::flush(&mut std::io::stdout());
    std::thread::sleep(std::time::Duration::from_secs(1));
}

/// Find a live session whose `workdir` matches `workdir` via `GET /sessions`.
///
/// Why: `tm launch` should reconnect to an existing session for a directory
/// rather than spawning a duplicate; the daemon owns the session registry, so
/// the match must be resolved against the live `GET /sessions` list.
/// What: fetches `GET /sessions`, normalizes each `workdir` (strip trailing
/// slash, canonicalize) and compares against the normalized target; returns the
/// first matching session's `tmux_name`, or `None` when the daemon is
/// unreachable or no session matches.
/// Test: `normalize_workdir_strips_trailing_slash`.
async fn find_existing_session(
    client: &reqwest::Client,
    url: &str,
    workdir: &str,
) -> Option<String> {
    /// One session row including its tmux name, as returned by `GET /sessions`.
    #[derive(Deserialize)]
    struct Row {
        #[serde(default)]
        workdir: String,
        #[serde(default)]
        tmux_name: String,
    }

    let target = normalize_workdir(workdir);
    let resp = client.get(format!("{url}/sessions")).send().await.ok()?;
    let rows: Vec<Row> = resp.error_for_status().ok()?.json().await.ok()?;
    rows.into_iter()
        .find(|r| !r.tmux_name.is_empty() && normalize_workdir(&r.workdir) == target)
        .map(|r| r.tmux_name)
}

/// Normalize a working-directory path for equality comparison.
///
/// Why: two paths can name the same directory yet differ textually (trailing
/// slash, relative vs absolute, symlinks); `tm launch` reconnect detection must
/// treat them as equal.
/// What: canonicalizes the path when it exists, otherwise strips a trailing
/// slash from the lossy string form.
/// Test: `normalize_workdir_strips_trailing_slash`.
fn normalize_workdir(workdir: &str) -> String {
    let path = std::path::Path::new(workdir);
    if let Ok(canonical) = path.canonicalize() {
        return canonical.to_string_lossy().to_string();
    }
    workdir.trim_end_matches('/').to_string()
}

/// Check whether a tmux session named `name` currently exists.
///
/// Why: the daemon may hold a stale session record after its tmux session has
/// exited; `tm launch` must verify the tmux session is live before attaching,
/// otherwise it would fall through to a normal launch.
/// What: runs `tmux has-session -t <name>` and returns true on exit code 0.
/// Test: covered indirectly by the launch reconnect integration path.
fn tmux_has_session(name: &str) -> bool {
    matches!(
        std::process::Command::new("tmux")
            .args(["has-session", "-t", name])
            .status(),
        Ok(status) if status.success()
    )
}

/// Print the full-screen `tm launch` banner with a "reconnecting" status line.
///
/// Why: when `tm launch` attaches to a pre-existing session the operator should
/// see that no new session was created.
/// What: prints the same full-screen banner as [`print_launch_banner`] but with
/// a `Status:` row noting the reconnect, then pauses one second so the banner
/// is legible before `tmux` takes over.
/// Test: `launch_reconnect_banner_does_not_panic`.
fn print_launch_banner_reconnecting(workdir: &str, tmux_name: &str) {
    let _ = terminal_width();
    print!(
        "{}",
        render_launch_banner(workdir, tmux_name, None, Some(tmux_name))
    );
    let _ = std::io::Write::flush(&mut std::io::stdout());
    std::thread::sleep(std::time::Duration::from_secs(1));
}

/// Detect whether the `trusty-memory` MCP integration is available.
///
/// Why: the launch banner reports which trusty companions are wired up.
/// What: returns `"trusty-memory"` when `~/.config/trusty-memory` exists or the
/// `trusty-memory` binary is on `PATH`, else `"(not detected)"`.
/// Test: covered indirectly by `launch_banner_does_not_panic`.
fn detect_memory() -> String {
    let config = dirs_config_dir().map(|c| c.join("trusty-memory"));
    let has_config = config.map(|c| c.exists()).unwrap_or(false);
    if has_config || binary_on_path("trusty-memory") {
        "trusty-memory".to_string()
    } else {
        "(not detected)".to_string()
    }
}

/// Detect whether a named tool binary is available on `PATH`.
///
/// Why: the launch banner reports `trusty-search` availability.
/// What: returns the tool name when its binary is on `PATH`, else
/// `"(not detected)"`.
/// Test: covered indirectly by `launch_banner_does_not_panic`.
fn detect_tool(name: &str) -> String {
    if binary_on_path(name) {
        name.to_string()
    } else {
        "(not detected)".to_string()
    }
}

/// Return the user's config directory (`~/.config` on Linux/macOS).
///
/// Why: `detect_memory` probes `~/.config/trusty-memory` without pulling in a
/// platform-dirs dependency.
/// What: returns `$XDG_CONFIG_HOME` when set, else `$HOME/.config`.
/// Test: covered indirectly by `launch_banner_does_not_panic`.
fn dirs_config_dir() -> Option<std::path::PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(std::path::PathBuf::from(xdg));
    }
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
}

/// Check whether an executable named `name` exists on `PATH`.
///
/// Why: banner detection of `trusty-memory` / `trusty-search` needs a
/// dependency-free `which`-style lookup.
/// What: scans each `PATH` entry for an existing `name` file.
/// Test: covered indirectly by `launch_banner_does_not_panic`.
fn binary_on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
}

/// Resolve a session id-or-name to its UUID string via `GET /sessions`.
///
/// Why: `session events` calls `GET /sessions/{id}/events`, which requires a
/// UUID; operators may pass a friendly `tmpm-<adj>-<noun>` name instead, so the
/// name must be resolved against the live session list first.
/// What: fetches `GET /sessions`, matching `id.0` or `tmux_name` against the
/// argument; returns `Some(uuid)` on a hit, `None` when no session matches.
/// Test: covered indirectly by `cli_parses_session_events`.
async fn resolve_session_id(
    client: &reqwest::Client,
    url: &str,
    id_or_name: &str,
) -> anyhow::Result<Option<String>> {
    #[derive(Deserialize)]
    struct Body {
        sessions: Vec<serde_json::Value>,
    }
    let body: Body = client
        .get(format!("{url}/sessions"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let found = body.sessions.iter().find_map(|s| {
        // `SessionId` is a single-field newtype: serde serializes it as a bare
        // UUID string, so `id` is read directly (not as `{"0": ...}`).
        let uuid = s.get("id").and_then(|v| v.as_str());
        let name = s.get("tmux_name").and_then(|v| v.as_str());
        match uuid {
            Some(u) if u == id_or_name || name == Some(id_or_name) => Some(u.to_string()),
            _ => None,
        }
    });
    Ok(found)
}

/// Summarize an opaque hook-event payload into a single short line.
///
/// Why: `session events` prints one row per event; a full JSON payload would
/// wrap the terminal, so a compact summary keeps rows readable.
/// What: shows the `tool` field when present, otherwise a truncated JSON dump.
/// Test: covered by `event_summary_*` unit tests.
fn event_summary(payload: &serde_json::Value) -> String {
    if let Some(tool) = payload.get("tool").and_then(|v| v.as_str()) {
        return format!("tool={tool}");
    }
    let dump = payload.to_string();
    if dump.len() > 60 {
        format!("{}…", &dump[..60])
    } else {
        dump
    }
}

/// Print a one-line compression-savings note when an output was summarized.
///
/// Why: `session run --summarize` and `session output --summarize` should tell
/// the operator how much the summary saved, completing the visible feedback for
/// the "summarize output" step of the user cycle.
/// What: when the response body carries a non-null `compress_level`, prints
/// `[summarized: A → B bytes (N% reduction)]` to a fresh line; does nothing when
/// the output was returned raw (no compression applied).
/// Test: `compression_stats_line_*` unit tests.
fn print_compression_stats(body: &serde_json::Value) {
    if body
        .get("compress_level")
        .and_then(|v| v.as_str())
        .is_none()
    {
        return;
    }
    let original = body
        .get("original_bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let compressed = body
        .get("compressed_bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let reduction = (100 * original.saturating_sub(compressed))
        .checked_div(original)
        .unwrap_or(0);
    println!("\n[summarized: {original} \u{2192} {compressed} bytes ({reduction}% reduction)]");
}

/// Resolve `target` to a session and either print JSON or open the TUI.
///
/// Why: `tm attach` is the ergonomic entry point for an *existing* session —
/// operators type a short name or path rather than a full UUID.
/// What: Fetches `/sessions`, resolves via `resolve_target`, then either
/// serialises to JSON (`--json`) or launches the TUI pre-focused on the match.
/// Test: Resolution logic is tested in `trusty-mpm-core::connect`; here we
/// test CLI flag parsing only (see unit tests).
async fn attach_cmd(
    client: &reqwest::Client,
    url: &str,
    target: &str,
    json: bool,
) -> anyhow::Result<()> {
    use trusty_mpm_core::{ResolveResult, SessionSummary, resolve_target};

    // The daemon wraps the session array in a `{ "sessions": [...] }` envelope.
    let resp: serde_json::Value = client
        .get(format!("{url}/sessions"))
        .send()
        .await?
        .json()
        .await?;
    let empty = vec![];
    let raw = resp
        .get("sessions")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);

    // Parse sessions array — be tolerant of missing fields. `SessionId` is
    // serialized as a bare UUID string; the friendly name is `tmux_name`.
    let sessions: Vec<SessionSummary> = raw
        .iter()
        .filter_map(|v| {
            Some(SessionSummary {
                id: v["id"].as_str()?.to_string(),
                name: v["tmux_name"].as_str().map(str::to_string),
                workdir: v["workdir"].as_str().unwrap_or("").to_string(),
                last_active: v["last_seen"]["secs_since_epoch"].as_u64().unwrap_or(0),
            })
        })
        .collect();

    match resolve_target(target, &sessions) {
        ResolveResult::Found(id) => {
            if json {
                // Find the full session JSON and print it.
                if let Some(s) = raw.iter().find(|v| v["id"].as_str() == Some(&id)) {
                    println!("{}", serde_json::to_string_pretty(s)?);
                }
                return Ok(());
            }
            // Launch the TUI focused on this session.
            let resolved_url = trusty_mpm_core::resolve_daemon_url(Some(url));
            trusty_mpm_tui::run_focused(resolved_url, 1000, Some(id)).await
        }
        ResolveResult::Ambiguous(ids) => {
            eprintln!(
                "Ambiguous target '{target}' — matched {} sessions:",
                ids.len()
            );
            for id in &ids {
                eprintln!("  {id}");
            }
            std::process::exit(1);
        }
        ResolveResult::NotFound => {
            eprintln!("No session matched '{target}'.");
            if !sessions.is_empty() {
                eprintln!("Available sessions:");
                for s in &sessions {
                    let name = s.name.as_deref().unwrap_or("-");
                    eprintln!("  {} ({})  {}", s.id, name, s.workdir);
                }
            }
            std::process::exit(1);
        }
    }
}

/// `overseer` subcommand — inspect the session overseer.
///
/// Why: operators need to see whether oversight is active without the TUI.
/// What: `Status` calls `GET /overseer` and prints the enabled flag and
/// handler type.
/// Test: `cli_parses_overseer_status`.
async fn overseer(
    client: &reqwest::Client,
    url: &str,
    action: OverseerAction,
) -> anyhow::Result<()> {
    match action {
        OverseerAction::Status => {
            let body: serde_json::Value = client
                .get(format!("{url}/overseer"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let overseer = &body["overseer"];
            let enabled = overseer
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let handler = overseer
                .get("handler")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            println!(
                "overseer: {} (handler: {handler})",
                if enabled { "enabled" } else { "disabled" }
            );
        }
    }
    Ok(())
}

/// Run the instruction merge pipeline and stash the merged result on disk.
///
/// Why: both `session start` and `session instructions` need to compose the
/// effective launch instructions; centralizing the pipeline call plus the
/// inspectable-stash write keeps the two call sites consistent.
/// What: builds a [`PipelineInput`] from `fw` and `project_dir`, runs
/// [`build_instructions`], writes the merged text to
/// `<project>/.trusty-mpm/last-instructions.md`, and returns the output along
/// with the stash path.
/// Test: covered indirectly by `cli_parses_session_instructions` and the
/// `instruction_pipeline` unit tests in trusty-mpm-core.
fn compose_session_instructions(
    fw: &trusty_mpm_core::paths::FrameworkPaths,
    project_dir: &std::path::Path,
) -> anyhow::Result<(
    trusty_mpm_core::instruction_pipeline::PipelineOutput,
    std::path::PathBuf,
)> {
    use trusty_mpm_core::instruction_pipeline::{PipelineInput, build_instructions};

    let input = PipelineInput {
        framework_instructions_path: fw.framework_instructions_path(),
        agents_dir: fw.claude_agents_dir(),
        claude_md_path: project_dir.join("CLAUDE.md"),
    };
    let output = build_instructions(&input)?;

    let stash_dir = project_dir.join(".trusty-mpm");
    std::fs::create_dir_all(&stash_dir)?;
    let stash = stash_dir.join("last-instructions.md");
    std::fs::write(&stash, &output.merged)?;

    Ok((output, stash))
}

/// Inspect or configure the token-use optimizer.
///
/// Why: the optimizer policy is framework-managed on disk; `Status` reads the
/// daemon's live view via `GET /optimizer`, while `Set` rewrites the policy
/// file itself (`~/.trusty-mpm/framework/hooks/optimizer.toml`) — the daemon's
/// watcher then reloads it.
/// What: `Status` prints the current config; `Set` writes a new `[default]`
/// level into the policy file, creating the `hooks/` directory if needed.
/// Test: `cli_parses_optimizer_status`, `cli_parses_optimizer_set`.
async fn optimizer(
    client: &reqwest::Client,
    url: &str,
    action: OptimizerAction,
) -> anyhow::Result<()> {
    match action {
        OptimizerAction::Status => {
            let body: serde_json::Value = client
                .get(format!("{url}/optimizer"))
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&body["optimizer"])?);
        }
        OptimizerAction::Set { level } => {
            let level_name = match level {
                CliCompressionLevel::Off => "Off",
                CliCompressionLevel::Trim => "Trim",
                CliCompressionLevel::Summarise => "Summarise",
                CliCompressionLevel::Caveman => "Caveman",
            };
            let paths = trusty_mpm_core::paths::FrameworkPaths::default();
            let path = paths.optimizer_config();
            std::fs::create_dir_all(&paths.hooks)?;
            let contents = format!(
                "# trusty-mpm token optimizer — framework hook configuration\n\
                 # Edited by: trusty-mpm optimizer set\n\n\
                 [default]\nlevel = \"{level_name}\"\n\n\
                 [tools]\n"
            );
            std::fs::write(&path, contents)?;
            println!("optimizer level set to {level_name} ({})", path.display());
        }
    }
    Ok(())
}

/// Render a `SessionId` newtype JSON value into a short, human id.
///
/// Why: the daemon serializes `SessionId` as `{"0": "<uuid>"}`; the CLI shows
/// only the first 8 characters so rows stay compact.
/// What: extracts the inner UUID string and truncates it, falling back to a
/// placeholder if the shape is unexpected.
/// Test: covered by the `short_id_*` unit tests below.
fn short_id(value: &serde_json::Value) -> String {
    value
        .get("0")
        .and_then(|v| v.as_str())
        .map(|s| s.chars().take(8).collect::<String>())
        .unwrap_or_else(|| "--------".to_string())
}

/// `status` subcommand — probe daemon health and list sessions.
///
/// Why: the first thing an operator runs to see if the daemon is alive.
/// What: `GET /health` then `GET /sessions`, printing one line per session.
/// Test: run against a live daemon; "daemon: unreachable" when it is down.
async fn status(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    if !daemon_healthy(client, url).await {
        println!("daemon: unreachable");
        return Ok(());
    }
    print_status(client, url).await
}

/// Probe the daemon's `/health` endpoint for liveness.
///
/// Why: both `start` (to decide whether to spawn) and the post-spawn wait loop
/// need a single yes/no liveness check; factoring it out keeps the two call
/// sites identical and the intent obvious.
/// What: issues `GET {url}/health`, returning `true` only on a 2xx response and
/// `false` on any transport error or non-success status.
/// Test: covered indirectly by running `tm start` against a live/dead daemon;
/// the logic mirrors the probe already used by `status`.
async fn daemon_healthy(client: &reqwest::Client, url: &str) -> bool {
    // Verify /health is 200 AND /sessions is 200. Code-intelligence on the same
    // port returns 200 for /health but 404 for /sessions, so checking both
    // discriminates our daemon from other HTTP servers on the same port.
    let health_ok = match client.get(format!("{url}/health")).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => return false,
    };
    if !health_ok {
        return false;
    }
    match client.get(format!("{url}/sessions")).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

/// `start` subcommand — ensure the daemon is running, then show status.
///
/// Why: operators want one command that is safe to run repeatedly — it brings
/// the daemon up if it is down and is a no-op (just status) if it is already
/// up, so `tm start` can sit in shell profiles and setup scripts.
/// What: probes `/health`; if healthy, prints "Daemon already running" plus the
/// same listing as `tm status`. If not, opens `~/.trusty-mpm/daemon.log`, spawns
/// `tm daemon` detached with stdout/stderr appended to that log, polls `/health`
/// for up to 5 seconds, then prints "Starting daemon... done" and the status.
/// Test: `cli_parses_start` covers parsing; the spawn/wait path is exercised by
/// running `tm start` against a clean environment.
async fn restart(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    if daemon_healthy(client, url).await {
        print!("Stopping daemon... ");
        use std::io::Write as _;
        std::io::stdout().flush().ok();
        // Kill any running tm/trusty-mpm daemon processes.
        std::process::Command::new("pkill")
            .args(["-f", "tm daemon"])
            .status()
            .ok();
        std::process::Command::new("pkill")
            .args(["-f", "trusty-mpm daemon"])
            .status()
            .ok();
        // Wait until the port is free (up to 3 s).
        for _ in 0..6 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if !daemon_healthy(client, url).await {
                break;
            }
        }
        println!("done");
    }
    start(client, url).await
}

/// `stop` subcommand — terminate every running trusty-mpm daemon.
///
/// Why: pairs with `start` so operators can shut the daemon down without a
/// full `restart` cycle. Mirrors `trusty-search stop` and `trusty-memory
/// stop` so the three daemons share a single mental model. The daemon
/// writes its address into `~/.trusty-mpm/daemon.lock`, but the lock-file
/// PID can go stale (a SIGKILL leaves it behind), so the source of truth
/// is the live process table.
/// What: walks the process table via `sysinfo` for every `trusty-mpm` or
/// `tm` process whose argv contains `daemon`, sends SIGTERM, polls 5 s for
/// them to exit, then SIGKILLs stragglers. Removes the stale lock file
/// when every targeted process has exited.
/// Test: `cli_parses_stop`; the spawn/kill path is exercised by running
/// `tm start` followed by `tm stop` against a clean environment.
async fn stop_daemon() -> anyhow::Result<()> {
    use std::time::{Duration, Instant};

    let targets = find_daemon_pids();
    if targets.is_empty() {
        anyhow::bail!("No daemon running");
    }

    println!(
        "Stopping trusty-mpm daemon ({} process(es): {:?})…",
        targets.len(),
        targets
    );

    // Phase 1: SIGTERM all targets.
    for pid in &targets {
        let _ = send_signal(*pid, "TERM");
    }

    // Phase 2: poll up to 5 s for every targeted PID to exit.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let any_alive = targets.iter().any(|p| pid_alive(*p));
        if !any_alive {
            println!("Daemon stopped");
            cleanup_lock_file();
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
    }

    // Phase 3: SIGKILL anything still alive.
    let stragglers: Vec<u32> = targets.iter().copied().filter(|p| pid_alive(*p)).collect();
    if !stragglers.is_empty() {
        println!(
            "{} process(es) ignored SIGTERM — sending SIGKILL: {:?}",
            stragglers.len(),
            stragglers
        );
        for pid in &stragglers {
            let _ = send_signal(*pid, "KILL");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    if targets.iter().any(|p| pid_alive(*p)) {
        println!("Daemon may still be shutting down");
    } else {
        println!("Daemon stopped");
        cleanup_lock_file();
    }
    Ok(())
}

/// Remove the stale `~/.trusty-mpm/daemon.lock` after a successful stop.
///
/// Why: the daemon writes its lock file on bind and removes it on graceful
/// shutdown, but SIGKILL leaves it behind; the next `tm status` call would
/// then chase a dead address through the discovery timeout.
/// What: best-effort `fs::remove_file` of `lock_file_path()`.
/// Test: covered indirectly by the stop integration path.
fn cleanup_lock_file() {
    let path = trusty_mpm_core::lock_file_path();
    let _ = std::fs::remove_file(&path);
}

/// Walk the process table and return every trusty-mpm daemon PID.
///
/// Why: `tm stop` needs to find the daemon regardless of which binary alias
/// (`trusty-mpm` or `tm`) was used to launch it. Matching argv on `daemon`
/// filters out short-lived CLI invocations (`tm status`, `tm doctor`) whose
/// process names also match.
/// What: refreshes the process list once, matches `name() in {trusty-mpm,
/// tm}` AND `cmd().contains("daemon")`. Excludes the current process.
/// Test: covered indirectly by the stop integration path.
fn find_daemon_pids() -> Vec<u32> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    );
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let me = std::process::id();
    let mut out = Vec::new();
    for (pid, proc_) in sys.processes() {
        let raw = pid.as_u32();
        if raw == me {
            continue;
        }
        let name = proc_.name().to_string_lossy();
        let is_tm_binary = name == "trusty-mpm" || name == "tm";
        if !is_tm_binary {
            continue;
        }
        let is_daemon = proc_.cmd().iter().any(|a| a.to_string_lossy() == "daemon");
        if is_daemon {
            out.push(raw);
        }
    }
    out
}

/// Send a POSIX signal to a PID by shelling out to `/bin/kill`.
///
/// Why: avoid adding a `nix` dependency for the sole purpose of sending
/// SIGTERM / SIGKILL. `kill -SIGNAL pid` is universally available on
/// every Unix the daemon supports (macOS, Linux).
/// What: spawns `kill -<sig> <pid>` and returns an error if the exit
/// status is non-zero.
/// Test: covered indirectly by the stop integration path.
#[cfg(unix)]
fn send_signal(pid: u32, sig: &str) -> std::io::Result<()> {
    let status = std::process::Command::new("kill")
        .arg(format!("-{sig}"))
        .arg(pid.to_string())
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "kill -{sig} {pid} exited {status}"
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn send_signal(_pid: u32, _sig: &str) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "signals unsupported on this platform",
    ))
}

/// Check whether a PID is still alive (Unix only).
///
/// Why: the SIGTERM-then-SIGKILL poll loop needs a portable "is this PID
/// alive?" probe. `kill -0` returns success when the process exists.
/// What: invokes `kill -0 <pid>` via `Command` so no extra dep is needed.
/// Test: covered indirectly by the stop integration path.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

async fn start(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    // Prefer the lock file URL — it's the address our daemon actually bound to,
    // not whatever default URL the CLI was given (which may point at a different
    // process on the same port, e.g. code-intelligence on :7880).
    let lock_url = trusty_mpm_core::resolve_daemon_url(None);
    let check_url = if lock_url != trusty_mpm_core::DEFAULT_DAEMON_URL
        || url == trusty_mpm_core::DEFAULT_DAEMON_URL
    {
        lock_url.clone()
    } else {
        url.to_string()
    };
    if daemon_healthy(client, &check_url).await {
        println!("Daemon already running on {check_url}");
        return print_status(client, &check_url).await;
    }

    // Resolve the log file under `~/.trusty-mpm/`, creating the dir if absent.
    let root = trusty_mpm_core::paths::FrameworkPaths::default().root;
    std::fs::create_dir_all(&root)?;
    let log_path = root.join("daemon.log");
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let stderr = stdout.try_clone()?;

    // Spawn `tm daemon` detached, pointed at our own binary so the spawned
    // daemon is always the same build as this CLI. We do NOT pass TRUSTY_MPM_ADDR
    // — the daemon picks its own port (falling back to ephemeral if needed) and
    // records the actual address in the lock file. We discover it from there.
    let lock_path = trusty_mpm_core::lock_file_path();
    // Remove any stale lock file before spawning so we can detect the new write.
    let _ = std::fs::remove_file(&lock_path);
    let exe = std::env::current_exe()?;
    std::process::Command::new(&exe)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(stderr))
        .spawn()?;

    // Poll the lock file for up to 5 seconds. The daemon writes it as soon as
    // it has a bound address — use that URL for the health check.
    print!("Starting daemon... ");
    use std::io::Write as _;
    std::io::stdout().flush().ok();
    let mut healthy = false;
    // Use the lock-file URL if available, fall back to the cli url.
    let mut actual_url = url.to_string();
    for _ in 0..10 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        // Re-resolve: once the lock file appears it wins over the default.
        actual_url = trusty_mpm_core::resolve_daemon_url(None);
        if daemon_healthy(client, &actual_url).await {
            healthy = true;
            break;
        }
    }
    if healthy {
        println!("done");
        if actual_url != url {
            println!("(listening on {actual_url})");
        }
    } else {
        println!("failed");
        println!(
            "daemon did not become healthy within 5s; see {}",
            log_path.display()
        );
        return Ok(());
    }

    print_status(client, &actual_url).await
}

/// Print the daemon health line, session listing, and Telegram-bot note.
///
/// Why: `status` and `start` must show identical state; sharing one printer
/// keeps the two outputs from drifting and adds the Telegram note in one place.
/// What: prints `daemon: ok`, one line per session from `GET /sessions`, and
/// `Telegram bot active` when a bot token is resolvable from the environment or
/// a local `.env.local` / `.env` file.
/// Test: covered indirectly by running `tm status` / `tm start` against a live
/// daemon; Telegram token resolution is tested in `trusty-mpm-telegram`.
async fn print_status(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    println!("daemon: ok");

    #[derive(Deserialize)]
    struct Body {
        sessions: Vec<SessionRow>,
    }
    let body: Body = client
        .get(format!("{url}/sessions"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    for s in &body.sessions {
        let status = s.status.as_str().unwrap_or("unknown");
        println!(
            "{} {} {} ({} delegations)",
            short_id(&s.id),
            status,
            s.workdir,
            s.active_delegations
        );
    }

    if trusty_mpm_telegram::resolve_token("TELEGRAM_BOT_TOKEN").is_some() {
        println!("Telegram bot active");
    }
    Ok(())
}

/// `events` subcommand — print the recent hook-event feed.
///
/// Why: gives operators a quick tail of daemon activity without the TUI. The
/// daemon serves a live SSE stream at `/events`; this CLI command polls the
/// legacy snapshot at `/events/poll`, which mirrors the historical behaviour.
/// What: `GET /events/poll`, printing `{timestamp} {session_short} {event}`.
/// Test: run against a daemon that has ingested hook events.
async fn events(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct Body {
        events: Vec<EventRow>,
    }
    let body: Body = client
        .get(format!("{url}/events/poll"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    for e in &body.events {
        println!("{} {} {}", e.at, short_id(&e.session), e.event);
    }
    Ok(())
}

/// `doctor` subcommand — run and print the full system diagnostic.
///
/// Why: a misconfigured trusty-mpm stack fails confusingly; `tm doctor` runs
/// every health probe in one command and prints a formatted verdict so the
/// operator can confirm — or fix — a broken install at a glance.
/// What: runs [`TrustyCommand::Doctor`] through the shared [`CommandExecutor`]
/// (which calls `GET /api/v1/doctor`), then prints one status-tagged line per
/// check plus an overall verdict. An unreachable daemon prints an error line.
/// Test: `cli_parses_doctor` covers parsing; the report path is covered by the
/// executor's `execute_doctor_against_test_daemon` test.
async fn doctor(url: &str) -> anyhow::Result<()> {
    use trusty_mpm_client::{CommandExecutor, CommandResult, TrustyCommand};
    use trusty_mpm_core::doctor::CheckStatus;

    let executor = CommandExecutor::new(url.to_string());
    match executor.execute(TrustyCommand::Doctor).await {
        CommandResult::Doctor(report) => {
            println!("trusty-mpm doctor");
            for check in &report.checks {
                println!(
                    "  {} {:<13} {}",
                    status_icon(check.status),
                    check.name,
                    check.message,
                );
            }
            println!(
                "\noverall: {} {}",
                status_icon(report.overall),
                match report.overall {
                    CheckStatus::Ok => "all checks passed",
                    CheckStatus::Warn => "passed with warnings",
                    CheckStatus::Fail => "one or more checks failed",
                },
            );
        }
        CommandResult::Error(msg) => eprintln!("doctor failed: {msg}"),
        other => eprintln!("doctor: unexpected result {other:?}"),
    }
    Ok(())
}

/// Render a [`CheckStatus`] as a status icon for terminal output.
///
/// Why: the `tm doctor` table marks each check with a glanceable symbol; one
/// helper keeps the mapping consistent between the per-check lines and the
/// overall verdict.
/// What: `Ok → ✅`, `Warn → ⚠️`, `Fail → ❌`.
/// Test: covered indirectly by the `doctor` output.
fn status_icon(status: trusty_mpm_core::doctor::CheckStatus) -> &'static str {
    use trusty_mpm_core::doctor::CheckStatus;
    match status {
        CheckStatus::Ok => "\u{2705}",
        CheckStatus::Warn => "\u{26a0}\u{fe0f}",
        CheckStatus::Fail => "\u{274c}",
    }
}

/// `daemon` subcommand — run the HTTP daemon (or MCP server) with auto port
/// selection, lock-file service discovery, and optional Tailscale exposure.
///
/// Why: the daemon must start even when the configured port is busy (auto
/// fallback to an ephemeral port), publish its real address so clients can
/// find it (lock file), and optionally be reachable over Tailscale.
/// What: in MCP mode delegates straight to `run_mcp`; otherwise binds `addr`
/// (falling back to `127.0.0.1:0` on `AddrInUse`), optionally binds a second
/// Tailscale listener, writes the lock file, registers a Ctrl-C handler that
/// removes the lock, then serves the API on the primary listener.
/// Test: `cli_parses_daemon_*` cover flag parsing; the bind/serve path is
/// exercised by the daemon e2e suite.
async fn run_daemon(addr: SocketAddr, tailscale: bool, mcp: bool) -> anyhow::Result<()> {
    use std::io::ErrorKind;

    let state = trusty_mpm_daemon::DaemonState::shared();
    if mcp {
        return trusty_mpm_daemon::run_mcp(state).await;
    }

    // Refuse to start a second instance: read the lock-file address, probe
    // `/health`, and bail out cleanly when an existing daemon answers. Without
    // this guard, the `AddrInUse` fallback below would auto-pick an ephemeral
    // port and silently spawn a duplicate daemon that splits traffic with the
    // original. `resolve_daemon_url` already validates the recorded PID is
    // alive (and clears stale lock files), so a `None`-ish result here means
    // either no lock exists or the recorded daemon is dead — proceed normally.
    let recorded_url = trusty_mpm_core::resolve_daemon_url(None);
    if recorded_url != trusty_mpm_core::DEFAULT_DAEMON_URL
        && trusty_common::probe_health(&recorded_url, "/health").await
    {
        eprintln!("trusty-mpm daemon is already running at {recorded_url}");
        return Ok(());
    }

    // Auto port selection: try configured address; fall back to ephemeral.
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == ErrorKind::AddrInUse => {
            tracing::warn!("port {} is busy — selecting an ephemeral port", addr.port());
            tokio::net::TcpListener::bind("127.0.0.1:0").await?
        }
        Err(e) => return Err(e.into()),
    };
    let actual_addr = listener.local_addr()?;
    let base_url = format!("http://{actual_addr}");
    tracing::info!("daemon listening on {base_url}");

    // Optional Tailscale second listener.
    let tailscale_url = if tailscale {
        match get_tailscale_ip() {
            Some(ts_ip) => {
                let ts_addr = format!("{ts_ip}:{}", actual_addr.port());
                match tokio::net::TcpListener::bind(&ts_addr).await {
                    Ok(ts_listener) => {
                        let ts_url = format!("http://{ts_addr}");
                        tracing::info!("Tailscale listener on {ts_url}");
                        // Spawn a second server sharing daemon state.
                        trusty_mpm_daemon::spawn_secondary_listener(
                            trusty_mpm_daemon::DaemonState::shared(),
                            ts_listener,
                        );
                        Some(ts_url)
                    }
                    Err(e) => {
                        tracing::warn!("failed to bind Tailscale address {ts_addr}: {e}");
                        None
                    }
                }
            }
            None => {
                tracing::warn!("--tailscale requested but no Tailscale IP found");
                None
            }
        }
    } else {
        None
    };

    // Write lock file so clients can discover us.
    trusty_mpm_daemon::lock::write_lock(&base_url, tailscale_url.as_deref());

    // Clean up the lock file on shutdown for BOTH Ctrl-C (SIGINT) and SIGTERM.
    // `tm restart` stops the old daemon with `pkill`, which sends SIGTERM — if we
    // only trapped SIGINT the lock file would leak with a dead PID, and the next
    // client's `resolve_daemon_url` would fall back to the default port (often
    // occupied by an unrelated process) and report "daemon unreachable".
    tokio::spawn(async {
        wait_for_shutdown_signal().await;
        trusty_mpm_daemon::lock::remove_lock();
        std::process::exit(0);
    });

    // Auto-start the Telegram bot alongside the daemon when a bot token is
    // configured. `resolve_token` honours `.env.local` → `.env` → the process
    // environment, so a single dotenv file configures both the daemon and the
    // bot. Without a token the daemon runs normally; only a warning is logged.
    spawn_telegram_bot(&base_url);

    trusty_mpm_daemon::serve_http(state, listener).await
}

/// Block until the process receives a shutdown signal (SIGINT or SIGTERM).
///
/// Why: the daemon must remove its lock file on every graceful stop, not just
/// Ctrl-C. `tm restart` stops the old daemon with `pkill`, which delivers
/// SIGTERM; trapping only SIGINT would leak a stale lock file and break daemon
/// discovery for the next client.
/// What: races a `ctrl_c()` future against a Unix SIGTERM stream; on non-Unix
/// platforms (no SIGTERM) it just awaits Ctrl-C.
/// Test: covered indirectly by `tm restart` — the new daemon binds cleanly and
/// the lock file reflects its address rather than the killed daemon's.
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("failed to register SIGTERM handler: {e}");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Spawn the Telegram bot as a background task when a token is configured.
///
/// Why: an operator who has set `TELEGRAM_BOT_TOKEN` expects the bot to come up
/// with the daemon — not as a separate process they must remember to start.
/// What: resolves the token via `trusty_mpm_telegram::resolve_token` (which
/// reads `.env.local`, then `.env`, then the environment). When a token is
/// found the bot's `run` is spawned on a tokio task pointed at `base_url`; when
/// absent a single warning is logged and the daemon continues.
/// Test: token resolution is covered by `trusty-mpm-telegram`'s
/// `resolve_token_*` tests; the spawn path is exercised by running the daemon.
fn spawn_telegram_bot(base_url: &str) {
    match trusty_mpm_telegram::resolve_token("TELEGRAM_BOT_TOKEN") {
        Some(token) => {
            tracing::info!("TELEGRAM_BOT_TOKEN found — starting Telegram bot");
            let url = base_url.to_string();
            tokio::spawn(async move {
                let options = trusty_mpm_telegram::BotOptions::default();
                if let Err(e) = trusty_mpm_telegram::run(url, Some(token), false, options).await {
                    tracing::warn!("Telegram bot exited: {e}");
                }
            });
        }
        None => {
            tracing::warn!(
                "TELEGRAM_BOT_TOKEN not set — Telegram bot not started \
                 (set it in .env.local to enable)"
            );
        }
    }
}

/// Detect the Tailscale IPv4 address by running `tailscale ip -4`.
///
/// Why: Tailscale's IP changes per device; we can't hardcode it. The CLI
/// is the most reliable cross-platform way to query it without adding a
/// Tailscale SDK dependency.
/// What: Spawns `tailscale ip -4`, trims the output, returns it if it
/// looks like an IP address. Returns `None` on any error or if Tailscale
/// is not installed.
/// Test: Hard to test without Tailscale installed; the unit test below
/// checks the happy-path string parsing.
fn get_tailscale_ip() -> Option<String> {
    let output = std::process::Command::new("tailscale")
        .args(["ip", "-4"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let ip = String::from_utf8(output.stdout).ok()?;
    let ip = ip.trim().to_string();
    // Basic sanity check: must look like an IPv4 or Tailscale CGNAT address.
    if ip.contains('.') && !ip.is_empty() {
        Some(ip)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn short_id_extracts_uuid_prefix() {
        // SessionId newtype shape `{"0": "<uuid>"}` → first 8 chars of the uuid.
        let value = serde_json::json!({"0": "abcd1234-5678-90ab-cdef-1234567890ab"});
        assert_eq!(short_id(&value), "abcd1234");
    }

    #[test]
    fn short_id_truncates_to_eight_chars() {
        // Any inner uuid string must collapse to exactly 8 characters.
        let value = serde_json::json!({"0": "0123456789abcdef-rest-ignored"});
        assert_eq!(short_id(&value).chars().count(), 8);
    }

    #[test]
    fn short_id_falls_back_when_field_missing() {
        // Missing `0` key or a scalar value → the placeholder.
        assert_eq!(short_id(&serde_json::json!({})), "--------");
        assert_eq!(short_id(&serde_json::json!("scalar")), "--------");
    }

    #[test]
    fn short_id_falls_back_when_value_not_str() {
        // `0` present but not a string → the placeholder.
        assert_eq!(short_id(&serde_json::json!({"0": 42})), "--------");
    }

    #[test]
    fn cli_parses_status() {
        let cli = Cli::try_parse_from(["trusty-mpm", "status"]).unwrap();
        assert!(matches!(cli.command, Command::Status));
    }

    #[test]
    fn cli_parses_start() {
        let cli = Cli::try_parse_from(["trusty-mpm", "start"]).unwrap();
        assert!(matches!(cli.command, Command::Start));
    }

    #[test]
    fn cli_parses_serve() {
        let cli = Cli::try_parse_from(["trusty-mpm", "serve"]).unwrap();
        assert!(matches!(cli.command, Command::Serve));
    }

    #[test]
    fn cli_parses_stop() {
        let cli = Cli::try_parse_from(["trusty-mpm", "stop"]).unwrap();
        assert!(matches!(cli.command, Command::Stop));
    }

    #[test]
    fn cli_parses_doctor() {
        let cli = Cli::try_parse_from(["trusty-mpm", "doctor"]).unwrap();
        assert!(matches!(cli.command, Command::Doctor));
    }

    #[test]
    fn cli_parses_project_init() {
        let cli = Cli::try_parse_from(["trusty-mpm", "project", "init"]).unwrap();
        match cli.command {
            Command::Project {
                action: ProjectAction::Init { dir },
            } => assert_eq!(dir, None),
            other => panic!("expected project init, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_project_init_with_dir() {
        let cli =
            Cli::try_parse_from(["trusty-mpm", "project", "init", "--dir", "/work/p"]).unwrap();
        match cli.command {
            Command::Project {
                action: ProjectAction::Init { dir },
            } => assert_eq!(dir.as_deref(), Some("/work/p")),
            other => panic!("expected project init, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_project_list() {
        let cli = Cli::try_parse_from(["trusty-mpm", "project", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Project {
                action: ProjectAction::List
            }
        ));
    }

    #[test]
    fn cli_parses_project_info() {
        let cli = Cli::try_parse_from(["trusty-mpm", "project", "info"]).unwrap();
        match cli.command {
            Command::Project {
                action: ProjectAction::Info { dir },
            } => assert_eq!(dir, None),
            other => panic!("expected project info, got {other:?}"),
        }
    }

    #[test]
    fn cli_project_requires_action() {
        // `project` with no action is an error.
        assert!(Cli::try_parse_from(["trusty-mpm", "project"]).is_err());
    }

    #[test]
    fn cli_parses_launch() {
        let cli = Cli::try_parse_from(["trusty-mpm", "launch"]).unwrap();
        match cli.command {
            Command::Launch { dir } => assert_eq!(dir, None),
            other => panic!("expected launch, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_launch_with_dir() {
        let cli = Cli::try_parse_from(["trusty-mpm", "launch", "/work/p"]).unwrap();
        match cli.command {
            Command::Launch { dir } => assert_eq!(dir.as_deref(), Some("/work/p")),
            other => panic!("expected launch, got {other:?}"),
        }
    }

    #[test]
    fn fallback_session_name_has_tmpm_prefix() {
        let name = fallback_session_name(std::path::Path::new("/work/p"));
        assert!(name.starts_with("tmpm-"), "got {name}");
    }

    #[test]
    fn fallback_session_name_uses_folder() {
        // The offline fallback name reflects the project folder, not a UUID.
        let name = fallback_session_name(std::path::Path::new("/Users/x/trusty-mpm"));
        assert_eq!(name, "tmpm-trusty-mpm");
    }

    #[test]
    fn launch_banner_does_not_panic() {
        // Why: the banner does width math and probes PATH; exercise it to catch
        // arithmetic underflow or formatting regressions.
        print_launch_banner("/Users/test/project", "tmpm-quiet-falcon", None);
        print_launch_banner(
            "/Users/test/project",
            "tmpm-quiet-falcon",
            Some(std::path::Path::new("/tmp/system-prompt.txt")),
        );
    }

    #[test]
    fn launch_reconnect_banner_does_not_panic() {
        // Why: the reconnect banner shares the render path with the launch
        // banner; exercise it to catch formatting regressions.
        print_launch_banner_reconnecting("/Users/test/project", "tmpm-quiet-falcon");
    }

    #[test]
    fn terminal_width_is_positive() {
        // Why: callers use the width for layout; a zero or absurd value would
        // break formatting. The probe must always yield a usable column count.
        assert!(terminal_width() > 0);
    }

    #[test]
    fn launch_banner_contains_session_fields() {
        // Why: the banner is the operator's only summary of what was wired up;
        // the project, session, and prompt fields must all appear, the screen
        // must be cleared, and the launch action line must be present.
        let banner = render_launch_banner(
            "/Users/test/project",
            "tmpm-quiet-falcon",
            Some(std::path::Path::new("/tmp/INSTRUCTIONS.md")),
            None,
        );
        assert!(banner.starts_with("\x1B[2J\x1B[1;1H"), "screen not cleared");
        assert!(banner.contains("/Users/test/project"));
        assert!(banner.contains("tmpm-quiet-falcon"));
        assert!(banner.contains("/tmp/INSTRUCTIONS.md"));
        assert!(banner.contains("Launching claude..."));
        assert!(banner.contains("TRUSTY") || banner.contains("M U L T I"));
    }

    #[test]
    fn launch_banner_marks_reconnect() {
        // Why: a reconnecting launch must not claim it started a new session;
        // the banner must show a Status row and the "Reconnecting..." action.
        let banner = render_launch_banner(
            "/Users/test/project",
            "tmpm-quiet-falcon",
            None,
            Some("tmpm-quiet-falcon"),
        );
        assert!(banner.contains("reconnecting to existing session"));
        assert!(banner.contains("Reconnecting..."));
        assert!(
            !banner.contains("Launching claude..."),
            "reconnect banner must not claim a fresh launch"
        );
    }

    #[test]
    fn normalize_workdir_strips_trailing_slash() {
        // Why: a daemon-stored workdir and the resolved cwd may differ only by a
        // trailing slash; reconnect detection must treat them as equal. Use a
        // path that does not exist so canonicalize falls through to the
        // string-normalization branch deterministically.
        let with = normalize_workdir("/no/such/dir/");
        let without = normalize_workdir("/no/such/dir");
        assert_eq!(with, without);
        assert_eq!(without, "/no/such/dir");
    }

    #[test]
    fn cli_parses_session_start() {
        let cli = Cli::try_parse_from(["trusty-mpm", "session", "start"]).unwrap();
        match cli.command {
            Command::Session {
                action: SessionAction::Start { dir },
            } => assert_eq!(dir, None),
            other => panic!("expected session start, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_session_start_with_dir() {
        let cli =
            Cli::try_parse_from(["trusty-mpm", "session", "start", "--dir", "/work/p"]).unwrap();
        match cli.command {
            Command::Session {
                action: SessionAction::Start { dir },
            } => assert_eq!(dir.as_deref(), Some("/work/p")),
            other => panic!("expected session start, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_session_stop() {
        let cli =
            Cli::try_parse_from(["trusty-mpm", "session", "stop", "tmpm-quiet-falcon"]).unwrap();
        match cli.command {
            Command::Session {
                action: SessionAction::Stop { id_or_name },
            } => assert_eq!(id_or_name, "tmpm-quiet-falcon"),
            other => panic!("expected session stop, got {other:?}"),
        }
    }

    #[test]
    fn cli_session_stop_requires_arg() {
        // `session stop` without an id-or-name is an error.
        assert!(Cli::try_parse_from(["trusty-mpm", "session", "stop"]).is_err());
    }

    #[test]
    fn cli_parses_session_list() {
        let cli = Cli::try_parse_from(["trusty-mpm", "session", "list"]).unwrap();
        match cli.command {
            Command::Session {
                action: SessionAction::List { dir },
            } => assert_eq!(dir, None),
            other => panic!("expected session list, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_session_clean() {
        let cli = Cli::try_parse_from(["trusty-mpm", "session", "clean"]).unwrap();
        match cli.command {
            Command::Session {
                action: SessionAction::Clean { dir },
            } => assert_eq!(dir, None),
            other => panic!("expected session clean, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_session_info() {
        let cli = Cli::try_parse_from(["trusty-mpm", "session", "info", "abc-123"]).unwrap();
        match cli.command {
            Command::Session {
                action: SessionAction::Info { id_or_name },
            } => assert_eq!(id_or_name, "abc-123"),
            other => panic!("expected session info, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_session_instructions() {
        let cli = Cli::try_parse_from(["trusty-mpm", "session", "instructions"]).unwrap();
        match cli.command {
            Command::Session {
                action: SessionAction::Instructions { dir },
            } => assert_eq!(dir, None),
            other => panic!("expected session instructions, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_session_instructions_with_dir() {
        let cli = Cli::try_parse_from(["trusty-mpm", "session", "instructions", "--dir", "/tmp"])
            .unwrap();
        match cli.command {
            Command::Session {
                action: SessionAction::Instructions { dir },
            } => assert_eq!(dir.as_deref(), Some("/tmp")),
            other => panic!("expected session instructions, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_events() {
        let cli = Cli::try_parse_from(["trusty-mpm", "events"]).unwrap();
        assert!(matches!(cli.command, Command::Events));
    }

    #[test]
    fn cli_url_flag_overrides_default() {
        let cli = Cli::try_parse_from(["trusty-mpm", "--url", "http://x:9", "status"]).unwrap();
        assert_eq!(cli.url, "http://x:9");
    }

    #[test]
    fn cli_rejects_no_subcommand() {
        // A subcommand is mandatory; bare invocation must error.
        assert!(Cli::try_parse_from(["trusty-mpm"]).is_err());
    }

    #[test]
    fn cli_parses_tui_defaults() {
        // The `--url` flag is passed explicitly: the `tui` subcommand's `url`
        // arg carries `env = "TRUSTY_MPM_URL"`, so an ambient `TRUSTY_MPM_URL`
        // in the test runner's environment would otherwise override the clap
        // `default_value` and make this assertion environment-dependent.
        // `interval_ms` has no env binding, so its default is asserted bare.
        let cli = Cli::try_parse_from(["trusty-mpm", "tui", "--url", DEFAULT_URL]).unwrap();
        match cli.command {
            Command::Tui { url, interval_ms } => {
                assert_eq!(url, DEFAULT_URL);
                assert_eq!(interval_ms, 1000);
            }
            other => panic!("expected Tui, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_tui_with_interval() {
        let cli = Cli::try_parse_from(["trusty-mpm", "tui", "--interval-ms", "500"]).unwrap();
        match cli.command {
            Command::Tui { interval_ms, .. } => assert_eq!(interval_ms, 500),
            other => panic!("expected Tui, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_telegram_pair() {
        let cli = Cli::try_parse_from(["trusty-mpm", "telegram", "pair"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Telegram {
                cmd: TelegramCmd::Pair
            }
        ));
    }

    #[test]
    fn cli_parses_telegram_status() {
        let cli = Cli::try_parse_from(["trusty-mpm", "telegram", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Telegram {
                cmd: TelegramCmd::Status
            }
        ));
    }

    #[test]
    fn cli_parses_telegram_stop() {
        let cli = Cli::try_parse_from(["trusty-mpm", "telegram", "stop"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Telegram {
                cmd: TelegramCmd::Stop
            }
        ));
    }

    #[test]
    fn cli_telegram_requires_subcommand() {
        // `telegram` with no action is an error now that it is a group.
        assert!(Cli::try_parse_from(["trusty-mpm", "telegram"]).is_err());
    }

    #[test]
    fn cli_parses_telegram_start_with_check() {
        let cli = Cli::try_parse_from(["trusty-mpm", "telegram", "start", "--check"]).unwrap();
        match cli.command {
            Command::Telegram {
                cmd: TelegramCmd::Start { check, token, .. },
            } => {
                assert!(check);
                assert_eq!(token, None);
            }
            other => panic!("expected Telegram start, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_telegram_start_with_token() {
        let cli =
            Cli::try_parse_from(["trusty-mpm", "telegram", "start", "--token", "secret"]).unwrap();
        match cli.command {
            Command::Telegram {
                cmd: TelegramCmd::Start { token, check, .. },
            } => {
                assert_eq!(token.as_deref(), Some("secret"));
                assert!(!check);
            }
            other => panic!("expected Telegram start, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_telegram_start_alert_and_user_flags() {
        let cli = Cli::try_parse_from([
            "trusty-mpm",
            "telegram",
            "start",
            "--alert-chat-id",
            "12345",
            "--allowed-user-id",
            "67890",
        ])
        .unwrap();
        match cli.command {
            Command::Telegram {
                cmd:
                    TelegramCmd::Start {
                        alert_chat_id,
                        allowed_user_id,
                        ..
                    },
            } => {
                assert_eq!(alert_chat_id, Some(12345));
                assert_eq!(allowed_user_id, Some(67890));
            }
            other => panic!("expected Telegram start, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_daemon_defaults() {
        let cli = Cli::try_parse_from(["trusty-mpm", "daemon"]).unwrap();
        match cli.command {
            Command::Daemon {
                addr,
                tailscale,
                mcp,
            } => {
                assert_eq!(addr.to_string(), "127.0.0.1:7880");
                assert!(!tailscale);
                assert!(!mcp);
            }
            other => panic!("expected Daemon, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_daemon_tailscale() {
        let cli = Cli::try_parse_from(["trusty-mpm", "daemon", "--tailscale"]).unwrap();
        match cli.command {
            Command::Daemon { tailscale, .. } => assert!(tailscale),
            other => panic!("expected Daemon, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_daemon_mcp() {
        let cli = Cli::try_parse_from(["trusty-mpm", "daemon", "--mcp"]).unwrap();
        match cli.command {
            Command::Daemon { mcp, .. } => assert!(mcp),
            other => panic!("expected Daemon, got {other:?}"),
        }
    }

    #[test]
    fn project_init_scaffolds_dotdir() {
        // `project init` must create `.trusty-mpm/{config.toml,sessions/}`
        // with a config skeleton naming the project after its directory.
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("my-app");
        std::fs::create_dir_all(&project).unwrap();
        let report = scaffold_project_dir(&project).unwrap();
        assert_eq!(report.len(), 2);

        let config = project.join(".trusty-mpm/config.toml");
        let sessions = project.join(".trusty-mpm/sessions");
        assert!(config.exists());
        assert!(sessions.is_dir());
        let contents = std::fs::read_to_string(&config).unwrap();
        assert!(contents.contains("name = \"my-app\""));
        assert!(contents.contains("[agents]"));
        assert!(contents.contains("[skills]"));
    }

    #[test]
    fn project_init_keeps_existing_config() {
        // Re-running `project init` must never clobber an edited config.toml.
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        scaffold_project_dir(&project).unwrap();
        let config = project.join(".trusty-mpm/config.toml");
        std::fs::write(&config, "# edited by hand").unwrap();

        let report = scaffold_project_dir(&project).unwrap();
        assert!(report.iter().any(|l| l.contains("skipped")));
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "# edited by hand"
        );
    }

    #[test]
    fn cli_parses_install_no_force() {
        let cli = Cli::try_parse_from(["trusty-mpm", "install"]).unwrap();
        match cli.command {
            Command::Install { force } => assert!(!force),
            other => panic!("expected Install, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_install_with_force() {
        let cli = Cli::try_parse_from(["trusty-mpm", "install", "--force"]).unwrap();
        match cli.command {
            Command::Install { force } => assert!(force),
            other => panic!("expected Install, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_hook() {
        let cli = Cli::try_parse_from(["trusty-mpm", "hook"]).unwrap();
        assert!(matches!(cli.command, Command::Hook));
    }

    /// Why: when `CLAUDE_MPM_SUB_AGENT` is present in the env, the hook
    /// handler must return Ok(()) without performing any I/O — that is the
    /// whole point of the sub-agent guard.
    /// What: sets the env var, calls `hook` with a deliberately-unreachable
    /// daemon URL, asserts the call returns Ok in well under the daemon's
    /// connect timeout (so we know the network code never ran).
    #[tokio::test]
    async fn hook_guard_short_circuits() {
        // SAFETY: tokio's current_thread runtime is single-threaded; this
        // env-var dance is isolated to the test.
        unsafe {
            std::env::set_var(SUB_AGENT_ENV, "1");
        }
        let client = reqwest::Client::new();
        let started = std::time::Instant::now();
        // Use a URL we know cannot be reached. If the guard fails the call
        // would consume the full 2s timeout; the assert below catches that.
        let res = hook(&client, "http://127.0.0.1:1").await;
        let elapsed = started.elapsed();
        unsafe {
            std::env::remove_var(SUB_AGENT_ENV);
        }
        assert!(res.is_ok(), "guard branch must return Ok");
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "guard must short-circuit without network I/O; took {elapsed:?}"
        );
    }

    /// Why: re-running `tm install` against a settings file that already
    /// carries the MPM hook block must not rewrite the file. This is the
    /// idempotency requirement.
    /// What: builds the additions block, merges it into an empty document,
    /// then merges the same additions back into the result, and asserts the
    /// two outputs are deeply equal.
    #[test]
    fn mpm_hook_additions_is_idempotent_under_merge() {
        use trusty_common::claude_config::merge_hook_entries;
        let additions = mpm_hook_additions();
        let once = merge_hook_entries(&serde_json::json!({}), &additions);
        let twice = merge_hook_entries(&once, &additions);
        assert_eq!(once, twice, "second merge must be a no-op");
        // And every expected event lands exactly once.
        for event in ["PreToolUse", "PostToolUse", "Stop"] {
            let arr = once["hooks"][event].as_array().expect("event array");
            assert_eq!(arr.len(), 1, "{event}: expected one matcher block");
        }
    }

    #[test]
    fn install_writes_all_artifacts() {
        // A fresh install must write every bundled artifact to disk under the
        // framework root, with matching content.
        let dir = tempfile::tempdir().unwrap();
        let paths = trusty_mpm_core::paths::FrameworkPaths::under(dir.path());
        let report = install_to(&paths, false).unwrap();
        assert_eq!(report.len(), trusty_mpm_core::bundle::ALL.len());
        for artifact in trusty_mpm_core::bundle::ALL {
            let dest = paths.framework.join(artifact.rel_path);
            assert!(dest.exists(), "missing {}", artifact.rel_path);
            let written = std::fs::read_to_string(&dest).unwrap();
            assert_eq!(written, artifact.contents);
        }
    }

    #[test]
    fn install_skips_existing_without_force() {
        // An existing artifact is left untouched without `--force` and the
        // report says so; `--force` overwrites it.
        let dir = tempfile::tempdir().unwrap();
        let paths = trusty_mpm_core::paths::FrameworkPaths::under(dir.path());
        let optimizer = paths.optimizer_config();
        std::fs::create_dir_all(&paths.hooks).unwrap();
        std::fs::write(&optimizer, "custom").unwrap();

        let report = install_to(&paths, false).unwrap();
        assert!(report.iter().any(|l| l.contains("skipped")));
        assert_eq!(std::fs::read_to_string(&optimizer).unwrap(), "custom");

        let forced = install_to(&paths, true).unwrap();
        assert!(forced.iter().all(|l| !l.contains("skipped")));
        assert_ne!(std::fs::read_to_string(&optimizer).unwrap(), "custom");
    }

    #[test]
    #[ignore = "requires bundled framework artifacts (base-agent) — run locally after tm install"]
    fn install_then_deploy_composes_agents() {
        // Installing the bundled agent sources and then deploying them must
        // produce composed, inheritance-flattened files in `.claude/agents/`.
        let dir = tempfile::tempdir().unwrap();
        let paths = trusty_mpm_core::paths::FrameworkPaths::under(dir.path());
        install_to(&paths, false).unwrap();

        let result = trusty_mpm_core::agent_deployer::deploy_agents(
            &paths.agent_source_dir(),
            &paths.claude_agents_dir(),
        )
        .unwrap();
        // All six bundled agents deploy on a fresh target.
        assert_eq!(result.deployed.len(), 6);
        assert!(result.skipped.is_empty());

        // The composed engineer carries inherited base content and no
        // `extends:` for Claude Code to interpret.
        let engineer =
            std::fs::read_to_string(paths.claude_agents_dir().join("engineer.md")).unwrap();
        assert!(engineer.contains("BASE-AGENT"));
        assert!(engineer.contains("BASE-ENGINEER"));
        assert!(engineer.contains("# Engineer"));
        assert!(!engineer.contains("extends:"));

        // The report formatter renders a composed-chain line.
        let lines = deploy_report_lines(&result, &paths.agent_source_dir());
        assert!(
            lines
                .iter()
                .any(|l| l.contains("engineer.md") && l.contains("composed:")),
            "lines = {lines:?}"
        );
    }

    #[test]
    fn cli_parses_optimizer_status() {
        let cli = Cli::try_parse_from(["trusty-mpm", "optimizer", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Optimizer {
                action: OptimizerAction::Status
            }
        ));
    }

    #[test]
    fn cli_parses_optimizer_set() {
        let cli = Cli::try_parse_from(["trusty-mpm", "optimizer", "set", "trim"]).unwrap();
        match cli.command {
            Command::Optimizer {
                action: OptimizerAction::Set { level },
            } => assert!(matches!(level, CliCompressionLevel::Trim)),
            other => panic!("expected optimizer set, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_session_events() {
        let cli = Cli::try_parse_from(["trusty-mpm", "session", "events", "abc-123"]).unwrap();
        match cli.command {
            Command::Session {
                action: SessionAction::Events { id_or_name },
            } => assert_eq!(id_or_name, "abc-123"),
            other => panic!("expected session events, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_session_breakers() {
        let cli = Cli::try_parse_from(["trusty-mpm", "session", "breakers"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Session {
                action: SessionAction::Breakers
            }
        ));
    }

    #[test]
    fn cli_parses_session_pause() {
        let cli =
            Cli::try_parse_from(["trusty-mpm", "session", "pause", "tmpm-quiet-falcon"]).unwrap();
        match cli.command {
            Command::Session {
                action: SessionAction::Pause { id_or_name, note },
            } => {
                assert_eq!(id_or_name, "tmpm-quiet-falcon");
                assert_eq!(note, None);
            }
            other => panic!("expected session pause, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_session_pause_with_note() {
        let cli = Cli::try_parse_from([
            "trusty-mpm",
            "session",
            "pause",
            "abc-123",
            "--note",
            "stepping away",
        ])
        .unwrap();
        match cli.command {
            Command::Session {
                action: SessionAction::Pause { id_or_name, note },
            } => {
                assert_eq!(id_or_name, "abc-123");
                assert_eq!(note.as_deref(), Some("stepping away"));
            }
            other => panic!("expected session pause, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_session_resume() {
        let cli = Cli::try_parse_from(["trusty-mpm", "session", "resume", "abc-123"]).unwrap();
        match cli.command {
            Command::Session {
                action: SessionAction::Resume { id_or_name },
            } => assert_eq!(id_or_name, "abc-123"),
            other => panic!("expected session resume, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_session_run() {
        let cli =
            Cli::try_parse_from(["trusty-mpm", "session", "run", "abc-123", "help me"]).unwrap();
        match cli.command {
            Command::Session {
                action:
                    SessionAction::Run {
                        id_or_name,
                        command,
                        summarize,
                    },
            } => {
                assert_eq!(id_or_name, "abc-123");
                assert_eq!(command, "help me");
                assert!(!summarize);
            }
            other => panic!("expected session run, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_session_run_with_summarize() {
        let cli = Cli::try_parse_from([
            "trusty-mpm",
            "session",
            "run",
            "tmpm-abc",
            "help",
            "--summarize",
        ])
        .unwrap();
        match cli.command {
            Command::Session {
                action: SessionAction::Run { summarize, .. },
            } => assert!(summarize),
            other => panic!("expected session run, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_session_output_defaults() {
        let cli = Cli::try_parse_from(["trusty-mpm", "session", "output", "abc-123"]).unwrap();
        match cli.command {
            Command::Session {
                action:
                    SessionAction::Output {
                        id_or_name,
                        lines,
                        summarize,
                    },
            } => {
                assert_eq!(id_or_name, "abc-123");
                assert_eq!(lines, 50);
                assert!(!summarize);
            }
            other => panic!("expected session output, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_session_output_with_lines() {
        let cli = Cli::try_parse_from([
            "trusty-mpm",
            "session",
            "output",
            "abc-123",
            "--lines",
            "120",
        ])
        .unwrap();
        match cli.command {
            Command::Session {
                action: SessionAction::Output { lines, .. },
            } => assert_eq!(lines, 120),
            other => panic!("expected session output, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_session_output_with_summarize() {
        let cli =
            Cli::try_parse_from(["trusty-mpm", "session", "output", "tmpm-abc", "--summarize"])
                .unwrap();
        match cli.command {
            Command::Session {
                action: SessionAction::Output { summarize, .. },
            } => assert!(summarize),
            other => panic!("expected session output, got {other:?}"),
        }
    }

    #[test]
    fn compression_stats_line_skipped_when_uncompressed() {
        // A response with a null `compress_level` is raw output; no stats line.
        let body = serde_json::json!({
            "output": "raw",
            "compress_level": serde_json::Value::Null,
        });
        // The helper must not panic and must treat a null level as "no stats".
        assert!(
            body.get("compress_level")
                .and_then(|v| v.as_str())
                .is_none()
        );
        print_compression_stats(&body);
    }

    #[test]
    fn compression_stats_line_printed_when_compressed() {
        // A response carrying a level and byte counts is exercised end-to-end;
        // the helper computes a percentage without panicking.
        let body = serde_json::json!({
            "output": "summary",
            "original_bytes": 4096,
            "compressed_bytes": 312,
            "compress_level": "summarise",
        });
        assert_eq!(
            body.get("compress_level").and_then(|v| v.as_str()),
            Some("summarise")
        );
        print_compression_stats(&body);
    }

    #[test]
    fn cli_session_pause_requires_arg() {
        assert!(Cli::try_parse_from(["trusty-mpm", "session", "pause"]).is_err());
    }

    #[test]
    fn cli_session_run_requires_command() {
        assert!(Cli::try_parse_from(["trusty-mpm", "session", "run", "abc-123"]).is_err());
    }

    #[test]
    fn cli_parses_overseer_status() {
        let cli = Cli::try_parse_from(["trusty-mpm", "overseer", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Overseer {
                action: OverseerAction::Status
            }
        ));
    }

    #[test]
    fn event_summary_prefers_tool_field() {
        let payload = serde_json::json!({"tool": "Bash", "input": {"command": "ls"}});
        assert_eq!(event_summary(&payload), "tool=Bash");
    }

    #[test]
    fn event_summary_truncates_long_payloads() {
        let payload = serde_json::json!({"k": "x".repeat(200)});
        let summary = event_summary(&payload);
        assert!(summary.chars().count() <= 61);
        assert!(summary.ends_with('\u{2026}'));
    }

    #[test]
    fn cli_parses_attach() {
        let cli = Cli::try_parse_from(["trusty-mpm", "attach", "frontend"]).unwrap();
        match cli.command {
            Command::Attach { target, json } => {
                assert_eq!(target, "frontend");
                assert!(!json);
            }
            other => panic!("expected Attach, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_attach_with_json() {
        let cli = Cli::try_parse_from(["trusty-mpm", "attach", "abc-123", "--json"]).unwrap();
        match cli.command {
            Command::Attach { target, json } => {
                assert_eq!(target, "abc-123");
                assert!(json);
            }
            other => panic!("expected Attach, got {other:?}"),
        }
    }

    #[test]
    fn cli_attach_requires_target() {
        assert!(Cli::try_parse_from(["trusty-mpm", "attach"]).is_err());
    }

    #[test]
    fn cli_parses_connect() {
        // `tm connect` is the no-deployment session starter; it takes an
        // optional project directory, exactly like `tm launch`.
        let cli = Cli::try_parse_from(["trusty-mpm", "connect"]).unwrap();
        match cli.command {
            Command::Connect { dir } => assert_eq!(dir, None),
            other => panic!("expected Connect, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_connect_with_dir() {
        let cli = Cli::try_parse_from(["trusty-mpm", "connect", "/work/p"]).unwrap();
        match cli.command {
            Command::Connect { dir } => assert_eq!(dir.as_deref(), Some("/work/p")),
            other => panic!("expected Connect, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_daemon_custom_addr() {
        let cli = Cli::try_parse_from(["trusty-mpm", "daemon", "--addr", "0.0.0.0:9000"]).unwrap();
        match cli.command {
            Command::Daemon { addr, .. } => assert_eq!(addr.to_string(), "0.0.0.0:9000"),
            other => panic!("expected Daemon, got {other:?}"),
        }
    }
}
