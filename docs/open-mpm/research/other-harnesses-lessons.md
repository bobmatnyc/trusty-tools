# Other Harnesses Lessons: Roo Code, Cline, and OpenAI Codex CLI

**Date**: 2026-04-22
**Scope**: Orchestration and agent management techniques applicable to open-mpm

---

## 1. Roo Code

Roo Code is a VS Code extension (forked from Cline) built around a mode-based multi-agent architecture. Its signature feature is **Boomerang Tasks** — now called Orchestrator mode — which allows a PM-style orchestrator agent to decompose complex work and spawn specialized sub-agents, each running in its own isolated conversation context. The Orchestrator issues a `new_task` tool call specifying a `mode` (e.g., Code, Architect, Debug) and a `message`. The sub-agent completes its work and calls `attempt_completion` with a result summary. Control returns to the Orchestrator with only that summary — never the full sub-agent context. This is deliberate: the Orchestrator is intentionally restricted from reading files directly, because doing so pollutes its context with low-level detail and causes "context poisoning." The parent task pauses, the child runs to completion in isolation, and only a compressed summary flows back up.

Each mode in Roo Code carries its own tool allowlist, file permission profile, and model assignment ("Sticky Models"). Architect mode is read-only; Code mode has full file and terminal access; a custom Review mode could be constrained to read-only operations. Different models can be pinned per mode — e.g., Gemini 2.5 for the Orchestrator (large context window for project scope) and Claude Sonnet for Code mode (speed and code quality). Roo Code switches models automatically when the user switches modes. This per-agent capability profile, separate from the system prompt, is a clean separation of concerns that open-mpm partially implements via TOML agent config but does not yet express as formal capability tiers.

Context management in Roo Code is handled by an **Intelligent Context Condensation** system (`autoCondenseContext`). By default, 30% of the context window is reserved (20% for output, 10% safety buffer), leaving 70% for history. When usage exceeds a configurable threshold (e.g., 80%), an LLM call summarizes older message history and replaces it in-place, displaying before/after token counts and cost. Slash commands embedded in the first message are preserved across condensations. As of version 3.36, condensation and sliding-window truncation preserve original messages internally so that rewinding to a checkpoint restores full history. File access is gated by `.rooignore` rules, checked before content is assembled into the user message.

---

## 2. Cline

Cline is the open-source VS Code extension that Roo Code forked. Its architecture centers on a strict **ReAct (Reason-Act-Observe) loop**: the LLM is expected to return a tool call on every turn. If it returns plain text instead, the system rejects it and sends back a structured error — `"[ERROR] You did not use a tool in your previous response! Please retry with a tool use."` — forcing the model back into the tool-use discipline. Cline supports over 20 modular tools organized so new capabilities can be added without touching core agent logic. It supports both sequential tool execution (safer, default) and parallel execution (higher throughput, for frontier models). The parallel path runs all identified tools simultaneously and recurses once with all results, rather than recursing after each individual tool.

Cline's most distinctive architectural contribution is its **shadow Git checkpoint system**. Every tool call that modifies the filesystem triggers a commit to a shadow Git repository stored in VS Code's global storage, completely separate from the user's own Git history. This provides two diff views: a step diff (between two consecutive checkpoints) and a task diff (from baseline to latest checkpoint). Checkpoints are used not just for rollback but also for context management: the `FileContextTracker` compares file modification timestamps against checkpoint timestamps when restoring conversation state, warning users about potential content mismatches. As of recent versions, condensation preserves the original messages so rewind fully restores context. The shadow Git approach has known fragility issues (repository corruption from `.git` folder renaming on Windows), but the conceptual model is sound.

Context management in Cline goes beyond simple sliding-window truncation. A `contextHistoryUpdates` map tracks modifications to message blocks over time, including timestamps, enabling point-in-time restoration of conversation state. Verbose content — like repeated file reads — can be replaced with concise notices to save tokens without losing semantic continuity. Cline reads state from disk on resumption from `api_conversation_history.json`, `cline_messages.json`, and `context_history.json`. For large codebases, Cline spawns **read-only sub-agents** for exploration: the main agent delegates codebase research to a read-only sub-agent, which reports findings back, and only the main agent makes modifications. Cline's MCP integration includes an MCP Marketplace (one-click installs) and MCP rules that automatically select the right MCP server based on conversation keywords — removing the need for the user to manually specify which tools to activate.

---

## 3. OpenAI Codex CLI

OpenAI Codex CLI is a terminal-based coding agent (open source, TypeScript) built on the OpenAI Responses API. Its agent loop is a multi-turn conversation where each turn can include many model-inference + tool-call iterations. Every turn ends with an assistant message signaling completion and returning control to the user. The system assembles a prioritized input list of messages with roles: system (server-controlled), developer (sandbox permissions, user config from `~/.codex`), and user (`AGENTS.md` content + actual query). This role hierarchy means the server can inject model-specific preamble without the client needing to know what it contains, while clients retain full control over task-specific context.

The sandbox architecture uses OS-native isolation: **macOS Seatbelt** (`sandbox-exec`) and **Linux bubblewrap + seccomp**. Three modes govern what the agent can touch: `workspace-write` (default — read/write within workspace, no network), `on-request` (sandbox by default, asks to go beyond), and `danger-full-access` (no restrictions). Separately, three approval policies control when the agent must pause for confirmation: **suggest** (all actions require approval), **auto-edit** (file changes auto-approved, shell commands need approval), and **full-auto** (everything runs without confirmation). Sandbox mode and approval policy are orthogonal controls layered on top of each other. The `--yolo` / `--dangerously-bypass-approvals-and-sandbox` flag removes both layers entirely and is documented as dangerous. Rollback relies on Git: Codex maintains a transcript of all actions, and `/diff` shows staged and unstaged changes. There is no shadow Git; the user's own Git history is the rollback mechanism.

Context management in Codex CLI is architecturally sophisticated. The system is carefully engineered for **prompt cache reuse**: system instructions, tool definitions, sandbox configuration, and environment context are kept stable and consistently ordered across requests to maximize KV-cache hits, achieving linear rather than quadratic token cost growth. Changing the tools list, switching models, or altering sandbox permissions mid-conversation invalidates the cache prefix. MCP servers that dynamically update their tool list (`notifications/tools/list_changed`) force Codex to append new developer messages rather than modifying earlier segments, preserving cache validity. For long sessions, a `/compact` command (and an automatic threshold) triggers an LLM summarization call that replaces the conversation history with a compressed form, including an opaque `type=compaction` item that encodes the model's latent understanding. **AGENTS.md** files are the project-scoped context injection mechanism: Codex walks from the Git root to the current working directory, collecting `AGENTS.md` files in each directory and injecting them as developer-role messages in root-to-leaf order before the user prompt. This hierarchical instruction discovery is analogous to CLAUDE.md walking but with explicit role separation between server-controlled and client-controlled context.

---

## 4. Technique Comparison Table

| Technique | Claude Code | Roo Code | Cline | Codex CLI | open-mpm |
|---|---|---|---|---|---|
| **Orchestrator → sub-agent delegation** | Via MCP/tools | `new_task` tool, built-in Orchestrator mode | Read-only sub-agents for exploration | Explicit subagent config in `config.toml` | `delegate_to_agent` tool, NDJSON IPC |
| **Context isolation per sub-agent** | Partial (shared thread) | Full isolation, only summary returned | Separate context for read-only agents | Each subagent has its own model turn | Full isolation (subprocess, separate stdin/stdout) |
| **Result compression on return** | No | Yes — `attempt_completion` summary only | Findings report only | Subagent returns bounded output | Not yet — full result passed back |
| **Per-agent model assignment** | Not native | Yes — Sticky Models per mode | Via provider settings | `[agents]` table in `config.toml` | Yes — model field in TOML config |
| **Per-agent tool allowlist** | Via permissions | Yes — per-mode tool permissions | Implicit (read-only vs. full) | Not explicit beyond sandbox mode | Yes — implemented |
| **Context condensation / summarization** | Yes (auto-compact) | Yes — LLM-based, configurable threshold | Yes — `contextHistoryUpdates` map | Yes — `/compact` + auto + Responses API | Not yet |
| **Shadow / parallel Git for rollback** | No | No (uses Roo tasks for isolation) | Yes — shadow Git repo per session | No (relies on user's Git + transcript) | Not yet |
| **Checkpoint-based context restore** | No | Yes — v3.36 preserves originals | Yes — timestamp-based restoration | Partial (conversation rewind via Esc) | Not yet |
| **AGENTS.md / hierarchical instruction files** | CLAUDE.md walk (single file) | `.roo/` config, mode definitions | Not applicable | AGENTS.md in every directory, root-to-leaf | CLAUDE.md + agent TOML (flat) |
| **Prompt cache engineering** | Yes (Anthropic cache control) | Partial | Not explicit | Yes — stable prefix ordering, explicit cache design | Not yet |
| **OS-native sandbox (seatbelt/bwrap)** | No | No | No | Yes — macOS seatbelt, Linux bwrap+seccomp | No |
| **Tiered approval modes** | Not native | Approval gates per subtask | Per-tool approval | suggest / auto-edit / full-auto | Not yet |
| **MCP integration** | Yes — native | Yes — full support | Yes — MCP Marketplace + rules | Yes — STDIO/SSE, first-class | Not yet |
| **Adaptive system prompt per model family** | Partial | Not explicit | Yes — XML vs. JSON tool format | Server-side, transparent to client | Not yet |
| **Read-only exploration sub-agents** | No | Orchestrator avoids file reads | Explicit read-only sub-agents | Subagents for bounded tasks | Not yet |

---

## 5. Top Actionable Lessons for open-mpm

The following techniques are not yet implemented in open-mpm and offer the highest value given the current architecture.

### Lesson 1: Return only a summary from sub-agents, not the full result

**What**: Roo Code's Orchestrator receives only the `attempt_completion` summary from a sub-agent — never the full conversation history or raw file reads. Cline's read-only exploration sub-agents return a findings report, not file dumps.

**Why it matters**: open-mpm currently passes the full sub-agent `content` field back to the PM. As task complexity grows, this floods the PM's context with low-level detail and wastes tokens. The PM should only receive the information it needs to decide the next delegation.

**How to implement**: Add a `summary` field to the `result` IPC message. Instruct sub-agents (via system prompt) to return a compressed findings summary as their final message, not raw artifacts. The PM can request the full artifact separately if needed.

---

### Lesson 2: Implement LLM-based context condensation with a configurable threshold

**What**: Both Roo Code and Codex CLI summarize conversation history when it approaches a token threshold (configurable, e.g., 80% of context window). Cline tracks message modifications with timestamps for point-in-time restoration.

**Why it matters**: open-mpm's `chat_with_tools` loop accumulates history indefinitely. Long-running multi-turn sessions against large codebases will eventually hit context limits and fail silently or produce degraded output.

**How to implement**: Track token count per turn (Anthropic's API returns usage). When total input tokens exceed a threshold (e.g., 70% of model's context window), call the LLM with a summarization prompt over the oldest N messages and replace them with the summary. Store a `condensed_at` marker for debugging. This is a self-contained change to the `chat_with_tools` loop.

---

### Lesson 3: Engineer the system prompt for prompt cache stability

**What**: Codex CLI keeps system instructions, tool definitions, sandbox config, and environment context stable and consistently ordered across requests. Rearranging or dynamically building the tools list mid-session breaks the KV cache prefix.

**Why it matters**: open-mpm currently rebuilds system prompts on each agent invocation. Anthropic's cache control headers (`cache_control: {"type": "ephemeral"}`) require stable prefixes to be effective. Instability in prompt ordering means paying full token costs on every turn even for identical context.

**How to implement**: Canonicalize the system prompt assembly order: (1) static base prompt from TOML, (2) injected skills (sorted deterministically), (3) dynamic context (task description, working directory). Mark the boundary between static and dynamic segments with Anthropic cache breakpoints. Never reorder or re-inject skills mid-session.

---

### Lesson 4: Add tiered approval modes (suggest / auto-edit / full-auto)

**What**: Codex CLI's orthogonal sandbox + approval layer (suggest, auto-edit, full-auto) lets users tune autonomy without modifying agent behavior. Roo Code requires approval for each subtask creation and completion by default.

**Why it matters**: open-mpm has no approval gating. In production use against real codebases, unsupervised tool execution (file writes, shell commands) is risky. A structured approval mode hierarchy also makes it easier to reason about trust boundaries.

**How to implement**: Add an `--approval-mode` CLI flag with values `suggest` (every tool call prompts), `auto-edit` (file reads/writes auto-approved, shell commands prompt), `full-auto` (no prompts). Implement a `ApprovalPolicy` trait that the tool executor consults before executing each tool. The PM loop reads stdin for user approval in interactive mode.

---

### Lesson 5: Hierarchical AGENTS.md-style instruction injection

**What**: Codex CLI walks from the Git root to the current working directory, collecting `AGENTS.md` files and injecting them as developer-role messages in root-to-leaf order before the user prompt. More specific directories override broader ones.

**Why it matters**: open-mpm currently reads a single CLAUDE.md at the project root. For multi-repo or mono-repo tasks, sub-directory-specific conventions (testing patterns, API contracts, style guides) cannot be injected contextually.

**How to implement**: At session start, walk from project root to the task's working directory. Collect any `AGENTS.md` (or `CLAUDE.md`) files found along the path. Inject them as ordered segments in the PM system prompt, root first. This is an extension of the existing layered system prompt builder.

---

### Lesson 6: Shadow checkpoint system for rollback without polluting user Git

**What**: Cline maintains a shadow Git repository in VS Code global storage. Every tool call that modifies files creates a commit in the shadow repo. This enables step-level and task-level diffs and rollback without touching the user's Git history.

**Why it matters**: open-mpm sub-agents can write files or run shell commands. There is currently no way to undo changes made during a failed sub-agent task without manual inspection. For a CLI tool, this is a significant reliability gap.

**How to implement**: Before a sub-agent begins tool execution, snapshot the relevant files (or `git stash` in a separate worktree). On sub-agent failure or user-requested rollback, restore the snapshot. A full shadow Git repo is the most robust approach; a simpler first step is to stash changes before each destructive tool call and expose a `rollback` IPC command.

---

### Lesson 7: Read-only exploration sub-agents for large codebase research

**What**: Cline spawns dedicated read-only sub-agents for codebase exploration. These agents can only read; they report findings to the main agent, which alone performs modifications. Roo Code's Orchestrator avoids file reads entirely, delegating that to Code or Architect sub-agents.

**Why it matters**: open-mpm's python-engineer agent is currently general-purpose. For tasks that require significant codebase understanding before writing code (e.g., "add a feature that integrates with the existing auth module"), mixing research and modification in one agent context leads to context bloat and harder-to-audit changes.

**How to implement**: Add a `researcher` agent TOML config with a system prompt that constrains it to read-only tools (no file writes, no shell exec). The PM delegates research subtasks to `researcher` and code-writing subtasks to `python-engineer`. This is a configuration change plus a tool allowlist enforcement change, both of which open-mpm's architecture already supports.

---

### Lesson 8: Enforce tool-call discipline — reject plain-text model responses

**What**: Cline's agent loop sends a structured error back to the model if it returns plain text instead of a tool call: `"[ERROR] You did not use a tool in your previous response! Please retry with a tool use."` This is injected as a user message in the next turn.

**Why it matters**: open-mpm's `chat_with_tools` loop handles the case where the model returns a stop reason of `end_turn` (i.e., a text response with no tool use). In agentic multi-turn scenarios, plain-text responses from the LLM mid-task usually indicate the model has gotten confused about its role. Injecting a structured error message is more reliable than silently treating the text as a final answer.

**How to implement**: In `chat_with_tools`, when a non-final turn produces `stop_reason = end_turn` with no tool calls, inject a `ToolResult::Error` message into the next turn's context with a prompt forcing a tool call. Track the number of consecutive plain-text responses and surface a hard error to the user after a configurable threshold (e.g., 3 retries).

---

## 6. Sources

- [Roo Code: Boomerang Tasks Documentation](https://docs.roocode.com/features/boomerang-tasks)
- [Roo Code: Using Modes](https://docs.roocode.com/basic-usage/using-modes)
- [Roo Code: Intelligent Context Condensing](https://docs.roocode.com/features/intelligent-context-condensing)
- [Roo Code: Customizing Modes](https://docs.roocode.com/features/custom-modes)
- [Roo Code: Cloud Agents](https://docs.roocode.com/roo-code-cloud/cloud-agents)
- [RooCodeInc/Roo-Code: Context and Message Management (DeepWiki)](https://deepwiki.com/RooCodeInc/Roo-Code/7-context-and-message-management)
- [RooCodeInc/Roo-Code: Context Window Management (DeepWiki)](https://deepwiki.com/RooCodeInc/Roo-Code/7.3-context-window-management)
- [Multi Agent Workflow With Roo Code (Xebia)](https://xebia.com/blog/multi-agent-workflow-with-roo-code/)
- [Boomerang Tasks — New AI-Powered Development (Medium)](https://mychen76.medium.com/boomerang-tasks-make-ai-agent-powered-development-fun-again-522bf8962dc4)
- [SPARC + Boomerang Orchestration (GitHub Gist)](https://gist.github.com/nickcent/49281afd13513a004ed32dcd631cb276)
- [Roo Code v3.17.0 Release Notes](https://docs.roocode.com/update-notes/v3.17.0)
- [Roo Code v3.36.0 Release Notes](https://docs.roocode.com/update-notes/v3.36.0)
- [Cline GitHub Repository](https://github.com/cline/cline)
- [Cline: Checkpoints Documentation](https://docs.cline.bot/features/checkpoints)
- [Inside Cline: How Its Agentic Chat System Really Works (Medium)](https://medium.com/@floralan212/inside-cline-how-its-agentic-chat-system-really-works-3d582935efa5)
- [Dissecting Cline — Context Management (Medium)](https://medium.com/@balajibal/dissecting-cline-cline-context-management-260aec3d84cb)
- [Cline Task Lifecycle (DeepWiki)](https://deepwiki.com/cline/cline/3.1-system-prompt)
- [Cline's Backroom Git: Shadow Git Explained (IT's FOSS)](https://itsfoss.gitlab.io/post/cline-s-backroom-git-the-secret-history-of-view-changes/)
- [Cline Context Enhancements Discussion (GitHub)](https://github.com/cline/cline/discussions/1887)
- [Why I Use Cline — Addy Osmani](https://addyo.substack.com/p/why-i-use-cline-for-ai-engineering)
- [OpenAI: Unrolling the Codex Agent Loop](https://openai.com/index/unrolling-the-codex-agent-loop/)
- [OpenAI Codex CLI: Features](https://developers.openai.com/codex/cli/features)
- [OpenAI Codex CLI: Sandbox](https://developers.openai.com/codex/concepts/sandboxing)
- [OpenAI Codex CLI: Agent Approvals & Security](https://developers.openai.com/codex/agent-approvals-security)
- [OpenAI Codex CLI: Custom Instructions with AGENTS.md](https://developers.openai.com/codex/guides/agents-md)
- [OpenAI Codex CLI: Configuration Reference](https://developers.openai.com/codex/config-reference)
- [Building Production-Ready AI Agents: Codex CLI Architecture (ZenML)](https://www.zenml.io/llmops-database/building-production-ready-ai-agents-openai-codex-cli-architecture-and-agent-loop-design)
- [OpenAI Codex CLI Architecture — adwaitx.com](https://www.adwaitx.com/openai-codex-agent-loop-architecture/)
- [Context Management Strategies for OpenAI Codex (Lakehouse Blog)](https://iceberglakehouse.com/posts/2026-03-context-openai-codex/)
- [OpenAI: Prompt Caching 201](https://developers.openai.com/cookbook/examples/prompt_caching_201)
