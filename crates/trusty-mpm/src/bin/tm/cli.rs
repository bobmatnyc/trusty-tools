//! CLI argument definitions for the `trusty-mpm` / `tm` binary.
//!
//! Why: keeping all clap struct/enum definitions in one file makes it easy to
//! audit the full CLI surface without wading through handler logic.
//! What: defines `Cli`, `Command`, and all sub-command enums.
//! Test: each variant is exercised by the `cli_parses_*` unit tests in
//! `tests.rs`.

use std::net::SocketAddr;

use clap::{Parser, Subcommand};

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

/// Verbs for the `tm mcp` user-scope MCP-server registry command group.
///
/// Why: mirrors the `claude mcp add|remove|list|get` UX so operators moving
/// between the stock CLI and tm get the same surface, while pointing writes at
/// tm's managed config dir instead of `~/.claude.json`.
/// What: `Add` upserts a stdio/http/sse server, `Remove` drops one, `List`
/// enumerates them, `Get` shows one. Every verb accepts `--root` to switch from
/// the daemon-managed dir to a standalone root.
/// Test: `cli_parses_mcp_add`, `cli_parses_mcp_add_http`, `cli_parses_mcp_remove`,
/// `cli_parses_mcp_list`, `cli_parses_mcp_get` in `tests.rs`.
#[derive(Debug, Subcommand)]
pub(crate) enum McpCmd {
    /// Add (or replace) a user-scope MCP server.
    ///
    /// stdio (default): `tm mcp add <name> [-e KEY=VAL]... <command> [-- <args>...]`
    /// http/sse:        `tm mcp add <name> -t http [-H "K: V"]... <url>`
    Add {
        /// Server name (the `mcpServers` key).
        name: String,
        /// Transport: `stdio` (local subprocess), `http`, or `sse` (remote).
        #[arg(short = 't', long = "transport", value_enum, default_value_t = McpTransportArg::Stdio)]
        transport: McpTransportArg,
        /// Environment variable for a stdio server (repeatable): `KEY=VALUE`.
        #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,
        /// HTTP header for an http/sse server (repeatable): `Name: Value`.
        #[arg(short = 'H', long = "header", value_name = "NAME: VALUE")]
        header: Vec<String>,
        /// The command/URL followed by any stdio subprocess args, mirroring
        /// `claude mcp add`. The FIRST token is the command (stdio) or URL
        /// (http/sse); the rest are subprocess args (stdio only). A leading `--`
        /// stops flag parsing so hyphen-led args pass through
        /// (e.g. `-- npx -y some-pkg`). All `-t`/`-e`/`-H`/`--root` options must
        /// precede this token.
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "COMMAND_OR_URL [ARGS...]"
        )]
        command_and_args: Vec<String>,
        /// Override the managed root (switches to the standalone config dir).
        ///
        /// Note: do NOT use `env = "TRUSTY_MPM_ROOT"` here — see `Register`.
        #[arg(long)]
        root: Option<String>,
    },
    /// Remove a user-scope MCP server by name.
    Remove {
        /// Server name to remove.
        name: String,
        /// Override the managed root (switches to the standalone config dir).
        #[arg(long)]
        root: Option<String>,
    },
    /// List all user-scope MCP servers in the tm config dir.
    List {
        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Override the managed root (switches to the standalone config dir).
        #[arg(long)]
        root: Option<String>,
    },
    /// Show one user-scope MCP server's definition.
    Get {
        /// Server name to show.
        name: String,
        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Override the managed root (switches to the standalone config dir).
        #[arg(long)]
        root: Option<String>,
    },
    /// Verify MCP servers by running a real handshake against each.
    ///
    /// Bare (`tm mcp test`): sweeps every user-scope server unioned with the
    /// three framework built-ins. `tm mcp test <name>`: tests just that server.
    /// stdio servers get a full `initialize` → `tools/list` handshake (reporting
    /// the tool count); http/sse servers get an HTTP reachability check. Exits
    /// non-zero if ANY tested server fails, so it is CI-usable.
    Test {
        /// Optional server name; omit to sweep all servers + built-ins.
        name: Option<String>,
        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Override the managed root (switches to the standalone config dir).
        #[arg(long)]
        root: Option<String>,
    },
}

/// Transport choice for `tm mcp add` (clap `ValueEnum` mirror of
/// [`trusty_mpm::core::mcp_config::McpTransport`]).
///
/// Why: clap needs a `ValueEnum` for `-t/--transport`; keeping it a thin mirror
/// of the core enum avoids leaking clap into the library crate.
/// What: `Stdio` | `Http` | `Sse`.
/// Test: `cli_parses_mcp_add_http` exercises the non-default value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum McpTransportArg {
    /// Local subprocess speaking MCP over stdio.
    Stdio,
    /// Remote streamable-HTTP endpoint.
    Http,
    /// Remote SSE endpoint.
    Sse,
}

/// Verbs for the `tm sessctl` SESSCTL control-plane command group (WI-2 #1593).
///
/// Why: Phase 2 exposes the full session lifecycle over the daemon HTTP API;
/// a sub-subcommand group makes each verb discoverable and individually
/// parseable without growing the top-level command surface.
/// What: `Run` spawns a session, `Connect` streams SSE events, `Stop` sends
/// a stop command, `Auth` polls the auth state, `List` enumerates sessions.
/// Test: `cli_parses_sessctl_*` in `tests.rs`.
#[derive(Debug, Subcommand)]
pub(crate) enum SessctlAction {
    /// Spawn a SESSCTL session for a project via the daemon HTTP API.
    ///
    /// Why: delegates session spawning to the daemon so a single shared
    /// registry owns all live sessions.
    /// What: POSTs to `POST /api/v1/control/sessions/run` and prints the
    /// allocated session ID.
    /// Test: `cli_parses_sessctl_run`.
    Run {
        /// Registered project ID.
        project_id: String,
        /// Use the tmux backend instead of stream-JSON.
        #[arg(long)]
        tmux: bool,
        /// Path to a system-prompt file (`--append-system-prompt-file`).
        #[arg(long)]
        prompt_file: Option<String>,
        /// Working directory override (daemon resolves from project-id by default).
        #[arg(long)]
        workdir: Option<String>,
    },
    /// Connect to a session (writer if write-lock available, observer otherwise).
    ///
    /// Why: streams SSE events from the daemon so the CLI can display live
    /// session output without embedding the registry.
    /// What: POSTs to `POST /api/v1/control/sessions/{id}/connect` and prints
    /// each event to stdout.
    /// Test: `cli_parses_sessctl_connect`.
    Connect {
        /// SESSCTL session ID (e.g. `my-proj-0`).
        session_id: String,
    },
    /// Stop a session (graceful by default, --force for immediate).
    ///
    /// Why: mirrors `tm sessions stop` for the SESSCTL surface.
    /// What: POSTs to `POST /api/v1/control/sessions/{id}/stop?force=<bool>`.
    /// Test: `cli_parses_sessctl_stop`.
    Stop {
        /// SESSCTL session ID.
        session_id: String,
        /// Send ForceStop instead of graceful Stop.
        #[arg(long)]
        force: bool,
    },
    /// Show the auth state for a session.
    ///
    /// Why: lets the operator poll for `awaiting-auth` without subscribing to
    /// the full SSE stream.
    /// What: GETs `GET /api/v1/control/sessions/{id}/auth` and prints the result.
    /// Test: `cli_parses_sessctl_auth`.
    Auth {
        /// SESSCTL session ID.
        session_id: String,
    },
    /// List all SESSCTL sessions.
    ///
    /// Why: provides a table or JSON view of every live control-plane session.
    /// What: GETs `GET /api/v1/control/sessions?project=<filter>` and renders
    /// the result as a table or JSON.
    /// Test: `cli_parses_sessctl_list`.
    #[command(name = "list")]
    List {
        /// Filter by project ID.
        #[arg(long)]
        project: Option<String>,
        /// Output format: `table` (default) or `json`.
        #[arg(long, default_value = "table", value_parser = ["table", "json"])]
        format: String,
    },
}

/// Verbs for the `tm meta` standalone-metaharness command group (#1045).
///
/// Why: `meta` is a group rather than a bare command because future work items
/// will add sibling verbs (e.g. inspecting transcripts under
/// `.trusty-mpm/meta-runs/`); modelling it as a sub-subcommand enum from the
/// start keeps that surface extensible without a breaking CLI change.
/// What: a single `Run` variant carrying the `--demo` flag, the optional
/// `--project <PATH>` working-directory argument, the `--no-provision` flag, and
/// the `--timeout-secs <N>` session-exit poll budget.
/// Test: `cli_parses_meta_run`, `cli_parses_meta_run_demo`,
/// `cli_parses_meta_run_project`, `cli_parses_meta_run_no_provision`,
/// `cli_parses_meta_run_timeout`, `cli_meta_requires_action` in `tests.rs`.
#[derive(Debug, Subcommand)]
pub(crate) enum MetaAction {
    /// Boot the metaharness for a single run.
    ///
    /// Why: this is the harness's primary entry point — it deploys the custom
    /// instructions and launches a REAL `claude` tmux session rooted at the
    /// project dir (#1049). With `--demo` (#1051) it additionally attaches a
    /// bundled task instructing the session to write `hello_metaharness.txt`,
    /// polls for the session to exit, and verifies the artifact, exiting 0 on
    /// success and non-zero on failure/timeout.
    /// What: `--demo` attaches + verifies the bundled demo task; `--project
    /// <PATH>` sets the working directory (defaults to the cwd); `--no-provision`
    /// is a CURRENT NO-OP (the POC always uses the `--project` dir in place, so
    /// there is no provisioning/clone step to skip — the flag reserves that future
    /// seam); `--timeout-secs <N>` bounds the session-exit poll (default
    /// [`super::commands::meta::DEFAULT_TIMEOUT_SECS`]).
    /// Test: `cli_parses_meta_run*` in `tests.rs`; handler behaviour in the
    /// `commands::meta` unit tests.
    Run {
        /// Attach + verify the bundled demo task (writes hello_metaharness.txt).
        #[arg(long)]
        demo: bool,

        /// Working directory the metaharness operates in (defaults to the cwd).
        #[arg(long)]
        project: Option<std::path::PathBuf>,

        /// Use the local `--project` dir in place (CURRENTLY A NO-OP).
        ///
        /// The POC always operates on the local `--project` dir directly, so
        /// there is no provisioning / git-clone step to skip — passing this flag
        /// (or not) changes nothing today. It is kept to make the in-place intent
        /// explicit and to reserve the seam for a future provisioned/clone path.
        #[arg(long)]
        no_provision: bool,

        /// Seconds to wait for the launched session to exit before timing out.
        #[arg(long)]
        timeout_secs: Option<u64>,
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
/// Why: catalog management splits into a remote-sync operation, a local listing,
/// a staleness check, and the rebuild/redeploy `apply`; separate sub-actions keep
/// each scriptable.
/// What: `Sync` fetches the catalog (respecting a TTL unless `--force`); `Ls`
/// lists cached agents and skills; `Status` reports whether deployed content is
/// stale; `Apply` syncs then redeploys the manifest-selected content (the HR-3
/// rebuild offer made concrete).
/// Test: `cli_parses_catalog_sync`, `cli_parses_catalog_ls`,
/// `cli_parses_catalog_status`, `cli_parses_catalog_apply`.
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
    /// Report whether the deployed content has drifted from the synced catalog.
    ///
    /// Why: the HR-3 staleness check surfaced as a scriptable CLI verb — the same
    /// signal `GET /health` and the TUI use, without mutating anything.
    /// What: compares the deployed checksum manifests against the synced catalog
    /// and prints stale/unknown plus a per-artifact change list.
    /// Test: `cli_parses_catalog_status`.
    Status {
        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Rebuild/redeploy the manifest-selected content from the catalog (HR-3).
    ///
    /// Why: the HR-3 rebuild OFFER made actionable — accepting it syncs the
    /// catalog then redeploys agents/skills from it, clearing staleness. Never
    /// runs automatically; the operator (or the TUI hint) invokes it explicitly.
    /// What: syncs (honouring the TTL unless `--force`), redeploys the
    /// manifest-selected agents and skills (updating the checksum manifests), and
    /// with `--prune` removes managed agents/skills the manifest no longer selects.
    /// Test: `cli_parses_catalog_apply`; behaviour by `tests/catalog_apply.rs`.
    Apply {
        /// Force a catalog fetch even if the cache TTL has not expired.
        #[arg(long)]
        force: bool,
        /// Also remove managed agents/skills the manifest no longer selects.
        #[arg(long)]
        prune: bool,
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
    /// Why: a crash during `tm install` or `tm sessions start` may leave stale
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

/// Actions for the `auth` subcommand (issue #2246).
///
/// Why: scoped sub-actions keep the token lifecycle (store/remove/inspect)
/// under one command group without cluttering the top-level CLI surface.
/// What: `SetToken`, `ClearToken`, `Status` — see [`Command::Auth`]'s doc
/// comment for the full rationale.
/// Test: `cli_parses_auth_set_token`, `cli_parses_auth_set_token_stdin`,
/// `cli_parses_auth_clear_token`, `cli_parses_auth_status`.
#[derive(Debug, Subcommand)]
pub(crate) enum AuthAction {
    /// Store a `CLAUDE_CODE_OAUTH_TOKEN` for managed sessions to use.
    ///
    /// Why: `--token <val>` is convenient for scripting but lands the token in
    /// shell history; the default (no `--token`) reads it from stdin instead
    /// (e.g. `claude setup-token | tm auth set-token`), so this is the safer
    /// path for interactive use.
    /// What: reads the token from `--token` when given, else from stdin
    /// (trimmed of surrounding whitespace/newline), and writes it to
    /// `~/.trusty-tools/trusty-mpm/claude-code-oauth.token` with mode 0600.
    /// Test: `cli_parses_auth_set_token`, `cli_parses_auth_set_token_stdin`.
    SetToken {
        /// The token value. When omitted, read from stdin instead.
        #[arg(long)]
        token: Option<String>,
        /// Explicitly read the token from stdin (the default when `--token`
        /// is omitted; accepted for clarity in scripts/docs).
        #[arg(long)]
        stdin: bool,
    },
    /// Remove the stored token, if present.
    ///
    /// Why: lets an operator roll back to ambient Keychain/`ANTHROPIC_API_KEY`
    /// auth, or rotate to a freshly generated token via a subsequent
    /// `set-token`.
    /// What: deletes the stored token file; a no-op (not an error) when no
    /// token is stored.
    /// Test: `cli_parses_auth_clear_token`.
    ClearToken,
    /// Report the current auth configuration without printing any secret.
    ///
    /// Why: the fastest way for an operator (or `tm doctor`) to see whether a
    /// managed session is at risk of the `CLAUDE_CONFIG_DIR` login loop
    /// (#2246) before it happens.
    /// What: prints presence/absence of the stored token, the
    /// `CLAUDE_CODE_OAUTH_TOKEN`/`ANTHROPIC_API_KEY` env vars, and a
    /// best-effort on-disk credentials check. NEVER prints the token value.
    /// Test: `cli_parses_auth_status`.
    Status,
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
        /// Base URL of the trusty-mpm daemon. `Option<String>`, no
        /// `default_value` (#2487) — resolved via `resolve_daemon_url`, which
        /// applies the lock-file / compiled-in-default fallback itself.
        #[arg(long, env = "TRUSTY_MPM_URL")]
        url: Option<String>,
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

/// Actions for the `slack` subcommand (DOC-20 adapter, #1294).
///
/// Why: the Slack bot mirrors the Telegram bot's lifecycle surface over the SAME
/// chat-core nucleus. Pairing is NOT duplicated — Slack uses its own app-install
/// flow (a bot token + app-level token), not the daemon pairing store — so only
/// the `start`/`stop` lifecycle is exposed here.
/// What: `Start` runs the Socket-Mode bot in the foreground; `Stop` kills it.
/// Test: `cli_parses_slack_start`, `cli_parses_slack_stop`.
#[derive(Debug, Subcommand)]
pub(crate) enum SlackCmd {
    /// Start the Slack bot process (Socket Mode — no public webhook required).
    Start {
        /// Base URL of the trusty-mpm daemon. `Option<String>`, no
        /// `default_value` (#2487) — resolved via `resolve_daemon_url`, which
        /// applies the lock-file / compiled-in-default fallback itself.
        #[arg(long, env = "TRUSTY_MPM_URL")]
        url: Option<String>,
        /// Slack bot token (`xoxb-…`). When omitted, resolved from `.env.local` /
        /// `.env` / the `SLACK_BOT_TOKEN` environment variable.
        #[arg(long)]
        bot_token: Option<String>,
        /// Slack app-level token (`xapp-…`, Socket Mode). When omitted, resolved
        /// from `.env.local` / `.env` / the `SLACK_APP_TOKEN` environment variable.
        #[arg(long)]
        app_token: Option<String>,
        /// Validate configuration and exit without connecting to Slack.
        #[arg(long)]
        check: bool,
    },
    /// Stop the Slack bot process if running.
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

/// Actions for the `tm projects` subcommand (DOC-35 §3.1/§10.8, #2115/#2381).
///
/// Why: the registry-B project surface plus the Deliverable/Milestone ledger,
/// exposed as a deterministic verb tree of thin HTTP clients.
/// What: the four registry verbs (`list`/`register`/`show`/`status`) and the two
/// nested subtrees (`deliverables`/`milestones`).
/// Test: `cli_parses_projects_*` in `tests_projects.rs`.
#[derive(Debug, Subcommand)]
pub(crate) enum ProjectsAction {
    /// List registered projects (optionally filtered by tag).
    List {
        /// Emit the raw project JSON instead of the table.
        #[arg(long)]
        json: bool,
        /// Only show projects carrying this tag.
        #[arg(long)]
        tag: Option<String>,
    },
    /// Register (idempotent upsert) a project in registry B.
    Register {
        /// Registry key / short project name.
        name: String,
        /// Full repository URL.
        #[arg(long)]
        repo_url: String,
        /// Default branch (daemon defaults to `main` when omitted).
        #[arg(long)]
        default_branch: Option<String>,
        /// Free-form description.
        #[arg(long)]
        description: Option<String>,
        /// Comma-separated classification tags.
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        /// Technology-stack hint (e.g. `rust`).
        #[arg(long)]
        stack_hint: Option<String>,
        /// Preferred `gh` login for this project (#2081).
        #[arg(long)]
        gh_user: Option<String>,
    },
    /// Show a project's config PLUS a read-only nested sessions listing.
    Show {
        /// Project name.
        name: String,
        /// Emit raw JSON (config + sessions) instead of the human view.
        #[arg(long)]
        json: bool,
    },
    /// Show a project's deterministic status rollup (session histogram + flags).
    Status {
        /// Project name.
        name: String,
        /// Emit the raw status JSON instead of the human view.
        #[arg(long)]
        json: bool,
    },
    /// View or edit a project's deterministic config (§3.1/§6, #2120).
    ///
    /// Bare (no `set`/`unset`/`tags` subcommand) is a read-only view (GET);
    /// each subcommand is a single deterministic PATCH — never free text.
    Config {
        /// Project name.
        name: String,
        /// Emit raw JSON instead of the human view (bare view form only).
        #[arg(long)]
        json: bool,
        /// `set <field> <value>` / `unset <field>` / `tags --add/--remove`;
        /// omitted = view.
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    /// Manage a project's Deliverables (§10.8).
    Deliverables {
        /// Deliverable action to perform.
        #[command(subcommand)]
        action: DeliverablesAction,
    },
    /// Manage a project's Milestones (§10.8).
    Milestones {
        /// Milestone action to perform.
        #[command(subcommand)]
        action: MilestonesAction,
    },
}

// The `tm manager <action>` subcommand tree lives in `cli_manager.rs` (extracted
// to keep this file under the SLOC cap); re-exported so `crate::cli::ManagerAction`
// stays the stable reference every dispatcher and parse test uses.
pub(crate) use crate::cli_manager::ManagerAction;

/// Actions for `tm projects config <name>` (DOC-35 §3.1/§6, #2120).
///
/// Why: the deterministic sub-verbs of the configurator — `set`/`unset` mirror
/// the field-level PATCH exactly (never free text); `tags` is a DEDICATED verb
/// rather than folded into `set`/`unset` — disclosed deviation from a literal
/// reading of the spec's CLI sketch (which groups tags under the same
/// set/unset comment line): `set <field> <value>` structurally cannot express
/// two independent lists (add AND remove) in one positional value, and §6's
/// own field table sanctions "`--add`/`--remove`" as the mechanism; the issue
/// text explicitly allows either "set/unset or dedicated --add/--remove
/// flags" and this is the dedicated-verb form.
/// What: three subcommands routed by `commands::projects::registry::config`.
/// Test: `cli_parses_projects_config_*` in `tests_projects.rs`.
#[derive(Debug, Subcommand)]
pub(crate) enum ConfigAction {
    /// Set a field to a new value.
    Set {
        /// Which field to set.
        #[arg(value_enum)]
        field: SettableConfigField,
        /// The new value.
        value: String,
    },
    /// Clear (unset) a field back to absent.
    ///
    /// `default_branch` is deliberately NOT a valid target here — see
    /// [`ClearableConfigField`]'s doc for why.
    Unset {
        /// Which field to clear.
        #[arg(value_enum)]
        field: ClearableConfigField,
    },
    /// Add and/or remove tags in one call (§6: "no free-text
    /// replace-whole-list footgun" — there is no plain tags-replace form).
    Tags {
        /// Comma-separated tags to add.
        #[arg(long, value_delimiter = ',')]
        add: Vec<String>,
        /// Comma-separated tags to remove (applied after `--add`, server-side).
        #[arg(long, value_delimiter = ',')]
        remove: Vec<String>,
    },
}

/// CLI value for a settable config field (§6); maps 1:1 to
/// `trusty_mpm::project_config::ConfigField` via `convert.rs`. Kept local so
/// `cli.rs` carries no domain dependency. Default kebab-case value names
/// (`default-branch`, `stack-hint`, …) match this codebase's other
/// `ValueEnum`s (e.g. `DeliverableStatusArg`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum SettableConfigField {
    /// `default_branch` — required, non-empty.
    DefaultBranch,
    /// `description` — free-form, clearable.
    Description,
    /// `stack_hint` — advisory, clearable.
    StackHint,
    /// `gh_user` — preferred `gh` login (#2081), clearable.
    GhUser,
}

/// CLI value for a clearable config field (§6) — DELIBERATELY NARROWER than
/// [`SettableConfigField`]: `default_branch` is excluded because it has no
/// wire representation for "clear" at all (`PatchProjectBody::default_branch`
/// is a plain `Option<String>` — absent=unchanged, present+blank=400,
/// present+non-blank=set; there is no double-Option `null`=clear story like
/// `description`/`stack_hint`/`gh_user` have). Rejecting `unset
/// default-branch` at clap parse time (an "invalid value" error) is strictly
/// better than proxying a request the server cannot honor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum ClearableConfigField {
    /// `description`.
    Description,
    /// `stack_hint`.
    StackHint,
    /// `gh_user`.
    GhUser,
}

/// Actions for `tm projects deliverables` (DOC-35 §10.8, #2381).
#[derive(Debug, Subcommand)]
pub(crate) enum DeliverablesAction {
    /// List a project's Deliverables (optionally filtered by status).
    List {
        /// Project name.
        project: String,
        /// Emit raw JSON instead of the table.
        #[arg(long)]
        json: bool,
        /// Only show Deliverables in this status.
        #[arg(long)]
        status: Option<DeliverableStatusArg>,
    },
    /// Create a Deliverable (starts in `proposed`).
    #[command(alias = "create")]
    Add {
        /// Project name.
        project: String,
        /// Human-readable name.
        #[arg(long)]
        name: String,
        /// Category of work.
        #[arg(long)]
        kind: DeliverableKindArg,
        /// Coarse effort tier (S/M/L/XL).
        #[arg(long)]
        estimate: EstimationTierArg,
        /// Free-form description.
        #[arg(long)]
        description: Option<String>,
        /// Repo-relative spec path (plain string, §10.4).
        #[arg(long)]
        spec_ref: Option<String>,
        /// Opaque gh-first ticket reference (plain string, §13 Q6).
        #[arg(long)]
        ticket_ref: Option<String>,
    },
    /// Show one Deliverable by id.
    Show {
        /// Project name.
        project: String,
        /// Deliverable id (UUID).
        id: String,
        /// Emit raw JSON instead of the human view.
        #[arg(long)]
        json: bool,
    },
    /// Transition a Deliverable's status (enforces the §10.3 state machine).
    SetStatus {
        /// Project name.
        project: String,
        /// Deliverable id (UUID).
        id: String,
        /// Target status.
        status: DeliverableStatusArg,
    },
}

/// Actions for `tm projects milestones` (DOC-35 §10.8, #2381).
#[derive(Debug, Subcommand)]
pub(crate) enum MilestonesAction {
    /// List a project's Milestones.
    List {
        /// Project name.
        project: String,
        /// Emit raw JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Create a Milestone.
    #[command(alias = "create")]
    Add {
        /// Project name.
        project: String,
        /// Human-readable name.
        #[arg(long)]
        name: String,
        /// Target date (RFC 3339, e.g. `2026-09-01T00:00:00Z`).
        #[arg(long)]
        target_date: String,
        /// Free-form description.
        #[arg(long)]
        description: Option<String>,
    },
    /// Show one Milestone by id.
    Show {
        /// Project name.
        project: String,
        /// Milestone id (UUID).
        id: String,
        /// Emit raw JSON instead of the human view.
        #[arg(long)]
        json: bool,
    },
}

/// CLI value for a Deliverable kind (§10.2); maps 1:1 to
/// `trusty_mpm::deliverable::DeliverableKind`. Kept local so `cli.rs` carries no
/// domain dependency; the projects command module does the mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum DeliverableKindArg {
    /// A new capability.
    Feature,
    /// A defect repair.
    Bugfix,
    /// A behavior-preserving restructuring.
    Refactor,
    /// Maintenance / housekeeping.
    Chore,
    /// Test-only work.
    Test,
    /// Documentation-only work.
    Docs,
}

/// CLI value for an estimation tier (§10.2); the value names are the exact
/// uppercase `S`/`M`/`L`/`XL` letters the spec uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum EstimationTierArg {
    /// Small.
    #[value(name = "S")]
    S,
    /// Medium.
    #[value(name = "M")]
    M,
    /// Large.
    #[value(name = "L")]
    L,
    /// Extra-large.
    #[value(name = "XL")]
    Xl,
}

/// CLI value for a Deliverable status (§10.3); default kebab-case value names
/// match the wire encoding (`in-progress`, `proposed`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum DeliverableStatusArg {
    /// Planned but not started.
    Proposed,
    /// Actively worked.
    InProgress,
    /// Paused on an external blocker.
    Blocked,
    /// Objective gate passed or user-confirmed.
    Complete,
    /// Terminal: delivered.
    Delivered,
    /// Terminal: shipped.
    Shipped,
}

/// Actions for the `sessions` subcommand (canonical since #2116; also
/// reachable via the deprecated hidden `session` alias, see `Command::Session`).
///
/// Two families coexist here (see `sessions --help` for the full description):
///   - Local project sessions: start, stop, list, run, output, pause, resume, …
///   - Managed fleet sessions: new, ls, activity, send, answer, decommission, …
#[derive(Debug, Subcommand)]
pub(crate) enum SessionAction {
    // ── Local project sessions ─────────────────────────────────────────────
    // These verbs operate on the current project directory and its daemon-backed
    // session registry. They are NOT valid targets for managed-session IDs.
    // ──────────────────────────────────────────────────────────────────────
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
    ///
    /// `kill` is a visible alias (#2191) for this same non-destructive stop —
    /// the workspace is preserved and the session remains resumable. For the
    /// terminal, workspace-removing teardown use `decommission` instead.
    #[command(visible_alias = "kill")]
    Stop {
        /// Session id or friendly name (e.g. `tm-quiet-falcon`).
        id_or_name: String,
    },
    /// List sessions for the current project.
    List {
        /// Project directory (defaults to the cwd).
        #[arg(long)]
        dir: Option<String>,
    },
    /// Launch the coordinator TUI: an input box over a live session list (#1272).
    ///
    /// Why (DOC-13): a Claude-Code-like surface — a text input on top of a live
    /// session list (controller bullet + one row per managed session, the active
    /// row in two columns `[id] │ [summary]`). This is a NEW screen, distinct
    /// from the existing `tm tui` dashboard. It lives under the `session` group
    /// as `tm sessions tui` (#1392): it operates on the same managed-session list
    /// the other `session` verbs do, so grouping it there keeps the surface
    /// discoverable without colliding with the `coordinator` SM chat command.
    /// What: polls the daemon's coordinator-context endpoint live on the
    /// `--interval-ms` cadence (self-healing the daemon URL on failure) and maps
    /// each session into the list. `--url` is resolved via `resolve_daemon_url`.
    /// Test: `cli_parses_session_tui` in `tests.rs`.
    Tui {
        /// Base URL of the trusty-mpm daemon.
        #[arg(long, env = "TRUSTY_MPM_URL")]
        url: Option<String>,
        /// Poll interval in milliseconds (daemon refresh cadence).
        #[arg(long, default_value_t = 1500)]
        interval_ms: u64,
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
    // ── Managed fleet sessions ─────────────────────────────────────────────
    // These verbs operate on provisioned worktree sessions in the managed store.
    // They are NOT valid targets for local project-session IDs.
    // ──────────────────────────────────────────────────────────────────────
    /// Spawn a new managed session from a repo + ref (session-manager MVP).
    ///
    /// Why: the session-manager MVP provisions an isolated workspace from a git
    /// repo and starts a harness in it; `tm sessions new` is the operator-facing
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
        /// Do NOT auto-inject the task into the session pane (#1903/#1299).
        ///
        /// By default the task is turnkey: once the session's runtime is ready
        /// it is typed into the pane so the session starts working immediately.
        /// Pass `--no-inject` for the legacy metadata-only behavior, where the
        /// task is stored but you deliver it yourself with `tm sessions send`.
        #[arg(long)]
        no_inject: bool,
        /// Bind this session to an existing Deliverable (DOC-35 §10.6, #2379).
        ///
        /// The daemon validates the id exists AND belongs to this session's
        /// project BEFORE spawning anything (a 404 otherwise); this is a
        /// pointer-only link — it never changes the Deliverable's own status.
        #[arg(long)]
        deliverable: Option<String>,
    },
    /// List managed sessions (session-manager MVP).
    ///
    /// Why: operators need to see every managed session and its pending decision.
    /// What: GETs `/api/v1/sessions/managed` (with optional source_id filter) and
    /// renders a table or JSON. `--current` scopes to sessions for the cwd's repo.
    /// By default, decommissioned tombstone sessions are hidden (#1809); `--all`
    /// opts in to seeing every state.
    /// Test: `cli_parses_session_ls`, `cli_parses_session_ls_source_id`,
    /// `cli_parses_session_ls_current`, `cli_parses_session_ls_all`.
    Ls {
        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Filter to sessions for this `owner/repo` slug.
        ///
        /// Why: with 149+ sessions `ls` is a firehose — scoping by project is the
        /// first thing every operator reaches for. Passed as `?source_id=` to the
        /// daemon, which already supports the query parameter.
        #[arg(long)]
        source_id: Option<String>,
        /// Derive source_id from the cwd's git remote (shorthand for --source-id).
        ///
        /// Why: in a project checkout `--current` is more ergonomic than copying the
        /// full `owner/repo` slug from the URL. Mutually exclusive with `--source-id`;
        /// passing both is a parse error.
        #[arg(long, conflicts_with = "source_id")]
        current: bool,
        /// Include decommissioned tombstone sessions in the output (#1809).
        ///
        /// Why: by default decommissioned sessions are hidden so the list shows only
        /// live sessions. `--all` opts in to the full unfiltered list for forensics.
        /// Has no effect on `--json` output (raw daemon response is always complete).
        #[arg(long)]
        all: bool,
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
    /// Hard-delete a managed session RECORD from the store (#2012).
    ///
    /// Why: distinct from `decommission` (stop runtime + maybe remove
    /// workspace + tombstone) — this permanently drops the record itself via
    /// the existing tombstone-compaction primitive, for a mis-provisioned or
    /// stale record an operator wants gone outright rather than left as a
    /// `Decommissioned` tombstone forever. Fail-closed: refuses a RUNNING
    /// session unless `--force` is passed.
    /// What: POSTs `/api/v1/sessions/managed/{id}/delete?force=<bool>`. NEVER
    /// removes the workspace directory from disk — deleting the record is a
    /// store-only operation (use `decommission` first to also reclaim disk).
    /// Test: `cli_parses_session_delete`.
    Delete {
        /// Managed session id.
        id: String,
        /// Bypass the running-session guard and delete the record anyway.
        #[arg(long)]
        force: bool,
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
    /// Tear down EVERY ephemeral (test/throwaway) managed session (#1508).
    ///
    /// Why: e2e harnesses (and any operator who tagged test sessions) need a
    /// one-shot "clean up all my throwaway sessions" verb. REAL sessions default
    /// `ephemeral=false` and are unreachable, so durable work is never harmed.
    /// What: POSTs `/api/v1/sessions/managed/decommission-ephemeral` and prints the
    /// count torn down.
    /// Test: `cli_parses_session_decommission_ephemeral`.
    DecommissionEphemeral,
    /// Print a cross-format catch-up digest for paused sessions (DOC-28 cutover bridge).
    ///
    /// Why: during migration from claude-mpm to trusty-mpm, paused sessions may
    /// exist in both the legacy JSON format (`.claude-mpm/sessions/`) and the
    /// native markdown format (`.trusty-mpm/sessions/`). `catchup` merges both
    /// and renders a work-context digest so the PM can restore full context
    /// without re-spawning the old tool.
    /// What: scans the current project (and, with `--all-projects`, every project
    /// registered in `~/.claude-mpm/session-registry.db`); calls
    /// `native_session_finder::find_paused_sessions` + `render_resume_context`
    /// and prints the resulting markdown to stdout.
    /// The `--full` flag is accepted for forward-compatibility with PR2 (watermark
    /// logic); for PR1 it forces full history and is otherwise a no-op.
    /// Test: `cli_parses_session_catchup` in `tests.rs`.
    ///
    // CUTOVER BRIDGE — remove post-migration (#1762)
    Catchup {
        /// Also enumerate machine-wide projects via the claude-mpm session registry.
        #[arg(long)]
        all_projects: bool,
        /// Force full history mode (watermark logic lands in PR2; accepted now for
        /// forward-compatibility).
        #[arg(long)]
        full: bool,
    },
    /// Prune managed sessions by state + compact tombstones (#1508).
    ///
    /// Why: ONE tool to (a) tear down ephemeral/stopped sessions and (b) compact
    /// the store by dropping decommissioned tombstones, so the legacy stale records
    /// can be purged with the same verb that cleans up test sessions. The
    /// fail-closed default never touches a RUNNING session.
    /// What: POSTs `/api/v1/sessions/managed/prune` with the `--state` filter;
    /// `--dry-run` reports what WOULD be pruned without mutating.
    /// Test: `cli_parses_session_prune`.
    Prune {
        /// Which records to target: `ephemeral` | `stopped` | `decommissioned` | `all`.
        #[arg(long)]
        state: String,
        /// Report what WOULD be pruned without killing, removing, or tombstoning
        /// any record.
        #[arg(long)]
        dry_run: bool,
        /// Also tear down RUNNING (`Active`/`Provisioning`) sessions. Off by
        /// default — the fail-closed safety gate.
        #[arg(long)]
        include_active: bool,
    },
    /// Remove orphaned per-session git worktree directories (#1840).
    ///
    /// Why: sessions decommissioned before the Fix 1a worktree-removal patch,
    /// or where `git worktree remove` failed, leave stale
    /// `.worktrees/<session-id>/` directories on disk. This command removes
    /// them safely — only directories without a corresponding active session
    /// are ever touched. `tm doctor` reports the count of orphans and suggests
    /// running this command.
    /// What: POSTs `/api/v1/sessions/managed/prune-worktrees`; defaults to dry
    /// run (pass `--force` to actually delete).
    /// Test: `cli_parses_session_prune_worktrees`.
    PruneWorktrees {
        /// Actually delete orphaned dirs (default: dry-run / preview only).
        #[arg(long)]
        force: bool,
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
