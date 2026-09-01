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
        /// (the `ANTHROPIC_API_KEY` path). Typed as `RuntimeKind` (a
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
    /// List managed sessions (session-manager MVP). Auto-prunes dead records;
    /// pass `--no-prune` for a read that changes nothing.
    ///
    /// Why: operators need to see every managed session and its pending decision.
    /// What: GETs `/api/v1/sessions/managed` (with optional source_id filter) and
    /// renders a table or JSON. `--current` scopes to sessions for the cwd's repo.
    /// By default, decommissioned tombstone sessions (#1809), sweep-classified
    /// dead records (#4994), and deleted-slot tombstones (#5952) are hidden;
    /// `--all` opts in to seeing every state.
    /// Test: `cli_parses_session_ls`, `cli_parses_session_ls_source_id`,
    /// `cli_parses_session_ls_current`, `cli_parses_session_ls_all`,
    /// `cli_parses_session_ls_terms_*`.
    Ls {
        /// Sort keyword and/or filter term(s).
        ///
        /// Grammar: `tm sessions ls [recent|alpha] [filter-word...]`. If the
        /// FIRST word case-insensitively equals `recent` or `alpha`, it selects
        /// the sort order (`recent` = most-recently-active first, the default;
        /// `alpha` = alphabetical by session name) and every remaining word
        /// (joined with a single space) becomes a case-insensitive substring
        /// filter matched against the id, name, project (source_id), state, and
        /// task columns. If the first word is NOT one of those two keywords,
        /// the sort stays `recent` and ALL words become the filter term.
        /// Examples: `tm sessions ls`, `tm sessions ls alpha`,
        /// `tm sessions ls recent api`, `tm sessions ls api` (same as
        /// `recent api`), `tm sessions ls alpha api`. No effect on `--json`
        /// output (raw daemon response is always complete, matching `--all`).
        #[arg(value_name = "SORT|FILTER", num_args = 0..)]
        terms: Vec<String>,
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
        /// Include decommissioned (#1809), dead (#4994), and deleted-slot
        /// (#5952) rows in the output.
        ///
        /// Why: by default those are hidden so the list shows only live sessions.
        /// `--all` opts in to the full unfiltered list for forensics — including
        /// the `-- deleted --` slot tombstones, each at its original slot number.
        /// Has no effect on `--json` output (raw daemon response is always complete).
        #[arg(long)]
        all: bool,
        /// Do NOT auto-prune dead records on this listing (#5950).
        ///
        /// Every listing decommissions confirmed-dead records by default
        /// (#4702) — records whose workspace is gone, never a live session —
        /// so `tm ls` stays usable instead of filling with tombstones. That
        /// makes a read verb mutate, which is surprising during a census, so
        /// this flag turns it off for one invocation: no decommission call, no
        /// confirmation-marker write, no prune notice. The listing then shows
        /// dead rows it would otherwise have hidden, because nothing swept
        /// them.
        #[arg(long)]
        no_prune: bool,
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
    /// \[DEPRECATED\] Stop a managed session's runtime — use `stop` instead.
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
    /// \[DEPRECATED\] Stop a managed session's runtime — use `stop` instead.
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
    /// \[DEPRECATED\] Resume a stopped managed session — use `resume` instead.
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
    /// Rename a managed session — from the list OR from within a session.
    ///
    /// Why: operators need to give a managed session a meaningful name. A
    /// managed session's identity name IS its tmux name, so a rename updates the
    /// record AND renames the live tmux session so `tmux attach`/`ls` reflect it.
    /// What: two forms —
    ///   • `tm sessions rename <id-or-name> <new-name>` (from the master list),
    ///   • `tm sessions rename <new-name>` (from WITHIN a session — the current
    ///     session is resolved from `$TM_MANAGED_SESSION_ID`).
    /// The first positional is the target when a second is given; otherwise it is
    /// the new name for the current session. PATCHes
    /// `/api/v1/sessions/managed/{id}` with `{ "name": … }`. Collisions and
    /// invalid names are rejected with an actionable error.
    /// Test: `cli_parses_sessions_rename_two_args`,
    /// `cli_parses_sessions_rename_in_session`.
    Rename {
        /// Target session id/name (two-arg form), or the NEW name for the
        /// current session (one-arg, in-session form).
        arg1: String,
        /// New name. When present, `arg1` selects the session to rename; when
        /// omitted, `arg1` is the new name and the session is the current one
        /// (resolved from `$TM_MANAGED_SESSION_ID`).
        arg2: Option<String>,
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
    /// count torn down. `--dry-run` (#6118) previews that same selection without
    /// tearing anything down — one call, one boolean, so the preview and the
    /// real sweep cannot disagree about what they picked.
    /// Test: `cli_parses_session_decommission_ephemeral`,
    /// `cli_parses_session_decommission_ephemeral_dry_run`.
    DecommissionEphemeral {
        /// Report which ephemeral sessions WOULD be torn down, without
        /// stopping, removing, or tombstoning any of them.
        #[arg(long)]
        dry_run: bool,
    },
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
    ///
    /// `--state unresolvable` (#6118) is the targeted cleanup for adoption
    /// ghosts: records naming no resolvable workspace (`cwd` is the `/unknown`
    /// sentinel and no `workspace_path`), which every other filter missed
    /// because their own live panes held them immune. It needs no
    /// `--include-active` and is not widened by it — 23 such records were
    /// measured on one host where `--state all --include-active` was the only
    /// command that reached them, and it also selected 32 healthy working
    /// sessions. Such a record has no workspace, so this can delete no file: it
    /// reclaims the leaked pane and the store row.
    /// Test: `cli_parses_session_prune`, `cli_parses_session_prune_unresolvable`.
    Prune {
        /// Which records to target: `ephemeral` | `stopped` | `decommissioned` |
        /// `deleted` | `all` | `unresolvable`.
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
    /// Re-sync a live managed session's deployed agents/skills/output-styles
    /// against the current catalog (issue #2444).
    ///
    /// Why: `#2002`'s asset deployment is one-shot at launch — a long-lived
    /// session never re-syncs when the bundled/catalog source changes
    /// underneath it, so it silently keeps running stale content. This
    /// OVERWRITES managed assets (the same "refresh a managed file, never
    /// touch a user-modified one" rule the launch-time deployers already
    /// enforce — see `core::session_launch::sync_session_assets`'s doc);
    /// it never touches CLAUDE.md, hooks, or MCP config. `tm sessions ls`
    /// flags a drifted session with a `[stale-assets]` marker.
    /// What: POSTs `/api/v1/sessions/managed/{id}/sync-assets` for one
    /// session (resolved by id or friendly name), or
    /// `/api/v1/sessions/managed/sync-assets` for EVERY syncable session
    /// when `--all` is passed. Exactly one of `id`/`--all` may be given.
    /// Test: `cli_parses_sessions_sync_assets`,
    /// `cli_parses_sessions_sync_assets_all`,
    /// `cli_sessions_sync_assets_id_and_all_conflict`.
    SyncAssets {
        /// Managed session id or friendly name to re-sync.
        #[arg(conflicts_with = "all", required_unless_present = "all")]
        id: Option<String>,
        /// Re-sync EVERY syncable session (active/stopped/errored) instead
        /// of one.
        #[arg(long)]
        all: bool,
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
    /// run (pass `--force` to actually delete). `--force` alone still refuses
    /// to touch a worktree holding uncommitted or unpushed work (#4091) — it
    /// is reported instead; `--discard-dirty` is the separate, explicit opt-in
    /// that permits destroying that work.
    /// `--merged-prs` (#2919) adds a second, independent reclaim pass: worktrees
    /// whose BRANCH has a merged pull request, which is the terminal state
    /// DOC-52 §3.4 makes the reclamation trigger. It is opt-in because it is the
    /// only path that acts on GitHub state; nothing automatic ever runs it.
    ///
    /// What that pass refuses, stated per owner because the two are checked by
    /// DIFFERENT gates (#5829): a worktree a managed SESSION claims, and one
    /// holding unsaved work, are refused by the session-record and dirty-tree
    /// gates. A worktree a DISPATCHED AGENT owns is refused by neither of those
    /// — an agent has no session record, and an agent that has committed and
    /// pushed has a clean tree — so it is refused by its own gate, which reads
    /// the ownership sentinel and the delegation registry (#5661). That gate
    /// permits deletion only when the registry can positively call the agent
    /// finished; a registry that cannot answer refuses. Every worktree it spares
    /// is now reported with the agent it was spared for.
    /// Test: `cli_parses_session_prune_worktrees`,
    /// `cli_prune_worktrees_discard_dirty_is_opt_in`,
    /// `cli_prune_worktrees_merged_prs_is_opt_in`.
    PruneWorktrees {
        /// Actually delete orphaned dirs (default: dry-run / preview only).
        #[arg(long)]
        force: bool,
        /// ALSO delete worktrees that still hold uncommitted or unpushed work
        /// (#4091). Off by default; this discards that work irreversibly.
        #[arg(long)]
        discard_dirty: bool,
        /// ALSO reclaim worktrees whose branch's pull request already merged
        /// (#2919). Off by default. Never destroys unsaved work.
        #[arg(long)]
        merged_prs: bool,
    },
    /// Report every git-registered worktree against `sessions.json` (#4288).
    ///
    /// Why: `prune-worktrees` shows only the reclaim CANDIDATES, so the
    /// worktrees it excludes — 33 of 118 on the machine this was measured on,
    /// six of them holding uncommitted work — are invisible. This verb reports
    /// the whole inventory, keyed on git's own registry identity rather than on
    /// a path shape (which misclassified 29% of them).
    /// What: GETs `/api/v1/sessions/managed/reconcile-worktrees` and prints one
    /// line per worktree — state, path, reason — plus the worktrees a later
    /// slice would adopt. READ-ONLY: there is no `--force`, because there is
    /// nothing here to force. Nothing is deleted, written, or migrated.
    /// Test: `cli_parses_session_reconcile_worktrees`,
    /// `cli_reconcile_worktrees_takes_no_destructive_flag`.
    ReconcileWorktrees {
        /// Print the full report as JSON instead of the one-line-per-row form.
        #[arg(long)]
        json: bool,
    },
    /// Take over a worktree whose owning session or agent is dead (#6497).
    ///
    /// Why: a session can die holding an agent's worktree that still contains
    /// uncommitted work, and until this verb a successor had no compliant way
    /// to finish it — every write route in was refused because the sentinel
    /// still named the dead owner, and the recorded workaround rebuilt the
    /// branch by hand in a new tree.
    /// What: POSTs `/api/v1/sessions/managed/adopt-worktree`. The daemon
    /// refuses unless the current owner is POSITIVELY known to have ended and
    /// nothing live is still working in the tree; on success the ownership
    /// sentinel names `--as` instead. It moves no files and commits nothing.
    /// Test: `cli_parses_session_adopt_worktree`,
    /// `cli_adopt_worktree_requires_the_adopting_session`.
    AdoptWorktree {
        /// The worktree directory whose ownership is being transferred.
        path: std::path::PathBuf,
        /// The managed session that becomes the new owner.
        #[arg(long = "as")]
        as_session: String,
    },
}
