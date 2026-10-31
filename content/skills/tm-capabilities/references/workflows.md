# Canonical End-to-End Workflows

> **HAND-AUTHORED — not generated.** Every other file under
> `tm-capabilities/` is produced by `tm generate capabilities` from live
> harness data; this file is curated synthesis — it ties the CLI, MCP, agent,
> and skill surfaces documented in the sibling `references/*.md` files into
> the handful of flows an operator or PM agent actually runs end-to-end. It
> is never touched by the generator or the CI drift gate
> (`scripts/check_capabilities.sh`) — edit it directly when a flow changes.

## 1. Session Launch

The path from "I want to work on this project" to a running, fully-provisioned
Claude Code session.

1. **Resolve or register the project** — `project_resolve` (MCP) or
   `tm projects` (CLI) maps a path/name to a registered project entry. A new
   project is registered with `project_register` / `tm projects register`.
2. **Provision the worktree + roster** — the daemon's session-launch pipeline
   (`session_launch`) creates or reuses a git worktree, deploys the bundled
   agent/skill roster (`bundle::ALL` → `~/.claude/{agents,skills}` or the
   managed workspace tier), and seeds framework instructions.
3. **Start the session** — `session_new` (MCP) or `tm sessions new` / guided
   default (`tm` with no subcommand) launches Claude Code inside the
   provisioned worktree, with `CLAUDE_CONFIG_DIR` and the output style wired.
   (`tm session new` — singular — still parses as a deprecated alias; see
   `references/cli.md`.)
4. **Verify** — `tm doctor` (see §3 below) confirms the roster deployed
   cleanly before real work starts.

Reference: `references/cli.md` (`session`, `projects`, `launch`, `connect`),
`references/mcp-tools.md` (`session_new`, `project_resolve`,
`project_register`).

## 2. Delegation

The PM-to-agent handoff every non-trivial task goes through.

1. **PM selects an agent** — using `references/agents.md`'s role/description/
   `skills:` columns to match the task to a specialist (see also
   `tm-delegation-patterns` for the decision matrix).
2. **Gate + track the delegation** — `agent_delegate` (MCP) records the
   delegation, applies the circuit-breaker and depth limits, and adds it to
   the session's dashboard tree. This does **not** execute the agent.
3. **Execute** — the native `Agent`/`Task` tool spawns the agent using the
   deployed name from `~/.claude/agents/` (e.g.
   `Agent(subagent_type="rust-engineer")`). The spawned agent inherits its
   full `skills:` chain (BASE-AGENT ∪ its own declarations — see
   `references/agents.md`).
4. **Handoff back** — the agent reports file paths, test evidence, and next
   steps per `tm-verification-protocols`; the PM decides whether to chain
   another delegation (e.g. engineer → code-critic → QA).

Reference: `references/mcp-tools.md` (`agent_delegate`,
`circuit_breaker_status`), `references/agents.md` (full roster).

## 3. Doctor Triage

Diagnosing "something is wired wrong" without guessing.

1. **Run the diagnostic** — `tm doctor` (CLI, round-trips through the daemon)
   or `GET /api/v1/doctor` runs all 15 checks (`references/doctor.md`) and
   returns the worst status across them.
2. **Read the check name** — look up the failing/warning check's name in
   `references/doctor.md` for exactly what it probes (e.g. `agent_skills`
   means a declared `skills:` reference didn't resolve; `skill_staleness`
   means a deployed skill's content diverged from the bundled source).
3. **Standalone alternative** — `tm validate [--repair]` runs the
   `deployment`-equivalent manifest diff directly against the filesystem, no
   daemon required — useful when the daemon itself won't start.
4. **Escalate or fix** — filesystem gaps usually self-heal via
   `tm validate --repair` or a fresh `tm install`; sidecar (`memory`/
   `search`) failures need the relevant daemon restarted; ambiguous
   `gh_account` needs an operator `gh auth switch`.

Reference: `references/doctor.md` (all 15 checks), `references/cli.md`
(`doctor`, `validate`, `repair`), `tm-doctor` skill (the `/tm-doctor` quick
invocation).

## 4. Bug-Report Pipeline

Turning an observed error into a filed, deduplicated issue.

1. **Discover** — `list_recent_errors` (MCP) surfaces recent captured errors
   from the bug-capture ring buffer (daemon/supervisor modes only — see
   `references/mcp-tools.md`).
2. **Preview** — `preview_bug_report` renders what would be filed (title,
   body, labels) without creating anything, so the PM/agent can review before
   committing to a GitHub issue.
3. **File** — `report_bug` creates the issue (or attaches to an existing
   duplicate), tagged per `issue-pr-assignee-and-label` convention
   (`--assignee @me --label trusty-mpm`).
4. **Route** — `tm-bug-reporting` and `tm-postmortem` cover the fuller
   protocol, including session-level error sweeps and postmortem synthesis
   across daemons.

Reference: `references/mcp-tools.md` (`list_recent_errors`,
`preview_bug_report`, `report_bug`), `tm-bug-reporting` / `tm-postmortem`
skills.

## 5. Asset Deployment Lifecycle

What a redeploy actually does to an already-deployed agent or skill. Relocated
here from the always-loaded PM prompt (#4574): a PM never runs a deploy, so the
lifecycle is a lookup, not a resident rule. `references/framework.md` carries
the directories and tier precedence this section assumes.

1. **Tier collisions.** On a name collision the highest-precedence source wins
   — project-custom, then user-custom (`~/.trusty-mpm/skills/`), then bundled.
   Lower-tier copies of that name are skipped and logged, so a shadowed skill
   is visible in the deploy log rather than silently absent.
2. **Hand-edit freeze.** A deployed file whose checksum no longer matches its
   source is treated as user-owned and skipped on every later redeploy, never
   clobbered. A project-custom skill gets the same protection structurally: it
   is absent from `.trusty-mpm-skills-manifest.json`, so the deployer never
   claims ownership of it. The freeze is not logged as a tier collision —
   there is no competing source to name.
3. **Orphaned copies are never retracted.** Removing a skill's source from
   `~/.trusty-mpm/skills/` does NOT delete the copies already deployed into
   projects; they stay until pruned. A removed bundled skill behaves the same
   way. This is deliberate — deployment adds, it does not reconcile.
4. **`mcp`-slugged skills never deploy.** A skill whose slug contains `mcp`
   would shadow Claude Code's built-in `/mcp`, so it is excluded outright.
5. **The managed tier reaches every session.** User-custom skills deploy into
   the shared `CLAUDE_CONFIG_DIR` too, so a skill authored once in
   `~/.trusty-mpm/skills/` is available to daemon-managed and standalone
   `tm run` sessions alike, not only per-project ones. The project-custom tier
   is naturally absent there — nothing hand-places a skill into the config dir.

Reference: `references/framework.md` (install layout, agent tier precedence,
skill deploy tiers), `tm-agent-architecture` (the agent compose chain).

## 6. PM Pause and Resume (`session_context_pause` / `session_context_catchup`)

The field reference for the two PM context tools. Relocated here from their MCP
descriptions (#10300), which had grown past the client's 4,096-character cut;
the descriptions now carry a summary and point here. `references/mcp-tools.md`
lists each tool's parameters.

### `session_context_pause`

Writes a `<session-id>/session-YYYYMMDD-HHMMSS.md` snapshot for `project_dir`
in the section format the catch-up reader parses (`## Summary` /
`## Completed` / `## In Progress` / `## Next Steps` / `## Git Context` /
`## Tmux Window`), and appends a `pause` line naming it to the append-only
`sessions-log.jsonl`. Only that session can resume from it (#5272).

- **`session_id`.** Leave it out. The daemon derives it from your own
  identity, which is what the next `session_context_catchup` matches on. An
  invented id files the snapshot where nothing will look for it (#6888).
- **`snapshot_publish` (#7282).** Where the project TRACKS
  `.trusty-mpm/sessions/`, the two files and nothing else are committed onto a
  fresh `chore/sessions-<session>-<ts>` branch. That branch is cut from the
  project's default branch: the one `config.yaml`'s `projects:` entry
  declares, else `origin/HEAD`, else `main`. It is pushed and opened as an
  auto-merging docs-only PR through `tm pr open`. Your branch, your HEAD and
  every other file stay untouched. `snapshot_publish` carries the branch,
  commit and PR URL for you to report. Publishing from any other branch, or a
  failure at any step, is an ERROR naming the step and the branch the commit
  was left on; the snapshot file is written either way. Every no-PR outcome
  reports `snapshot_publish.status: "skipped"` with a `reason`:
  `not_tracked` (the project git-ignores `.trusty-mpm/sessions/`),
  `not_a_git_repo`, or `unchanged`.
- **Session ref (ADR-0062, #7830).** Separately, on every project tracked or
  not, the snapshot is appended to this session's own append-only git ref
  `refs/tm/sessions/<user-id>/<session-key>` and lease-pushed to `origin` —
  only when GitHub confirms `origin` is private. A public, non-GitHub or
  unconfirmed `origin` gets no push and a `ref_error` saying why
  (#10614); the pushed copy reads `~` for the home directory and a
  hashed `<user-id>`.
  `ref_name` names the ref, `ref_published` says whether the push landed, and
  `ref_error` carries the reason when it did not. A ref failure NEVER fails the
  pause, because the local snapshot is the primary write. A non-null
  `ref_error` is yours to REPORT to the operator, not to retry. With
  `[session_refs] enabled = false` in `~/.trusty-mpm/config.toml` no ref is
  written and both `ref_name` and `ref_error` are null.
- **`resume_slot` (#9390).** Reports the `ws:<session>/resume` Tier C slot the
  pause writes over the memory socket for the next `session_context_catchup`
  to re-verify: a 24-hour expiry, the branch, PR numbers and snapshot path
  plus their re-read commands, never a head SHA. It is `stored`,
  `stored_without_verify:<reason>`, `stored_unslotted:slot_refused` or
  `skipped:<reason>`. The write is fail-open and never fails the pause.
- **Worktree prune (#4091).** Unless `prune_worktrees` is `false`, the pause
  prunes orphaned managed-session git worktrees in-process, with the engine
  `tm session prune-worktrees` uses. It NEVER removes a worktree holding
  uncommitted or unpushed work: each such worktree is returned in
  `skipped_dirty_worktrees` with a reason and file/commit counts for you to
  relay to the user.
- It does NOT touch tmux.

### `session_context_catchup`

Returns STRUCTURED JSON resume context for `project_dir`. No call ever
advances the catch-up watermark, so repeated calls are safe and
`watermark_advanced` is always `false`.

- **Lean default (#9883).** `digest: "lean"`; the resolved snapshot
  (`resolved_snapshot`, `resolved_via`, and `resolved_session` with its
  `summary`, `in_progress`, `next_steps` and pause-time `git_context`, or
  null); a LIVE `git_status` (`available`, at most 40
  `git status --porcelain` `entries`, `entries_total`, `truncated`; or
  `available: false` with an `error` starting `unavailable:` when git failed or
  timed out); and the resolution fields below. It carries no `sessions`,
  `recent_commits` or `recent_memory`.
- **`full: true`.** The FULL digest instead: paused sessions (native
  trusty-mpm and legacy claude-mpm), recent git commits and recent
  memory-palace activity over the whole history, with no watermark.
- **Resolution.** `resolved_snapshot` names a snapshot belonging to the
  `session_id` you pass. Several sessions share one store, so there is no
  "latest overall" fallback. When that id owns nothing (a relaunch mints a new
  session id inside the same window), passing `tmux_window` resolves the
  newest snapshot THIS WINDOW paused in this project. Failing that, it
  resolves the newest one THIS NAMED TMUX SESSION paused since it was created,
  because a relaunch recreates the window (#8408). That route needs
  `tmux_session_created` and resolves nothing without it. `resolved_via` says
  which route answered (`session_id`, `tmux_window`, `tmux_session`, or null),
  so do not read a window match as an exact one.
- **Staleness.** `resolved_snapshot_superseded` is true when this checkout's
  HEAD has moved past the commit the snapshot recorded. Its `in_progress` and
  `next_steps` may already be done, so read `commits_since_snapshot` (newest
  first, at most 20 of `commits_since_snapshot_total`) before re-planning any
  of them. It is false whenever the answer cannot be established: false means
  "not shown to be stale", never "proven current".
- **`resume_verify` (#9390).** Re-reads what the resume relies on. It is null
  when no snapshot resolved. Otherwise it reads the `ws:<session>/resume` slot
  of the session that wrote the snapshot and runs its `verify` commands — ONLY
  `gh pr view <n> --json headRefOid,state` and `git rev-parse <branch>`,
  argv-only, in `project_dir`, each command and the whole pass time-bounded.
  Each `checks` entry carries `command`, `claimed`, `returned` and `verdict`:
  `verified`, `stale` (the PR is MERGED or CLOSED, or the branch tip moved off
  the snapshot's recorded commit), or `unverified:<reason>`. Act on
  `returned`, the live head, never on a head in the snapshot, and read
  `unverified` as unknown, never as confirmed. `status` is `ran`, or
  `unverified:<reason>` when no command could be read (memory daemon
  unreachable, no live slot, no verify command). The pass only reports: it
  edits no slot, and a failure never fails the catch-up.
- **`session_refs` hydration (ADR-0062, #7830).** Before any of that is read,
  the local session cache is rebuilt from THIS caller's own append-only git
  ref `refs/tm/sessions/<user-id>/<session-key>`, so a resume from a fresh
  clone still resolves. `session_refs` reports the pass: `hydrated` (it ran),
  `refs_seen` (how many session refs exist on the remote, this caller's or
  not), `own_ref_found` (whether this caller's own ref was among them),
  `restored` (how many snapshot files were written back), and `error`.
  `session_refs.own_ref_found` is about THIS CALLER'S REF and is unrelated to
  the per-session `sessions[].owned`. `refs_seen > 0` with
  `own_ref_found: false` is a real and PERMANENT state, not a transient: after
  a hostname change or a `gh` account switch this caller reaches none of its
  old refs and never will. Report it rather than reading five refs as five
  restorable sessions. A hydration failure never fails the catch-up.
- **Ownership (#5386).** Each entry in `sessions` carries `owned`: true when
  the session is attributable to you (your `session_id` paused it, you are in
  the window that did, or it is the one snapshot resolved for you by tmux
  session name). A session you do not own is listed with `format`,
  `paused_at` and `summary` only. Its `source_file`, `tmux_window`,
  `in_progress`, `next_steps` and `git_context` are withheld, so the digest
  cannot hand you the means to adopt another session's state.
- **Paging (#5557).** `sessions` is a bounded PAGE: the whole response is
  fitted to a size a caller can read in one tool result. It is ordered with
  the sessions you own first, then newest-first, so page 0 carries what a
  resume reads. `sessions_total` says how many matched, `sessions_offset`
  where this page starts, and `sessions_next_offset` the value to pass back as
  `sessions_offset` for the remainder (null on the last page). Whenever
  anything was withheld — sessions, commits or drawers — `truncated` is true
  and `truncation_notice` says what and how to retrieve it, so a short page
  never reads as a complete one. `over_budget` is the separate case where
  nothing was withheld but one record is larger than a whole page: it ships
  intact and `page_bytes` says how big the page got.

Reference: `references/mcp-tools.md` (`session_context_pause`,
`session_context_catchup`), the `tm-session-pause` and `tm-session-resume`
skills.
