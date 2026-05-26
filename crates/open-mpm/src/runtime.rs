// Pre-existing clippy warnings across this large binary crate.
// Each category below is suppressed at crate level with rationale:
// - dead_code / unused_imports: Many helpers are kept for future use, behind
//   feature flags, or used only on certain platforms / by tests; pruning them
//   is its own refactor and would churn unrelated modules.
// - clippy::collapsible_if / collapsible_else_if: Style preference; nested
//   ifs are often clearer with the existing comments and gating logic.
// - clippy::manual_str_repeat / manual_repeat_n / single_char_add_str: Style
//   nits in display/formatting code where current form reads fine.
// - clippy::too_many_arguments: A few orchestration entry points genuinely
//   need their argument count; signatures are part of internal contracts.
// - clippy::await_holding_lock: Test-only — a std::sync::Mutex serializes
//   tests that mutate process-global env (HOME, etc.). The await points are
//   inside the critical section by design, and tests are single-threaded
//   per-test by virtue of the lock.
// - clippy::clone_on_copy / len_zero / map_or / etc.: Misc style nits in
//   pre-existing code; not worth the churn vs. risk of breaking 1500+ tests.
#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_mut)]
#![allow(unused_assignments)]
#![allow(unused_variables)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::manual_str_repeat)]
#![allow(clippy::manual_repeat_n)]
#![allow(clippy::single_char_add_str)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::await_holding_lock)]
#![allow(clippy::clone_on_copy)]
#![allow(clippy::len_zero)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::manual_map)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::unnecessary_sort_by)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::new_without_default)]
#![allow(clippy::manual_split_once)]
#![allow(clippy::needless_splitn)]
#![allow(clippy::single_match_else)]
#![allow(clippy::single_match)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::manual_clamp)]
#![allow(clippy::redundant_closure)]
#![allow(clippy::manual_pattern_char_comparison)]
#![allow(clippy::vec_init_then_push)]
#![allow(clippy::single_component_path_imports)]
#![allow(clippy::derivable_impls)]
#![allow(clippy::match_single_binding)]
#![allow(clippy::redundant_pattern_matching)]

//! open-mpm entry point (PM orchestrator + sub-agent runner + direct/workflow modes).
//!
//! Why: A single binary hosts all execution modes so we don't have to build
//! or distribute separate crates. The binary inspects argv and dispatches.
//! What:
//!   - No args  -> PM mode: reads a line from stdin, calls OpenRouter with
//!     the `delegate_to_agent` tool, spawns the chosen sub-agent subprocess,
//!     forwards the task via NDJSON, prints the result to stdout.
//!   - `--agent <name>` -> sub-agent mode: reads one NDJSON task line from
//!     stdin, runs a chat completion (with tool support when the agent's
//!     config enables it) using the agent's config, writes a single NDJSON
//!     result line to stdout, exits.
//!   - `--direct <name> [--task-file <path>] [--out-dir <dir>]` -> direct mode:
//!     bypasses the PM LLM, sends stdin/file contents straight to the named
//!     sub-agent and optionally extracts file sections from the output.
//!   - `--workflow <name> --task-file <path> --out-dir <dir>` -> workflow mode:
//!     loads `.open-mpm/workflows/<name>.json` and runs each phase sequentially.
//! Test: `cargo run -- --agent python-engineer` with a Task JSON piped on
//! stdin returns a JSON Result line on stdout.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs,
};
use chrono;
use clap::Parser;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// Bundled declarative help config (issue #216). Loaded once per process.
///
/// Why: every standalone trusty-* binary embeds its `help.yaml` via
/// `include_str!` so the workspace-shared `trusty_common::help::suggest`
/// helper has a single source of truth for unknown-subcommand hints. The
/// native `cli::did_you_mean` path that scans `KNOWN_SUBCOMMANDS` still runs
/// first for the common-case typos; this static covers the residual cases
/// the clap layer reports as `InvalidSubcommand` / `UnknownArgument`.
/// What: `LazyLock<HelpConfig>` parsed from `crates/open-mpm/help.yaml` at
/// first access. `expect` is acceptable because the YAML is shipped inside
/// the binary; a parse failure would be caught on the first invocation.
/// Test: parse coverage lives in `trusty-common`; this site is exercised
/// manually via `open-mpm memori`.
static HELP: std::sync::LazyLock<trusty_common::help::HelpConfig> =
    std::sync::LazyLock::new(|| {
        trusty_common::help::load_help(include_str!("../help.yaml"))
            .expect("open-mpm help.yaml is bundled and valid")
    });

/// Top-level clap CLI for the `open-mpm` binary.
///
/// Why: Replaces 200+ lines of hand-rolled `args.iter().any(...)` /
/// `args.iter().position(...)` scanning with a single derive-based parser.
/// Help text, error messages, value validation, and `--version` come for
/// free; adding a new flag is one struct field.
/// What: Mode-flags (`--api`, `--agent`, `--workflow`, `--direct`, `--pm`,
/// `--ctrl`, `--reindex`, `--watch`, `--check-orphans`, `--clear-sessions`,
/// `--reinit`) coexist as optionals/bools because the existing dispatch
/// inspects them in priority order. Subcommands like `memory`, `code`,
/// `memories`, `agents`, `skills`, `inspect`, `postmortem` are still
/// detected on argv before clap runs (they have their own clap parsers
/// inside their handlers) so their argv-passthrough semantics are
/// preserved exactly.
/// Test: All existing `--workflow`/`--direct`/`--api` invocations continue
/// to work; `cargo run -- --version` still prints the build banner.
#[derive(Debug, Parser, Default)]
#[command(
    name = "open-mpm",
    about = "Rust-based AI agent orchestration harness",
    long_about = "Rust-based AI agent orchestration harness.

Additional commands (run without flags):
  om start | stop | status    Server lifecycle
  om connect <path>           Register project with the running server
  om session new              --project <path> --name <name> [--agent <agent>] [--worktree]
  om session list             [<project-path>]
  om session attach           <session-id>
  om session kill             <session-id>
  om memory | code | agents   Data management

Run `om session` with no arguments for full session usage.",
    disable_version_flag = true,
    // We accept extra positional tokens (free text the user wants to forward
    // to the controller) so `open-mpm "do X"` keeps working.
    trailing_var_arg = true,
    allow_hyphen_values = true
)]
struct Cli {
    /// Run as a sub-agent: read one NDJSON task from stdin, write one NDJSON
    /// result to stdout, exit.
    #[arg(long)]
    agent: Option<String>,

    /// Run a named workflow from `.open-mpm/workflows/<name>.json`.
    #[arg(long)]
    workflow: Option<String>,

    /// Direct-agent mode: bypass the PM LLM and forward stdin/file to the
    /// named sub-agent.
    #[arg(long)]
    direct: Option<String>,

    /// Inline task text (alternative to `--task-file` / stdin).
    #[arg(long)]
    task: Option<String>,

    /// Path to a task description file.
    #[arg(long = "task-file")]
    task_file: Option<String>,

    /// Output directory for workflow / direct artifacts (assignments.json,
    /// phase logs, observe output, perf records). When `--project-dir` is
    /// also set, generated application code lands in `--project-dir` and
    /// only workflow artifacts land here.
    #[arg(long = "out-dir")]
    out_dir: Option<String>,

    /// Project directory where generated application code should land.
    /// Defaults to the value of `--out-dir` (or the auto-generated
    /// `out/<label>-<ts>/` path) for backward compatibility. Set this to
    /// CWD (e.g. `--project-dir .`) to have generated code written to your
    /// current project directory while keeping workflow artifacts
    /// elsewhere via `--out-dir`.
    #[arg(long = "project-dir")]
    project_dir: Option<String>,

    /// Emit machine-readable JSON output where supported.
    #[arg(long)]
    json: bool,

    /// Start the HTTP API server + embedded web UI.
    #[arg(long)]
    api: bool,

    /// Alias for `--api` (kept for backwards compatibility).
    #[arg(long)]
    serve: bool,

    /// Port for the API server (default 8080).
    #[arg(long)]
    port: Option<u16>,

    /// Bearer token required for `POST /api/task` (overrides
    /// `OPEN_MPM_API_TOKEN`).
    #[arg(long = "api-token")]
    api_token: Option<String>,

    /// Single-shot PM mode (legacy compat).
    #[arg(long)]
    pm: bool,

    /// Explicit CTRL mode (the default when no other mode flag is set).
    #[arg(long)]
    ctrl: bool,

    /// Run the Telegram bot gateway (#264). Requires `TELEGRAM_BOT_TOKEN`.
    ///
    /// Headless/server mode: takes over the process and runs only the bot.
    /// For interactive use inside the REPL, prefer the `/telegram` slash
    /// command, which runs the bot as a background tokio task while keeping
    /// the REPL interactive.
    #[arg(long)]
    telegram: bool,

    /// Run the Slack Socket Mode bot gateway (#418). Requires
    /// `SLACK_APP_TOKEN` (xapp-...) and `SLACK_BOT_TOKEN` (xoxb-...).
    ///
    /// Headless/server mode: takes over the process and runs only the bot.
    #[arg(long)]
    slack: bool,

    /// Reindex the local code/memory store.
    #[arg(long)]
    reindex: bool,

    /// File-watcher mode.
    #[arg(long)]
    watch: bool,

    /// Print and re-home orphaned files.
    #[arg(long = "check-orphans")]
    check_orphans: bool,

    /// Clear in-process persistent agent sessions before this run.
    #[arg(long = "clear-sessions")]
    clear_sessions: bool,

    /// Force re-initialization of the project (regenerate `.open-mpm/state/`).
    #[arg(long)]
    reinit: bool,

    /// #348: Enable AST-native tools for the engineer agent regardless of
    /// the agent TOML's `[tools] ast_native` setting.
    ///
    /// Why: Lets bake-off operators flip the substrate per-invocation
    /// without editing config. Honoured for `--direct` and `--workflow` runs.
    /// What: Sets a process-global flag that the in-process runner reads
    /// when registering tools.
    #[arg(long = "ast-native", default_value_t = false)]
    ast_native: bool,

    /// #348: Run a bake-off in comparison mode — execute the task once with
    /// the traditional substrate and once with `--ast-native`, then emit a
    /// side-by-side report of LLM calls, token counts, and output sizes.
    #[arg(long, default_value_t = false)]
    compare: bool,

    /// #350: Parse `src/` into the symbol registry and persist it to
    /// `.open-mpm/state/symbol-registry.json`.
    #[arg(long, default_value_t = false)]
    parse_to_registry: bool,

    /// #350: Project the persisted symbol registry back to source files
    /// under the project root (deterministic emission).
    #[arg(long, default_value_t = false)]
    emit_from_registry: bool,

    /// #350: Verify all symbol-registry content hashes match their stored
    /// source. Exits non-zero if any mismatches are found.
    #[arg(long, default_value_t = false)]
    verify_registry: bool,

    /// Print the version banner and exit.
    #[arg(long, short = 'V')]
    version: bool,

    /// Manage the persistent open-mpm background service (#343).
    /// Accepts: `start`, `stop`, `status`. When set the binary handles
    /// the subcommand and exits without entering REPL/serve modes.
    #[arg(long)]
    service: Option<String>,

    /// #374: Run the search-as-a-service daemon. Owns the redb code-store
    /// lock and serves /search/{health,query,index-file,remove-file,reindex}
    /// over HTTP for the lifetime of the process. Used by other open-mpm
    /// processes (REPL, sub-agents, --api server) to share a single warm
    /// index without re-opening the on-disk store per process.
    #[arg(long = "search-service", default_value_t = false)]
    search_service: bool,

    /// Anything else — typically a free-text task to forward to the
    /// controller. Preserved as positional tokens so `open-mpm "do X"`
    /// keeps working.
    #[arg(allow_hyphen_values = true, num_args = 0..)]
    rest: Vec<String>,
}

// Why: Modules are owned by the `open_mpm` library crate (see src/lib.rs); this
//      binary re-exports them under `crate::` so existing `crate::foo::*` paths
//      throughout this file (and the integration tests) keep resolving without
//      a large sweep. This also gives external agent crates (cto-assistant) a
//      stable library handle to the same `ToolExecutor` / `AgentPlugin` types
//      this binary uses for injection.
// What: One `use open_mpm::foo as foo;` per top-level module. The `pub use`
//       re-export pattern would also work but keeps the binary's surface
//       deliberately small.
// Test: The binary continues to build and run end-to-end via `cargo build`
//       and the existing tmux/REPL tests.
use crate::default_bundled_config_dir;
use crate::{
    adapters, agents, api, ast, build_info, bus, cli, compress, context, ctrl, ctrl_session,
    debugger, docs_index, eval, events, git, identity, init, inspection, intent, interaction_log,
    ipc, llm, local_inference, logging, mcp, memory, mistake_log, perf, plugins, process_tracker,
    progress, rbac, recap, registry, repl, rpc, search, service, session, session_record,
    session_registry, skills, slack, state_writer, subprocess, telegram, ticketing, tm, tmux,
    tools, update, usage, workflow,
};

use memory::{CodeStore, FastEmbedder};
use search::{CodeIndexer, FileWatcher};

use agents::AgentConfig;
use agents::claude_code_runner::{ClaudeCodeAgentRunner, DispatchingAgentRunner};
use agents::harness_protocol::{BASE_PROTOCOL, CLAUDE_CODE_PROTOCOL, FINISH_TASK_PROTOCOL};
use agents::prompt_builder::SystemPromptBuilder;
use build_info::BuildInfo;
use ipc::{IpcMessage, extract_summary, parse_message, serialize_message};
use subprocess::{SubprocessAgentRunner, spawn_subagent_and_run};
use tools::SkillResolver;
use tools::fs_reader::{GrepFilesTool, ListDirTool, ReadFileTool};
#[allow(unused_imports)]
use tools::memory::{MemoryRecallTool, VectorSearchTool};
use tools::phase_audit::PhaseAuditTool;
use tools::shell::ShellExecTool as LocalOpsShellTool;
use tools::skill_loader::{FsSkillResolver, SkillListTool, SkillLoaderTool};
use tools::web_search::{BraveSearchTool, FetchUrlTool};
use tools::write_file::WriteFileTool;
use tools::{ToolRegistry, delegate::DelegateToAgentTool, shell_exec::ShellExecTool};
use workflow::WorkflowEngine;

/// Library entry point — contains the full top-level dispatch previously
/// hosted in `fn main()`.
///
/// Why: Exposing the binary's startup logic via the library lets private
///      launchers (e.g. `open-mpm-local`) install additional agent plugins
///      via `install_plugins(...)` before delegating to this function, so
///      the published `open-mpm` crate stays free of references to
///      `publish = false` agent crates.
/// What: Performs argv parsing, env loading, tracing setup, plugin spawn,
///       subcommand dispatch, mode-flag dispatch (--api/--workflow/--direct
///       /--agent/...), and finally the interactive REPL or CTRL fallback.
/// Test: Indirectly via `cargo run -p open-mpm`/`open-mpm-local` and the
///       crate's integration tests.
pub async fn run() -> Result<()> {
    // Handle --version / -V before anything else (no env/tracing/etc.).
    // Why: `--version` must be cheap and side-effect-free so it's safe to
    // run in CI or scripts without an OPENROUTER_API_KEY. It still bumps
    // the build counter so CI runs are disambiguated in logs.
    //
    // Note: We probe argv directly here (not via clap) so the version path
    // doesn't depend on clap successfully parsing every other flag. Clap's
    // own version-handling is disabled (`disable_version_flag = true` on
    // `Cli`) so we control formatting + the build-counter bump.
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.iter().any(|a| a == "--version" || a == "-V") {
        // Resolve project dir so `.open-mpm/state` lands in the project root
        // even when invoked from a subdirectory.
        let state_dir = ctrl::detect_self_project()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .join(".open-mpm")
            .join("state");
        tokio::fs::create_dir_all(&state_dir).await?;
        let info = BuildInfo::load_and_increment().await?;
        println!("{}", info.display_string());
        return Ok(());
    }

    // Load env and init tracing first so everything downstream has logs/keys.
    //
    // #250: `.env.local` lookup is relative to cwd, so launching `open-mpm` from
    // anywhere other than the project root (e.g. `cd /tmp && open-mpm ctrl`) used
    // to skip credential loading entirely and surface as
    // "no LLM credentials configured". We additionally try the detected
    // self-project directory so the harness picks up its own `.env.local`
    // regardless of the user's cwd. dotenvy does NOT override existing env vars
    // by default, so cwd-local `.env.local` still wins when both exist.
    dotenvy::from_filename(".env.local").ok();
    dotenvy::dotenv().ok();
    if let Some(project_dir) = ctrl::detect_self_project() {
        let project_env = project_dir.join(".env.local");
        if project_env.is_file() {
            dotenvy::from_path(&project_env).ok();
        }
    }

    // Why: External agent plugins (cto-assistant, future personas) are
    //      installed by private launchers BEFORE calling `run()`. The
    //      published `open-mpm` crate has zero knowledge of those private
    //      crates — see `install_plugins()` in `crate::lib` and the
    //      sibling `open-mpm-local` binary for the wiring point.
    // What: Anything the launcher passed to `install_plugins(...)` has
    //       already populated the OnceLock; the ctrl loop will pick it
    //       up when it builds the persona's tool surface.
    // Test: `open-mpm-local` integration; `open-mpm` standalone has an
    //       empty plugin list.

    // #366: Credential onboarding banner. After env loading is the right time
    // to check — both `.env.local` files and the host environment have been
    // merged in. Suppress in quiet/non-interactive modes (sub-agent IPC,
    // workflow runners, HTTP servers) where stderr output would corrupt
    // protocol streams or just be noise.
    {
        let raw_args: Vec<String> = std::env::args().collect();
        let quiet_mode = raw_args
            .iter()
            .any(|a| matches!(a.as_str(), "--agent" | "--serve" | "--api" | "--workflow"));
        if !quiet_mode {
            check_credentials_and_warn();
        }
    }

    // Default log level: "warn" for interactive REPL (clean UX), "info" for
    // batch/workflow/api modes. RUST_LOG always overrides both.
    // Set OPEN_MPM_LOG=info (or debug/trace) to override without RUST_LOG syntax.
    let is_interactive_repl = repl::is_tty()
        && !std::env::args().any(|a| {
            matches!(
                a.as_str(),
                "--workflow" | "--direct" | "--api" | "--serve" | "--agent"
            )
        });
    let default_level = std::env::var("OPEN_MPM_LOG")
        .unwrap_or_else(|_| if is_interactive_repl { "warn" } else { "info" }.to_string());

    // #257: When running the interactive REPL, route tracing output to a log
    // file instead of stderr. Both stdout and stderr render in the same TTY,
    // so a single `WARN` line from `tracing` would clobber the carefully
    // positioned chat scrollback (and was visibly leaking into the prompt).
    // Non-interactive modes (subagent, workflow, api, direct, piped stdin)
    // keep stderr writing so existing log-capture tooling still works.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level));
    if is_interactive_repl {
        let log_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".open-mpm")
            .join("logs");
        let _ = std::fs::create_dir_all(&log_dir);
        let log_path = log_dir.join("repl.log");
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            Ok(file) => {
                tracing_subscriber::fmt()
                    .with_env_filter(env_filter)
                    .with_writer(std::sync::Mutex::new(file))
                    .with_ansi(false)
                    .init();
            }
            Err(_) => {
                // Fallback: discard logs entirely rather than corrupt the TTY.
                tracing_subscriber::fmt()
                    .with_env_filter(env_filter)
                    .with_writer(std::io::sink)
                    .init();
            }
        }
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_writer(std::io::stderr) // keep stdout clean for NDJSON
            .init();
    }

    // #api-early-dispatch: Short-circuit `--api` / `--serve` BEFORE any
    // filesystem state setup. The Tauri sidecar spawns this binary with cwd=`/`
    // (sealed read-only APFS volume on macOS), so the subsequent
    // `create_dir_all(&state_dir)` would crash with EROFS ("/.open-mpm/state")
    // before the HTTP listener ever binds. The API server is fully
    // self-contained — it doesn't need state dirs, build-counter increments,
    // worktree cleanup, project registry, or message bus — so we can take the
    // fast path here and let `serve_with_config` own its own setup.
    //
    // We do a manual argv scan here because the full `Cli::try_parse_from` runs
    // later (after subcommand dispatch). Env loading and tracing init have
    // already happened above, so credentials and logs work as expected.
    //
    // Why: Fix for "API server did not become healthy within 20s" when the
    // Tauri app launches the sidecar from cwd=`/`.
    // What: When --api/--serve is present, parse --port and --api-token from
    //       argv and call serve_with_config directly, bypassing all PM-mode
    //       state initialization.
    // Test: `cd / && open-mpm --api --port 8765 &; sleep 2; curl
    //       http://127.0.0.1:8765/api/health` returns 200 instead of crashing.
    {
        let raw_args: Vec<String> = std::env::args().collect();
        let wants_api = raw_args.iter().any(|a| a == "--api" || a == "--serve");
        if wants_api {
            // Find --port <N> in argv (default 8080 to match clap default).
            let mut port: u16 = 8080;
            let mut iter = raw_args.iter();
            while let Some(a) = iter.next() {
                if a == "--port"
                    && let Some(v) = iter.next()
                    && let Ok(n) = v.parse::<u16>()
                {
                    port = n;
                    break;
                }
                if let Some(rest) = a.strip_prefix("--port=")
                    && let Ok(n) = rest.parse::<u16>()
                {
                    port = n;
                    break;
                }
            }
            // Find --api-token <TOK> (or env fallback).
            let mut token: Option<String> = None;
            let mut iter = raw_args.iter();
            while let Some(a) = iter.next() {
                if a == "--api-token"
                    && let Some(v) = iter.next()
                {
                    token = Some(v.clone());
                    break;
                }
                if let Some(rest) = a.strip_prefix("--api-token=") {
                    token = Some(rest.to_string());
                    break;
                }
            }
            let token = token
                .or_else(|| std::env::var("OPEN_MPM_API_TOKEN").ok())
                .filter(|s| !s.is_empty());
            return api::server::serve_with_config(api::server::ApiConfig { port, token }).await;
        }
    }

    // #374 early dispatch: `--search-service` runs the search daemon.
    // Same rationale as the `--api` early dispatch above — the daemon is
    // self-contained and doesn't need the heavy state-dir scaffolding,
    // and we want it to start fast.
    {
        let raw_args: Vec<String> = std::env::args().collect();
        if raw_args.iter().any(|a| a == "--search-service") {
            let project_root =
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            return search::service::run_search_service(project_root).await;
        }
    }

    // Bump the persistent build counter and log the banner so every process
    // invocation (PM, sub-agent, workflow, --reindex, etc.) is tagged.
    // Resolve project dir so `.open-mpm/state` lands in the project root
    // even when invoked from a subdirectory.
    let state_dir = ctrl::detect_self_project()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".open-mpm")
        .join("state");
    tokio::fs::create_dir_all(&state_dir).await?;
    let build_info = BuildInfo::load_and_increment().await?;
    tracing::info!("{}", build_info.display_string());

    // Feature B3: Initialise the chat logger. The log directory lives under
    // the resolved project's `.open-mpm/state/logs/`, mirroring where other
    // runtime state is written. Cleanup of expired `.log.gz` archives runs
    // synchronously at startup so retention is enforced once per boot.
    {
        let log_cfg = mcp::GlobalConfig::load().await.logging.clone();
        if log_cfg.enabled {
            let log_dir = state_dir.join("logs");
            let _ = std::fs::create_dir_all(&log_dir);
            let logger = logging::ChatLogger::start(log_dir, log_cfg.clone());
            logger.cleanup_old_logs(log_cfg.retain_days);
            logging::init_global(logger);
        }
    }

    // Ensure every process and its subprocesses share a single run_id.
    // Sub-agents inherit this env var when spawned, so all sessions within a
    // PM/workflow invocation land in the same `sessions/<run_id>/` directory.
    // SAFETY: set_var is considered unsafe in Rust 2024; we call it exactly
    // once at startup before any threads that might read env vars are spawned.
    if std::env::var("OPEN_MPM_RUN_ID").is_err() {
        let run_id = uuid::Uuid::new_v4().to_string();
        // SAFETY: single-threaded context at startup.
        unsafe {
            std::env::set_var("OPEN_MPM_RUN_ID", &run_id);
        }
        tracing::debug!(run_id = %run_id, "generated OPEN_MPM_RUN_ID");

        // #session-tagging: Record this session in the lightweight JSON
        // registry so cleanup/export tooling can enumerate it. Best-effort:
        // a write failure here never blocks startup.
        let state_dir = std::path::Path::new(".open-mpm").join("state");
        if let Ok(reg) = session_registry::SessionsRegistry::open(&state_dir) {
            // Workflow is unknown at this point (parsed later from CLI). Use
            // a placeholder; a future enhancement can update it post-parse.
            if let Err(e) = reg.record_start(&run_id, "pending") {
                tracing::debug!(error = %e, "session registry: record_start failed");
            }
        }
    }

    // Migrate legacy `.open-mpm/store/` layout to the new split layout. Safe
    // no-op if already migrated or on first run.
    //
    // NOTE: `open_mpm_dir` here refers to the *runtime state* subdirectory
    // (`.open-mpm/state/`), NOT the repo-root `.open-mpm/` which now holds
    // committed bundled config (agents/, skills/, workflows/, etc.).
    if let Ok(cwd) = std::env::current_dir() {
        let open_mpm_dir = cwd.join(".open-mpm").join("state");
        if open_mpm_dir.exists()
            && let Err(e) = memory::migrate_if_needed(&open_mpm_dir)
        {
            tracing::warn!(error = %e, "memory migration failed (continuing)");
        }

        // #74: Clean up stale worktrees from any prior interrupted run so
        // `git worktree add` doesn't fail with "already registered" errors
        // the next time a parallel phase spins one up.
        let worktree_base = open_mpm_dir.join("worktrees");
        let mgr = workflow::worktree::WorktreeManager::new(worktree_base);
        if let Err(e) = mgr.cleanup_stale().await {
            tracing::warn!(error = %e, "worktree cleanup_stale failed (continuing)");
        }

        // #116: Register the current project in the global project registry and
        // clean up any entries whose directories no longer exist.
        if let Err(e) = async {
            let reg = registry::ProjectRegistry::new()?;
            reg.register(&cwd).await?;
            reg.deregister_missing().await?;
            anyhow::Ok(())
        }
        .await
        {
            tracing::warn!(error = %e, "project registry update failed (continuing)");
        }

        // #130: Clean up stale sub-agent PIDs from `.open-mpm/state/processes.json`
        // left over by any prior crashed run. Best-effort; failures are logged
        // and never block startup.
        {
            let tracker = process_tracker::ProcessTracker::new(&open_mpm_dir);
            match tracker.cleanup_stale().await {
                Ok(n) if n > 0 => {
                    tracing::info!(count = n, "cleaned up stale sub-agent processes");
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "process tracker cleanup failed (continuing)");
                }
            }
        }

        // #117: Start the inter-project message bus in the background.
        // project_id is the directory basename.
        let project_id = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        match bus::MessageBus::start(&project_id).await {
            Ok(_bus_arc) => {
                tracing::debug!(project_id = %project_id, "inter-project message bus started");
                // Bus is kept alive via the Arc returned by `start`; the
                // accept_loop task owns a reference and keeps it alive for the
                // process lifetime.
            }
            Err(e) => {
                tracing::warn!(error = %e, "message bus failed to start (continuing)");
            }
        }
    }

    let args: Vec<String> = std::env::args().collect();

    // #409: Find the subcommand position even when preceded by mode flags.
    //
    // Why: The `om` shell alias prepends `--ctrl` unconditionally, which
    // pushes `session new`, `session list`, etc. past argv[1]. Without this
    // helper, the early-argv dispatch below misses the subcommand and falls
    // through to the CTRL REPL, which swallows the REST API response before
    // the user can see it.
    // What: Returns the index of the first non-flag, non-flag-value token
    // that matches a known subcommand name. We treat tokens starting with
    // `-` as flags and skip a single positional value after `--port`,
    // `--workflow`, `--agent`, etc. (the small set of mode flags that take
    // values and could appear before a subcommand).
    // Test: `om --ctrl session list` should dispatch to handle_session_subcommand,
    // not run_ctrl_repl.
    fn find_subcommand_index(args: &[String], known: &[&str]) -> Option<usize> {
        // Mode flags that take a value and might appear before a subcommand.
        // Keep this list narrow — flags not on it are treated as bare flags.
        const VALUE_FLAGS: &[&str] = &[
            "--port",
            "--workflow",
            "--agent",
            "--out-dir",
            "--lines",
            "--session",
            "--task",
        ];
        let mut i = 1;
        while i < args.len() {
            let a = &args[i];
            if a.starts_with('-') {
                if VALUE_FLAGS.contains(&a.as_str()) {
                    i += 2;
                } else if let Some(eq_pos) = a.find('=')
                    && VALUE_FLAGS.contains(&&a[..eq_pos])
                {
                    i += 1;
                } else {
                    i += 1;
                }
                continue;
            }
            if known.contains(&a.as_str()) {
                return Some(i);
            }
            // First non-flag token that isn't a known subcommand: stop scanning.
            return None;
        }
        None
    }

    // Subcommand prefixes are dispatched before the top-level clap parser
    // runs so each handler can own its own clap schema (and so argv tokens
    // after the subcommand are passed through verbatim).
    //
    // #366: Friendly typo suggestions for top-level subcommands. We only
    // suggest when the first positional token doesn't start with `-` (so
    // top-level flags like `--workflow` still flow through to clap) and
    // when the input doesn't already match a known subcommand. Edit-distance
    // <= 3 catches "memori" -> "memory", "skilss" -> "skills" without
    // hijacking a typed slash command or unrelated arg.
    const KNOWN_SUBCOMMANDS: &[&str] = &[
        "memory",
        "code",
        "memories",
        "agents",
        "skills",
        "inspect",
        "postmortem",
        "debug",
        "eval",
        // #403: persistent service lifecycle subcommands
        "start",
        "stop",
        "status",
        // #405: connect to project + launch REPL in client mode
        "connect",
        // #406: CTRL session management (new/list/attach/kill)
        "session",
        // #442: launch Tauri desktop dashboard GUI ("dash" is an alias)
        "dashboard",
        "dash",
    ];
    if args.len() > 1 {
        let candidate = &args[1];
        if !candidate.starts_with('-')
            && !candidate.starts_with('/')
            && !KNOWN_SUBCOMMANDS.contains(&candidate.as_str())
            && let Some(suggestion) = cli::did_you_mean(candidate, KNOWN_SUBCOMMANDS, 3)
        {
            eprintln!("open-mpm: unknown subcommand '{candidate}'");
            eprintln!("  Did you mean '{suggestion}'?");
            eprintln!("  Run `open-mpm --help` for available commands.");
            std::process::exit(1);
        }
    }

    // CLI subcommands: `memory search`, `memory run`, `code search`.
    // These run against the local store only; no LLM key required.
    if args.len() > 1 && (args[1] == "memory" || args[1] == "code") {
        return cli::run_search_command(&args[1..]).await;
    }

    // `memories <export|import|list>` — cross-machine session sharing.
    if args.len() > 1 && args[1] == "memories" {
        return cli::run_memories_command(&args[2..]).await;
    }

    // #186: `postmortem [--session <id>] [--last N]` — run the postmortem
    // agent against either a specific session's mistake log or the N most
    // recent mistakes from the global log.
    if args.len() > 1 && args[1] == "postmortem" {
        return run_postmortem_subcommand(&args[2..]).await;
    }

    // #167: `agents list` — print all agents discovered from the hierarchical
    // search paths with their source + capability tags. Useful to verify
    // per-project / per-user overrides are being picked up.
    if args.len() > 1 && args[1] == "agents" {
        return run_agents_subcommand(&args[2..]).await;
    }

    // #168: `skills list [--tag <tag>]` — print all skills discovered from the
    // hierarchical search paths with their source + tags. Supports `--tag`
    // to filter + rank by tag overlap.
    if args.len() > 1 && args[1] == "skills" {
        return run_skills_subcommand(&args[2..]).await;
    }

    // #237: `debug [--session <name>] [--lines <N>] [--no-launch]` —
    // launch open-mpm REPL inside detached tmux session and render a
    // ratatui split-pane TUI in the invoking terminal. See
    // `src/debugger/mod.rs` for full behaviour.
    if args.len() > 1 && args[1] == "debug" {
        return debugger::run_debug_subcommand(&args[2..]).await;
    }

    // PM harness inspection: `open-mpm inspect --task <text> [--dry-run]`.
    // Reports which agent + skills the registry would pick for a task
    // without spawning a sub-agent. Dry-run mode does zero LLM calls.
    if args.len() > 1 && args[1] == "inspect" {
        return inspection::run_inspect_subcommand(&args[2..]).await;
    }

    // #414: `open-mpm plugins [list|status|check]` — report which optional
    // MCP plugins (trusty-search, trusty-memory) are present on PATH.
    if args.len() > 1 && args[1] == "plugins" {
        return run_plugins_subcommand(&args[2..]).await;
    }

    // #449: `open-mpm eval run --suite <path>` — run a behavior eval suite.
    if args.len() > 1 && args[1] == "eval" {
        return run_eval_subcommand(&args[2..]).await;
    }

    // #403: `open-mpm start|stop|status` — persistent background server lifecycle.
    //
    // Why: Mirror the existing `--service start|stop|status` flag as
    // first-class subcommands so users can type `om start` instead of
    // `om --service start`. The underlying mechanics (`start_service`,
    // `stop_service`, `status_line` from `src/service/mod.rs`) are reused
    // verbatim — this is purely a friendlier CLI surface.
    // Test: `om start` brings up the daemon, `om status` reports it,
    // `om stop` shuts it down.
    // #409: Dispatch service-lifecycle and session subcommands by scanning
    // for the subcommand token even when preceded by mode flags like
    // `--ctrl` (injected by the `om` shell alias). Without this, those
    // subcommands fall through to the CTRL REPL and the REST API response
    // is swallowed before it can print to stdout.
    // #442: `open-mpm dashboard` — launch the Tauri desktop GUI.
    //
    // Why: Users want a one-shot way to spin up the bundled UI without
    // hunting for the binary path. We resolve it relative to the installed
    // `om` binary and the current working directory so it works whether the
    // user invokes `om` from a clone or after `cargo install`.
    // Test: With the UI built, `om dashboard` should spawn the Tauri binary
    // and exit 0. Without it, it prints a build hint and exits non-zero.
    if let Some(idx) = find_subcommand_index(&args, &["dashboard", "dash"]) {
        return run_dashboard_subcommand(&args[idx + 1..]).await;
    }

    const SERVICE_SUBCOMMANDS: &[&str] = &["start", "stop", "status", "connect", "session"];
    if let Some(idx) = find_subcommand_index(&args, SERVICE_SUBCOMMANDS) {
        match args[idx].as_str() {
            "start" => return run_start_subcommand(&args[idx + 1..]).await,
            "stop" => return run_stop_subcommand(&args[idx + 1..]).await,
            "status" => return run_status_subcommand(&args[idx + 1..]).await,
            "connect" => return run_connect_subcommand(&args[idx + 1..]).await,
            "session" => return handle_session_subcommand(&args[idx + 1..]).await,
            _ => unreachable!("SERVICE_SUBCOMMANDS guards this match"),
        }
    }

    // Top-level clap parse: every non-subcommand mode flag is captured here
    // so the dispatch below can read fields off `cli` instead of rescanning
    // argv five different ways. We use `try_parse_from` so a parse error
    // returns a friendly clap-rendered message instead of panicking.
    //
    // Issue #216: when the parse fails on an unknown subcommand or unknown
    // argument, attach the workspace-shared `trusty_common::help` suggester
    // hint. The native open-mpm `cli::did_you_mean` path above
    // (KNOWN_SUBCOMMANDS scan) catches the common typos before we reach
    // here; this layer covers residual cases that don't match
    // `KNOWN_SUBCOMMANDS` but do match the declarative `help.yaml`.
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(e) => {
            let kind = e.kind();
            if matches!(
                kind,
                clap::error::ErrorKind::InvalidSubcommand | clap::error::ErrorKind::UnknownArgument
            ) {
                eprintln!("{e}");
                trusty_common::help::print_suggestion_hint(&args, &HELP);
                std::process::exit(e.exit_code());
            }
            return Err(anyhow::anyhow!("{e}"));
        }
    };

    // #344: Slash-command passthrough. When the first positional token is a
    // slash command (e.g. `open-mpm /service start`, `open-mpm /help`,
    // `open-mpm /tm list`), execute it via the REPL's slash dispatcher and
    // exit without entering the interactive REPL or any other mode.
    //
    // Why: Operators want a one-shot CLI surface for control commands that
    // already exist as REPL slash handlers — no need to launch a TTY just
    // to run `/service status` or `/help`.
    // What: Reconstructs the slash line by joining `cli.rest` with spaces,
    // builds a minimal REPL instance, dispatches the command, prints the
    // captured output to stdout, and exits with 0 (handled) or 1 (unknown).
    // Test: `slash_passthrough_help_returns_zero` integration test (manual).
    if let Some(first) = cli.rest.first()
        && first.starts_with('/')
    {
        let slash_line = cli.rest.join(" ");
        let user_profile = identity::user_profile::UserProfile::load();
        let mut repl = repl::OpenMpmRepl::new(user_profile)?;
        match repl.try_handle_slash(&slash_line).await {
            Some(Ok((_continue, output))) => {
                // The REPL slash dispatcher captures "unknown command: ..."
                // for slashes it doesn't recognize. Surface that as exit 1
                // so scripts can detect bad commands.
                let is_unknown = output.trim_start().starts_with("unknown command:");
                if !output.is_empty() {
                    print!("{output}");
                    if !output.ends_with('\n') {
                        println!();
                    }
                }
                if is_unknown {
                    std::process::exit(1);
                }
                return Ok(());
            }
            Some(Err(e)) => {
                eprintln!("slash command error: {e:#}");
                std::process::exit(1);
            }
            None => {
                eprintln!("Unknown slash command: {slash_line}");
                std::process::exit(1);
            }
        }
    }

    // #167: Build the agent registry once at startup so the rest of the
    // dispatch path can look up discovered agents by name or by capability.
    // Failure is non-fatal (empty registry just means no dynamic discovery;
    // legacy `AgentConfig::by_name` paths continue to work for bundled agents).
    // #477: `AgentRegistry::load` walks the filesystem (hierarchical search
    // paths) and parses every agent TOML it finds — blocking IO that would
    // otherwise stall the async startup path. Run it on the blocking pool.
    let _registry = {
        let search_paths = agents::registry::agent_search_paths(&default_bundled_config_dir());
        Arc::new(
            tokio::task::spawn_blocking(move || {
                agents::registry::AgentRegistry::load(&search_paths)
            })
            .await
            .expect("AgentRegistry::load panicked"),
        )
    };
    if !_registry.is_empty() {
        tracing::info!(
            count = _registry.len(),
            "agent registry loaded from hierarchical search paths"
        );
    }

    // #168: Build the skill registry at startup so every code path (sub-agents,
    // workflow phases, `skills list` subcommand) shares one scanned, tag-indexed
    // catalog. Missing source dirs are a graceful no-op.
    //
    // #170: This PM-process registry is informational — sub-agents run in
    // separate processes and rebuild their own registry inside
    // `run_subagent`. Logged here so operators can confirm discovery at startup.
    // #172: Load operator-configurable skill sources (.open-mpm/skill-sources.toml)
    // and refresh remote-git caches before scanning. Falls back to the legacy
    // hard-coded paths when no config file is present so existing installs
    // keep working unchanged.
    let project_root_for_skills = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // #477: For the interactive (ctrl) path, restrict the upcoming skill scan
    // to project-local sources. `run_ctrl_inner` used to set this, but that
    // fires *after* the registry below is already built — so the first scan
    // walked every remote source and stalled startup. Setting it here, before
    // the scan, makes the speed-up actually take effect.
    // SAFETY: single-threaded startup context before any spawn.
    if cli.workflow.is_none()
        && cli.agent.is_none()
        && std::env::var("OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY").is_err()
    {
        unsafe {
            std::env::set_var("OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY", "1");
        }
        tracing::debug!("startup: defaulting OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY=1 (interactive)");
    }
    let source_registry = skills::sources::SkillSourceRegistry::load(&project_root_for_skills);
    // Fire-and-forget background refresh: `git fetch`/`clone` blocks startup
    // noticeably when network is slow. The current run uses the on-disk cache
    // as-is; updates land in time for the next launch.
    let source_registry_bg = source_registry.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = source_registry_bg.ensure_remote_sources() {
            tracing::warn!(error = %e, "skill sources: background refresh failed");
        }
    });
    let skill_registry = Arc::new({
        let mut reg = skills::registry::SkillRegistry::from_sources(
            &source_registry,
            &default_bundled_config_dir().join("skills"),
        );
        // #171: Merge persisted effectiveness/usage fields back over the
        // freshly scanned defaults so the system's learning survives restarts.
        let index_path = skills::registry::skill_index_path();
        if let Err(e) = reg.merge_index(&index_path) {
            tracing::warn!(
                error = %e,
                path = %index_path.display(),
                "skill registry: failed to merge persisted effectiveness index (continuing with defaults)"
            );
        }
        reg
    });
    if !skill_registry.is_empty() {
        tracing::info!(
            count = skill_registry.len(),
            "skill registry: indexed skills from hierarchical search paths"
        );
    }

    if let Some(name) = cli.agent.as_deref() {
        return run_subagent(name).await;
    }

    // #193: Top-level (non-agent) invocations are CTRL by default. Setting
    // `OPEN_MPM_CALLER=ctrl` here means any in-process tool that consults
    // `CallerIdentity::from_env()` gets the unrestricted ceiling. Sub-agents
    // override this on their own child Command (see `subprocess.rs`); this
    // never leaks down because `Command::env` is per-child.
    // SAFETY: single-threaded startup context before any spawn.
    if std::env::var(identity::ENV_CALLER).is_err() {
        unsafe {
            std::env::set_var(identity::ENV_CALLER, "ctrl");
        }
    }

    // #350: Symbol registry CLI flags. These run synchronously and exit
    // before any other mode is considered so they're safe in CI / scripts.
    if cli.parse_to_registry {
        let root = std::env::current_dir()?;
        let registry = ast::parse_directory(&root.join("src"), &root)?;
        registry.save()?;
        println!(
            "Registry built: {} symbols → {}",
            registry.len(),
            registry.registry_path().display()
        );
        return Ok(());
    }

    if cli.emit_from_registry {
        let root = std::env::current_dir()?;
        let registry = ast::SymbolRegistry::load(&root)?;
        let rules = ast::LayoutRules::default();
        let outputs = ast::emit(
            &registry,
            &rules,
            &trusty_common::symgraph::ModulePathStrategy::default(),
        )?;
        let written = ast::apply_emit(&outputs, &root)?;
        println!("Emitted {} files", written.len());
        for p in &written {
            println!("  {}", p.display());
        }
        return Ok(());
    }

    if cli.verify_registry {
        let root = std::env::current_dir()?;
        let registry = ast::SymbolRegistry::load(&root)?;
        let stale = registry.verify_hashes();
        if stale.is_empty() {
            println!("Registry OK — all {} hashes match", registry.len());
        } else {
            println!("Stale symbols ({}):", stale.len());
            for id in &stale {
                println!("  {id}");
            }
            std::process::exit(1);
        }
        return Ok(());
    }

    if cli.reindex {
        return run_reindex().await;
    }

    // #343: `--service start|stop|status` — manage the persistent daemon
    // backing `--serve`. We dispatch this before mode-flag handling so it
    // composes cleanly with `--port` (which `start` honors) and so it
    // never falls through to REPL/serve startup.
    if let Some(cmd) = cli.service.as_deref() {
        let port = cli.port.unwrap_or(service::DEFAULT_SERVICE_PORT);
        match cmd {
            "start" => match service::start_service(port).await {
                Ok(state) => {
                    println!(
                        "service started: pid {} port {} (started {})",
                        state.pid,
                        state.port,
                        state.started_at.to_rfc3339()
                    );
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("service start failed: {e:#}");
                    std::process::exit(1);
                }
            },
            "stop" => match service::stop_service().await {
                Ok(()) => {
                    println!("service stopped");
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("service stop failed: {e:#}");
                    std::process::exit(1);
                }
            },
            "status" => {
                println!("{}", service::status_line(port).await);
                return Ok(());
            }
            other => {
                eprintln!("unknown --service subcommand: {other} (use start | stop | status)");
                std::process::exit(2);
            }
        }
    }

    // #151 phase-2: `--serve` / `--api` launches the HTTP API server + web UI.
    // Both flags are accepted; `--api` is the canonical user-facing alias used
    // in the Makefile and README; `--serve` is kept for backwards compat.
    if cli.api || cli.serve {
        let port = cli.port.unwrap_or(8080);
        // #181: bearer token from `--api-token <TOK>` (preferred) or
        // `OPEN_MPM_API_TOKEN` env var. CLI flag takes precedence so an
        // operator can override an env-defaulted token without unsetting it.
        let token = cli
            .api_token
            .clone()
            .or_else(|| std::env::var("OPEN_MPM_API_TOKEN").ok())
            .filter(|s| !s.is_empty());
        return api::server::serve_with_config(api::server::ApiConfig { port, token }).await;
    }

    if cli.check_orphans {
        return run_check_orphans().await;
    }

    if cli.watch {
        return run_watch().await;
    }

    // MIN-3 (#100): `--clear-sessions` now actually clears any persisted
    // session state before this run.
    if cli.clear_sessions {
        let mgr = session::SessionManager::new();
        mgr.clear_all().await;
        tracing::info!(
            "--clear-sessions: persistent agent sessions cleared via SessionManager::clear_all"
        );
    }

    // #126 bug 1: allow inline `--task <STRING>` as an alternative to
    // `--task-file <path>` or piping via stdin.
    //
    // #223: Read --task-file eagerly via std::fs::read_to_string (synchronous,
    // blocking) immediately after clap parse, before any stdin involvement.
    // When stdout is piped the async stdin path inside read_task_text_with_inline
    // can return an empty string (stdin closes in the subshell), producing a
    // spurious "empty task" error. Reading the file here — before the workflow
    // or direct dispatch — ensures the content is always sourced from the file
    // regardless of how stdin/stdout are wired. The task_file *path* is still
    // threaded through to run_workflow for label generation and session records.
    let task_file_content: Option<String> = if let Some(path) = cli.task_file.as_deref() {
        Some(
            std::fs::read_to_string(path)
                .with_context(|| format!("--task-file: failed to read '{path}'"))?,
        )
    } else {
        None
    };
    // inline_task: --task flag takes highest precedence; --task-file content is
    // second; stdin fallback happens inside read_task_text_with_inline when both
    // are None.
    let inline_task: Option<&str> = cli.task.as_deref().or(task_file_content.as_deref());

    // #348: Apply --ast-native override BEFORE any agent runs so the
    // in-process runner sees the flag at registration time.
    if cli.ast_native {
        ast::set_ast_native_override(true);
        tracing::info!("--ast-native: AST-native tool bundle force-enabled for this run");
    }

    // #348: --compare runs the task twice (traditional + ast-native) and
    // emits a side-by-side report. Requires --task or --task-file.
    if cli.compare {
        return run_compare_bakeoff(
            cli.direct.as_deref(),
            cli.workflow.as_deref(),
            cli.task_file.as_deref(),
            inline_task,
        )
        .await;
    }

    // #424: Spawn optional MCP plugins (trusty-search, trusty-memory) once at
    // startup so the agent loop in REPL/--workflow/--direct/--pm modes can
    // actually call their tools. `init_global` is idempotent and degrades
    // gracefully (logs WARN per missing plugin, never crashes the harness).
    // CLI subcommand paths (`om plugins status`, `om start`, etc.) returned
    // earlier and aren't affected.
    //
    // #477: Spawn this off the startup critical path. Plugin init shells out
    // to MCP child processes and runs handshakes — awaiting it inline added
    // noticeable latency before the prompt appeared. `init_global` is
    // idempotent; the agent loop tolerates plugins that aren't ready yet.
    tokio::spawn(async {
        plugins::init_global().await;
    });

    if let Some(name) = cli.workflow.as_deref() {
        return run_workflow(
            name,
            cli.task_file.as_deref(),
            inline_task,
            cli.out_dir.as_deref(),
            cli.project_dir.as_deref(),
            cli.json,
        )
        .await;
    }

    if let Some(name) = cli.direct.as_deref() {
        return run_direct(
            name,
            cli.task_file.as_deref(),
            inline_task,
            cli.out_dir.as_deref(),
        )
        .await;
    }

    // --telegram flag: run the Telegram bot gateway (#264).
    // Why: Lets users drive open-mpm from a phone via @openmpm_bot. Each
    // chat gets its own ChatSession + ConversationTurn history.
    if cli.telegram {
        let project_path = ctrl::detect_self_project()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        // #334: Standalone --telegram mode has no REPL; create a fresh
        // (orphan) pending map. New chats will be told to run /telegram pair
        // in the REPL — which won't exist in this mode. This path is
        // intentionally for ops-only usage; pairing requires the REPL.
        let pending = telegram::new_pending_pairs();
        return telegram::run_telegram_bot(project_path, pending).await;
    }

    // --slack flag: run the Slack Socket Mode bot gateway (#418).
    // Why: Same shape as --telegram — each channel gets its own ChatSession +
    // ConversationTurn history, dispatched through ctrl::run_pm_task_with_history.
    if cli.slack {
        let project_path = ctrl::detect_self_project()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        // Standalone --slack mode has no REPL; create a fresh (orphan)
        // pending map. Pairing requires shell access to the host running
        // the REPL, so this path is ops-only.
        let pending = slack::new_pending_pairs();
        // #480/#481: Parse per-user RBAC + default-persona config from env so
        // the bot routes to `cto-assistant` and enforces tier gating.
        let rbac = std::sync::Arc::new(slack::SlackRbacConfig::from_env());
        return slack::run_slack_bot(project_path, pending, rbac).await;
    }

    // #372: Auto-start the file watcher in the background so the code index
    // tracks the working tree without the user remembering `--watch`. We do
    // this *after* the standalone modes (`--watch`, `--reindex`, `--service`,
    // `--api`) have already returned so we don't fight them for the redb
    // lock, and *before* PM/REPL/CTRL dispatch so any in-process search_code
    // call benefits from a fresh index.
    spawn_background_file_watcher();

    // --pm flag: single-shot PM mode (backward compat)
    if cli.pm {
        return run_pm().await;
    }

    // --ctrl flag: explicit CTRL mode (also the default when no mode flag is set).
    // #120: Even though CTRL is the default, an explicit flag lets scripts be
    // unambiguous when future modes are added.
    let _ = cli.ctrl;

    // #192 Phase A: probe for an existing controller. If one is listening on
    // the per-project socket, forward this invocation's argv as a `task`
    // command and stream its replies. Otherwise fall through and become the
    // controller ourselves.
    //
    // Why: Lets the user run `open-mpm "do X"` from any terminal in a
    // project that already has a CTRL REPL running, without having to
    // know whether the controller is alive. The probe has a hard 50ms
    // budget so a non-running controller does not perceptibly delay startup.
    // What: When forwarded text is non-empty (i.e., the user passed a task on
    // argv), forward it; when empty (bare `open-mpm` re-invocation), we still
    // become the controller — re-binding the socket fails because the first
    // controller already owns it, which is the desired behavior. We log and
    // continue so the second user gets a local REPL anyway.
    let project_id = ctrl::cwd_project_id();
    let sock_path = ctrl::ctrl_socket_path(&project_id);
    let argv_task = argv_as_task_text(&args);
    if !argv_task.trim().is_empty() {
        match ctrl::CtrlSocket::probe_default(&sock_path).await {
            Ok(stream) => {
                tracing::debug!(path = %sock_path.display(), "controller alive — forwarding");
                // Why: One-shot CLI invocations have no prior conversation
                // history, so pass an empty slice. The accumulated output
                // text is discarded — output streamed to stdout already.
                // The controller resolves agent configs relative to the
                // forwarded `cwd`; for one-shot CLI we use the OS cwd since
                // there's no REPL state to consult.
                let argv_cwd =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                return ctrl::forward_to_controller(stream, argv_task, &[], &argv_cwd)
                    .await
                    .map(|_| ());
            }
            Err(e) if ctrl::is_connection_refused(&e) => {
                tracing::debug!(path = %sock_path.display(), "stale ctrl socket — cleaning up");
                ctrl::CtrlSocket::cleanup(&sock_path);
            }
            Err(e) => {
                tracing::debug!(error = %e, "no controller found — starting one");
            }
        }
    }

    // Default interactive mode: use the rich reedline REPL when stdin is a
    // TTY; fall back to the legacy stdin loop in `run_ctrl` otherwise so
    // piped input keeps working unchanged.
    if repl::is_tty() {
        // Profile interview is handled inside `run_ctrl` (via
        // load_or_create_user_profile). To keep its side effects we still
        // start the controller — but we replace its stdin loop with the
        // REPL by spawning the controller in a dedicated task and running
        // the REPL on top.
        //
        // #268 P5: The legacy crossterm banner printer is gone — the ratatui
        // REPL renders its own banner widget once `run()` enters the alt
        // screen, so no pre-spawn banner print is needed here.
        let user_profile = identity::user_profile::UserProfile::load();
        let mut repl = repl::OpenMpmRepl::new(user_profile)?;

        // #477: Wait on an explicit readiness signal instead of a fixed
        // sleep. The controller fires `ctrl_ready_tx` once it reaches the
        // socket-bind stage; the REPL then probes without guessing timing.
        let (ctrl_ready_tx, ctrl_ready_rx) = tokio::sync::oneshot::channel::<()>();
        let ctrl_handle = tokio::spawn(async move {
            if let Err(e) = ctrl::run_ctrl_headless(Some(ctrl_ready_tx)).await {
                tracing::warn!(error = %e, "controller task exited with error");
            }
        });
        // Auto-start Telegram bot as background task if TELEGRAM_BOT_TOKEN is set (#335).
        // #334: Share the REPL's pending-pairs map so /telegram pair codes
        // issued in the REPL are validatable by the bot's /pair handler.
        let _telegram_handle = if std::env::var("TELEGRAM_BOT_TOKEN").is_ok() {
            let tg_project_path = ctrl::detect_self_project()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."));
            let tg_pending = repl.telegram_pairing_handle();
            Some(tokio::spawn(async move {
                if let Err(e) = telegram::run_telegram_bot(tg_project_path, tg_pending).await {
                    tracing::warn!(error = %e, "telegram bot exited with error");
                }
            }))
        } else {
            None
        };
        // Wait for the controller to signal it reached the socket-bind
        // stage. Capped at 200ms so a stalled controller can't block the
        // REPL indefinitely (#477).
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), ctrl_ready_rx).await;

        // #343: If a persistent service is already running on the default
        // port, switch the REPL into thin-client mode so user messages
        // forward to the existing daemon instead of running in-process.
        // We probe synchronously here (with a tight 500ms HTTP budget) so
        // the user sees the connection banner before the prompt appears.
        let service_already_running =
            service::is_service_running(service::DEFAULT_SERVICE_PORT).await;
        if service_already_running {
            let url = format!("http://localhost:{}", service::DEFAULT_SERVICE_PORT);
            let started = service::read_pid_file()
                .map(|s| {
                    format!(
                        "pid {} port {} since {}",
                        s.pid,
                        s.port,
                        s.started_at.to_rfc3339()
                    )
                })
                .unwrap_or_else(|| format!("port {}", service::DEFAULT_SERVICE_PORT));
            eprintln!("--- connected to running open-mpm service ---");
            eprintln!("    {}", started);
            eprintln!("    (use `/service stop` to shut it down)");
            repl.set_service_client_mode(url);
        }

        // #364: auto-launch Tauri desktop GUI on startup.
        // The Tauri app manages its own API sidecar (open-mpm --api --port 7654),
        // so we only need to open the .app bundle — no server spawn here.
        // Resolve the app path relative to OPEN_MPM_PROJECT_DIR (set by the `om` wrapper)
        // or relative to cwd, falling back gracefully if the bundle isn't built.
        {
            let app_path = std::env::var("OPEN_MPM_PROJECT_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| {
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                })
                .join("ui/src-tauri/target/release/bundle/macos/open-mpm.app");

            if app_path.exists() {
                tracing::info!(path = %app_path.display(), "launching Tauri desktop GUI");
                let _ = std::process::Command::new("open").arg(&app_path).spawn();
            } else {
                tracing::debug!(
                    path = %app_path.display(),
                    "Tauri app not found — skipping GUI launch (run `cd ui && pnpm tauri build` to build it)"
                );
            }
        }

        let result = repl.run().await;
        ctrl_handle.abort();
        if let Some(h) = _telegram_handle {
            h.abort();
        }
        return result;
    }

    ctrl::run_ctrl().await
}

/// Concatenate non-flag positional args into a single task string.
///
/// Why: When the user runs `open-mpm "say hi"` (or `open-mpm say hi`), we
/// want to forward "say hi" — but only the parts that aren't mode flags
/// already filtered above. Mode-flagged invocations short-circuit before
/// reaching this function.
/// What: Skips argv[0] (binary name) and any token starting with `--`.
/// Joins the remainder with single spaces.
/// Test: `argv_as_task_text_strips_flags_and_joins`.
/// Print a prominent onboarding banner when no API credential is configured.
///
/// Why: New users who clone the repo and run `om` without configuring a key
/// get confusing LLM errors. Surfacing setup instructions before the REPL
/// opens is friendlier and self-service. OpenRouter is recommended because
/// it's free-tier, supports many models, and is already the deployment
/// fallback.
/// What: Checks for any of the three supported credential env vars; when
/// none are set, prints a boxed banner to stderr with setup steps and the
/// OpenRouter sign-up URL. Non-fatal — the REPL still opens so CLI-only
/// subcommands (memory search, skills list) keep working.
/// Test: Manual — unset all three env vars and run `cargo run`. Banner should
/// appear once on stderr; setting any one of the three suppresses it.
fn check_credentials_and_warn() {
    let has_claude_code = std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let has_anthropic = std::env::var("ANTHROPIC_API_KEY")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let has_openrouter = std::env::var("OPENROUTER_API_KEY")
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    if has_claude_code || has_anthropic || has_openrouter {
        return;
    }

    eprintln!();
    eprintln!("┌─────────────────────────────────────────────────────────────────┐");
    eprintln!("│  ⚡  No API key found — open-mpm needs a key to talk to an LLM  │");
    eprintln!("├─────────────────────────────────────────────────────────────────┤");
    eprintln!("│                                                                 │");
    eprintln!("│  Quickest option — get a free OpenRouter key (5 min):           │");
    eprintln!("│    https://openrouter.ai/keys                                   │");
    eprintln!("│                                                                 │");
    eprintln!("│  Then create .env.local in your project root:                   │");
    eprintln!("│    echo 'OPENROUTER_API_KEY=sk-or-v1-...' >> .env.local         │");
    eprintln!("│                                                                 │");
    eprintln!("│  Or use Claude Code OAuth (if you have Claude Code installed):  │");
    eprintln!("│    claude setup-token   # copies token to clipboard             │");
    eprintln!("│    echo 'CLAUDE_CODE_OAUTH_TOKEN=...' >> .env.local             │");
    eprintln!("│                                                                 │");
    eprintln!("│  Restart open-mpm after adding the key. (REPL continues below)  │");
    eprintln!("└─────────────────────────────────────────────────────────────────┘");
    eprintln!();
}

fn argv_as_task_text(args: &[String]) -> String {
    args.iter()
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod main_tests {
    use super::{Cli, argv_as_task_text, read_task_text_with_inline};
    use clap::Parser;

    #[test]
    fn argv_as_task_text_strips_flags_and_joins() {
        let args: Vec<String> = vec!["open-mpm", "write", "hello", "world"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(argv_as_task_text(&args), "write hello world");
    }

    #[test]
    fn argv_as_task_text_ignores_long_flags() {
        let args: Vec<String> = vec!["open-mpm", "--ctrl", "do", "thing"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(argv_as_task_text(&args), "do thing");
    }

    #[test]
    fn argv_as_task_text_empty_when_no_positional() {
        let args: Vec<String> = vec!["open-mpm".to_string()];
        assert_eq!(argv_as_task_text(&args), "");
    }

    /// Why: #223 — verify clap parses --task-file correctly so the path is
    /// not lost into `rest` due to trailing_var_arg interaction.
    /// What: Parses a workflow invocation with --task-file and asserts that
    /// `task_file` is `Some` and `rest` is empty.
    /// Test: This test itself.
    #[test]
    fn clap_task_file_parses_correctly_with_workflow() {
        let args = vec![
            "open-mpm",
            "--workflow",
            "prescriptive",
            "--task-file",
            "level-1.txt",
        ];
        let cli = Cli::try_parse_from(args).expect("clap should parse");
        assert_eq!(
            cli.task_file.as_deref(),
            Some("level-1.txt"),
            "task_file should capture the path, not be None"
        );
        assert_eq!(cli.workflow.as_deref(), Some("prescriptive"));
        assert!(
            cli.rest.is_empty(),
            "rest should not consume the --task-file value: {:?}",
            cli.rest
        );
    }

    /// Why: #223 — verify clap parses --task-file with --out-dir correctly.
    /// What: Ensures multiple named flags all parse without leaking values
    /// into `rest`.
    /// Test: This test itself.
    #[test]
    fn clap_task_file_parses_correctly_with_out_dir() {
        let args = vec![
            "open-mpm",
            "--workflow",
            "prescriptive",
            "--task-file",
            "tasks/level-2.txt",
            "--out-dir",
            "/tmp/out",
        ];
        let cli = Cli::try_parse_from(args).expect("clap should parse");
        assert_eq!(cli.task_file.as_deref(), Some("tasks/level-2.txt"));
        assert_eq!(cli.out_dir.as_deref(), Some("/tmp/out"));
        assert!(cli.rest.is_empty(), "rest should be empty: {:?}", cli.rest);
    }

    /// Why: #223 — verify read_task_text_with_inline returns file content
    /// when task_file is None but inline_task is provided (simulates the
    /// eagerly-read file content path added by the #223 fix).
    /// What: Inline task content bypasses all file/stdin reads.
    /// Test: This test itself.
    #[tokio::test]
    async fn read_task_text_inline_takes_priority_over_file() {
        let result = read_task_text_with_inline(None, Some("  hello world  "))
            .await
            .unwrap();
        assert_eq!(result, "hello world");
    }

    /// Why: #223 — verify read_task_text_with_inline reads from an actual file
    /// when task_file path is given and inline_task is None.
    /// What: Writes a temp file, calls the function, asserts content is read.
    /// Test: This test itself.
    #[tokio::test]
    async fn read_task_text_reads_from_file_when_path_given() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("task.txt");
        std::fs::write(&path, "  write a hello world script  ").unwrap();
        let path_str = path.to_str().unwrap();
        let result = read_task_text_with_inline(Some(path_str), None)
            .await
            .unwrap();
        assert_eq!(result, "write a hello world script");
    }
}

/// Bundled agent config directory — honors `OPEN_MPM_CONFIG_DIR` with a
/// CWD-relative `.open-mpm/` fallback (#167).
///
/// Why: The registry search-path function wants the "bundled" config root
/// (it appends `/agents` internally). We honor the same env var as
/// `agents::mod::agent_config_path` so packaged binaries can point the
/// loader at a vendored config tree.
/// What: Returns `${OPEN_MPM_CONFIG_DIR}` as-is if set (so search_paths
/// appends `/agents`), else `./.open-mpm`. Note: the repo's bundled config
/// lives at `.open-mpm/` now (formerly `config/`); runtime state is in
/// `.open-mpm/state/` (gitignored).
// Why: `default_bundled_config_dir` is defined once in `crate::lib` and
//      pulled in at the top of this file via `use crate::default_bundled_config_dir;`.
//      No duplicate definition here.

/// Persist post-run skill effectiveness + usage to `~/.open-mpm/skills/index.json`
/// (#171, #174).
///
/// Why: Skill rankings only improve over time if observations from each run
/// flow back into the persisted score. The observe-agent now emits a
/// structured `## Skill Ratings` block (#174) that lets us feed fine-grained,
/// per-skill scores instead of one coarse pass/fail signal applied to every
/// injected skill. When the structured block is absent (older runs, observe
/// skipped, parse failure) we fall back to the original status-derived signal
/// so this hook always produces some signal.
/// What: Rebuilds the registry from the canonical search paths, merges the
/// existing index, increments `use_count` + `last_used` for each skill in
/// `perf_record.skills_used`. If `observe_output` contains a `## Skill Ratings`
/// block with at least one parseable rating, applies those scores via
/// `update_effectiveness`. Otherwise applies a coarse status-derived signal
/// (`success`→0.8, `partial`→0.5, anything else→0.3) to every used skill.
/// All errors are logged at WARN and swallowed so persistence never breaks a
/// run.
/// Test: Indirect for I/O — verified by running with skills auto-injected and
/// inspecting `~/.open-mpm/skills/index.json`. Behavior is unit-tested at the
/// registry level (`merge_index_restores_effectiveness_after_reload`) and the
/// rating-parser level (`parse_skill_ratings_*`).
fn update_skill_usage_after_run(perf_record: &perf::PerfRecord, observe_output: Option<&str>) {
    if perf_record.skills_used.is_empty() {
        return;
    }
    let mut reg = skills::registry::SkillRegistry::load(&skills::registry::skill_search_paths(
        &default_bundled_config_dir(),
    ));
    let index_path = skills::registry::skill_index_path();
    if let Err(e) = reg.merge_index(&index_path) {
        tracing::warn!(
            error = %e,
            path = %index_path.display(),
            "skill registry: failed to merge persisted index before update (continuing)"
        );
    }

    let now_iso = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    // Always record the usage counter / last_used timestamp for every skill
    // that was injected — that's independent of how we score effectiveness.
    for name in &perf_record.skills_used {
        reg.record_use(name, &now_iso);
    }

    // Prefer fine-grained ratings emitted by the observe-agent (#174). If the
    // structured block is present and parseable, only those skills receive
    // updates this run; coarse fallback only applies when no ratings are found.
    let ratings = observe_output
        .map(skills::rating::parse_skill_ratings)
        .unwrap_or_default();

    let updated_count = if !ratings.is_empty() {
        for rating in &ratings {
            reg.update_effectiveness(&rating.skill, rating.score);
        }
        tracing::info!(
            count = ratings.len(),
            "skill ratings: updated {} skills from observe-agent",
            ratings.len()
        );
        ratings.len()
    } else {
        // Coarse fallback: derive a single signal from run status and apply it
        // to every injected skill. This guarantees even non-rating runs (older
        // observe-agent prompts, observe phase skipped, parse errors) still
        // contribute *some* signal to the EMA.
        let signal = skills::rating::coarse_fallback_signal(&perf_record.status);
        for name in &perf_record.skills_used {
            reg.update_effectiveness(name, signal);
        }
        tracing::info!(
            count = perf_record.skills_used.len(),
            status = %perf_record.status,
            signal = signal,
            "skill ratings: no structured block found; applied coarse fallback"
        );
        perf_record.skills_used.len()
    };

    if let Err(e) = reg.save_index(&index_path) {
        tracing::warn!(
            error = %e,
            path = %index_path.display(),
            "skill registry: failed to save updated effectiveness index (continuing)"
        );
    } else {
        tracing::info!(
            count = updated_count,
            status = %perf_record.status,
            path = %index_path.display(),
            "skill registry: persisted post-run effectiveness update"
        );
    }
}

/// Handle `open-mpm agents <subcommand>` (#167).
///
/// Why: Exposes the discovery results to operators. Without this, there's
/// no way to verify which agents were picked up from which directory.
/// What: Currently supports `agents list`. Prints discovered agents with
/// their source and capability tags in the format described in the issue.
/// Test: Covered manually; unit-tested via `AgentRegistry::list`.
async fn run_agents_subcommand(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "list" => {
            let reg = agents::registry::AgentRegistry::load(&agents::registry::agent_search_paths(
                &default_bundled_config_dir(),
            ));
            let items = reg.list();
            println!("Discovered agents ({}):", items.len());
            let bundled = default_bundled_config_dir().join("agents");
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let max_name = items.iter().map(|s| s.name.len()).max().unwrap_or(0).max(4);
            for s in items {
                let source_label = classify_source(&s.source, &bundled, home.as_deref());
                let mut parts = Vec::new();
                if !s.roles.is_empty() {
                    parts.push(format!("roles: {}", s.roles.join(",")));
                }
                if !s.languages.is_empty() {
                    parts.push(format!("languages: {}", s.languages.join(",")));
                }
                if !s.frameworks.is_empty() {
                    parts.push(format!("frameworks: {}", s.frameworks.join(",")));
                }
                if !s.tags.is_empty() {
                    parts.push(format!("tags: {}", s.tags.join(",")));
                }
                println!(
                    "  {name:<width$}  [{src}]  {caps}",
                    name = s.name,
                    width = max_name,
                    src = source_label,
                    caps = parts.join("  ")
                );
            }
            Ok(())
        }
        other => {
            // #366: Surface a "did you mean?" hint for typos like
            // `agents lst` -> `agents list`.
            let known = &["list"];
            if let Some(s) = cli::did_you_mean(other, known, 2) {
                eprintln!("open-mpm agents: unknown subcommand '{other}'. Did you mean '{s}'?");
            } else {
                eprintln!("open-mpm agents: unknown subcommand '{other}'. Try: list");
            }
            bail!("unknown agents subcommand: {other}");
        }
    }
}

/// Handle `open-mpm plugins <subcommand>` (#414).
///
/// Why: Operators need a quick way to confirm which optional MCP plugins
/// (trusty-search, trusty-memory) the harness is able to spawn. Without
/// this surface, plugin misconfiguration is invisible until an agent tries
/// to use a missing tool.
/// What: Supports `list`, `status` (default), and `check`. All three
/// currently render the same status table; we keep them as distinct verbs
/// so future expansion (e.g. `check` returning non-zero on missing plugins)
/// doesn't break existing scripts.
/// Test: Manual — `om plugins status` on a machine without the trusty
/// binaries reports both as UNAVAILABLE; with binaries on PATH and an MCP
/// handshake, both report ACTIVE.
async fn run_plugins_subcommand(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("status");
    match sub {
        "list" | "status" | "check" => {
            print_plugins_status().await;
            Ok(())
        }
        other => {
            let known = &["list", "status", "check"];
            if let Some(s) = cli::did_you_mean(other, known, 2) {
                eprintln!("open-mpm plugins: unknown subcommand '{other}'. Did you mean '{s}'?");
            } else {
                eprintln!(
                    "open-mpm plugins: unknown subcommand '{other}'. Try: list | status | check"
                );
            }
            bail!("unknown plugins subcommand: {other}");
        }
    }
}

/// Why: Wire `om eval run --suite <path> [--agent <toml>] [--json]` (#449)
/// into the CLI dispatch. Loads the suite, resolves the agent system prompt
/// (defaults to a generic helpful-assistant prompt), drives the live
/// OpenRouter client, and prints either a human-friendly report or a JSON
/// array of `EvalResult`.
/// What: Subcommands: `run`. Exit code 0 iff all cases pass.
/// Test: Eval framework itself is unit-tested in `src/eval/mod.rs`; this
/// function is integration-level (requires OPENROUTER_API_KEY).
async fn run_eval_subcommand(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("run");
    if sub != "run" {
        eprintln!("open-mpm eval: unknown subcommand '{sub}'. Try: run");
        bail!("unknown eval subcommand: {sub}");
    }

    // Parse flags: --suite <path> [--agent <toml>] [--json]
    let rest = &args[1..];
    let mut suite_path: Option<String> = None;
    let mut agent_path: Option<String> = None;
    let mut as_json = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--suite" => {
                suite_path = rest.get(i + 1).cloned();
                i += 2;
            }
            "--agent" => {
                agent_path = rest.get(i + 1).cloned();
                i += 2;
            }
            "--json" => {
                as_json = true;
                i += 1;
            }
            other => {
                eprintln!("open-mpm eval run: unknown flag '{other}'");
                bail!("unknown flag");
            }
        }
    }

    let suite_path = suite_path.ok_or_else(|| anyhow::anyhow!("--suite <path> is required"))?;
    let suite = eval::EvalSuite::from_toml(std::path::Path::new(&suite_path))?;

    // Resolve agent system prompt + model.
    let (system_prompt, model) = if let Some(p) = agent_path.as_deref() {
        let cfg = agents::AgentConfig::load(std::path::Path::new(p))?;
        (cfg.system_prompt.content.clone(), cfg.agent.model.clone())
    } else {
        (
            "You are a helpful assistant.".to_string(),
            "anthropic/claude-sonnet-4-6".to_string(),
        )
    };

    // Live LLM client adapter — uses the existing OpenRouter chat path.
    let client = llm::create_client()?;
    let live = LiveEvalClient {
        client,
        model: model.clone(),
    };

    println!("Running {} eval cases...\n", suite.cases.len());
    let results = suite.run(&system_prompt, &live).await;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        print!("{}", eval::EvalSuite::report(&results));
    }

    let failed = results.iter().filter(|r| !r.passed).count();
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Live `EvalLlmClient` driven by the existing OpenRouter chat path.
struct LiveEvalClient {
    client: async_openai::Client<async_openai::config::OpenAIConfig>,
    model: String,
}

#[async_trait::async_trait]
impl eval::EvalLlmClient for LiveEvalClient {
    async fn complete_with_tools(
        &self,
        system: &str,
        user: &str,
        _user_tier: Option<&str>,
    ) -> Result<(String, Vec<String>)> {
        let resp = llm::chat(&self.client, &self.model, system, user, 0.0, 1024, vec![]).await?;
        let names = resp.tool_calls.iter().map(|t| t.name.clone()).collect();
        Ok((resp.content.unwrap_or_default(), names))
    }
}

/// Render the plugin status table to stdout.
///
/// Why: Shared by `list`, `status`, and `check` so output stays consistent.
/// What: Initialises a `PluginManager`, prints one line per known plugin
/// with state and either the discovered binary path or an install hint.
async fn print_plugins_status() {
    use plugins::PluginState;
    // #424: Reuse the process-wide manager when one is already initialised
    // (e.g. when this is reached via an in-REPL command in the future). At
    // CLI top-level the OnceLock is empty, so we fall back to a fresh
    // `init_global()` so the global is also populated for any subsequent
    // operations in the same process.
    let mgr = match plugins::plugin_manager() {
        Some(existing) => existing,
        None => plugins::init_global().await,
    };
    let s = mgr.status();
    println!("Plugin Status:");
    print_plugin_row("trusty-search", s.search, "cargo install trusty-search");
    print_plugin_row("trusty-memory", s.memory, "cargo install trusty-memory");

    fn print_plugin_row(name: &str, state: PluginState, install_hint: &str) {
        let detail = match state {
            PluginState::Active => match resolve_binary_path(name) {
                Some(p) => format!("(path: {p})"),
                None => String::new(),
            },
            PluginState::Unavailable => format!("(install: {install_hint})"),
        };
        println!("  {name:<14}  {:<11}  {detail}", state.label());
    }

    fn resolve_binary_path(name: &str) -> Option<String> {
        let out = std::process::Command::new("which")
            .arg(name)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if path.is_empty() { None } else { Some(path) }
    }
}

/// Handle `open-mpm start [--port <port>]` (#403).
///
/// Why: Friendly subcommand alias for `--service start` so users get the
/// daemon-management UX they expect from CLIs like `nginx start` or
/// `docker start`. Reuses `service::start_service` so behaviour and pid-file
/// semantics stay identical.
/// What: Parses optional `--port <u16>` (default 8765 to align with the
/// open-mpm web UI port), polls `/api/health` for up to 10s, prints PID +
/// port on success.
/// Test: `om start` then `om status` then `om stop` against a clean repo.
/// Handle `open-mpm dashboard` (#442).
///
/// Why: Surface the Tauri desktop UI behind a friendly subcommand so users
/// can run `om dashboard` without remembering build paths. We probe a small
/// set of candidate locations (release-first, then debug) and spawn the
/// binary detached if found.
/// What: Tries `<om_dir>/../../ui/src-tauri/target/release/open-mpm-ui`,
/// then `<cwd>/ui/src-tauri/target/release/open-mpm-ui`, then the debug
/// equivalent. On hit: spawns + exits 0. On miss: prints a build hint and
/// exits 1.
/// Test: Manual — `om dashboard` should pop the GUI when built; the error
/// path is exercised by deleting the binaries and re-running.
async fn run_dashboard_subcommand(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: open-mpm dashboard|dash");
        println!();
        println!("Launches the Tauri desktop GUI for open-mpm.");
        return Ok(());
    }
    if !args.is_empty() {
        bail!("`dashboard` takes no arguments (got {:?})", args);
    }

    // Candidate paths, in priority order: release first (installed/used),
    // then cwd-relative release, then cwd-relative debug.
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        // `<exe_dir>/../../ui/src-tauri/target/release/open-mpm-ui`
        candidates.push(
            exe_dir
                .join("..")
                .join("..")
                .join("ui")
                .join("src-tauri")
                .join("target")
                .join("release")
                .join("open-mpm-ui"),
        );
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(
            cwd.join("ui")
                .join("src-tauri")
                .join("target")
                .join("release")
                .join("open-mpm-ui"),
        );
        candidates.push(
            cwd.join("ui")
                .join("src-tauri")
                .join("target")
                .join("debug")
                .join("open-mpm-ui"),
        );
    }

    let found = candidates.into_iter().find(|p| p.is_file());
    let Some(binary) = found else {
        eprintln!("Dashboard UI not built. Run: cd ui && npm run tauri:build");
        std::process::exit(1);
    };

    println!("Launching dashboard: {}", binary.display());
    match tokio::process::Command::new(&binary).spawn() {
        Ok(_child) => {
            // Detach: drop the child handle so we don't wait for it. The
            // GUI runs independently of the `om` shell.
            Ok(())
        }
        Err(e) => {
            eprintln!("failed to launch dashboard: {e}");
            std::process::exit(1);
        }
    }
}

async fn run_start_subcommand(args: &[String]) -> Result<()> {
    let mut port: u16 = 8765;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--port" {
            let v = args
                .get(i + 1)
                .ok_or_else(|| anyhow::anyhow!("--port requires a value"))?;
            port = v
                .parse::<u16>()
                .map_err(|e| anyhow::anyhow!("invalid --port: {e}"))?;
            i += 2;
            continue;
        }
        if args[i] == "--help" || args[i] == "-h" {
            println!("Usage: open-mpm start [--port <port>]");
            return Ok(());
        }
        bail!("unknown argument to `start`: {}", args[i]);
    }

    println!("Starting open-mpm server on port {port}...");
    match service::start_service(port).await {
        Ok(state) => {
            // service::start_service already polls /api/health for up to 3s
            // before returning; do an additional 7s budget here so we hit a
            // 10s total ceiling per the spec.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(7);
            while std::time::Instant::now() < deadline && !service::is_service_running(port).await {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
            println!("Server started (PID: {}, port: {})", state.pid, state.port);
            Ok(())
        }
        Err(e) => {
            eprintln!("start failed: {e:#}");
            std::process::exit(1);
        }
    }
}

/// Handle `open-mpm stop` (#403).
///
/// Why: Symmetric with `start`; reuses `service::stop_service` which sends
/// SIGTERM (escalating to SIGKILL after 3s) and removes the pid file.
/// What: No arguments accepted. Prints a short progress line then "stopped".
/// Test: `om stop` after `om start` removes `.open-mpm/state/service.pid`.
async fn run_stop_subcommand(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: open-mpm stop");
        return Ok(());
    }
    if !args.is_empty() {
        bail!("`stop` takes no arguments (got {:?})", args);
    }

    println!("Stopping open-mpm server...");
    match service::stop_service().await {
        Ok(()) => {
            println!("Server stopped.");
            Ok(())
        }
        Err(e) => {
            eprintln!("stop failed: {e:#}");
            std::process::exit(1);
        }
    }
}

/// Handle `open-mpm status [--port <port>]` (#403).
///
/// Why: Quick "is the server up?" check without grepping `ps`. Reuses
/// `service::status_line` so the format matches `--service status` and
/// `/service status` in the REPL.
/// What: Prints the human-readable status line for the configured port.
/// Test: With and without a running daemon, the line distinguishes them.
async fn run_status_subcommand(args: &[String]) -> Result<()> {
    let mut port: u16 = 8765;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--port" {
            let v = args
                .get(i + 1)
                .ok_or_else(|| anyhow::anyhow!("--port requires a value"))?;
            port = v
                .parse::<u16>()
                .map_err(|e| anyhow::anyhow!("invalid --port: {e}"))?;
            i += 2;
            continue;
        }
        if args[i] == "--help" || args[i] == "-h" {
            println!("Usage: open-mpm status [--port <port>]");
            return Ok(());
        }
        bail!("unknown argument to `status`: {}", args[i]);
    }

    println!("{}", service::status_line(port).await);
    Ok(())
}

/// Handle `open-mpm connect <path> [--agent <name>]` (#405).
///
/// Why: Lets users register an arbitrary project directory with the running
/// server and immediately drop into a REPL scoped to that project. Mirrors
/// the `--project-dir` UX but routes through the daemon, so the same
/// long-running server can host multiple projects.
/// What: Resolves the path, POSTs `/api/projects` to register it, then
/// prints a confirmation. Launching the REPL in client mode is a
/// follow-up — for now we leave the user a clear next-step hint.
/// Test: With the server running, `om connect .` returns 200 from
/// `/api/projects` and prints the resolved name + path.
async fn run_connect_subcommand(args: &[String]) -> Result<()> {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: open-mpm connect <path> [--agent <name>] [--port <port>]");
        return Ok(());
    }

    let mut path: Option<PathBuf> = None;
    let mut agent: Option<String> = None;
    let mut port: u16 = 8765;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--agent" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("--agent requires a value"))?;
                agent = Some(v.clone());
                i += 2;
            }
            "--port" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("--port requires a value"))?;
                port = v
                    .parse::<u16>()
                    .map_err(|e| anyhow::anyhow!("invalid --port: {e}"))?;
                i += 2;
            }
            other if other.starts_with("--") => {
                bail!("unknown argument to `connect`: {other}");
            }
            _ => {
                if path.is_none() {
                    path = Some(PathBuf::from(&args[i]));
                } else {
                    bail!(
                        "`connect` takes a single path positional (got extra: {})",
                        args[i]
                    );
                }
                i += 1;
            }
        }
    }

    let path = path.ok_or_else(|| anyhow::anyhow!("connect: missing <path>"))?;
    let abs_path = path.canonicalize().unwrap_or(path);
    let name = abs_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| abs_path.to_string_lossy().to_string());

    // Register the project with the running server.
    let url = format!("http://127.0.0.1:{port}/api/projects");
    let body = serde_json::json!({
        "path": abs_path.to_string_lossy(),
        "name": name,
    });
    let client = reqwest::Client::new();
    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            println!("Connected to project: {name} ({})", abs_path.display());
            if let Some(a) = agent.as_deref() {
                println!("(agent override: {a})");
            }
            println!("Tip: launch the REPL with `open-mpm` to chat with the running server.");
            Ok(())
        }
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("server rejected /api/projects: {status} {text}");
        }
        Err(e) => {
            bail!("could not reach server at {url}: {e}. Is `open-mpm start` running?");
        }
    }
}

/// Handle `open-mpm session <new|list|attach|kill|run>` (#406).
///
/// Why: Gives users a CLI surface to manage interactive REPL sessions backed
/// by the running open-mpm server, including optional git worktree creation
/// so multiple agents can work on the same repo in parallel.
/// What: Resolves the server port and dispatches to a per-subcommand helper.
/// All subcommands flow through HTTP to `/api/ctrl/sessions*` on the
/// configured `--port` (default 8765).
/// Test: Smoke-tested by creating, listing, attaching to, killing, and
/// running a session against a running server.
async fn handle_session_subcommand(args: &[String]) -> Result<()> {
    let port = extract_port_flag(args).unwrap_or(8765);
    let base_url = format!("http://127.0.0.1:{}", port);
    let client = reqwest::Client::new();

    match args.first().map(|s| s.as_str()) {
        Some("new") => handle_session_new(args, &base_url, &client).await,
        Some("list") => handle_session_list(args, &base_url, &client).await,
        Some("attach") => handle_session_attach(args, &base_url, &client).await,
        Some("kill") => handle_session_kill(args, &base_url, &client).await,
        Some("run") => handle_session_run(args, port).await,
        _ => {
            print_session_usage();
            Ok(())
        }
    }
}

// Purpose: `om session new` — POST a new session to the server and print
// the resulting ID, name, agent, status, and worktree info if any.
async fn handle_session_new(
    args: &[String],
    base_url: &str,
    client: &reqwest::Client,
) -> Result<()> {
    let project = extract_flag(args, "--project")
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        })
        .unwrap_or_default();
    let name = extract_flag(args, "--name")
        .unwrap_or_else(|| format!("session-{}", &uuid::Uuid::new_v4().to_string()[..8]));
    let agent = extract_flag(args, "--agent").unwrap_or_else(|| "pm".to_string());
    let worktree = args.iter().any(|a| a == "--worktree");

    let body = serde_json::json!({
        "project_path": project,
        "name": name,
        "agent": agent,
        "worktree": worktree,
    });

    let resp = client
        .post(format!("{}/api/ctrl/sessions", base_url))
        .json(&body)
        .send()
        .await?;

    if resp.status().is_success() {
        let session: serde_json::Value = resp.json().await?;
        // #409: Print session details in a stable, human-readable
        // block. Order: ID, Name, Project, Agent, Status. Worktree
        // fields appear only when the server actually provisioned one.
        println!("Session created:");
        println!("  ID:      {}", session["id"].as_str().unwrap_or("?"));
        println!("  Name:    {}", session["name"].as_str().unwrap_or("?"));
        println!(
            "  Project: {}",
            session["project_name"].as_str().unwrap_or("?")
        );
        println!("  Agent:   {}", session["agent"].as_str().unwrap_or("?"));
        println!(
            "  Status:  {}",
            session["status"].as_str().unwrap_or("idle")
        );
        if let Some(wt) = session["worktree_path"].as_str() {
            println!("  Worktree: {}", wt);
            println!(
                "  Branch:   {}",
                session["worktree_branch"].as_str().unwrap_or("?")
            );
        }
        println!();
        println!(
            "To attach: om session attach {}",
            session["id"].as_str().unwrap_or("?")
        );
    } else {
        eprintln!("Failed to create session: {}", resp.status());
    }

    Ok(())
}

// Purpose: `om session list` — GET sessions from the server, optionally
// filtered by project, and pretty-print as a fixed-width table.
async fn handle_session_list(
    args: &[String],
    base_url: &str,
    client: &reqwest::Client,
) -> Result<()> {
    let project_filter = args
        .get(1)
        .filter(|a| !a.starts_with('-'))
        .map(|p| format!("?project={}", p))
        .unwrap_or_default();

    let resp = client
        .get(format!("{}/api/ctrl/sessions{}", base_url, project_filter))
        .send()
        .await?;

    if resp.status().is_success() {
        let data: serde_json::Value = resp.json().await?;
        let sessions = data["sessions"].as_array().cloned().unwrap_or_default();
        if sessions.is_empty() {
            println!("No sessions found.");
        } else {
            println!(
                "{:<36}  {:<20}  {:<15}  {:<8}  STATUS",
                "ID", "NAME", "PROJECT", "AGENT"
            );
            println!("{}", "-".repeat(100));
            for s in &sessions {
                println!(
                    "{:<36}  {:<20}  {:<15}  {:<8}  {}{}",
                    s["id"].as_str().unwrap_or("?"),
                    s["name"].as_str().unwrap_or("?"),
                    s["project_name"].as_str().unwrap_or("?"),
                    s["agent"].as_str().unwrap_or("?"),
                    s["status"].as_str().unwrap_or("?"),
                    if s["worktree_path"].is_string() {
                        " [worktree]"
                    } else {
                        ""
                    },
                );
            }
        }
    } else {
        eprintln!("Failed to list sessions: {}", resp.status());
    }

    Ok(())
}

// Purpose: `om session attach <id>` — POST attach, then re-exec the current
// binary inside the session's working_dir with OPEN_MPM_SESSION_ID /
// OPEN_MPM_AGENT exported. This function ends with `process::exit` on
// success, so it never returns to the caller.
async fn handle_session_attach(
    args: &[String],
    base_url: &str,
    client: &reqwest::Client,
) -> Result<()> {
    let id = args
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("Usage: om session attach <session-id>"))?;

    let resp = client
        .post(format!("{}/api/ctrl/sessions/{}/attach", base_url, id))
        .send()
        .await?;

    if resp.status().is_success() {
        let info: serde_json::Value = resp.json().await?;
        let working_dir = info["working_dir"].as_str().unwrap_or(".").to_string();
        let agent = info["agent"].as_str().unwrap_or("pm").to_string();
        let name = info["name"].as_str().unwrap_or("session").to_string();

        println!("Attaching to session '{}' (agent: {})...", name, agent);
        println!("Working directory: {}", working_dir);
        println!();

        let exe = std::env::current_exe()?;
        let mut cmd = std::process::Command::new(&exe);
        cmd.env("OPEN_MPM_SESSION_ID", id)
            .env("OPEN_MPM_AGENT", &agent)
            .current_dir(&working_dir);

        let status = cmd.status()?;
        std::process::exit(status.code().unwrap_or(0));
    } else if resp.status() == reqwest::StatusCode::NOT_FOUND {
        eprintln!("Session not found: {}", id);
    } else {
        eprintln!("Failed to attach: {}", resp.status());
    }

    Ok(())
}

// Purpose: `om session kill <id>` — DELETE the session and report success
// or a not-found / generic failure message.
async fn handle_session_kill(
    args: &[String],
    base_url: &str,
    client: &reqwest::Client,
) -> Result<()> {
    let id = args
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("Usage: om session kill <session-id>"))?;

    let resp = client
        .delete(format!("{}/api/ctrl/sessions/{}", base_url, id))
        .send()
        .await?;

    if resp.status().is_success() {
        println!("Session {} terminated.", id);
    } else if resp.status() == reqwest::StatusCode::NOT_FOUND {
        eprintln!("Session not found: {}", id);
    } else {
        eprintln!("Failed to terminate session: {}", resp.status());
    }

    Ok(())
}

// Purpose: `om session run` (#408) — supervised workflow executor with retry.
// The other subcommands are interactive lifecycle helpers; `run` is the
// only one that drives a task to completion in one shot, returning a
// structured Success / Blocked outcome. Parses --project / --task /
// --agent / --max-attempts / --name, delegates to `CtrlSupervisor`, and
// prints the outcome, exiting with the appropriate code on Blocked.
async fn handle_session_run(args: &[String], port: u16) -> Result<()> {
    let project = extract_flag(args, "--project")
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        })
        .ok_or_else(|| anyhow::anyhow!("--project is required (or run from a project dir)"))?;
    let task = extract_flag(args, "--task").ok_or_else(|| anyhow::anyhow!("--task is required"))?;
    let agent = extract_flag(args, "--agent").unwrap_or_else(|| "pm".to_string());
    let max_attempts: u32 = extract_flag(args, "--max-attempts")
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let name = extract_flag(args, "--name");

    let supervisor = ctrl::CtrlSupervisor::new(
        std::path::PathBuf::from(&project),
        task.clone(),
        agent,
        max_attempts,
        name,
        port,
    );

    match supervisor.run().await? {
        ctrl::SupervisorOutcome::Success {
            summary,
            session_id,
            attempts,
        } => {
            println!("✓ Task completed (attempt {}/{})", attempts, max_attempts);
            println!("  Session: {}", session_id);
            println!();
            println!("{}", summary);
        }
        ctrl::SupervisorOutcome::SuccessWithCaveats {
            summary,
            caveats,
            session_id,
            attempts,
        } => {
            println!(
                "✓ Task completed (attempt {}/{}) — with pre-existing test failures",
                attempts, max_attempts
            );
            println!("  Session: {}", session_id);
            println!();
            println!("{}", summary);
            if !caveats.is_empty() {
                println!();
                println!(
                    "Note: QA found {} pre-existing failure(s) unrelated to this task:",
                    caveats.len()
                );
                for c in &caveats {
                    println!("  - {}", c);
                }
                println!();
                println!("These are out of scope and were not introduced by this run.");
            }
        }
        ctrl::SupervisorOutcome::Blocked {
            reason,
            attempts,
            session_id,
        } => {
            eprintln!("✗ Blocked after {} attempt(s)", attempts);
            eprintln!("  Session: {} (status: blocked)", session_id);
            eprintln!();
            eprintln!("Reason: {}", reason);
            eprintln!();
            eprintln!(
                "To retry manually: om session run --project {} --task \"{}\"",
                project, task
            );
            std::process::exit(1);
        }
    }

    Ok(())
}

// Purpose: Default-arm usage block for `om session`.
fn print_session_usage() {
    println!("Usage: om session <new|list|attach|kill|run> [options]");
    println!();
    println!("Commands:");
    println!("  new    --project <path> --name <name> [--agent <agent>] [--worktree]");
    println!("  list   [<project-path>]");
    println!("  attach <session-id>");
    println!("  kill   <session-id>");
    println!("  run    --project <path> --task <text> [--agent <agent>]");
    println!("         [--max-attempts <n>] [--name <name>]");
}

/// Find `--flag <value>` pair in argv slice.
///
/// Why: The session subcommand has half a dozen optional flags; a tiny helper
/// keeps the dispatcher readable.
/// What: Returns the value following the first occurrence of `flag`.
/// Test: Indirectly via session subcommand smoke tests.
fn extract_flag(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

/// Parse `--port <N>` from argv slice.
///
/// Why: Same as `extract_flag` but typed as `u16`.
/// What: Reads and parses; returns `None` on either missing or unparsable.
/// Test: Covered indirectly by session subcommand tests.
fn extract_port_flag(args: &[String]) -> Option<u16> {
    extract_flag(args, "--port").and_then(|p| p.parse().ok())
}

/// Handle `open-mpm skills <subcommand>` (#168).
///
/// Why: Gives operators visibility into which skills were discovered, from
/// where, and lets them verify tag-based lookup before delegating. Without
/// this, the registry is invisible.
/// What: Supports `skills list [--tag <tag>]`. Without `--tag`, prints every
/// discovered skill with source label + tags. With `--tag <tag>` (repeatable),
/// filters + ranks by tag-overlap score.
/// Test: Covered manually; unit-tested via `SkillRegistry::find_by_tags`.
async fn run_skills_subcommand(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "list" => {
            // Collect zero-or-more `--tag <val>` pairs.
            let mut tags: Vec<String> = Vec::new();
            let mut i = 1; // args[0] == "list"
            while i < args.len() {
                if args[i] == "--tag" {
                    if let Some(v) = args.get(i + 1) {
                        tags.push(v.clone());
                        i += 2;
                        continue;
                    } else {
                        bail!("--tag requires a value");
                    }
                }
                i += 1;
            }

            let reg = skills::registry::SkillRegistry::load(&skills::registry::skill_search_paths(
                &default_bundled_config_dir(),
            ));
            let items: Vec<&skills::registry::SkillMeta> = if tags.is_empty() {
                reg.list()
            } else {
                let refs: Vec<&str> = tags.iter().map(String::as_str).collect();
                reg.find_by_tags(&refs)
            };

            if tags.is_empty() {
                println!("Discovered skills ({}):", items.len());
            } else {
                println!(
                    "Skills matching tags [{}] ({}):",
                    tags.join(","),
                    items.len()
                );
            }

            let bundled = default_bundled_config_dir().join("skills");
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let max_name = items.iter().map(|s| s.name.len()).max().unwrap_or(0).max(4);
            let max_src = items
                .iter()
                .map(|s| classify_skill_source(&s.source_path, &bundled, home.as_deref()).len())
                .max()
                .unwrap_or(0)
                .max(8);
            for s in items {
                let source_label = classify_skill_source(&s.source_path, &bundled, home.as_deref());
                let score_prefix = if tags.is_empty() {
                    String::new()
                } else {
                    let refs: Vec<&str> = tags.iter().map(String::as_str).collect();
                    format!("score={}  ", reg.tag_overlap_score(&s.name, &refs))
                };
                println!(
                    "  {name:<nw$}  [{src:<sw$}]  {score}tags: {tags}",
                    name = s.name,
                    nw = max_name,
                    src = source_label,
                    sw = max_src,
                    score = score_prefix,
                    tags = s.tags.join(","),
                );
            }
            Ok(())
        }
        "sources" => run_skills_sources_subcommand().await,
        other => {
            // #366: Surface a "did you mean?" hint for typos like
            // `skills sourcs` -> `skills sources`.
            let known = &["list", "sources"];
            if let Some(s) = cli::did_you_mean(other, known, 2) {
                eprintln!("open-mpm skills: unknown subcommand '{other}'. Did you mean '{s}'?");
            } else {
                eprintln!(
                    "open-mpm skills: unknown subcommand '{other}'. Try: list [--tag <tag>] | sources"
                );
            }
            bail!("unknown skills subcommand: {other}");
        }
    }
}

/// Print configured skill sources (`open-mpm skills sources`) (#172).
///
/// Why: Operators editing `.open-mpm/skill-sources.toml` need to confirm which
/// sources the harness actually loaded, whether each is enabled, and how many
/// skills each contributed. Otherwise misconfiguration is silent.
/// What: Loads the source registry, scans each path, and prints a one-line
/// summary per source: priority, type, identifier, enabled flag, skill count.
/// Test: Smoke-tested by invoking `cargo run -- skills sources`; correctness
/// of the underlying machinery is covered by `SkillSourceRegistry` unit tests.
async fn run_skills_sources_subcommand() -> Result<()> {
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let source_registry = skills::sources::SkillSourceRegistry::load(&project_root);
    let sources = source_registry.sources();
    let resolved = source_registry.resolved_paths();

    println!("Sources ({}):", sources.len());

    // Track the resolved-path index so we can map enabled sources to their
    // computed dir and count skills there.
    let mut resolved_iter = resolved.iter();
    for source in sources {
        let type_label = match source.source_type {
            skills::sources::SkillSourceType::Local => "local",
            skills::sources::SkillSourceType::RemoteGit => "remote",
        };
        let identifier = match source.source_type {
            skills::sources::SkillSourceType::Local => {
                source.path.clone().unwrap_or_else(|| "<unset>".to_string())
            }
            skills::sources::SkillSourceType::RemoteGit => source
                .name
                .clone()
                .or_else(|| source.url.clone())
                .unwrap_or_else(|| "<unnamed>".to_string()),
        };
        let enabled_label = if source.enabled {
            "enabled"
        } else {
            "disabled"
        };
        let approval_label = if source.approved { "" } else { " (unapproved)" };

        let skill_count = if source.enabled {
            // Pull the matching resolved path off the iterator and count `.md`
            // files there.
            resolved_iter
                .next()
                .map(|p| count_skill_files(p))
                .unwrap_or(0)
        } else {
            0
        };

        println!(
            "  [{prio:>2}] {type_label:<7} {ident:<32} {enabled_label} {count} skills{approval}",
            prio = source.priority,
            type_label = type_label,
            ident = identifier,
            enabled_label = enabled_label,
            count = skill_count,
            approval = approval_label,
        );
    }
    Ok(())
}

/// Count `.md` files reachable under `dir` recursively (zero when missing).
fn count_skill_files(dir: &Path) -> usize {
    if !dir.is_dir() {
        return 0;
    }
    let mut count = 0;
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            count += count_skill_files(&path);
        } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
            count += 1;
        }
    }
    count
}

/// Label a skill source path against the known hierarchy (#168).
///
/// Why: Users care which layer produced a skill, not its absolute path. We
/// only match on the *prefix* (not parent equality like the agents helper)
/// because bundled skills live in nested subdirs (e.g.
/// `.open-mpm/skills/frameworks/fastapi.md`).
fn classify_skill_source(source: &Path, bundled: &Path, home: Option<&Path>) -> String {
    if source.starts_with(bundled) {
        return "bundled".to_string();
    }
    if source.starts_with(".open-mpm/skills") {
        return ".open-mpm/skills".to_string();
    }
    if source.starts_with(".claude/skills") {
        return ".claude/skills".to_string();
    }
    if let Some(home) = home {
        if source.starts_with(home.join(".open-mpm/skills")) {
            return "~/.open-mpm/skills".to_string();
        }
        if source.starts_with(home.join(".claude/skills")) {
            return "~/.claude/skills".to_string();
        }
    }
    source.display().to_string()
}

/// Turn an absolute source path into a short label for `agents list` output.
///
/// Why: Users care which layer of the search path an agent came from, not
/// the full absolute path. Mapping known dirs to labels keeps output tidy.
/// What: Returns `bundled`, `.open-mpm/agents`, `.claude/agents`,
/// `~/.open-mpm/agents`, `~/.claude/agents`, or the full path as fallback.
fn classify_source(source: &Path, bundled: &Path, home: Option<&Path>) -> String {
    let parent = source.parent();
    if let Some(parent) = parent {
        if parent == bundled {
            return "bundled".to_string();
        }
        if parent == Path::new(".open-mpm/agents") {
            return ".open-mpm/agents".to_string();
        }
        if parent == Path::new(".claude/agents") {
            return ".claude/agents".to_string();
        }
        if let Some(home) = home {
            if parent == home.join(".open-mpm/agents") {
                return "~/.open-mpm/agents".to_string();
            }
            if parent == home.join(".claude/agents") {
                return "~/.claude/agents".to_string();
            }
        }
    }
    source.display().to_string()
}

/// Default on-disk code store directory (`$CWD/.open-mpm/state/code/`).
///
/// Why: Mirrors `cli::search_cmd::default_code_dir` so `--reindex`/`--watch`
/// write to the same location that `code search` reads from.
fn default_code_dir() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("failed to read cwd")?;
    Ok(cwd.join(".open-mpm").join("state").join("code"))
}

/// Default source extensions the watcher/reindex track.
///
/// Why: The indexer supports more extensions than callers typically want to
/// watch; exposing a curated default keeps `--reindex`/`--watch` usable
/// without extra flags.
fn default_extensions() -> Vec<String> {
    ["rs", "py", "ts", "tsx", "js", "jsx", "go", "md"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Read `[search] cool_after_minutes` from `<root>/.open-mpm/config.toml`.
///
/// Why: #372 — operators need a knob to override the 15-minute cool-down
/// default without recompiling. Stays best-effort: a missing/malformed file
/// silently falls back to [`search::indexer::DEFAULT_COOL_AFTER_MINUTES`].
/// What: Parses just the `[search]` table; any other top-level fields are
/// ignored so this loader composes with the rest of `config.toml`.
/// Test: Indirect — exercised via the `cool_after_minutes` config knob; a
/// missing file path is the common case in CI.
fn load_search_cool_after(project_root: &Path) -> std::time::Duration {
    #[derive(serde::Deserialize, Default)]
    struct SearchSection {
        cool_after_minutes: Option<u64>,
    }
    #[derive(serde::Deserialize, Default)]
    struct Wrapper {
        #[serde(default)]
        search: Option<SearchSection>,
    }
    let path = project_root.join(".open-mpm").join("config.toml");
    let minutes = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| toml::from_str::<Wrapper>(&s).ok())
        .and_then(|w| w.search.and_then(|s| s.cool_after_minutes))
        .unwrap_or(search::indexer::DEFAULT_COOL_AFTER_MINUTES);
    std::time::Duration::from_secs(minutes.saturating_mul(60))
}

/// Construct a `FileWatcher` rooted at the current working directory and
/// backed by the on-disk redb+usearch store.
///
/// Why: Both `--reindex` and `--watch` need identical setup; factoring it
/// keeps the CLI handlers terse and consistent.
/// What: Resolves the store dir, opens `RedbUsearchStore`, constructs a
/// `FastEmbedder`, wraps both in a `CodeIndexer`, returns a `FileWatcher`
/// with the default extensions. Honors `[search] cool_after_minutes` from
/// `.open-mpm/config.toml` for the cool-down threshold (#372).
async fn build_file_watcher() -> Result<FileWatcher> {
    const EMBED_DIM: usize = 384;
    let root = std::env::current_dir().context("failed to read cwd")?;
    let code_dir = default_code_dir()?;
    std::fs::create_dir_all(&code_dir)
        .with_context(|| format!("failed to create code dir: {}", code_dir.display()))?;
    let store = CodeStore::open(&code_dir, EMBED_DIM).context("failed to open CodeStore")?;
    let embedder = FastEmbedder::new().context("failed to construct FastEmbedder")?;
    let cool_after = load_search_cool_after(&root);
    let indexer =
        Arc::new(CodeIndexer::new(Arc::new(store), Arc::new(embedder)).with_cool_after(cool_after));
    Ok(FileWatcher::new(indexer, root, default_extensions()))
}

/// One-shot full re-index of the working tree, then exit.
///
/// Why: Seeds (or refreshes) the code index without waiting for filesystem
/// events — useful after pulling large changes or on first setup.
/// What: Builds a `FileWatcher`, calls `reindex_all`, prints the count.
/// Test: Manual: `cargo run -- --reindex`.
async fn run_reindex() -> Result<()> {
    let watcher = build_file_watcher().await?;
    let n = watcher.reindex_all().await?;
    println!("Indexed {n} chunks.");
    Ok(())
}

/// List tracked sub-agent PIDs and their liveness status.
///
/// Why: #130 — operators need a quick way to inspect `.open-mpm/state/processes.json`
/// and distinguish running, completed, and orphaned entries without parsing
/// JSON by hand.
/// What: Reads the tracker file for the current project, walks every entry,
/// and prints `pid  status  alive?  agent  task` to stdout. Marks entries as
/// `ORPHAN` when `status=Running` but the PID is no longer alive.
/// Test: `cargo run -- --check-orphans` in a project with an empty tracker
/// prints "No tracked sub-agent processes." and exits 0.
async fn run_check_orphans() -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read cwd")?;
    let open_mpm_dir = cwd.join(".open-mpm").join("state");
    let tracker = process_tracker::ProcessTracker::new(&open_mpm_dir);
    let entries = tracker.load().await?;

    if entries.is_empty() {
        println!("No tracked sub-agent processes.");
        return Ok(());
    }

    println!(
        "{:<8} {:<10} {:<8} {:<24} TASK",
        "PID", "STATUS", "ALIVE", "AGENT"
    );
    let mut sorted: Vec<_> = entries.values().collect();
    sorted.sort_by_key(|e| e.pid);
    for e in sorted {
        let alive = std::process::Command::new("kill")
            .args(["-0", &e.pid.to_string()])
            .stderr(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let status = format!("{:?}", e.status).to_lowercase();
        let alive_str = if alive { "yes" } else { "no" };
        let tag = if matches!(e.status, process_tracker::ProcessStatus::Running) && !alive {
            " (ORPHAN)"
        } else {
            ""
        };
        println!(
            "{:<8} {:<10} {:<8} {:<24} {}{}",
            e.pid, status, alive_str, e.agent_name, e.task_id, tag
        );
    }
    Ok(())
}

/// Spawn a background `FileWatcher` so the code index stays fresh during
/// normal interactive use, without the user having to remember `--watch`.
///
/// Why: Hybrid `search_code` is only useful when the index reflects the
/// working tree. Auto-watching (issue #372) closes the "did you reindex?"
/// gap that would otherwise hit every developer who edits files between
/// queries. We do this as a fire-and-forget tokio task so failures (no
/// permissions, embedder unavailable, redb lock contention) only emit a
/// warning — never abort startup.
/// What: Builds a `FileWatcher` exactly as `--watch` would, then spawns its
/// `watch()` future on the tokio runtime. Returns immediately. Skips the
/// initial reindex to avoid blocking startup; the existing index (if any)
/// is reused, and on-disk changes since last run will be picked up
/// incrementally as events arrive.
/// Test: Indirect — exercised end-to-end in any interactive run; the
/// helper itself is a thin wrapper around `build_file_watcher` + spawn.
fn spawn_background_file_watcher() {
    tokio::spawn(async {
        // #374: If the search daemon is already running for this project,
        // it owns the redb code-store lock — we'd just deadlock trying to
        // open the same store. Skip the local watcher entirely; tools
        // route their queries through the daemon via SearchDaemonClient.
        let project_root =
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        if search::service::is_daemon_running(&project_root).await {
            tracing::info!(
                "search daemon detected — skipping in-process file watcher (queries will route to daemon)"
            );
            return;
        }
        match build_file_watcher().await {
            Ok(watcher) => {
                // #372 warm-start: load the persisted HNSW into RAM before
                // any user query lands. The on-disk file already exists at
                // .open-mpm/state/code/code.usearch (created on prior runs);
                // this just ensures it's resident so the first search isn't
                // gated on a load. We log warm-up errors but continue —
                // searches will lazily warm on first use.
                let indexer = watcher.indexer();
                if let Err(e) = indexer.warm_up().await {
                    tracing::warn!(error = %e, "code-index warm-up failed; will warm lazily on first search");
                } else {
                    tracing::info!("code-index warmed at PM startup");
                }
                // #372 cool-down: evict the in-memory HNSW after N minutes
                // of no searches so an idle PM doesn't pin RAM. The file
                // watcher keeps running through cool-down — only the
                // in-memory vector index is dropped.
                let _cool = indexer.spawn_cool_down_monitor();
                tracing::info!("background file watcher started (auto-indexing on changes)");
                if let Err(e) = watcher.watch().await {
                    tracing::warn!(error = %e, "background file watcher exited with error");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not start background file watcher (continuing without auto-index)");
            }
        }
    });
}

/// Watch the working tree forever and keep the index in sync.
///
/// Why: Developer-facing incremental indexing. Cheaper than `--reindex`
/// for each edit and keeps search fresh without user intervention.
/// What: Builds a `FileWatcher` and calls `watch()`; blocks until the
/// process is killed.
/// Test: Manual: `cargo run -- --watch`.
async fn run_watch() -> Result<()> {
    let watcher = build_file_watcher().await?;
    // Seed the index first so the initial search state isn't empty.
    let _ = watcher.reindex_all().await;
    watcher.watch().await
}

/// Workflow mode: load a prescriptive workflow and iterate its phases.
///
/// Why: For bake-off tasks, a fixed pipeline (research -> plan -> code ->
/// QA -> observe) produces more reliable results than dynamic PM delegation.
/// What: Reads task text from `--task-file` (or stdin), constructs a
/// `WorkflowEngine` wired to `SubprocessAgentRunner`, runs the named
/// workflow, handles code-phase file extraction, and prints the final
/// observe report.
/// Test: `open-mpm --workflow prescriptive --task-file t.md --out-dir /tmp/x`
/// loads `.open-mpm/workflows/prescriptive.json` and runs each phase.
async fn run_workflow(
    name: &str,
    task_file: Option<&str>,
    inline_task: Option<&str>,
    out_dir: Option<&str>,
    project_dir: Option<&str>,
    json_output: bool,
) -> Result<()> {
    // #218: Pre-flight check — emit a clear, actionable error when the
    // project's `.open-mpm/agents/` directory is missing instead of letting
    // the first sub-agent spawn panic with a cryptic "failed to load agent
    // config" failure. Workflow JSON file existence is checked downstream
    // by `WorkflowDef::load`, which already produces a clear error.
    let cwd_for_check = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let agents_dir_check = cwd_for_check.join(".open-mpm").join("agents");
    if !agents_dir_check.exists() {
        bail!(
            "no `.open-mpm/agents/` found in {}.\n\n\
             open-mpm needs an agent config directory in the current project.\n\
             To bootstrap a new project, copy bundled defaults from your \
             open-mpm install:\n\n  \
               mkdir -p .open-mpm\n  \
               cp -r <open-mpm-source>/.open-mpm/agents .open-mpm/\n  \
               cp -r <open-mpm-source>/.open-mpm/workflows .open-mpm/\n  \
               cp -r <open-mpm-source>/.open-mpm/skills .open-mpm/  # optional\n\n\
             Also ensure `.env.local` (or the env) contains `OPENROUTER_API_KEY=...`.\n\
             A future `open-mpm init` subcommand will automate this; \
             see GitHub issue #218.",
            cwd_for_check.display()
        );
    }
    let workflows_dir_check = cwd_for_check.join(".open-mpm").join("workflows");
    if !workflows_dir_check.exists() {
        bail!(
            "no `.open-mpm/workflows/` found in {}.\n\n\
             Copy bundled workflow definitions from your open-mpm install:\n\n  \
               cp -r <open-mpm-source>/.open-mpm/workflows .open-mpm/\n\n\
             See GitHub issue #218.",
            cwd_for_check.display()
        );
    }

    let task = read_task_text_with_inline(task_file, inline_task).await?;
    if task.is_empty() {
        bail!("empty task");
    }

    // #410: Capture the user's project directory at invocation time, BEFORE
    // any directory derivation/canonicalization runs. This is the directory
    // the harness was launched from, which is the user's actual project
    // source tree by default. Used downstream to (a) seed the default for
    // `--project-dir` when the flag was omitted, and (b) populate
    // `OPEN_MPM_PROJECT_DIR` for every spawned agent.
    let invocation_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Default `--project-dir` to the invocation CWD when the flag was not
    // supplied. Pre-#410 behavior left `project_dir` as `None`, which caused
    // agents to run with `CWD = out_dir` and break source-file lookup. With
    // this default, agents always see the user's project tree as their CWD,
    // and the artifacts dir (`--out-dir`) is used only for workflow output
    // files (assignments.json, workflow-report.md, etc.).
    let invocation_cwd_str = invocation_cwd.to_string_lossy().to_string();
    let project_dir: Option<&str> = match project_dir {
        Some(p) => Some(p),
        None => Some(invocation_cwd_str.as_str()),
    };

    // #222 / #410: Full separation of "artifacts dir" (`--out-dir`) and
    // "code dir" (`--project-dir`). After #410, `--project-dir` defaults to
    // the invocation CWD when omitted, so the typical case is now:
    //   - both set explicitly → out_dir = artifacts, code_dir = project
    //   - only --project-dir set → both = project (legacy #220 behavior)
    //   - only --out-dir set → out_dir = artifacts, code_dir = invocation
    //     CWD (#410: agents now see project source, not artifacts)
    //   - neither → out_dir = auto-generated, code_dir = invocation CWD
    let (artifacts_input, code_input): (Option<&str>, Option<&str>) = match (out_dir, project_dir) {
        (Some(o), Some(p)) => {
            tracing::info!(
                out_dir = %o,
                project_dir = %p,
                "#222: separated artifacts dir (--out-dir) and code dir (--project-dir)"
            );
            (Some(o), Some(p))
        }
        (None, Some(p)) => {
            // #220 legacy: --project-dir alone overrides both.
            (Some(p), Some(p))
        }
        (Some(o), None) => (Some(o), None),
        (None, None) => (None, None),
    };

    // Auto-generate out_dir when not specified. Naming convention:
    // `out/<label>-v<version>-<YYYYMMDD>-<HHMMSS>` where:
    //   - label = shortened task-file stem (level-2.txt → l2) or workflow name
    //   - version = Cargo package version with dots stripped (0.1.17 → v0117)
    //   - timestamp = UTC wall clock at run start
    let out_dir = artifacts_input;
    let out_dir_buf: Option<PathBuf> = match out_dir {
        Some(d) => Some(PathBuf::from(d)),
        None => {
            let label = task_file
                .and_then(|f| std::path::Path::new(f).file_stem())
                .and_then(|s| s.to_str())
                .map(|stem| {
                    // "level-2" → "l2", "level-3" → "l3", else use stem as-is
                    if let Some(rest) = stem.strip_prefix("level-") {
                        format!("l{rest}")
                    } else {
                        stem.to_string()
                    }
                })
                .unwrap_or_else(|| name.to_string());
            let version = env!("CARGO_PKG_VERSION").replace('.', "");
            let now = chrono::Utc::now();
            let ts = now.format("%Y%m%d-%H%M%S").to_string();
            let dir_name = format!("{label}-v{version}-{ts}");
            let out_path = std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("out")
                .join(&dir_name);
            tracing::info!(
                out_dir = %out_path.display(),
                "no --out-dir provided; auto-generated output directory"
            );
            Some(out_path)
        }
    };

    // #222: Resolve the code dir. When `--project-dir` was supplied, we use
    // that (independent of out_dir). Otherwise, fall back to out_dir so
    // generated code and artifacts share a single directory (legacy mode).
    let code_dir_buf: Option<PathBuf> = match code_input {
        Some(p) => Some(PathBuf::from(p)),
        None => out_dir_buf.clone(),
    };
    if let (Some(o), Some(c)) = (out_dir_buf.as_deref(), code_dir_buf.as_deref())
        && o != c
    {
        tracing::info!(
            out_dir = %o.display(),
            code_dir = %c.display(),
            "#222: artifacts dir and code dir are distinct"
        );
    }

    // #47: Read the already-incremented build counter without re-incrementing,
    // so perf records match the startup banner. Falls back to 0 on read failure.
    let build_num = read_current_build_number().await.unwrap_or(0);

    // #60: Build the runner. Scan the workflow's agents for any that opt
    // into `runner = "claude-code"`; if so, construct a ClaudeCodeAgentRunner,
    // verify auth at startup (fail fast with a clear message), and wrap
    // everything in the dispatcher. Otherwise use the subprocess runner
    // directly so we don't pay the path-lookup cost on every run.
    //
    // Capture the project root (CWD at startup, which is still the
    // project root here — `out_dir_buf` is only used as child CWD later)
    // and forward an absolute agents config dir to child processes via
    // `OPEN_MPM_CONFIG_DIR`. Without this, sub-agents spawned with
    // `current_dir(out_dir)` would try to load `.open-mpm/agents/<name>.toml`
    // relative to `out_dir` — which doesn't exist — and every phase fails
    // with "failed to load agent config for '<name>': No such file".
    // Mirrors the pattern in `ctrl::run_pm_task` (src/ctrl/mod.rs ~line 205).
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let agents_config_dir = project_root.join(".open-mpm").join("agents");
    // #410: Forward the resolved project_dir (== code_dir when separated, or
    // the invocation CWD when only --out-dir was supplied) so every spawned
    // sub-agent sees `OPEN_MPM_PROJECT_DIR` and runs with CWD anchored at
    // the user's source tree rather than the artifacts directory.
    let subprocess_runner: Arc<dyn tools::AgentRunner> = Arc::new(
        SubprocessAgentRunner::new()
            .with_config_dir(Some(agents_config_dir))
            .with_out_dir(out_dir_buf.clone())
            .with_code_dir(code_dir_buf.clone())
            .with_project_dir(code_dir_buf.clone()),
    );
    let runner = build_runner_for_workflow(name, subprocess_runner.clone()).await?;
    let perf_dir: PathBuf = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("docs")
        .join("performance");

    // #69/#70/#72: context + memory management subsystem.
    // All three are optional/gracefully degrading — if OPENROUTER_API_KEY is
    // absent, the indexer drops turns with a debug log and the cleaner skips
    // any LLM-backed steps. We spawn them unconditionally so the wiring is
    // testable and the store dir is created eagerly.
    let store_dir = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".open-mpm")
        .join("state")
        .join("history");
    let api_key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
    let indexer = context::HistoryIndexer::spawn(store_dir.clone(), api_key.clone());
    let cleaner = context::cleaner::MemoryCleaner::spawn(store_dir.clone(), api_key.clone(), 20);

    // #81/#115: Scan .open-mpm/skills/ plus global paths and share the merged
    // registry across the workflow so each phase gets the most relevant skill
    // bodies as a prompt prefix.
    let skills_dir = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".open-mpm")
        .join("skills");
    let cwd_for_skills = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // #115: Refresh the global skills cache so newly-added skills from
    // ~/.open-mpm/skills/files/ and ~/Projects/skillset-mcp are indexed.
    // Fire-and-forget: failures are logged but never block the workflow.
    match skills::global_cache::GlobalSkillsCache::new() {
        Ok(cache) => {
            if let Err(e) = cache.refresh(&cwd_for_skills).await {
                tracing::warn!(error = %e, "global skills cache refresh failed (continuing)");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "global skills cache init failed (continuing)");
        }
    }

    // #115: Load project-local skills merged with global discovery paths.
    // #128: Also merge claude-mpm skills from `.claude/skills/` (project) and
    // `~/.claude/skills/` (user) so users can drop claude-mpm content directly.
    let skill_registry = Arc::new({
        let mut registry = skills::SkillRegistry::load_with_global_cache(&cwd_for_skills)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to load skill registry; using empty");
                skills::SkillRegistry::empty()
            });
        // Project-level claude-mpm first (higher priority after existing set).
        registry
            .load_additional_dir(&cwd_for_skills.join(".claude").join("skills"))
            .await;
        // User-level claude-mpm as a lower-priority fallback.
        if let Some(home) = dirs::home_dir() {
            registry
                .load_additional_dir(&home.join(".claude").join("skills"))
                .await;
        }
        registry
    });

    // #128: Discover claude-mpm agents for dynamic loading. This populates
    // diagnostics only; the actual fallback happens inside
    // `AgentConfig::by_name_async` when a TOML is not found.
    let _claude_mpm_agents = agents::claude_mpm_loader::discover_agents(&cwd_for_skills)
        .await
        .unwrap_or_default();
    tracing::info!(
        count = _claude_mpm_agents.len(),
        "discovered claude-mpm agents"
    );

    // #108/#109: Project self-initialization + memory seeding. Runs
    // once per project per day (marker TTL) and is otherwise a no-op. The
    // produced `InitContext` is injected as a prefix into every agent phase.
    let init_ctx = {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let omd = cwd.join(".open-mpm").join("state");
        let initializer = init::ProjectInitializer::new(cwd, omd);
        let reinit = std::env::args().any(|a| a == "--reinit");
        let result = if reinit {
            initializer.force_reinitialize().await
        } else {
            initializer.initialize_if_needed().await
        };
        match result {
            Ok(ctx) => {
                tracing::info!(
                    memories = ctx.relevant_memories.len(),
                    summary_chars = ctx.project_summary.len(),
                    "project self-initialization complete"
                );
                // Cross-machine memory share: if `.open-mpm/shared-memories.jsonl`
                // exists and its hash differs from the last-imported tracker,
                // import it now so teammate sessions become recallable via
                // `memory_recall scope=imported`. Best-effort.
                {
                    let cwd_share = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    match cli::memories_cmd::auto_import_if_changed(&cwd_share).await {
                        Ok(n) if n > 0 => {
                            eprintln!(
                                "[open-mpm] Imported {n} shared memories from .open-mpm/shared-memories.jsonl"
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(error = %e, "auto-import shared memories failed (continuing)");
                        }
                    }
                }

                // #190: Seed agent memory with project docs so workflow agents
                // can recall user/developer documentation via memory_recall.
                // Best-effort: failures (e.g., model download issues) are
                // logged but do not block workflow execution.
                let cwd_inner = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let omd_inner = cwd_inner.join(".open-mpm").join("state");
                let session_dir = cwd_inner.join(".open-mpm").join("sessions").join("default");
                if let Err(e) = std::fs::create_dir_all(&session_dir) {
                    tracing::warn!(error = %e, "doc seed: create session dir failed");
                } else {
                    match memory::open_memory_store(&session_dir) {
                        Ok(store) => match memory::FastEmbedder::new() {
                            Ok(embedder) => {
                                let initializer =
                                    init::ProjectInitializer::new(cwd_inner, omd_inner);
                                // #190+: seed docs + skills + MCP connections in one call.
                                // seed_all() emits its own combined log line and
                                // never fails — individual stages log warnings on
                                // failure but don't abort.
                                let _ = initializer.seed_all(store.as_ref(), &embedder).await;
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "doc seed: embedder unavailable");
                            }
                        },
                        Err(e) => {
                            tracing::warn!(error = %e, "doc seed: store open failed");
                        }
                    }
                }
                Some(ctx)
            }
            Err(e) => {
                tracing::warn!(error = %e, "project self-initialization failed (continuing)");
                None
            }
        }
    };

    // INTENT: Construct a SkillsLoader so the engine auto-injects relevant
    // skill bodies (language + framework detection) into every phase's prompt.
    let skills_loader = Arc::new(skills::SkillsLoader::new(skills_dir.clone()));

    // #118: Open the user-scoped memory store and extract a prompt suffix.
    // Backed by an embedded redb + usearch store at ~/.open-mpm/memory/.
    // Injected at lower priority than project context so project-specific
    // knowledge always wins. Non-fatal on failure.
    let user_memory_suffix = match memory::user_store::UserMemoryStore::open().await {
        Ok(store) => {
            let suffix = store.to_prompt_suffix();
            tracing::debug!(suffix_chars = suffix.len(), "user memory store opened");
            if suffix.is_empty() {
                None
            } else {
                Some(suffix)
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "user memory store unavailable (continuing)");
            None
        }
    };

    // #173: Tag-indexed skill registry for pre-plan automatic skill discovery.
    // This is independent of the legacy `skill_registry` (which is the older
    // relevance-scored search index used for per-phase auto-injection). The
    // tag registry mirrors the PM startup load so workflow runs see the same
    // bundled+local skills.
    let tag_skill_registry = Arc::new({
        let mut reg = skills::registry::SkillRegistry::load(&skills::registry::skill_search_paths(
            &default_bundled_config_dir(),
        ));
        let index_path = skills::registry::skill_index_path();
        if let Err(e) = reg.merge_index(&index_path) {
            tracing::warn!(
                error = %e,
                path = %index_path.display(),
                "tag skill registry: failed to merge persisted effectiveness index (continuing with defaults)"
            );
        }
        reg
    });

    let mut engine = WorkflowEngine::new(runner, PathBuf::from(".open-mpm/workflows"))
        .with_build(build_num)
        .with_perf_dir(Some(perf_dir))
        .with_indexer(Some(indexer))
        .with_skill_registry(Some(skill_registry))
        .with_skills_loader(Some(skills_loader))
        .with_init_context(init_ctx)
        .with_user_memory(user_memory_suffix)
        .with_tag_skill_registry(Some(tag_skill_registry))
        .with_progress(Some(Arc::new(progress::ProgressReporter::new())));

    // #84: If the workflow JSON declares `ticket_management` with
    // `enabled=true`, attach a TicketManager so the engine creates and closes
    // a GitHub tracking issue around the run. We peek at the config file
    // before calling `engine.run` so we can keep the manager optional.
    {
        let wf_path = if name.ends_with(".json") || name.contains('/') {
            PathBuf::from(name)
        } else {
            PathBuf::from(".open-mpm/workflows").join(format!("{name}.json"))
        };
        if let Ok(def) = workflow::WorkflowDef::load(&wf_path)
            && let Some(tm_cfg) = def.ticket_management.clone()
            && tm_cfg.enabled
        {
            let tm = workflow::TicketManager::new(tm_cfg);
            engine = engine.with_ticket_manager(tm);
        }
    }

    let (ctx, perf_record) = engine
        .run_with_perf_and_dirs(name, task, out_dir_buf.clone(), code_dir_buf.clone())
        .await
        .context("workflow execution failed")?;

    // #72: Kick off a cleanup pass after each workflow run. Fire-and-forget.
    cleaner.trigger();

    // #171/#174: Persist updated effectiveness/usage to
    // ~/.open-mpm/skills/index.json so the next run's ranking benefits from
    // this run's signal. Prefer the structured `## Skill Ratings` block from
    // observe-agent when available; fall back to a coarse status-derived
    // signal otherwise. Non-fatal: a write failure here never masks a
    // successful workflow result.
    let observe_out_for_ratings = ctx.phase_outputs.get("observe").map(String::as_str);
    update_skill_usage_after_run(&perf_record, observe_out_for_ratings);

    // Record this run into the cross-project session log at
    // ~/.open-mpm/sessions/runs.jsonl so CTRL's `search_sessions` can grep
    // over history. Non-fatal: a write failure here never masks a successful
    // workflow result.
    {
        let project_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let observe_out = ctx
            .phase_outputs
            .get("observe")
            .cloned()
            .unwrap_or_default();
        let score = session_record::extract_score(&observe_out);
        let files_modified: Vec<String> = out_dir_buf
            .as_deref()
            .map(|dir| collect_modified_files(dir))
            .unwrap_or_default();
        let record = session_record::record_from_perf(
            &perf_record,
            &project_path,
            task_file,
            files_modified,
            score,
        );
        if let Err(e) = session_record::append_run_record(&record).await {
            tracing::warn!(error = %e, "failed to append session record");
        }

        // Also record a one-line summary interaction so InteractionLog
        // grep can answer "what did this run actually do?". Non-fatal:
        // failures here never mask a successful workflow result.
        let session_id = format!("build{}", perf_record.build);
        let summary = format!(
            "workflow={} status={} cost=${:.2} mins={} task={}",
            record.workflow, record.status, record.cost_usd, record.duration_mins, record.task,
        );
        let ilog = interaction_log::InteractionLog::new(&project_path, &session_id);
        if let Err(e) = ilog.append("pm", &summary, None).await {
            tracing::warn!(error = %e, "failed to append interaction summary");
        }

        // #186: If any mistakes were recorded for this session, fire off
        // the postmortem agent in the background so it doesn't block the
        // user-visible result. The OPEN_MPM_RUN_ID is the session id used
        // by the subprocess mistake recorder; we also try the build label
        // since interaction logs use that.
        let run_id = std::env::var("OPEN_MPM_RUN_ID").unwrap_or_default();
        let candidate_ids: Vec<String> = [run_id.clone(), session_id.clone()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        let mut mistake_count = 0usize;
        let mut hit_id: Option<String> = None;
        for sid in &candidate_ids {
            if let Ok(records) = mistake_log::MistakeLog::read_session(&project_path, sid)
                && !records.is_empty()
            {
                mistake_count = records.len();
                hit_id = Some(sid.clone());
                break;
            }
        }
        if let (count, Some(sid)) = (mistake_count, hit_id)
            && count > 0
        {
            eprintln!("\n⚠  {count} mistakes logged — running postmortem analysis...");
            // Fire-and-forget: spawn the postmortem in the background so we
            // never delay the main workflow result.
            let project_root = project_path.clone();
            tokio::spawn(async move {
                if let Err(e) = trigger_postmortem(&project_root, &sid).await {
                    tracing::warn!(error = %e, "postmortem agent dispatch failed");
                }
            });
        }
    }

    // #64: File extraction between phases is now handled INSIDE
    // `WorkflowEngine::run` for any phase with `produces_files: true`, so QA
    // can run against materialized files. This post-run extraction is kept as
    // a fallback for legacy workflow configs that do not yet set
    // `produces_files` on their code phase — re-running extraction is
    // idempotent (same bytes written to the same paths), so it is safe either
    // way. The `--direct` mode (which does not go through the engine) still
    // relies on `extract_files_to_dir` below for one-shot extraction.
    if let (Some(dir), Some(code_output)) = (out_dir_buf.as_deref(), ctx.phase_outputs.get("code"))
    {
        // MIN-1 (#99): Use the shared `extract_files_from_content` from `ipc`
        // instead of a duplicate `extract_files_to_dir`. Writing the files
        // here is the fallback path for legacy workflow configs whose code
        // phase does not set `produces_files: true`.
        let files = ipc::extract_files_from_content(code_output);
        for (filename, content) in files {
            let dest = dir.join(&filename);
            if let Some(parent) = dest.parent() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    tracing::warn!(path = %dest.display(), error = %e, "fallback extract: mkdir failed (non-fatal)");
                    continue;
                }
            }
            if let Err(e) = tokio::fs::write(&dest, &content).await {
                tracing::warn!(path = %dest.display(), error = %e, "fallback extract: write failed (non-fatal)");
            }
        }
    }

    // Determine the narrative. Preserves the pre-#151 behavior: observe
    // phase output wins, falling back to the last phase's output.
    let narrative = ctx
        .phase_outputs
        .get("observe")
        .cloned()
        .or_else(|| ctx.phase_outputs.values().last().cloned())
        .unwrap_or_default();

    if json_output {
        // #151 Phase 1: emit a full `PmResponse` JSON envelope instead of
        // the narrative-only output. Default (no `--json`) preserves the
        // historical stdout contract.
        let response = api::builder::build_from_workflow(
            &ctx,
            Some(&perf_record),
            narrative,
            Some(name),
            Vec::new(),
        );
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if !narrative.is_empty() {
        println!("{narrative}");
    }

    Ok(())
}

/// Build the `AgentRunner` used for a workflow run, wiring in a
/// `ClaudeCodeAgentRunner` when any phase's agent opts into it (#60).
///
/// Why: Most workflows only use subprocess agents, and the `claude` CLI
/// lookup + auth check adds latency; scanning the workflow lets us skip
/// both when nothing needs them. When an agent *does* require claude-code,
/// we fail fast with a clear message if the CLI is missing or unauthenticated.
/// What: Loads the workflow JSON to enumerate agent names, peeks at each
/// agent's TOML to find `runner = "claude-code"`. If none → return the
/// subprocess runner unchanged. Otherwise build `ClaudeCodeAgentRunner`,
/// run `check_auth`, and return a `DispatchingAgentRunner` that routes
/// per-agent.
/// Test: Manual — a workflow with no claude-code agents returns the
/// subprocess runner; one with a claude-code agent triggers auth check.
async fn build_runner_for_workflow(
    workflow_name: &str,
    fallback: Arc<dyn tools::AgentRunner>,
) -> Result<Arc<dyn tools::AgentRunner>> {
    let path = if workflow_name.ends_with(".json") || workflow_name.contains('/') {
        PathBuf::from(workflow_name)
    } else {
        PathBuf::from(".open-mpm/workflows").join(format!("{workflow_name}.json"))
    };

    // Soft failure: if we can't read the workflow yet, just return the
    // fallback and let the engine produce its own WorkflowNotFound error.
    let def = match workflow::WorkflowDef::load(&path) {
        Ok(d) => d,
        Err(_) => return Ok(fallback),
    };

    let needs_claude_code = def.phases.iter().any(|p| {
        AgentConfig::by_name(&p.agent)
            .map(|c| c.agent.runner == agents::RunnerKind::ClaudeCode)
            .unwrap_or(false)
    });
    // #198 / Phase C: scan for in-process agents so we can build the
    // shared `InProcessAgentRunner` once per workflow.
    let needs_in_process = def.phases.iter().any(|p| {
        AgentConfig::by_name(&p.agent)
            .map(|c| c.agent.runner == agents::RunnerKind::InProcess)
            .unwrap_or(false)
    });

    if !needs_claude_code && !needs_in_process {
        return Ok(fallback);
    }

    let cc_arc = if needs_claude_code {
        tracing::info!(
            workflow = %workflow_name,
            "workflow uses claude-code runner; resolving claude CLI and verifying auth"
        );
        let cc = ClaudeCodeAgentRunner::new()
            .await
            .context("failed to resolve `claude` CLI for claude-code runner")?;
        cc.check_auth()
            .await
            .context("claude CLI auth check failed")?;
        tracing::info!("claude-code runner: authenticated via Claude Max OAuth");
        Some(Arc::new(cc))
    } else {
        None
    };

    let in_process_arc: Option<Arc<dyn tools::AgentRunner>> = if needs_in_process {
        tracing::info!(
            workflow = %workflow_name,
            "workflow uses in-process runner; constructing shared LLM client"
        );
        let client = Arc::new(llm::create_client()?);
        let runner = agents::in_process_runner::InProcessAgentRunner::with_default_resolver(client);
        Some(Arc::new(runner))
    } else {
        None
    };

    Ok(Arc::new(
        DispatchingAgentRunner::new(fallback, cc_arc).with_in_process(in_process_arc),
    ))
}

/// Read the current build number from `.open-mpm/state/build.json` without bumping.
///
/// Why: (#47) `main()` already calls `BuildInfo::load_and_increment()` at
/// startup. Calling it again from the workflow path would double-count.
/// What: Parses `.open-mpm/state/build.json` and returns the `build` field.
/// Test: Integration — verified when the emitted perf record's `build`
/// matches the startup banner in manual runs.
/// Walk `out_dir` and return the list of files (relative to `out_dir`).
///
/// Why: The session record's `files_modified` field is most useful when it
/// lists the files the workflow actually produced, which for file-extracting
/// phases live under `out_dir`. Walking a tree is cheap and avoids relying
/// on ctx internals.
/// What: Recursively enumerates regular files under `out_dir`, returning
/// their paths relative to `out_dir` as strings. Silently returns an empty
/// list if `out_dir` does not exist or can't be read — this is best-effort.
/// Test: Covered indirectly; a run with files in out_dir produces non-empty
/// `files_modified` in `~/.open-mpm/sessions/runs.jsonl`.
fn collect_modified_files(out_dir: &Path) -> Vec<String> {
    fn walk(root: &Path, cur: &Path, acc: &mut Vec<String>) {
        let entries = match std::fs::read_dir(cur) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(ft) if ft.is_file() => {
                    if let Ok(rel) = path.strip_prefix(root) {
                        acc.push(rel.to_string_lossy().to_string());
                    }
                }
                Ok(ft) if ft.is_dir() => walk(root, &path, acc),
                _ => {}
            }
            // Cap to prevent runaway in huge trees.
            if acc.len() >= 200 {
                return;
            }
        }
    }
    let mut out = Vec::new();
    walk(out_dir, out_dir, &mut out);
    out.sort();
    out
}

async fn read_current_build_number() -> Result<u64> {
    #[derive(serde::Deserialize)]
    struct PersistedBuild {
        build: u64,
    }
    let path = std::env::current_dir()?
        .join(".open-mpm")
        .join("state")
        .join("build.json");
    let bytes = tokio::fs::read(&path).await?;
    let p: PersistedBuild = serde_json::from_slice(&bytes)?;
    Ok(p.build)
}

#[allow(dead_code)]
async fn read_task_text(task_file: Option<&str>) -> Result<String> {
    read_task_text_with_inline(task_file, None).await
}

/// Resolve task text from either an inline `--task <STRING>` argument,
/// a `--task-file <path>`, or stdin (in that priority order).
///
/// Why: #126 bug 1 — callers want to pass short task strings directly on
/// the command line without creating a temp file.
/// What: Checks `inline_task` first (highest precedence), then `task_file`,
/// then falls back to reading stdin. Trims the result.
/// Test: `cargo run -- --direct python-engineer --task "hello"` should route
/// `"hello"` to the agent without reading stdin.
async fn read_task_text_with_inline(
    task_file: Option<&str>,
    inline_task: Option<&str>,
) -> Result<String> {
    if let Some(text) = inline_task {
        return Ok(text.trim().to_string());
    }
    let task = if let Some(path) = task_file {
        tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read task file: {path}"))?
    } else {
        let mut s = String::new();
        tokio::io::stdin()
            .read_to_string(&mut s)
            .await
            .context("failed to read task from stdin")?;
        s
    };
    Ok(task.trim().to_string())
}

/// Direct mode: bypass the PM LLM and call a sub-agent with raw task text.
///
/// Why: The PM orchestration layer adds an extra LLM hop that is unnecessary
/// when the caller already knows which agent should handle the task. Direct
/// mode is useful for bake-off challenges and scripting, and lets us iterate
/// on sub-agent prompts without burning PM tokens.
/// What: Reads task text from `--task-file` (or stdin if absent), spawns the
/// named sub-agent subprocess via `spawn_subagent_and_run`, prints the result
/// content to stdout, and optionally extracts `## File: <path>` sections into
/// `--out-dir`.
/// Test: `echo "Write a Python hello world" | open-mpm --direct python-engineer`
/// should print a Python snippet.
async fn run_direct(
    agent_name: &str,
    task_file: Option<&str>,
    inline_task: Option<&str>,
    out_dir: Option<&str>,
) -> Result<()> {
    let task = read_task_text_with_inline(task_file, inline_task).await?;
    if task.is_empty() {
        bail!("empty task");
    }

    tracing::info!(agent = %agent_name, "direct mode: calling sub-agent");

    let result = spawn_subagent_and_run(agent_name, &task).await?;

    let content = match result {
        IpcMessage::Result { content, .. } => content,
        IpcMessage::Error { error, .. } => bail!("sub-agent error: {error}"),
        IpcMessage::Task { .. } => bail!("unexpected Task message from sub-agent"),
    };
    println!("{content}");

    if let Some(dir) = out_dir {
        // MIN-1 (#99): Use the shared `extract_files_from_content` in `ipc`
        // instead of a duplicate extractor.
        let base = std::path::Path::new(dir);
        let files = ipc::extract_files_from_content(&content);
        let count = files.len();
        for (filename, body) in files {
            let full_path = base.join(&filename);
            if let Some(parent) = full_path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("failed to create dir: {}", parent.display()))?;
            }
            let mut final_body = body;
            if !final_body.ends_with('\n') {
                final_body.push('\n');
            }
            tokio::fs::write(&full_path, final_body.as_bytes())
                .await
                .with_context(|| format!("failed to write file: {}", full_path.display()))?;
            eprintln!("Wrote: {}", full_path.display());
        }
        if count == 0 {
            tracing::warn!(
                "no `## File: <path>` sections detected in output; nothing written to {dir}"
            );
        } else {
            tracing::info!(count = count, dir = %dir, "extracted files");
        }
    }

    Ok(())
}

/// Observable run statistics for a single bake-off invocation (#348).
///
/// Why: `run_compare_bakeoff` needs a uniform record per side so the report
/// table is symmetric. Token counts are tracked best-effort (zero when the
/// runner does not surface them).
/// What: Output file paths + their byte sizes, plus elapsed wall-clock.
/// Test: Implicit via the `--compare` smoke run.
#[derive(Debug, Default, Clone)]
struct BakeoffRunStats {
    elapsed_ms: u128,
    output_files: Vec<PathBuf>,
    total_bytes: usize,
    syntax_valid: Option<bool>,
}

/// #348: Run a task twice (traditional + AST-native) and emit a comparison
/// report.
///
/// Why: Bake-off operators want a one-shot way to see whether the AST
/// substrate produces equivalent (or better) output than the traditional
/// write-file path. Re-running the same task with both modes side-by-side
/// keeps the comparison apples-to-apples.
/// What: Resolves the agent name (defaulting to `engineer`), runs `run_direct`
/// twice with separate `out_dir`s — once with the override off, once on —
/// then walks each directory for produced files and prints a markdown table.
/// The report is also written to `out/compare-report-<ts>.md`.
/// Test: Manual smoke (see `--compare` instructions in CLAUDE.md).
async fn run_compare_bakeoff(
    direct: Option<&str>,
    workflow: Option<&str>,
    task_file: Option<&str>,
    inline_task: Option<&str>,
) -> Result<()> {
    if direct.is_none() && workflow.is_none() {
        bail!("--compare requires --direct <agent> or --workflow <name>");
    }
    let task = read_task_text_with_inline(task_file, inline_task).await?;
    if task.is_empty() {
        bail!("--compare requires --task or --task-file");
    }

    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let trad_dir = format!("out/compare-traditional-{ts}");
    let ast_dir = format!("out/compare-ast-{ts}");

    // Traditional run: ensure override is off.
    ast::set_ast_native_override(false);
    eprintln!("=== compare run 1 / 2 — traditional substrate (out: {trad_dir}) ===");
    let t0 = std::time::Instant::now();
    let trad_inline = task.clone();
    if let Some(name) = direct {
        run_direct(name, None, Some(trad_inline.as_str()), Some(&trad_dir)).await?;
    } else if let Some(name) = workflow {
        run_workflow(
            name,
            None,
            Some(trad_inline.as_str()),
            Some(&trad_dir),
            None,
            false,
        )
        .await?;
    }
    let trad_stats = collect_run_stats(Path::new(&trad_dir), t0.elapsed().as_millis());

    // AST-native run.
    ast::set_ast_native_override(true);
    eprintln!("=== compare run 2 / 2 — AST-native substrate (out: {ast_dir}) ===");
    let t1 = std::time::Instant::now();
    let ast_inline = task.clone();
    if let Some(name) = direct {
        run_direct(name, None, Some(ast_inline.as_str()), Some(&ast_dir)).await?;
    } else if let Some(name) = workflow {
        run_workflow(
            name,
            None,
            Some(ast_inline.as_str()),
            Some(&ast_dir),
            None,
            false,
        )
        .await?;
    }
    let ast_stats = collect_run_stats(Path::new(&ast_dir), t1.elapsed().as_millis());
    ast::set_ast_native_override(false);

    let report = format_compare_report(&task, &trad_stats, &ast_stats, &trad_dir, &ast_dir);
    println!("{report}");

    let report_path = format!("out/compare-report-{ts}.md");
    if let Some(parent) = Path::new(&report_path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(e) = tokio::fs::write(&report_path, &report).await {
        tracing::warn!(error = %e, path = %report_path, "failed to write compare report");
    } else {
        eprintln!("Report written to {report_path}");
    }
    Ok(())
}

/// Walk `dir` collecting non-hidden file paths and their sizes for the
/// compare report.
fn collect_run_stats(dir: &Path, elapsed_ms: u128) -> BakeoffRunStats {
    let mut stats = BakeoffRunStats {
        elapsed_ms,
        ..Default::default()
    };
    if !dir.exists() {
        return stats;
    }
    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|n| n.starts_with('.'))
            .unwrap_or(false)
        {
            continue;
        }
        let size = entry.metadata().map(|m| m.len() as usize).unwrap_or(0);
        stats.total_bytes += size;
        stats.output_files.push(path.to_path_buf());
    }
    // Best-effort syntax check on Rust/Python/JS/Go outputs.
    let mut all_valid = true;
    let mut had_check = false;
    for f in &stats.output_files {
        if let Some((lang, _)) = ast::detect_language(f) {
            had_check = true;
            if let Ok(src) = std::fs::read_to_string(f)
                && ast::validate_syntax(&src, lang).is_err()
            {
                all_valid = false;
            }
        }
    }
    if had_check {
        stats.syntax_valid = Some(all_valid);
    }
    stats
}

/// Build a markdown comparison report from two `BakeoffRunStats`.
fn format_compare_report(
    task: &str,
    trad: &BakeoffRunStats,
    ast: &BakeoffRunStats,
    trad_dir: &str,
    ast_dir: &str,
) -> String {
    let task_summary: String = task.lines().next().unwrap_or("").chars().take(80).collect();
    let pct = |a: usize, b: usize| -> String {
        if a == 0 {
            "—".to_string()
        } else {
            let delta = b as f64 / a as f64 - 1.0;
            format!("{:+.1}%", delta * 100.0)
        }
    };
    let yn = |o: Option<bool>| match o {
        Some(true) => "YES",
        Some(false) => "NO",
        None => "—",
    };
    format!(
        "═══ Bake-off Comparison Report (#348) ═══\n\
         Task: {task_summary}\n\
         Timestamp: {ts}\n\
         \n\
         |                  | Traditional | AST-Native | Δ |\n\
         |------------------|-------------|------------|---|\n\
         | Output files     | {trad_files} | {ast_files} | {files_delta} |\n\
         | Output bytes     | {trad_bytes} | {ast_bytes} | {bytes_delta} |\n\
         | Wall-clock (ms)  | {trad_ms} | {ast_ms} | {ms_delta} |\n\
         | Syntax valid     | {trad_valid} | {ast_valid} | — |\n\
         \n\
         Traditional out_dir: {trad_dir}\n\
         AST-native  out_dir: {ast_dir}\n\
         \n\
         Note: LLM call / token counts are not yet plumbed end-to-end through \n\
         `run_direct`. The AST-native verdict is based on observable file \n\
         outputs and syntax validity. To compare per-call token use, attach \n\
         a perf collector (see `src/perf.rs`).\n",
        ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
        trad_files = trad.output_files.len(),
        ast_files = ast.output_files.len(),
        files_delta = pct(trad.output_files.len(), ast.output_files.len()),
        trad_bytes = trad.total_bytes,
        ast_bytes = ast.total_bytes,
        bytes_delta = pct(trad.total_bytes, ast.total_bytes),
        trad_ms = trad.elapsed_ms,
        ast_ms = ast.elapsed_ms,
        ms_delta = if trad.elapsed_ms > 0 {
            format!(
                "{:+.1}%",
                (ast.elapsed_ms as f64 / trad.elapsed_ms as f64 - 1.0) * 100.0
            )
        } else {
            "—".to_string()
        },
        trad_valid = yn(trad.syntax_valid),
        ast_valid = yn(ast.syntax_valid),
    )
}

/// PM mode: interactive orchestrator.
async fn run_pm() -> Result<()> {
    tracing::info!("open-mpm PM starting (orchestrator mode)");

    let mut pm_cfg = AgentConfig::by_name("pm").context("failed to load pm agent config")?;

    // Inject the dynamic agent roster into the PM system prompt. Without this,
    // the PM's TOML-encoded prompt would either hardcode a partial agent list
    // (root cause of over-delegation to `python-engineer`) or leave the
    // `{{available_agents}}` placeholder literal. Load the registry from the
    // same search-path policy used elsewhere so project-level overrides win.
    let roster_registry = agents::registry::AgentRegistry::load(
        &agents::registry::agent_search_paths(&default_bundled_config_dir()),
    );
    pm_cfg.system_prompt.content = agents::registry::inject_roster_into_prompt(
        &pm_cfg.system_prompt.content,
        &roster_registry,
    );

    let client = llm::create_client()?;

    // Registry with a single tool (delegate_to_agent) wired to the
    // production subprocess runner.
    let runner: Arc<dyn tools::AgentRunner> = Arc::new(SubprocessAgentRunner::new());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(DelegateToAgentTool::new(runner)));
    // #304: Coordinator-facing shell executor — see `tools::run_bash`.
    {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        registry.register(Arc::new(tools::run_bash::RunBashTool::new(cwd)));
    }
    // #244: Dynamic MCP service management tools (mcp_list/add/remove/enable/disable).
    for tool in tools::mcp_tools::mcp_tool_executors() {
        registry.register(tool);
    }
    // #243: Native ticketing tools (gated on `[github]` identity in
    // ~/.open-mpm/config.toml — silently absent when not configured).
    {
        let cfg = mcp::config::GlobalConfig::load().await;
        if let Some(identity) = cfg.github_identity(None)
            && let Some(tk_cfg) = identity.to_ticketing_config()
        {
            match tk_cfg.build_client().await {
                Ok(client_box) => {
                    let client: Arc<dyn ticketing::TicketingClient> = Arc::from(client_box);
                    let actions = ticketing::actions::build_actions_client(
                        identity.token().as_deref(),
                        identity.repo().as_deref(),
                    )
                    .await;
                    for tool in tools::native_ticketing::ticketing_tools(client, actions) {
                        registry.register(tool);
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "ticketing client build failed; PM running without ticketing tools");
                }
            }
        }
    }
    // #247: Native git tools, gated by `[git].available_for_roles` for "pm".
    // Repo discovery from cwd; failure is non-fatal (PM simply runs without
    // git tools when not inside a repo).
    {
        let cfg = mcp::config::GlobalConfig::load().await;
        if cfg.git.available_for_roles.iter().any(|r| r == "pm") {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            match git::GitRepo::open(&cwd) {
                Ok(repo) => {
                    for tool in tools::git_tools::git_tools(repo.root.clone()) {
                        registry.register(tool);
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "no git repo discovered; PM running without git tools");
                }
            }
        }
    }
    let openai_tools = registry.openai_tools()?;

    eprint!("> ");
    let mut user_input = String::new();
    let mut stdin = BufReader::new(tokio::io::stdin());
    stdin
        .read_line(&mut user_input)
        .await
        .context("failed to read user input from stdin")?;
    let user_input = user_input.trim().to_string();
    if user_input.is_empty() {
        bail!("empty user input");
    }

    tracing::debug!(user_input = %user_input, "dispatching to PM LLM");
    let response = llm::chat(
        &client,
        &pm_cfg.agent.model,
        &pm_cfg.system_prompt.content,
        &user_input,
        pm_cfg.llm.temperature,
        pm_cfg.llm.max_tokens,
        openai_tools,
    )
    .await?;

    if response.tool_calls.is_empty() {
        if let Some(text) = response.content {
            println!("{text}");
        } else {
            println!("(no content and no tool calls)");
        }
        return Ok(());
    }

    for tc in response.tool_calls {
        if !registry.contains(&tc.name) {
            tracing::warn!(tool = %tc.name, "ignoring unknown tool call");
            continue;
        }
        tracing::info!(tool = %tc.name, "dispatching PM tool call");
        let result = registry.dispatch(&tc.name, tc.arguments).await;
        if result.is_error() {
            eprintln!("tool '{}' failed: {}", tc.name, result.content());
        } else {
            println!("{}", result.content());
        }
    }

    Ok(())
}

/// Sub-agent mode: consume one Task, produce one Result/Error, exit.
///
/// Supports two execution paths based on the agent config's system prompt
/// "tools" list (resolved from the agent name):
///   - Agents with tool needs (research, qa, etc.) run a multi-turn loop
///     via `llm::chat_with_tools` with an appropriate `ToolRegistry`.
///   - Plain agents (python-engineer, plan-agent, observe-agent) run a
///     single-shot `llm::chat` with no tools.
async fn run_subagent(name: &str) -> Result<()> {
    tracing::info!(agent = %name, "sub-agent starting");

    let mut cfg = AgentConfig::by_name(name)
        .with_context(|| format!("failed to load agent config for '{name}'"))?;

    // #88: Per-call `max_turns` override via `OPEN_MPM_MAX_TURNS`. The wave
    // loop sets this to tighten the turn budget per file (e.g. 20) so a
    // single invocation can't absorb an entire wave's work. Applied after
    // config load and before any use of `cfg.llm.max_turns` so every code
    // path (tool-using + single-shot) honors it.
    // Why: The sub-agent reads the agent TOML (e.g. `code-agent.toml`,
    // `max_turns = 50`) which is correct for legacy/monolithic runs but too
    // loose for per-file wave-loop invocations. Env-var override keeps the
    // TOML as the default while letting the orchestrator enforce a tighter
    // cap without reshaping the `AgentRunner` trait.
    // What: Parses the env var as u32; silently ignores unparseable values
    // so a malformed override can't brick a sub-agent.
    if let Ok(s) = std::env::var("OPEN_MPM_MAX_TURNS")
        && let Ok(v) = s.parse::<u32>()
        && v > 0
    {
        tracing::info!(
            agent = %name,
            original = cfg.llm.max_turns,
            override_to = v,
            "applying OPEN_MPM_MAX_TURNS override"
        );
        cfg.llm.max_turns = v;
    }

    // Qualify bare Claude model ids with `anthropic/` when this sub-agent
    // routes via OpenRouter. Mirrors the PM-side fix in
    // `ctrl::run_pm_task_with_history`; without it, agent TOMLs that ship
    // bare ids (e.g. `claude-haiku-4-5`) get rejected with HTTP 400 by
    // OpenRouter. Centralized in `llm::credentials::qualify_openrouter_model`
    // so every dispatch path uses the same rule.
    if let Some(creds) = llm::credentials::pick_credentials(Some(cfg.agent.runner)) {
        let qualified = llm::credentials::qualify_openrouter_model(&creds, &cfg.agent.model);
        if qualified != cfg.agent.model {
            tracing::debug!(
                agent = %name,
                from = %cfg.agent.model,
                to = %qualified,
                "qualifying bare claude model id for OpenRouter (sub-agent)"
            );
            cfg.agent.model = qualified;
        }
    }

    // #61: Log which endpoint and auth source this agent will use so operators
    // can verify Claude Max OAuth vs API key vs OpenRouter at a glance.
    {
        let ep = cfg.adapter.api_endpoint(cfg.llm.use_anthropic_direct);
        // Strip "https://" prefix and any path after the host for a compact log.
        let host = ep
            .base_url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or(&ep.base_url);
        tracing::info!(
            agent = %name,
            model = %cfg.agent.model,
            endpoint = %host,
            auth = %ep.auth_source,
            "resolved agent endpoint"
        );
    }

    let client = llm::create_client()?;

    // Read stdin for the NDJSON Task line.
    let mut input = String::new();
    tokio::io::stdin()
        .read_to_string(&mut input)
        .await
        .context("failed to read sub-agent stdin")?;
    let first_line = input.lines().next().context("no NDJSON line on stdin")?;
    let msg = parse_message(first_line)?;

    let (task_id, task_text, history, session_reset) = match msg {
        IpcMessage::Task {
            id,
            task,
            history,
            session_reset,
        } => (id, task, history, session_reset),
        other => bail!("sub-agent expected Task message, got: {other:?}"),
    };

    // #51: Persistent-session reset. When the caller sets `session_reset`,
    // the sub-agent must behave as if no prior history exists for this run.
    // We simply ignore any history the caller also sent in that case.
    let effective_history: Option<Vec<session::HistoryMessage>> = if session_reset.unwrap_or(false)
    {
        None
    } else {
        history
    };

    tracing::debug!(task_id = %task_id, agent = %name, "sub-agent processing task");

    // Assemble the effective system prompt in layers:
    //   1. Base prompt from the agent TOML.
    //   2. CLAUDE.md ancestor walk from CWD (project + home instructions).
    //   3. Any resolved skills declared by the agent.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut builder =
        SystemPromptBuilder::new(cfg.system_prompt.content.clone()).walk_project_instructions(&cwd);

    // Harness protocol layers (single source of truth for write_file /
    // finish_task / out_dir / ## Summary rules). Injected between goal block
    // and base TOML prompt. Content is compiled into the binary via
    // `agents::harness_protocol` — the protocol is binary behavior, not user
    // config, so it cannot be disabled by editing files on disk.
    builder = builder.add_harness_layer(BASE_PROTOCOL);
    if matches!(cfg.agent.runner, agents::RunnerKind::ClaudeCode) && !cfg.llm.use_finish_task {
        builder = builder.add_harness_layer(CLAUDE_CODE_PROTOCOL);
    }
    if cfg.llm.use_finish_task {
        builder = builder.add_harness_layer(FINISH_TASK_PROTOCOL);
    }

    if let Some(skills) = &cfg.system_prompt.skills
        && !skills.is_empty()
    {
        let resolver = FsSkillResolver::from_defaults();
        for s in skills {
            if let Some(text) = resolver.resolve(s) {
                let layer = format!("# Skill: {s}\n\n{text}");
                builder = builder.add_skill(layer);
            } else {
                tracing::warn!(agent = %name, skill = %s, "skill not found; skipping");
            }
        }
    }

    // #241: MCP tool descriptions, role-gated. Engineer/coder/qa/ops agents
    // are excluded by `inject_for_roles` in the global config so this is a
    // no-op for them; coordinating roles (ctrl, pm, research, observe) get
    // a Markdown block listing the tools they can call.
    // #244: Use load() (no create-if-absent) so changes made by mcp_* tools
    // in earlier turns are reflected in this prompt build without caching.
    let mcp_cfg = mcp::GlobalConfig::load().await;
    if let Some(section) = mcp_cfg.render_prompt_section(&cfg.agent.role) {
        builder = builder.add_mcp_layer(section);
    }

    // #420: Inject caveman-style output compression fragment from the agent's
    // [compress] output_style field. Defaults to OutputStyle::Full so every
    // agent gets compression unless explicitly set to `output_style = "none"`.
    builder = builder.with_output_style(cfg.compress.output_style);

    let system_prompt_content = builder.build();

    // Optional out_dir for audit tool (from env set by subprocess runner).
    let out_dir = std::env::var_os("OPEN_MPM_OUT_DIR").map(PathBuf::from);
    // #222: Optional code_dir override for tools that write generated source
    // files (code-agent's WriteFileTool). Falls back to out_dir when unset
    // so legacy single-dir runs are unchanged.
    let code_dir = std::env::var_os("OPEN_MPM_CODE_DIR").map(PathBuf::from);

    // #81: Load the legacy skill registry once per sub-agent invocation. Missing
    // `.open-mpm/skills/` is a graceful no-op — the registry just stays empty.
    let skill_registry = Arc::new(
        skills::SkillRegistry::load(&cwd.join(".open-mpm").join("skills"))
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to load skill registry; using empty");
                skills::SkillRegistry::empty()
            }),
    );

    // #170: Load the tag-indexed skill registry (#168) using the same
    // hierarchical search paths as the PM process. This powers tag-ranked
    // `list_skills(tags=[...])` from within this sub-agent. Missing source
    // dirs are a graceful no-op — the registry simply returns empty results.
    let tag_skill_registry = Arc::new(skills::registry::SkillRegistry::load(
        &skills::registry::skill_search_paths(&default_bundled_config_dir()),
    ));

    // Build the per-agent tool registry based on agent name.
    let mut registry = build_registry_for_agent(
        name,
        out_dir.as_deref(),
        code_dir.as_deref(),
        skill_registry.clone(),
        tag_skill_registry.clone(),
    );

    // #57: If the agent opts into `use_finish_task`, auto-register the
    // terminal tool. Create a fresh registry when the agent didn't have one
    // (a pure `finish_task`-only agent is still valid).
    if cfg.llm.use_finish_task {
        let reg = registry.get_or_insert_with(ToolRegistry::new);
        reg.register(Arc::new(tools::finish_task::FinishTaskTool::new()));
    }

    let result = if let Some(reg) = registry {
        run_subagent_with_tools(
            &client,
            &cfg,
            &system_prompt_content,
            &task_text,
            reg,
            effective_history.as_deref(),
        )
        .await
    } else {
        run_subagent_single_shot(
            &client,
            &cfg,
            &system_prompt_content,
            &task_text,
            effective_history.as_deref(),
        )
        .await
    };

    let response = match result {
        Ok((content, usage)) => {
            // #27: Extract a summary from the agent's content so downstream
            // workflow phases receive a concise digest via `{{phase_name}}`
            // substitution rather than the full (often huge) output.
            let summary = extract_summary(&content);
            let summary_opt = if summary.is_empty() {
                None
            } else {
                Some(summary)
            };
            // #47: Only attach usage if we actually saw token counts; zero
            // usage would skew perf aggregations (the wire protocol omits
            // absent usage entirely thanks to `skip_serializing_if`).
            let usage_opt = if usage == perf::TokenUsage::default() {
                None
            } else {
                Some(usage)
            };
            IpcMessage::new_result_full(&task_id, content, summary_opt, usage_opt)
        }
        Err(e) => {
            let err_msg = IpcMessage::new_error(&task_id, format!("agent '{name}' failed: {e:#}"));
            let line = serialize_message(&err_msg)?;
            let mut stdout = tokio::io::stdout();
            stdout.write_all(line.as_bytes()).await?;
            stdout.flush().await?;
            return Err(e);
        }
    };

    let line = serialize_message(&response)?;
    let mut stdout = tokio::io::stdout();
    stdout.write_all(line.as_bytes()).await?;
    stdout.flush().await?;
    tracing::info!(agent = %name, "sub-agent complete");
    Ok(())
}

async fn run_subagent_single_shot(
    client: &async_openai::Client<async_openai::config::OpenAIConfig>,
    cfg: &AgentConfig,
    system_prompt: &str,
    task_text: &str,
    history: Option<&[session::HistoryMessage]>,
) -> Result<(String, perf::TokenUsage)> {
    // When the caller provided persistent-session history, we need the full
    // message vector path (system + history... + user). `llm::chat` only
    // takes system+user, so we fall through to the messages-based loop with
    // an empty tool registry in that case.
    if let Some(hist) = history
        && !hist.is_empty()
    {
        // #135: Apply send-time compression (no-op unless [compress] enabled).
        let (hist_compressed, task_compressed) =
            llm::apply_compression(hist.to_vec(), task_text.to_string(), &cfg.compress);

        let system_msg: ChatCompletionRequestMessage =
            ChatCompletionRequestSystemMessageArgs::default()
                .content(system_prompt)
                .build()
                .context("failed to build system message")?
                .into();
        let mut messages: Vec<ChatCompletionRequestMessage> =
            Vec::with_capacity(hist_compressed.len() + 2);
        messages.push(system_msg);
        for h in &hist_compressed {
            messages.push(h.clone().into_typed()?);
        }
        let user_msg: ChatCompletionRequestMessage =
            ChatCompletionRequestUserMessageArgs::default()
                .content(task_compressed.as_str())
                .build()
                .context("failed to build user message")?
                .into();
        messages.push(user_msg);

        // Bedrock-routed sub-agents need AWS profile/region exposed via env vars
        // (mirrors the guard in `run_subagent_with_tools`).
        let _aws_env_guard = if cfg.adapter.provider() == llm::adapter::Provider::Bedrock {
            Some(agents::in_process_runner::BedrockEnvGuard::install(
                cfg.llm.aws_profile.as_deref(),
                cfg.llm.aws_region.as_deref(),
            ))
        } else {
            None
        };

        let (content, usage) = llm::chat_with_tools_gated(
            client,
            &cfg.agent.model,
            &*cfg.adapter,
            messages,
            Arc::new(ToolRegistry::new()),
            cfg.tools.allowed.clone(),
            cfg.llm.temperature,
            cfg.llm.max_tokens,
            2,
            cfg.llm.enable_prompt_caching,
            resolve_tool_choice(cfg.llm.tool_choice, &*cfg.adapter),
            cfg.llm.use_finish_task,
            cfg.llm.use_anthropic_direct,
            &cfg.llm.stop_sequences,
        )
        .await?;
        return Ok((content, usage));
    }

    let response = llm::chat(
        client,
        &cfg.agent.model,
        system_prompt,
        task_text,
        cfg.llm.temperature,
        cfg.llm.max_tokens,
        vec![],
    )
    .await?;
    Ok((
        response
            .content
            .unwrap_or_else(|| "(sub-agent produced no content)".to_string()),
        response.usage,
    ))
}

async fn run_subagent_with_tools(
    client: &async_openai::Client<async_openai::config::OpenAIConfig>,
    cfg: &AgentConfig,
    system_prompt: &str,
    task_text: &str,
    registry: ToolRegistry,
    history: Option<&[session::HistoryMessage]>,
) -> Result<(String, perf::TokenUsage)> {
    let system_msg: ChatCompletionRequestMessage =
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_prompt)
            .build()
            .context("failed to build system message")?
            .into();

    // #135: Apply send-time compression (no-op unless [compress] enabled).
    // Stored history in the SessionManager is never mutated — only the
    // wire copy we're about to send is.
    let (hist_for_wire, task_for_wire) = llm::apply_compression(
        history.map(|h| h.to_vec()).unwrap_or_default(),
        task_text.to_string(),
        &cfg.compress,
    );

    // #51: If the caller forwarded session history (persistent agent), splice
    // it between the system message and the new user task so the model has
    // the full running dialog.
    let mut messages: Vec<ChatCompletionRequestMessage> = Vec::new();
    messages.push(system_msg);
    for h in &hist_for_wire {
        messages.push(h.clone().into_typed()?);
    }
    let user_msg: ChatCompletionRequestMessage = ChatCompletionRequestUserMessageArgs::default()
        .content(task_for_wire.as_str())
        .build()
        .context("failed to build user message")?
        .into();
    messages.push(user_msg);

    let allowed = cfg.tools.allowed.clone();

    // Bedrock-routed sub-agents need AWS profile/region exposed via env vars
    // so `chat_with_tools_gated` can build the Bedrock client. The in-process
    // runner installs an identical guard; the subprocess path was missing it,
    // which made `bedrock/...` agents fail with the SDK default credential
    // chain (no profile, wrong region).
    let _aws_env_guard = if cfg.adapter.provider() == llm::adapter::Provider::Bedrock {
        Some(agents::in_process_runner::BedrockEnvGuard::install(
            cfg.llm.aws_profile.as_deref(),
            cfg.llm.aws_region.as_deref(),
        ))
    } else {
        None
    };

    let (content, usage) = llm::chat_with_tools_gated(
        client,
        &cfg.agent.model,
        &*cfg.adapter,
        messages,
        Arc::new(registry),
        allowed,
        cfg.llm.temperature,
        cfg.llm.max_tokens,
        cfg.llm.max_turns,
        cfg.llm.enable_prompt_caching,
        resolve_tool_choice(cfg.llm.tool_choice, &*cfg.adapter),
        cfg.llm.use_finish_task,
        cfg.llm.use_anthropic_direct,
        &cfg.llm.stop_sequences,
    )
    .await?;
    Ok((content, usage))
}

/// Translate the TOML-level `ToolChoice` enum into the provider-specific
/// `tool_choice` JSON value using the agent's adapter.
///
/// Why: `agents::ToolChoice` is a small config enum; the actual wire shape
/// depends on the provider family (`{"type":"any"}` vs `"required"`), so we
/// funnel through the adapter here.
/// What: Maps `Auto` → adapter's auto value (usually `"auto"`), `Any` →
/// `tool_choice_any`, `None` → literal JSON `"none"`. Returns `None` when
/// the adapter has no preference (generic providers), letting the chat
/// builder omit the field entirely.
/// Test: Exercised through `main` integration; unit coverage via adapter tests.
fn resolve_tool_choice(
    choice: agents::ToolChoice,
    adapter: &dyn llm::adapter::ModelAdapter,
) -> Option<serde_json::Value> {
    match choice {
        agents::ToolChoice::Auto => adapter.tool_choice_auto(),
        agents::ToolChoice::Any => adapter.tool_choice_any(),
        agents::ToolChoice::None => Some(serde_json::Value::String("none".to_string())),
    }
}

/// Build a tool registry tailored to a specific agent.
///
/// Why: Different agents need different tools (research -> web_search,
/// load_skill; qa -> pytest_exec). Hardcoding the mapping here keeps it
/// discoverable; a later version could drive it from the agent TOML.
/// What: Returns `Some(ToolRegistry)` for agents that use tools, else None.
/// `out_dir`, if present, is used to register `advance_workflow_phase`.
/// Test: Called during `run_subagent`.
fn build_registry_for_agent(
    name: &str,
    out_dir: Option<&std::path::Path>,
    code_dir: Option<&std::path::Path>,
    skill_registry: Arc<skills::SkillRegistry>,
    tag_skill_registry: Arc<skills::registry::SkillRegistry>,
) -> Option<ToolRegistry> {
    // #222: When `code_dir` is set and distinct from `out_dir`, the code-agent
    // and any future tool that writes *generated source files* should root at
    // `code_dir` (the user's project tree). All other agents (plan, docs,
    // observe) keep writing artifacts to `out_dir`. When `code_dir` is None
    // we fall back to `out_dir` for full backward compatibility.
    let code_root = code_dir.or(out_dir);
    // #81: `load_skill` and `list_skills` are registered for every agent that
    // builds a registry. The skill registry itself is loaded once per process
    // (empty when `.open-mpm/skills/` is absent, so wiring is safe unconditionally).
    // Per-agent `[tools].allowed` lists still gate whether the agent can call
    // these; agents that omit `allowed` get unrestricted access as before.
    //
    // #170: When a non-empty tag-indexed registry (#168) is available, wire it
    // into `list_skills` so `tags=[...]` returns tag-ranked results. The
    // legacy `SkillRegistry` remains as a fallback for rendering when the
    // tag registry yields nothing and for `load_skill`'s frontmatter-aware
    // body rendering.
    let register_skill_tools = |reg: &mut ToolRegistry| {
        let resolver: Arc<dyn tools::SkillResolver> = Arc::new(FsSkillResolver::from_defaults());
        reg.register(Arc::new(SkillLoaderTool::with_registry(
            resolver.clone(),
            skill_registry.clone(),
        )));
        if !tag_skill_registry.is_empty() {
            reg.register(Arc::new(SkillListTool::with_tag_registry(
                resolver,
                Some(skill_registry.clone()),
                tag_skill_registry.clone(),
            )));
        } else {
            reg.register(Arc::new(SkillListTool::with_registry(
                resolver,
                skill_registry.clone(),
            )));
        }
    };
    // #52: `web_search` and `fetch_url` are registered unconditionally for
    // every agent that builds a registry. The per-agent `[tools].allowed`
    // list in TOML governs who is actually permitted to call them; the tool
    // itself degrades gracefully when BRAVE_API_KEY is unset.
    fn register_web_tools(reg: &mut ToolRegistry) {
        reg.register(Arc::new(BraveSearchTool::from_env()));
        reg.register(Arc::new(FetchUrlTool::new()));
    }

    /// #199: `wait_ms` and `poll_until` are universal async-flow tools — every
    /// agent benefits from being able to back off or wait for an external
    /// signal. Per-agent TOML allowlists still gate actual usage.
    fn register_timer_tools(reg: &mut ToolRegistry) {
        reg.register(Arc::new(tools::timer::WaitMsTool::new()));
        reg.register(Arc::new(tools::timer::PollUntilTool::new()));
    }

    // #53: `memory_recall` and `vector_search` are research aids and are
    // registered alongside web tools for any agent that benefits from them.
    // Both degrade gracefully when their underlying stores are missing, so
    // registering them is safe even when the project hasn't been indexed.
    //
    // #71: `memory_search` is a hybrid (vector + BM25) retriever with LLM
    // consolidation over the `.open-mpm/history/` turn log. Added alongside
    // the existing memory tools for the same gracefully-degrading rationale.
    fn register_memory_tools(reg: &mut ToolRegistry) {
        reg.register(Arc::new(MemoryRecallTool::new()));
        reg.register(Arc::new(VectorSearchTool::new()));
        reg.register(Arc::new(tools::memory_search::MemorySearchTool::from_env()));
    }

    match name {
        "research-agent" => {
            // Unified read-only investigator: web tools + memory/vector tools +
            // skills + read-only filesystem exploration. Merged with the former
            // explorer-agent so research-agent is the single "find out" agent.
            // All tools here are side-effect free; per-agent TOML allowlist
            // governs which are actually callable.
            let mut reg = ToolRegistry::new();
            register_web_tools(&mut reg);
            register_memory_tools(&mut reg);
            register_skill_tools(&mut reg);
            register_timer_tools(&mut reg);
            reg.register(Arc::new(ReadFileTool::new()));
            reg.register(Arc::new(ListDirTool::new()));
            reg.register(Arc::new(GrepFilesTool::new()));
            // #373: research benefits from structural analysis tools.
            for t in tools::analysis::analysis_tools() {
                reg.register(t);
            }
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
        "analysis-agent" => {
            // #373: code-quality analyst agent. Registers the full analysis
            // tool bundle (complexity, smells, hotspots, dependency cycles,
            // call graphs) plus read-only filesystem + skills + memory so it
            // can dig into specific files when an automated metric flags one.
            let mut reg = ToolRegistry::new();
            register_memory_tools(&mut reg);
            register_skill_tools(&mut reg);
            reg.register(Arc::new(ReadFileTool::new()));
            reg.register(Arc::new(ListDirTool::new()));
            reg.register(Arc::new(GrepFilesTool::new()));
            for t in tools::analysis::analysis_tools() {
                reg.register(t);
            }
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
        "code-agent" => {
            // Code generation agent. Gets write_file so it can emit files
            // directly as tool calls (avoids plain-text-mid-task retries for
            // large multi-file outputs). Also gets read-only exploration tools
            // so it can inspect existing code and the phase-audit tool for
            // workflow phase management.
            let mut reg = ToolRegistry::new();
            register_skill_tools(&mut reg);
            register_timer_tools(&mut reg);
            reg.register(Arc::new(ReadFileTool::new()));
            reg.register(Arc::new(ListDirTool::new()));
            reg.register(Arc::new(GrepFilesTool::new()));
            // #222: write_file roots at `code_root` (= code_dir when set,
            // else out_dir) so generated source lands in the user's project
            // tree when --project-dir is used. PhaseAuditTool stays anchored
            // at out_dir because the audit trail is an artifact.
            if let Some(dir) = code_root {
                // #88: If `OPEN_MPM_ASSIGNED_FILE` is set, we're inside a
                // per-file wave-loop invocation and must restrict writes to
                // that single path. Otherwise fall through to the legacy
                // unrestricted behavior (full code_root tree writable).
                let mut write_tool = WriteFileTool::new(dir.to_path_buf());
                if let Some(assigned) = std::env::var_os("OPEN_MPM_ASSIGNED_FILE") {
                    write_tool = write_tool.with_allowed_path(PathBuf::from(assigned));
                }
                reg.register(Arc::new(write_tool));
            } else {
                let fallback = std::env::current_dir().unwrap_or_default();
                reg.register(Arc::new(WriteFileTool::new(fallback)));
            }
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
        "plan-agent" => {
            // #53: planners benefit from memory_recall + vector_search to
            // ground implementation plans in existing code/decisions.
            // #87: plan-agent also gets write_file (scoped to out_dir) so it
            // can emit stub files and assignments.json for interface-first
            // decomposition. When out_dir is absent we fall back to CWD so
            // the tool remains discoverable in schemas.
            let mut reg = ToolRegistry::new();
            register_memory_tools(&mut reg);
            register_skill_tools(&mut reg);
            register_timer_tools(&mut reg);
            if let Some(dir) = out_dir {
                reg.register(Arc::new(WriteFileTool::new(dir.to_path_buf())));
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            } else {
                let fallback = std::env::current_dir().unwrap_or_default();
                reg.register(Arc::new(WriteFileTool::new(fallback)));
            }
            Some(reg)
        }
        "qa-agent" => {
            let mut reg = ToolRegistry::new();
            register_web_tools(&mut reg);
            // #71: memory tools so QA can recall prior decisions / failures.
            register_memory_tools(&mut reg);
            register_skill_tools(&mut reg);
            register_timer_tools(&mut reg);
            reg.register(Arc::new(ShellExecTool::new()));
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
        "local-ops-agent" => {
            // #77: Local operations agent. Registers a permissive (allowlisted)
            // shell executor plus the read-only filesystem tools so the agent
            // can run commands and verify their effects without mutating
            // source files. `finish_task` is auto-registered elsewhere when
            // `use_finish_task = true` in the agent TOML.
            let mut reg = ToolRegistry::new();
            let work_dir = std::env::current_dir().unwrap_or_default();
            reg.register(Arc::new(LocalOpsShellTool::new(work_dir)));
            reg.register(Arc::new(ReadFileTool::new()));
            reg.register(Arc::new(ListDirTool::new()));
            reg.register(Arc::new(GrepFilesTool::new()));
            register_skill_tools(&mut reg);
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
        "docs-agent" => {
            // #82: Documentation specialist. Reads generated code (read_file /
            // list_dir / grep_files) and writes docs (write_file) scoped to
            // the workflow's out_dir. `finish_task` is auto-registered
            // elsewhere via `use_finish_task = true` in the agent TOML.
            let mut reg = ToolRegistry::new();
            register_skill_tools(&mut reg);
            reg.register(Arc::new(ReadFileTool::new()));
            reg.register(Arc::new(ListDirTool::new()));
            reg.register(Arc::new(GrepFilesTool::new()));
            if let Some(dir) = out_dir {
                reg.register(Arc::new(WriteFileTool::new(dir.to_path_buf())));
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            } else {
                // Even without out_dir, register a WriteFileTool rooted at CWD
                // so the tool is discoverable in schemas. In practice workflow
                // mode always provides out_dir; direct mode may not.
                let fallback = std::env::current_dir().unwrap_or_default();
                reg.register(Arc::new(WriteFileTool::new(fallback)));
            }
            Some(reg)
        }
        _ => {
            // #81: Agents without a dedicated tool branch still benefit from
            // skill discovery/loading. Build a minimal registry that just
            // exposes `list_skills` and `load_skill`, plus the phase-audit
            // tool when a workflow out_dir is available. Per-agent allowlists
            // still govern whether any of these can actually be called.
            let mut reg = ToolRegistry::new();
            register_skill_tools(&mut reg);
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
    }
}

/// #186: Spawn the postmortem-agent subprocess for a session. Used both by
/// the auto-trigger path (after a workflow run with logged mistakes) and the
/// `postmortem` CLI subcommand.
///
/// Why: Centralizing dispatch keeps the construction of the task prompt,
/// agent name, and config dir in one place; both callers want the agent to
/// inspect the local `.open-mpm/state/mistakes/<session>.jsonl` file.
/// What: Builds a SubprocessAgentRunner pointed at the project's bundled
/// agents directory, hands it a task that names the session id and the
/// file path, and prints the resulting agent output to stderr.
/// Test: Manual; covered indirectly by the auto-trigger end-to-end flow.
async fn trigger_postmortem(project_root: &Path, session_id: &str) -> Result<()> {
    use tools::AgentRunner;
    let agents_config_dir = project_root.join(".open-mpm").join("agents");
    let log_path = project_root
        .join(".open-mpm")
        .join("state")
        .join("mistakes")
        .join(format!("{session_id}.jsonl"));
    let task = format!(
        "Analyze the mistake log at {} for session {} and produce a postmortem report following your standard format. Categorize each failure, apply fixes you are confident about, and recommend follow-ups.",
        log_path.display(),
        session_id
    );
    let runner = subprocess::SubprocessAgentRunner::new().with_config_dir(Some(agents_config_dir));
    let output = runner.run("postmortem-agent", &task).await?;
    eprintln!(
        "\n=== Postmortem Report ({session_id}) ===\n{}",
        output.content
    );
    Ok(())
}

/// #186: `open-mpm postmortem [--session <id>] [--last N]` subcommand.
///
/// Why: Operators want to invoke postmortem analysis on demand — either on
/// a specific failed session or on the recent global error stream — without
/// running a full workflow.
/// What: Parses --session and --last flags, dispatches to either
/// `trigger_postmortem` or feeds the recent global mistakes inline.
/// Test: Manual smoke (`open-mpm postmortem --last 5`); the helper logic is
/// unit-tested via `MistakeLog` directly.
async fn run_postmortem_subcommand(args: &[String]) -> Result<()> {
    let mut session: Option<String> = None;
    let mut last: usize = 20;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--session" => {
                session = args.get(i + 1).cloned();
                i += 2;
            }
            "--last" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<usize>().ok()) {
                    last = v;
                }
                i += 2;
            }
            _ => i += 1,
        }
    }

    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    if let Some(sid) = session {
        return trigger_postmortem(&project_root, &sid).await;
    }

    // No --session: feed the last N global mistakes to the agent inline.
    let recent = mistake_log::MistakeLog::read_recent_global(last)?;
    if recent.is_empty() {
        println!("(no mistakes recorded)");
        return Ok(());
    }
    let payload = serde_json::to_string_pretty(&recent)?;
    let task = format!(
        "Analyze these {} recent agent mistakes and produce a postmortem report:\n\n{}",
        recent.len(),
        payload
    );
    use tools::AgentRunner;
    let agents_config_dir = project_root.join(".open-mpm").join("agents");
    let runner = subprocess::SubprocessAgentRunner::new().with_config_dir(Some(agents_config_dir));
    let output = runner.run("postmortem-agent", &task).await?;
    println!("{}", output.content);
    Ok(())
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    fn empty_skill_registry() -> Arc<skills::SkillRegistry> {
        Arc::new(skills::SkillRegistry::empty())
    }

    fn empty_tag_registry() -> Arc<skills::registry::SkillRegistry> {
        Arc::new(skills::registry::SkillRegistry::empty())
    }

    #[test]
    fn research_agent_registry_has_web_tools() {
        let reg = build_registry_for_agent(
            "research-agent",
            None,
            None,
            empty_skill_registry(),
            empty_tag_registry(),
        )
        .expect("research-agent builds a registry");
        assert!(
            reg.contains("web_search"),
            "web_search missing from research-agent registry"
        );
        assert!(
            reg.contains("fetch_url"),
            "fetch_url missing from research-agent registry"
        );
    }

    #[test]
    fn research_agent_registry_has_memory_tools() {
        // #53: memory_recall + vector_search registered for the research agent.
        let reg = build_registry_for_agent(
            "research-agent",
            None,
            None,
            empty_skill_registry(),
            empty_tag_registry(),
        )
        .expect("research-agent builds a registry");
        assert!(reg.contains("memory_recall"), "memory_recall missing");
        assert!(reg.contains("vector_search"), "vector_search missing");
    }

    #[test]
    fn research_agent_registry_has_readonly_fs_tools() {
        // Merged from the former explorer-agent: research-agent is now the
        // single "find out" agent and must be able to read/grep the codebase.
        let reg = build_registry_for_agent(
            "research-agent",
            None,
            None,
            empty_skill_registry(),
            empty_tag_registry(),
        )
        .expect("research-agent builds a registry");
        assert!(reg.contains("read_file"), "read_file missing");
        assert!(reg.contains("list_dir"), "list_dir missing");
        assert!(reg.contains("grep_files"), "grep_files missing");
    }

    #[test]
    fn plan_agent_registry_has_memory_tools() {
        // #53: plan-agent gets memory_recall + vector_search so it can ground
        // plans in existing code / project knowledge.
        let reg = build_registry_for_agent(
            "plan-agent",
            None,
            None,
            empty_skill_registry(),
            empty_tag_registry(),
        )
        .expect("plan-agent builds a registry");
        assert!(reg.contains("memory_recall"), "memory_recall missing");
        assert!(reg.contains("vector_search"), "vector_search missing");
    }

    #[test]
    fn all_known_agents_get_skill_tools() {
        // #81: every agent that builds a registry should have load_skill and
        // list_skills available, regardless of whether the skill registry is
        // empty or populated. Per-agent `[tools].allowed` still controls which
        // tools are callable at runtime.
        for agent in [
            "research-agent",
            "plan-agent",
            "qa-agent",
            "local-ops-agent",
            "docs-agent",
            // Unknown agent name: default branch also registers skill tools.
            "unknown-agent",
        ] {
            let reg = build_registry_for_agent(
                agent,
                None,
                None,
                empty_skill_registry(),
                empty_tag_registry(),
            )
            .unwrap_or_else(|| panic!("{agent} should get a registry"));
            assert!(reg.contains("load_skill"), "{agent}: load_skill missing");
            assert!(reg.contains("list_skills"), "{agent}: list_skills missing");
        }
    }

    #[test]
    fn plan_agent_registry_has_write_file_tool() {
        // #87: plan-agent gets write_file so it can emit stub files and
        // assignments.json for interface-first decomposition.
        let reg = build_registry_for_agent(
            "plan-agent",
            None,
            None,
            empty_skill_registry(),
            empty_tag_registry(),
        )
        .expect("plan-agent builds a registry");
        assert!(
            reg.contains("write_file"),
            "write_file missing from plan-agent registry"
        );
    }

    #[test]
    fn docs_agent_registry_has_write_and_read_tools() {
        // #82: docs-agent gets write_file + read-only exploration tools so it
        // can inspect generated code and emit documentation files.
        let reg = build_registry_for_agent(
            "docs-agent",
            None,
            None,
            empty_skill_registry(),
            empty_tag_registry(),
        )
        .expect("docs-agent builds a registry");
        assert!(reg.contains("write_file"), "write_file missing");
        assert!(reg.contains("read_file"), "read_file missing");
        assert!(reg.contains("list_dir"), "list_dir missing");
        assert!(reg.contains("grep_files"), "grep_files missing");
    }

    #[tokio::test]
    async fn list_skills_uses_tag_registry_when_wired() {
        // #170: When `build_registry_for_agent` is called with a non-empty
        // tag-indexed registry, the resulting `list_skills` tool must return
        // tag-ranked JSON (not the legacy float-score format).
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("fastapi.md"),
            "---\nname: fastapi\ndescription: async routes\ntags: [python, fastapi]\n---\nbody\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("rust.md"),
            "---\nname: rust\ndescription: rust idioms\ntags: [rust]\n---\nbody\n",
        )
        .unwrap();

        let tag_reg = Arc::new(skills::registry::SkillRegistry::load(&[dir
            .path()
            .to_path_buf()]));
        assert!(!tag_reg.is_empty(), "sanity: tag registry loaded skills");

        let reg = build_registry_for_agent(
            "research-agent",
            None,
            None,
            empty_skill_registry(),
            tag_reg,
        )
        .expect("research-agent builds a registry");
        assert!(reg.contains("list_skills"));

        let result = reg
            .dispatch("list_skills", serde_json::json!({"tags": ["python"]}))
            .await;
        let content = result.content();
        assert!(
            content.contains("\"fastapi\""),
            "expected fastapi in tag-ranked output, got: {content}"
        );
        assert!(
            content.contains("\"match_score\""),
            "expected tag-registry JSON (match_score field), got: {content}"
        );
        assert!(
            !content.contains("\"rust\""),
            "rust has no 'python' tag and must be filtered out: {content}"
        );
    }

    #[tokio::test]
    async fn list_skills_falls_back_to_legacy_when_tag_registry_empty() {
        // #170: Wiring preserves legacy behavior when the tag registry is
        // empty (no `.open-mpm/skills/` configured). The tool must still
        // register and return a non-panicking response.
        let reg = build_registry_for_agent(
            "research-agent",
            None,
            None,
            empty_skill_registry(),
            empty_tag_registry(),
        )
        .expect("research-agent builds a registry");
        assert!(reg.contains("list_skills"));
        let result = reg.dispatch("list_skills", serde_json::json!({})).await;
        // Empty legacy + empty tag registry yields the resolver fallback
        // string; just assert the call succeeds without panicking.
        let _ = result.content();
    }

    #[tokio::test]
    async fn web_search_without_api_key_returns_graceful_error() {
        // Ensure no key is set for this scope.
        // SAFETY: removing an env var in a test; other tests do not rely on
        // BRAVE_API_KEY being set. The graceful-error path is what we assert.
        unsafe {
            std::env::remove_var("BRAVE_API_KEY");
        }
        let tool = BraveSearchTool::from_env();
        use tools::ToolExecutor;
        let out = tool
            .execute(serde_json::json!({"query": "rust async"}))
            .await;
        assert!(
            out.is_error(),
            "expected an error result when BRAVE_API_KEY is unset"
        );
        assert!(
            out.content().contains("BRAVE_API_KEY"),
            "error should mention BRAVE_API_KEY, got: {}",
            out.content()
        );
    }
}
