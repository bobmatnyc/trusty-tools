//! `tm sessions` command group — local project sessions + managed fleet
//! sessions (canonical since #2116; also reachable via the deprecated hidden
//! `tm session` singular alias).
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap. This is the single largest command
//! group in the CLI surface, so it gets its own dedicated module.
//! What: [`SessionAction`] — local project verbs (`start`/`stop`/`list`/…)
//! plus managed-fleet verbs (`new`/`ls`/`activity`/`send`/…).
//! Test: `cli_parses_session_*` / `cli_parses_sessions_*` in `tests.rs`.

use clap::Subcommand;

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
