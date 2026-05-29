# open-mpm — Component Specifications

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** research synthesis + code/docs/tickets audit

**Status legend:** ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational

One section per major subsystem. Each states **Responsibility**, **Key
types/modules** (with `src/` paths), **Current state**, and **Known gaps**,
framed Vision / Current / Gap. For the cross-cutting dispatch model see
[ARCHITECTURE.md §5](./ARCHITECTURE.md); for product framing see
[PRD.md](./PRD.md).

---

## 1. PM Orchestration / CTRL — `src/ctrl/`

**Responsibility.** The persistent controller that coordinates everything: it
owns the per-project PM actors, the event bus, the process tracker, the HTTP
API, and the CLI socket listener. The PM actor reads input, calls the LLM with a
`delegate_to_agent` tool, and routes work to sub-agents.

**Key types/modules.**
- `src/ctrl/state.rs` — controller state.
- `src/ctrl/config.rs` — controller/CTRL configuration.
- `src/ctrl/ctrl_turn/` — the CTRL-scope LLM turn (split from `mod.rs`).
- `src/ctrl/pm_task/` — PM-scope task execution (`run_pm_task_with_history`, `run_pm_task_with_persona`).
- `src/ctrl/repl/` — REPL glue for the controller.
- `src/ctrl/socket.rs`, `src/ctrl/socket_listener.rs` — UNIX socket controller protocol.
- `src/ctrl/handlers/`, `src/ctrl/supervisor/`, `src/ctrl/claude_cli.rs`.
- `PmHandle` map — one tokio task per connected project.
- Tool registries: `build_ctrl_registry()` (CTRL) + inline PM registry.

**Current state.** ✅ Multi-project coordination works: `/connect <path>` wires
the REPL to a project PM, and several projects run concurrently. The UNIX socket
protocol lets a second invocation act as a CLI client to a running controller.
The credential-correct PM path is `run_pm_task_with_history`.

**Known gaps.**
- 🟡 **Credential-routing bug:** `ctrl_chat_turn` (legacy stdin REPL,
  `crates/open-mpm/src/ctrl/ctrl_turn/dispatch.rs:48`) hardcodes `CTRL_MODEL` and calls `llm::chat()` without
  `pick_credentials()`, so it always routes via OpenRouter. (#408; supersedes pre-refactor `src/ctrl/mod.rs:3391` from #358–#366 cap-sweep module split.) (PRD FR-1.4).
- 🟡 **No singleton enforcement:** two near-simultaneous invocations can race on
  the socket instead of the second auto-routing (PRD FR-1.3).
- 🟡 **Oversized file:** `src/ctrl/mod.rs` ~5,730 lines (#170), partially split.
- 🔵 **User-level cancel unimplemented** (the `SessionCancelled` event exists).

---

## 2. Sub-Agent Subprocess Model — `src/subprocess/`, `src/ipc/`

**Responsibility.** Run delegated agent tasks with the right isolation: file/shell
agents in isolated OS subprocesses (crash isolation), read-only agents as
in-process tokio tasks (avoid embedder re-init).

**Key types/modules.**
- `src/subprocess/` — `SubprocessAgentRunner`, spawn helpers (re-invokes the
  binary as `open-mpm --agent <name>`).
- `src/ipc/mod.rs` — `IpcMessage` NDJSON codec.
- `AgentRunner` trait + `DispatchingAgentRunner` (selects by `RunnerKind`).
- `InProcessAgentRunner` (`src/agents/in_process_runner.rs`).

**Current state.** ✅ NDJSON IPC over stdin/stdout with stderr inherited and
separate read/write tokio tasks to avoid deadlock. `{"type":"task"}` →
`{"type":"result"}` / `{"type":"error"}` then the sub-agent exits. Isolation
policy enforced: `ShellExec`/`WriteFile` agents are subprocess-isolated;
read-only agents may run in-process.

**Known gaps.**
- 🔵 **No result summarization:** full sub-agent `content` is returned to the PM;
  there is no `attempt_completion`-style summary compression (the `summary` wire
  field is present but not the path the PM consumes). Inflates PM context
  (PRD FR-2.4).
- ⚪ **No OS-native sandbox** (seatbelt/bubblewrap) — explicit non-goal; isolation
  is process-level only.

---

## 3. Tool-Using Agents — `src/tools/`

**Responsibility.** Give agents a multi-turn LLM tool-call loop over a rich,
per-agent-gated tool set, with safe filesystem and shell access.

**Key types/modules.**
- `src/tools/registry/` — `ToolRegistry` (OpenAI-compatible schemas).
- `src/tools/traits.rs` — `ToolExecutor` trait.
- `src/tools/delegate.rs` — `delegate_to_agent`.
- `src/tools/fs_reader/`, `src/tools/write_file.rs` (atomic, `out_dir`-sandboxed),
  `src/tools/shell_exec.rs` / `shell.rs` / `run_bash.rs`.
- `src/tools/git_tools/`, `src/tools/mcp_tools/` + `mcp_service_tools.rs`,
  `src/tools/memory/` + `memory_search.rs`, `src/tools/analysis/`,
  `src/tools/ast_tools/`, `src/tools/phase_audit.rs`, `src/tools/finish_task.rs`,
  `src/tools/web_search.rs`, `src/tools/skill_loader/`, `src/tools/native_*`.

**Current state.** ✅ 30+ tools across read/write/shell/web/skills/delegation/
git/MCP/ticketing/memory. Atomic `write_file` sandboxed to `out_dir`. Per-agent
allowlists via `dispatch_gated`. Two registries: `build_ctrl_registry()` for
CTRL and an inline PM registry. AST-native tools (`src/tools/ast_tools/`) reduce
round-trips and are validated at workflow L1/L2/L3.

**Known gaps.**
- 🟡 The tag-indexed skill registry is **not threaded** into sub-agent tool
  registries (see §4). 

---

## 4. Skill Injection — `src/skills/`

**Responsibility.** Discover, select, and compose Markdown skills into agent
prompts; surface the four coding personas.

**Key types/modules.**
- `SkillRegistry` (tag index) + `SkillRegistry::auto_inject` (tag scoring).
- `SkillsLoader` (language/framework detection).
- `src/skills/llm.rs` — LLM-backed selection.
- `GlobalSkillsCache`.
- `PhaseDef.skills: Option<Vec<String>>` (per-phase skills).
- Personas via `src/agents/persona/`.

**Current state.** 🟡 Five-tier discovery
(`.open-mpm/skills/` > `.claude/skills/` > `~/.open-mpm/skills/` >
`~/.claude/skills/` > bundled). Skills are Markdown + YAML frontmatter
(`name`/`description`/`tags`). Two selection mechanisms plus LLM-backed
selection. Per-phase skills work. Four personas (engineer/hacker/vibe-coder/
novice) delivered.

**Known gaps.**
- 🟡 Skill source paths are **hard-coded** — no operator-configurable skill source
  URLs.
- 🟡 **No persistent skill index:** rebuilt each run; `GlobalSkillsCache` exists
  but is unwired; the tag-indexed `list_skills` path is implemented but
  `#[allow(dead_code)]` pending wiring; `TagSkillRegistry` is built at startup but
  not threaded into sub-agent registries.
- 🔵 No skill-effectiveness scoring / feedback loop.

---

## 5. Workflow Engine — `src/workflow/`

**Responsibility.** Run declarative, multi-phase JSON workflows
(research → plan → code → QA → observe) with per-phase agent, context template,
tool set, skill list, AST-substrate flag, and dependencies; in sequential or
parallel-wave mode.

**Key types/modules.**
- `src/workflow/config/` — `WorkflowDef` / `PhaseDef`.
- `src/workflow/engine/` — `WorkflowEngine` (prescriptive mode).
- `src/workflow/parallel.rs` — wave mode (parallel sub-tasks).
- `src/workflow/worktree.rs` — git worktree management for waves.
- `src/workflow/context.rs` — `WorkflowContext` (inter-phase template substitution).
- `src/workflow/tickets.rs`, `autopush.rs`, `resolver.rs`.
- Per-phase `ast_native: Option<bool>` with an RAII guard; `PerfCollector` for
  per-phase timing/cost.

**Current state.** ✅ Both modes implemented: **prescriptive** (sequential) and
**wave** (parallel sub-tasks with git worktrees). Per-phase AST-native substrate
toggling validated at L1/L2/L3. Inter-phase outputs flow via template
substitution. Phase ticket tracking, autopush, and per-phase perf collection
work.

**Known gaps.**
- 🟡 **Oversized file:** `src/workflow/engine.rs` ~4,965 lines (#172); being split
  into `src/workflow/engine/`.

---

## 6. Token Compression — `src/compress/`

**Responsibility.** Keep prompts inside per-model budgets using **deterministic**
NLP (no LLM calls), preserving the most relevant context.

**Key types/modules.**
- `CompressConfig` (target/max token budgets).
- Pipeline stages: tool-output filtering → dedup (`dedup_sections`) →
  sliding-window (`TokenBudget`) → stop-word removal → TF-IDF.
- `ContextManager` (trims to a `soft_threshold` fraction).
- Per-agent `[compress]` TOML.
- `src/llm/compress.rs` integration.

**Current state.** ✅ Full pipeline implemented. Model-aware context windows:
Claude 200k, GPT-5.1-codex 400k, unknown 128k.

**Known gaps.** None material at this time.

---

## 7. Memory & Search — `src/memory/`, `src/search/`, `src/context/`, `src/init/`

**Responsibility.** Provide embedded, service-free vector memory; code search;
project-index generation; and hybrid retrieval for context assembly.

**Key types/modules.**
- `src/memory/` — `RedbUsearchStore` (redb metadata + usearch HNSW),
  `SessionStore`, `CodeStore`, `FastEmbedder`, `MemoryGraph`, `TrustyBacked`.
- `SessionRegistry` (`src/session_registry.rs`).
- `src/search/` — tree-sitter code indexer + file watcher; trusty-search MCP
  integration.
- `src/context/retrieval.rs` — BM25 + embedding hybrid; `ClusterStore` (2× boost).
- `src/init/` — `ProjectInitializer` (`project-index.md`, 24 h TTL, injected into
  the PM prompt; kuzu-memory loading).

**Current state.** ✅ Embedded memory with no external services. Per-session
stores and registry. Code search via in-tree indexer + trusty-search MCP. Hybrid
BM25+embedding retrieval with cluster boosting. Project index generated and
injected into PM prompts.

**Known gaps.**
- 🔵 **No hierarchical `AGENTS.md`-style directory walking** — a single root
  `CLAUDE.md` is used (gap vs nested-context harnesses).

---

## 8. Global Infrastructure — `src/registry/`, `src/bus/`, `src/process_tracker.rs`

**Responsibility.** Cross-process, cross-project coordination state on the local
filesystem and over UNIX sockets.

**Key types/modules.**
- `src/registry/` — `ProjectRegistry` at `~/.open-mpm/projects.json`.
- `src/bus/` — UNIX-socket `MessageBus` + `BusEnvelope` at
  `~/.open-mpm/sockets/<id>.sock`.
- `src/process_tracker.rs` — `ProcessTracker`, PID lifecycle at
  `~/.open-mpm/processes.json`.
- Shared skills at `~/.open-mpm/skills/`.

**Current state.** ✅ Project registry, inter-project UNIX-socket message bus,
process tracker, and shared-skills directory all in place.

**Known gaps.**
- 🟡 **No `backon` retry** on 429/5xx LLM responses.
- 🟡 **Hand-rolled CLI arg parsing** — **clap migration is HIGH priority** (the
  multi-mode `src/main.rs` dispatch is bespoke today).

---

## 9. UI Surfaces — TUI / Web / Telegram

Three surfaces share the controller's event bus and HTTP API. Completeness
varies: **TUI ~80%, Web ~40%, Telegram ~60%.**

### 9.1 TUI — `src/repl/`

**Responsibility.** Primary surface for power developers: live REPL, slash
commands, scope-colored output, subagent panel, statusline.

**Key types/modules.** ratatui TUI, `ReplBridge`, slash-command dispatch,
`agent_commands`, banner. `OpenMpmRepl` holds `model_override` /
`provider_override`.

**Current state.** ✅/🟡 (~80%). Slash commands implemented:
`/help /connect /disconnect /model /provider /agent /projects /status /tools
/workflow /version /quit`. Scope colors: cyan = User/ctrl, yellow = Project/PM.
Subagent panel and statusline present. `/model` and `/provider` overrides applied
before credential routing (ARCHITECTURE §5.5). **tmux e2e harness**
(`scripts/tmux-repl-test.sh`) is required before REPL/CTRL commits.

**Known gaps.** 🔵 Token streaming is Phase 4.

### 9.2 Web GUI — Tauri + Svelte

**Responsibility.** Surface for PMs/team leads: sidebar project status, task
history, cost tracking, dispatch without CLI.

**Key types/modules.** Tauri + Svelte app; Axum HTTP API (`src/api/`, split from
the 3,360-line `server.rs` via #364); SSE event consumer.

**Current state.** 🟡 (~40%). The GUI exists; the HTTP API and SSE backend are
in place.

**Known gaps.** 🔵 **Phase 2** features all outstanding: SSE consumer wiring,
subagent-activity strip, footer statusline, command palette (Cmd+K), tool-call
cards, session persistence. Web UI currently **polls every ~2 s** (no real-time
push); token streaming is Phase 4.

### 9.3 Telegram — `src/telegram/`

**Responsibility.** Mobile/async surface: kick off tasks, monitor progress,
receive results from a phone (`--telegram`).

**Key types/modules.** `src/telegram/` — handlers, pairing, formatting, session
persistence.

**Current state.** 🟡 (~60%). Bot runs; pairing, formatting, and basic session
persistence present; slash commands available.

**Known gaps.** 🔵 **Phase 3** features outstanding: file-based session
persistence, `/tools`, `/projects`, and `/connect <N>`. Token streaming is
Phase 4.

> A **Slack** integration also exists (`src/slack/`) as an additional surface
> beyond the three primary personas' surfaces.
