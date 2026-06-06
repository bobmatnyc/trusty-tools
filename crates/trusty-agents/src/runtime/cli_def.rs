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

//! Top-level clap CLI definition, bundled help config, and the small argv /
//! credential helpers that run before (and around) clap parsing.

use clap::Parser;

/// Bundled declarative help config (issue #216). Loaded once per process.
///
/// Why: every standalone trusty-* binary embeds its `help.yaml` via
/// `include_str!` so the workspace-shared `trusty_common::help::suggest`
/// helper has a single source of truth for unknown-subcommand hints. The
/// native `cli::did_you_mean` path that scans `KNOWN_SUBCOMMANDS` still runs
/// first for the common-case typos; this static covers the residual cases
/// the clap layer reports as `InvalidSubcommand` / `UnknownArgument`.
/// What: `LazyLock<HelpConfig>` parsed from `crates/trusty-agents/help.yaml` at
/// first access. `expect` is acceptable because the YAML is shipped inside
/// the binary; a parse failure would be caught on the first invocation.
/// Test: parse coverage lives in `trusty-common`; this site is exercised
/// manually via `trusty-agents memori`.
pub(super) static HELP: std::sync::LazyLock<trusty_common::help::HelpConfig> =
    std::sync::LazyLock::new(|| {
        trusty_common::help::load_help(include_str!("../../help.yaml"))
            .expect("trusty-agents help.yaml is bundled and valid")
    });

/// Top-level clap CLI for the `trusty-agents` binary.
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
    name = "trusty-agents",
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
    // to the controller) so `trusty-agents "do X"` keeps working.
    trailing_var_arg = true,
    allow_hyphen_values = true
)]
pub(super) struct Cli {
    /// Run as a sub-agent: read one NDJSON task from stdin, write one NDJSON
    /// result to stdout, exit.
    #[arg(long)]
    pub(super) agent: Option<String>,

    /// Run a named workflow from `.trusty-agents/workflows/<name>.json`.
    #[arg(long)]
    pub(super) workflow: Option<String>,

    /// Direct-agent mode: bypass the PM LLM and forward stdin/file to the
    /// named sub-agent.
    #[arg(long)]
    pub(super) direct: Option<String>,

    /// Inline task text (alternative to `--task-file` / stdin).
    #[arg(long)]
    pub(super) task: Option<String>,

    /// Path to a task description file.
    #[arg(long = "task-file")]
    pub(super) task_file: Option<String>,

    /// Output directory for workflow / direct artifacts (assignments.json,
    /// phase logs, observe output, perf records). When `--project-dir` is
    /// also set, generated application code lands in `--project-dir` and
    /// only workflow artifacts land here.
    #[arg(long = "out-dir")]
    pub(super) out_dir: Option<String>,

    /// Project directory where generated application code should land.
    /// Defaults to the value of `--out-dir` (or the auto-generated
    /// `out/<label>-<ts>/` path) for backward compatibility. Set this to
    /// CWD (e.g. `--project-dir .`) to have generated code written to your
    /// current project directory while keeping workflow artifacts
    /// elsewhere via `--out-dir`.
    #[arg(long = "project-dir")]
    pub(super) project_dir: Option<String>,

    /// Emit machine-readable JSON output where supported.
    #[arg(long)]
    pub(super) json: bool,

    /// Start the HTTP API server + embedded web UI.
    #[arg(long)]
    pub(super) api: bool,

    /// Alias for `--api` (kept for backwards compatibility).
    #[arg(long)]
    pub(super) serve: bool,

    /// Port for the API server (default 8080).
    #[arg(long)]
    pub(super) port: Option<u16>,

    /// Bearer token required for `POST /api/task` (overrides
    /// `TAGENT_API_TOKEN`).
    #[arg(long = "api-token")]
    pub(super) api_token: Option<String>,

    /// Single-shot PM mode (legacy compat).
    #[arg(long)]
    pub(super) pm: bool,

    /// Explicit CTRL mode (the default when no other mode flag is set).
    #[arg(long)]
    pub(super) ctrl: bool,

    /// Run the Telegram bot gateway (#264). Requires `TELEGRAM_BOT_TOKEN`.
    ///
    /// Headless/server mode: takes over the process and runs only the bot.
    /// For interactive use inside the REPL, prefer the `/telegram` slash
    /// command, which runs the bot as a background tokio task while keeping
    /// the REPL interactive.
    #[arg(long)]
    pub(super) telegram: bool,

    /// Run the Slack Socket Mode bot gateway (#418). Requires
    /// `SLACK_APP_TOKEN` (xapp-...) and `SLACK_BOT_TOKEN` (xoxb-...).
    ///
    /// Headless/server mode: takes over the process and runs only the bot.
    #[arg(long)]
    pub(super) slack: bool,

    /// Reindex the local code/memory store.
    #[arg(long)]
    pub(super) reindex: bool,

    /// File-watcher mode.
    #[arg(long)]
    pub(super) watch: bool,

    /// Print and re-home orphaned files.
    #[arg(long = "check-orphans")]
    pub(super) check_orphans: bool,

    /// Clear in-process persistent agent sessions before this run.
    #[arg(long = "clear-sessions")]
    pub(super) clear_sessions: bool,

    /// Force re-initialization of the project (regenerate `.trusty-agents/state/`).
    #[arg(long)]
    pub(super) reinit: bool,

    /// #348: Enable AST-native tools for the engineer agent regardless of
    /// the agent TOML's `[tools] ast_native` setting.
    ///
    /// Why: Lets bake-off operators flip the substrate per-invocation
    /// without editing config. Honoured for `--direct` and `--workflow` runs.
    /// What: Sets a process-global flag that the in-process runner reads
    /// when registering tools.
    #[arg(long = "ast-native", default_value_t = false)]
    pub(super) ast_native: bool,

    /// #348: Run a bake-off in comparison mode — execute the task once with
    /// the traditional substrate and once with `--ast-native`, then emit a
    /// side-by-side report of LLM calls, token counts, and output sizes.
    #[arg(long, default_value_t = false)]
    pub(super) compare: bool,

    /// #350: Parse `src/` into the symbol registry and persist it to
    /// `.trusty-agents/state/symbol-registry.json`.
    #[arg(long, default_value_t = false)]
    pub(super) parse_to_registry: bool,

    /// #350: Project the persisted symbol registry back to source files
    /// under the project root (deterministic emission).
    #[arg(long, default_value_t = false)]
    pub(super) emit_from_registry: bool,

    /// #350: Verify all symbol-registry content hashes match their stored
    /// source. Exits non-zero if any mismatches are found.
    #[arg(long, default_value_t = false)]
    pub(super) verify_registry: bool,

    /// Print the version banner and exit.
    #[arg(long, short = 'V')]
    pub(super) version: bool,

    /// Manage the persistent trusty-agents background service (#343).
    /// Accepts: `start`, `stop`, `status`. When set the binary handles
    /// the subcommand and exits without entering REPL/serve modes.
    #[arg(long)]
    pub(super) service: Option<String>,

    /// #374: Run the search-as-a-service daemon. Owns the redb code-store
    /// lock and serves /search/{health,query,index-file,remove-file,reindex}
    /// over HTTP for the lifetime of the process. Used by other trusty-agents
    /// processes (REPL, sub-agents, --api server) to share a single warm
    /// index without re-opening the on-disk store per process.
    #[arg(long = "search-service", default_value_t = false)]
    pub(super) search_service: bool,

    /// Anything else — typically a free-text task to forward to the
    /// controller. Preserved as positional tokens so `trusty-agents "do X"`
    /// keeps working.
    #[arg(allow_hyphen_values = true, num_args = 0..)]
    pub(super) rest: Vec<String>,
}

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
pub(super) fn check_credentials_and_warn() {
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
    eprintln!("│  ⚡  No API key found — trusty-agents needs a key to talk to an LLM  │");
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
    eprintln!("│  Restart trusty-agents after adding the key. (REPL continues below)  │");
    eprintln!("└─────────────────────────────────────────────────────────────────┘");
    eprintln!();
}

/// Concatenate non-flag positional args into a single task string.
///
/// Why: When the user runs `trusty-agents "say hi"` (or `trusty-agents say hi`), we
/// want to forward "say hi" — but only the parts that aren't mode flags
/// already filtered above. Mode-flagged invocations short-circuit before
/// reaching this function.
/// What: Skips argv[0] (binary name) and any token starting with `--`.
/// Joins the remainder with single spaces.
/// Test: `argv_as_task_text_strips_flags_and_joins`.
pub(super) fn argv_as_task_text(args: &[String]) -> String {
    args.iter()
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}
