# trusty-mpm — Architecture & Technical Specification (Reconstructed)

**Date**: 2026-05-29
**Status**: Reconstructed — Draft for review
**Crate**: `crates/trusty-mpm` (version `0.5.0`, edition 2024, `publish = false`)
**Companion PRD**: [prd-2026-05-29.md](prd-2026-05-29.md)
**Sources**: reconstructed from source code, embedded heritage assets, and a 2026-05-29 core-feature code review.

> **Convention.** Throughout, **INTENDED (original product design)** marks design surface
> that the heritage assets / docs describe, and **CURRENT IMPLEMENTATION** marks what the
> Rust source actually does. File references use `path:line` against the worktree at the
> documented date.

---

## 1. Architecture Overview

trusty-mpm is one Cargo crate with feature-gated binaries and library modules. A single
resident daemon (`trusty-mpmd`) is the coordination hub; the CLI, TUI, Telegram bot, GUI,
and the in-session MCP server are all clients of that daemon. Coordinated Claude Code
session processes connect to the daemon over HTTP (hook relay) and are launched as stock
`claude` processes shaped by deployed framework artifacts.

```
                          ┌──────────────────────────────────────────────┐
                          │   ONE MACHINE  (single-instance enforced)      │
                          │                                                │
  operator ── tm CLI ─────┼────────────┐                                   │
  operator ── tm tui ─────┼──────────┐ │   HTTP (loopback :7880)           │
  phone ───── Telegram ───┼────────┐ │ │   + SSE event feed                │
  desktop ─── GUI ────────┼──────┐ │ │ │                                   │
                          │      ▼ ▼ ▼ ▼                                   │
                          │   ┌───────────────────────────────────────┐   │
                          │   │   trusty-mpmd  (resident daemon)        │   │
                          │   │   ┌─────────────────────────────────┐  │   │
                          │   │   │ Arc<DaemonState> (single source) │  │   │
                          │   │   │  sessions · delegations ·        │  │   │
                          │   │   │  breakers · memory · hook ring · │  │   │
                          │   │   │  projects · overseer · pairing   │  │   │
                          │   │   └─────────────────────────────────┘  │   │
                          │   │   axum router · hook relay · watcher ·  │   │
                          │   │   reaper · discovery · MCP backend       │   │
                          │   └───────▲───────────────▲─────────────────┘   │
                          │           │ POST /hooks   │ spawn / send-keys    │
                          │     hook forwarder        │ (tmux)               │
                          │   (`tm hook`)             ▼                      │
                          │   ┌──────────────────────────────────────────┐  │
                          │   │  coordinated Claude Code session processes │  │
                          │   │  (stock `claude`, hosted in tmux panes or  │  │
                          │   │   native Terminal.app windows)             │  │
                          │   │  ── each shaped by deployed agents/skills, │  │
                          │   │     project CLAUDE.md, appended system     │  │
                          │   │     prompt, and injected trusty-* MCP      │  │
                          │   │  ── each can call the in-session MCP       │  │
                          │   │     server's 6 orchestration tools         │  │
                          │   └──────────────────────────────────────────┘  │
                          │                                                │
                          │   framework install: ~/.trusty-mpm/...          │
                          │   deploy targets:     ~/.claude/{agents,skills} │
                          └──────────────────────────────────────────────┘
```

Lock file: `~/.trusty-mpm/daemon.lock` records the bound address + PID so clients resolve
the live daemon and a second `trusty-mpmd` start is refused.

---

## 2. Process & Coordination Model (the core)

> This is the heart of the product: **one daemon per machine coordinating multiple
> Claude Code session processes.**

### 2.1 Single-instance enforcement

**CURRENT IMPLEMENTATION** (`crates/trusty-mpm/src/bin/tm.rs:3139-3171`):

1. On `tm daemon` (HTTP mode), the daemon first calls `resolve_daemon_url(None)` to read
   the lock file's recorded address. `resolve_daemon_url` validates the recorded PID is
   alive and clears stale lock files (`core/connect.rs`).
2. If the recorded URL is not the default *and* `probe_health(url, "/health")` succeeds,
   the daemon prints `trusty-mpm daemon is already running at <url>` and exits cleanly —
   refusing to start a duplicate.
3. Otherwise it binds the configured address (`127.0.0.1:7880` default,
   `TRUSTY_MPM_ADDR` override). On `AddrInUse` it falls back to an ephemeral port
   (`127.0.0.1:0`). The health-probe guard above prevents the ephemeral fallback from
   silently spawning a traffic-splitting second daemon.
4. After bind, `lock::write_lock(base_url, tailscale_url)` writes
   `~/.trusty-mpm/daemon.lock` (TOML: `pid`, `addr`, optional `tailscale_addr`,
   `started_at`) — `daemon/lock.rs:18-35`.
5. A shutdown task traps SIGINT **and** SIGTERM and calls `lock::remove_lock()` so a
   `tm restart` (pkill → SIGTERM) never leaks a stale lock (`tm.rs:3209-3218`).

So the single-instance guarantee is enforced by **three layers**: lock-file PID validation,
`/health` probe, and the OS port bind.

### 2.2 Spawning and tracking session processes

A "session" is a Claude Code process. Sessions enter the registry by **three paths**:

| Path | Trigger | Code |
|---|---|---|
| **Spawn** | `POST /sessions` with a `workdir` (e.g. GUI "New Session") → daemon creates the tmux host and starts `claude` via `TmuxService::spawn_claude` *before* registering (a 422/500 leaves the registry untouched). | `api.rs:303-354` |
| **Register-only** | `POST /sessions` without `workdir` (CLI launch, `tm connect` → `POST /api/v1/sessions/connect`) — pure bookkeeping; the CLI/client owns the tmux + deployment work. | `api.rs:272-381` |
| **Connection-driven** | `POST /hooks` with a `SessionStart` event for an unknown id → the daemon auto-registers the session from the incoming UUID. This is how a session "announces itself". | `api.rs:929-945` |

After spawn/register, the daemon discovers the real `claude` PID inside the tmux pane in
the background (`spawn_pid_capture`) so the reaper can monitor process liveness
(`api.rs:344-348`). The CLI/daemon also reports the PID back via `PATCH /sessions/{id}/pid`.

**Auto-discovery**: at boot and on `POST /sessions/discover`, `discovery::discover_all`
scans tmux panes *and* native Terminal.app `ps` processes running Claude Code and adopts
them, so externally-started sessions appear in the dashboard without a manual adopt
(`daemon/mod.rs:84-92`, `api.rs:505-528`).

### 2.3 Session registry (the single source of truth)

`Arc<DaemonState>` holds the coordinated picture of the world
(`crates/trusty-mpm/src/daemon/state.rs:72-162`):

- `sessions: DashMap<SessionId, Session>` — every managed session.
- `delegations: DashMap<Uuid, Delegation>` — active delegation trees.
- `breakers: DashMap<String, CircuitBreaker>` — per-agent circuit-breaker state.
- `memory: DashMap<SessionId, MemoryUsage>` — latest token-usage snapshot per session.
- `hook_history: Mutex<VecDeque<HookEventRecord>>` — bounded ring buffer (`HOOK_HISTORY_LIMIT = 1024`).
- `projects: RwLock<HashMap<PathBuf, ProjectInfo>>` — registered project dirs.
- `overseer: Arc<dyn Overseer>` + optional `llm` — hook evaluation + chat.
- `event_tx: broadcast::Sender<Value>` — SSE fan-out (`EVENT_CHANNEL_CAPACITY = 1024`).
- `paired_chat_id` / `pair_code` / `framework_root` — Telegram pairing (persisted).

One `Arc<DaemonState>` is injected into every axum handler and into the MCP `StateBackend`,
so HTTP clients and in-session MCP tools observe the same state.

### 2.4 Hook relay & enforcement

The hook forwarder (`tm hook`) is the Rust replacement for claude-mpm's per-fire Python
process. It reads minimal context from Claude Code env vars, posts `hook_event` to the
daemon, exits 0, and short-circuits when `CLAUDE_MPM_SUB_AGENT=1` (so nested sub-agents
don't generate hook traffic).

`POST /hooks` (`api.rs:904-953`) is the enforcement point:

1. Parse session id (malformed → 400; unknown event name → 400 at deserialization).
2. On `SessionStart` for an unknown session → auto-register (connection-driven path).
3. Run `HookService::process` → consult the overseer on tool-use events; a `Block`
   decision returns **403** before the tool runs. Every decision is audited (JSONL).
4. Compress `PostToolUse` output per the optimizer policy.
5. Append a `HookEventRecord` to the ring buffer and broadcast a JSON copy to all SSE
   subscribers.

> **INTENDED (daemon-enforced circuit breakers).** The current overseer consults the LLM
> overseer policy for Block/Allow decisions. An intended future extension (FR-PM-2b) is for
> `HookService::process` to also detect CB#1–#14 violations directly from the hook event
> payload and return 403 as a **hard block** independent of the LLM overseer. The per-agent
> `CircuitBreaker` in `core/circuit.rs` is the partial foundation. CURRENT: CB#1–#14 are
> PM self-discipline enforced only through instructions. INTENDED: daemon-side hard enforcement.

### 2.5 Event streaming

- `GET /events` / `GET /sessions/{id}/events` — live SSE streams (per-session filtering by
  UUID substring), 15s keep-alive ping (`api.rs:192-218,559-593`).
- `GET /events/poll` / `GET /sessions/{id}/events/poll` — synchronous ring-buffer snapshots
  for non-SSE clients.

### 2.6 Reaping

A 60s `reap_loop` (`daemon/mod.rs:128-152`) and `DELETE /sessions/dead` call
`reap_dead_sessions` → `reap_against` (`state.rs:564-634`): tmux session gone → remove
the entry; tmux alive but tracked `claude` PID dead → mark `Stopped` in place; native
(non-tmux) sessions are never tmux-reaped.

---

## 3. Module Breakdown

```
crates/trusty-mpm/src/
├── lib.rs            # re-exports: services, core, client (always); mcp/daemon/tui/telegram (gated)
├── services/         # ALWAYS-ON: tm services discovery (manifest + discoverer)
├── core/             # ALWAYS-ON: domain types, deploy pipeline, instruction assembly, IPC
├── client/           # ALWAYS-ON: DaemonClient (HTTP), TrustyCommand, CommandExecutor
├── mcp/              # feature: mcp  — OrchestratorBackend trait, tool catalog, dispatch
├── daemon/           # feature: daemon — axum API, DaemonState, hook relay, watcher, lock
├── tui/              # feature: tui  — ratatui dashboard
├── telegram/         # feature: telegram — teloxide bot
└── bin/              # tm, trusty-mpmd, trusty-mpm-tui, trusty-mpm-telegram, trusty-mpm-gui
```

| Module | Responsibility | Key files |
|---|---|---|
| `core` | Domain model + the launch machinery: sessions, agents, hooks, circuit breakers, memory, overseer, **agent inheritance/deploy**, **skill deploy**, **instruction pipeline**, **session launch prep**, `FrameworkPaths`, lock-file path, daemon-URL resolution. | `agent_builder.rs`, `agent_deployer.rs`, `agent_manifest.rs`, `skill_deployer.rs`, `skill_manifest.rs`, `instruction_pipeline.rs`, `session_launch.rs`, `paths.rs`, `circuit.rs`, `memory.rs`, `delegation_authority.rs`, `connect.rs`, `bundle.rs` |
| `client` | One shared HTTP transport + command model used by TUI, Telegram, CLI. | `http_client.rs`, `command.rs`, `executor.rs`, `result.rs` |
| `mcp` | Six orchestration tools + `OrchestratorBackend` trait + `dispatch`. | `mcp/mod.rs`, `mcp/tools.rs` |
| `daemon` | HTTP API + shared state + hook relay + file watcher + reaper + discovery + lock + MCP backend + overseer composition + pairing store. | `daemon/api.rs`, `daemon/state.rs`, `daemon/lock.rs`, `daemon/watcher.rs`, `daemon/discover.rs`, `daemon/discovery.rs`, `daemon/mcp_backend.rs`, `daemon/services/*` |
| `tui` | ratatui dashboard polling the daemon. | `tui/dashboard.rs`, `tui/client.rs`, `tui/health.rs` |
| `telegram` | teloxide adapter → `CommandExecutor`, formatter, push-alert loop. | `telegram/commands.rs`, `telegram/alerts.rs`, `telegram/formatter.rs` |
| `services` | `tm services` manifest + discovery engine. | `services/manifest.rs`, `services/discoverer.rs` |

---

## 4. Data Model

| Type | Purpose | Persistence |
|---|---|---|
| `Session` (`core/session.rs`) | id (UUID), project, `workdir`, `tmux_name`, `pid`, `status` (Starting/Active/Paused/Stopped), `origin` (Tmux/Native), `control_model`, `project_path`, pause fields. | In-memory `DashMap`; pause records persisted via `session_store`. |
| `Delegation` (`core/agent.rs`) | id, session, target agent, task — delegation tree. | In-memory `DashMap`. |
| `CircuitBreaker` (`core/circuit.rs`) | per-agent `consecutive_failures`, `allows_delegation()`, `CircuitConfig` (default 3-strike). | In-memory `DashMap`. |
| `MemoryUsage` / `MemoryPressure` (`core/memory.rs`) | `used_tokens`/`window_tokens`; pressure = ok/warn/alert/compact via `MemoryConfig`. | In-memory `DashMap`. |
| `HookEventRecord` (`core/hook.rs`) | session, `HookEvent` variant, payload, timestamp. | Bounded ring buffer (1024) + SSE broadcast. |
| `ProjectInfo` (`core/project.rs`) | path-keyed registered project. | In-memory `RwLock<HashMap>`. |
| `AgentManifest` / `ManifestEntry` (`core/agent_manifest.rs`) | `filename → {source_chain, sha256 checksum, deployed_at, origin}`; `Origin` ∈ {Bundled, Registry, User}. | `~/.claude/agents/.trusty-mpm-manifest.json`. |
| `SkillManifest` / entry (`core/skill_manifest.rs`) | same ownership model for skills. | `~/.claude/skills/` manifest. |
| `ServicesManifest` / `ServiceDecl` / `ServiceStatus` (`services/`) | declared services + live probe results. | `~/.claude-mpm/services.yaml` (or embedded default). |
| Pairing record (`daemon/pairing_store.rs`) | Telegram `chat_id`. | `~/.trusty-mpm/pairing.json`. |
| `OptimizerConfig` / `OverseerConfig` | compression + oversight policy. | `~/.trusty-mpm/framework/hooks/optimizer.toml` / `overseer.toml`. |

`SessionId` is a single-field UUID newtype serialized as a bare string.

---

## 5. Instruction Assembly Pipeline

### 5.1 CURRENT IMPLEMENTATION — compile-time concat + runtime merge

Two layers operate today:

**(a) System-prompt assembly** (`instruction_pipeline.rs:31-72`):
```
PM_INSTRUCTIONS  →  WORKFLOW  →  AGENT_DELEGATION  →  BASE_PM
```
All four are `include_str!`'d at compile time and joined with `\n\n---\n\n`. `BASE_PM` is
appended **last** as the non-overridable floor (carries the Trusty tool-priority block).
`install_system_prompt()` writes the result to
`~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md`; `build_system_prompt()` reads it
back (regenerating on first run) and passes it to `claude --append-system-prompt-file`
(`session_launch.rs:541-579`).

**(b) Runtime merge pipeline** (`build_instructions`, `instruction_pipeline.rs:163-204`),
sections **3 → 4 → 5**:
1. **Framework** — the on-disk `INSTRUCTIONS.md` (missing ⇒ empty, non-fatal).
2. **Delegation authority** — generated fresh from the deployed agents in
   `~/.claude/agents/` via `scan_agents` + `generate_authority`.
3. **Project `CLAUDE.md`** — loaded, or the stub is seeded once and never overwritten.

The merged text is stashed to `<project>/.trusty-mpm/last-instructions.md` for inspection.

### 5.2 INTENDED (original product design) — project override layering

`BASE_PM.md:17-33` advertises a **5-file project override system** under `.trusty-mpm/`:

| User wants | File | Effect |
|---|---|---|
| Project rules | `.trusty-mpm/INSTRUCTIONS.md` | Appended to PM prompt |
| Agent routing | `.trusty-mpm/AGENT_DELEGATION.md` | Replaces routing table |
| Workflow phases | `.trusty-mpm/WORKFLOW.md` | Replaces default workflow |
| Memory behavior | `.trusty-mpm/MEMORY.md` | Replaces memory section |
| Full PM replacement | `.trusty-mpm/PM_INSTRUCTIONS_DEPLOYED.md` | Replaces entire PM prompt |

…plus trigger phrases ("remember/always/never for this project" → write `INSTRUCTIONS.md`)
and a `PM_INSTRUCTIONS_VERSION` marker (`PM_INSTRUCTIONS.md:1` = `0014`) for upgrade gating.

> ⚠️ **DIVERGENCE.** No Rust code reads any of these five files, and the version marker is
> inert. The only project-level input the pipeline consumes is `CLAUDE.md`. This override
> layer is intended scope, not yet built. (PRD FR-IN-4 / FR-IN-5.)

---

## 6. Agent & Skill Deployment Model

### 6.1 CURRENT IMPLEMENTATION — compose, checksum, manifest

**Composition** (`agent_builder.rs`): `compose_agent` walks the `extends:` chain base-first,
merges frontmatter child-wins (name/role/description/model), strips intermediate
frontmatter, emits one self-contained file. Guards: cycle detection and `MAX_DEPTH = 8`.
Confirmed chain: `engineer → base-engineer → base-agent`.

**Deployment** (`agent_deployer.rs` / `skill_deployer.rs`) into `~/.claude/{agents,skills}/`,
governed by `AgentManifest`/`SkillManifest` (`agent_manifest.rs`):

| Target state | Action |
|---|---|
| Absent from manifest, file exists | user-owned → **skip** |
| Managed, checksum matches deployed copy, composed == current | **unchanged** |
| Managed, checksum matches, composed differs | **safe refresh** (rewrite) |
| Managed, checksum differs from deployed copy | user-edited → **skip** |
| New trusty-mpm file | **compose + write + record** (`Origin::Bundled`) |

**Source resolution** (`paths.rs:171-208`): prefers the `agents/` git submodule
(`<root>/agents/{agents,skills}/`) in a source checkout; otherwise the bundled
`framework/{agents,skills}/`. There is exactly **one source** and **one target**.

### 6.2 INTENDED (original product design) — registry + 3-level precedence

`PM_INSTRUCTIONS.md` "Agent Deployment" describes:
- **3-level precedence**: project `.claude/agents/` > user `~/.claude-mpm/agents/` >
  cached remote (`bobmatnyc/claude-mpm-agents`).
- **Remote registry**: `Origin::Registry`, `FrameworkPaths::registry`
  (`~/.trusty-mpm/registry`), a `config.toml [agents] sources` table, fetch/TTL/offline
  caching.

> ⚠️ **DIVERGENCE.** `Origin::Registry` is a forward-compat enum variant only; `registry`
> is a resolved-but-unused path; there is no fetch, no TTL, no offline cache, no
> precedence resolution, and no `config.toml [agents]` reader. The model is single-source,
> single-target. (PRD FR-AG-4 / FR-AG-5.)
>
> ⚠️ **GAP.** `tm install` deploys agents but **not** skills — skills deploy only inside
> `prepare_session` on session start (`session_launch.rs:99-102`). (PRD FR-AG-6 / FR-SK-3.)
>
> ⚠️ **DEFECT.** The frontmatter parser splits on the first `:` and truncates values that
> contain a colon (`agent_builder.rs:123-132`); a second, divergent parser copy exists
> (gray_matter path); there is no workaround for the Claude Code `model:` frontmatter
> injection bug. (PRD FR-AG-7.)

---

## 7. Daemon HTTP API Surface

Built in `api::router` (`crates/trusty-mpm/src/daemon/api.rs:72-127`). Default base
`http://127.0.0.1:7880`. OpenAPI/Swagger served at `/api-docs`.

| Method | Path | Purpose | Status |
|---|---|---|---|
| GET | `/health` | Liveness probe (`"ok"`). | Implemented |
| GET | `/sessions` | List sessions (optional `?project=`). | Implemented |
| POST | `/sessions` | Register, or spawn when `workdir` present. | Implemented |
| POST | `/api/v1/sessions/connect` | Register for a connect (no-deploy) launch. | Implemented |
| DELETE | `/sessions/dead` | Reap dead tmux sessions. | Implemented |
| POST | `/sessions/discover` | Auto-discover tmux + native sessions. | Implemented |
| GET/DELETE | `/sessions/{id}` | Fetch / deregister a session. | Implemented |
| GET | `/sessions/{id}/events` | Live SSE stream (per session). | Implemented |
| GET | `/sessions/{id}/events/poll` | Snapshot of one session's events. | Implemented |
| POST | `/sessions/{id}/pause` · `/resume` | Pause/resume with note. | Implemented |
| POST | `/sessions/{id}/command` | Send a line to the tmux pane, capture output. | Implemented |
| GET | `/sessions/{id}/output` · `/pane` | Capture pane scrollback (optional compress). | Implemented |
| PATCH | `/sessions/{id}/pid` | Record the `claude` PID. | Implemented |
| GET/POST | `/projects`, `/projects/current`, `/projects/discover` | Project registry. | Implemented |
| GET | `/events` · `/events/poll` | Global SSE / snapshot event feed. | Implemented |
| POST | `/hooks` | Universal hook relay (overseer enforcement point). | Implemented |
| GET | `/breakers` | Per-agent circuit-breaker state. | Implemented |
| GET | `/optimizer` · `/overseer` | Read framework-managed policy. | Implemented |
| POST | `/llm/chat` | Coordinator LLM chat (503 if no overseer). | Implemented |
| GET/POST | `/api/v1/coordinator/context` · `/chat` | Cross-session coordinator. | Implemented |
| GET/POST | `/tmux/sessions`, `/tmux/sessions/{name}/snapshot`, `/tmux/adopt` | Universal tmux management. | Implemented |
| GET/POST | `/claude-config*` | Claude Code config analyzer + checkpoints/profiles. | Implemented |
| POST/GET | `/pair/request` · `/confirm` · `/status` · `/reset` | Telegram pairing. | Implemented |
| GET | `/api/v1/doctor` | Full stack diagnostic. | Implemented |
| GET | `/api-docs` (+ `/openapi.json`) | Swagger UI. | Implemented |

> **Planned.** Per-session file tracking (issue #94) — surfacing files an agent created so
> the PM's git-file-tracking protocol can be observed daemon-side — is **not** yet an
> endpoint; `set_session_pid`'s doc and `trusty_addrs()` carry follow-up markers. (Tag:
> Not-started / Planned.)

---

## 8. MCP Tool Surface

Stdio JSON-RPC server (`tm daemon --mcp` / `run_mcp`), backed by `StateBackend` over the
same `Arc<DaemonState>` (`daemon/mod.rs:154-170`). Six tools (`mcp/tools.rs:19-136`):

| Tool | Purpose |
|---|---|
| `session_list` | List sessions the daemon manages (status, cwd, delegation count). |
| `session_status` | Detailed status for one session (uptime, tokens, agent, pressure). |
| `agent_delegate` | Request a delegation; daemon applies circuit-breaker + depth limits before spawning. |
| `memory_protect` | Report context-window usage; daemon classifies pressure (ok/warn/alert/compact). |
| `circuit_breaker_status` | Inspect all or one agent's breaker. |
| `hook_event` | Forward a Claude Code hook event into the observability pipeline. |

The MCP surface is how an in-session Claude Code process introspects and participates in
host-wide coordination from inside its own context.

---

## 9. Filesystem Layout

```
~/.trusty-mpm/                              # framework root (FrameworkPaths::root)
├── daemon.lock                            # bound addr + PID (single-instance discovery)
├── config.toml                            # CANONICAL config (models.agents.*, [agents] sources, …)
├── pairing.json                           # Telegram chat pairing
├── logs/                                  # rolling daemon log + overseer audit JSONL
├── registry/                              # INTENDED remote-agent cache (unused stub)
└── framework/
    ├── instructions/INSTRUCTIONS.md       # assembled system prompt (regenerated)
    ├── instructions/CLAUDE.md             # user-editable stub
    ├── agents/                            # bundled agent SOURCE (fallback)
    ├── skills/                            # bundled skill SOURCE (fallback)
    └── hooks/{optimizer.toml,overseer.toml}  # framework-managed policy

~/.claude/                                  # Claude Code reads these
├── agents/                                # composed agent deploy target (+ .trusty-mpm-manifest.json)
├── skills/                                # skill deploy target (+ manifest)
└── output-styles/trusty-mpm.md            # deployed output style

~/.claude-mpm/                              # heritage / services namespace
├── services.yaml                          # tm services manifest (or embedded default)
└── agents/                                # INTENDED user-level agent source (3-level precedence; unread)

<project>/                                  # per-project, written on launch prep
├── CLAUDE.md                              # seeded once, never overwritten
├── .claude/settings.json                  # outputStyle + spinner tips + trusty-memory hooks
├── .mcp.json                              # injected trusty-memory MCP server
├── .trusty-mpm/last-instructions.md       # merged-instruction stash (inspection)
└── .trusty-mpm/{INSTRUCTIONS,AGENT_DELEGATION,WORKFLOW,MEMORY,PM_INSTRUCTIONS_DEPLOYED}.md
                                            #   INTENDED project overrides (unread)
```

Source-checkout source: `<repo>/agents/{agents,skills}/` (the `agents/` submodule) wins
over the bundled framework directories when present (`paths.rs:181-208`).

---

## 10. Configuration

**Canonical config file: `~/.trusty-mpm/config.toml`** (TOML, matching Rust/Cargo
conventions). This is the single source of configuration truth for the Rust harness,
covering `[agents]`, `[skills]`, `models.agents.*`, and any future `[registry]` sources.

> **Stale artifact.** `PM_INSTRUCTIONS.md` references `~/.trusty-mpm/config.yaml` for
> per-agent model overrides. That path is a documentation artifact from the Python-era
> `claude-mpm` harness and has never been read by Rust code. It must be corrected to
> `config.toml` in a future pass over `PM_INSTRUCTIONS.md`. All design and spec references
> use `config.toml` as canonical.

| File | Read by | Status |
|---|---|---|
| `~/.trusty-mpm/framework/hooks/optimizer.toml` | daemon (`OptimizerConfig`, hot-reloaded by the watcher) | Implemented |
| `~/.trusty-mpm/framework/hooks/overseer.toml` | daemon (`OverseerConfig`; `[llm]` section gates LLM overseer) | Implemented |
| `~/.claude-mpm/services.yaml` | `tm services` (`ServicesManifest`; embedded `assets/default-services.yaml` fallback) | Implemented |
| `~/.trusty-mpm/config.toml` `models.agents.*` | (intended) per-agent model overrides | **Partial** — `config.toml` is canonical; the reference in `PM_INSTRUCTIONS.md` to `config.yaml` is a stale documentation artifact to be corrected; no Rust code currently reads model overrides from either path |
| `~/.trusty-mpm/config.toml` `[agents] sources` | (intended) remote registry sources | **Stub** — never read |

Environment: `TRUSTY_MPM_URL` (client base URL), `TRUSTY_MPM_ADDR` (daemon bind),
`TRUSTY_MPM_TAILSCALE`, `OPENROUTER_API_KEY` (LLM overseer/chat), `RUST_LOG`,
`CLAUDE_MPM_SUB_AGENT=1` (hook short-circuit), `TELEGRAM_BOT_TOKEN`.

---

## 11. Distribution

Single-binary, single-install convention (`Cargo.toml`):

- **Features**: `default = [cli, daemon]`; `cli → daemon + tui + telegram`;
  `daemon → mcp` (+ axum/tower/dashmap/notify/…); `tui`, `telegram`, `gui` (separate
  Tauri crate) opt-in.
- **`[[bin]]` targets**: `tm` and `trusty-mpm` (both `src/bin/tm.rs`, feature `cli`),
  `trusty-mpmd` (feature `daemon`), `trusty-mpm-tui`, `trusty-mpm-telegram`,
  `trusty-mpm-gui` (back-compat shims; prefer `tm tui` / `tm telegram` / `tm gui`).
- **Library surfaces**: `core` + `client` + `services` always; `mcp`/`daemon`/`tui`/
  `telegram` gated.

`cargo install trusty-mpm` therefore installs the CLI plus a bundled daemon, MCP server,
TUI, and Telegram bot — one install target, no external runtime, framework assets compiled
in (offline-capable).

---

## 12. Gaps & Deviations from Intent

Prioritized; each tagged with status + severity (impact on the product vision).

| # | Item | Status | Severity | Notes |
|---|---|---|---|---|
| 1 | **Project override system** (`.trusty-mpm/{INSTRUCTIONS,AGENT_DELEGATION,WORKFLOW,MEMORY,PM_INSTRUCTIONS_DEPLOYED}.md`) advertised in `BASE_PM.md` but unread. | Not-started (divergent) | **High** | Users following the in-prompt instructions write files that have no effect — a correctness/trust issue. Either implement the reader in `build_instructions` or remove the claim from `BASE_PM.md`. |
| 2 | **3-level agent precedence** (project > user > cached remote) documented but only single-source/single-target exists. | Not-started (divergent) | **High** | Same trust gap as #1; `PM_INSTRUCTIONS.md` "Agent Deployment" overstates capability. |
| 3 | **Remote agent registry** (`Origin::Registry`, `registry/` path, `config.toml [agents]`, fetch/TTL/offline). | Stub | **Medium** | Forward-compat only. Blocks distributing agents without a source checkout/bundled assets. |
| 4 | **`tm install` does not deploy skills** (only agents); skills deploy on session start. | Partial / Gap | **Medium** | A user who runs `install` then inspects `~/.claude/skills/` finds it empty until first launch. |
| 5 | **Frontmatter parser truncates `:`-containing values; two divergent parser copies; no `model:`-injection workaround.** | Partial / Defect | **Medium** | Risks dropping/garbling agent metadata (esp. descriptions). Consolidate on one parser; round-trip test colon values. |
| 6 | **Model-selection per-agent overrides** (`~/.trusty-mpm/config.toml models.agents.*`) intended but not read by Rust. The `PM_INSTRUCTIONS.md` reference to `config.yaml` is a stale artifact — `config.toml` is canonical. | Partial | **Low–Medium** | Advisory-only today; instruction text implies enforcement. Correct `PM_INSTRUCTIONS.md` to reference `config.toml`. |
| 7 | **`PM_INSTRUCTIONS_VERSION` marker** present but inert (no upgrade gating). | Stub | **Low** | Needed if/when instruction-version migration is built. |
| 8 | **kuzu-memory / static `.claude-mpm/memories`**: these were Python-era `claude-mpm` concepts. `trusty-memory` (MCP) is the sole intended memory backend. | **Out of scope** | N/A | Not a gap — explicitly excluded. trusty-memory MCP wiring is fully implemented. |
| 9 | **PM-behaviour circuit breakers (CB#1–#14)** are currently instruction-driven inside the session; the daemon tracks only a per-agent failure breaker. Daemon-side hook-driven hard enforcement is an **intended future requirement** (FR-PM-2b). | Not-started | **Medium** | The per-agent `CircuitBreaker` in `core/circuit.rs` and the `POST /hooks` enforcement point are the partial foundation. Implementing CB#1–#14 detection + 403 return in `HookService::process` would close this gap. |
| 10 | **Per-session file tracking** (issue #94) — no daemon endpoint. | Not-started / Planned | **Low** | Would let the daemon observe the PM's git-file-tracking protocol. |

---

## 13. Source Reference Index

| Concern | File:line |
|---|---|
| Crate manifest (features, bins) | `crates/trusty-mpm/Cargo.toml` |
| Library surface (gated modules) | `crates/trusty-mpm/src/lib.rs` |
| Single-instance enforcement + daemon boot | `crates/trusty-mpm/src/bin/tm.rs:3139-3218` |
| Lock file | `crates/trusty-mpm/src/daemon/lock.rs` |
| Daemon boot / discovery / reaper / MCP | `crates/trusty-mpm/src/daemon/mod.rs` |
| HTTP router + handlers | `crates/trusty-mpm/src/daemon/api.rs:72-1413` |
| Shared state (single source of truth) | `crates/trusty-mpm/src/daemon/state.rs:72-906` |
| Session registry / reaping | `crates/trusty-mpm/src/daemon/state.rs:476-634` |
| Overseer composition (deterministic + LLM) | `crates/trusty-mpm/src/daemon/state.rs:193-274` |
| MCP tool catalog | `crates/trusty-mpm/src/mcp/tools.rs:19-136` |
| Instruction assembly (4-asset concat) | `crates/trusty-mpm/src/core/instruction_pipeline.rs:31-72` |
| Runtime merge pipeline (3→4→5) | `crates/trusty-mpm/src/core/instruction_pipeline.rs:163-204` |
| Session launch prep (deploy + MCP/hook wiring) | `crates/trusty-mpm/src/core/session_launch.rs:82-579` |
| Agent inheritance composition | `crates/trusty-mpm/src/core/agent_builder.rs:212-327` |
| Agent deploy (checksum/manifest) | `crates/trusty-mpm/src/core/agent_deployer.rs:57-134` |
| Skill deploy | `crates/trusty-mpm/src/core/skill_deployer.rs` |
| Manifest + `Origin::Registry` stub | `crates/trusty-mpm/src/core/agent_manifest.rs:34-63` |
| Framework paths + submodule source | `crates/trusty-mpm/src/core/paths.rs:74-232` |
| Project override claim (unread) | `crates/trusty-mpm/src/assets/instructions/BASE_PM.md:17-33` |
| PM model (delegation/CB/workflow) | `crates/trusty-mpm/src/assets/instructions/PM_INSTRUCTIONS.md` |
| CLI command surface | `crates/trusty-mpm/src/bin/tm.rs:38-197` |
| `tm services` spec | `docs/trusty-mpm/research/tm-services-discovery-spec-2026-05-28.md` |
