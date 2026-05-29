# open-mpm — System Architecture

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** research synthesis + code/docs/tickets audit

**Status legend:** ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational

This document describes how open-mpm fits together: the process model, the IPC
wire formats, the event system, the source-module map, and — at length — the
**model-agnostic dispatch** subsystem that is the product's core differentiator.

---

## 1. Process Model

open-mpm is a **multi-mode single binary** (`src/main.rs`). One executable
dispatches to PM, sub-agent, workflow, direct, CTRL, API-server, CLI-search, and
Telegram modes depending on arguments. The runtime topology:

```
CLI invocation
    │
    ├─ Probe ~/.open-mpm/sockets/<project>.ctrl.sock (50ms timeout)
    │     ├─ Connected → CLI client mode: write JSON command, stream replies, exit
    │     └─ Not connected → Controller mode
    │
Controller (single long-running process)
    ├─ ratatui TUI REPL (main task)
    ├─ Axum HTTP API server (tokio task, port 8080)
    ├─ CLI socket listener (tokio task)
    ├─ EventBus (tokio::sync::broadcast<Event>, capacity 1024)
    ├─ ProcessTracker (PID lifecycle)
    └─ PmHandle map (one tokio task per connected project)
          └─ LLM call → tool dispatch
                └─ DispatchingAgentRunner
                      ├─ ClaudeCodeAgentRunner (claude CLI subprocess)
                      ├─ InProcessAgentRunner (tokio task, fast path)
                      └─ SubprocessAgentRunner (NDJSON IPC subprocess)
```

### Key properties

- **Controller is long-running.** It hosts the TUI REPL on the main task and
  spins tokio tasks for the HTTP API (Axum, port 8080), the CLI socket listener,
  the event bus, the process tracker, and one PM actor per connected project
  (`PmHandle` map). ✅
- **PM actors are isolated tokio tasks.** Each connected project gets exactly
  one PM task; the PM calls the LLM, dispatches tools, and delegates to
  sub-agents through the `DispatchingAgentRunner`. ✅
- **Three runner kinds** sit beneath dispatch (see §5): the claude-CLI runner,
  the in-process runner (fast path, read-only agents), and the subprocess runner
  (full OS isolation, file/shell agents). ✅
- **Socket probe → client-or-controller.** A new invocation probes the project's
  `.ctrl.sock` with a 50 ms timeout. On connect, it acts as a thin CLI client:
  write a JSON command, stream replies, exit. On no-connect, it becomes the
  controller. ✅ — *singleton enforcement is now guaranteed at the socket layer*:
  the probe-then-bind, anti-clobber socket handling (PR #411) ends the race, so
  the second of two near-simultaneous invocations reliably routes to the first
  rather than clobbering the socket (see PRD FR-1.3 and §1.1 below).

### Process module support

- `ProcessTracker` (`src/process_tracker.rs`) records PID lifecycle to
  `~/.open-mpm/processes.json`. ✅
- The CLI socket protocol lives in `src/ctrl/socket.rs` and
  `src/ctrl/socket_listener.rs`. ✅
- **Real-time push is incomplete:** the web UI polls every ~2 s; SSE is designed
  and partly built; token streaming is Phase 4. 🟡

### 1.1 Daemon topology & three-tier process hierarchy

> **Decided** — see [ADR-0003](../decisions/0003-daemon-process-model.md) and
> PRD FR-1.3 / FR-1.6 / FR-1.7.

open-mpm runs as a **daemon**, and the intended topology is a process hierarchy
of four roles: **daemon per user-facing agent identity → { one PM process per
project, one TPM ("tmux PM") per session } → coding agents / external
harnesses**. The PM and TPM are *sibling* roles under an identity daemon: the
**PM** orchestrates open-mpm's own native agents (subprocess runners, NDJSON
IPC), while the **TPM** orchestrates *external* harnesses (claude-code, codex,
aider, …) by automating tmux.

```
Agent identity          Project (PM) / Session (TPM)        Workers
──────────────────────────────────────────────────────────────────────────────
CTRL (daemon)  ───┬──►  PM(CTRL, ~/proj-a)  ──────────►  [agent₁ … agentₙ] (n ≤ 20)
                  ├──►  PM(CTRL, ~/proj-b)  ──────────►  [agent₁ … agentₙ] (n ≤ 20)
                  └──►  TPM(CTRL, session-x) ─tmux────►  [claude-code | codex | aider …]

Izzie (daemon) ──────►  PM(Izzie, ~/proj-a) ──────────►  [agent₁ … agentₙ] (n ≤ 20)

CTO Assistant ───────►  PM(CTO, ~/proj-c)   ──────────►  [agent₁ … agentₙ] (n ≤ 20)
 (daemon)
```

- **Tier 1 — one daemon per user-facing agent identity.** CTRL, Izzie, and CTO
  Assistant each run as their **own** daemon; multiple identity daemons coexist
  on one machine. This is *not* a single global singleton. 🔵 *(daemonization +
  per-identity process separation designed-not-built)*
- **Tier 2a — one PM process per project, singleton per `(identity, project)`.**
  Each project's PM is a singleton; the guarantee is scoped to the
  `(agent-identity, project)` pair, so `(CTRL, ~/proj-a)` and
  `(Izzie, ~/proj-a)` are distinct singletons. The PM drives open-mpm's **native**
  agents over NDJSON IPC. The socket-level enforcement of this singleton is
  **implemented** (probe-then-bind, anti-clobber socket handling — PR #411).
  ✅ *(today keyed on `(project)` socket path; per-identity keying via
  `~/.open-mpm/processes.json` is the 🔵 gap)*
- **Tier 2b — one TPM ("tmux PM") per session.** A sibling to the PM that drives
  **external** harnesses (claude-code, codex, aider, …) inside tmux panes rather
  than as native NDJSON subprocesses; cardinality is one TPM per session. The
  tmux-driving substrate is largely built in `src/tm/` — `TmManager`
  (`src/tm/manager.rs`) over a `TmuxOrchestrator`, an `AdapterRegistry`
  (`src/adapters/`, with detectors for claude-code/codex/augment/gemini), and a
  JSON `TmSessionRegistry`, with real `new_session`/`kill_session`/`pause`/
  `resume`/`capture_pane`/`send_message`/`reconcile`. 🟡 *(tmux machinery built;
  formalization as a daemon-managed per-session process role is the 🔵 gap)*
- **Tier 3 — ≤20 coding-agent subprocesses per PM.** A PM may have at most a
  bounded number of coding-agent subprocesses spawned concurrently — documented
  default **20** (configurable). Requests beyond the cap queue / apply
  backpressure. Each subprocess speaks NDJSON over stdin/stdout (§2, ADR-0001).
  🔵 *(cap + queue/backpressure designed-not-built)*

**Current vs. gap:** the per-(identity, project) singleton **enforcement** at the
socket layer is built (PR #411, ✅), and the **TPM's tmux-driving machinery**
(`src/tm/`, `src/adapters/`) is largely built (🟡). Daemonization, per-identity
process separation, the **per-session TPM role**, and the 20-process
coding-agent cap are **designed-not-built** (🔵).

---

## 2. IPC Model (NDJSON)

Sub-agents communicate with their PM over **newline-delimited JSON (NDJSON)** on
stdin/stdout. The codec lives in `src/ipc/mod.rs` (`IpcMessage`). Each subprocess
has **stdin piped, stdout piped (one NDJSON line per message), stderr
inherited**. The PM uses **separate tokio read and write tasks** to prevent
deadlock. ✅

### Message formats

**PM → sub-agent (task):**

```json
{
  "type": "task",
  "id": "<uuid>",
  "task": "…",
  "history": [ … ]
}
```

**Sub-agent → PM (success):**

```json
{
  "type": "result",
  "id": "<uuid>",
  "content": "…",
  "summary": "…",
  "usage": { … }
}
```

**Sub-agent → PM (failure):**

```json
{
  "type": "error",
  "id": "<uuid>",
  "error": "…",
  "status": "error"
}
```

The `id` correlates request and response. The sub-agent emits exactly one
NDJSON result/error line, then exits — giving the subprocess runner its crash
isolation guarantee (a panicking file/shell agent dies alone; the controller
reads the closed pipe and surfaces an error). ✅

> **Known gap:** the `summary` field exists in the wire format, but the PM today
> consumes **full `content`** rather than a compressed summary — there is no
> Roo-Code-style `attempt_completion` summarization step between sub-agent and
> PM (see PRD FR-2.4). 🔵

---

## 3. Event Architecture

The controller runs an **`EventBus`** built on `tokio::sync::broadcast<Event>`
with **capacity 1024**. The `Event` enum lives in `src/events.rs`. ✅

### Event enum

The canonical variants (`src/events.rs`) cover the session, PM, agent, tool,
phase, and LLM lifecycles. The naming in the originating research used
`*Completed`; the implemented variants use `*Done`:

| Lifecycle | Variants (as implemented in `src/events.rs`) |
|---|---|
| Session | `SessionStarted`, `SessionDone`, `SessionCancelled` |
| PM | `PmThinking`, `PmDelegating` |
| Agent | `AgentSpawned`, `AgentStarted`, `AgentMessage`, `AgentDone`, `AgentFailed` |
| Tool | `ToolCalled`, `ToolResult` |
| AST | `AstOperation` |
| Phase | `PhaseStarted`, `PhaseDone`, `PhaseSkipped` |
| LLM | `LlmRequested`, `LlmResponded` |
| Reporting | `ReportGenerated`, `RecapGenerated` |
| Persona | `PersonaDetected` |

> The research also referenced `MemoryPressure` and `SubprocessExited`
> subscribers; treat the table above as authoritative for variant *names* and
> verify the exact set in `src/events.rs` before depending on a specific variant.

### Subscribers

A single broadcast feeds many consumers; each subscribes independently: ✅

| Subscriber | Role |
|---|---|
| **SSE handler** | Streams events to the web GUI (Phase 2 consumer still partial). |
| **Audit logger** | Persists an audit trail of session/agent/tool activity. |
| **Controller monitor** | Drives controller-side state and health. |
| **Task store updater** | Updates the task/session store as events arrive. |
| **CTRL REPL printer** | Renders live activity into the ratatui TUI. |
| **ProcessTracker** | Reacts to subprocess lifecycle to maintain PID state. |

Because broadcast has bounded capacity (1024), slow subscribers can lag; this is
acceptable for UI/audit consumers but is a consideration for any future
back-pressure-sensitive consumer.

---

## 4. Source Module Map

Top-level modules under `crates/open-mpm/src/` and their responsibilities. Some
modules cited in the originating research as single `.rs` files (e.g.
`ctrl/mod.rs`'s `ctrl_turn`/`pm_task`, `workflow/engine.rs`) have since been
split into same-named **subdirectories** during the 500-line-cap sweep
(#358–#366); paths below reflect the reviewed layout.

| Module | Responsibility |
|---|---|
| `src/ctrl/` | CTRL actor: `state.rs`, `config.rs`, `ctrl_turn/`, `pm_task/`, `repl/`, `socket.rs`, `socket_listener.rs`, `handlers/`, `claude_cli.rs`, `supervisor/` |
| `src/agents/` | `AgentConfig` TOML loader (`config.rs`, `loader.rs`), model resolution (`model.rs`), credential routing, persona injection (`persona/`), `in_process_runner.rs`, `claude_code_runner/`, `prompt_builder/`, `registry/`, `harness_protocol.rs`, `context_filter.rs` |
| `src/workflow/` | `WorkflowEngine` (`engine/`), `WorkflowDef`/`PhaseDef` (`config/`), `WorkflowContext` (`context.rs`), parallel wave (`parallel.rs`), worktree mgmt (`worktree.rs`), ticket tracking (`tickets.rs`), autopush (`autopush.rs`), `resolver.rs` |
| `src/tools/` | `ToolRegistry` (`registry/`), `ToolExecutor` trait (`traits.rs`), `delegate.rs`, `fs_reader/`, `git_tools/`, `mcp_tools/` + `mcp_service_tools.rs`, `memory/`, `analysis/`, `ast_tools/`, `phase_audit.rs`, `finish_task.rs`, `write_file.rs`, `shell_exec.rs`, `web_search.rs`, `skill_loader/`, `native_*` |
| `src/llm/` | LLM client (OpenRouter / AnthropicDirect / Bedrock), credential routing (`credentials.rs`), tool loop (`tool_loop/`), `single_turn.rs`, compression (`compress.rs`), `thinking_classifier.rs`, `anthropic_native/`, `bedrock/`, `adapter/`, `http.rs` |
| `src/compress/` | Deterministic NLP compression (dedup, history sliding-window, tool-output, context, session) |
| `src/skills/` | `SkillRegistry` (tag index), `SkillsLoader` (lang/framework detection), LLM-backed selection (`llm.rs`), `GlobalSkillsCache` |
| `src/memory/` | `RedbUsearchStore`, `SessionStore`, `CodeStore`, `FastEmbedder`, `MemoryGraph`, `TrustyBacked` |
| `src/repl/` | ratatui TUI, `ReplBridge`, slash commands, `agent_commands`, banner, dispatch |
| `src/ipc/` | NDJSON `IpcMessage` codec |
| `src/subprocess/` | `SubprocessAgentRunner`, spawn helpers |
| `src/bus/` | UNIX-socket `MessageBus`, `BusEnvelope` |
| `src/telegram/` | Telegram bot (handlers, pairing, format, session persistence) |
| `src/context/` | `ContextManager` (token budget), `ClusterStore` (BM25 + embedding), conversation-history indexing (`retrieval.rs`) |
| `src/registry/` | `ProjectRegistry` (`~/.open-mpm/projects.json`) |
| `src/search/` | Code indexer + file watcher (tree-sitter) |
| `src/tm/` | `TmManager` (tmux session mgmt), `TmProject`, `TmSession`, adapter detection |
| `src/identity/` | `UserProfile` |
| `src/init/` | `ProjectInitializer` (`project-index.md` generation, kuzu-memory loading) |
| `src/mcp/` | MCP server/client integration |
| `src/ticketing/` | GitHub Issues ticketing |
| `src/eval/` | Bake-off evaluation harness |
| `src/ast/` | AST-native substrate flag, tree-sitter tooling |
| `src/rbac/` | Role-based access control |
| `src/slack/` | Slack integration |
| `src/api/` | Axum HTTP API server (split from the 3,360-line `server.rs` via #364) |

Additional modules present in the tree include `src/adapters/`, `src/cli/`,
`src/debugger/`, `src/git/`, `src/inspection/`, `src/intent/`,
`src/local_inference/`, `src/logging/`, `src/perf/`, `src/plugins/`,
`src/recap/`, `src/rpc/`, `src/service/`, `src/tmux/`, `src/update/`,
`src/usage/`, plus top-level support files (`runtime/`, `session*.rs`,
`state_writer.rs`, `interaction_log.rs`, `mistake_log.rs`, `progress.rs`).

---

## 5. Model-Agnostic Dispatch (core differentiator)

This is the heart of open-mpm's product claim: **any agent → any model → any
provider**, configured per-agent in TOML, with no code changes. It has six
moving parts: credential priority, per-agent TOML, the three runner kinds, the
four LLM backend paths, session-level override, model qualification, and
thinking mode.

### 5.1 Credential priority (`src/llm/credentials.rs`)

The `LlmCredentials` enum has three variants. `pick_credentials()` checks env
vars **in priority order — first match wins** (verified in
`src/llm/credentials.rs`): ✅

| Priority | Credential | Label | Env var | Effect |
|---|---|---|---|---|
| 1 (highest) | `ClaudeCode` | `"claude-code"` | `CLAUDE_CODE_OAUTH_TOKEN` | Routes to `ClaudeCodeAgentRunner` (spawns the `claude` CLI). |
| 2 | `AnthropicDirect` | `"anthropic-direct"` | `ANTHROPIC_API_KEY` | POSTs to `api.anthropic.com` with `x-api-key`. |
| 3 | `OpenRouter` | `"openrouter"` | `OPENROUTER_API_KEY` | Routes through `openrouter.ai/api/v1` (500+ models). |

Important nuance from the implementation: `ClaudeCode` is selected **only when
`runner == Some(RunnerKind::ClaudeCode)` *and* `CLAUDE_CODE_OAUTH_TOKEN` is
set** — because an OAuth token would 401 against the Anthropic REST API, so it
must drive the CLI subprocess. Otherwise the function prefers `AnthropicDirect`,
then `OpenRouter`.

### 5.2 Per-agent TOML

Each agent's behavior — including its model and provider — is declared in TOML
(`AgentConfig`, `src/agents/config.rs`). Reassigning a role to a different model
or provider is a two-line edit, no code change: ✅

```toml
[agent]
model  = "anthropic/claude-sonnet-4-6"   # OpenRouter format: provider/model-id
runner = "claude-code"                    # or "subprocess", "in-process"

[llm]
use_anthropic_direct = true               # bypass OpenRouter, POST api.anthropic.com
aws_profile          = "prod"             # AWS Bedrock
aws_region           = "us-east-1"
model_override       = "gpt-4.1"          # beats [agent].model when set
elevation_threshold  = 5000               # token count that triggers elevation_model
elevation_model      = "anthropic/claude-opus-4-5"
```

### 5.3 Three runner kinds (`RunnerKind`)

Dispatch chooses one of three runners per agent invocation; `DispatchingAgentRunner`
selects by `RunnerKind`: ✅

1. **`RunnerKind::ClaudeCode`** — `ClaudeCodeAgentRunner` invokes the `claude`
   CLI as a subprocess. OAuth-token auth (`CLAUDE_CODE_OAUTH_TOKEN`), model via
   the `--model` flag, supports extended thinking. (`src/agents/claude_code_runner/`,
   `src/ctrl/claude_cli.rs`)
2. **`RunnerKind::InProcess`** — `InProcessAgentRunner` runs the tool loop *inside
   the controller process* over the Anthropic/OpenRouter REST path. Shares
   `Arc<SkillRegistry>`, `Arc<CodeStore>`, and a singleton `reqwest::Client`.
   Intended for **read-only agents** (avoids the 2–3 s embedder re-init of a fresh
   subprocess). (`src/agents/in_process_runner.rs`)
3. **`RunnerKind::Subprocess`** — `SubprocessAgentRunner` re-invokes the binary
   as `open-mpm --agent <name>`, giving full OS process isolation over NDJSON.
   **Required for `ShellExec`/`WriteFile` agents.** (`src/subprocess/`)

### 5.4 Four LLM backend paths (`llm::chat_with_tools_gated`)

Beneath the runners, the actual HTTP/CLI transport resolves to one of four
backend paths: ✅

1. **Bedrock** (`src/llm/bedrock/`) — AWS Bedrock via `OPEN_MPM_AWS_PROFILE` /
   `OPEN_MPM_AWS_REGION`.
2. **AnthropicNative** (`src/llm/anthropic_native/`) — REST to
   `api.anthropic.com`; supports extended thinking and prompt caching via
   `cache_control`.
3. **OpenRouter raw** (`send_raw_completion`) — `reqwest` raw POST for
   non-standard headers OpenRouter requires.
4. **async-openai typed** (`client.chat().create()`) — the default,
   OpenAI-compatible path.

### 5.5 Session-level override

`/model` and `/provider` in the REPL store `model_override` / `provider_override`
on `OpenMpmRepl`. They are injected **after** `AgentConfig::load()` and **before**
`apply_credential_routing()`, so a session override **takes precedence over both
the TOML `model` and the env-var credential**. ✅

> **Caveat:** the legacy stdin REPL turn `ctrl_chat_turn` (`crates/open-mpm/src/ctrl/ctrl_turn/dispatch.rs:48`)
> hardcodes `CTRL_MODEL` and calls `llm::chat()` directly *without*
> `pick_credentials()`, so that one path always routes via OpenRouter regardless
> of overrides/credentials. The ratatui `run_pm_task_with_history` path routes
> correctly. (#408; supersedes pre-refactor `src/ctrl/mod.rs:3391` from #358–#366 cap-sweep module split.) (PRD FR-1.4) 🟡

### 5.6 Model qualification

`qualify_openrouter_model(&creds, &model_name)` injects an `"anthropic/"` prefix
for bare Claude model IDs **when routing via OpenRouter** (the OpenRouter
`provider/model-id` convention, #268). When `use_anthropic_direct` or
`claude-code` is in effect, the model name is passed **verbatim**. ✅

### 5.7 Thinking mode

`ThinkingMode` classification (`src/llm/thinking_classifier.rs`) determines
extended-thinking applicability per model/provider — e.g. Anthropic-native and
claude-CLI paths support extended thinking; other backends may not. The
classifier gates whether a thinking budget is requested for a given dispatch. ✅

### 5.8 End-to-end dispatch flow (summary)

```
AgentConfig::load(<name>)            # [agent].model, [agent].runner, [llm].*
   │
   ├─ session override?  →  apply model_override / provider_override
   │
   ├─ apply_credential_routing()     # pick_credentials(): ClaudeCode > AnthropicDirect > OpenRouter
   │
   ├─ qualify_openrouter_model()     # prefix "anthropic/" only for OpenRouter routing
   │
   ├─ ThinkingMode classification    # extended-thinking applicability
   │
   └─ DispatchingAgentRunner (RunnerKind)
         ├─ ClaudeCode   → claude CLI subprocess (--model, OAuth)
         ├─ InProcess    → tool loop in controller → backend path
         └─ Subprocess   → open-mpm --agent <name> (NDJSON) → backend path
                              backend = Bedrock | AnthropicNative | OpenRouter-raw | async-openai
```

> **Known dispatch gaps:** `llm::create_client()` is called **per request** in
> `run_pm_task_with_history` / `run_pm_task_with_persona` (no singleton client on
> those paths — `InProcessAgentRunner` *does* cache correctly); there is **no
> adaptive system prompt per model family** (Claude vs GPT vs Llama tool-calling
> differences — a gap vs Cline); and the system prompt is rebuilt per request
> with **no stabilized cache prefix** (Codex-CLI-style cache-prefix ordering not
> applied). See PRD §6.
