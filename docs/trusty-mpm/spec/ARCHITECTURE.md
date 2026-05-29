# trusty-mpm — System Architecture

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** code/docs/tickets audit
> **Crate:** `crates/trusty-mpm/` (version `0.5.0`, edition 2024, `publish = false`)
> **Companion docs:** [PRD.md](./PRD.md) · [COMPONENTS.md](./COMPONENTS.md)

Status tags: ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational.

---

## 1. One crate, five surfaces, one daemon

trusty-mpm is **one Cargo crate** that compiles into **multiple feature-gated
binaries** sharing a common set of library modules. A single resident daemon
(`trusty-mpmd`) is the coordination hub; the CLI, TUI, Telegram bot, GUI, and the
in-session MCP server are all **clients** of that daemon. Coordinated Claude Code
session processes connect to the daemon over HTTP (the hook relay) and are
launched as **stock `claude` processes** shaped by deployed framework artifacts —
trusty-mpm never forks the `claude` binary.

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
                          │   │  (stock `claude`, in tmux panes or native  │  │
                          │   │   Terminal.app windows) — each shaped by   │  │
                          │   │   deployed agents/skills, project CLAUDE.md,│  │
                          │   │   appended system prompt, injected trusty-* │  │
                          │   │   MCP; each can call the in-session MCP     │  │
                          │   │   server's 6 orchestration tools            │  │
                          │   └──────────────────────────────────────────┘  │
                          │                                                │
                          │   framework install: ~/.trusty-mpm/...          │
                          │   deploy targets:     ~/.claude/{agents,skills} │
                          └──────────────────────────────────────────────┘
```

Lock file: `~/.trusty-mpm/daemon.lock` records the bound address + PID so clients
resolve the live daemon and a second `trusty-mpmd` start is refused.

### Relationship to open-mpm

There is **no Cargo dependency** between trusty-mpm and open-mpm in either
direction, and neither crate imports the other. The only cross-references are
doc-comment lineage notes: the tmux helpers in `src/core/tmux.rs:6` and
`src/daemon/tmux.rs:7` were modelled on open-mpm's `tm` manager module, and
`src/bin/tm.rs:1318` notes the shared tmux-session naming convention. open-mpm is
the *orchestration engine* (it dispatches tasks to models itself, in-process);
trusty-mpm is the *coordinator of external Claude Code processes*. See
[README §Relationship to open-mpm](./README.md#relationship-to-open-mpm).

---

## 2. Multi-binary topology (single-install bundling)

The crate manifest declares one library and six `[[bin]]` targets, each gated by
the feature it needs (`crates/trusty-mpm/Cargo.toml`).

### Bin-target table

| Binary | `[[bin]]` path | Required feature | Role | Status |
|---|---|---|---|---|
| `tm` | `src/bin/tm.rs` | `cli` | Primary unified CLI: daemon control, sessions, projects, launch/connect/attach, optimizer/overseer, coordinator, services, install, hook, plus `tui`/`telegram`/`gui` subcommands. | ✅ |
| `trusty-mpm` | `src/bin/tm.rs` (same source) | `cli` | Long-name alias of `tm` — identical entry point. | ✅ |
| `trusty-mpmd` | `src/bin/trusty-mpmd.rs` | `daemon` | Backward-compatible daemon shim → `daemon::run_http` / `run_mcp`. Prefer `tm daemon`. | ✅ |
| `trusty-mpm-tui` | `src/bin/trusty-mpm-tui.rs` | `tui` | Backward-compatible TUI shim → `tui::run`. Prefer `tm tui`. | ✅ |
| `trusty-mpm-telegram` | `src/bin/trusty-mpm-telegram.rs` | `telegram` | Backward-compatible Telegram shim → `telegram::run`. Prefer `tm telegram`. | ✅ |
| `trusty-mpm-gui` | `src/bin/trusty-mpm-gui.rs` | `gui` | Thin shim → `trusty_mpm_gui::run()` (the Tauri app lives in the separate `trusty-mpm-gui` crate). Prefer `tm gui`. | ✅ (out-of-crate) |

**Single-install convention.** `cargo install trusty-mpm` builds the
`default = ["cli", "daemon"]` features, producing `tm`/`trusty-mpm` (which embeds
the daemon, TUI, and Telegram logic via the `cli → daemon + tui + telegram`
feature chain) plus the `trusty-mpmd` shim. The standalone `trusty-mpm-tui`,
`trusty-mpm-telegram`, and `trusty-mpm-gui` binaries are kept only as
backward-compatible shims; the canonical entry points are `tm <subcommand>`. All
binaries call the **same library functions**, so the five surfaces never drift.

### Feature graph

```
cli ── enables ──▶ daemon ── enables ──▶ mcp        (async-trait)
   ├─ enables ──▶ tui                                (ratatui, crossterm)
   └─ enables ──▶ telegram                           (teloxide, tokio-util)

daemon  pulls ── axum, tower, tower-http, tokio-stream, futures,
                 dashmap, parking_lot, notify, tracing-appender,
                 utoipa-swagger-ui
gui     pulls ── trusty-mpm-gui (separate Tauri crate)   ← opt-in only
```

Each optional binary is gated so a user pays the compile cost only for the
components they install. `cargo add trusty-mpm --no-default-features` yields the
libraries (`core` + `client` + `services`) with no binaries.

---

## 3. Library module map (shared core)

```
crates/trusty-mpm/src/
├── lib.rs            # re-exports: services, core, client (always);
│                     #             mcp/daemon/tui/telegram (feature-gated)
├── services/         # ALWAYS-ON: `tm services` discovery (manifest + discoverer)
├── core/             # ALWAYS-ON: domain types, deploy pipeline, instruction
│                     #            assembly, IPC, paths, lock-file/url resolution
├── client/           # ALWAYS-ON: DaemonClient (HTTP), TrustyCommand,
│                     #            CommandExecutor, CommandResult  (the one shared seam)
├── mcp/              # feature: mcp       — OrchestratorBackend trait, tool catalog, dispatch
├── daemon/           # feature: daemon    — axum API, DaemonState, hook relay,
│                     #                       watcher, reaper, discovery, lock, MCP backend
├── tui/              # feature: tui       — ratatui dashboard
├── telegram/         # feature: telegram  — teloxide bot
└── bin/              # the six [[bin]] entry points (thin)
```

| Module | Responsibility | Key files |
|---|---|---|
| `core` | Domain model + launch machinery: sessions, agents, hooks, circuit breakers, memory, overseer, **agent inheritance/deploy**, **skill deploy**, **instruction pipeline**, **session launch prep**, `FrameworkPaths`, lock-file path, daemon-URL resolution. | `agent_builder.rs`, `agent_deployer.rs`, `agent_manifest.rs`, `skill_deployer.rs`, `skill_manifest.rs`, `instruction_pipeline.rs`, `session_launch.rs`, `paths.rs`, `circuit.rs`, `memory.rs`, `delegation_authority.rs`, `connect.rs`, `bundle.rs` |
| `client` | The **single shared seam**: one HTTP transport (`DaemonClient`) + one command model (`TrustyCommand`) + one dispatcher (`CommandExecutor`) + one UI-agnostic result type (`CommandResult`). CLI, TUI, and Telegram all depend on exactly this layer. | `http_client.rs`, `command.rs`, `executor.rs`, `result.rs` |
| `mcp` | Six orchestration tools + the `OrchestratorBackend` trait (Dependency-Inversion seam) + `dispatch`. | `mcp/mod.rs`, `mcp/tools.rs` |
| `daemon` | HTTP API + shared state + hook relay + file watcher + reaper + discovery + lock + MCP backend + overseer composition + pairing store + coordinator/claude-config routes. | `daemon/api.rs` (+ `api/`), `daemon/state.rs`, `daemon/lock.rs`, `daemon/watcher.rs`, `daemon/discovery.rs`, `daemon/mcp_backend.rs`, `daemon/services/*` |
| `tui` | ratatui coordinator dashboard polling the daemon. | `tui/dashboard.rs`, `tui/client.rs`, `tui/health.rs`, `tui/iterm2.rs` |
| `telegram` | teloxide adapter → `CommandExecutor`, formatter, push-alert loop, pairing flow. | `telegram/commands.rs`, `telegram/alerts.rs`, `telegram/formatter.rs` |
| `services` | `tm services` manifest + discovery engine. | `services/manifest.rs`, `services/discoverer.rs` |

---

## 4. Single-daemon coordination model

> The heart of the product: **one daemon per machine coordinating multiple
> Claude Code session processes.**

### 4.1 Single-instance enforcement (`src/bin/tm.rs:3139-3171`)

Three layers guarantee one daemon per host:

1. **Lock-file PID validation.** `resolve_daemon_url(None)` reads the recorded
   address from `~/.trusty-mpm/daemon.lock`, validates the recorded PID is alive,
   and clears stale locks (`core/connect.rs`).
2. **`/health` probe.** If the recorded URL is non-default *and*
   `probe_health(url, "/health")` succeeds, the daemon prints "already running"
   and exits cleanly — refusing a duplicate.
3. **OS port bind.** Otherwise it binds `127.0.0.1:7880` (`TRUSTY_MPM_ADDR`
   override). On `AddrInUse` it falls back to an ephemeral port; the health-probe
   guard prevents that fallback from silently spawning a traffic-splitting second
   daemon.

After bind, `lock::write_lock` records `pid`/`addr`/optional `tailscale_addr`/
`started_at`. A shutdown task traps **SIGINT and SIGTERM** and removes the lock so
a `tm restart` (pkill → SIGTERM) never leaks a stale lock (`tm.rs:3209-3218`).

### 4.2 Session entry paths

A "session" is a Claude Code process. Sessions enter the registry by three paths:

| Path | Trigger | Code |
|---|---|---|
| **Spawn** | `POST /sessions` with a `workdir` → daemon creates the tmux host and starts `claude` via `TmuxService::spawn_claude` *before* registering (a 422/500 leaves the registry untouched). | `api.rs:303-354` |
| **Register-only** | `POST /sessions` without `workdir`, or `POST /api/v1/sessions/connect` — pure bookkeeping; the CLI owns tmux + deployment. | `api.rs:272-381` |
| **Connection-driven** | `POST /hooks` with a `SessionStart` for an unknown id → the daemon auto-registers from the incoming UUID (the session "announces itself"). | `api.rs:929-945` |

After spawn/register, the daemon discovers the real `claude` PID inside the tmux
pane in the background (`spawn_pid_capture`) so the reaper can monitor liveness;
the CLI also reports it via `PATCH /sessions/{id}/pid`. **Auto-discovery** at boot
and on `POST /sessions/discover` scans tmux panes *and* native Terminal.app `ps`
processes and adopts them (`daemon/mod.rs:84-92`).

### 4.3 The single source of truth (`daemon/state.rs:72-162`)

`Arc<DaemonState>` is injected into every axum handler **and** the MCP backend, so
HTTP clients and in-session MCP tools observe the same world:

- `sessions: DashMap<SessionId, Session>`
- `delegations: DashMap<Uuid, Delegation>`
- `breakers: DashMap<String, CircuitBreaker>`
- `memory: DashMap<SessionId, MemoryUsage>`
- `hook_history: Mutex<VecDeque<HookEventRecord>>` (ring buffer, `HOOK_HISTORY_LIMIT = 1024`)
- `projects: RwLock<HashMap<PathBuf, ProjectInfo>>`
- `overseer: Arc<dyn Overseer>` + optional `llm`
- `event_tx: broadcast::Sender<Value>` (SSE fan-out, `EVENT_CHANNEL_CAPACITY = 1024`)
- `paired_chat_id` / `pair_code` / `framework_root` (Telegram pairing, persisted)

### 4.4 Hook relay & enforcement point (`POST /hooks`, `api.rs:904-953`)

The hook forwarder (`tm hook`) is the Rust replacement for claude-mpm's per-fire
Python process: it reads minimal context from Claude Code env vars, posts a
`hook_event`, exits 0, and short-circuits when `CLAUDE_MPM_SUB_AGENT=1` (so nested
sub-agents generate no hook traffic). `POST /hooks` is the enforcement point:

1. Parse session id (malformed → 400; unknown event name → 400 at deserialization).
2. `SessionStart` for an unknown session → auto-register (connection-driven path).
3. `HookService::process` → consult the overseer on tool-use events; a `Block`
   decision returns **403** before the tool runs; every decision is audited (JSONL).
4. Compress `PostToolUse` output per the optimizer policy.
5. Append a `HookEventRecord` to the ring buffer; broadcast a JSON copy to SSE.

🔵 **Designed-not-built (FR-CB-3, #393).** `HookService::process` is the intended
hook point for daemon-enforced CB#1–#14 hard blocks; today CB#1–#14 are PM
self-discipline enforced only through instructions, and the daemon enforces only
the per-agent failure breaker.

### 4.5 Event streaming & reaping

- `GET /events` / `GET /sessions/{id}/events` — live SSE streams (15s keep-alive
  ping); `GET .../events/poll` — synchronous ring-buffer snapshots for non-SSE
  clients (`api.rs:192-218,559-593`).
- A 60s `reap_loop` + `DELETE /sessions/dead` call `reap_against`
  (`state.rs:564-634`): tmux gone → remove; tmux alive but `claude` PID dead →
  mark `Stopped`; native sessions are never tmux-reaped.

---

## 5. MCP stdio framing

The in-session MCP server (`tm daemon --mcp` / `run_mcp`) is a stdio JSON-RPC
server backed by `StateBackend` over the same `Arc<DaemonState>`
(`daemon/mod.rs:154-170`). `dispatch` (`mcp/mod.rs`) routes a JSON-RPC `Request`
to the `OrchestratorBackend` trait; the daemon supplies the concrete impl, keeping
the `mcp` module free of daemon internals (process spawning, tmux, sockets) and
thus unit-testable against a `MockBackend`.

🔴 **stdout is reserved for JSON-RPC framing.** All logging goes to **stderr**
(`init_tracing`). A stray `println!` would corrupt the protocol. This holds across
every binary, including the daemon's HTTP mode.

The six tools (`mcp/tools.rs:19-136`) are how a Claude Code session introspects and
participates in host-wide coordination from inside its own context: `session_list`,
`session_status`, `agent_delegate`, `memory_protect`, `circuit_breaker_status`,
`hook_event`. Server identity is `SERVER_NAME = "trusty-mpm"`, version =
`CARGO_PKG_VERSION`.

---

## 6. Daemon HTTP API surface

Built in `api::router` (`daemon/api.rs:72-127`). Default base
`http://127.0.0.1:7880`. OpenAPI/Swagger at `/api-docs` (+ `/openapi.json`).

| Method | Path | Purpose | Status |
|---|---|---|---|
| GET | `/health` | Liveness probe (`"ok"`). | ✅ |
| GET / POST | `/sessions` | List (optional `?project=`) / register-or-spawn. | ✅ |
| POST | `/api/v1/sessions/connect` | Register for a connect (no-deploy) launch. | ✅ |
| DELETE | `/sessions/dead` | Reap dead tmux sessions. | ✅ |
| POST | `/sessions/discover` | Auto-discover tmux + native sessions. | ✅ |
| GET / DELETE | `/sessions/{id}` | Fetch / deregister. | ✅ |
| GET | `/sessions/{id}/events[/poll]` | Live SSE / snapshot for one session. | ✅ |
| POST | `/sessions/{id}/pause` · `/resume` | Pause/resume with note. | ✅ |
| POST | `/sessions/{id}/command` | Send a line to the tmux pane, capture output. | ✅ |
| GET | `/sessions/{id}/output` · `/pane` | Capture pane scrollback (optional compress). | ✅ |
| PATCH | `/sessions/{id}/pid` | Record the `claude` PID. | ✅ |
| GET | `/sessions/{id}/files` | Per-session created-file tracking. | 🔵 #94 |
| GET / POST | `/projects`, `/projects/current`, `/projects/discover` | Project registry. | ✅ |
| GET | `/events` · `/events/poll` | Global SSE / snapshot feed. | ✅ |
| POST | `/hooks` | Universal hook relay (overseer enforcement point). | ✅ |
| GET | `/breakers` | Per-agent circuit-breaker state. | ✅ |
| GET | `/optimizer` · `/overseer` | Read framework-managed policy. | ✅ |
| POST | `/llm/chat` | Coordinator LLM chat (503 if no overseer). | ✅ |
| GET / POST | `/api/v1/coordinator/context` · `/chat` | Cross-session coordinator. | ✅ |
| GET / POST | `/tmux/sessions`, `/tmux/sessions/{name}/snapshot`, `/tmux/adopt` | Universal tmux management. | ✅ |
| GET / POST | `/claude-config*` | Claude Code config analyzer + checkpoints/profiles. | ✅ |
| POST / GET | `/pair/request` · `/confirm` · `/status` · `/reset` | Telegram pairing. | ✅ |
| GET | `/api/v1/doctor` | Full-stack diagnostic. | ✅ |
| GET | `/api-docs` (+ `/openapi.json`) | Swagger UI. | ✅ |

---

## 7. Instruction-assembly pipeline

Two layers operate (`core/instruction_pipeline.rs`):

**(a) System-prompt assembly** (`:31-72`) — `include_str!` concat at compile time:

```
PM_INSTRUCTIONS  →  WORKFLOW  →  AGENT_DELEGATION  →  BASE_PM
```

joined with `\n\n---\n\n`; `BASE_PM` is appended **last** as the non-overridable
floor (carries the Trusty tool-priority block). `install_system_prompt()` writes
the result to `~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md`;
`build_system_prompt()` reads it back and passes it to
`claude --append-system-prompt-file` (`session_launch.rs:541-579`).

**(b) Runtime merge** (`build_instructions`, `:163-204`), sections **3 → 4 → 5**:
framework `INSTRUCTIONS.md` → delegation authority generated fresh from deployed
agents (`scan_agents` + `generate_authority`) → project `CLAUDE.md` (seeded once,
never overwritten). The merged text is stashed to
`<project>/.trusty-mpm/last-instructions.md`.

🔵 **Designed-not-built (FR-IN-4).** `BASE_PM.md:17-33` advertises a 5-file project
override system (`.trusty-mpm/{INSTRUCTIONS,AGENT_DELEGATION,WORKFLOW,MEMORY,
PM_INSTRUCTIONS_DEPLOYED}.md`); no Rust code reads these — only `CLAUDE.md` is
consumed. 🟡 Defects #382 (stash diverges from actual prompt) and #383 (`tm
install` overwrites the prompt with a 4-line stub) live here.

---

## 8. Filesystem layout

```
~/.trusty-mpm/                              # framework root (FrameworkPaths::root)
├── daemon.lock                            # bound addr + PID (single-instance discovery)
├── config.toml                            # CANONICAL config (models.agents.*, [agents] sources, …)
├── pairing.json                           # Telegram chat pairing
├── logs/                                  # rolling daemon log + overseer audit JSONL
├── registry/                              # 🔵 INTENDED remote-agent cache (unused stub, #388)
└── framework/
    ├── instructions/INSTRUCTIONS.md       # assembled system prompt (regenerated)
    ├── instructions/CLAUDE.md             # user-editable stub
    ├── agents/                            # bundled agent SOURCE (fallback)
    ├── skills/                            # bundled skill SOURCE (fallback)
    └── hooks/{optimizer.toml,overseer.toml}  # framework-managed policy (watcher hot-reloads)

~/.claude/                                  # Claude Code reads these
├── agents/                                # composed agent deploy target (+ .trusty-mpm-manifest.json)
├── skills/                                # skill deploy target (+ manifest)
└── output-styles/trusty-mpm.md            # deployed output style

~/.claude-mpm/                              # heritage / services namespace
├── services.yaml                          # tm services manifest (or embedded default)
└── agents/                                # 🔵 INTENDED user-level agent source (3-level precedence; unread, #387)

<project>/                                  # per-project, written on launch prep
├── CLAUDE.md                              # seeded once, never overwritten
├── .claude/settings.json                  # outputStyle + spinner tips + trusty-memory hooks
├── .mcp.json                              # injected trusty-memory MCP server
├── .trusty-mpm/last-instructions.md       # merged-instruction stash (inspection)
└── .trusty-mpm/{INSTRUCTIONS,AGENT_DELEGATION,WORKFLOW,MEMORY,PM_INSTRUCTIONS_DEPLOYED}.md
                                            #   🔵 INTENDED project overrides (unread, FR-IN-4)
```

A source-checkout's `agents/` submodule (`<repo>/agents/{agents,skills}/`) wins
over the bundled framework directories when present (`paths.rs:181-208`).

---

## 9. Configuration & environment

**Canonical config: `~/.trusty-mpm/config.toml`** (TOML, Rust/Cargo conventions).
The `config.yaml` reference in `PM_INSTRUCTIONS.md` is a stale Python-era artifact
to be corrected (#385/#394).

| File | Read by | Status |
|---|---|---|
| `framework/hooks/optimizer.toml` | daemon `OptimizerConfig` (watcher hot-reloads) | ✅ |
| `framework/hooks/overseer.toml` | daemon `OverseerConfig` (`[llm]` gates the LLM overseer) | ✅ |
| `~/.claude-mpm/services.yaml` | `tm services` (`ServicesManifest`; embedded default fallback) | ✅ |
| `config.toml models.agents.*` | (intended) per-agent model overrides | 🟡 not read (#394) |
| `config.toml [agents] sources` | (intended) remote registry sources | 🔵 not read (#388) |

Environment: `TRUSTY_MPM_URL` (client base URL), `TRUSTY_MPM_ADDR` (daemon bind),
`TRUSTY_MPM_TAILSCALE`, `OPENROUTER_API_KEY` (LLM overseer/chat), `RUST_LOG`,
`CLAUDE_MPM_SUB_AGENT=1` (hook short-circuit), `TELEGRAM_BOT_TOKEN`.

---

## 10. Source reference index

| Concern | File:line |
|---|---|
| Crate manifest (features, bins) | `crates/trusty-mpm/Cargo.toml` |
| Library surface (gated modules) | `src/lib.rs` |
| Single-instance enforcement + daemon boot | `src/bin/tm.rs:3139-3218` |
| Lock file | `src/daemon/lock.rs` |
| Daemon boot / discovery / reaper / MCP wiring | `src/daemon/mod.rs` |
| HTTP router + handlers | `src/daemon/api.rs:72-1413` |
| Shared state (single source of truth) | `src/daemon/state.rs:72-906` |
| Session registry / reaping | `src/daemon/state.rs:476-634` |
| Overseer composition (deterministic + LLM) | `src/daemon/state.rs:193-274` |
| MCP tool catalog + dispatch | `src/mcp/mod.rs`, `src/mcp/tools.rs:19-136` |
| Shared client seam (transport/command/executor) | `src/client/mod.rs` |
| Instruction assembly (4-asset concat) | `src/core/instruction_pipeline.rs:31-72` |
| Runtime merge pipeline (3→4→5) | `src/core/instruction_pipeline.rs:163-204` |
| Session launch prep (deploy + MCP/hook wiring) | `src/core/session_launch.rs:82-579` |
| Agent inheritance composition | `src/core/agent_builder.rs:212-327` |
| Agent deploy (checksum/manifest) | `src/core/agent_deployer.rs:57-134` |
| Framework paths + submodule source | `src/core/paths.rs:74-232` |
| CLI command surface | `src/bin/tm.rs:38-197` |
| open-mpm lineage notes (no dep) | `src/core/tmux.rs:6`, `src/daemon/tmux.rs:7`, `src/bin/tm.rs:1318` |
