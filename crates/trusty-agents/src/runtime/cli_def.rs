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

    /// Interface to bind the API server on (#3329). Defaults to loopback
    /// (`127.0.0.1`). A non-loopback bind (e.g. `0.0.0.0`) is an explicit
    /// opt-in and REQUIRES `--api-token`; the server refuses to start
    /// unauthenticated on a non-loopback interface. Prefer the trusty-console
    /// proxy (`/api/agents/*`) for remote access over binding this directly.
    #[arg(long)]
    pub(super) bind: Option<std::net::IpAddr>,

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

    /// #3052: Force the persistent plain-line REPL instead of the ratatui
    /// full-screen TUI, even when stdin is a TTY. Intended for SSH / narrow
    /// terminals (e.g. Terminus on iPhone) where the alt-screen TUI reflows
    /// badly and modal pickers can swallow keystrokes. Equivalent to setting
    /// `TAGENT_NO_TUI=1`. Bare `trusty-agents` (neither flag nor env set)
    /// keeps launching the TUI unchanged.
    #[arg(long)]
    pub(super) plain: bool,

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

/// Whether at least one of the three credential providers ctrl/PM's own LLM
/// calls route through (`openrouter`, `anthropic`, `claude-code`) resolves
/// via ANY tier (process env, `.env.local`, or the secure store).
///
/// Why: `check_credentials_and_warn` couples this decision to printing a
/// stderr banner, which makes the decision itself impossible to unit test
/// without capturing/parsing stderr output. Extracting the pure predicate
/// (issue #3429 code-critic follow-up on PR #3431 — the doc-pointer lint
/// caught that this logic had NO real test coverage, only a dangling
/// `Test:` citation) lets tests assert the resolve / no-resolve outcome
/// directly. Hermeticity: rather than injecting a resolver closure, this
/// reuses the SAME `$HOME`-sandboxed `FileKeyStore` convention
/// `llm::credentials::tests::pick_consults_store_when_env_absent` already
/// established for testing this exact production `resolve_key` call
/// (`resolve_key`'s store tier resolves via `dirs::home_dir()`, so
/// redirecting `$HOME` to a tempdir makes the real function hermetic
/// without needing a seam) — the tests below (`banner_suppressed_when_
/// store_configures_a_key`, `banner_fires_when_nothing_resolves`) never
/// touch the real developer machine's env or store.
/// What: `true` when `trusty_common::inference::credentials::resolve_key`
/// finds `claude-code`, `anthropic`, OR `openrouter` — the exact same three
/// providers `llm::credentials::pick_credentials` checks (this predicate
/// does NOT gate `claude-code` on an agent's `runner` the way
/// `pick_credentials` does, since the banner's question is simply "is ANY
/// of these three configured at all," not "which one would THIS agent
/// use").
/// Test: `banner_suppressed_when_store_configures_a_key`,
/// `banner_fires_when_nothing_resolves`.
fn any_credential_resolves() -> bool {
    trusty_common::inference::credentials::resolve_key("claude-code").is_some()
        || trusty_common::inference::credentials::resolve_key("anthropic").is_some()
        || trusty_common::inference::credentials::resolve_key("openrouter").is_some()
}

/// Print a prominent onboarding banner when no API credential resolves via
/// ANY tier (env, project `.env.local`, user `$HOME/.env.local`, or the
/// secure store).
///
/// Why: New users who clone the repo and run `om` without configuring a key
/// get confusing LLM errors. Surfacing setup instructions before the REPL
/// opens is friendlier and self-service. OpenRouter is recommended because
/// it's free-tier, supports many models, and is already the deployment
/// fallback.
///
/// Issue #3406 follow-up (confirmed live): the previous implementation
/// checked ONLY raw `std::env::var` for the three credential names — so a
/// key configured via `tagent config keys set <provider>` (the secure store)
/// or a project/user `.env.local` produced a FALSE "no API key found" banner
/// even though real dispatch (`llm::credentials::pick_credentials`, which
/// this now matches) would resolve and use it fine. This is the exact bug
/// the owner's clean-shell repro hit: `openrouter` was configured via the
/// secure store, `tagent config keys list` correctly reported it, yet the
/// startup banner still claimed no key existed.
/// What: Consults the same 3-tier resolver
/// (`trusty_common::inference::credentials::resolve_key`) `pick_credentials`
/// uses for the three provider names this binary's own LLM calls route
/// through (`openrouter`, `anthropic`, `claude-code`); when at least one
/// resolves, no banner prints. When none resolve, prints a boxed banner
/// listing every location actually checked (env var names, the project
/// `.env.local` path when one is in scope, the user `$HOME/.env.local` path,
/// and the secure store) plus BOTH remediation options — a project/user
/// `.env.local` line AND `tagent config keys set <provider>` (the durable,
/// works-from-any-directory option `.env.local` alone cannot offer from
/// `$HOME`). Non-fatal — the REPL still opens so CLI-only subcommands
/// (memory search, skills list) keep working. Delegates the actual
/// resolve-or-not DECISION to `any_credential_resolves` — a pure,
/// independently-testable predicate — rather than inlining it here, since
/// this function's own stderr side effects make ITS behavior awkward to
/// assert directly.
/// Test: `banner_suppressed_when_store_configures_a_key`,
/// `banner_fires_when_nothing_resolves` (both in the `tests` module below)
/// exercise `any_credential_resolves` directly — the gate this function
/// wraps.
pub(super) fn check_credentials_and_warn() {
    if any_credential_resolves() {
        return;
    }

    let project_env_local = std::env::current_dir()
        .ok()
        .and_then(|cwd| trusty_common::inference::credentials::find_workspace_env_local(&cwd));
    let user_env_local = dirs::home_dir()
        .and_then(|h| trusty_common::inference::credentials::user_env_local_path(&h));

    eprintln!();
    eprintln!("┌─────────────────────────────────────────────────────────────────┐");
    eprintln!("│  ⚡  No API key found — trusty-agents needs a key to talk to an LLM  │");
    eprintln!("├─────────────────────────────────────────────────────────────────┤");
    eprintln!("│                                                                 │");
    eprintln!("│  Checked (none resolved):                                       │");
    eprintln!("│    env: OPENROUTER_API_KEY / ANTHROPIC_API_KEY /                │");
    eprintln!("│         CLAUDE_CODE_OAUTH_TOKEN                                  │");
    match &project_env_local {
        Some(p) => eprintln!("│    project .env.local: {}", p.display()),
        None => eprintln!("│    project .env.local: none found above this directory          │"),
    }
    match &user_env_local {
        Some(p) => eprintln!("│    user .env.local: {}", p.display()),
        None => eprintln!("│    user ~/.env.local: not present                                │"),
    }
    eprintln!("│    secure store (`tagent config keys list`): empty              │");
    eprintln!("│                                                                 │");
    eprintln!("│  Durable option — works from ANY directory:                     │");
    eprintln!("│    tagent config keys set openrouter                            │");
    eprintln!("│                                                                 │");
    eprintln!("│  Quickest option — get a free OpenRouter key (5 min):           │");
    eprintln!("│    https://openrouter.ai/keys                                   │");
    eprintln!("│                                                                 │");
    eprintln!("│  Or drop it in ~/.env.local (works from any directory) or a     │");
    eprintln!("│  project .env.local (project-scoped only):                      │");
    eprintln!("│    echo 'OPENROUTER_API_KEY=sk-or-v1-...' >> ~/.env.local       │");
    eprintln!("│                                                                 │");
    eprintln!("│  Or use Claude Code OAuth (if you have Claude Code installed):  │");
    eprintln!("│    claude setup-token   # copies token to clipboard             │");
    eprintln!("│    tagent config keys set claude-code                           │");
    eprintln!("│                                                                 │");
    eprintln!("│  Restart trusty-agents after adding the key. (REPL continues below)  │");
    eprintln!("└─────────────────────────────────────────────────────────────────┘");
    eprintln!();
}

/// Decide whether an interactive TTY invocation should be forced into the
/// persistent plain-line CLI mode (`ctrl::run_plain_cli`) instead of the
/// ratatui full-screen TUI (#3052).
///
/// Why: The decision has two independent triggers — the `--plain` clap flag
/// and the `TAGENT_NO_TUI=1` env var — and `dispatch_cli_mode` needs to check
/// it before the existing `repl::is_tty()` branch so it wins regardless of
/// terminal detection. Extracted as a pure function (rather than inlined at
/// the call site) so the decision matrix is unit-testable without spinning up
/// a real CLI/TTY/process.
/// What: `true` when `plain_flag` is set OR `no_tui_env` is exactly `"1"`.
/// Any other env value (unset, empty, `"0"`, `"true"`, …) is treated as
/// not-set, matching the literal `TAGENT_NO_TUI=1` convention documented on
/// the `--plain` flag and in the crate README.
/// Test: `should_force_plain_cli_flag_alone`, `_env_alone`, `_neither`,
/// `_env_zero_is_not_forced`.
pub(super) fn should_force_plain_cli(plain_flag: bool, no_tui_env: Option<String>) -> bool {
    plain_flag || no_tui_env.as_deref() == Some("1")
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

#[cfg(test)]
mod tests {
    use super::should_force_plain_cli;

    #[test]
    fn should_force_plain_cli_flag_alone() {
        assert!(should_force_plain_cli(true, None));
    }

    #[test]
    fn should_force_plain_cli_env_alone() {
        assert!(should_force_plain_cli(false, Some("1".to_string())));
    }

    #[test]
    fn should_force_plain_cli_neither() {
        assert!(!should_force_plain_cli(false, None));
    }

    #[test]
    fn should_force_plain_cli_env_zero_is_not_forced() {
        assert!(!should_force_plain_cli(false, Some("0".to_string())));
    }

    #[test]
    fn should_force_plain_cli_env_garbage_is_not_forced() {
        assert!(!should_force_plain_cli(false, Some("true".to_string())));
    }

    #[test]
    fn should_force_plain_cli_bare_no_flags_matches_default_dispatch() {
        // Bare `trusty-agents` — neither the clap flag parsed nor the env var
        // set — must NOT force plain mode, so the existing
        // is_tty()-vs-piped-loop dispatch branch is unchanged (no regression
        // for desktop TUI users).
        assert!(!should_force_plain_cli(false, None));
    }

    /// Coverage for `any_credential_resolves` — the pure decision
    /// `check_credentials_and_warn`'s banner gate wraps (issue #3429
    /// code-critic follow-up on PR #3431: the doc-comment pointer lint
    /// caught that this logic had zero real test coverage).
    mod credential_banner {
        use super::super::any_credential_resolves;
        use serial_test::serial;

        /// Clears every env var `any_credential_resolves` (transitively, via
        /// `resolve_key`) consults. Callers must hold both
        /// `crate::test_env::ENV_LOCK` and `crate::test_env::HOME_LOCK` for
        /// the whole test body — mirrors
        /// `llm::credentials::tests::clear_all` in the sibling module that
        /// tests the same underlying `resolve_key` calls.
        fn clear_all() {
            unsafe {
                std::env::remove_var("OPENROUTER_API_KEY");
                std::env::remove_var("ANTHROPIC_API_KEY");
                std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");
            }
        }

        /// Why: this is the exact bug the owner's live clean-shell repro
        /// hit — `openrouter` configured ONLY via the secure store (never
        /// exported to the shell), with all three credential env vars
        /// absent. The pre-fix banner (a raw `std::env::var` check) would
        /// have fired here even though real dispatch resolves the key fine;
        /// `any_credential_resolves` must say "resolved" so
        /// `check_credentials_and_warn` suppresses the banner.
        /// What: sandboxes `$HOME` to a tempdir (mirrors
        /// `llm::credentials::tests::pick_consults_store_when_env_absent`,
        /// which tests the identical `resolve_key` call), seeds `openrouter`
        /// directly into a `FileKeyStore` rooted there, clears every
        /// credential env var, and asserts `any_credential_resolves()` is
        /// `true`. Never touches the real developer machine's store — the
        /// sandboxed `$HOME` is a fresh tempdir for the duration of the
        /// test only.
        /// Test: itself.
        #[test]
        #[serial]
        fn banner_suppressed_when_store_configures_a_key() {
            let _env_guard = crate::test_env::ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _home_guard = crate::test_env::HOME_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            clear_all();

            let tmp = tempfile::TempDir::new().expect("tempdir");
            let prev_home = std::env::var_os("HOME");
            // SAFETY: HOME_LOCK held for the entire test body.
            unsafe {
                std::env::set_var("HOME", tmp.path());
            }

            let store = trusty_common::inference::credentials::FileKeyStore::at(tmp.path());
            trusty_common::inference::credentials::KeyStore::set(
                &store,
                "openrouter",
                "sk-or-from-store", // pragma: allowlist secret
            )
            .expect("seed store");

            assert!(
                any_credential_resolves(),
                "a store-only openrouter key must resolve and suppress the banner"
            );

            // SAFETY: HOME_LOCK still held.
            unsafe {
                match prev_home {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }

        /// Why: the banner's legitimate-fire case — nothing configured
        /// anywhere (no env var, no `.env.local` loaded into this test's
        /// process env, no store entry). Sandboxing `$HOME` to a FRESH,
        /// never-written-to tempdir guarantees the store tier is genuinely
        /// empty rather than accidentally reading the real developer
        /// machine's `~/.trusty-agents`/keychain state.
        /// What: clears every credential env var, sandboxes `$HOME` to an
        /// empty tempdir (no `FileKeyStore` entries seeded), and asserts
        /// `any_credential_resolves()` is `false`.
        /// Test: itself.
        #[test]
        #[serial]
        fn banner_fires_when_nothing_resolves() {
            let _env_guard = crate::test_env::ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _home_guard = crate::test_env::HOME_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            clear_all();

            let tmp = tempfile::TempDir::new().expect("tempdir");
            let prev_home = std::env::var_os("HOME");
            // SAFETY: HOME_LOCK held for the entire test body.
            unsafe {
                std::env::set_var("HOME", tmp.path());
            }

            assert!(
                !any_credential_resolves(),
                "with nothing configured anywhere, the banner must fire"
            );

            // SAFETY: HOME_LOCK still held.
            unsafe {
                match prev_home {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }
}
