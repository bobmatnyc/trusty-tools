# trusty-mpm — Component Specifications

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** code/docs/tickets audit
> **Crate:** `crates/trusty-mpm/` (version `0.5.0`, edition 2024, `publish = false`)
> **Companion docs:** [PRD.md](./PRD.md) · [ARCHITECTURE.md](./ARCHITECTURE.md)

Status tags: ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational.
Each component states **responsibility**, **key types/modules** (with `src/`
paths), **current state**, and **gaps**.

---

## A. Binaries

### 1. CLI — `tm` / `trusty-mpm` (`src/bin/tm.rs`, feature `cli`)

- **Responsibility.** The primary unified entry point. One `Command` enum
  (`tm.rs:38-197`) covers daemon control (`start`/`stop`/`restart`/`status`/
  `daemon`), session ops, project ops, `launch`/`connect`/`attach`, optimizer/
  overseer, coordinator, `services`, `install`, `hook`, and the `tui`/`telegram`/
  `gui` subcommands (which call the in-crate library modules directly).
- **Key types.** `Command` enum; `Launch` → `session_launch::prepare_session`;
  the daemon-boot + single-instance logic at `tm.rs:3139-3218`; the hook
  forwarder (`Hook` arm).
- **Current state.** ✅ Full command surface; both `tm` and `trusty-mpm` map to
  the same source. `tm tui`/`tm telegram`/`tm gui` are the canonical entry points;
  the standalone shim binaries are kept only for backward compatibility.
- **Gaps.** 🟡 `tm.rs` is ~4,442 lines, far over the 500-line cap — split tracked
  by **#395**. 🟡 `tm install` overwrites the assembled system prompt with a
  4-line stub (**#383**) and historically deployed agents but not skills
  (FR-DEP-3, #386).

### 2. Daemon — `trusty-mpmd` (`src/bin/trusty-mpmd.rs` shim; `src/daemon/`, feature `daemon`)

- **Responsibility.** The resident coordination hub: HTTP API + universal hook
  relay + session registry + file watcher + reaper + discovery + MCP backend +
  overseer composition + Telegram pairing store. One per machine.
- **Key types.** `DaemonState` (`daemon/state.rs:72-162`); `api::router`
  (`daemon/api.rs:72-127`); `run_http` / `run_mcp` (`daemon/mod.rs:154-170`);
  `Lock` (`daemon/lock.rs`); `watcher` (hot-reload of `optimizer.toml`/
  `overseer.toml`); `reap_loop` (`daemon/mod.rs:128-152`); service modules
  (`daemon/services/{hook_service,session_service,tmux_service,pairing_service}.rs`).
- **Current state.** ✅ All HTTP endpoints in
  [ARCHITECTURE §6](./ARCHITECTURE.md#6-daemon-http-api-surface) implemented;
  three-layer single-instance enforcement; SIGINT+SIGTERM-clean lock teardown;
  Swagger at `/api-docs`.
- **Gaps.** 🟡 Atomic deploy writes + corrupt-manifest handling (**#392**) live in
  the deploy services. 🔵 `GET /sessions/{id}/files` per-session file tracking
  (**#94**).

### 3. MCP server (`src/mcp/`, feature `mcp`; served via `tm daemon --mcp`)

- **Responsibility.** Expose six orchestration tools to an in-session Claude Code
  process over stdio JSON-RPC, backed by the live `Arc<DaemonState>`.
- **Key types.** `OrchestratorBackend` trait (`mcp/mod.rs`) — the
  Dependency-Inversion seam keeping the MCP layer free of daemon internals;
  `dispatch`; `TOOL_CATALOG`/`tool_catalog` (`mcp/tools.rs:19-136`);
  `StateBackend` concrete impl (`daemon/mcp_backend.rs`); `SERVER_NAME =
  "trusty-mpm"`.
- **Current state.** ✅ Six tools: `session_list`, `session_status`,
  `agent_delegate`, `memory_protect`, `circuit_breaker_status`, `hook_event`.
  Unit-tested against a `MockBackend`. stdout reserved for framing; logs to stderr.
- **Gaps.** None functional; tool count must stay consistent with docs (**#430**).

### 4. TUI — `trusty-mpm-tui` / `tm tui` (`src/tui/`, feature `tui`)

- **Responsibility.** A ratatui app: a coordinator chat with visibility into every
  active session, beside a dismissable session sidebar and a health panel.
- **Key types.** `tui::run(url, interval_ms)`; `dashboard` (panes), `client`
  (HTTP), `health` (combined search+memory health, #37), `iterm2` (terminal
  integration). Polls the daemon's coordinator-context endpoint on a timer and
  POSTs to the coordinator-chat endpoint.
- **Current state.** ✅ Live dashboard; rendering + client unit-tested. Shipped as
  `tm tui` (canonical) and the `trusty-mpm-tui` shim.
- **Gaps.** None known.

### 5. Telegram bot — `trusty-mpm-telegram` / `tm telegram` (`src/telegram/`, feature `telegram`)

- **Responsibility.** Remote management from a phone: pair a bot, list/inspect
  sessions, send commands, approve permission requests, inspect overseer/tmux,
  and receive push alerts.
- **Key types.** `run` (boots the teloxide dispatcher); `TelegramCommand` + its
  `From`/conversion into the shared `TrustyCommand` (`telegram/commands.rs`);
  `TelegramFormatter` (`telegram/formatter.rs`); the pure alert-decision core
  `AlertConfig`/`LastSeen` (`telegram/alerts.rs`); `BotOptions`. A *thin adapter*
  over `client::CommandExecutor` — all daemon I/O lives in the shared client.
- **Current state.** ✅ Pairing (`/pair/*` + `pairing_store`), command dispatch,
  formatting, push-alert loop. Token resolved from `--token` / `.env.local` /
  `.env` / `TELEGRAM_BOT_TOKEN`; `--check` validates without connecting.
- **Gaps.** None known.

### 6. GUI shim — `trusty-mpm-gui` / `tm gui` (`src/bin/trusty-mpm-gui.rs`, feature `gui`)

- **Responsibility.** Wrap the Tauri desktop app (which lives in the separate
  `trusty-mpm-gui` crate because Tauri requires its own `build.rs` +
  `tauri.conf.json`) as a `[[bin]]` target of the unified crate.
- **Key types.** A ~5-line shim calling `trusty_mpm_gui::run()`; suppresses the
  console window on Windows release builds.
- **Current state.** ✅ Out-of-crate; opt-in via the `gui` feature (optional
  dependency). Requires Tauri prerequisites (`xcode-select`, `rustup`, `pnpm`).
- **Gaps.** None in this crate; GUI logic is owned by `trusty-mpm-gui`.

---

## B. Shared library subsystems

### 7. Shared client seam — `src/client/`

- **Responsibility.** The single HTTP/command layer that CLI, TUI, and Telegram
  all share, so a new endpoint is wired once and the UIs never drift.
- **Key types.** `DaemonClient` (one HTTP transport, `http_client.rs`);
  `TrustyCommand` (one command model, `command.rs`); `CommandExecutor` (the only
  command→HTTP translator, `executor.rs`); `CommandResult` (one UI-agnostic result
  type, `result.rs`); plus the row/outcome DTOs (`SessionRow`, `BreakerRow`,
  `CoordinatorContext`, `LlmChatOutcome`, …).
- **Current state.** ✅ URL construction, command model, and executor unit-tested
  against an in-process test daemon.
- **Gaps.** None known.

### 8. Session management — `src/core/session*.rs`, `src/daemon/state.rs`

- **Responsibility.** The registry, spawn/register/discover paths, pause/resume
  persistence, command/pane I/O, and reaping that make "one daemon, many sessions"
  real.
- **Key types.** `Session` (`core/session.rs`: id, project, `workdir`, `tmux_name`,
  `pid`, `status` ∈ Starting/Active/Paused/Stopped, `origin` ∈ Tmux/Native,
  `control_model`, pause fields); `SessionId` (UUID newtype, bare-string serde);
  `session_store` (disk persistence of pause records); `session_launch`
  (deploy + MCP/hook wiring); the three entry paths + `reap_against`
  (`state.rs:303-381,564-634`).
- **Current state.** ✅ Register/spawn/get/list/remove/pause/resume/command/pane;
  boot + on-demand auto-discovery of tmux and native sessions; periodic reaping
  that preserves native sessions.
- **Gaps.** 🔵 Per-session created-file tracking (`GET /sessions/{id}/files`, #94).

### 9. Agent delegation — `src/core/agent.rs`, `delegation_authority.rs`, `mcp/tools.rs`

- **Responsibility.** Let a session request a delegation; the daemon applies
  circuit-breaker + depth limits before spawning, and tracks the delegation tree.
- **Key types.** `Delegation` (id, session, target agent, task; `core/agent.rs`),
  stored in `DaemonState.delegations`; `generate_authority` (builds the delegation
  routing table from deployed agents at launch, `delegation_authority.rs`); the
  `agent_delegate` MCP tool.
- **Current state.** ✅ Delegation requests gated by per-agent breaker + depth.
- **Gaps.** None functional; the delegation *routing table* depends on deployed
  agents, so deploy gaps (§12) bound it.

### 10. Circuit breakers — `src/core/circuit.rs`, `src/daemon/state.rs`

- **Responsibility.** Two distinct breaker concepts: (a) the **per-agent failure
  breaker** the daemon enforces, and (b) the **PM-behaviour breakers (CB#1–#14)**
  defined in instructions.
- **Key types.** `CircuitBreaker` (`consecutive_failures`, `allows_delegation()`)
  + `CircuitConfig` (default 3-strike); `DaemonState.record_outcome`; `GET
  /breakers` + the `circuit_breaker_status` MCP tool.
- **Current state.** ✅ Per-agent failure breaker tracked and surfaced. 🟡 CB#1–#14
  (block Edit/Write, `curl`/`lsof`/`ps`/`make`, browser tools, `gh`, `sed`/`awk`,
  …) are PM self-discipline enforced only through instructions.
- **Gaps.** 🔵 **Daemon-enforced CB#1–#14** — detect violations at `POST /hooks`
  (`HookService::process`) and return 403 before the tool runs (FR-CB-3, **#393**).

### 11. Memory protection & routing — `src/core/memory.rs`, `session_launch.rs`

- **Responsibility.** Per-session context-window pressure classification, plus
  wiring the trusty-memory MCP backend + memory hooks into a launched project.
- **Key types.** `MemoryUsage` / `MemoryPressure` (`used_tokens`/`window_tokens`;
  ok/warn/alert/compact via `MemoryConfig`, `core/memory.rs`);
  `DaemonState.record_memory`; the `memory_protect` MCP tool; `write_project_hooks`
  + `remove_global_trusty_memory_hooks` (`session_launch.rs:286-539`).
- **Current state.** ✅ Pressure classification; trusty-memory MCP injected into
  project `.mcp.json`; `trusty-memory hooks fire …` written into project
  `.claude/settings.json`; global duplicate hooks removed.
- **Gaps.** None. (kuzu-memory / static `.claude-mpm/memories` are **out of
  scope** — Python-era; trusty-memory MCP is the sole intended backend.)

### 12. Agent & skill deployment — `src/core/agent_builder.rs`, `agent_deployer.rs`, `agent_manifest.rs`, `skill_*`

- **Responsibility.** Compose `extends:` agent chains into self-contained files
  and deploy agents + skills into `~/.claude/` idempotently, never clobbering
  user-owned/edited files.
- **Key types.** `compose_agent` (base-first walk, child-wins frontmatter merge,
  cycle + `MAX_DEPTH = 8` guards, `agent_builder.rs:212-327`); `deploy_agents` /
  `deploy_skills`; `AgentManifest`/`SkillManifest` (`filename → {source_chain,
  sha256, deployed_at, origin}`; `Origin` ∈ Bundled/Registry/User); `FrameworkPaths`
  source resolution (`paths.rs:171-208`: `agents/` submodule wins over bundled
  framework dirs).
- **Current state.** ✅ Composition + checksum-guarded ownership-aware deploy
  (skip user-owned, safe-refresh managed-unchanged-source, skip user-edited,
  compose+write+record new). Confirmed chain `engineer → base-engineer →
  base-agent`. One source, one target.
- **Gaps.** 🟡 Frontmatter parser splits on first `:` and truncates colon-bearing
  values; two divergent parser copies (**#389**); no Claude Code `model:`-injection
  workaround (**#390**). 🟡 Non-atomic writes / corrupt-manifest handling
  (**#392**). 🔵 Stale-file pruning on rename/removal (**#391**). 🔵 3-level
  precedence (project > user > remote, **#387**). 🔵 Remote registry
  (`Origin::Registry` + `registry/` are forward-compat stubs only, **#388**).

### 13. Instruction assembly — `src/core/instruction_pipeline.rs`, `assets/instructions/`

- **Responsibility.** Produce the PM system prompt and the per-launch merged
  instruction text.
- **Key types.** `install_system_prompt` / `build_system_prompt` (compile-time
  `include_str!` concat `PM_INSTRUCTIONS → WORKFLOW → AGENT_DELEGATION → BASE_PM`,
  `:31-72`); `build_instructions` (runtime merge framework → delegation authority
  → `CLAUDE.md`, `:163-204`). `BASE_PM` is the non-overridable floor.
- **Current state.** ✅ Compile-time assembly + runtime merge; merged text stashed
  to `<project>/.trusty-mpm/last-instructions.md`.
- **Gaps.** 🔵 5-file project override system advertised in `BASE_PM.md:17-33` but
  unread (FR-IN-4 — only `CLAUDE.md` is consumed); 🔵 `PM_INSTRUCTIONS_VERSION`
  marker inert (**#384**); 🟡 first-launch stash diverges from the actual prompt
  (**#382**) and `tm install` overwrites it with a stub (**#383**).

### 14. Overseer & coordinator — `src/core/{overseer,deterministic_overseer,llm_overseer}.rs`, `src/daemon/`

- **Responsibility.** Evaluate hook events (allow/block/respond/flag) and host an
  optional interactive coordinator chat with full session context.
- **Key types.** `Overseer` trait + `DeterministicOverseer` (always on) + optional
  `CompositeOverseer`/`llm_overseer` (built from `overseer.toml [llm]`);
  `overseer_compose` (`daemon/overseer_compose.rs`); `POST /llm/chat` and the
  `/api/v1/coordinator/*` routes (`daemon/api/coordinator_routes.rs`); the `tm
  coordinator` CLI + `tui` coordinator chat.
- **Current state.** ✅ Deterministic overseer + opt-in LLM overseer (reuses
  `OPENROUTER_API_KEY`); cross-session coordinator chat. Disabled by default.
- **Gaps.** None functional (the daemon-enforced CB hard blocks in §10 would also
  flow through this enforcement point).

### 15. Service discovery — `src/services/`

- **Responsibility.** A canonical, scriptable service-discovery interface (`tm
  services`) replacing ad-hoc `lsof`/`curl`/`pgrep` hardcoded in prompts.
- **Key types.** `ServicesManifest` (the stable contract), `Discoverer` (runtime
  probe engine), `ServiceStatus`/`HealthState`, manifest validation +
  tilde-expansion helpers (`services/manifest.rs`, `services/discoverer.rs`);
  `assets/default-services.yaml` embedded fallback.
- **Current state.** ✅ Eight subcommands (list/status/port/url/health/log/init/
  restart) with `--json` + spec exit codes (`tm.rs:178-196`). Backed by
  `~/.claude-mpm/services.yaml` or the embedded default. See the
  [research spec](../research/tm-services-discovery-spec-2026-05-28.md).
- **Gaps.** None known.

---

## C. Data model summary

| Type | Purpose | Persistence |
|---|---|---|
| `Session` (`core/session.rs`) | id, project, `workdir`, `tmux_name`, `pid`, `status`, `origin`, `control_model`, pause fields. | `DashMap`; pause records via `session_store`. |
| `Delegation` (`core/agent.rs`) | id, session, target agent, task — delegation tree. | `DashMap`. |
| `CircuitBreaker` (`core/circuit.rs`) | per-agent `consecutive_failures`; default 3-strike. | `DashMap`. |
| `MemoryUsage`/`MemoryPressure` (`core/memory.rs`) | token usage; ok/warn/alert/compact. | `DashMap`. |
| `HookEventRecord` (`core/hook.rs`) | session, `HookEvent` variant, payload, timestamp. | Ring buffer (1024) + SSE. |
| `ProjectInfo` (`core/project.rs`) | path-keyed registered project. | `RwLock<HashMap>`. |
| `AgentManifest`/`ManifestEntry` (`core/agent_manifest.rs`) | `filename → {source_chain, sha256, deployed_at, origin}`. | `~/.claude/agents/.trusty-mpm-manifest.json`. |
| `SkillManifest` (`core/skill_manifest.rs`) | same ownership model for skills. | `~/.claude/skills/` manifest. |
| `ServicesManifest`/`ServiceStatus` (`services/`) | declared services + probe results. | `~/.claude-mpm/services.yaml` / embedded default. |
| Pairing record (`daemon/pairing_store.rs`) | Telegram `chat_id`. | `~/.trusty-mpm/pairing.json`. |
| `OptimizerConfig`/`OverseerConfig` | compression + oversight policy. | `framework/hooks/{optimizer,overseer}.toml`. |
