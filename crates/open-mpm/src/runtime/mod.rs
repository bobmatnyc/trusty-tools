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

// Submodule declarations (issue #171: runtime.rs split into focused modules).
mod direct_mode;
mod indexer;
mod registry_cmds;
mod service_cmds;
mod session_cmds;
mod subagent_mode;
mod workflow_mode;

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
        trusty_common::help::load_help(include_str!("../../help.yaml"))
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
        return subagent_mode::run_postmortem_subcommand(&args[2..]).await;
    }

    // #167: `agents list` — print all agents discovered from the hierarchical
    // search paths with their source + capability tags. Useful to verify
    // per-project / per-user overrides are being picked up.
    if args.len() > 1 && args[1] == "agents" {
        return registry_cmds::run_agents_subcommand(&args[2..]).await;
    }

    // #168: `skills list [--tag <tag>]` — print all skills discovered from the
    // hierarchical search paths with their source + tags. Supports `--tag`
    // to filter + rank by tag overlap.
    if args.len() > 1 && args[1] == "skills" {
        return registry_cmds::run_skills_subcommand(&args[2..]).await;
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
        return registry_cmds::run_plugins_subcommand(&args[2..]).await;
    }

    // #449: `open-mpm eval run --suite <path>` — run a behavior eval suite.
    if args.len() > 1 && args[1] == "eval" {
        return registry_cmds::run_eval_subcommand(&args[2..]).await;
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
        return registry_cmds::run_dashboard_subcommand(&args[idx + 1..]).await;
    }

    const SERVICE_SUBCOMMANDS: &[&str] = &["start", "stop", "status", "connect", "session"];
    if let Some(idx) = find_subcommand_index(&args, SERVICE_SUBCOMMANDS) {
        match args[idx].as_str() {
            "start" => return service_cmds::run_start_subcommand(&args[idx + 1..]).await,
            "stop" => return service_cmds::run_stop_subcommand(&args[idx + 1..]).await,
            "status" => return service_cmds::run_status_subcommand(&args[idx + 1..]).await,
            "connect" => return service_cmds::run_connect_subcommand(&args[idx + 1..]).await,
            "session" => return session_cmds::handle_session_subcommand(&args[idx + 1..]).await,
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
        return subagent_mode::run_subagent(name).await;
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
        return indexer::run_reindex().await;
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
        return indexer::run_check_orphans().await;
    }

    if cli.watch {
        return indexer::run_watch().await;
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
        return direct_mode::run_compare_bakeoff(
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
        return workflow_mode::run_workflow(
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
        return direct_mode::run_direct(
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
    indexer::spawn_background_file_watcher();

    // --pm flag: single-shot PM mode (backward compat)
    if cli.pm {
        return subagent_mode::run_pm().await;
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
pub(super) fn update_skill_usage_after_run(
    perf_record: &perf::PerfRecord,
    observe_output: Option<&str>,
) {
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

#[cfg(test)]
mod main_tests {
    use super::workflow_mode::read_task_text_with_inline;
    use super::{Cli, argv_as_task_text};
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

// Bundled agent config directory — honors `OPEN_MPM_CONFIG_DIR` with a
// CWD-relative `.open-mpm/` fallback (#167).
//
// Why: The registry search-path function wants the "bundled" config root
// (it appends `/agents` internally). We honor the same env var as
// `agents::mod::agent_config_path` so packaged binaries can point the
// loader at a vendored config tree.
// What: Returns `${OPEN_MPM_CONFIG_DIR}` as-is if set (so search_paths
// appends `/agents`), else `./.open-mpm`. Note: the repo's bundled config
// lives at `.open-mpm/` now (formerly `config/`); runtime state is in
// `.open-mpm/state/` (gitignored).
//
// Note: `default_bundled_config_dir` is defined once in `crate::lib` and
//       pulled in at the top of this file via `use crate::default_bundled_config_dir;`.
//       No duplicate definition here.
