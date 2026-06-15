# trusty-mpm — Session Manager Daemon: MVP Spec

> **Status:** Draft · 2026-06-14
> **Author:** Bob Matsuoka
> **Crate:** `crates/trusty-mpm/` (edition 2024, `publish = false`)
> **Parent epic:** [#380](https://github.com/bobmatnyc/trusty-tools/issues/380)
> **Relationship to roadmap:** Refocuses the M1 "standalone metaharness POC" milestone;
> the Phase-1 agent/skill content parity work is orthogonal and continues unchanged.
> **Companion docs:** [PRD.md](./PRD.md) · [ARCHITECTURE.md](./ARCHITECTURE.md) ·
> [Gap Analysis](../research/trustympm-gap-analysis-decision-2026-06-05.md)

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

The daemon stops being a coordinator *of* sessions that announce themselves via
hooks, and becomes a **supervisor+spawner+observer** of sessions it *creates and
names itself*. The operator interface is a `tm session` CLI backed by the same HTTP
daemon. The daemon is the only party that knows the naming convention and can
reconcile reality on restart.

---

## 2. Goals / Non-Goals

### Goals (MVP)

1. A `tm session new` command that: assembles instructions + deploys agents/skills,
   creates a named tmux session, and spawns the interactive Claude Code CLI inside
   it — using the operator's Max OAuth credentials (not an API key).
2. A `tm session ls` command that lists all daemon-managed sessions with name and
   state.
3. A `tm session send <id> "<text>"` command that injects text into a live session
   pane.
4. A `tm session activity <id>` command that returns an LLM-classified activity
   verdict and per-check cost metrics.
5. A `tm session attach <id>` command that prints the `tmux attach-session` command.
6. A `tm session stop <id>` command that kills a managed session.
7. Daemon reconciliation on restart: adopt existing managed sessions, detect
   orphaned sessions, detect dead sessions.
8. Unit tests with a fake tmux driver and fake LLM client; one `#[ignore]` live
   smoke test.

### Non-Goals for MVP (explicit)

These items are **deferred, not deleted**. Code that supports them is left in place
or behind a seam; it is simply not exercised in the MVP.

| Item | Status |
|------|--------|
| Hook relay (overseer/hook relay stays dormant) | DORMANT — keep existing code, add no new hook wiring |
| Monitor dashboard / TUI integration for session-manager | DEFERRED |
| Socket.IO niceties | DEFERRED |
| Inter-session IPC (sessions communicating with each other) | DEFERRED |
| Dispatcher mode (daemon routes tasks to sessions) | DEFERRED — sessions self-drive from assembled instructions |
| trusty-code (tcode) runtime adapter | DEFERRED — seam defined, not implemented |
| Remote agent/skill registry (#387/#388) | DEFERRED — local deploy only |
| Daemon-enforced circuit breakers (#393) | DEFERRED |
| Per-agent model overrides (#394) | DEFERRED |

---

## 3. Architecture

### The model

One Rust daemon holds all supervision logic. N tmux sessions run independently;
each pane runs a **RuntimeAdapter** implementation. For MVP the only adapter is
`ClaudeCodeAdapter`, which spawns the interactive `claude` CLI. The daemon never
runs in each session — it is purely a control plane.

```
┌──────────────────────────────────────────────────────────────────┐
│   ONE MACHINE (single-instance enforced via daemon.lock)          │
│                                                                  │
│   tm session new/ls/send/activity/attach/stop                    │
│       │                                                          │
│       │  HTTP (loopback :7880, discovered via ~/.trusty-mpm/daemon.lock)
│       ▼                                                          │
│   ┌──────────────────────────────────────────────────┐          │
│   │   trusty-mpm daemon (Arc<DaemonState>)            │          │
│   │                                                  │          │
│   │   session_manager/         (NEW module)           │          │
│   │   ├── SessionRecord        (id, name, cwd, state) │          │
│   │   ├── SessionManager       (CRUD + reconcile)     │          │
│   │   └── reconcile_on_boot()  (adopt / detect orphan)│          │
│   │                                                  │          │
│   │   runtime/                 (NEW module)           │          │
│   │   ├── RuntimeAdapter trait (spawn / is_alive)     │          │
│   │   ├── ClaudeCodeAdapter   (MVP — ONLY this one)   │          │
│   │   └── TrustyCodeAdapter   (DEFERRED — seam only)  │          │
│   │                                                  │          │
│   │   activity/               (NEW module)            │          │
│   │   ├── PaneHash            (SHA256 of pane tail)   │          │
│   │   ├── ActivityCache       (last hash + verdict)   │          │
│   │   └── ActivityMonitor     (capture → hash → LLM)  │          │
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
│   │  pane: claude (interactive)     pane: claude (interactive) │  │
│   │  worktree: /path/to/wt-A        worktree: /path/to/wt-B  │  │
│   └──────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

### Module boundaries

New code lives under three focused modules inside `crates/trusty-mpm/src/`:

```
src/
├── session_manager/
│   ├── mod.rs          (re-exports; ~50 SLOC)
│   ├── record.rs       (SessionRecord, ManagedSessionState; ~80 SLOC)
│   ├── manager.rs      (SessionManager: new/ls/stop/reconcile; ~200 SLOC)
│   └── store.rs        (on-disk JSON persistence for crash recovery; ~100 SLOC)
├── runtime/
│   ├── mod.rs          (RuntimeAdapter trait + factory; ~60 SLOC)
│   └── claude_code.rs  (ClaudeCodeAdapter: spawn via TmuxService; ~80 SLOC)
└── activity/
    ├── mod.rs          (re-exports; ~40 SLOC)
    ├── cache.rs        (ActivityCache: hash + verdict + cost tally; ~100 SLOC)
    └── monitor.rs      (ActivityMonitor: capture → hash → LLM; ~180 SLOC)
```

All files stay under the 500-SLOC production cap. Existing modules are reused as-is
or extended minimally (new routes in `daemon/api.rs`).

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

Concrete implementation in `ClaudeCodeAdapter::spawn`:

```rust
// Why: Max OAuth path requires ANTHROPIC_API_KEY to be absent.
// If the daemon's own env has the key (e.g. from a shell export), we must
// explicitly unset it for the child so Claude Code uses ~/.claude credentials.
Command::new("env")
    .arg("-u")
    .arg("ANTHROPIC_API_KEY")
    .arg("tmux")
    .args(["send-keys", "-t", &tmux_name, "claude", "Enter"])
    // OR: use TmuxDriver::send_line with the env prefix in the shell command
```

In practice the cleanest approach is to have the daemon send the shell command
`env -u ANTHROPIC_API_KEY claude` as literal text into the tmux pane, so the
unsetting happens in the user's shell inside the pane, not in the daemon process.

### Why not headless (`claude -p`)

`claude -p` is a single-turn exchange: it sends one prompt, gets one response, and
exits. It cannot maintain a conversation across many tool-use cycles or await user
confirmation prompts. The interactive CLI inside a tmux pane is a durable process
that runs indefinitely, handling multi-turn flows, permission prompts, and tool-use
cycles naturally. `send-keys` injection and `capture-pane` observation are the
control primitives that make this model work for autonomous sessions.

---

## 5. Session Lifecycle & Naming

### Naming convention

The daemon derives tmux session names using the **existing** convention from
`src/core/names.rs`:

- Default: `tmpm-<adjective>-<noun>` via `name_from_uuid(&session_id)` — e.g.
  `tmpm-quiet-falcon`, `tmpm-swift-lotus`.
- When `--cwd` is provided: `tmpm-<folder-slug>` via `name_from_dir(cwd)` — e.g.
  `tmpm-trusty-mpm`, `tmpm-ticket-1234`. Truncated to 32 chars, lowercase,
  dashes only.

The prefix `tmpm-` is the ownership marker. The daemon scans for `tmpm-` sessions
on reconciliation and treats any session with that prefix as potentially managed.

### Collision handling

`TmuxCommand::NewSession` already uses `-A` (attach-if-exists): creating a session
whose name already exists is idempotent. If the existing session is in the daemon's
registry, the new `session new` call returns the existing session record. If the
name is taken by a tmux session the daemon does not know about (e.g. from a
previous daemon crash), reconciliation adopts it first; `session new` then fails
with a clear "name already in use, use `tm session ls` to find it" error.

### Reconciliation on restart

On daemon boot, `reconcile_on_boot()` runs before accepting HTTP requests:

1. List all tmux sessions via `TmuxDriver::list_sessions()`.
2. Filter to those matching `tmpm-` prefix.
3. Cross-reference against the on-disk session store (`~/.trusty-mpm/sessions.json`):
   - In store **and** in tmux → **adopt**: mark `Active`, recover the record.
   - In store **but not** in tmux → **orphaned**: mark `Dead` (session died while
     daemon was down).
   - In tmux **but not** in store → **external tmpm session**: adopt tentatively
     as `Adopted` (may be from a previous daemon install or manual creation).

Reconciliation is logged at `info` level so the operator can see what was recovered.

---

## 6. Control API

### HTTP routes (new, added to `daemon/api.rs`)

All routes follow the existing axum pattern: `Arc<DaemonState>` injected via `State`,
JSON request/response bodies, `DaemonError` mapped to HTTP status codes.
Port and base URL are discovered from `~/.trusty-mpm/daemon.lock` exactly as the
existing client does today (`src/core/connect.rs:resolve_daemon_url`).

```
POST   /api/v1/sessions/managed          create + spawn a new managed session
GET    /api/v1/sessions/managed          list all managed sessions
GET    /api/v1/sessions/managed/{id}     get one session record
POST   /api/v1/sessions/managed/{id}/send    inject text into pane
GET    /api/v1/sessions/managed/{id}/activity  LLM activity verdict
DELETE /api/v1/sessions/managed/{id}     stop and deregister
```

**POST /api/v1/sessions/managed** — create request:

```json
{
  "task": "Implement feature #1234 — add OAuth2 support",
  "cwd": "/path/to/worktree",          // optional; defaults to $HOME
  "name_hint": "ticket-1234"           // optional; overrides name derivation
}
```

Response `201 Created`:

```json
{
  "id": "uuid-v4",
  "name": "tmpm-ticket-1234",
  "cwd": "/path/to/worktree",
  "state": "starting",
  "created_at": "2026-06-14T10:00:00Z",
  "attach_cmd": "tmux attach-session -t tmpm-ticket-1234"
}
```

**GET /api/v1/sessions/managed** — list response:

```json
{
  "sessions": [
    {
      "id": "uuid",
      "name": "tmpm-ticket-1234",
      "cwd": "/path/to/worktree",
      "state": "active",              // starting | active | idle | dead | orphaned
      "created_at": "…",
      "last_activity_at": "…"
    }
  ]
}
```

**POST /api/v1/sessions/managed/{id}/send** — inject request:

```json
{ "text": "Please summarize what you have done so far." }
```

Response `200 OK`:

```json
{ "sent": true, "tmux_name": "tmpm-ticket-1234" }
```

**GET /api/v1/sessions/managed/{id}/activity** — activity response:

```json
{
  "id": "uuid",
  "tmux_name": "tmpm-ticket-1234",
  "verdict": {
    "state": "working",               // working | idle | blocked_on_permission | errored | done
    "summary": "Running cargo test, 3 of 12 tests passed so far.",
    "confidence": 0.87
  },
  "cost": {
    "model": "openai/gpt-4o-mini",
    "input_tokens": 412,
    "output_tokens": 64,
    "latency_ms": 340,
    "total_checks": 7,
    "llm_calls_made": 5             // < total_checks because 2 were cache hits
  },
  "cache_hit": false                 // true when pane was unchanged → no LLM call
}
```

### CLI surface (`tm session`)

New `tm session` subcommand tree, added alongside existing `tm launch`, `tm hook`, etc.
Implementation lives in `src/bin/tm/commands/session.rs` (new file) and dispatches to
`DaemonClient` HTTP calls.

```
tm session new --task "<description>" [--cwd <path>] [--name <hint>]
tm session ls [--json]
tm session activity <id>
tm session send <id> "<text>"
tm session attach <id>         # prints: tmux attach-session -t <name>
tm session stop <id>
```

`attach` does **not** exec into tmux — it prints the command so the operator can
copy-paste or pipe it. This keeps the CLI stateless and avoids the ergonomic
complexity of exec-ing into a child tmux client.

---

## 7. LLM Activity Monitor

### Purpose

This component answers "what is session X doing right now?" without requiring hooks
or instrumentation inside the Claude Code process. It is the MVP substitute for a
hook-based activity feed. The secondary goal is to **measure** whether a cheap LLM
can reliably classify agent states from raw pane output, and at what cost, so the
hook-vs-LLM-monitor trade-off can be evaluated empirically.

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
                          system prompt: classification prompt (see below)
                          → structured JSON verdict
                          → update ActivityCache
                          → record per-check metrics
```

### Classification prompt (classification target)

The system prompt asks the model to return a JSON object in one of these states:

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
- `done` — task completion signal visible ("All tasks complete", "No more steps")

The monitor is **read-only and observational for MVP**. It does not auto-inject
responses to permission prompts or errors.

### Cost instrumentation

Every check (LLM or cache hit) records:

```rust
pub struct CheckMetrics {
    pub session_id: SessionId,
    pub at: DateTime<Utc>,
    pub model: String,              // "" if cache hit
    pub input_tokens: u32,          // 0 if cache hit
    pub output_tokens: u32,         // 0 if cache hit
    pub latency_ms: u64,            // 0 if cache hit
    pub cache_hit: bool,
    pub verdict_state: ActivityState,
}
```

A running tally is accumulated in `ActivityCache` and returned in every activity
response. The intent is to accumulate enough data (over a real unicorn-factory run)
to answer: "how many LLM calls per session-hour, what does it cost, and how
accurate is the verdict?"

### Runtime agnosticism

`capture-pane -p` captures whatever text is in the pane regardless of what program
is running. The monitor works identically for `claude` panes and `tcode` panes —
it reads plain text, not process metadata. This means the activity monitor is the
same code for both runtime backends.

### Dependencies

- `crates/trusty-common/src/chat/openai_compat/` — `OpenRouterProvider` (reused as-is)
- Environment: `OPENROUTER_API_KEY` (required for LLM calls), `TRUSTY_LLM_MODEL`
  (default `openai/gpt-4o-mini`)
- If `OPENROUTER_API_KEY` is unset, `activity` returns a degraded verdict
  `{ state: "unknown", summary: "OPENROUTER_API_KEY not configured" }` rather than
  failing the request.

---

## 8. Reused vs Added vs Deferred

| Concern | Code location | Status |
|---------|--------------|--------|
| tmux argv builder | `src/core/tmux.rs` | **REUSED unchanged** |
| tmux process driver | `src/daemon/tmux.rs` (`TmuxDriver`) | **REUSED unchanged** |
| tmux service facade | `src/daemon/services/tmux_service.rs` (`TmuxService::spawn_claude`, `capture`, `send_command`) | **REUSED unchanged** |
| Session naming (adjective-noun / dir-slug) | `src/core/names.rs` | **REUSED unchanged** |
| Instruction assembly pipeline | `src/core/instruction_pipeline.rs` (`build_instructions`, `assemble_system_prompt`) | **REUSED unchanged** |
| Session launch prep (deploy + MCP/hook wiring) | `src/core/session_launch/mod.rs` (`prepare_session`) | **REUSED unchanged** |
| Agent deployment (checksum/manifest) | `src/core/agent_deployer.rs` (`deploy_agents`) | **REUSED unchanged** |
| Skill deployment | `src/core/skill_deployer.rs` (`deploy_skills`) | **REUSED unchanged** |
| Daemon lock + URL discovery | `src/daemon/lock.rs`, `src/core/connect.rs` | **REUSED unchanged** |
| axum HTTP router | `src/daemon/api.rs` | **EXTENDED — new routes added** |
| DaemonState | `src/daemon/state.rs` | **EXTENDED — SessionManager field added** |
| OpenRouter/openai-compat client | `crates/trusty-common/src/chat/openai_compat/` | **REUSED unchanged** |
| HarnessAdapter / AdapterRegistry | `crates/trusty-agents-common/src/adapters/` | **INSPIRATION ONLY** — RuntimeAdapter is a simpler independent trait |
| Session manager module | `src/session_manager/` | **NEW** |
| RuntimeAdapter trait + ClaudeCodeAdapter | `src/runtime/` | **NEW (seam only for tcode)** |
| ActivityMonitor + ActivityCache | `src/activity/` | **NEW** |
| `tm session` CLI commands | `src/bin/tm/commands/session.rs` | **NEW** |
| Hook relay / overseer | `src/daemon/services/hook_service.rs`, `daemon/overseer_compose.rs` | **DORMANT — untouched** |
| trusty-code (tcode) RuntimeAdapter | `src/runtime/trusty_code.rs` | **DEFERRED — file not created** |
| TUI integration for managed sessions | `src/tui/` | **DEFERRED** |
| Inter-session IPC | N/A | **DEFERRED** |

---

## 9. MVP Definition

The smallest independently demoable slice:

1. `tm session new --task "implement OAuth2" --cwd /path/to/wt` creates a named
   tmux session, runs `prepare_session` (deploys agents/skills, assembles
   instructions), and spawns `env -u ANTHROPIC_API_KEY claude` in the pane.
2. `tm session ls` shows the session with its name and state.
3. `tm session send <id> "what have you done so far?"` injects text into the pane.
4. `tm session activity <id>` returns an LLM verdict. A second call made without
   pane change returns the cached verdict without an LLM call.
5. `tm session attach <id>` prints `tmux attach-session -t tmpm-oauth2`.
6. `tm session stop <id>` kills the tmux session and marks the record dead.
7. After a daemon restart, running sessions are re-adopted; dead ones are marked
   orphaned.

---

## 10. Acceptance Criteria

1. `tm session new --task "Implement feature #1234" --cwd /tmp/wt1` exits 0 and
   prints the session id and `attach_cmd`; a tmux session named `tmpm-wt1` exists
   (`tmux has-session -t tmpm-wt1` succeeds).

2. The tmux pane launched by criterion 1 does NOT have `ANTHROPIC_API_KEY` set
   in its environment. Verified by checking the pane output contains no "API key"
   error and by confirming the command sent was `env -u ANTHROPIC_API_KEY claude`.

3. `~/.claude/agents/` contains the composed trusty-mpm agents and `~/.claude/skills/`
   contains the skills after `tm session new` returns, matching the output of
   `prepare_session`.

4. `tm session ls` returns a list that includes the session created in criterion 1
   with `state: active` or `state: starting`.

5. `tm session send <id> "hello from test"` exits 0; the string `hello from test`
   appears in the pane output captured by `tmux capture-pane -p -t <name>` within
   2 seconds.

6. `tm session activity <id>` returns a verdict with `state` set to one of
   `{working, idle, blocked_on_permission, errored, done}` and a non-empty
   `summary`. The `cost` block contains `model`, `input_tokens`, and `latency_ms`.

7. With a pane that has been idle (no change to its last-60-line tail), a second
   call to `tm session activity <id>` returns `cache_hit: true` and does NOT
   increment `input_tokens` in the cost tally.

8. `tm session attach <id>` prints exactly `tmux attach-session -t tmpm-<slug>` to
   stdout and exits 0.

9. `tm session stop <id>` exits 0; `tmux has-session -t <name>` returns non-zero
   afterwards; a subsequent `tm session ls` shows the record with `state: dead`.

10. After `tm daemon restart` (SIGTERM + relaunch), `tm session ls` shows:
    - Previously active sessions as `active` (they were re-adopted from tmux).
    - Sessions whose tmux session was killed before the restart as `orphaned`.

11. Unit tests pass without a real tmux binary or real OpenRouter key:
    - `FakeTmuxDriver` implements the same send/capture interface as `TmuxDriver`
      and is injected via a trait bound.
    - `FakeLlmProvider` returns a fixed `ActivityVerdict` and records call count,
      enabling the cache-hit test in criterion 7.

12. One `#[ignore]` smoke test (`test_live_session_e2e`) creates a real tmux
    session, runs `echo hello`, captures it, calls the real activity monitor
    against OpenRouter, and asserts the verdict state is `idle` or `working`.
    Requires: `tmux` installed, `OPENROUTER_API_KEY` set.

---

## 11. Risks & Open Questions

| Risk | Description | Mitigation |
|------|-------------|------------|
| Activity-detection accuracy | A 60-line pane tail may not contain enough signal for a cheap model to classify state reliably. `errored` and `done` may be hardest. | Accumulate real data from a unicorn-factory run; adjust tail size and prompt; measure per-state accuracy before committing to a polling interval. |
| Shared Max rate-limit contention | N concurrent `claude` sessions share one Max OAuth account's rate limit. Heavy use across many sessions may trigger 429s inside the panes, which look like `errored` to the monitor. | Watch for rate-limit strings in pane text; add a `rate_limited` verdict state in v2. The flat-rate Max model does not have per-token limits but does have per-minute request limits. |
| Daemon-restart session recovery | If a managed session's pane is interactive (`claude` is waiting for input), re-adopting it on restart is safe. If it is mid-task (running tool calls), re-adoption is transparent to the pane. The record is recovered from `sessions.json`. | Test with a session mid-run; verify that `tmux has-session` and `capture-pane` still work after daemon restart. |
| Security of the inject API | `POST /sessions/managed/{id}/send` injects arbitrary text into a live `claude` pane. A compromised local service or accidental exposure could inject malicious instructions. | Bind daemon to `127.0.0.1` only (existing behavior). No auth for MVP (the operator's machine). Log every inject call at `info` level. Add auth as a follow-up if multi-user scenarios arise. |
| `env -u` shell portability | The env-unset pattern works on macOS and Linux. Windows is not a target. | Document macOS/Linux only requirement; `TmuxDriver::is_available()` gates the feature. |

### Open Questions

1. Should `tm session new` block until the `claude` prompt is visible in the pane
   (polling `capture-pane`) or return immediately with `state: starting`?
   Recommendation: return immediately; let the operator poll `activity` or attach.

2. Should `sessions.json` be stored in `~/.trusty-mpm/sessions.json` (alongside
   `daemon.lock`) or in a separate `~/.trusty-mpm/session-manager/sessions.json`
   to avoid collision with the existing session registry in `DaemonState`?
   Recommendation: separate subdirectory to avoid confusion with the hook-based
   session registry.

3. The activity monitor uses a fixed 60-line tail. Should this be configurable
   per-check via a query parameter (`?lines=N`)? Recommendation: yes, with a
   default of 60 and a cap of 200.

---

## 12. Relationship to Existing Roadmap

### How this fits

This MVP refocuses the **M1 "standalone metaharness POC" milestone** of epic #380.
Instead of continuing to close Python-parity gaps in the single-session hook-relay
model, M1 now delivers a multi-session daemon that solves the unicorn-factory problem
first.

The existing PRD roadmap phases are **not cancelled**:

| Phase | Original work | Status after MVP |
|-------|--------------|-----------------|
| Phase 0 — Fidelity | #385, #383, #389, #390, #394, #395 | Continues independently; `prepare_session` improvements directly benefit `tm session new` |
| Phase 1 — Content parity | #387, #388, B1/B2 agent/skill catalog | Continues; agents deployed by `tm session new` via `prepare_session` |
| Phase 2 — Enforcement & robustness | #393, #391, #392, #384 | Continues; hooks stay dormant in MVP |
| Phase 3 — Ecosystem surfaces | B4/B5/B6/B7 ticketing, auto-config, postmortem | Continues |

The decision to keep the hook relay **dormant** (not deleted) means Phase 2 can
activate CB enforcement once the MVP ships — the enforcement point in
`HookService::process` (`src/daemon/services/hook_service.rs`) is intact.

The activity monitor's LLM approach is a **parallel experiment** to the hook relay,
not a replacement. Once data is collected, the team can decide whether to activate
hook-based monitoring for sessions that run `claude` with hooks wired (the existing
`write_project_hooks` path in `prepare_session`), use LLM polling, or both.

---

## Appendix: House Conventions for New Code

All new files must follow workspace conventions:

- **Why/What/Test doc pattern** on every public item (see `CLAUDE.md`).
- **No `unwrap()` in library code** — `thiserror` errors in the `session_manager`,
  `runtime`, and `activity` modules; `anyhow::Result` in the `session.rs` CLI command.
- **500-SLOC production cap** — each new file listed in section 3 is budgeted under
  500 SLOC. If any module approaches the limit during implementation, split before
  merging.
- **Logs to stderr** — no `println!` in daemon code; all tracing goes to
  `tracing::info!` / `warn!` / `error!`.
- **Feature gates** — the activity monitor imports `trusty-common`'s chat module.
  If `trusty-common` adds the `chat` feature behind a flag, gate the monitor
  dependency accordingly.
- **MSRV 1.91, edition 2024** — let-chains are available.
