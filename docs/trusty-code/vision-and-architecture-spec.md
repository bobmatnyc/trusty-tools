# trusty-code — Vision & Architecture Specification

**Status:** FOUNDATIONAL SPEC — RE-VISION APPROVED  
**Epic:** [#587](https://github.com/bobmatnyc/trusty-tools/issues/587) — trusty-code extraction from open-mpm  
**Owned by:** Bob Matsuoka (owner decision points marked below)  
**Last updated:** 2026-07-06  
**Supersedes:** Epic #1039 (old benchmark harness scope)

---

## 1. Purpose & Vision

### Re-Vision Thesis

**trusty-code is being RE-VISIONED. Its old documented scope — a narrow "article-ready model-comparison benchmark harness" (GitHub epic #1039) — is now SUPERSEDED.**

**NEW VISION:** trusty-code is an **ORIGINAL, TOKEN-EFFICIENT, best-of-breed AI coding harness** that pulls the best design patterns from Claude Code, OpenCode, claude-mpm, Aider, Cline, and Codex CLI, and is a genuine daily-driver coding tool. It is NOT a one-shot benchmarking utility, and NOT the old narrow "parity floor" surface from #1039.

While the **parity-spec** (`docs/trusty-code/parity-spec.md`) remains normative for cross-model *comparison runs*, the production trusty-code harness embraces **token efficiency as a first-class design axis** (co-equal with correctness) and implements ALL of the below axioms and features.

### What trusty-code IS

- A **long-running, per-project daemon** (`tcode serve`) that orchestrates code-generation, edit, test, and verification cycles via the PM main loop and typed sub-agents.
- A **JSON-RPC/STDIO + HTTP API gateway** surface (like `trusty-memory` and `trusty-search`), with a **thin CLI client** that attaches to live running sessions.
- A **Claude-Code-compatible configuration reader** for agents, skills, MCP servers, CLAUDE.md, permissions, and settings.
- A **token-efficient coding orchestrator** with repo-map, edit-format selection, progressive-disclosure skills, non-destructive compaction, and structured output.
- A **foundation for later UI layers**: TUI, TELGUI (Telegram UI), REST — all thin clients over the same JSON-RPC surface.

### What trusty-code is NOT

- NOT a tmux session manager (unlike trusty-mpm).
- NOT a multi-project daemon (unlike trusty-mpm); it is one instance per project.
- NOT a knowledge-worker assistant (that is trusty-agents).
- NOT a one-shot CLI tool (the `run-task` CLI is one of many entry points; the daemon is primary).
- NOT a weak benchmarking utility — it is a production daily-driver with power-user token-optimization features.

---

## 2. Non-Negotiable Architecture Axioms

**These are owner directives. They are binding requirements for all phases.**

### Axiom 1: Daemon-First, Event-Driven

trusty-code runs as a **long-running per-project daemon**. The `serve` mode is the **PRIMARY entry point**, not the one-shot `run-task`. It builds on:

- The **existing in-crate event bus** (`src/events` — process-global broadcast via `tokio::sync::broadcast`).
- The **NDJSON protocol** (`src/ipc/`) for structured message framing.
- The **event-driven mandate** from `docs/architecture/harnesses.md` (binding principle across all three harnesses).

**Rationale:** A daemon that emits events enables multi-client attach/detach, live progress streaming, MCP notifications, and integration into trusty-mpm's hook relay. A request-response tool harness cannot do these.

**Phase 1 implication:** The JSON-RPC/STDIO API surface (Axiom 2) is wired into `tcode serve`, not a separate binary.

---

### Axiom 2: Primary API = JSON-RPC over STDIO + HTTP /rpc

Every trusty-tool daemon speaks **JSON-RPC 2.0 over STDIO** (like `trusty-memory serve --stdio` and `trusty-search serve --stdio`). trusty-code must do the same:

```
trusty-code serve --stdio
  └─> stdin: JSON-RPC 2.0 requests (e.g., `{"jsonrpc":"2.0","id":1,"method":"task.run","params":{...}}`)
  └─> stdout: JSON-RPC 2.0 responses + event notifications (NDJSON)
  └─> stderr: logs only (never JSON)
```

**Also expose `POST /rpc`** (HTTP equivalent) on the daemon's HTTP port for clients that prefer REST.

**The SHARED trusty-console gateway** is the ONE REST gateway for the entire trusty family. trusty-code daemons are started per-project and discovered via the console (do not invent a second gateway).

**Rationale:** JSON-RPC/STDIO is the ecosystem standard. It enables:
- Claude Code integration via MCP (the console invokes tcode daemons via stdio).
- Seamless connection pooling (one TCP connection, multiplex N requests).
- Event streaming (notifications pushed over the same connection).
- Unit testing with a mock client (no network).

**Conformance:** The `trusty-common::rpc` module provides the JSON-RPC 2.0 primitives; trusty-code embeds them.

---

### Axiom 3: CLI/API-First; UIs are Deferred Thin Clients

Follow the **layer priority: API → CLI → TUI → Web**. Every deterministic feature lands in the **JSON-RPC API first**. Additional interfaces come later and contain **NO logic of their own**:

| Layer | Status | What it does |
|-------|--------|-------------|
| **API (JSON-RPC)** | Phase 1 | All business logic; request/response/event definitions; state machine |
| **CLI** | Phase 1 | Parses args; calls the API; formats output |
| **TUI** | Phase 2+ | Terminal UI over the API (no orchestration logic) |
| **TELGUI** | Phase 2+ | Telegram UI over the API (no orchestration logic) |
| **REST (HTTP)** | Phase 2+ | REST shim over the same API (for non-JSON-RPC clients) |

**Rationale:** This prevents UI-specific logic from leaking into the core. When new interfaces emerge, they reuse the same API without duplication.

---

### Axiom 4: The Daemon OWNS Sessions; CLI Attaches Over the API

**Unlike trusty-mpm** (which owns tmux sessions and attaches via tmux), a trusty-code running session is a **first-class object inside the daemon**. The CLI must be able to:

- Connect to a RUNNING session over JSON-RPC (not start a fresh one each time).
- Subscribe to its event stream.
- Send input (task modifications, user interrupt).
- Observe output and progress.
- Detach and re-attach with NO data loss.

**NO tmux. NO PTY-scraping.** This gives multi-client attach/detach for free:
- CLI now (Phase 1).
- TUI/TELGUI/REST later attach identically.

**Core API surface (session-attach protocol):**

```rust
// Session management
session.list()              → [Session]
session.create(task, ...)   → Session (returns immediately; runs async)
session.attach(id)          → { streaming event subscription }
session.send(id, input)     → { acknowledged }
session.detach(id)          → { }
session.status(id)          → Session
session.cancel(id)          → { }
```

**Rationale:** Tmux binds the UI to the terminal multiplexer; it is NOT portable to TUI/TELGUI/REST. A daemon that owns sessions is architecture-agnostic.

---

### Axiom 5: Token Efficiency is a First-Class Design Axis

Token efficiency (measured in prompt size, context window, output verbosity, cost) is **co-equal with correctness** as a design driver. Mechanisms (listed in §5 Token-Efficiency Design) MUST be built into Phase 1, not bolted on later:

- **Repo-map** — tree-sitter-backed identifier ranking (§5.1).
- **Per-model edit-format selection** — SEARCH/REPLACE fallback (§5.2).
- **Progressive-disclosure skills** — metadata cached, bodies on-demand (§5.3).
- **Non-destructive compaction** — mark pruned messages, preserve active zone (§5.4).
- **Prompt-cache-aware ordering** — static content before volatile (§5.5).
- **Deferred tool-schema loading** — metadata upfront, schemas on-demand (§5.6).
- **Subagent isolation** — fresh context window per agent (§5.7).
- **Structured output** — machine-readable, not prose (§5.8).

**Reconciliation with parity-spec D2** (see §5.9): The parity spec mandates **full tool schemas** for benchmark fairness. The production harness proposes two modes:
1. **Daily-driver mode** (default): deferred schemas + all optimizations.
2. **Parity/benchmark mode** (opt-in): full schemas + no optimization.

The selection mechanism is an open question for the owner (§9).

---

## 3. Comparative Feature Audit

A table of best-of-breed features extracted from Claude Code, OpenCode, claude-mpm, Aider, Cline, and Codex CLI. Rows = features; columns = existing harnesses + trusty-code adoption plan.

| Feature | Claude Code | OpenCode | claude-mpm | Aider | Cline | Codex | **trusty-code (Phase)** |
|---------|:-:|:-:|:-:|:-:|:-:|:-:|---|
| **Repo-map** (tree-sitter def/ref→PageRank) | — | — | — | ✓ | — | — | Phase 1 (Tier 1) |
| **Per-model edit-format selection** (unified-diff / SEARCH-REPLACE / whole-file) | — | — | — | ✓ (A/B: 20%→61% success) | — | — | Phase 1 (Tier 1) |
| **Progressive-disclosure skill loading** (60-200 tok entry vs 3-6k body) | ✓ | — | ✓ | — | — | — | Phase 1 (Tier 1) |
| **Non-destructive compaction** (`compacted` tag + last-message replay) | — | ✓ | — | — | ✓ | — | Phase 1 (Tier 1) |
| **Prompt-cache-aware ordering** (static content before volatile for cache hits) | — | — | — | — | — | — | Phase 1 (Tier 2) |
| **Per-agent permission matrix** | ✓ | — | ✓ | — | — | — | Phase 1 (Tier 2) |
| **Hidden subagents** (model cannot attempt disallowed delegation) | — | ✓ | — | — | ✓ | — | Phase 2 (Tier 2) |
| **Plan/Act mode split** (read-only plan vs write/execute) | — | — | — | — | ✓ | — | Phase 2 (Tier 2) |
| **First-class verification gate + circuit-breaker** (validate diff applies, tests exist) | — | — | ✓ | — | — | — | Phase 1 (Tier 2) |
| **Deferred tool-schema loading** (metadata cached, schemas on-demand) | ✓ | — | — | — | ✓ | — | Phase 1 (Tier 1) |
| **Subagent-isolated contexts** (fresh window per subagent) | — | ✓ | ✓ | — | ✓ | — | Phase 1 (Tier 1) |
| **Structured output** (ReportFindings instead of prose) | ✓ | — | — | — | — | — | Phase 1 (Tier 1) |
| **Session as first-class unit** (attach/detach, multi-client) | — | ✓ | — | — | — | — | Phase 1 (Axiom 4) |
| **MCP tool integration** | ✓ | — | ✓ | — | ✓ | — | Phase 1 ✓ |
| **Daemon-first event-driven** | — | ✓ | ✓ | — | — | — | Phase 1 (Axiom 1) ✓ |

**Key:**  
- **Tier 1** = highest impact, Phase 1 non-negotiable.  
- **Tier 2** = high impact, Phase 1 if time permits, Phase 2 otherwise.  
- ✓ = implemented; — = not present.

**Sources:**
- Aider: [`aider/repomap.py`](https://github.com/paul-gauthier/aider/blob/main/aider/repomap.py); Gousios et al. edit-format A/B data (20%→61% success).
- OpenCode: [github.com/valhuber/GenAI-Stack](https://github.com/valhuber/GenAI-Stack), non-destructive compaction RFC.
- claude-mpm: crate `trusty-agents` (formerly `open-mpm`), permission model + circuit-breaker.
- Cline: [cline/src](https://github.com/cline/cline), plan/act, subagent isolation.
- Codex: Codex CLI sandboxing + approval-mode orthogonality (future reference).

---

## 4. Architecture

### 4.1 Daemon Lifecycle & Startup

```
1. User runs: tcode serve --project <path>
   └─ or: trusty-mpm spawns tcode serve for a session

2. Daemon initialization:
   ├─ Load .claude/agents, .claude/skills, .claude/commands, .mcp.json
   ├─ Start MCP bridge (establish stdio channels to each MCP server)
   ├─ Initialize event bus (tokio::sync::broadcast)
   ├─ Bind HTTP listener (if --http-port supplied; else stdio-only)
   ├─ Load project CLAUDE.md for context injection
   └─ Ready for requests

3. Daemon lifetime:
   ├─ Listen for JSON-RPC requests on stdin/stdout + /rpc POST
   ├─ Emit events on stdout (NDJSON) / /events (SSE)
   ├─ Enforce filesystem sandbox & bash approval (§11 Security)
   ├─ Manage session durability & transcript retention (§12 Session Durability)
   └─ Graceful shutdown on SIGTERM (drain in-flight sessions)
```

**Concurrency model:** `tokio` multi-threaded runtime. Each session runs in its own spawned task; tool execution is concurrent within a session's agent loop.

**Security & Trust:** See §11 (filesystem sandbox, bash approval, write-permission policy, symlink-traversal rules, MCP server trust levels, environment-variable leakage prevention, secrets redaction).

**Session Durability:** See §12 (in-memory vs persisted transcripts, event replay, daemon restart recovery, idle expiry, ring-buffer bounds, cancellation semantics).

---

### 4.2 Event Bus & NDJSON Protocol

**In-process event bus** (within daemon):

```rust
// Defined in src/events
pub enum Event {
    SessionStarted    { session_id: String, project: String },
    SessionFinished   { session_id: String, transcript: Transcript },
    AgentStarted      { session_id: String, agent: String },
    AgentMessage      { session_id: String, content: String },
    ToolCall          { session_id: String, tool: String, args: Value },
    ToolResult        { session_id: String, success: bool, output: String },
    WorkflowPhase     { session_id: String, phase: String },
    ...
}

// emit_event publishes to broadcast channel
// subscribers (CLI, MCP, TUI) receive live updates
```

**Wire format** (stdout relay, SSE, or HTTP streaming):

```
{"type":"session_started","session_id":"abc-123",...}\n
{"type":"agent_started","session_id":"abc-123","agent":"engineer"}\n
{"type":"tool_call","session_id":"abc-123","tool":"read_file",...}\n
...
```

---

### 4.3 JSON-RPC API Surface

**Core method families** (Phase 1):

| Family | Methods | Purpose |
|--------|---------|---------|
| **session** | `session.list`, `session.create`, `session.attach`, `session.send`, `session.detach`, `session.status`, `session.cancel` | Session lifecycle & attach protocol |
| **task** | `task.run` | Single task execution (use case: CLI `tcode run-task`) |
| **agent** | `agent.list`, `agent.describe`, `agent.reload` | Agent discovery & config |
| **skill** | `skill.list`, `skill.describe`, `skill.invoke` | Skill discovery & direct invocation |
| **config** | `config.get`, `config.validate` | Agent/permission/MCP config inspection |
| **mcp** | `mcp.list_tools`, `mcp.describe_tool` | MCP tool discovery |
| **harness** | `harness.describe`, `harness.doc(topic)` | Self-awareness: compact harness summary + on-demand doc retrieval (§6) |

**Example: `task.run` (single-shot, for CLI)**

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "task.run",
  "params": {
    "task_description": "Implement login API endpoint",
    "agent_name": "engineer",
    "context": "See docs/auth.md for spec",
    "model_override": null
  }
}
```

Response (stream of events, ending with `task_finished`):

```
{"type":"task_started","task_id":"t-001"}
{"type":"agent_started","agent":"engineer"}
{"type":"tool_call","tool":"read_file","args":{"path":"src/auth.rs"}}
{"type":"tool_result","success":true,"output":"..."}
...
{"type":"task_finished","task_id":"t-001","exit_code":0,"transcript":{...}}
```

**Example: `session.attach` (streaming attach)**

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "session.attach",
  "params": { "session_id": "s-xyz" }
}
```

Response (streaming notifications until `session.detach` or completion):

```
{"type":"agent_message","content":"Reading files..."}
{"type":"tool_call","tool":"bash","args":{"cmd":"cargo test"}}
...
```

---

### 4.4 CLI as Thin Client

The `tcode` CLI (Phase 1) is a thin JSON-RPC client:

```bash
# Attach to live session
$ tcode attach session-id
  # Subscribes to session events, streams them to terminal
  # Forwards stdin (user interrupt, input) back to daemon

# Single-shot task (creates a transient session)
$ tcode run-task engineer "Implement login" --context "See docs/auth.md"
  # Creates a session, runs task.run, streams events, exits

# Session management
$ tcode session list         # → lists running sessions
$ tcode session status s-id  # → shows session state
$ tcode session cancel s-id  # → sends cancel signal
```

**No CLI-internal orchestration logic.** Every decision goes through the API.

---

### 4.5 Tool Layer

The tool system (`src/tools`) is the abstraction for ALL capabilities the PM and sub-agents can invoke:

**Tool types:**

1. **Filesystem** (`read_file`, `write_file`, `edit`) — path-confined, checked against project root.
2. **Execution** (`bash`) — subprocess runner with process-group cleanup.
3. **Delegation** (`delegate_to_agent`) — calls back into the daemon's runner layer.
4. **Code analysis** (`glob`, `grep`) — delegated to trusty-search MCP (Phase 1).
5. **Repository map** (`repo_map`) — backed by trusty-search tree-sitter symbols (Phase 1; see §5.1).
6. **MCP tools** — Phase 1 supports three integrations:
   - **trusty-search** (code search, symbol indexing, repo-map backing)
   - **trusty-memory** (persistent memory read/write)
   - **External servers** from `.mcp.json` (generic Claude-Code-compatible MCP client)
   
   All tools namespaced as `mcp__<server>__<tool>`.

**Per-agent gating** (`AgentConfig.tools.allowed`):

```toml
[agents.security-reviewer]
tools = ["read_file", "bash"]  # QA may not write
```

---

### 4.6 Provider & Model Layer

Each agent specifies its model (or uses a default). **Phase 1 supports OpenRouter only.** AWS Bedrock support is deferred to Phase 2.

```rust
#[async_trait]
pub trait Provider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse>;
}
```

**Model routing (Phase 1 — OpenRouter only):**

```toml
[agents.engineer]
model = "openai/gpt-4o"  # OpenAI via OpenRouter

[agents.qa]
model = "anthropic/claude-opus-4-1"  # Anthropic via OpenRouter

# Default (if unspecified)
[default]
model = "openai/gpt-4o-mini"
```

**Phase 2:** AWS Bedrock will be added as a second provider backend, enabling `model = "bedrock:claude-opus"` routing.

---

### 4.7 PM → Engineer Orchestration & Session Model

The **PM main loop** (Phase 1, in `src/agent_loop` and `src/runner`):

1. **Receive task** (from `task.run`, `session.attach`, or trusty-mpm).
2. **Assemble PM prompt** (BASE preamble + project CLAUDE.md + catch-up context).
3. **Enter multi-turn loop**:
   - Call the LLM with the assembled prompt.
   - Parse tool calls from the response.
   - Dispatch each tool (with permission checks, RBAC).
   - Feed results back to the model.
   - Repeat until "no tool call" (finish signal, per parity-spec §5) or turn limit.
4. **Extract final answer** (from the last no-tool-call turn).
5. **Emit `session_finished` event** with transcript, exit code, token usage.

**In-process engineer delegation:**

When the PM calls `delegate_to_agent("engineer", task_description)`:

1. Load engineer agent config.
2. Gate its tools (`tools.allowed`).
3. Assemble the engineer's system prompt (agent prompt + project context).
4. Run an `AgentLoop` with the engineer's model.
5. Return the engineer's output to the PM.
6. Roll engineer token usage onto the PM's token counter.

(All in the same process, same LLM client instance.)

---

### 4.8 trusty-console Integration & POST /rpc

trusty-mpm's **trusty-console** (the REST gateway for the trusty family) discovers and manages tcode daemons. Each project root gets one `tcode serve` instance registered with the console.

The console can:

- Start a tcode daemon per project (subprocess).
- Route API calls via `POST /rpc` (the daemon's HTTP endpoint).
- Relay events back to Claude Code via MCP notifications.

**No second gateway.** trusty-code daemons are accessed through trusty-console only.

---

### 4.9 Relationships to Sibling Harnesses

| Harness | Relationship |
|---------|--------------|
| **trusty-mpm** | trusty-mpm's console discovers and manages tcode daemons; trusty-mpm can spawn `tcode serve` for a session and monitor its events via hook relay or direct API call. |
| **trusty-agents** | trusty-agents can delegate coding tasks to tcode via the `task.run` API call (or CLI subprocess). |
| **trusty-search** | tcode may call trusty-search's MCP server (via `.mcp.json`) for `grep`, `search_semantic`, etc. OR tcode implements native `glob`/`grep` tools; this is an open question (§9). |
| **trusty-memory** | tcode may call trusty-memory via MCP for memory read/write. DOC-28 catch-up context is pulled from trusty-mpm's session registry (not directly from trusty-memory). |

---

## 5. Token-Efficiency Design

**Concrete mechanisms trusty-code will implement** (building on existing infrastructure, measured against Claude Code baseline).

### 5.1 Repo-Map: Trusty-Search Symbol Indexing + Personalized PageRank (Tier 1)

**Source:** Aider's repomap.py; Gousios et al. (2015).

**Architecture — REUSING trusty-search:**
1. **trusty-search** provides the symbol extraction layer (tree-sitter parsing + def/ref tagging, already built and cached).
2. **trusty-code** builds ONLY the ranking + render layer on top:
   - Fetch def/ref symbols from trusty-search via MCP or HTTP API.
   - Rank definitions by PageRank biased toward recently-edited files.
   - Render top-ranked identifiers into a configurable token budget (default: ~1k tokens).
3. **Graceful degradation:** If trusty-search is unavailable, repo-map is omitted from the prompt (does NOT block the loop; does NOT fall back to a fresh tree-sitter parser in Phase 1).

**Configuration:**

```toml
[code_harness]
repo_map_enabled = true           # default
repo_map_token_budget = 1000      # tokens; configurable per project
```

**Impact:** Primary value is **fewer search/read turns and less wrong-file exploration** — the agent goes straight to the right symbol/file instead of iteratively probing. Secondary benefit: -50 to -200 token delta in prompt size (vs. sending full file lists).

**Phase 1 implementation:** Lean ranking + render layer in `src/tools/repo_map.rs`; calls trusty-search via existing MCP client (§4.5). NO tree-sitter reimplementation.

---

### 5.2 Per-Model Edit-Format Selection (Tier 1)

**Source:** Aider A/B data (20% baseline → 61% success with udiff).

**Mechanism:**
1. Maintain a **format preference matrix** per model (learned from perf data or configured).
2. For each edit decision:
   - Try SEARCH/REPLACE format first (highest token efficiency).
   - Fall back to unified-diff if the model struggles.
   - Last resort: whole-file replacement.
3. Track success rates per format-model pair; adjust matrix as new data arrives.

**Impact:** -30 to -50 tokens per edit (output + follow-up rounds); 20%+ success improvement.

**Phase 1 implementation:** Simple matrix in agent config; full learning loop deferred to Phase 2.

---

### 5.3 Progressive-Disclosure Skill Loading (Tier 1)

**Source:** claude-mpm; 60–200 tok entry vs 3–6k full docs; ~87% discovery-token savings.

**Mechanism:**
1. Skills live at `.claude/skills/<name>/SKILL.md` (Claude-Code-compatible path).
2. Skills are split into two parts:
   - **Metadata** (name, description, one-line usage): always cached in memory (~100 tok per skill).
   - **Body** (full docs, code examples): loaded on-demand when invoked.
3. The PM/agents see only metadata in the prompt.
4. If an agent invokes a skill, fetch the full body from disk.

**Impact:** -500 to -1000 tokens for a 20-skill catalog (80% of skills never invoked per session).

**Phase 1 implementation:** Skill loader adapted from trusty-mpm's pattern, but configured for `.claude/skills/` path (not `~/.trusty-mpm/skills/`). Lazy-body resolver built into the skill loader.

---

### 5.4 Non-Destructive Compaction + Last-Message Replay (Tier 1)

**Source:** OpenCode; Cline; defined in OpenCode RFC.

**Mechanism:**
1. When context approaches the limit, tag old messages with a `"compacted": true` marker.
2. Replace their content with a one-line summary.
3. Replay the last user message *after* compaction to re-anchor the model.
4. Keep the **active work zone** (last N turns + recent tool results) uncompacted.
5. Preserve skill outputs and user requests forever (never compacted).

**Impact:** -30% context growth in long sessions; fully auditable (can expand compacted zones).

**Phase 1 implementation:** Middleware in the agent-loop message buffer.

---

### 5.5 Prompt-Cache-Aware Content Ordering (Tier 2)

**Source:** Claude Prompt Caching API best practices.

**Mechanism:**
1. Order prompt sections by volatility:
   - **Static first**: BASE preamble, project CLAUDE.md, repo-map, agent prompt.
   - **Dynamic last**: current task, chat history, tool results.
2. This maximizes the "stable prefix" that the cache can reuse across requests.

**Impact:** -40% to -60% effective tokens if prompt caching is enabled.

**Phase 1 implementation:** Simple section reordering in `prompt::assemble_system_prompt`.

---

### 5.6 Deferred Tool-Schema Loading (Tier 1)

**Source:** Claude Code; Cline; recognized pattern.

**Mechanism:**
1. Send tool **metadata** (name, description) in the initial request (~100B each).
2. Full JSON Schema is fetched on-demand when the model asks for a specific tool's parameters.
3. This is transparent to the model; the LLM layer handles the deferral.

**Implementation Risk (CRITICAL):** Many model/provider APIs expect the **full tool schema set UPFRONT** in the request. True "transparent" deferral may NOT be possible with ordinary function-calling — the model may have no way to ask "what are the parameters for this tool?" Deferral may require a **two-step planning/tool-discovery protocol**: (1) model first asks "which tools exist?" (responses: tool names + descriptions); (2) schemas are injected for next turn. This differs fundamentally from standard tool calling and requires a spike to validate feasibility per provider.

**Impact:** -50 to -200KB for a large tool catalog (if deferral is feasible); massive potential for MCP. Risk may reduce impact for some providers.

**Phase 1 placement:** P1B (not P1A) — must not gate the control plane. If two-step protocol is required, it is a post-P1A refinement.

---

### 5.7 Subagent-Isolated Contexts (Tier 1)

**Source:** claude-mpm (open-mpm), Goose, Cline.

**Mechanism:**
1. Each sub-agent (engineer, QA, security) gets a fresh context window.
2. The PM only receives a short summary of the sub-agent's work (not the full transcript).
3. No context bleed between agents.

**Impact:** -20 to -50% per delegation (no "previous agent" context bloat).

**Phase 1 implementation:** Already built into `src/runner::InProcessAgentRunner` and `src/agent_loop`.

---

### 5.8 Structured Output & `finish_task` Tool (Tier 1)

**Source:** Claude Code; also known as "ReportFindings" tool in some harnesses.

**Mechanism:**

An agent signals completion by **calling the `finish_task` tool** (not returning prose). This is a validated JSON-schema tool call:

```json
{
  "type": "function",
  "function": {
    "name": "finish_task",
    "description": "Signal task completion with structured output.",
    "parameters": {
      "type": "object",
      "properties": {
        "status": {"type": "string", "enum": ["completed", "failed", "cancelled"]},
        "summary": {"type": "string"},
        "changes": {
          "type": "array",
          "items": {"type": "object", "properties": {"file": {"type": "string"}, "lines_added": {"type": "integer"}, "lines_removed": {"type": "integer"}}}
        },
        "tests_run": {"type": "integer"},
        "tests_passed": {"type": "integer"}
      },
      "required": ["status", "summary"]
    }
  }
}
```

**Repair Path:** If the model emits malformed JSON (e.g., invalid enum value, missing required fields), the executor validates and returns a recoverable error message instructing the model to correct and retry.

**Impact:** -20 to -30% per agent output; deterministic (no prose interpretation, no re-asking for clarification).

**Phase 1 implementation:** Define `finish_task` schema in `src/tools/`, implement in `ToolRegistry`. P1A includes the tool availability; P1B includes mode-aware prompt assembly that teaches agents to use it.

---

### 5.9 Reconciliation with Parity-Spec Decision D2

**The Problem:** The parity-spec (§2d, decision D2) mandates **full tool schemas** sent to every model, identically, for benchmark fairness. The production harness wants **deferred schemas** for token efficiency (mechanism §5.6).

**Resolved:** **Two modes, three-tier selection hierarchy** (owner-approved).

| Mode | Full schemas | Deferred schemas | Compaction | Edit-format selection | Repo-map | When to use |
|------|:-:|:-:|:-:|:-:|:-:|---|
| **Parity / Benchmark** | ✓ | — | — | unified-diff only | — | Cross-model comparison research |
| **Daily-Driver** | — | ✓ | ✓ | per-model optimized | ✓ | Production; real coding work (DEFAULT) |

**Selection mechanism (three-tier, highest to lowest precedence):**

1. **Environment variable** (escape hatch): `TRUSTY_CODE_MODE=parity` or `daily-driver`. Overrides all.
2. **Per-task override** (API layer): `task.run` params include optional `mode: "parity" | "daily-driver"`. Overrides setting.
3. **settings.json config** (default): `code_harness.mode` key (default: `daily-driver`).

**Example configuration:**

```json
{
  "code_harness": {
    "mode": "daily-driver",
    "repo_map_token_budget": 1000
  }
}
```

**Example per-task override:**

```json
{
  "jsonrpc": "2.0",
  "method": "task.run",
  "params": {
    "task_description": "Implement login",
    "agent_name": "engineer",
    "mode": "parity"
  }
}
```

The parity-spec document itself remains frozen and normative for benchmark runs. The production harness layers these token-efficiency mechanisms on top as a separate feature set.

---

## 6. Self-Awareness & Embedded Documentation Model

**Purpose:** trusty-code is self-describing without polluting the context. The harness ships embedded documentation (not external files), and agents access harness identity/capability/doc information via progressive-disclosure JSON-RPC methods, not full-text dumps.

### 6.1 Crate-Embedded Documentation

trusty-code embeds its own documentation inside the crate as markdown assets (e.g., a `docs/` asset directory or embedded strings), so a running daemon is self-describing without requiring external files on the operator's filesystem. Embedded docs cover:

- Identity: "what is trusty-code" (the harness purpose, architecture, versioning).
- Capability: "what can this harness do" (tool surface, agent types, modes, API surface).
- Guides: specific how-to sections (setup, common patterns, token-budget tuning, parity mode vs daily-driver selection).

### 6.2 Compact Harness Summary

A SHORT **harness summary** (< 500 tokens) describes the harness in a way safe to inject into agent/system contexts cheaply:

- **Identity**: "You are trusty-code, a daemon harness for code generation and tool-driven tasks."
- **Capabilities**: "I can: read/write/edit files, run bash, search code, recall memory, delegate to agents. I have two modes: daily-driver (token-efficient) and parity (benchmark-safe)."
- **Tools**: List of available tools (names + one-line descriptions only; full schemas on-demand).
- **Constraints**: "I run with filesystem sandbox, bash approval gates, symlink protection, secrets redaction."
- **Links**: "For more, call `harness.doc('tool-reference')` or `harness.doc('modes')`."

### 6.3 Links, Not Dumps (Progressive Disclosure)

The **anti-pattern being avoided**: dumping full harness documentation into every context (context pollution). Instead:

- Summary is always embedded; full docs are NEVER injected unless explicitly requested.
- Agents receive the compact summary + a manifest of available doc topics.
- Agents can call `harness.doc(topic)` to fetch a specific fuller doc on-demand.
- Token impact: baseline context stays tiny (~100 tokens for summary); fuller context loaded only when needed.

This matches the trusty-ecosystem's identity/self-awareness pattern (WHAT-IS-* + get_prompt_context progressive-disclosure).

### 6.4 JSON-RPC Methods for Self-Description

Two new API methods in the harness method family (§4 API surface):

**`harness.describe()`** → Returns: `{ identity: string, capabilities: [string], tools: [{name, description}], modes: [string], manifest: {topic: string, size_tokens: integer}[] }`

Example response:
```json
{
  "identity": "trusty-code: per-project coding harness with daemon-first event-driven architecture",
  "capabilities": ["file-i/o", "bash", "code-search", "memory-recall", "agent-delegation"],
  "tools": [
    {"name": "read_file", "description": "Read file contents with line-range support"},
    {"name": "finish_task", "description": "Signal task completion with structured output"}
  ],
  "modes": ["daily-driver", "parity"],
  "manifest": [
    {"topic": "tool-reference", "size_tokens": 800},
    {"topic": "modes", "size_tokens": 500},
    {"topic": "architecture", "size_tokens": 2000}
  ]
}
```

**`harness.doc(topic: string)`** → Returns the full embedded doc for a specific topic (e.g., "tool-reference", "modes", "architecture"). Returns: `{ topic: string, content: string, size_tokens: integer }`

### 6.5 Instruction Validity & Conformance

The assembled harness instructions (BASE preamble + agent prompt + project CLAUDE.md) should reference the compact summary. A conformance check validates that the assembled instructions are syntactically correct and align with the harness capability manifest.

This ties M1's "instructions are valid" acceptance criterion: before a task runs, the instruction assembly is validated against the known harness surface.

---

## 7. Anti-Patterns & Risks (Lessons from Production Harnesses)

**Avoid explicitly**, with issue citations:

| Anti-Pattern | Where it happened | How trusty-code prevents it |
|--------------|-------------------|----------------------------|
| **Global/shared agent & skill namespace** | claude-mpm #924 | All agent/skill/session state is **project-scoped**; no cross-project namespace pollution. |
| **Unbounded agent/skill catalog in every context** | claude-mpm #923 | Progressive-disclosure skills (metadata only by default). Repo-map replaces huge file lists. |
| **Unbounded context without spend cap** | Cline reported #1 user complaint | Phase 1: configurable turn limit, token budget cap, wall-clock timeout. Phase 2: learn model-specific limits. |
| **Invariants documented but unenforced** | claude-mpm #922, #919 (circuit breaker bugs) | **Verification gates as first-class design** (§4.7 orchestration loop). Diff validation, test existence checks, circuit breaker wired from day 1. |
| **No session attach/detach protocol** | Claude Code (tmux-bound) | Axiom 4: daemon owns sessions; session-attach protocol is core API. |
| **Tool-schema parity violations** | implicit (would break benchmarks) | parity-spec D2 enforced at assembly time; benchmark mode guarantees byte-identical schemas. |
| **Large-repo performance collapse** | Cline shadow-git (per-tool-call checkpointing) | repo-map cached on disk (mtime-keyed); git operations only at phase boundaries, not per-tool. |

---

## 7. Current State & Gap Analysis

### What Works Today (Phase 1-3 modules extracted from open-mpm)

| Module | Status | Notes |
|--------|--------|-------|
| `events` | ✓ | Broadcast event bus; NDJSON relay |
| `ipc` | ✓ | NDJSON framing for subprocess IPC |
| `perf` | ✓ | Per-phase latency/token/cost instrumentation |
| `intent` | ✓ | Intent classifier for fast-path |
| `progress` | ✓ | Phase progress reporter |
| `build_info` | ✓ | Version + build counter |
| `tools` | ✓ | Tool traits, registry, `delegate_to_agent` |
| `rbac` | ✓ | ServiceTier, UserIdentity, permission gating |
| `agents` | ✓ | TOML config loading, `discover_agents` |
| `llm` | ✓ | OpenRouter HTTP client + request/response types |
| `provider` | ✓ | Trait abstraction for OpenRouter + Bedrock (Bedrock stubbed) |
| `project_context` | ✓ | Load `.claude/` CLAUDE.md with size cap |
| `identity` | ✓ | CallerIdentity, RecallCeiling for memory scoping |
| `logging` | ✓ | Tracing init |
| `agent_loop` | ✓ | Multi-turn LLM loop (bounded by turns/timeout) |
| `runner` | ✓ | InProcessAgentRunner; tool gating + model routing |
| `run_task` | ✓ | End-to-end `tcode run-task` CLI command (works for single task) |
| `prompt` | ✓ | Parity-spec assembler (BASE + agent + project context + fallback guidance) |
| `catchup` | ✓ | DOC-28 PM catch-up context injection |

### What is Stubbed / Exits with "not implemented"

| Surface | Issue | Impact |
|---------|-------|--------|
| `tcode serve` (daemon mode) | #587 Phase 1 | The daemon is not running; JSON-RPC/STDIO surface is not wired. |
| `tcode run-workflow` | #587 Phase 3 | Workflow engine for declarative multi-phase pipelines. |
| AWS Bedrock provider | Stubbed in `src/provider/` | `provider_for("bedrock:...")` returns `unimplemented!()`. |
| MCP tool bridge | `.mcp.json` parsing exists; tool invocation not wired | Can't call MCP servers yet. |
| Glob/grep tools | #1027 | Not yet exposed to agents. |
| Repo-map tool | #1027 | Not yet built. |
| Skill progressive-disclosure | Partially in trusty-mpm; not ported | Skills always sent in full. |
| Non-destructive compaction | Not in trusty-code yet | Context simply truncates when limit hit. |
| Parity-spec per-model edit-format selection | Unified-diff only | SEARCH/REPLACE fallback not implemented. |

### Known Unfixed Bugs (#1475)

| Bug | Severity | Fix in Phase |
|-----|----------|--------------|
| Cost priced at PM-model rate even when engineer uses a different model | Medium | Phase 1 (fix immediately; trivial) |
| Transcript records requested model slug, not resolved slug | Low | Phase 1 |
| `git add -AN` mutates the index as a side-effect (non-determinism) | Medium | Phase 1 |
| Before/after diff unreliable on dirty tree | Medium | Phase 1 (git operations only at phase boundaries) |
| `collect_files` has no symlink-loop guard (stack-overflow DoS) | High | Phase 1 |

### Long-pole Feature (#1023)

**Cross-provider tool-calling fallback** — enable tool use on OSS models (Qwen, DeepSeek, Gemma) that have weak/absent `tool_calls` support.

- Extraction strategies defined in #1023 matrix (JSON, angle-bracket format, etc.).
- NOT blocking Phase 1; lives in Phase 2+ (#1023).
- Parity-spec already accommodates fallback guidance (§4 in parity-spec).

---

## 8. Capability-Milestone Roadmap (M1 / M2 / M3)

trusty-code is delivered as three **canonical owner-facing capability milestones** (M1, M2, M3). Each milestone is a demonstrable increment in harness power. Underlying these milestones are three engineering-delivery phases (P1A, P1B, P1C) that respect the control-plane-first principle from §13 Cut Line.

### Overview: Milestones and Engineering Phases

| Milestone | Goal | Owner Demo | Engineering Phases |
|-----------|------|-----------|-------------------|
| **M1** | Context, Self-Awareness & Delegation | Launch daemon, access memory/search, delegation works, instructions valid, self-aware | P1A (control plane) + MCP integrations (from P1C) + self-awareness docs (new) |
| **M2** | Tool Calling | Robust portable tool surface, per-model edit formats, cross-provider fallback | P1B token-efficiency items + #1023 cross-provider fallback |
| **M3** | Coding Agent Delegation & Real Tasks | PM delegates real coding, agent edits files, simple task completes with passing tests | P1A + P1B + P1C hardened end-to-end + repo-map + agent definitions |

**Principle:** M1 **builds ON the P1A control-plane cut line** (§13). P1A proves the daemon foundation internally; M1 adds external integrations and self-awareness to make it production-visible. M2 and M3 layer on top.

---

### Milestone 1: Context, Self-Awareness & Delegation

**Goal:** tcode launches against a real repo and proves the harness foundation is live and integrated.

**Demonstrable Acceptance Criteria:**

- [ ] Launch `tcode serve` (or `tcode run-task`) against a target repo.
- [ ] Accesses **trusty-memory** to recall project context (via the MCP client). Demonstrates: agent can retrieve and use persistent memory.
- [ ] Uses **trusty-search** to BUILD CONTEXT (code/repo search) before/while working. Demonstrates: agent navigates the repo using symbol search.
- [ ] **Agent delegation works end-to-end** (PM → sub-agent round-trip, observable via events). Demonstrates: delegation yields correct output; PM receives and processes result.
- [ ] **Harness instructions are VALID** — the instruction-assembly / parity path (BASE preamble + agent prompt + CLAUDE.md) produces correct, working harness instructions. Includes a conformance check.
- [ ] **Self-awareness without context pollution** — tcode can answer "what am I / what can I do" from a COMPACT embedded harness summary, with LINKS to fuller docs loaded ON DEMAND (progressive disclosure, see §6 Self-Awareness).
- [ ] **M1 E2E Suite** — Full end-to-end suite (§9 Requirement 3) driving real `tcode serve` daemon over stdio and HTTP, CLI invocations, and JSON-RPC assertions on responses/events. M1 is not complete until this suite passes.

**Engineering Deliverables (from P1A control plane + P1C integrations + new self-awareness):**

- P1A: Daemon foundation, session model, task/delegation APIs, event streaming, CLI refactor, #1475 fixes (all control-plane essentials per §13).
- P1C: MCP client wiring for trusty-search + trusty-memory (exercised in M1, not deferred).
- New: Crate-embedded harness docs, `harness.describe` / `harness.doc(topic)` JSON-RPC methods, compact summary + progressive-disclosure links.

---

### Milestone 2: Tool Calling

**Goal:** A robust, provider-portable tool-calling surface.

**Demonstrable Acceptance Criteria:**

- [ ] Filesystem tools (read_file, write_file, edit), bash, and repo/search tools are all callable and reliable.
- [ ] Structured `finish_task` tool convention working (validated JSON-schema + repair loop per §5.8). Agent can finish a task and report structured completion.
- [ ] **Cross-provider tool-calling fallback** (#1023) enables models WITHOUT native function-calling to still use tools (fenced-JSON / tolerant-scan strategies + validate-repair loop).
- [ ] Per-model edit-format selection (unified-diff default, SEARCH/REPLACE fallback). Different models get appropriate edit formats.
- [ ] **M2 E2E Suite** — Full end-to-end suite driving real daemon, exercising all tool-calling paths (fs, bash, repo, search), format selection fallbacks, and structured finish_task. M2 is not complete until this suite passes.

**Engineering Deliverables (from P1B + #1023):**

- P1B: Per-model edit-format selection, progressive-disclosure skills, non-destructive compaction, deferred schemas, `finish_task` tool, mode-aware prompt assembly.
- #1023: Cross-provider tool-calling fallback matrix + extraction strategies for OSS models.

---

### Milestone 3: Coding Agent Delegation & Simple Coding Tasks

**Goal:** Real end-to-end coding.

**Demonstrable Acceptance Criteria:**

- [ ] PM delegates a real coding task to a coding agent (e.g., "implement a login endpoint").
- [ ] Agent edits files through the tool surface; changes actually land in the repo (write_file / edit success).
- [ ] Diff / transcript / cost captured correctly (depends on #1475 fixes from P1A).
- [ ] A SIMPLE coding task (e.g., a small function + its unit test) completes and is verifiable (tests pass, diff applies cleanly).
- [ ] **M3 E2E Suite** — Full end-to-end suite exercising real PM→agent delegation, file edits, repo-map integration, and actual coding task completion with passing tests. M3 is not complete until this suite passes.
- [ ] **Bake-Off Capstone:** trusty-code, run against the `ai-coding-bake-off` L1-L3 challenges (`~/Projects/ai-coding-bake-off`), achieves scores COMPARABLE to the claude-code (4.53) / codex (4.49) band on BOTH automated pytest pass-rate AND peer-review average — WHILE demonstrating LOWER real token/dollar cost (measured from tcode's own perf/usage telemetry, not self-reported). L4-L5 deferred (5–70× more expensive). This validates the token-efficiency thesis: harness quality > model quality.

**Engineering Deliverables (from P1A + P1B + P1C hardened end-to-end + repo-map + agent definitions):**

- P1A: Session/task/delegation APIs, transcript, CLI.
- P1B: Token-efficiency (edit formats, compaction, etc.).
- P1C: Repo-map ranking layer + MCP integrations (exercised at scale in M3).
- New: Hardened coding-agent TOML definitions (engineer, QA, security-reviewer), end-to-end testing scenario.
- **API-Driven Bake-Off Runner** — A runner (reference template: `scripts/oneshot_bakeoff.py`) that drives tcode via its JSON-RPC API against a bake-off level, writes solution files + REAL cost metadata (from tcode telemetry) to harness output dir, and runs the pytest verifier. Doubles as M3's e2e/testability artifact (ties to §9 Testability).

---

### Phase 1: Daemon Control-Plane Foundation + Token-Efficiency Layers (5-6 months total)

**Overarching principle:** Phase 1 (engineering delivery) is split into three sub-phases (P1A, P1B, P1C) that map to the three owner milestones (M1, M2, M3). **P1A is the internal control-plane foundation** (§13 Cut Line): it is not a milestone by itself, but the platform on which M1 builds.

---

### Phase 1A: Daemon / Session / API Control-Plane Foundation (Internal Checkpoint)

**Duration:** 2-3 months. **This is the Phase 1 Cut Line (§14).**

**Purpose:** Prove the daemon/session/event/API control plane works end-to-end. Every user-facing feature is a thin client over the JSON-RPC surface. No token-efficiency layers, no external dependencies.

**Deliverables:**

- [ ] Wire `tcode serve --stdio` entry point (JSON-RPC/STDIO dispatcher, NDJSON framing).
- [ ] Implement HTTP endpoint for `/rpc` (JSON-RPC over HTTP POST handler).
- [ ] Build session model + session-attach protocol (`session.{list,create,attach,send,detach,cancel,status}` JSON-RPC methods).
- [ ] Event bus + event streaming over stdout/HTTP (NDJSON lifecycle/tool/session events; NOT token-by-token model output — see Phase 2).
- [ ] Cancellation semantics (session.cancel, in-flight turn cleanup).
- [ ] Transcript inspection (`session.get_transcript`, time-bounded retrieval).
- [ ] Task execution through the API (`task.run` JSON-RPC method, single-shot or sessionful).
- [ ] Engineer task delegation in-process (via session + PM loop, no subprocess).
- [ ] Mode selection mechanism wiring (settings.json key, task.run override, env var escape hatch per Resolved Decision #1).
- [ ] CLI refactor (`tcode attach`, `tcode session list`, `tcode run-task`, `tcode cancel`) as thin clients over JSON-RPC.
- [ ] Fix #1475 bugs (cost model routing, transcript slug accuracy, git index mutation, diff reliability on dirty tree, symlink-loop guard per §11).
- [ ] Comprehensive Phase 1A unit + integration tests (cargo test -p trusty-code).
- [ ] Regression baseline snapshot (latency, event counts, transcript sizes for control-plane-only tasks).

**Proposed Epic / Issue Breakdown (P1A):**

- #587-P1A-1: `tcode serve` daemon foundation + JSON-RPC/STDIO wiring
- #587-P1A-2: Session model + attach/detach protocol API surface
- #587-P1A-3: Event bus + streaming (lifecycle/tool/session events over stdout + HTTP)
- #587-P1A-4: Task execution through API (`task.run` method + in-process engineer delegation)
- #587-P1A-5: Cancellation + session lifecycle management
- #587-P1A-6: Transcript inspection API
- #587-P1A-7: Mode selection mechanism (daily-driver / parity / override)
- #587-P1A-8: CLI refactor (thin client over JSON-RPC)
- #587-P1A-9: Fix #1475 known bugs (cost, transcript, git, diff, symlink)
- #587-P1A-10: P1A integration tests + regression baseline

---

### Phase 1B: Token-Efficiency Tier 1 (Layers after control plane)

**Duration:** 1-2 months. **Prerequisite: P1A cut line achieved.**

**Purpose:** Layer token-efficiency mechanisms on top of the proven control plane without changing the API surface.

**Deliverables:**

- [ ] Per-model edit-format selection (Tier 1; SEARCH/REPLACE fallback, fallback repair loop).
- [ ] Progressive-disclosure skill loading (adapted from trusty-mpm pattern; `.claude/skills/` metadata cached, bodies on-demand).
- [ ] Non-destructive compaction + last-message replay (mark pruned, preserve active zone + skill outputs).
- [ ] Deferred tool-schema loading (metadata upfront, bodies on-demand; see implementation risk in §5.6).
- [ ] Structured-output `finish_task` tool (validated JSON-schema finish convention per §5.8, repair loop for malformed JSON).
- [ ] Mode-aware prompt assembly (daily-driver applies all optimizations; parity mode disables token efficiency per §5.9).

**Proposed Epic / Issue Breakdown (P1B):**

- #587-P1B-1: Per-model edit-format selection
- #587-P1B-2: Progressive-disclosure skill loading (`.claude/skills/` path)
- #587-P1B-3: Non-destructive compaction + last-message replay
- #587-P1B-4: Deferred tool-schema loading (planning/discovery protocol spike)
- #587-P1B-5: Structured-output `finish_task` tool + repair loop
- #587-P1B-6: Mode-aware prompt assembly + parity mode validation

---

### Phase 1C: MCP + Repo-Map Integration (Layers after control plane)

**Duration:** 1-2 months. **Prerequisite: P1A cut line achieved. Can run in parallel with P1B.**

**Purpose:** Wire external integrations (MCP servers, symbol indexing) without blocking the control plane.

**Deliverables:**

- [ ] MCP client wiring (trusty-search, trusty-memory, external `.mcp.json` servers per Resolved Decision #5).
- [ ] Repo-map ranking layer (reuse trusty-search symbol indexing; graceful degradation if unavailable per §5.1).
- [ ] Repo-map token-budget enforcement (configurable default ~1k tokens).

**Proposed Epic / Issue Breakdown (P1C):**

- #587-P1C-1: MCP client wiring (trusty-search, trusty-memory, external servers)
- #587-P1C-2: Repo-map ranking layer (trusty-search symbol integration)
- #587-P1C-3: Repo-map token-budget enforcement + graceful degradation

---

### Phase 2: Advanced Orchestration + UI Foundation + AWS Bedrock (3-4 months)

**Deliverables:**

- [ ] AWS Bedrock provider support (deferred from Phase 1; enables `model = "bedrock:..."` routing).
- [ ] Plan/Act mode split (read-only planning phase vs write/execute).
- [ ] Per-agent permission matrix (hidden subagent restrictions).
- [ ] Prompt-cache-aware content ordering (Tier 2; if Claude API support available).
- [ ] Workflow engine (from trusty-agents, migrated to trusty-mpm; re-expose in tcode).
- [ ] TUI foundation (ratatui-based session monitor; attachable, non-blocking).
- [ ] REST resources shim (thin resource-oriented HTTP wrapper over JSON-RPC core; trusty-console remains the ONE public REST gateway).
- [ ] Streaming LLM responses (token-by-token model output streaming; Phase 2 confirmed; lifecycle/tool/session events already stream in P1A).
- [ ] Cross-provider tool-calling fallback (#1023; enable OSS models).
- [ ] Integration with trusty-console (daemon discovery, event relay).
- [ ] Circuit-breaker enforcement (from trusty-mpm; enforce in tcode).
- [ ] Per-conversation session state (multi-turn attach/detach resilience).

**Proposed Epic / Issue Breakdown:**

- #587-P2-1: AWS Bedrock provider (deferred from Phase 1)
- #587-P2-2: Plan/Act mode split + hidden subagent enforcement
- #587-P2-3: TUI foundation (ratatui session monitor)
- #587-P2-4: Workflow engine port + multi-phase orchestration
- #587-P2-5: REST resources shim (resource-oriented HTTP wrapper over JSON-RPC)
- #587-P2-6: Streaming LLM responses (token-by-token model output)
- #587-P2-7: Cross-provider tool-calling fallback (#1023 integration)
- #587-P2-8: trusty-console integration (daemon discovery + event relay)
- #587-P2-9: Circuit-breaker enforcement in tcode
- #587-P2-10: Per-conversation session resilience

---

### Phase 3: Domain-Specific Agents + Advanced Streaming (2-3 months)

**Deliverables:**

- [ ] Specialized sub-agents (security-reviewer, performance-auditor, doc-writer).
- [ ] Streaming tool output (long-running bash commands; beyond Phase 2's LLM token streaming).
- [ ] TELGUI foundation (Telegram UI over JSON-RPC).
- [ ] Multi-project workspace support (scope limit; trusty-mpm owns multi-project).
- [ ] Agent auto-discovery from community registries (if applicable).

**Proposed Epic / Issue Breakdown:**

- #587-P3-1: Domain-specific sub-agents (security, perf, docs)
- #587-P3-2: Streaming LLM responses + event emission
- #587-P3-3: TELGUI foundation (Telegram session monitor)
- #587-P3-4: Workspace scope boundaries (max N projects, rate limits)
- #587-P3-5: Community agent registry integration

---

### Phase 4 & Beyond

- Model routing learning (per-model performance tracking).
- Advanced compaction strategies (attention-based importance scoring).
- IDE plugins (VS Code, JetBrains native attachment to tcode daemons).
- Sandbox / approval-mode orthogonal design (Codex-inspired).

---

## 9. Testability & E2E Testing

**Standing Owner Directive:** trusty-code must be 100% testable by CLI/API, and each milestone ships full end-to-end testing. This section codifies the testability requirement as a first-class architectural constraint tied to the daemon-first + CLI/API-first design.

### Requirement 1: 100% CLI/API-Testable

Every capability of trusty-code MUST be reachable and exercisable through the CLI or the JSON-RPC API. No functionality may be reachable only via internal code paths or private test harnesses.

**Rationale:** This is the payoff of the daemon-first + CLI/API-first architecture (Axioms 2, 3, 4). The entire tool is black-box testable; operators and testers interact with the SAME surface that development tests use. There is no "internal API" or "private testing backdoor" that differs from the user-facing surface.

### Requirement 2: Per-Issue E2E Coverage

Each implementation issue carries API-driven integration/e2e coverage for its slice. Tests that implement an issue must:

- Spawn the real `tcode serve` daemon over both **stdio** AND **HTTP** transports.
- Invoke the real CLI (`tcode attach`, `tcode run-task`, etc.) as a subprocess speaking the actual JSON-RPC protocol.
- Assert on real responses, error codes, events, and transcripts received over the wire.

**Rationale:** Unit tests alone are insufficient. E2E tests drive the real daemon and catch integration failures (framing, protocol, event ordering, session lifecycle) that unit tests miss. Every issue is a slice of the public surface; it must be validated against that surface.

### Requirement 3: Per-Milestone E2E Suite (Merge Gate)

Each milestone ships a full end-to-end suite driving the real CLI/API. The milestone is not "done" until the e2e suite passes against the real CLI/API surface.

**M1 E2E Suite (Concretely):** The §13 Cut Line scenario, black-box:
1. Start `tcode serve --stdio` (or over HTTP).
2. Create a session via JSON-RPC (`session.create`).
3. Run an engineer task via JSON-RPC (`task.run` or `session.send`).
4. Receive lifecycle/tool/session events in real-time (observable via NDJSON stdout or `/events` SSE endpoint).
5. Cancel the session via JSON-RPC (`session.cancel`).
6. Inspect the transcript via JSON-RPC (`session.get_transcript`).
7. Replay the same task through the thin CLI (`tcode run-task engineer "..."`), which internally calls the API.
8. Assert on all outputs, event streams, exit codes, and transcript integrity.

This M1 suite lands with issue #2062 (M1 E2E Validation); earlier M1 issues (#587-P1A-1 through #587-P1A-10) each add their own API-level e2e slice so the full suite is built incrementally.

**M2 & M3 suites:** Defined similarly, exercising tool-calling and coding workflows end-to-end. M3's suite includes the API-driven bake-off runner (reference: `scripts/oneshot_bakeoff.py`) that validates trusty-code against the `ai-coding-bake-off` L1-L3 challenges for the bake-off capstone criterion.

### Requirement 4: E2E is a Merge Gate

A milestone is not complete until its e2e suite passes. All pull requests contributing to a milestone must either add e2e coverage for their slice or inherit coverage from a prior issue. No feature merges without e2e validation against the real CLI/API surface.

**Tie-ins:**
- §8 Milestone acceptance criteria: Each milestone's acceptance criteria note that its e2e suite (driving real CLI/API) is required for sign-off.
- §13 Phase 1 Cut Line: The P1A control-plane foundation is internally validated by §9 Requirement 3 M1 suite.

---

## 10. Resolved Decisions (Owner-Approved 2026-07-06)

**These decisions were resolved by Bob Matsuoka and are now settled policy for Phase 1+ implementation.**

1. **D2 Reconciliation: Three-tier mode selection hierarchy** (DECIDED).  
   Settings.json key `code_harness.mode` (default: `daily-driver`), per-task `task.run` param override, env var `TRUSTY_CODE_MODE` escape hatch. See §5.9 for mechanism and examples.

2. **Provider Strategy: OpenRouter Phase 1, AWS Bedrock Phase 2** (DECIDED).  
   Phase 1 implements OpenRouter only; no Bedrock work in Phase 1. Bedrock added in Phase 2 as a second provider backend. See §4.6 and Phase 2 roadmap.

3. **Repo-map Token Budget: ~1k tokens default, configurable per project** (DECIDED).  
   Default budget 1000 tokens in `settings.json` key `code_harness.repo_map_token_budget`. Projects may increase/decrease. See §5.1.

4. **Repo-map Source: Reuse trusty-search symbol indexing** (DECIDED).  
   trusty-code calls trusty-search API/MCP for def/ref tags; implements ONLY ranking + token-budget render layer. NO tree-sitter reimplementation in Phase 1. Graceful degradation if trusty-search unavailable (repo-map omitted, loop continues). See §5.1 and §4.5.

5. **MCP Client Support Scope: Three integrations in Phase 1** (DECIDED).  
   (a) trusty-search (code search, repo-map backing), (b) trusty-memory (persistent memory), (c) external servers from `.mcp.json`. These three are MVP Phase 1 scope. See §4.5 and Phase 1 roadmap.

6. **Streaming LLM Responses: Phase 2** (DECIDED).  
   Phase 1 is request-response only. Streaming is Phase 2. See Phase 1 & Phase 2 roadmaps.

7. **Skill Progressive Disclosure: Adapt trusty-mpm loader for `.claude/skills/` path** (DECIDED).  
   Reuse trusty-mpm's loader pattern; configure for `.claude/skills/` (Claude-Code-compatible path, NOT `~/.trusty-mpm/skills/`). See §5.3 and Phase 1 roadmap.

---

## 11. Security & Trust Model

This section defines the filesystem sandbox, approval thresholds, and trust levels for integrations.

### 10.1 Filesystem Sandbox & Path Confinement

**Requirement 1:** All file paths (`read_file`, `write_file`, `edit`) are resolved relative to the project root and confined to it. Attempts to read/write outside the project root are rejected with a permission error.

**Requirement 2:** Symlink traversal is prevented: symlinks are followed only if the resolved path is still within the project root. Circular symlink detection (§1475 symlink-loop guard) prevents stack-overflow DoS.

**Requirement 3:** Relative path traversal (`../../../etc/passwd`) is blocked: paths are canonicalised, and parent-directory escapes are caught at the executor layer before any I/O.

### 10.2 Bash Approval Model

**Auto-approved bash commands:**
- Read-only: `git log`, `git show`, `git status`, `ls`, `find`, `grep`, `cat`, `tail`, `head`, `wc`, `du`, `ps`.
- Information: `uname`, `pwd`, `whoami`, `date`, `echo`.

**Prompted (ask-to-run):**
- Writes: any command that creates/modifies files or environment state (`git add`, `cargo build`, `npm install`, `docker run`).
- Dangerous: `rm`, `mv`, `chmod`, `chown`, `kill`, `dd`, `format`.
- Network: `curl`, `wget`, `ssh`, `nc` (all outbound).

**Denied:**
- Shellcode injection vectors: backticks, `$()`, pipes to eval.
- Privilege escalation: `sudo`, `su`.

### 10.3 Write-Permission Policy

**Requirement 1:** Only the PM and explicitly gated agents (those with `tools: ["write_file", ...]`) may write.

**Requirement 2:** Sub-agents inherit the parent PM's write permission unless restricted by their `AgentConfig.tools.allowed` list.

**Requirement 3:** QA, security-reviewer, and other read-only agents are gated with `tools: ["read_file", "bash"]` only, no write.

### 10.4 MCP Server Trust Levels

**Bundled ecosystem servers** (trusty-search, trusty-memory via Phase 1C):
- Shipped with trusty-code.
- Assume trusted; no sandboxing.
- Tools are namespaced: `mcp__trusty_search__grep`, `mcp__trusty_memory__recall`.

**External servers** (from `.mcp.json`):
- User-declared; subprocess-based (stdio transport).
- Server process is spawned with project-root cwd.
- Tool output is logged; errors are caught and surfaced as tool results.
- No automatic redaction (see next section).

### 10.5 Environment-Variable Leakage Prevention

**Requirement 1:** The daemon does not inherit or expose user's `~/.env`, `~/.bashrc`, `.env`, or similar files.

**Requirement 2:** Sub-agent execution is isolated: only explicitly declared `[env]` vars from agent config are available to the agent.

**Requirement 3:** Secrets (API keys, tokens) in the environment are NOT automatically leaked. If an agent needs a secret, it is explicitly passed via `AgentConfig.env_vars` and then redacted in transcripts (see below).

### 10.6 Secrets Redaction in Transcripts & Events

**Requirement 1:** Common secret patterns (API keys, tokens, AWS credentials, SSH keys) are detected and redacted in all event output and transcripts before they are stored or streamed.

**Requirement 2:** Redaction pattern list is configurable per project via `settings.json`.

**Requirement 3:** Redacted values are replaced with `[REDACTED: <type>]` (e.g., `[REDACTED: AWS_ACCESS_KEY_ID]`).

---

## 12. Session Durability Model

This section defines how sessions persist, recover, and expire.

### 11.1 Transcript Persistence

**Requirement 1:** Session transcripts are stored in-memory during the daemon's lifetime.

**Requirement 2:** On daemon shutdown (SIGTERM + graceful drain), in-flight sessions are marked as "interrupted"; their transcripts are lost (recovery is a Phase 2 feature, not Phase 1A).

**Requirement 3:** Session metadata (session ID, task description, start time, status) is logged to stderr (for operator visibility) but not persisted to disk in Phase 1A.

### 11.2 Event Replay on Attach

**Requirement 1:** When a client attaches to a running session, it receives the **last N events** (configurable ring buffer; default: 1000 events or 10 MB, whichever is smaller).

**Requirement 2:** Events older than the ring buffer are not available; a freshly-attached client will see recent progress but not the full session history.

**Requirement 3:** Full transcript retrieval is via `session.get_transcript(session_id)`, which returns the accumulated history up to now.

### 11.3 Daemon Restart & Recovery

**Requirement 1:** In Phase 1A, session state is NOT persisted across daemon restarts. On restart, all sessions are lost.

**Requirement 2:** Operators are expected to use trusty-mpm's session recovery (DOC-28 catch-up) or manual re-run.

**Requirement 3:** Phase 2+ may add session durability (persisting transcripts to disk, recovery on restart), but this is explicitly out of scope for P1A.

### 11.4 Session TTL & Idle Expiry

**Requirement 1:** Sessions do not auto-expire in Phase 1A. A session remains in the daemon until it finishes or the daemon restarts.

**Requirement 2:** Explicit cancellation (`session.cancel(id)`) terminates a session immediately.

**Requirement 3:** Phase 2+ may add idle-expiry (auto-terminate after N minutes of inactivity), but this is not Phase 1A scope.

### 11.5 Ring-Buffer Bounds & Event History Limit

**Requirement 1:** The event bus maintains a bounded in-memory ring buffer (default: 1000 events).

**Requirement 2:** When the buffer is full, oldest events are dropped.

**Requirement 3:** Operators can query `session.get_transcript` for full accumulated transcript (a separate log, not the event ring buffer).

### 11.6 Cancellation Semantics

**Requirement 1:** `session.cancel(session_id)` sends a cancellation signal to the in-flight agent loop.

**Requirement 2:** The agent loop checks the cancellation flag at the end of each tool call. If cancelled, it breaks the loop, emits a `SessionFinished` event with `status: "cancelled"`, and returns.

**Requirement 3:** Cancellation is NOT instantaneous (if the agent is mid-tool-call, it will complete that call before checking the flag).

**Requirement 4:** The transcript up to cancellation is preserved and accessible via `session.get_transcript`.

---

## 13. API Error Model

This section defines standard error handling across JSON-RPC/STDIO and HTTP `/rpc`.

### 12.1 Standard JSON-RPC Error Envelope

All errors are returned in the JSON-RPC 2.0 error envelope:

```json
{
  "jsonrpc": "2.0",
  "id": <request-id>,
  "error": {
    "code": <integer>,
    "message": "<string>",
    "data": {
      "error_type": "<domain-error-type>",
      "details": "<optional human-readable details>"
    }
  }
}
```

### 12.2 Domain Error Taxonomy

| Error Type | Code | HTTP | Retryable | Meaning |
|---|---|---|---|---|
| `permission_denied` | -32001 | 403 | No | Caller lacks permission (RBAC, tool gate, agent restriction). |
| `not_found` | -32002 | 404 | No | Session, agent, or tool does not exist. |
| `invalid_argument` | -32003 | 400 | No | Malformed request (schema validation failure, invalid enum value). |
| `timeout` | -32004 | 504 | Yes | Request exceeded wall-clock or turn limit. |
| `cancelled` | -32005 | 499 | No | Session was cancelled by user. |
| `provider_failure` | -32006 | 502 | Yes | LLM provider (OpenRouter, Bedrock) returned an error. |
| `session_not_found` | -32007 | 404 | No | Requested session does not exist. |
| `internal_error` | -32603 | 500 | No | Server-side panic or unexpected state. |

### 12.3 Error Surfacing Over STDIO & HTTP /rpc

**STDIO (JSON-RPC 2.0):**
- Errors are emitted as single-line JSON over stdout: `{"jsonrpc":"2.0","id":1,"error":{...}}\n`
- Logs (including error context) go to stderr.

**HTTP `/rpc` POST:**
- Errors are returned with matching HTTP status code.
- JSON-RPC error envelope is the response body.

### 12.4 Retryable vs Terminal

**Retryable errors** (`timeout`, `provider_failure`): Caller should implement exponential backoff and retry.

**Terminal errors** (all others): Caller should not retry; examine the error and handle accordingly (e.g., fix the input for `invalid_argument`, handle cancellation for `cancelled`, etc.).

---

## 14. Phase 1 Cut Line

**Verbatim definition of the minimum shippable daemon (P1A milestone):**

> A user can start `tcode serve --stdio`, create a session via JSON-RPC, run an engineer task, receive lifecycle/tool/session events in real-time, cancel the session, inspect the full transcript, and replicate the same task through the thin CLI (`tcode run-task`) — all with no token-efficiency layers, no MCP integrations, and no external dependencies beyond OpenRouter.

**Scope:**

- JSON-RPC/STDIO dispatcher + HTTP `/rpc` endpoint.
- Session model with attach/detach protocol.
- Event streaming (lifecycle, tool calls, session state).
- Task execution API (`task.run`).
- Engineer delegation in-process.
- Transcript inspection.
- CLI refactor (thin clients over JSON-RPC).
- #1475 bug fixes (cost, transcript, git, diff, symlink).
- Security sandbox (filesystem, bash approval, redaction).
- Session durability model (in-memory, event replay, cancellation).
- API error model (standard JSON-RPC + domain taxonomy).
- Mode selection (daily-driver / parity / override).

**Out of scope (P1A):**

- Token-efficiency layers (P1B).
- MCP integrations (P1C).
- Streaming LLM responses (Phase 2).
- Bedrock provider (Phase 2).
- Workflows (Phase 2+).
- TUI/TELGUI (Phase 2+).

**Definition of "working":**

- All P1A deliverables (see Phase 1A §8) are implemented and tested.
- Regression baseline is captured (latency, event counts, transcript sizes).
- End-to-end scenario (create session → run task → cancel → inspect transcript) succeeds.

---

## 16. References

### External References (Tooling Benchmarks)

- **Aider repo-map:** [`aider/repomap.py`](https://github.com/paul-gauthier/aider/blob/main/aider/repomap.py)
- **Aider unified-diff A/B:** Gousios et al. "On the Efficiency of Test-Driven Software Development" (2015); Aider repo edit-format comparison data.
- **OpenCode server-is-product model:** [valhuber/GenAI-Stack](https://github.com/valhuber/GenAI-Stack)
- **OpenCode non-destructive compaction:** OpenCode RFC (internal; referenced in code review #2019).
- **Cline plan/act split:** [Cline repository](https://github.com/cline/cline)
- **Cline subagent isolation:** Cline issue #1037 (context windowing per subagent).
- **Claude Prompt Caching:** [Anthropic docs](https://docs.anthropic.com/en/docs/build-a-bot/manage-conversation-history#controlling-which-messages-claude-reads)

### In-Repo References

- **Parity Spec (frozen):** [`docs/trusty-code/parity-spec.md`](parity-spec.md) — cross-model comparison baseline; decision D1–D5.
- **Claude Code Compatibility Spec:** [`docs/trusty-code/research/claude-compat-spec-2026-06-02.md`](research/claude-compat-spec-2026-06-02.md) — config surface inventory.
- **Three-Harness Architecture:** [`docs/architecture/harnesses.md`](../architecture/harnesses.md) — trusty-code, trusty-mpm, trusty-agents boundary.
- **ADR-0004:** [`docs/adr/0004-three-harnesses-shared-event-driven-common.md`](../adr/0004-three-harnesses-shared-event-driven-common.md) — event-driven mandate, boundary decision.
- **Crate README:** [`crates/trusty-code/README.md`](../../crates/trusty-code/README.md)
- **Crate lib.rs doc:** [`crates/trusty-code/src/lib.rs`](../../crates/trusty-code/src/lib.rs) — module map, Why/What/Test pattern.
- **Epic #587:** [#587](https://github.com/bobmatnyc/trusty-tools/issues/587) — trusty-code extraction phases (P1–P6).
- **Known issues:** #1023 (cross-provider tool calling), #1027 (glob/grep tools), #1475 (cost, diff, git, symlink bugs).
- **DOC-28:** Session catch-up context (trusty-mpm session registry integration).
- **trusty-mpm circuit-breaker:** [`crates/trusty-mpm/src/core/circuit.rs`](../../crates/trusty-mpm/src/core/circuit.rs)
- **trusty-agents workflow engine:** [`crates/open-mpm/src/workflow/`](../../crates/open-mpm/src/workflow/) (planned migration to trusty-mpm)

---

## Appendix: Compliance with Parity-Spec

**This spec does NOT override the parity-spec** (`docs/trusty-code/parity-spec.md`). The parity-spec is normative for cross-model *comparison runs* (benchmark mode, Axiom 5.9 Option B).

The parity-spec defines:
- BASE protocol preamble (byte-identical §2a).
- Per-agent prompt injection (§2b).
- Project CLAUDE.md injection (§2c).
- Tool-schema count (Decision D2: full set, no cap).
- Fallback guidance for weak-tool-calling models (§4).
- Finish convention (no-tool-call turn, §5).

The vision spec *layers token-efficiency mechanisms on top* without violating parity for benchmark runs:

| Parity-Spec Rule | trusty-code Daily-Driver Mode | trusty-code Parity/Benchmark Mode | Conflict? |
|---|---|---|---|
| BASE identical across models | ✓ Maintained | ✓ Maintained | No |
| Agent prompt identical | ✓ Maintained | ✓ Maintained | No |
| CLAUDE.md identical | ✓ Maintained | ✓ Maintained | No |
| Full tool schemas | — (deferred) | ✓ (full set sent) | **Resolved by mode** |
| Fallback guidance per tier | ✓ Identical | ✓ Identical | No |
| Finish signal (no-tool-call) | ✓ Maintained | ✓ Maintained | No |

**Benchmark runs select parity mode (Axiom 5.9 Option B); production runs select daily-driver mode.**

---

**END OF SPEC**
