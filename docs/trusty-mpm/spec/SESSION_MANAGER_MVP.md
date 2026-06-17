# trusty-mpm — Session Manager Daemon: MVP Spec

> **Status:** Draft v2 · 2026-06-14
> **Author:** Bob Matsuoka
> **Crate:** `crates/trusty-mpm/` (edition 2024, `publish = false`)
> **Parent epic:** [#380](https://github.com/bobmatnyc/trusty-tools/issues/380)
> **Relationship to roadmap:** Refocuses the M1 "standalone metaharness POC" milestone;
> the Phase-1 agent/skill content parity work is orthogonal and continues unchanged.
> **Companion docs:** [PRD.md](./PRD.md) · [ARCHITECTURE.md](./ARCHITECTURE.md) ·
> [Gap Analysis](../research/trustympm-gap-analysis-decision-2026-06-05.md)

---

## Changelog (v1 → v2)

This section records the architectural decisions made after v1 was drafted (2026-06-14
brainstorming session) that supersede parts of that draft. The spec PR (#1200) was on
hold pending these decisions.

| # | Decision | v1 | v2 |
|---|---|---|---|
| D1 | **Control surface** | MCP-primary framing implied | Synchronous HTTP API is primary. MCP is a thin optional wrapper considered for a later release only. |
| D2 | **Calling agentic process** | Implied to be built into the substrate | Pluggable and BORROWED — any agentic process (Bob's CTO Claude MPM instance, Unicorn Factory bot, etc.) drives harnesses via the HTTP API. The session manager never embeds the driving logic. |
| D3 | **Autonomy operating model** | Not addressed | ~80% of harness decisions auto-accepted by the calling agentic process; ~20% escalated to human. API exposes PENDING DECISION + PROPOSED DEFAULT per session. Autonomy policy lives in the calling agentic process, not the substrate. Activity monitor is observability-only, NEVER the auto-accept gate. |
| D4 | **Observation layer** | Tmux pane capture only | Tmux panes AND GitHub issues/PRs/artifacts the harness produces. Substrate must expose workspace path, repo, and branch so the calling agentic process can correlate a session with its worktree/PR. |
| D5 | **Workspace isolation** | `--cwd` supplied by caller; no provisioner | NEW substrate responsibility: Workspace Provisioner pulls repo+ref into an mpm-owned isolated directory (`~/.trusty-mpm/workspaces/<project>/<session>`), runs `prepare_session` there, then launches the harness. Never touches existing project checkouts. |
| D6 | **Content source** | Local deploy only (deferred remote registry) | Agents/skills synced FROM the claude-mpm repository via the remote-registry mechanism (#387/#388). claude-mpm repo is the source of truth; mpm fetches and caches the catalog. |
| D7 | **Runtime adapter / auth** | Claude Code adapter via `env -u ANTHROPIC_API_KEY` | Unchanged from v1. |
| D8 | **Already-built modules** | N/A (v1 was pre-build) | `session_manager`, `runtime`, `activity` modules are implemented, compile, and are transport-agnostic. v2 adds HTTP API layer, workspace provisioner, content sync, and pending-decision exposure on top of them. |

---

## 1. Context & Problem

### The shift

trusty-mpm's existing architecture wraps stock `claude` processes and coordinates
them through a daemon. The current direction closely followed the Python `claude-mpm`
model: an in-process meta-harness that installs hooks, relays events, and runs one
supervised session per invocation. That model has a structural ceiling.

### The driver: unicorn-factory needs N concurrent autonomous sessions

The concrete need is the **unicorn-factory** pattern: run N autonomous agent sessions
concurrently — one per open ticket — each in an isolated git worktree, each
progressing independently. The operator needs to:

- Start session N without disturbing sessions 1…N-1.
- Walk away: sessions must survive the invoking shell closing.
- Come back later and observe what happened (pane output, activity state).
- Inject a question or command into a specific session without attaching to it.
- Know which sessions are working, which are stuck, which are done.

The Python harness cannot fulfill this because it is an **ephemeral in-process
runtime**: when the shell exits, the process dies. There is no daemon holding
shared session state across restarts.

### Why tmux is the right primitive

`claude -p` (headless mode) is one-shot: each invocation is a single-turn exchange.
Multi-turn autonomous work requires a durable interactive process. The interactive
Claude Code CLI (`claude`) maintains a real conversation context across many tool
calls and turns inside a single running process. tmux provides the missing layer:
an addressable, named, durable pane that persists across SSH disconnections and
terminal closures, and that can be injected into via `send-keys` and observed via
`capture-pane`.

This is exactly what `TmuxDriver::spawn_claude` already does
(`src/daemon/services/tmux_service.rs:147`) and why
`TmuxCommand::NewSession` uses `-A -d` (`src/core/tmux.rs:71`) — idempotent
detached creation.

### The pivot

The daemon stops being driven by sessions that announce themselves via
hooks, and becomes a **supervisor+spawner+observer** of sessions it *creates and
names itself*. The operator interface is a `tm session` CLI backed by the same HTTP
daemon. The daemon is the only party that knows the naming convention and can
reconcile reality on restart.

---

## 2. Goals / Non-Goals

### Goals (MVP)

1. **HTTP API** — a synchronous HTTP API (extending the existing axum router) with
   endpoints to spawn, list, get activity, send input, get attach command, and stop
   managed sessions. This is the primary control surface.
2. **Workspace Provisioner** — given `(repo_url, ref, task)`, pull code into an
   mpm-owned isolated directory, run `prepare_session` there, and launch the harness
   with `cwd` pointing to that isolated workspace. Never touch existing project checkouts.
3. **Content sync from claude-mpm repo** — agent/skill catalog synced from the
   claude-mpm repository (remote-registry #387/#388), not re-ported by hand.
4. **Pending-decision exposure** — per session, the API exposes not just coarse state
   but the PENDING DECISION + the harness's PROPOSED DEFAULT, and accepts an injected
   answer. Autonomy policy (auto-accept vs. escalate) lives in the calling agentic process.
5. **`tm session` CLI** — `new`, `ls`, `send`, `activity`, `attach`, `stop` subcommands
   backed by the HTTP API.
6. **LLM activity monitor** (observability only) — classifies pane state via a cheap
   LLM. Used for human observation and the calling agentic process correlation ONLY — not for
   auto-accepting decisions (see safety rule in section 5.2).
7. **Auth via Max OAuth** — `ANTHROPIC_API_KEY` unset; Claude Code uses `~/.claude`
   credentials.
8. **Daemon reconciliation** — adopt existing managed sessions, detect orphaned
   sessions, detect dead sessions on restart.
9. **Unit tests** with a fake tmux driver and fake LLM client; one `#[ignore]` live
   smoke test.

### Non-Goals for MVP (explicit)

These items are **deferred, not deleted**. Code that supports them is left in place
or behind a seam; it is simply not exercised in the MVP.

| Item | Status |
|------|--------|
| MCP wrapper for session manager | DEFERRED — HTTP API first; MCP is an optional thin wrapper considered for a later release |
| Hook relay (overseer/hook relay stays dormant) | DORMANT — keep existing code, add no new hook wiring |
| Monitor dashboard / TUI integration for session-manager | DEFERRED |
| Socket.IO niceties | DEFERRED |
| Inter-session IPC (sessions communicating with each other) | DEFERRED |
| Dispatcher mode (daemon routes tasks to sessions) | DEFERRED — sessions self-drive from assembled instructions |
| trusty-code (tcode) runtime adapter | DEFERRED — seam defined, not implemented |
| Autonomy policy (auto-accept tiers T1–T4) | DEFERRED — calling agentic process responsibility, not substrate |
| Ticket/artifact correlation depth (issue/PR tracking per session) | DEFERRED — substrate exposes the workspace/repo/branch; correlation logic is the calling agentic process's concern |
| Unattended supervisor daemon (always-on 24/7 fleet operation) | DEFERRED — tmux holds durable sessions; the calling agentic process reconnects |
| Daemon-enforced circuit breakers (#393) | DEFERRED |
| Per-agent model overrides (#394) | DEFERRED |

---

## 3. Architecture

### 3.1 The model

One Rust daemon holds all supervision logic. N tmux sessions run independently;
each pane runs a **RuntimeAdapter** implementation. For MVP the only adapter is
`ClaudeCodeAdapter`, which spawns the interactive `claude` CLI. The daemon never
runs in each session — it is purely a control plane.

**The driving logic is borrowed, not built.** The substrate (trusty-mpm daemon) is a
dumb harness launcher and observer. It does not make autonomy decisions. Those are
the calling agentic process's job — whether that is Bob's CTO Claude MPM instance, a Unicorn
Factory controlling bot, or a human typing `tm sessions send`. See section 5 for the
substrate/caller boundary.

```
┌──────────────────────────────────────────────────────────────────┐
│   ONE MACHINE (single-instance enforced via daemon.lock)          │
│                                                                  │
│  CALLING AGENTIC PROCESS (external — Bob's CTO Claude MPM,        │
│  Unicorn Factory, or human operator)                             │
│       │                                                          │
│       │  HTTP (loopback :7880, discovered via ~/.trusty-mpm/daemon.lock)
│       ▼                                                          │
│   ┌──────────────────────────────────────────────────┐          │
│   │   trusty-mpm daemon (Arc<DaemonState>)            │          │
│   │                                                  │          │
│   │   session_manager/  (IMPLEMENTED — transport-agnostic)       │
│   │   ├── SessionRecord  (id, name, cwd, repo, branch, state,   │
│   │   │                   pending_decision, proposed_default)    │
│   │   ├── SessionManager (CRUD + reconcile)           │          │
│   │   └── reconcile_on_boot()                         │          │
│   │                                                  │          │
│   │   runtime/          (IMPLEMENTED — transport-agnostic)       │
│   │   ├── RuntimeAdapter trait (spawn / is_alive)     │          │
│   │   ├── ClaudeCodeAdapter   (MVP — ONLY this one)   │          │
│   │   └── TrustyCodeAdapter   (DEFERRED — seam only)  │          │
│   │                                                  │          │
│   │   activity/         (IMPLEMENTED — transport-agnostic)       │
│   │   ├── PaneHash      (SHA256 of pane tail)         │          │
│   │   ├── ActivityCache (last hash + verdict)         │          │
│   │   └── ActivityMonitor (capture → hash → LLM)     │          │
│   │                                                  │          │
│   │   provisioner/      (NEW — workspace isolation)  │          │
│   │   ├── WorkspaceProvisioner (clone repo → setup)  │          │
│   │   └── PreparedWorkspace (path, repo, branch)     │          │
│   │                                                  │          │
│   │   content/          (NEW — claude-mpm catalog sync)│         │
│   │   └── CatalogSync   (fetch/cache from claude-mpm) │         │
│   │                                                  │          │
│   │   (EXISTING, REUSED unchanged)                   │          │
│   │   core/tmux.rs            TmuxCommand / argv     │          │
│   │   daemon/tmux.rs          TmuxDriver             │          │
│   │   daemon/services/        TmuxService            │          │
│   │   core/instruction_pipeline.rs   build_instructions│         │
│   │   core/session_launch/mod.rs     prepare_session  │          │
│   │   core/agent_deployer.rs         deploy_agents    │          │
│   │   core/skill_deployer.rs         deploy_skills    │          │
│   │   daemon/lock.rs          daemon.lock I/O         │          │
│   │   daemon/api.rs           axum router (extended)  │          │
│   │   daemon/state.rs         Arc<DaemonState>        │          │
│   │   (DORMANT, not extended)                        │          │
│   │   POST /hooks / overseer  hook relay              │          │
│   └──────────────────┬───────────────────────────────┘          │
│                      │ tmux new-session / send-keys / capture    │
│                      ▼                                          │
│   ┌──────────────────────────────────────────────────────────┐  │
│   │  Session A (tmux: tmpm-<slug>)  Session B (tmpm-<slug>)  │  │
│   │  pane: claude (interactive)     pane: claude (interactive)│  │
│   │  cwd: ~/.trusty-mpm/workspaces/ cwd: ~/.trusty-mpm/workspaces/
│   │       <proj>/<session-a>/            <proj>/<session-b>/  │  │
│   └──────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

### 3.2 Module boundaries

New and extended code under `crates/trusty-mpm/src/`:

```
src/
├── session_manager/       (IMPLEMENTED — reused unchanged)
│   ├── mod.rs             (re-exports; ~50 SLOC)
│   ├── record.rs          (SessionRecord: id, name, cwd, repo, branch,
│   │                       state, pending_decision, proposed_default; ~100 SLOC)
│   ├── manager.rs         (SessionManager: new/ls/stop/reconcile; ~200 SLOC)
│   └── store.rs           (on-disk JSON persistence; ~100 SLOC)
├── runtime/               (IMPLEMENTED — reused unchanged)
│   ├── mod.rs             (RuntimeAdapter trait + factory; ~60 SLOC)
│   └── claude_code.rs     (ClaudeCodeAdapter: spawn via TmuxService; ~80 SLOC)
├── activity/              (IMPLEMENTED — reused unchanged)
│   ├── mod.rs             (re-exports; ~40 SLOC)
│   ├── cache.rs           (ActivityCache: hash + verdict + cost tally; ~100 SLOC)
│   └── monitor.rs         (ActivityMonitor: capture → hash → LLM; ~180 SLOC)
├── provisioner/           (NEW — workspace isolation)
│   ├── mod.rs             (re-exports; ~40 SLOC)
│   └── workspace.rs       (WorkspaceProvisioner: clone/pull + prepare_session; ~200 SLOC)
├── content/               (NEW — claude-mpm catalog sync)
│   ├── mod.rs             (re-exports; ~40 SLOC)
│   └── catalog_sync.rs    (CatalogSync: fetch/cache from claude-mpm repo; ~200 SLOC)
└── daemon/
    └── api.rs             (EXTENDED — new session routes added)
```

All files stay under the 500-SLOC production cap. Existing modules (`session_manager`,
`runtime`, `activity`) are reused as-is.

### 3.3 Harness-Agnostic Design Principles

The MVP ships with `ClaudeCodeAdapter` as the only runtime implementation. However, the
`RuntimeAdapter` abstraction is the entry point for a multi-harness future. This
section documents the design principles that make pluggable harnesses work.

**The three core principles are:**

#### 1. Canonical-Context Preservation Principle

Whatever harness runs, it MUST receive and operate under MPM's canonical instructions,
workflow, agents, skills, and memory. Instruction/prompt assembly stays in MPM;
each harness implementation only **PROJECTS** that canonical context into the
runtime's native format and deployment model.

- MPM assembles the canonical instruction bundle once (agents, skills, system
  prompt, `.mcp.json`, `.claude/settings.json`, memory palace links, hooks).
- The `RuntimeAdapter` trait receives this bundle as input.
- The adapter's `spawn()` method translates the bundle into the runtime's native
  format: e.g. config files for Claude Code, environment variables for trusty-code,
  agent/skill registration for hypothetical future harnesses.
- The adapter launches the harness with the projected context.
- **Adding a new harness = implementing the context-projection + launch/capture/inject
  mechanics, never re-authoring canonical content per harness.**

This principle ensures:
- Agents and skills have one source of truth (MPM's catalog).
- Instructions do not diverge across harnesses.
- Memory, circuit breakers, and autonomy policies are enforced uniformly.
- Future harnesses inherit the ecosystem without re-implementation.

#### 2. No-Drop-In-Required Principle

A user NEVER needs to drop into a harness to make progress on their work.
All observation, decision-answering, and control happen through MPM's own interfaces:
CLI (`tm session`), TUI, web dashboard, or chat. The managed session runs autonomously,
observed and driven via these interfaces.

However, the harness remains directly attachable for users who WANT to interact
directly (e.g. `tmux attach` to the session, or the harness's native UI). Dropping
in is always optional, never required for core functionality.

- **Required path**: All progress happens via MPM API (HTTP, CLI, TUI).
- **Optional path**: Power users may attach directly for debugging or manual
  intervention, but this is not the primary workflow.
- **Consequence**: The MPM-side interfaces must be feature-complete for autonomous
  operation. The harness UI is not a required stop.

This principle holds across all harnesses: Claude Code (tmux-attachable), trusty-code
(CLI-attachable), and any future runtime. It ensures that MPM is the primary
control surface regardless of which harness is running underneath.

#### 3. Transport-Agnostic Core Modules

The `session_manager`, `runtime`, `activity`, and `provisioner` modules contain
no HTTP, CLI, or tmux-specific logic. They define domain types (`SessionRecord`,
`RuntimeAdapter`, `ActivityVerdict`) and pure transformation functions. This
makes them reusable across any transport (HTTP, MCP, CLI commands).

- Library modules (under `src/`) contain business logic and domain types.
- Transport layers (`daemon/api.rs`, `bin/tm.rs`) consume these modules and map
  them to HTTP, CLI, or MCP.
- A new harness or transport can reuse the same core without modification.

---

## 4. Runtime Backends & Auth

| | Backend A — MVP | Backend B — Deferred |
|---|---|---|
| **Name** | Claude Code CLI (interactive) | trusty-code (`tcode`) |
| **Auth model** | Claude Max OAuth — flat rate subscription | Direct Anthropic API — per-token |
| **How launched** | `claude` CLI via `tmux send-keys` | `tcode` binary via `tmux send-keys` |
| **Why tmux** | Interactive multi-turn; `claude -p` is one-shot only | Same — durable pane needed |
| **API key** | MUST be unset (`env -u ANTHROPIC_API_KEY`) | Set from config |
| **Rate limit concern** | Shared Max rate limit across N concurrent sessions | Per-token cost |
| **When** | MVP | After tcode reaches session parity |

### Critical auth constraint

The daemon MUST spawn the Claude Code CLI with `ANTHROPIC_API_KEY` **unset** so
Claude Code falls back to the logged-in Max OAuth credentials stored in `~/.claude`.
If `ANTHROPIC_API_KEY` is set in the daemon's environment it takes precedence and
the operator incurs per-token charges instead of using their flat-rate Max
subscription.

Only user-level logins are reused from the host environment: `~/.claude` (Max OAuth),
`gh` auth, and git credentials. Everything else (`.claude/`, `.mcp.json`, deployed
agents, `.claude-mpm/`, worktrees/) lives exclusively inside the mpm-owned workspace.

Concrete implementation in `ClaudeCodeAdapter::spawn`:

```rust
// Why: Max OAuth path requires ANTHROPIC_API_KEY to be absent.
// If the daemon's own env has the key (e.g. from a shell export), we must
// explicitly unset it for the child so Claude Code uses ~/.claude credentials.
// What: Sends the literal shell command `env -u ANTHROPIC_API_KEY claude`
// into the tmux pane so the unsetting happens in the user's shell.
// Test: Verify pane output does not contain an "API key" error; confirm the
// command sent starts with `env -u ANTHROPIC_API_KEY`.
Command::new("env")
    .arg("-u")
    .arg("ANTHROPIC_API_KEY")
    .arg("tmux")
    .args(["send-keys", "-t", &tmux_name, "claude", "Enter"])
```

### Why not headless (`claude -p`)

`claude -p` is a single-turn exchange: it sends one prompt, gets one response, and
exits. It cannot maintain a conversation across many tool-use cycles or await user
confirmation prompts. The interactive CLI inside a tmux pane is a durable process
that runs indefinitely, handling multi-turn flows, permission prompts, and tool-use
cycles naturally. `send-keys` injection and `capture-pane` observation are the
control primitives that make this model work for autonomous sessions.

---

## 5. Substrate / Calling Agentic Process Boundary

### 5.1 What the substrate does

The substrate (trusty-mpm daemon) is responsible for:

- Provisioning isolated workspaces (section 6).
- Launching harnesses (tmux sessions running `claude`).
- Observing pane state (LLM activity monitor — section 8).
- Storing and serving `SessionRecord` metadata (id, name, workspace path, repo, branch).
- Accepting and persisting a pending-decision answer injected by the calling agentic process.
- Reconciling session state on restart.

The substrate does NOT:

- Decide whether a harness's proposed default should be accepted or escalated.
- Know anything about unicorn-factory tiers (T1–T4) or PR autonomy rules.
- Correlate sessions with GitHub issues/PRs/artifacts (beyond storing repo+branch).
- Coordinate between sessions.

### 5.2 What the calling agentic process does

The calling agentic process is **borrowed**: it is an existing agentic process (Bob's CTO Claude
MPM instance, a Unicorn Factory controlling bot, a human with a terminal). It:

- Calls the HTTP API to spawn sessions with specific `repo_url`, `ref`, and `task`.
- Polls `GET /api/v1/sessions/managed/{id}/activity` to observe session state.
- Uses the `pending_decision` + `proposed_default` fields to decide whether to
  auto-accept or escalate.
- Calls `POST /api/v1/sessions/managed/{id}/answer` to inject an accepted answer.
- Tracks session progress by correlating the session's `repo` + `branch` with GitHub
  PR/issue APIs directly (the substrate exposes the fields; the calling agentic process queries
  GitHub itself).

### 5.3 Autonomy operating model

Approximately 80% of harness decisions can run without human intervention. The
calling agentic process auto-accepts guarded default choices and escalates only the ~20% that
are genuinely ambiguous or high-risk (comparable to the unicorn-factory's tiered PR
autonomy model: T1 trivial auto-merge through T4 human-required review).

**SAFETY RULE (non-negotiable):** The auto-accept gate MUST be driven by STRUCTURED
guardrail signals:

- trusty-review verdict (APPROVE/REJECT on the diff).
- CI/test status (green tests from the harness's own run).
- Search/memory consistency (trusty-search + trusty-memory confirm no conflicting
  context).

The auto-accept gate MUST NOT be driven by the cheap pane-reading LLM activity
monitor. That monitor is **observability-only** — it tells you what the pane looks
like, not whether the harness's proposed change is correct. Using a gpt-4o-mini
pane classifier as the approval signal would let a subtly wrong harness auto-merge
bad code. The trusty stack (search, memory, review) exists precisely to provide
the structured signals that make autonomous operation trustworthy.

### 5.4 Pending-decision API shape

`SessionRecord` carries two optional fields updated by the harness (via inject or
future hook) and consumed by the calling agentic process:

```json
{
  "pending_decision": "Should I create a new branch for this fix or commit to the existing branch 'feat/oauth'?",
  "proposed_default": "Create a new branch 'fix/token-refresh' off 'feat/oauth'."
}
```

The calling agentic process reads these fields, applies its autonomy policy, and either:

- Calls `POST /api/v1/sessions/managed/{id}/answer` with `{ "answer": "<text>" }`
  to inject an accepted or overridden answer, OR
- Escalates to the human (Slack notification, GitHub comment, etc.).

Once an answer is injected, the substrate clears `pending_decision` and
`proposed_default` and records the answer in the activity log.

---

## 6. Workspace Provisioner

### 6.1 Problem

Harness workspaces must never overlap with existing project checkouts on the
operator's machine. `.claude/`, `.mcp.json`, deployed agents, `.claude-mpm/`,
and `worktrees/` directories would collide between the live project and the harness,
and between concurrent harnesses on the same repo.

### 6.2 Solution: mpm-owned workspace root

The workspace provisioner:

1. Accepts `(repo_url, ref, task)`.
2. Clones the repository into `~/.trusty-mpm/workspaces/<project>/<session-id>/`
   (fresh clone preferred; bare-mirror optimization deferred to reduce disk I/O).
3. Runs `prepare_session` (existing `core/session_launch/mod.rs`) INSIDE that
   isolated directory — deploying agents/skills, writing `.mcp.json`,
   `.claude/settings.json`, and `CLAUDE.md` there.
4. Returns a `PreparedWorkspace { path, repo_url, branch }` struct.
5. The caller (`SessionManager::new_session`) passes `cwd = workspace.path` to
   `ClaudeCodeAdapter::spawn`.

There is ZERO overlap with Bob's real project folders or between concurrent
harnesses on the same repo. The only shared resources are user-level login state
(`~/.claude`, `~/.gitconfig`, `gh` auth tokens) which are read-only and safe to
share.

### 6.3 CLI surface

```
tm sessions new --repo <url> --ref <branch-or-sha> --task "<description>" [--name <hint>]
```

`--repo` and `--ref` replace the v1 `--cwd` flag (which assumed the caller had
already set up a workspace). Both flags feed the provisioner.

For local development without provisioning (testing only), `--cwd <path>` remains
accepted with a warning that config-collision prevention is the caller's
responsibility.

### 6.4 Code conventions for the provisioner

```rust
// Why: Isolates harness workspaces so they never collide with the operator's
//      existing project checkouts or with other concurrent harnesses.
// What: Clones repo_url at ref into ~/.trusty-mpm/workspaces/<project>/<session>,
//       runs prepare_session, and returns a PreparedWorkspace.
// Test: Unit-test with a fake git backend; integration test with a temp bare repo.
pub async fn provision(
    &self,
    session_id: &SessionId,
    repo_url: &str,
    git_ref: &str,
    task: &str,
) -> Result<PreparedWorkspace, ProvisionError> { … }
```

Error type uses `thiserror` (library code). Provisioner logs to stderr via
`tracing::info!` / `tracing::error!`.

---

## 7. Content Sync from claude-mpm Repository

### 7.1 Rationale

trusty-mpm is a different launcher, but the agent and skill content is essentially
the same as Python claude-mpm (~40 agents, ~25 skills). Re-porting this content by
hand would immediately diverge. Instead, the remote-registry mechanism (#387/#388)
is pointed at the claude-mpm repository as the source of truth.

### 7.2 Mechanism

`CatalogSync` (new `src/content/catalog_sync.rs`):

1. Fetches the agent/skill manifest from the claude-mpm GitHub repository (or a
   configured local path for offline development).
2. Caches the downloaded catalog under `~/.trusty-mpm/catalog/` with a
   content-addressed layout (SHA of the manifest file).
3. On `prepare_session`, the existing `deploy_agents` and `deploy_skills` calls
   consume from the cached catalog rather than from the local workspace source tree.

The cache is invalidated by a TTL (default 24h, configurable via
`TRUSTY_MPM_CATALOG_TTL_HOURS`) or by `tm catalog sync --force`.

### 7.3 CLI surface

```
tm catalog sync          # fetch/update from claude-mpm repo
tm catalog sync --force  # bypass TTL and re-download
tm catalog ls            # list cached agents and skills with versions
```

### 7.4 Code conventions

```rust
// Why: Eliminates manual re-porting of ~40 agents/~25 skills from claude-mpm.
// What: Fetches the manifest from the claude-mpm repo, validates checksums,
//       and writes artifacts under ~/.trusty-mpm/catalog/.
// Test: Unit-test with a mock HTTP response; assert cached layout and checksum.
pub async fn sync(&self, force: bool) -> Result<CatalogSyncResult, CatalogError> { … }
```

Error type uses `thiserror`. Sync logs to stderr.

---

## 8. Control API (HTTP — Primary Surface)

**The daemon's HTTP API is the primary and only control surface for MVP.** It is a
synchronous REST API extending the existing axum router in `daemon/api.rs`. MCP is
explicitly NOT the primary surface; it may be added as a thin wrapper later.

All routes follow the existing axum pattern: `Arc<DaemonState>` injected via `State`,
JSON request/response bodies, `DaemonError` mapped to HTTP status codes. Port and
base URL are discovered from `~/.trusty-mpm/daemon.lock` via `src/core/connect.rs`.

### 8.1 Route table

```
POST   /api/v1/sessions/managed              spawn a new managed session
GET    /api/v1/sessions/managed              list all managed sessions
GET    /api/v1/sessions/managed/{id}         get one session record
POST   /api/v1/sessions/managed/{id}/send    inject text into pane
GET    /api/v1/sessions/managed/{id}/activity  LLM activity verdict + pending decision
POST   /api/v1/sessions/managed/{id}/answer  inject answer to pending decision
GET    /api/v1/sessions/managed/{id}/attach-cmd  return the tmux attach command string
DELETE /api/v1/sessions/managed/{id}         stop and deregister
```

### 8.2 Request/response shapes

**POST /api/v1/sessions/managed** — spawn:

```json
{
  "repo_url": "https://github.com/bobmatnyc/trusty-tools",
  "ref": "main",
  "task": "Implement feature #1234 — add OAuth2 support",
  "name_hint": "ticket-1234"
}
```

Response `201 Created`:

```json
{
  "id": "uuid-v4",
  "name": "tmpm-ticket-1234",
  "workspace_path": "/Users/bob/.trusty-mpm/workspaces/trusty-tools/uuid-v4",
  "repo_url": "https://github.com/bobmatnyc/trusty-tools",
  "branch": "main",
  "state": "starting",
  "created_at": "2026-06-14T10:00:00Z",
  "attach_cmd": "tmux attach-session -t tmpm-ticket-1234"
}
```

**GET /api/v1/sessions/managed** — list:

```json
{
  "sessions": [
    {
      "id": "uuid",
      "name": "tmpm-ticket-1234",
      "workspace_path": "/Users/bob/.trusty-mpm/workspaces/trusty-tools/uuid",
      "repo_url": "https://github.com/bobmatnyc/trusty-tools",
      "branch": "feat/oauth",
      "state": "active",
      "created_at": "…",
      "last_activity_at": "…",
      "pending_decision": null,
      "proposed_default": null
    }
  ]
}
```

**POST /api/v1/sessions/managed/{id}/send** — inject text:

```json
{ "text": "Please summarize what you have done so far." }
```

Response `200 OK`:

```json
{ "sent": true, "tmux_name": "tmpm-ticket-1234" }
```

**GET /api/v1/sessions/managed/{id}/activity** — activity + pending decision:

```json
{
  "id": "uuid",
  "tmux_name": "tmpm-ticket-1234",
  "verdict": {
    "state": "working",
    "summary": "Running cargo test, 3 of 12 tests passed so far.",
    "confidence": 0.87
  },
  "pending_decision": "Should I create a new branch for this fix or commit to feat/oauth?",
  "proposed_default": "Create branch fix/token-refresh off feat/oauth.",
  "cost": {
    "model": "openai/gpt-4o-mini",
    "input_tokens": 412,
    "output_tokens": 64,
    "latency_ms": 340,
    "total_checks": 7,
    "llm_calls_made": 5
  },
  "cache_hit": false
}
```

**POST /api/v1/sessions/managed/{id}/answer** — accept or override proposed default:

```json
{ "answer": "Create branch fix/token-refresh off feat/oauth." }
```

Response `200 OK`:

```json
{ "injected": true, "tmux_name": "tmpm-ticket-1234" }
```

**GET /api/v1/sessions/managed/{id}/attach-cmd** — return attach command:

```json
{ "attach_cmd": "tmux attach-session -t tmpm-ticket-1234" }
```

### 8.3 CLI surface (`tm session`)

```
tm sessions new --repo <url> --ref <branch-or-sha> --task "<desc>" [--name <hint>]
tm sessions ls [--json]
tm sessions activity <id>
tm sessions send <id> "<text>"
tm sessions answer <id> "<answer>"
tm sessions attach <id>     # prints attach_cmd; does not exec into tmux
tm sessions stop <id>
```

`tm catalog sync [--force]` and `tm catalog ls` are added alongside.

---

## 9. Session Lifecycle & Naming

### Naming convention

The daemon derives tmux session names using the **existing** convention from
`src/core/names.rs`:

- Default: `tmpm-<adjective>-<noun>` via `name_from_uuid(&session_id)` — e.g.
  `tmpm-quiet-falcon`, `tmpm-swift-lotus`.
- When `--name` hint is provided: `tmpm-<hint-slug>` — e.g. `tmpm-ticket-1234`.
  Truncated to 32 chars, lowercase, dashes only.

The prefix `tmpm-` is the ownership marker. The daemon scans for `tmpm-` sessions
on reconciliation and treats any session with that prefix as potentially managed.

### Reconciliation on restart

On daemon boot, `reconcile_on_boot()` runs before accepting HTTP requests:

1. List all tmux sessions via `TmuxDriver::list_sessions()`.
2. Filter to those matching `tmpm-` prefix.
3. Cross-reference against `~/.trusty-mpm/sessions.json`:
   - In store **and** in tmux → **adopt**: mark `Active`, recover the record.
   - In store **but not** in tmux → **orphaned**: mark `Dead`.
   - In tmux **but not** in store → **external tmpm session**: adopt as `Adopted`.

---

## 10. LLM Activity Monitor

### Purpose

This component answers "what is session X doing right now?" without requiring hooks
or instrumentation inside the Claude Code process. It is the MVP substitute for a
hook-based activity feed, AND provides the `pending_decision` / `proposed_default`
visibility layer for the calling agentic process.

The monitor is **strictly observational**. It MUST NOT trigger auto-accept decisions.
See the safety rule in section 5.3.

### Flow

```
GET /api/v1/sessions/managed/{id}/activity
        │
        ▼
ActivityMonitor::check(session_id)
        │
        ├─ TmuxService::capture(session, lines=60)
        │       → raw pane text (plain, NO ANSI: capture-pane -p without -e)
        │
        ├─ hash tail (SHA-256 of last 60 lines)
        │
        ├─ compare to ActivityCache.last_hash
        │
        ├─ [UNCHANGED] → return cached verdict + cost { cache_hit: true }
        │                  NO LLM call made
        │
        └─ [CHANGED]  → send diff/tail (≤60 lines) to OpenRouter
                          model: TRUSTY_LLM_MODEL (default openai/gpt-4o-mini)
                          via OpenRouterProvider (crates/trusty-common/src/chat/openai_compat/)
                          → structured JSON verdict
                          → update ActivityCache
                          → record per-check metrics
```

### Classification prompt

The system prompt asks the model to return:

```json
{
  "state": "working",
  "summary": "One-line plain-English summary of what the pane shows.",
  "confidence": 0.90
}
```

Valid `state` values:
- `working` — tool calls running, file edits in progress, compilation output streaming
- `idle` — prompt is visible, waiting for input, no activity in pane
- `blocked_on_permission` — a permission confirmation prompt is visible
- `errored` — crash, panic, or repeated error output with no forward progress
- `done` — task completion signal visible

The monitor does not auto-inject responses. A `blocked_on_permission` verdict
surfaces in the activity response so the calling agentic process can decide to call
`POST /answer` or escalate to the human.

### Cost instrumentation

```rust
/// Why: Enables empirical cost/accuracy measurement of the pane-reading LLM.
/// What: Records per-check token usage, latency, cache-hit flag, and verdict.
/// Test: FakeLlmProvider records call count; assert cache_hit path skips the call.
pub struct CheckMetrics {
    pub session_id: SessionId,
    pub at: DateTime<Utc>,
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub latency_ms: u64,
    pub cache_hit: bool,
    pub verdict_state: ActivityState,
}
```

### Dependencies

- `crates/trusty-common/src/chat/openai_compat/` — reused as-is.
- `OPENROUTER_API_KEY` (required); `TRUSTY_LLM_MODEL` (default `openai/gpt-4o-mini`).
- If `OPENROUTER_API_KEY` is unset, returns `{ state: "unknown", summary: "OPENROUTER_API_KEY not configured" }`.

---

## 11. Reused vs Added vs Deferred

| Concern | Code location | Status |
|---------|--------------|--------|
| tmux argv builder | `src/core/tmux.rs` | **REUSED unchanged** |
| tmux process driver | `src/daemon/tmux.rs` (`TmuxDriver`) | **REUSED unchanged** |
| tmux service facade | `src/daemon/services/tmux_service.rs` | **REUSED unchanged** |
| Session naming | `src/core/names.rs` | **REUSED unchanged** |
| Instruction assembly pipeline | `src/core/instruction_pipeline.rs` | **REUSED unchanged** |
| Session launch prep | `src/core/session_launch/mod.rs` (`prepare_session`) | **REUSED — called by provisioner** |
| Agent deployment | `src/core/agent_deployer.rs` | **REUSED — fed by catalog sync** |
| Skill deployment | `src/core/skill_deployer.rs` | **REUSED — fed by catalog sync** |
| Daemon lock + URL discovery | `src/daemon/lock.rs`, `src/core/connect.rs` | **REUSED unchanged** |
| axum HTTP router | `src/daemon/api.rs` | **EXTENDED — new session routes** |
| DaemonState | `src/daemon/state.rs` | **EXTENDED — SessionManager + CatalogSync fields** |
| OpenRouter/openai-compat client | `crates/trusty-common/src/chat/openai_compat/` | **REUSED unchanged** |
| Session manager module | `src/session_manager/` | **IMPLEMENTED — transport-agnostic; reused unchanged** |
| RuntimeAdapter trait + ClaudeCodeAdapter | `src/runtime/` | **IMPLEMENTED — reused unchanged** |
| ActivityMonitor + ActivityCache | `src/activity/` | **IMPLEMENTED — reused unchanged** |
| Workspace Provisioner | `src/provisioner/` | **NEW** |
| Content / catalog sync | `src/content/` | **NEW** |
| `tm session` CLI commands | `src/bin/tm/commands/session.rs` | **NEW / EXTENDED** |
| `tm catalog` CLI commands | `src/bin/tm/commands/catalog.rs` | **NEW** |
| Hook relay / overseer | `src/daemon/services/hook_service.rs` | **DORMANT — untouched** |
| trusty-code (tcode) RuntimeAdapter | `src/runtime/trusty_code.rs` | **DEFERRED** |
| MCP wrapper for session manager | N/A | **DEFERRED** |
| TUI integration for managed sessions | `src/tui/` | **DEFERRED** |
| Inter-session IPC | N/A | **DEFERRED** |
| Autonomy policy | N/A (calling agentic process, not substrate) | **DEFERRED / NOT SUBSTRATE** |

---

## 12. MVP Definition

The smallest independently demoable slice:

1. `tm catalog sync` fetches the agent/skill catalog from the claude-mpm repo and
   caches it under `~/.trusty-mpm/catalog/`.
2. `tm sessions new --repo https://github.com/… --ref main --task "implement OAuth2"`
   provisions an isolated workspace under `~/.trusty-mpm/workspaces/`, runs
   `prepare_session` using the synced catalog, creates a named tmux session, and
   spawns `env -u ANTHROPIC_API_KEY claude` in the pane.
3. `tm sessions ls` shows the session with its name, state, workspace path, repo, and
   branch.
4. `tm sessions send <id> "what have you done so far?"` injects text into the pane.
5. `tm sessions activity <id>` returns an LLM verdict plus any `pending_decision` +
   `proposed_default` the harness has surfaced. A second call without pane change
   returns the cached verdict without an LLM call.
6. `tm sessions answer <id> "Create branch fix/token-refresh"` injects the answer
   into the pane and clears `pending_decision`.
7. `tm sessions attach <id>` prints `tmux attach-session -t tmpm-<slug>`.
8. `tm sessions stop <id>` kills the tmux session and marks the record dead.
9. After a daemon restart, running sessions are re-adopted; dead ones are marked
   orphaned.

---

## 13. Acceptance Criteria

1. `tm catalog sync` exits 0 and writes agent/skill artifacts under
   `~/.trusty-mpm/catalog/`; `tm catalog ls` lists at least one agent and one skill.

2. `tm sessions new --repo <url> --ref main --task "…"` exits 0; the workspace exists
   at `~/.trusty-mpm/workspaces/<project>/<session-id>/`; it is NOT inside any of
   the operator's existing project directories; `tmux has-session -t tmpm-<slug>`
   succeeds.

3. The workspace directory provisioned in criterion 2 contains `.mcp.json`,
   `.claude/settings.json`, and `CLAUDE.md` written by `prepare_session`, and at
   least one agent file deployed from the synced catalog.

4. The tmux pane launched by criterion 2 does NOT have `ANTHROPIC_API_KEY` set
   in its environment. Confirmed by verifying the command sent was
   `env -u ANTHROPIC_API_KEY claude`.

5. `tm sessions ls` returns a list that includes the session from criterion 2 with
   `state: active` or `state: starting`, and includes `workspace_path`, `repo_url`,
   and `branch` fields with correct values.

6. `tm sessions send <id> "hello from test"` exits 0; the string appears in the pane
   output captured by `tmux capture-pane -p -t <name>` within 2 seconds.

7. `tm sessions activity <id>` returns a JSON body with:
   - `verdict.state` set to one of `{working, idle, blocked_on_permission, errored, done}`.
   - `verdict.summary` non-empty.
   - `cost.model`, `cost.input_tokens`, `cost.latency_ms` present.
   - `pending_decision` field present (may be `null`).
   - `proposed_default` field present (may be `null`).

8. With a pane that has been idle (unchanged last-60-line tail), a second call to
   `tm sessions activity <id>` returns `cache_hit: true` and does NOT increment
   `input_tokens` in the cost tally.

9. When `pending_decision` is non-null, `tm sessions answer <id> "<text>"` exits 0;
   a subsequent `tm sessions activity <id>` returns `pending_decision: null`; the
   injected answer text appears in the pane capture.

10. `tm sessions attach <id>` prints exactly `tmux attach-session -t tmpm-<slug>` to
    stdout and exits 0.

11. `tm sessions stop <id>` exits 0; `tmux has-session -t <name>` returns non-zero
    afterwards; `tm sessions ls` shows the record with `state: dead`.

12. After `tm daemon restart` (SIGTERM + relaunch), `tm sessions ls` shows:
    - Previously active sessions as `active` (re-adopted from tmux).
    - Sessions whose tmux session was killed before the restart as `orphaned`.

13. Unit tests pass without a real tmux binary, real git remote, or real OpenRouter key:
    - `FakeTmuxDriver` implements the same send/capture interface as `TmuxDriver`.
    - `FakeGitBackend` returns a pre-staged fixture directory for provisioning.
    - `FakeLlmProvider` returns a fixed `ActivityVerdict` and records call count,
      enabling the cache-hit test in criterion 8.

14. One `#[ignore]` smoke test (`test_live_session_e2e`) provisions a real workspace
    from a public repo, creates a real tmux session, runs `echo hello`, captures it,
    calls the real activity monitor, and asserts verdict state is `idle` or `working`.
    Requires: `tmux` installed, `OPENROUTER_API_KEY` set, network access.

---

## 14. Deferred / vNext

The following are explicitly out of scope for this MVP. They are captured here so
the acceptance criteria stay tight.

| Item | Notes |
|------|-------|
| MCP wrapper for session-manager tools | HTTP API ships first; MCP is a thin adapter later |
| Autonomy tiers (T1–T4) | Lives in the calling agentic process (CTO Claude MPM), not the substrate |
| Ticket/artifact correlation (issue/PR tracking per session) | Substrate exposes repo+branch; calling agentic process queries GitHub directly |
| Unattended supervisor daemon | Deferred until 24/7 fleet operation is needed; tmux holds state |
| Per-session rate-limit monitoring | Add `rate_limited` verdict state when data warrants it |
| Bare-mirror workspace optimization (disk savings) | Fresh clone for MVP; mirror deferred |
| Hook relay activation | Hook code stays dormant; activate in Phase 2 |

---

## 15. Risks & Open Questions

| Risk | Description | Mitigation |
|------|-------------|------------|
| Activity-detection accuracy | 60-line pane tail may not have enough signal for reliable classification. | Accumulate real data; adjust tail size and prompt; measure per-state accuracy. |
| Shared Max rate-limit contention | N concurrent `claude` sessions share one Max account's rate limit. | Watch for rate-limit strings in pane text; add `rate_limited` verdict state in vNext. |
| Workspace disk usage | Fresh clone per session can accumulate large workspaces. | Bare-mirror optimization in vNext; add `tm workspace prune` command to clean old workspaces. |
| Daemon-restart session recovery | Re-adopting an active session is safe but the `workspace_path` field must survive to disk. | Ensure `sessions.json` persists `workspace_path` and test recovery round-trip. |
| Inject API security | `POST /send` and `POST /answer` inject arbitrary text into a live `claude` pane. | Bind daemon to `127.0.0.1` only (existing). Log every inject at `info` level. Add auth if multi-user. |
| `env -u` shell portability | Works on macOS and Linux. Windows is not a target. | Document macOS/Linux-only requirement. |

### Open Questions

1. Should `tm sessions new` block until the `claude` prompt is visible in the pane
   (polling `capture-pane`) or return immediately with `state: starting`?
   Recommendation: return immediately; let the operator poll `activity` or attach.

2. Should the catalog sync TTL be per-agent or per-manifest?
   Recommendation: per-manifest (one TTL for the whole fetch; per-agent would
   complicate partial-sync failure recovery).

3. The activity monitor uses a fixed 60-line tail. Should this be configurable
   per-check via a query parameter (`?lines=N`)?
   Recommendation: yes, default 60, cap 200.

---

## 16. Relationship to Existing Roadmap

### How this fits

This MVP refocuses the **M1 "standalone metaharness POC" milestone** of epic #380.
Instead of continuing to close Python-parity gaps in the single-session hook-relay
model, M1 now delivers a multi-session daemon that solves the unicorn-factory problem
with workspace isolation, content sync, and an agentic-process-ready HTTP API.

The existing PRD roadmap phases are **not cancelled**:

| Phase | Original work | Status after MVP |
|-------|--------------|-----------------|
| Phase 0 — Fidelity | #385, #383, #389, #390, #394, #395 | Continues independently; `prepare_session` improvements benefit the provisioner |
| Phase 1 — Content parity | #387, #388 — agent/skill catalog | NOW DELIVERED via claude-mpm repo sync; no manual re-porting |
| Phase 2 — Enforcement & robustness | #393, #391, #392, #384 | Continues; hooks stay dormant in MVP |
| Phase 3 — Ecosystem surfaces | B4/B5/B6/B7 ticketing, auto-config, postmortem | Continues |

The activity monitor's LLM approach is a **parallel experiment** to the hook relay,
not a replacement. Once data is collected, the team can decide whether to activate
hook-based monitoring for sessions that run `claude` with hooks wired, use LLM
polling, or both.

---

## Appendix: House Conventions for New Code

All new files must follow workspace conventions:

- **Why/What/Test doc pattern** on every public item (see `CLAUDE.md`).
- **No `unwrap()` in library code** — `thiserror` errors in `session_manager`,
  `runtime`, `activity`, `provisioner`, and `content` modules; `anyhow::Result` in
  the CLI command files.
- **500-SLOC production cap** — each new file listed in section 3.2 is budgeted
  under 500 SLOC. Split before merging if a module approaches the limit.
- **Logs to stderr** — no `println!` in daemon code; all tracing goes to
  `tracing::info!` / `warn!` / `error!`.
- **Feature gates** — the activity monitor imports `trusty-common`'s chat module.
  If `trusty-common` adds the `chat` feature behind a flag, gate the monitor
  dependency accordingly.
- **MSRV 1.91, edition 2024** — let-chains are available.
