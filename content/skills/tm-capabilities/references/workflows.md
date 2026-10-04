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
