# open-mpm — Product Requirements Document

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** research synthesis + code/docs/tickets audit

**Status legend:** ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational
Each requirement is framed **Vision / Current / Gap**.

---

## 1. Vision & Mission

### North-star vision

> **open-mpm is a superset of Warp and Claude Code** — an *agentic assistant
> manager* **and** an *original coding harness* whose defining property is that
> **any model can be used for any agent**: OpenRouter (500+ models), the direct
> Anthropic API, AWS Bedrock, and the `claude` CLI — assignable per-agent via
> TOML, with no code changes.

Where Claude Code is locked to the `claude` CLI and Anthropic's runtime, and
Warp manages sessions for a single model family, open-mpm aspires to be the
**coordination layer *above* any LLM provider**. Different agents in the *same
session* can be backed by different providers simultaneously — a cheap fast
model for read-only triage, a frontier model for the hard coding phase, a
local/OpenRouter model for bulk work — and switching a role from one provider to
another is a two-line TOML edit.

open-mpm re-implements the PM-orchestrator-plus-specialized-subagent pattern
that **claude-mpm** pioneered, in **Rust**, for a single-binary, low-overhead,
local deployment model — with a radically broader model-choice surface.

### Mission

Deliver a single, self-contained binary that lets a developer (or a team lead,
or a phone) hand off a high-level task and have it coordinated across multiple
specialized agents, each running on whichever model makes sense for its role —
without managing the orchestration by hand, and without a cloud service, a
Python runtime, or a Node.js dependency in the core.

### Why this is novel

The research corpus positioned open-mpm as: *"no competing Rust harness matches
claude-mpm's feature set — open-mpm would be novel."* The competitive frame is
Roo Code, Cline, Kilo.ai, and the OpenAI Codex CLI; the differentiator versus
all of them is **model-agnostic, per-agent, multi-provider dispatch** combined
with **process-isolated sub-agents** in a **single Rust binary**.

---

## 2. Goals & Non-Goals

### Goals

| # | Goal | Status |
|---|---|---|
| G1 | **PM orchestration** — a long-running CTRL layer coordinating multiple PM sessions across project directories. | ✅ |
| G2 | **Subprocess delegation with process isolation** — sub-agents spawned as OS subprocesses or tokio tasks over NDJSON IPC; crash isolation for file-writing/shell agents. | ✅ |
| G3 | **Model-agnostic dispatch** — any agent → any model → any provider; credential routing (ClaudeCode > AnthropicDirect > OpenRouter) plus per-agent TOML. | ✅ 🟡 (one legacy code path bypasses routing — see §4.1 / FR-1.4) |
| G4 | **Skill injection** — Markdown skills composed into prompts; five-tier discovery; auto-select by tag/language/relevance. | 🟡 |
| G5 | **Declarative workflow engine** — JSON multi-phase workflows (research → plan → code → QA → observe), per-phase agent assignment, skill injection, AST-native/traditional substrate switching, parallel wave execution. | ✅ |
| G6 | **Token compression** — deterministic NLP: tool-output filtering, dedup, sliding-window, stop-word removal, TF-IDF. | ✅ |
| G7 | **Three UI surfaces** — ratatui TUI, Tauri + Svelte GUI, Telegram bot. | 🟡 (TUI ~80%, Web ~40%, Telegram ~60%) |
| G8 | **Inter-project message bus** — UNIX domain socket. | ✅ |
| G9 | **Vector memory** — embedded redb + usearch + fastembed; no external services. | ✅ |
| G10 | **Competitive benchmarking** — ai-coding-bake-off L1–L5. | 🟡 (L1–L3 validated) |

### Non-Goals

| Non-Goal | Rationale |
|---|---|
| Central server / cloud-hosted execution | open-mpm is a single-binary **local** tool. |
| Forcing a single LLM provider or model family | BYOK + model choice is first-class; the opposite of the product. |
| Python runtime or Node.js dependency for the **core** | Rust-native single binary; Node is only used to build embedded Svelte UIs. |
| VS Code extension | Possible future surface; out of scope today. |
| OS-native sandboxing (macOS seatbelt / Linux bubblewrap) | A known gap vs Codex CLI; **not planned**. Isolation is process-level only. |

---

## 3. Target Users / Personas

| Persona | Who | Primary need | Surface |
|---|---|---|---|
| **Power developer** | Engineers living in the terminal | Slash commands, keybindings, live status bars, multi-project switching | ratatui **TUI** |
| **Project manager / team lead** | Coordinators who prefer chat over CLI | Sidebar of project status, task history, cost tracking, dispatch without CLI | Tauri + Svelte **GUI** |
| **Mobile / async user** | People away from the desk | Kick off tasks from a phone, monitor progress, receive results | **Telegram** bot |

**Unifying need across all three:** delegate a high-level task across multiple
agents, using whichever models make sense for each role, without managing the
orchestration manually.

Additionally, four **coding personas** are surfaced *through the skill system*
(not user accounts): **engineer**, **hacker**, **vibe-coder**, **novice** —
each shaping prompt style and skill selection (see FR-4).

---

## 4. Functional Requirements

Grouped by capability area. Each requirement carries Vision / Current / Gap and
an inline status tag. Source paths are cited where known.

### 4.1 PM Orchestration & CTRL (`src/ctrl/`)

**FR-1.1 — Persistent CTRL coordinator** ✅
- *Vision:* A single long-running controller manages a per-project PM actor (one tokio task per active project), reading input, calling the LLM with a `delegate_to_agent` tool, and routing to sub-agents.
- *Current:* Implemented in `src/ctrl/` (state, config, `ctrl_turn/`, `pm_task/`, `repl/`, `socket.rs`, `socket_listener.rs`, `handlers/`, `claude_cli.rs`).
- *Gap:* `ctrl/mod.rs` remains oversized (~5,730 lines, #170) and is being split incrementally.

**FR-1.2 — Multi-project dispatcher** ✅
- *Vision:* `/connect <path>` wires the REPL to a project PM; multiple projects coordinated simultaneously from one controller.
- *Current:* Implemented; a `PmHandle` map holds one tokio task per connected project.
- *Gap:* None material; cancel semantics are weak (see FR-1.5).

**FR-1.3 — Daemon process model: one daemon per agent identity, one PM per project** ✅ (socket enforcement) · 🔵 (full model) — *see [ADR-0003](../decisions/0003-daemon-process-model.md)*
- *Vision:* open-mpm runs as a **daemon**. Each **user-facing agent identity** — e.g. CTRL (the multi-project dispatcher), Izzie, and CTO Assistant — runs as its **own** daemon process; multiple such daemons legitimately coexist (this is *not* one global singleton). Within a daemon, each project's PM is a singleton process. The singleton guarantee is scoped **per `(agent-identity, project)`**, not globally. A second CLI invocation for the same `(identity, project)` detects the running PM over its UNIX socket and routes to it instead of spawning a duplicate (`src/ctrl/socket.rs`, `socket_listener.rs`). The PM has a **sibling role, the TPM ("tmux PM"), scoped one-per-session**, which drives *external* harnesses via tmux instead of native NDJSON subprocesses (see FR-1.7).
- *Current:* Socket-level singleton **enforcement is implemented** (PR #411): probe-then-bind with anti-clobber socket handling, so two near-simultaneous invocations no longer race — the CLI probes `~/.open-mpm/sockets/<project>.ctrl.sock` (50 ms timeout) and the second reliably routes as a client. ✅
- *Gap:* The full daemon/identity model is **designed-not-built** 🔵 — true daemonization (detach/supervise/attach), a per-identity process registry keyed on `(agent-identity, project)` (extending `~/.open-mpm/processes.json`), and per-user-facing-agent process separation are not yet implemented. Today's enforcement keys on `(project)` socket path only. See [ADR-0003](../decisions/0003-daemon-process-model.md).

**FR-1.4 — Credential-correct PM turns** 🟡
- *Vision:* Every PM/CTRL turn routes through `pick_credentials()` so the configured provider priority is honored.
- *Current:* The ratatui path (`run_pm_task_with_history`) routes correctly.
- *Gap:* **Credential-routing bug** — the legacy stdin REPL turn (`ctrl_chat_turn`, `crates/open-mpm/src/ctrl/ctrl_turn/dispatch.rs:48`) hardcodes `CTRL_MODEL` and calls `llm::chat()` directly *without* `pick_credentials()`, so it always routes via OpenRouter regardless of configured credentials. (#408; supersedes pre-refactor `src/ctrl/mod.rs:3391` from #358–#366 cap-sweep module split.)

**FR-1.5 — User-level cancellation** 🔵
- *Vision:* A user can cancel an in-flight PM/agent task from any surface.
- *Current:* `SessionCancelled` event exists in the bus.
- *Gap:* Cancel is **unimplemented at the user level** — no surface reliably interrupts a running task.

**FR-1.6 — Bounded coding-agent fan-out** 🔵 — *see [ADR-0003](../decisions/0003-daemon-process-model.md)*
- *Vision:* A PM may spawn coding-agent subprocesses up to a concurrency cap (default **20**, configurable); requests beyond the cap **queue / apply backpressure** rather than spawning unbounded processes. Bounding fan-out caps memory, file-descriptor, and CPU pressure and gives the `~/.open-mpm/processes.json` tracker a predictable ceiling.
- *Current:* No cap — a PM dispatches coding-agent subprocesses without a concurrency bound.
- *Gap:* **Designed-not-built** — enforcing the cap requires a semaphore/queue in the dispatch path plus surfacing backpressure to the caller. See [ADR-0003](../decisions/0003-daemon-process-model.md).

**FR-1.7 — TPM ("tmux PM") for external harnesses** 🟡 (tmux machinery) · 🔵 (per-session role) — *see [ADR-0003](../decisions/0003-daemon-process-model.md)*
- *Vision:* A **TPM** is a PM variant — sibling to the native PM (FR-1.3) — that orchestrates **external** coding harnesses (third-party CLIs such as `claude-code`, `codex`, `aider`, …) by driving them inside **tmux** panes/sessions rather than as native NDJSON subprocesses. Cardinality is **one TPM per session**. PM vs. TPM: the PM drives open-mpm's *own* agents over NDJSON IPC; the TPM drives *third-party* tools it does not own by automating tmux (create session/pane, send keys, capture pane, detect harness).
- *Current:* The tmux-driving substrate is largely built in `src/tm/`: `TmManager` (`src/tm/manager.rs`) ties a `TmuxOrchestrator`, an `AdapterRegistry` (`src/adapters/` — pane-output detectors for `claude-code`, `codex`, `augment`, `gemini`, plus `shell`/`claude-mpm`/`open-mpm`), and a JSON `TmSessionRegistry` (`src/tm/registry.rs`) behind one async API. Real session lifecycle works — `new_session`/`kill_session`, `pause_session`/`resume_session` (via each adapter's tmux pause/resume command), `capture_pane`, `send_message`, `attach_instructions`, `reconcile` — over the `TmProject`/`TmSession` model (`src/tm/project.rs`), with a background `TmMonitor` and a `/tm` command handler. 🟡 (tmux integration tests gated behind tmux availability.)
- *Gap:* The **per-session TPM *role*** — a daemon-managed "1 TPM per session" process owned by the identity daemon and recorded in `~/.open-mpm/processes.json` alongside the native PM — is **designed-not-built** 🔵. Today `src/tm/` is a library/CLI-driven facade, not a supervised per-session process. See [ADR-0003](../decisions/0003-daemon-process-model.md).

### 4.2 Sub-Agent Subprocess Model (`src/subprocess/`, `src/ipc/`)

**FR-2.1 — Subprocess sub-agents** ✅
- *Vision:* Sub-agents spawned via `tokio::process::Command` re-invoking the binary as `open-mpm --agent <name>`, with full OS isolation.
- *Current:* Implemented; `SubprocessAgentRunner` in `src/subprocess/`.
- *Gap:* None material.

**FR-2.2 — NDJSON IPC** ✅
- *Vision:* PM writes `{"type":"task"}`; sub-agent returns `{"type":"result"}` or `{"type":"error"}` then exits (`src/ipc/mod.rs`).
- *Current:* Implemented; stdin/stdout piped, stderr inherited, separate read/write tokio tasks prevent deadlock.
- *Gap:* None material.

**FR-2.3 — Isolation policy** ✅
- *Vision:* File/shell agents **must** be subprocess-isolated; read-only agents may run as tokio tasks to avoid the 2–3 s embedder re-init cost.
- *Current:* `InProcessAgentRunner` + `SubprocessAgentRunner` implement the `AgentRunner` trait; `DispatchingAgentRunner` selects by `RunnerKind`.
- *Gap:* None for the policy itself; see FR-2.4 for the result-handling gap.

**FR-2.4 — Result summarization** 🔵
- *Vision:* Sub-agent results compressed to a PM-facing summary (Roo Code's `attempt_completion` pattern) rather than returning full content.
- *Current:* **Full** sub-agent content is returned to the PM.
- *Gap:* No summary-compression step between sub-agent and PM, inflating PM context.

### 4.3 Tool-Using Agents (`src/tools/`)

**FR-3.1 — Multi-turn tool-call loop** ✅
- *Vision:* Agents run an LLM tool-call loop over a rich tool set: `read_file`, `write_file` (atomic, sandboxed to `out_dir`), `shell_exec`, `web_search`, `load_skill`, `list_skills`, `delegate_to_agent`, git tools, MCP service tools, ticketing, memory.
- *Current:* 30+ tools implemented with OpenAI-compatible schemas; atomic `write_file` with `out_dir` sandboxing (`src/tools/write_file.rs`, `shell_exec.rs`, `delegate.rs`, `git_tools/`, `mcp_tools/`, `memory/`, …).
- *Gap:* None material.

**FR-3.2 — Per-agent tool allowlists** ✅
- *Vision:* Each agent exposes only the tools its role needs (`dispatch_gated`).
- *Current:* Two registries — `build_ctrl_registry()` for CTRL and an inline PM registry for `run_pm_task_with_history`; per-agent allowlists applied.
- *Gap:* TagSkillRegistry not threaded into sub-agent registries (see FR-4.4).

**FR-3.3 — AST-native tools** ✅
- *Vision:* Structural introspection tools (`src/tools/ast_tools/`) reduce LLM round-trips versus text grepping.
- *Current:* Implemented and validated at workflow levels L1/L2/L3.
- *Gap:* None material.

### 4.4 Skill Injection (`src/skills/`)

**FR-4.1 — Five-tier skill discovery** 🟡
- *Vision:* Resolve skills across `.open-mpm/skills/` > `.claude/skills/` > `~/.open-mpm/skills/` > `~/.claude/skills/` > bundled.
- *Current:* Five-tier discovery implemented; skills are Markdown + YAML frontmatter (`name`/`description`/`tags`).
- *Gap:* Skill source paths are **hard-coded** — no operator-configurable skill source URLs.

**FR-4.2 — Auto-selection** 🟡
- *Vision:* Two mechanisms — `SkillsLoader` (language/framework detection) and `SkillRegistry::auto_inject` (tag scoring) — plus LLM-backed selection (`src/skills/llm.rs`).
- *Current:* Both mechanisms exist; LLM-backed selection implemented.
- *Gap:* No skill-effectiveness scoring / feedback loop to learn which skills help.

**FR-4.3 — Per-phase skills** ✅
- *Vision:* A phase declares `Option<Vec<String>>` of skills in its `PhaseDef`.
- *Current:* Implemented in the workflow config.
- *Gap:* None material.

**FR-4.4 — Persistent skill index** 🟡
- *Vision:* A tag-indexed skill registry built once and reused, threaded into all (including sub-agent) tool registries.
- *Current:* `SkillRegistry` tag index and `GlobalSkillsCache` exist; the tag-indexed `list_skills` path is implemented but currently `#[allow(dead_code)]` pending wiring.
- *Gap:* Index is rebuilt each run (`GlobalSkillsCache` unwired); TagSkillRegistry built at startup but **not threaded** into sub-agent tool registries.

**FR-4.5 — Coding personas** ✅
- *Vision:* Four personas (engineer / hacker / vibe-coder / novice) shape prompts and skill selection.
- *Current:* Delivered via the skill system + `src/agents/persona/`.
- *Gap:* None material.

### 4.5 Workflow Engine (`src/workflow/`)

**FR-5.1 — Declarative JSON workflows** ✅
- *Vision:* Named phases, each with agent / context template / tool set / skill list / `ast_native` flag / dependency config (`src/workflow/config/`).
- *Current:* Implemented.
- *Gap:* `workflow/engine.rs` oversized (~4,965 lines, #172); being split into `workflow/engine/`.

**FR-5.2 — Execution modes** ✅
- *Vision:* **Prescriptive** (sequential) and **wave** (parallel sub-tasks with git worktrees) (`src/workflow/engine/`, `parallel.rs`).
- *Current:* Both implemented; worktree management in `src/workflow/worktree.rs`.
- *Gap:* None material.

**FR-5.3 — Per-phase AST-native substrate** ✅
- *Vision:* `ast_native: Option<bool>` per phase, toggled via an RAII guard.
- *Current:* Implemented and validated at L1/L2/L3.
- *Gap:* None material.

**FR-5.4 — Inter-phase context passing** ✅
- *Vision:* `WorkflowContext` passes prior-phase outputs into later phases via template substitution.
- *Current:* Implemented (`src/workflow/context.rs`).
- *Gap:* None material.

**FR-5.5 — Ticket tracking, autopush, perf** ✅
- *Vision:* Per-phase ticket tracking (`tickets.rs`), autopush (`autopush.rs`), per-phase timing/cost via `PerfCollector`.
- *Current:* Implemented.
- *Gap:* None material.

### 4.6 Token Compression (`src/compress/`)

**FR-6.1 — Deterministic compression pipeline** ✅
- *Vision:* `CompressConfig` (target/max token budgets) drives: tool-output filtering → dedup (`dedup_sections`) → sliding-window (`TokenBudget`) → stop-word removal → TF-IDF.
- *Current:* Pipeline implemented; `ContextManager` trims to a `soft_threshold` fraction.
- *Gap:* None material.

**FR-6.2 — Model-aware context windows** ✅
- *Vision:* Window sizing per model — Claude 200k, GPT-5.1-codex 400k, unknown 128k — with per-agent `[compress]` TOML.
- *Current:* Implemented.
- *Gap:* None material.

### 4.7 CTRL CLI / Multi-Project UX (`src/repl/`, `src/telegram/`, GUI)

**FR-7.1 — TUI REPL slash commands** ✅
- *Vision:* `/help /connect /disconnect /model /provider /agent /projects /status /tools /workflow /version /quit`, scope colors (cyan = User/ctrl, yellow = Project/PM).
- *Current:* Implemented in `src/repl/` (ratatui), incl. subagent panel and statusline (~80% complete).
- *Gap:* Token streaming pending (Phase 4).

**FR-7.2 — Session model/provider override** ✅
- *Vision:* `/model` and `/provider` set `model_override` / `provider_override` on `OpenMpmRepl`, applied **before** credential routing and beating TOML + env.
- *Current:* Implemented.
- *Gap:* None material (note the unrelated `ctrl_chat_turn` bug, FR-1.4).

**FR-7.3 — Telegram bot** 🟡
- *Vision:* Slash-command parity from a phone (`--telegram`, `src/telegram/`).
- *Current:* Bot, handlers, pairing, formatting, session persistence exist (~60%).
- *Gap:* File-based session persistence, `/tools`, `/projects`, `/connect <N>` are Phase 3.

**FR-7.4 — Tauri + Svelte GUI** 🟡
- *Vision:* Sidebar project status, command palette (Cmd+K), SSE consumer, tool-call cards, cost tracking.
- *Current:* GUI exists (~40%).
- *Gap:* SSE consumer, subagent-activity strip, footer statusline, command palette, tool-call cards, session persistence — all Phase 2.

### 4.8 Memory & Search (`src/memory/`, `src/search/`, `src/context/`, `src/init/`)

**FR-8.1 — Embedded vector memory** ✅
- *Vision:* `RedbUsearchStore` (redb metadata + usearch HNSW) with `FastEmbedder`; no external services.
- *Current:* Implemented; per-session `SessionStore` + `SessionRegistry`.
- *Gap:* None material.

**FR-8.2 — Code search** ✅
- *Vision:* In-tree code search (`src/search/`, tree-sitter) plus trusty-search MCP integration.
- *Current:* Implemented (indexer + file watcher).
- *Gap:* None material.

**FR-8.3 — Project index injection** ✅
- *Vision:* `ProjectInitializer` generates `project-index.md` (24 h TTL), injected into the PM prompt.
- *Current:* Implemented (`src/init/`).
- *Gap:* No hierarchical `AGENTS.md`-style directory walking (single root `CLAUDE.md`) — see §6.

**FR-8.4 — Hybrid retrieval** ✅
- *Vision:* BM25 + embedding hybrid retrieval (`src/context/retrieval.rs`); `ClusterStore` with a 2× boost.
- *Current:* Implemented.
- *Gap:* None material.

### 4.9 Global Infrastructure (`src/registry/`, `src/bus/`, process tracking)

**FR-9.1 — Project registry** ✅ — `~/.open-mpm/projects.json` (`src/registry/`).
**FR-9.2 — Inter-project message bus** ✅ — UNIX socket `~/.open-mpm/sockets/<id>.sock` (`src/bus/`).
**FR-9.3 — Process tracker** ✅ — `~/.open-mpm/processes.json` (PID lifecycle).
**FR-9.4 — Shared skills** ✅ — `~/.open-mpm/skills/`.

### 4.10 Evaluation (`src/eval/`)

**FR-10.1 — Bake-off harness** 🟡
- *Vision:* ai-coding-bake-off L1–L5 competitive benchmarking.
- *Current:* Harness implemented; L1–L3 validated.
- *Gap:* L4–L5 not yet validated.

---

## 5. Success Criteria / Differentiators

A release meets the bar when:

1. **Model-agnostic dispatch is real and frictionless** — any agent can be
   reassigned from one provider to another (OpenRouter / Anthropic-direct /
   Bedrock / claude CLI) by editing two TOML lines, and *all* PM/CTRL turns
   honor the configured credential priority (closing FR-1.4). ✅ 🟡
2. **Process isolation holds** — a crashing file/shell sub-agent never takes
   down the controller; read-only agents stay in-process for speed. ✅
3. **Multi-project coordination from one binary** — several projects driven
   concurrently from a single CTRL, with the singleton guarantee enforced
   (closing FR-1.3). 🟡
4. **Declarative workflows run end-to-end** — research → plan → code → QA →
   observe, with per-phase models/skills/AST substrate, in both prescriptive and
   parallel-wave modes. ✅
5. **Self-contained** — no cloud, no Python, no Node in the core; vector memory
   and search are embedded. ✅
6. **Competitive** — measurable performance on ai-coding-bake-off L1–L5 versus
   Roo Code, Cline, Kilo.ai, and Codex CLI. 🟡

**Core differentiator (restated):** open-mpm is the only single-binary Rust
harness offering claude-mpm-style PM/sub-agent orchestration with **per-agent,
multi-provider, model-agnostic dispatch** and **OS-level sub-agent isolation**.

---

## 6. Open Questions & Roadmap

### Open questions

- **Singleton vs. multi-controller:** ✅ **Resolved → see [ADR-0003](../decisions/0003-daemon-process-model.md).** open-mpm runs as a daemon, one daemon per user-facing agent identity (CTRL / Izzie / CTO Assistant), one PM per project; the singleton is scoped per `(agent-identity, project)`, so multiple controllers legitimately coexist. The socket-level enforcement that ends the race is implemented (PR #411, FR-1.3); the full daemon/identity/cap model is designed-not-built (FR-1.3, FR-1.6).
- **Approval modes:** adopt Codex CLI-style tiered approval (suggest / auto-edit
  / full-auto)? Currently absent. ⚪
- **Checkpoint/rollback:** adopt Cline-style shadow-Git checkpoints for safe
  rollback of agent edits? Currently absent. 🔵
- **OS-native sandbox:** the research marks seatbelt/bubblewrap as **not
  planned** — confirm this stays a non-goal versus Codex CLI parity.
- **Adaptive system prompts per model family:** Claude vs GPT vs Llama have
  different tool-calling conventions; should the prompt builder specialize per
  family (gap vs Cline)? ⚪
- **Prompt-cache stabilization:** Codex CLI-style stable cache-prefix ordering
  is not applied; the system prompt is rebuilt per request. Worth doing? 🔵

### Roadmap (phased, from current gaps)

| Phase | Theme | Highlights |
|---|---|---|
| **Now** | Correctness & hygiene | Fix `ctrl_chat_turn` credential blindness (FR-1.4); finish the 500-line-cap sweep — #170/#171/#172 remain (`ctrl/mod.rs`, `runtime.rs`, `workflow/engine.rs`); #356 systematic 66-file sweep; **clap migration** (CLI arg parsing is hand-rolled — HIGH priority); `backon` retry on 429/5xx. |
| **Phase 2** | Web GUI | SSE consumer, subagent-activity strip, footer statusline, command palette, tool-call cards, session persistence (Web → from ~40%). |
| **Phase 3** | Telegram | File-based session persistence, `/tools`, `/projects`, `/connect <N>` (Telegram → from ~60%). |
| **Phase 4** | Streaming | Token streaming across all surfaces; real-time push (replace 2 s polling with live SSE). |
| **Later** | Differentiation depth | Sub-agent result summarization (FR-2.4); persistent skill index + effectiveness feedback (FR-4.2/4.4); user-level cancel (FR-1.5); hierarchical `AGENTS.md` walking; adaptive per-family prompts; bake-off L4–L5. |
