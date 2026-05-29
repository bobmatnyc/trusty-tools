# trusty-mpm — Product Requirements Document

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** code/docs/tickets audit
> **Crate:** `crates/trusty-mpm/` (version `0.5.0`, edition 2024, `publish = false`)
> **Companion docs:** [ARCHITECTURE.md](./ARCHITECTURE.md) · [COMPONENTS.md](./COMPONENTS.md)

Status tags: ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational.
Each requirement is framed **Vision / Current / Gap**: the intended product
surface, what the code does today, and where they diverge.

---

## 1. Vision & Mission

### North-star vision

> **One install, five surfaces. One daemon, many sessions.**

trusty-mpm is a **Rust reimplementation of the Python `claude-mpm` meta-harness**.
It runs as **a single daemon instance per machine** that **coordinates every
Claude Code session process on that host**, and ships as a **single binary**
(`tm` / `trusty-mpm`) that bundles a background daemon, an in-session MCP server,
a TUI dashboard, and a Telegram bot behind feature flags. Where the Python
`claude-mpm` spawned a fresh interpreter per hook fire and stored its framework
in `~/.claude-mpm/`, trusty-mpm runs one resident daemon (`trusty-mpmd`) that
enforces single-instance-per-machine, spawns/tracks/reaps Claude Code sessions
through an HTTP API + session registry, and carries the claude-mpm PM-orchestration
product model (delegation, circuit breakers, verification gates, a 5-phase
workflow, memory routing, hooks) as compile-time instruction assets.

### Mission

Give a developer (or a small team on one workstation) **PM-driven delegation
discipline plus a single pane of glass over every active Claude Code session**,
delivered as one offline-capable install with no external runtime. The PM
identity is never a forked `claude` binary — every session is a stock `claude`
process whose PM behaviour comes from deployed agents, deployed skills, a project
`CLAUDE.md`, an assembled system prompt passed via
`claude --append-system-prompt-file`, and injected trusty-* MCP servers.

### Why this is distinct

- **Process coordination, not a model harness.** Unlike `open-mpm` (which
  dispatches tasks to models itself), trusty-mpm coordinates *external* Claude
  Code OS processes. The two crates share the `claude-mpm` heritage and
  vocabulary but have **no Cargo dependency in either direction** (see
  [README §Relationship to open-mpm](./README.md#relationship-to-open-mpm)).
- **Single resident daemon.** One `trusty-mpmd` per host knows about and
  coordinates every session — a host-wide registry, hook relay, and event feed
  that no per-session harness can offer.
- **Single-install, offline-first.** `cargo install trusty-mpm` yields a working
  CLI + daemon + TUI + Telegram with the framework assets compiled into the
  binary; launching a configured session needs no network.

---

## 2. Goals & Non-Goals

### Goals

1. **One daemon per machine** coordinating multiple Claude Code session processes.
2. **Single-binary distribution** — `cargo install trusty-mpm` yields `tm` /
   `trusty-mpm` with the daemon, MCP server, TUI, and Telegram bot bundled behind
   feature gates.
3. **Faithful port of the claude-mpm PM product model** — delegation, circuit
   breakers, verification gates, 5-phase workflow, autonomous execution, memory
   routing.
4. **Agent + skill system** with `extends:` inheritance, idempotent
   checksum-guarded ownership-aware deployment, and (intended) remote registry
   distribution.
5. **Customizable, layered instruction assembly** — framework floor + project
   overrides.
6. **Full host-wide observability** — session registry, hook relay, live SSE
   event stream, circuit-breaker views, TUI dashboard, remote Telegram management.
7. **Offline-first** — framework ships embedded in the binary; no network to
   launch a configured session.
8. **Canonical service discovery** (`tm services`) replacing ad-hoc
   `lsof`/`curl`/`ps` in agent prompts.

### Non-Goals

- **Not a fork of Claude Code.** trusty-mpm always launches stock `claude`; it
  shapes behaviour through instructions/agents/skills/MCP, never by patching the
  binary.
- **Not a multi-machine orchestrator.** The coordination boundary is one host.
  Telegram remote management is operator access, not cross-host coordination.
- **Not an LLM provider / model harness.** Model selection is advisory PM
  instruction plus an optional LLM overseer/chat that reuses OpenRouter
  credentials. (Dispatching tasks to arbitrary models is `open-mpm`'s job.)
- **Not a replacement for the user's own `~/.claude/` files.** Deployment is
  ownership-aware and never clobbers user-authored or user-edited files.
- **kuzu-memory and static `.claude-mpm/memories` fallback are out of scope** —
  Python-era concepts. `trusty-memory` (MCP) is the sole intended memory backend.

---

## 3. Target Users / Personas

| Persona | Who | Primary surface | Needs |
|---|---|---|---|
| **CLI developer** | Solo dev running orchestrated Claude Code sessions in a terminal. | `tm` / `trusty-mpm` CLI | `tm launch` a configured PM session; `tm connect`/`attach`; `tm status`; `tm services`. |
| **MCP host integrator** | Building a Claude Code experience that introspects orchestration state from inside a session. | In-session MCP server | `session_list`, `session_status`, `agent_delegate`, `memory_protect`, `circuit_breaker_status`, `hook_event`. |
| **TUI operator** | Dev wanting one pane of glass over all sessions on the host. | `trusty-mpm-tui` (`tm tui`) | Coordinator chat + session sidebar + health panel polling the daemon. |
| **Telegram / mobile user** | Operator driving the daemon from a phone, away from the workstation. | `trusty-mpm-telegram` (`tm telegram`) | Pair a bot; list/inspect sessions; send commands; receive push alerts. |

---

## 4. Functional Requirements

Requirements are grouped by **surface** then by **capability**, each tagged with
a status. Where implementation diverges from intent, the divergence is described
inline. Detailed source citations live in [COMPONENTS.md](./COMPONENTS.md) and
[ARCHITECTURE.md](./ARCHITECTURE.md).

### 4.1 CLI surface (`tm` / `trusty-mpm`)

| Req | Intent | Status |
|---|---|---|
| FR-CLI-1 | One unified binary: daemon control (`start`/`stop`/`restart`/`status`/`daemon`), sessions, projects, `launch`/`connect`/`attach`, optimizer/overseer, coordinator, services, install, hook, and the `tui`/`telegram`/`gui` subcommands. | ✅ — full `Command` enum (`src/bin/tm.rs:38-197`). |
| FR-CLI-2 | `tm launch` deploys agents/skills/instructions + MCP config into the project, then starts `claude` as a PM. | ✅ — `Launch` → `session_launch::prepare_session`. |
| FR-CLI-3 | `tm connect`/`attach` start or attach a tmux-hosted session **without** redeploying the framework. | ✅ — `POST /api/v1/sessions/connect` register-only path. |
| FR-CLI-4 | `tm hook` posts a Claude Code lifecycle event to the daemon and exits 0; silent degradation when the daemon is down. | ✅ — short-circuits when `CLAUDE_MPM_SUB_AGENT=1`. |
| FR-CLI-5 | `tm install` deploys the framework (agents **and** skills) to `~/.claude/`. | 🟡 — deploys agents; skills deploy only on session start (#386 closed for deploy-on-install; verify). See FR-DEP-3. |
| FR-CLI-6 | `src/bin/tm.rs` stays under the 500-line cap. | 🟡 — `tm.rs` is ~4,442 lines; split tracked by **#395**. |

### 4.2 Daemon surface (`trusty-mpmd`)

| Req | Intent | Status |
|---|---|---|
| FR-DMN-1 | Exactly one daemon per machine; a second start is refused (lock-file PID validation + `/health` probe + OS port bind). | ✅ — `src/bin/tm.rs:3139-3171`, `daemon/lock.rs`. |
| FR-DMN-2 | Clients discover the daemon's live address via `~/.trusty-mpm/daemon.lock`; stale locks (dead PID) are cleared. | ✅ — `lock::write_lock`/`remove_lock` + `resolve_daemon_url`. |
| FR-DMN-3 | One shared `Arc<DaemonState>` is the single source of truth for sessions, delegations, breakers, memory, hook history, projects. | ✅ — injected into every handler + the MCP backend (`daemon/state.rs:72-162`). |
| FR-DMN-4 | Loopback JSON HTTP API for CLI/TUI/Telegram/GUI + universal hook relay + SSE event feed; OpenAPI/Swagger at `/api-docs`. | ✅ — `api::router` (`daemon/api.rs:72-127`). See ARCHITECTURE §HTTP API. |
| FR-DMN-5 | Graceful shutdown traps SIGINT **and** SIGTERM and removes the lock so `tm restart` never leaks a stale lock. | ✅ — `src/bin/tm.rs:3209-3218`. |
| FR-DMN-6 | A file watcher hot-reloads framework-managed policy (`optimizer.toml`, `overseer.toml`). | ✅ — `daemon/watcher.rs`. |

### 4.3 Session / process lifecycle

| Req | Intent | Status |
|---|---|---|
| FR-SES-1 | Register/spawn a session, track tmux name + `claude` PID, list/get/remove. | ✅ — `POST/GET/DELETE /sessions[/{id}]`, `PATCH /sessions/{id}/pid` (`api.rs:272-503`). |
| FR-SES-2 | Auto-discover existing Claude Code sessions (tmux panes + native Terminal.app `ps`) at boot and on demand — no manual adopt. | ✅ — `discovery::discover_all`, `POST /sessions/discover`. |
| FR-SES-3 | Pause/resume with a "where I left off" note that survives a daemon restart. | ✅ — `POST /sessions/{id}/pause|resume` + `session_store` disk persistence. |
| FR-SES-4 | Send a line to a session's tmux pane; capture pane scrollback (optional compression). | ✅ — `POST /sessions/{id}/command`, `GET /sessions/{id}/output|pane`. |
| FR-SES-5 | Reap dead sessions (tmux gone → remove; tmux alive but `claude` PID dead → mark Stopped); never tmux-reap native sessions. | ✅ — 60s `reap_loop` + `DELETE /sessions/dead` (`state.rs:564-634`). |
| FR-SES-6 | Surface files an agent created per session so the PM git-file-tracking protocol is observable daemon-side. | 🔵 — designed; no endpoint. Tracked by **#94** (`GET /sessions/{id}/files`). |

### 4.4 MCP tool surface (in-session)

| Req | Intent | Status |
|---|---|---|
| FR-MCP-1 | Stdio JSON-RPC MCP server exposing six orchestration tools to a Claude Code session, backed by the same `Arc<DaemonState>`. | ✅ — `tm daemon --mcp` / `run_mcp` over `StateBackend` (`mcp/tools.rs:19-136`). |
| FR-MCP-2 | `session_list` / `session_status` — enumerate sibling sessions and inspect one (uptime, tokens, agent, pressure). | ✅. |
| FR-MCP-3 | `agent_delegate` — request a delegation; daemon applies circuit-breaker + depth limits before spawning. | ✅. |
| FR-MCP-4 | `memory_protect` — report context-window usage; daemon classifies pressure (ok/warn/alert/compact). | ✅. |
| FR-MCP-5 | `circuit_breaker_status` — inspect all or one agent's breaker. | ✅. |
| FR-MCP-6 | `hook_event` — forward a Claude Code hook into the observability pipeline. | ✅. |

### 4.5 TUI surface (`trusty-mpm-tui` / `tm tui`)

| Req | Intent | Status |
|---|---|---|
| FR-TUI-1 | A ratatui app: a coordinator chat with visibility into every active session, beside a dismissable session sidebar. | ✅ — `tui` module (feature `tui`); polls the coordinator-context endpoint. |
| FR-TUI-2 | Secondary health screen showing combined search + memory health. | ✅ — `tui/health.rs` (#37 closed). |
| FR-TUI-3 | Shipped both as `tm tui` and as the backward-compatible `trusty-mpm-tui` shim. | ✅ — shim delegates to `trusty_mpm::tui::run`. |

### 4.6 Telegram surface (`trusty-mpm-telegram` / `tm telegram`)

| Req | Intent | Status |
|---|---|---|
| FR-TG-1 | Pair a bot to a daemon (request → confirm code → status/reset), persisted across restarts. | ✅ — `POST/GET /pair/*` + `pairing_store`. |
| FR-TG-2 | List/inspect sessions and send commands from a phone via the shared `CommandExecutor`. | ✅ — teloxide adapter → `TrustyCommand`. |
| FR-TG-3 | Push unsolicited alerts (pure alert-decision core) to a paired chat. | ✅ — `telegram/alerts.rs`. |
| FR-TG-4 | Bot token resolved from `--token` / `.env.local` / `.env` / `TELEGRAM_BOT_TOKEN`; `--check` validates config without connecting. | ✅ — `src/bin/trusty-mpm-telegram.rs`. |

### 4.7 PM orchestration model (instruction-driven)

| Req | Intent | Status |
|---|---|---|
| FR-PM-1 | PM delegates all work to specialist agents; default = delegate, exception only on explicit "you do it". | ✅ — `PM_INSTRUCTIONS.md` asset. |
| FR-PM-2 | Verification gates: "done" claims require evidence (file paths, commit hash, QA repro/verify, live health). | ✅ — `WORKFLOW.md` + `PM_INSTRUCTIONS.md`. |
| FR-PM-3 | 5-phase workflow (Research → Code Analysis → Implementation → QA → Documentation) with skip rules. | ✅ — instruction asset. |
| FR-PM-4 | Autonomous execution — PM runs the full pipeline; asks the user only below ~90% success probability. | ✅ — instruction asset. |
| FR-PM-5 | Model-selection protocol (haiku/sonnet/opus tiers, per-agent overrides via `~/.trusty-mpm/config.toml models.agents.*`). | 🟡 — specified in instructions; **no Rust code reads the key**. The `config.yaml` reference in `PM_INSTRUCTIONS.md` is a stale Python-era artifact — `config.toml` is canonical. Tracked by **#394**. |
| FR-PM-6 | Optional LLM overseer evaluating hook events (allow/block/respond/flag) + interactive coordinator chat. | ✅ — `DeterministicOverseer` + optional `CompositeOverseer` from `overseer.toml`; `POST /llm/chat`. Disabled by default; opt-in. |

### 4.8 Circuit breakers

| Req | Intent | Status |
|---|---|---|
| FR-CB-1 | PM-behaviour breakers (CB#1–#14, 3-strike WARNING → ESCALATION → FAILURE) block forbidden PM actions (Edit/Write, `curl`/`lsof`/`ps`/`make`, browser tools, `gh`, `sed`/`awk`, …). | 🟡 — *policy* fully specified in instructions; enforced as PM self-discipline inside the session, **not** daemon-enforced. |
| FR-CB-2 | Per-agent failure breaker (`consecutive_failures`, default 3-strike) gates delegation. | ✅ — `core/circuit.rs`, `state.rs:record_outcome`, surfaced via `GET /breakers` + `circuit_breaker_status`. |
| FR-CB-3 | **Daemon-enforced** CB#1–#14: detect violations from hook events at `POST /hooks` and return HTTP 403 (hard block) before the forbidden tool runs. | 🔵 — designed as an extension of the `HookService::process` enforcement point; per-agent breaker is the foundation. Tracked by **#393**. |

### 4.9 Agent & skill deployment

| Req | Intent | Status |
|---|---|---|
| FR-DEP-1 | Agents declare `extends:` frontmatter; chains flatten at build time into self-contained files (Claude Code has no native inheritance). All agents inherit a `BASE_AGENT`. | ✅ — `compose_agent` walks base-first, child-wins merge, cycle + depth(8) guards (`core/agent_builder.rs`). |
| FR-DEP-2 | Deployment is idempotent, checksum-guarded, manifest-tracked: never clobber user-owned or user-edited files. | ✅ — `deploy_agents`/`deploy_skills` + `AgentManifest`/`SkillManifest` (sha256, origin, source chain). |
| FR-DEP-3 | `tm install` deploys both agents and skills at install time. | 🟡 — historically agents only; skills deployed on session start (`prepare_session`). #386 addressed install-time skill deploy — verify against current `tm install`. |
| FR-DEP-4 | Robust agent frontmatter parsing (no `:`-truncation; single parser; Claude Code `model:`-injection workaround). | 🟡 — defect: parser splits on first `:` and truncates colon-bearing values; two divergent parser copies. Tracked by **#389** (truncation/dup) and **#390** (model-injection). |
| FR-DEP-5 | Atomic deploy writes; graceful handling of a corrupt manifest. | 🟡 — tracked by **#392**. |
| FR-DEP-6 | Prune stale deployed agent/skill files on rename or removal. | 🔵 — tracked by **#391**. |
| FR-DEP-7 | 3-level agent precedence: project `.claude/agents/` > user `~/.claude-mpm/agents/` > cached remote. | 🔵 — documented in `PM_INSTRUCTIONS.md`; implementation has **one source, one target**. Tracked by **#387**. |
| FR-DEP-8 | Remote agent registry: fetch agents with TTL/offline-cache; record `Origin::Registry`. | 🔵 — `Origin::Registry` is a forward-compat enum variant; `registry/` path resolves but nothing fetches. Tracked by **#388**. |

### 4.10 Instruction assembly & customization

| Req | Intent | Status |
|---|---|---|
| FR-IN-1 | Every launched session receives identical, version-controlled PM instructions (`PM_INSTRUCTIONS → WORKFLOW → AGENT_DELEGATION → BASE_PM`, `BASE_PM` last as the non-overridable floor). | ✅ — compile-time `include_str!` concat (`instruction_pipeline.rs:31-72`), passed via `--append-system-prompt-file`. |
| FR-IN-2 | Runtime merge composes framework + delegation authority (generated from deployed agents) + project `CLAUDE.md`. | ✅ — `build_instructions` (`instruction_pipeline.rs:163-204`); stash at `.trusty-mpm/last-instructions.md`. |
| FR-IN-3 | First-launch instruction stash matches the actual system prompt; `tm install` does not overwrite the assembled prompt with a stub. | 🟡 — defects: stash diverges (**#382**), `tm install` overwrites the prompt with a 4-line stub (**#383**). |
| FR-IN-4 | Project override system: `.trusty-mpm/{INSTRUCTIONS,AGENT_DELEGATION,WORKFLOW,MEMORY,PM_INSTRUCTIONS_DEPLOYED}.md` customize/replace PM behaviour. | 🔵 — advertised in `BASE_PM.md:17-33`; **no Rust code reads these files** (only `CLAUDE.md` is read). Trust gap. |
| FR-IN-5 | `PM_INSTRUCTIONS_VERSION` marker gates instruction upgrades. | 🔵 — marker present (`= 0014`) but inert. Tracked by **#384**. |

### 4.11 Memory routing & protection

| Req | Intent | Status |
|---|---|---|
| FR-MEM-1 | Recall before research; remember findings immediately via the **trusty-memory MCP backend** (sole intended backend). | ✅ — policy in instructions; wired by injecting the `trusty-memory` MCP server into project `.mcp.json` and writing `trusty-memory hooks fire …` into `.claude/settings.json` (`session_launch.rs:286-450`). |
| FR-MEM-2 | Per-session context-window pressure tracking with warn/alert/compact classification. | ✅ — `memory_protect` tool + `DaemonState::record_memory` → `MemoryPressure`. |

### 4.12 Hook handling

| Req | Intent | Status |
|---|---|---|
| FR-HK-1 | A thin hook forwarder (`tm hook`) posts every lifecycle event to the daemon and exits, replacing claude-mpm's per-fire Python process. | ✅. |
| FR-HK-2 | Universal hook relay endpoint ingests events, drives the overseer (Block → 403), compresses output, appends to a ring buffer, broadcasts via SSE. | ✅ — `POST /hooks` (`api.rs:904-953`). |
| FR-HK-3 | Project-scoped memory hooks written into project settings; global duplicates removed. | ✅ — `write_project_hooks` + `remove_global_trusty_memory_hooks`. |

### 4.13 Service discovery (`tm services`)

| Req | Intent | Status |
|---|---|---|
| FR-SVC-1 | A canonical `tm services` CLI (list/status/port/url/health/log/init/restart) backed by a YAML manifest, replacing `lsof`/`curl`/`ps` in agent prompts; `--json` + spec exit codes. | ✅ — `services` module + `assets/default-services.yaml` (`tm.rs:178-196`). See [research spec](../research/tm-services-discovery-spec-2026-05-28.md). |

---

## 5. Non-Functional Requirements

| NFR | Requirement | Status |
|---|---|---|
| NFR-1 | **Single binary / single install** — `cargo install trusty-mpm` installs everything; feature gates (`cli` → `daemon`+`tui`+`telegram`; `daemon` → `mcp`). | ✅ — `Cargo.toml [features]`/`[[bin]]`. |
| NFR-2 | **MSRV 1.88, edition 2024** (let-chains). | ✅ — workspace-shared. |
| NFR-3 | **No `unwrap()` in library code** — `thiserror` for libs, `anyhow` for binaries. | ✅ — typed errors (`AgentBuildError`, `PrepError`, `PipelineError`, `DaemonError`). |
| NFR-4 | **stderr-only logging** — stdout reserved for MCP JSON-RPC framing. | ✅. |
| NFR-5 | **Idempotent deploys** — re-running install/launch never duplicates or clobbers; checksum-guarded. | ✅ — `prepare_session_is_idempotent`. |
| NFR-6 | **Single-instance enforcement** per machine. | ✅. |
| NFR-7 | **Offline-capable** — framework assets compiled in; no network to launch. | ✅ — the unbuilt remote-registry path (FR-DEP-8) is the only thing that *would* add network. |
| NFR-8 | **500-line file cap** per source file. | 🟡 — `src/bin/tm.rs` (~4,442 lines) exceeds the cap; split tracked by **#395**. |

---

## 6. Success Criteria

1. **One daemon, many sessions** — a single `trusty-mpmd` tracks ≥ N concurrent
   Claude Code sessions (tmux + native) with accurate status and reaping.
2. **Zero clobber** — deploy runs never overwrite a user-authored/edited agent or
   skill (manifest tests pass; field reports of lost edits = 0).
3. **Launch fidelity** — a `tm launch`ed session shows `style:trusty-mpm`, has the
   trusty-memory MCP server + hooks wired, and receives the full PM system prompt
   (closing the stash/stub divergence, #382/#383).
4. **Hook throughput** — hook events ingest without blocking the user's prompt
   even during a daemon restart (silent degradation verified).
5. **Single-install** — `cargo install trusty-mpm` produces a working `tm` with
   daemon + TUI + Telegram, no extra runtime.
6. **Offline launch** — launching a configured session with no network succeeds.

---

## 7. Open Questions & Roadmap

Gap-remediation is tracked by epic
[**#380**](https://github.com/bobmatnyc/trusty-tools/issues/380) and its children.

### High-severity trust gaps (documented capability that does not exist)

- **Project override system unread** (FR-IN-4) — users following `BASE_PM.md`
  write `.trusty-mpm/*.md` files that have no effect. Either implement the reader
  in `build_instructions` or remove the claim. (#382/#383 cover the related
  stash/stub instruction defects.)
- **3-level agent precedence** (FR-DEP-7, **#387**) — `PM_INSTRUCTIONS.md`
  overstates capability; only single-source/single-target exists.

### Functional gaps

- **Daemon-enforced circuit breakers** (FR-CB-3, **#393**).
- **Remote agent registry** (FR-DEP-8, **#388**); **3-level precedence** (#387).
- **`PM_INSTRUCTIONS_VERSION` gating** (FR-IN-5, **#384**).
- **Per-agent model overrides + canonical `config.toml` loading** (FR-PM-5, **#394**).
- **Per-session file tracking** (FR-SES-6, **#94**).

### Defects

- **Frontmatter parser truncation + duplication** (FR-DEP-4, **#389**),
  **model-injection workaround** (**#390**).
- **Atomic deploy writes / corrupt-manifest handling** (FR-DEP-5, **#392**).
- **Stale-file pruning** (FR-DEP-6, **#391**).

### Hygiene

- **Purge stale `~/.claude-mpm` Python-era path references** from embedded assets
  (**#385**); **canonicalize config to `config.toml`** (**#394**).
- **Split `src/bin/tm.rs`** below the 500-line cap (**#395**).
- **Reconcile stale crate inventory + MCP tool counts** across docs (**#430**).
