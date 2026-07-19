//! CLI argument definitions for the `trusty-mpm` / `tm` binary.
//!
//! Why: keeping all clap struct/enum definitions in one file makes it easy to
//! audit the full CLI surface without wading through handler logic. As of
//! issue #2603 the file itself is split into a thin facade (`Cli` +
//! `Command`, this file) plus one submodule per command group's action enum
//! under `cli/actions/` — mirroring the #170-#172 precedent — because the
//! flat file had grown to 882 SLOC, past the 500-SLOC production cap.
//! What: defines `Cli` and `Command` here; every `Command` variant's nested
//! action enum (`SessionAction`, `ProjectsAction`, `McpCmd`, …) is re-exported
//! from `cli::actions` below so `crate::cli::<Type>` keeps resolving exactly
//! as before the split. `ManagerAction` continues to live in the sibling
//! `cli_manager.rs` (extracted pre-#2603 for the same SLOC-cap reason) and is
//! re-exported the same way.
//! Test: each variant is exercised by the `cli_parses_*` unit tests in
//! `tests.rs`.

use std::net::SocketAddr;

use clap::{Parser, Subcommand};

mod actions;

pub(crate) use actions::*;

// The `tm manager <action>` subcommand tree lives in `cli_manager.rs` (extracted
// to keep this file under the SLOC cap); re-exported so `crate::cli::ManagerAction`
// stays the stable reference every dispatcher and parse test uses.
pub(crate) use crate::cli_manager::ManagerAction;

/// Default daemon URL, for test convenience only (issue #2487).
///
/// Why (issue #1268, revised #2487): this used to be the `default_value` for
/// every `--url`/`TRUSTY_MPM_URL` clap field so the client and the daemon's
/// bind address could never drift. As of #2487 every such field is
/// `Option<String>` with NO `default_value` — the "no explicit override"
/// fallback now lives entirely in `core::discovery::resolve_daemon_url*`
/// (still ultimately `trusty_mpm::core::DEFAULT_DAEMON_URL`), not in clap.
/// This alias survives purely so tests can assert against the well-known
/// default value without importing the library constant under a different
/// name; `#[cfg(test)]` reflects that it has no production callers.
/// What: alias of [`trusty_mpm::core::DEFAULT_DAEMON_URL`].
/// Test: used throughout `tests.rs`; `core::discovery::default_url_matches_addr`
/// proves URL and addr agree.
#[cfg(test)]
pub(crate) const DEFAULT_URL: &str = trusty_mpm::core::DEFAULT_DAEMON_URL;

/// Default daemon bind address for the `daemon --addr` flag (issue #1268).
///
/// Why: keeps the daemon's bind default tied to the same
/// [`trusty_mpm::core::DEFAULT_DAEMON_URL`] the client resolvers fall back to
/// (see `core::discovery`), so `tm start` and the thin CLI always agree.
/// What: alias of [`trusty_mpm::core::DEFAULT_DAEMON_ADDR`].
/// Test: `core::discovery::default_addr_parses`.
pub(crate) const DEFAULT_ADDR: &str = trusty_mpm::core::DEFAULT_DAEMON_ADDR;

/// trusty-mpm command-line interface.
///
/// When invoked without a subcommand (#1708) the guided default fires:
/// the daemon's in-project spawn path is called for the current directory,
/// reconnecting to an existing session or provisioning a new worktree.
#[derive(Debug, Parser)]
#[command(
    name = "trusty-mpm",
    version,
    about = "trusty-mpm — unified binary",
    subcommand_required = false,
    arg_required_else_help = false
)]
pub(crate) struct Cli {
    /// Base URL of the trusty-mpm daemon (used by the thin CLI subcommands).
    ///
    /// Why (#2487): deliberately `Option<String>` with NO `default_value`.
    /// `None` means "the operator did not pass `--url` and `TRUSTY_MPM_URL`
    /// is unset" — every downstream resolver (`core::discovery::
    /// resolve_daemon_url*`) treats that, and only that, as license to fall
    /// through to the lock file / gateway probe / compiled-in default. A
    /// `String` with `default_value = DEFAULT_URL` made "unset" and "operator
    /// explicitly set TRUSTY_MPM_URL to the literal default value" the exact
    /// same string, which is what let `TRUSTY_MPM_URL=http://127.0.0.1:7880`
    /// get silently rerouted through the trusty-console gateway proxy instead
    /// of honoured directly (issue #2487). Matches the pattern already used
    /// by `SessionAction::Tui::url`.
    /// What: `Some(v)` exactly when `--url` or `TRUSTY_MPM_URL` was supplied.
    /// Test: `cli_url_flag_overrides_default`, `cli_url_flag_equal_to_default_is_still_some`.
    #[arg(long, env = "TRUSTY_MPM_URL", global = true)]
    pub(crate) url: Option<String>,

    /// Subcommand to run. When absent, the guided default fires (#1708).
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
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
    ///
    /// Two distinct session families live under this command group:
    ///
    ///   Local project sessions (bound to the cwd, daemon-backed):
    ///     start, stop, list, run, output, pause, resume, events, info, …
    ///
    ///   Managed fleet sessions (provisioned in isolated worktrees):
    ///     new, ls, activity, send, answer, decommission, prune-idle, …
    ///
    /// Why (#2116, DOC-35 §2.2/§3.2): session lifecycle verbs are promoted to a
    /// sibling top-level plural noun — `tm sessions` — matching `tm projects`
    /// (epic #2108) rather than folding under either. This reverses the earlier
    /// #1916 rename to the singular `session`: that rename assumed `tm` had no
    /// external users to keep a compatibility alias for, but the owner has since
    /// resolved that the plural is the durable spelling and the singular
    /// (`Command::Session` below) is kept as a hidden deprecated alias instead of
    /// being retired outright, so scripts/skills/muscle memory keep working.
    /// What: the `tm sessions <action>` command group (`tui`, `ls`, `new`, …).
    /// Test: `cli_parses_sessions_*` in `tests.rs` cover the canonical plural;
    /// `cli_parses_session_*` continue to cover the deprecated singular alias.
    Sessions {
        /// Session action to perform.
        #[command(subcommand)]
        action: SessionAction,
    },
    /// [DEPRECATED] Alias of `sessions` (#2116) — use `tm sessions <verb>`.
    ///
    /// Why: `tm session` was the canonical spelling from #1916 until #2116
    /// promoted `sessions` to a sibling top-level plural alongside `tm projects`.
    /// This singular form is kept as a hidden alias (mirroring the
    /// `ManagedStop`/`RuntimeStop`/`ManagedResume` verb-level precedent) so
    /// existing scripts, skills (`tm-session-management`, `tm-session-pause`,
    /// `tm-session-resume`), and muscle memory keep working unchanged.
    /// What: identical dispatch to `Sessions`; `main.rs` prints a one-line
    /// deprecation notice to stderr exactly once per invocation before routing
    /// to the same handler — no functional difference in behavior.
    /// Test: every existing `cli_parses_session_*` test in `tests.rs`;
    /// `tm_session_singular_prints_deprecation_notice_once` (integration test)
    /// asserts the notice fires exactly once.
    #[command(name = "session", hide = true)]
    Session {
        /// Session action to perform.
        #[command(subcommand)]
        action: SessionAction,
    },
    /// [INTERNAL] Spawn a program with macOS TCC responsibility disclaimed
    /// (issue #2997) — not for direct use.
    ///
    /// Why: trusty-mpm's managed sessions launch `claude` by typing a shell
    /// command into a tmux pane, and the pane's shell is forked by the ONE
    /// long-lived, shared tmux server. A `posix_spawn` disclaim attribute set
    /// by the daemon can't reach that `claude`, so tccd walks its responsibility
    /// chain up to the shared tmux server and blames it for the pane's
    /// RemovableVolumes/App-Data access (issue #2997). This hidden subcommand is
    /// the tm-owned process the pane routes `claude` through: it re-`posix_spawn`s
    /// `claude` WITH the disclaim attribute set (reusing the #3037
    /// `disclaimed_status` seam), making `claude` its OWN responsible process so
    /// the tccd walk stops at Claude Code's stable code identity instead of the
    /// tmux server. Hidden because it is an internal launch shim
    /// ([`crate::core::spawn_disclaim::PANE_DISCLAIM_SUBCOMMAND`]), never a
    /// user-facing verb.
    /// What: spawns `argv[0]` with `argv[1..]` via
    /// [`trusty_mpm::core::spawn_disclaim::disclaimed_status`] (inherited stdio),
    /// waits, and exits with the child's status code. On non-macOS the disclaim
    /// is a no-op pass-through to a plain `Command::status()`.
    /// Test: `cli_parses_internal_spawn_disclaimed` (in `tests.rs`) covers the
    /// trailing-argv parse; the spawn behaviour is covered by
    /// `crate::core::spawn_disclaim`'s `disclaimed_status_*` tests.
    #[command(name = "internal-spawn-disclaimed", hide = true)]
    InternalSpawnDisclaimed {
        /// The program to launch, followed by its arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
        argv: Vec<String>,
    },
    /// Manage the project registry (registry B) and its Deliverable/Milestone
    /// ledger.
    ///
    /// Why (#2115/#2381, DOC-35 §3.1/§10.8): `tm projects` is the deterministic
    /// CLI half of the project control plane — a sibling top-level plural noun
    /// alongside `tm sessions` (#2116). Its verbs are thin HTTP clients over the
    /// daemon's registry-B surface (§1.3): `list`/`register`/`show`/`status` cover
    /// the project registry itself, and the `deliverables`/`milestones` subtrees
    /// cover the L3-substrate work-tracking ledger (§10). Mutating session verbs
    /// deliberately live at `tm sessions`; `tm projects show` surfaces sessions
    /// read-only per the naming split.
    ///
    /// `action` stays a MANDATORY clap subcommand (#2118 does not relax this):
    /// a bare, non-interactive `tm projects` must keep failing with clap's own
    /// "requires a subcommand" usage error (exit code 2), unchanged. The
    /// interactive case — bare `tm projects` on a TTY launches the 4-pane
    /// project-control-plane TUI skeleton — is intercepted in `main.rs` BEFORE
    /// `Cli::try_parse()` even runs (see `main.rs`'s module doc and
    /// `commands::projects::launch_bare_tui`), so it never has to touch this
    /// mandatory-subcommand shape at all.
    /// What: the `tm projects <action>` command group.
    /// Test: `cli_parses_projects_*` in `tests_projects.rs`.
    Projects {
        /// Project action to perform.
        #[command(subcommand)]
        action: ProjectsAction,
    },
    /// Layer-3 portfolio manager — cross-project status, digest, and chat
    /// (DOC-36, epic #2109, WI-6 #2583).
    ///
    /// Why: `tm manager` is the thin CLI client over the daemon's
    /// `/api/v1/manager/*` surface (DOC-36 §3.2) — the "reason across the
    /// WHOLE portfolio" layer `tm projects`/`tm sessions` structurally cannot
    /// provide (DOC-35 §11 scopes cross-project synthesis to #2109). Every
    /// verb here is read-only in phase 1: `status` is a pure aggregation (no
    /// LLM), `digest` and `chat` may call an LLM but never mutate a
    /// Deliverable/Milestone or a session (§2.1 boundary).
    /// What: the `tm manager <action>` command group.
    /// Test: `cli_parses_manager_*` in `tests_manager.rs`.
    Manager {
        /// Manager action to perform.
        #[command(subcommand)]
        action: ManagerAction,
    },
    /// Show the recent hook-event feed.
    Events,
    /// Run a full system diagnostic of the trusty-mpm stack.
    Doctor {
        /// Hidden manual escape hatch: force-remove stale pre-rename
        /// `~/.claude/skills/mpm-*` directories.
        ///
        /// Why: the mpm-*→tm-* skill rename (#1905) left orphaned `mpm-*`
        /// skill directories deployed before the rename in place —
        /// `skill_deployer::deploy_skills` never deletes a skill's old
        /// directory when it is renamed. Normally this is cleaned up
        /// automatically and silently by the one-time
        /// [`trusty_mpm::core::stale_skills::run_stale_mpm_skills_migration_once`]
        /// migration on `tm` startup, so this flag exists only as a manual
        /// troubleshooting hatch (e.g. re-running after the migration marker
        /// was somehow cleared) — hidden from `--help` because it is not
        /// part of normal operation.
        /// What: after printing the diagnostic report, scans
        /// `~/.claude/skills/` for trusty-mpm's own frozen pre-rename skill
        /// names (never an unrelated `mpm-*` skill from another tool) and
        /// deletes each one found, printing what was removed.
        #[arg(long, hide = true)]
        prune_stale_skills: bool,
    },
    /// Validate a workspace's deployed `.claude/{agents,skills}` payload and
    /// `settings.json` against the canonical bundled roster (issue #2158).
    ///
    /// Why: `tm doctor` requires a reachable daemon (it round-trips through
    /// `GET /api/v1/doctor`); this command runs the identical diff engine
    /// ([`trusty_mpm::core::deploy_validate::validate_workspace`]) directly
    /// against the local filesystem, so it works standalone (no daemon
    /// needed) and can gate a script/CI step on a non-zero exit code.
    /// What: prints every gap found, or a completeness confirmation, and
    /// exits non-zero when the workspace is incomplete (after an optional
    /// `--repair` attempt still leaves gaps).
    /// Test: `cli_parses_validate`, `cli_parses_validate_with_path_and_repair`.
    Validate {
        /// Workspace directory to validate. Defaults to the current directory.
        #[arg(long)]
        path: Option<std::path::PathBuf>,
        /// Attempt to auto-repair any gaps found by re-running the deploy
        /// pipeline ([`trusty_mpm::core::session_launch::prepare_session_with_repo_url`])
        /// before reporting the final verdict.
        #[arg(long)]
        repair: bool,
    },
    /// Project-settings hook hygiene: detect and remove tm hook contamination
    /// (issue #2940).
    ///
    /// Why: `tm install` used to wire tm hooks into every discovered
    /// project's `.claude/settings.json`/`settings.local.json` on the
    /// machine, which was a one-way street away from other harnesses and
    /// could conflict with a project's own claude-mpm hooks. Hooks now live
    /// solely in the tm-owned `CLAUDE_CONFIG_DIR`
    /// (see [`trusty_mpm::core::trusty_tools_config::managed_claude_config_dir`]);
    /// this command reverses damage a pre-fix `tm install` already did.
    /// What: `clean` scans (dry-run by default) for tm-owned hook entries in
    /// project settings files and removes them with `--force` (backing up
    /// each modified file first). See [`HooksAction::Clean`]'s doc comment
    /// for the full behavior.
    /// Test: `cli_parses_hooks_clean`, `cli_parses_hooks_clean_with_path_and_force`.
    Hooks {
        /// Hooks action to perform.
        #[command(subcommand)]
        action: HooksAction,
    },
    /// Inspect the deployed agent roster's declared skills (DOC-42, issue #2889).
    ///
    /// Why: `skills:` frontmatter declares which skills an agent depends on
    /// (`docs/specs/agent-bundled-skills.md` §SPEC-AGENTSKILLS-01); this
    /// command surfaces those declarations and their 3-tier resolution so an
    /// operator debugging a missing-skill error can see the gap without
    /// grepping deployed `.md` files by hand.
    /// What: `list` prints every deployed agent with its declared skills;
    /// `show <name>` prints one agent's full metadata plus each declared
    /// skill's resolved tier (or `NOT FOUND`). Reads the operator's deployed
    /// roster (`~/.claude/agents/`, `~/.claude/skills/`) directly — no daemon
    /// round-trip needed.
    /// Test: `cli_parses_agent_list`, `cli_parses_agent_show`.
    Agent {
        /// Agent action to perform.
        #[command(subcommand)]
        action: AgentAction,
    },
    /// Regenerate derived, committed artifacts from live harness surfaces
    /// (issue #2913).
    ///
    /// Why: a dev-time codegen namespace, following the same "generate,
    /// commit, CI-diff" pattern as `scripts/check_line_cap.sh`'s
    /// `--update`/ratchet convention — never a build.rs and never
    /// deploy-time-dynamic (see `tm-capabilities`'s provenance banner for the
    /// rationale).
    /// What: `capabilities` regenerates the `tm-capabilities` bundled skill;
    /// see [`GenerateAction`].
    /// Test: `cli_parses_generate_capabilities`, `cli_parses_generate_capabilities_check`.
    Generate {
        /// Generate action to perform.
        #[command(subcommand)]
        action: GenerateAction,
    },
    /// Report daemon health: reachability, catalog freshness, and a fleet summary.
    Health,
    /// Launch the ratatui multi-session TUI dashboard.
    Tui {
        /// Base URL of the trusty-mpm daemon. `Option<String>`, no
        /// `default_value` (#2487) — resolved via `resolve_daemon_url`, which
        /// applies the lock-file / compiled-in-default fallback itself.
        #[arg(long, env = "TRUSTY_MPM_URL")]
        url: Option<String>,
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
    /// Manage the Slack remote-management bot (start, stop) — DOC-20 adapter #1294.
    ///
    /// Why: Slack is a peer control surface to the Telegram bot (DOC-18 §ONB-3),
    /// driving the managed fleet through the SAME chat-core nucleus. It connects
    /// in Socket Mode (no public webhook required), so the operator only needs a
    /// Slack app with a bot token + app-level token.
    /// What: `start` runs the Socket-Mode bot in the foreground; `stop` kills it.
    /// Test: `cli_parses_slack_start`, `cli_parses_slack_stop`.
    Slack {
        /// Slack action to perform.
        #[command(subcommand)]
        cmd: SlackCmd,
    },
    /// Install the bundled framework artifacts to `~/.trusty-mpm/framework/`.
    ///
    /// `--reset-agents` (issue #2504) is the explicit reconciliation path for
    /// composed agent files that predate per-file manifest tracking: a
    /// normal deploy conservatively skips any target file absent from the
    /// manifest (it might be user-owned), which meant such files could never
    /// receive bundle updates again. Passing `--reset-agents` with no names
    /// force-recomposes every bundled agent; passing a comma-separated list
    /// restricts the reset to those agents. Content that cannot be proven to
    /// already be trusty-mpm's own prior output is backed up (`<file>.bak-
    /// <unix_nanos>`) before being overwritten — nothing is silently lost.
    ///
    /// `--reset-agents-workspaces` (issue #2508) extends the SAME reset to
    /// every intact session workspace's PROJECT-LOCAL `.claude/agents/` —
    /// without it, `--reset-agents` only ever reconciles the USER-LEVEL
    /// `~/.claude/agents/` copy, leaving every already-provisioned session
    /// worktree stale. Each workspace is reset through its OWN resolved
    /// harness plan, so an agent that workspace's manifest deliberately
    /// excludes is never resurrected (issue #2462's `[agents].exclude` roster
    /// filter). Requires `--reset-agents` (there is nothing "workspace" to
    /// reset without it).
    Install {
        /// Overwrite artifacts that already exist on disk.
        #[arg(long)]
        force: bool,
        /// Force-recompose agent files from the current bundle (issue #2504).
        /// With no value, resets every bundled agent. Pass a comma-separated
        /// list (e.g. `--reset-agents engineer,qa`) to restrict the scope.
        #[arg(long, num_args = 0.., value_delimiter = ',')]
        reset_agents: Option<Vec<String>>,
        /// Also reset every intact session workspace's project-local
        /// `.claude/agents/` (issue #2508). Requires `--reset-agents`.
        #[arg(long, requires = "reset_agents")]
        reset_agents_workspaces: bool,
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
    ///
    /// `--pm-guard` (issue #1977) switches this invocation into PM-enforcement
    /// mode instead of the observability relay: it reads the `PreToolUse` stdin
    /// payload, classifies the tool locally (no daemon round-trip), and emits a
    /// `permissionDecision: "deny"` response for direct file edits / forbidden
    /// Bash verbs the PM must delegate — see `commands::pm_guard`. The managed
    /// launcher registers exactly this form as the session's `PreToolUse` hook.
    /// Test: `cli_parses_hook` / `cli_parses_hook_pm_guard` plus the inline
    /// `hook_guard_short_circuits`.
    Hook {
        /// Run in PM-enforcement mode (blocks direct edits; steers to delegate).
        #[arg(long)]
        pm_guard: bool,
    },
    /// Compress a piped command's stdout — the `tm hook` PreToolUse Bash
    /// command-rewrite spike's filter stage (issue #1956, Option 0).
    ///
    /// Why: `tm hook` rewrites a Bash tool call's command to
    /// `<original> | tm compress --tool "<effective tool name>"` (the tool
    /// name is derived from the wrapped command — e.g. `"cargo test"`,
    /// `"git diff"` — not a hardcoded `"bash"`, see
    /// `commands::hook_rewrite::effective_tool_name`) so Claude Code's own
    /// subprocess execution produces already-compressed output before the
    /// model ever sees the raw payload — see
    /// `docs/specs/tool-output-interception-seam.md` §Option 0.
    /// What: reads all of stdin, compresses it via
    /// `trusty_agents_common::compress::compress_tool_output_async_with_path`
    /// (hoisted from `trusty-agents` in issue #1959), logs a structured
    /// stats line, and writes the compressed text to stdout.
    /// Test: `cli_parses_compress` plus `commands::compress`'s unit tests
    /// and the `tm_compress_pipe` integration test.
    Compress {
        /// Tool name used to select the compression filter (e.g. "cargo test").
        #[arg(long)]
        tool: String,
    },
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
        /// Active output style id for this launch (HR-4). Overrides the
        /// `[style] active` config key. Bundled ids: `trusty-mpm` (default),
        /// `trusty-mpm-teacher`, `trusty-mpm-research`. An unknown id falls back
        /// to the default.
        #[arg(long)]
        style: Option<String>,
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

    /// Manage the tm-managed `CLAUDE_CODE_OAUTH_TOKEN` store (issue #2246).
    ///
    /// Why: managed sessions run `claude` under a relocated `CLAUDE_CONFIG_DIR`
    /// (`~/.trusty-tools/trusty-mpm/claude-config/`). On macOS, Claude Code's
    /// primary OAuth login lives in the Keychain under an entry keyed by a
    /// hash of `CLAUDE_CONFIG_DIR` — so a `/login` run inside a managed
    /// session diverges from the login stored under the operator's default
    /// config dir, producing a "login successful" then immediately
    /// "not logged in" loop. `CLAUDE_CODE_OAUTH_TOKEN` bypasses the Keychain
    /// entirely; this command manages the tm-owned store the daemon injects
    /// it from. Get a token via `claude setup-token`, then store it here.
    /// What: `set-token` stores a token (0600) at
    /// `~/.trusty-tools/trusty-mpm/claude-code-oauth.token` — read from stdin
    /// by default, or `--token <val>` (avoid the latter in a shared shell
    /// history); `clear-token` removes it; `status` reports presence/absence
    /// of the stored token, the `CLAUDE_CODE_OAUTH_TOKEN`/`ANTHROPIC_API_KEY`
    /// env vars, and a best-effort on-disk credentials check — NEVER the
    /// token value itself.
    /// Test: `cli_parses_auth_set_token`, `cli_parses_auth_clear_token`,
    /// `cli_parses_auth_status`.
    Auth {
        /// Auth action to perform.
        #[command(subcommand)]
        action: AuthAction,
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

    /// Register a GitHub repo alias for the standalone managed driver (DOC-24).
    ///
    /// Why: declares an alias→URL mapping without cloning so users can register
    /// their fleet cheaply and `tm load <alias>` lazily.
    /// What: persists `{alias, url}` to `<root>/registry.json` and
    /// prints `registered <alias> → <url>`.
    /// Test: `cli_parses_register`.
    Register {
        /// Short alias identifier (e.g. `my-project`).
        alias: String,
        /// Clone-able GitHub URL (HTTPS or SSH).
        url: String,
        /// Overwrite an existing alias with a different URL.
        #[arg(long)]
        force: bool,
        /// Override the managed root (default: `~/.trusty-mpm`).
        ///
        /// Precedence: this flag > `TRUSTY_MPM_ROOT` env var >
        /// `[standalone] root` in `$XDG_CONFIG_HOME/trusty-mpm/config.toml` >
        /// default `~/.trusty-mpm`.
        ///
        /// Note: do NOT use `env = "TRUSTY_MPM_ROOT"` here. The env var is
        /// read as tier-2 inside `resolve_managed_paths`; binding it to this
        /// arg would promote it to tier-1 (CLI-flag) and silently skip the
        /// config-file tier whenever the env var is set.
        #[arg(long)]
        root: Option<String>,
    },

    /// Interactive managed-session connector — list sessions and connect (#2311).
    ///
    /// Why: bare `tm ls` should do the most useful fleet action for the operator's
    /// current context. On a real terminal it opens the interactive session picker
    /// (the same numbered menu as bare `tm`, but scoped to the managed fleet), so
    /// resuming a session is one keystroke away. Piped, scripted, or `--json`
    /// invocations degrade to the same static, pipeable list as `tm sessions ls`,
    /// never blocking on stdin. The former top-level `tm ls` (the DOC-24 alias /
    /// project-registry list) now lives behind `--projects`/`-p`.
    /// What: with `--projects`/`-p`, prints the local-project + managed-fleet alias
    /// registry (`--json` = combined JSON, `--root` overrides the managed root).
    /// Without `--projects`, it is the session connector: a TTY with ≥1 session
    /// opens the picker; `--json`, `--all`, a non-TTY, or 0 sessions print the
    /// static table. `--source-id`/`--current`/`--all` mirror `tm sessions ls`.
    /// Test: `cli_parses_ls_connector_bare`, `cli_parses_ls_projects`,
    /// `cli_parses_ls_projects_short`, `cli_parses_ls_json`, `cli_parses_ls_current`,
    /// `cli_ls_source_id_and_current_conflict`.
    #[command(name = "ls")]
    Ls {
        /// Show the repo-alias / project registry instead of managed sessions.
        ///
        /// Why (#2311): preserves the pre-connector top-level `tm ls` behavior —
        /// the DOC-24 local-project + managed-fleet alias list — as an explicit,
        /// discoverable opt-in now that bare `tm ls` is the session connector.
        /// With `--projects`, `--json` prints the combined alias JSON and `--root`
        /// selects the managed root; the session flags below are ignored.
        #[arg(long, short = 'p')]
        projects: bool,
        /// Output as JSON. With `--projects`: the combined alias JSON array.
        /// Without `--projects`: the raw daemon managed-session JSON (byte-for-byte
        /// passthrough), forcing static output even on a TTY.
        #[arg(long)]
        json: bool,
        /// Filter sessions to this `owner/repo` slug (session mode only).
        ///
        /// Passed to the daemon as `?source_id=`; mirrors `tm sessions ls`.
        #[arg(long)]
        source_id: Option<String>,
        /// Derive `source_id` from the cwd's git remote (session mode only).
        ///
        /// Mutually exclusive with `--source-id`; passing both is a parse error.
        #[arg(long, conflicts_with = "source_id")]
        current: bool,
        /// Include decommissioned tombstone sessions (session mode only).
        ///
        /// Forces static output (the full forensic list is not a connect target).
        #[arg(long)]
        all: bool,
        /// Override the managed root (default: `~/.trusty-mpm`). `--projects` only.
        ///
        /// Precedence: this flag > `TRUSTY_MPM_ROOT` env var >
        /// `[standalone] root` in `$XDG_CONFIG_HOME/trusty-mpm/config.toml` >
        /// default `~/.trusty-mpm`.
        ///
        /// Note: do NOT use `env = "TRUSTY_MPM_ROOT"` here — see `Register`.
        #[arg(long)]
        root: Option<String>,
    },

    /// Clone or refresh the managed workspace for a registered alias (DOC-24).
    ///
    /// Why: `load` is the idempotent step that materializes a registered alias
    /// into a fully-configured project directory.
    /// What: clones the repo (or fast-forward-pulls if it exists), runs
    /// `prepare_session`, writes the managed marker, and prints the repo path.
    /// Test: `cli_parses_load_standalone`.
    Load {
        /// Registered alias to load.
        alias: String,
        /// Override the managed root (default: `~/.trusty-mpm`).
        ///
        /// Precedence: this flag > `TRUSTY_MPM_ROOT` env var >
        /// `[standalone] root` in `$XDG_CONFIG_HOME/trusty-mpm/config.toml` >
        /// default `~/.trusty-mpm`.
        ///
        /// Note: do NOT use `env = "TRUSTY_MPM_ROOT"` here — see `Register`.
        #[arg(long)]
        root: Option<String>,
    },

    /// Launch an interactive `claude` session for a managed alias (DOC-24).
    ///
    /// Why: `tm run` is the claude-mpm replacement — it launches Claude Code
    /// with `CLAUDE_CONFIG_DIR=<root>/claude-config` so the global
    /// hooks/MCPs are supplied and the real `~/.claude` is excluded.
    /// What: loads the alias if needed, checks credentials, and spawns `claude`
    /// with inherited stdio.
    /// Test: `cli_parses_run_standalone`.
    Run {
        /// Registered alias to run.
        alias: String,
        /// Optional initial task to pre-seed (currently unused by MVP).
        #[arg(long)]
        task: Option<String>,
        /// Override the managed root (default: `~/.trusty-mpm`).
        ///
        /// Precedence: this flag > `TRUSTY_MPM_ROOT` env var >
        /// `[standalone] root` in `$XDG_CONFIG_HOME/trusty-mpm/config.toml` >
        /// default `~/.trusty-mpm`.
        ///
        /// Note: do NOT use `env = "TRUSTY_MPM_ROOT"` here — see `Register`.
        #[arg(long)]
        root: Option<String>,
    },

    /// Print the stable repo path for a loaded alias (DOC-24 IDE-attach).
    ///
    /// Why: `tm path <alias>` lets IDEs, scripts, or `cd $(tm path <alias>)`
    /// open the project directory without running a session.
    /// What: prints the absolute path to `<root>/projects/<alias>/repo/`
    /// when the alias is loaded.
    /// Test: `cli_parses_path_standalone`.
    Path {
        /// Registered and loaded alias.
        alias: String,
        /// Override the managed root (default: `~/.trusty-mpm`).
        ///
        /// Precedence: this flag > `TRUSTY_MPM_ROOT` env var >
        /// `[standalone] root` in `$XDG_CONFIG_HOME/trusty-mpm/config.toml` >
        /// default `~/.trusty-mpm`.
        ///
        /// Note: do NOT use `env = "TRUSTY_MPM_ROOT"` here — see `Register`.
        #[arg(long)]
        root: Option<String>,
    },

    /// One-time keychain login for managed `tm run` sessions (WI-10, DOC-24).
    ///
    /// Why: `CLAUDE_CONFIG_DIR` relocates the macOS Keychain entry used for
    /// Claude Max/Pro OAuth (A9). A fresh managed `claude-config/` has no
    /// keychain entry, so `tm run` reports "Not logged in". `tm login` runs
    /// `claude auth login` under the tm-global `CLAUDE_CONFIG_DIR` so the
    /// OAuth flow creates a keychain entry for that path. This is a one-time
    /// setup — the entry persists across sessions on this machine.
    /// What: ensures the tm-global config dir exists, spawns
    /// `claude auth login` with `CLAUDE_CONFIG_DIR=<root>/claude-config`
    /// and inherited stdio, prints guidance before/after the OAuth flow.
    /// Alternative: set `ANTHROPIC_API_KEY` to use the API-key+`--bare` path
    /// instead (for CI/automation, no login required).
    /// Test: `cli_parses_login_standalone`.
    Login {
        /// Override the managed root (default: `~/.trusty-mpm`).
        ///
        /// Precedence: this flag > `TRUSTY_MPM_ROOT` env var >
        /// `[standalone] root` in `$XDG_CONFIG_HOME/trusty-mpm/config.toml` >
        /// default `~/.trusty-mpm`.
        ///
        /// Note: do NOT use `env = "TRUSTY_MPM_ROOT"` here — see `Register`.
        #[arg(long)]
        root: Option<String>,
    },

    /// Remove a managed alias: deregister and delete its project dir (DOC-24).
    ///
    /// Why: completes the lifecycle — operators need a clean teardown verb that
    /// removes the cloned repo and registry entry without touching the shared
    /// CLAUDE_CONFIG_DIR (which is shared across all aliases).
    /// What: deregisters `<alias>` from `registry.json` and removes
    /// `<root>/projects/<alias>/`. The shared `<root>/claude-config/` is
    /// untouched. Errors clearly if the alias is unknown.
    /// Test: `cli_parses_rm_standalone`.
    #[command(name = "rm")]
    Rm {
        /// Registered alias to remove.
        alias: String,
        /// Override the managed root (default: `~/.trusty-mpm`).
        ///
        /// Note: do NOT use `env = "TRUSTY_MPM_ROOT"` here — see `Register`.
        #[arg(long)]
        root: Option<String>,
    },

    /// Refresh a loaded alias — pull latest and re-deploy managed config (DOC-24).
    ///
    /// Why: operators need a way to bring a loaded project up to date without
    /// re-running `tm rm && tm load`. `tm update` is the idempotent refresh verb.
    /// What: for each target alias, runs `git pull --ff-only` on its repo then
    /// re-runs the same managed-config deploy as `load` (idempotent). With no
    /// alias, updates ALL registered aliases whose project dir exists (loaded).
    /// If the alias is registered but not yet loaded, errors with a hint to run
    /// `tm load <alias>` first.
    /// Test: `cli_parses_update_standalone`, `cli_parses_update_all_standalone`.
    Update {
        /// Registered alias to update; omit to update all loaded aliases.
        alias: Option<String>,
        /// Override the managed root (default: `~/.trusty-mpm`).
        ///
        /// Note: do NOT use `env = "TRUSTY_MPM_ROOT"` here — see `Register`.
        #[arg(long)]
        root: Option<String>,
    },

    /// Manage SESSCTL control-plane sessions (WI-2 #1593).
    ///
    /// Why: the Phase-2 implementation replaces the flat `sessctl-run` command
    /// with a proper subcommand group that mirrors the daemon HTTP API surface
    /// (`run`, `connect`, `stop`, `auth`, `list`).
    /// What: dispatches to daemon HTTP endpoints under
    /// `/api/v1/control/sessions/*`.
    /// Test: `cli_parses_sessctl_run`, `cli_parses_sessctl_connect`,
    /// `cli_parses_sessctl_stop`, `cli_parses_sessctl_auth`,
    /// `cli_parses_sessctl_list` in `tests.rs`.
    #[command(name = "sessctl")]
    Sessctl {
        /// SESSCTL action to perform.
        #[command(subcommand)]
        action: SessctlAction,
    },

    /// Standalone metaharness — PM + sub-agent delegation without the daemon (#1045).
    ///
    /// Why: the M1 POC (issue #1045) builds a self-contained metaharness that
    /// boots without the trusty-mpm daemon, driving a REAL Claude Code (`claude`
    /// CLI) session through trusty-mpm's existing launch machinery (#1049/#1051).
    /// The `meta` command group is the operator entry point for that harness.
    /// `meta run --project <dir>` deploys the custom instructions (agents, skills,
    /// CLAUDE.md, PM prompt, MCP) and launches a real `claude` tmux session rooted
    /// at that directory; `meta run --demo` additionally attaches a bundled task,
    /// polls for the session to exit, and verifies the demo artifact.
    /// What: a `run` sub-subcommand that accepts `--demo` (attach + verify the
    /// bundled demo task), `--project <PATH>` (the working directory the harness
    /// operates in), `--no-provision` (a current no-op reserving the future
    /// provisioned/clone seam — the POC always uses the local dir in place), and
    /// `--timeout-secs <N>` (session-exit poll budget).
    /// Test: `cli_parses_meta_run`, `cli_parses_meta_run_demo`,
    /// `cli_parses_meta_run_project`, `cli_parses_meta_run_no_provision`,
    /// `cli_parses_meta_run_timeout`, `cli_meta_requires_action` in `tests.rs`.
    Meta {
        /// Metaharness action to run.
        #[command(subcommand)]
        action: MetaAction,
    },

    /// Read Claude Code statusLine JSON from stdin and print one compact line.
    ///
    /// Why: Claude Code's `statusLine` hook calls this command on every render
    /// cycle; this handler parses the hook JSON and emits one compact segment
    /// string for the status bar.
    /// What: reads a JSON object from stdin, renders segments (project, model,
    /// daemon, cost), exits 0. Missing or invalid fields degrade gracefully.
    /// Test: `cli_parses_statusline` in `tests.rs`.
    Statusline,

    /// Preview the launch banner in the current terminal without starting Claude.
    ///
    /// Why: operators and developers need a way to eyeball the colored robot
    /// art, TRUSTY wordmark, and rich welcome panel without running a full
    /// `tm launch` session. The `--reconnecting` flag renders the alternate
    /// reconnect variant (same banner but with the reconnecting status row).
    /// What: renders the robot splash (without the full-screen clear, so
    /// scrollback is preserved) followed by the info-box welcome panel (without
    /// the 1-second sleep). Service/daemon data reflects the current environment
    /// — graceful when the daemon is not running.
    ///
    /// Banner art override (precedence: env var > persistent file > built-in default):
    ///   • `TRUSTY_MPM_BANNER_FILE=<path>` — one-shot override via env var.
    ///   • `~/.trusty-mpm/banner.txt` — persistent user-editable file (seeded
    ///     from the embedded default on first run; edit freely).
    ///
    /// Test: `cli_parses_banner`, `cli_parses_banner_reconnecting` in `tests.rs`.
    Banner {
        /// Preview the reconnect variant (shows the "reconnecting" status row).
        #[arg(long)]
        reconnecting: bool,
    },

    /// Register user-level MCP servers into tm's OWN tm-owned CLAUDE_CONFIG_DIR.
    ///
    /// Why: stock `claude mcp add` cannot target tm's managed config dir, so there
    /// was no ergonomic way to add an MCP server that EVERY tm-managed session
    /// sees. `tm mcp` edits the top-level `mcpServers` map of that dir's
    /// `.claude.json` — the true "available in all your projects" USER scope. This
    /// command is inherently user-scope; there is deliberately no `--scope` flag.
    /// What: `add`/`remove`/`list`/`get` subcommands. By default they target the
    /// daemon-managed config dir (`~/.trusty-tools/trusty-mpm/claude-config/`);
    /// `--root <path>` switches to the standalone dir via the same precedence
    /// chain `tm register`/`tm ls` use (`--root` > `TRUSTY_MPM_ROOT` >
    /// `[standalone] root` config > `~/.trusty-mpm`).
    /// Test: `cli_parses_mcp_*` in `tests.rs`; logic in `commands::mcp`.
    Mcp {
        /// MCP server registry action.
        #[command(subcommand)]
        cmd: McpCmd,
    },

    /// Manage inference provider configuration (API keys) — the universal
    /// `config keys set/list/test/unset` surface shared by every trusty-*
    /// binary (epic #2400 Wave 1, #2405).
    ///
    /// Boundary note: this is the inference-provider credential store, distinct
    /// from tm's MCP-native `config_read`/`config_write` daemon config tools —
    /// different domain, deliberately not merged.
    Config(trusty_common::inference::config::ConfigCommand),
}
