# Kilo.ai Analysis: What open-mpm Can Learn

**Date**: 2026-05-01
**Source**: https://kilo.ai/, https://github.com/Kilo-Org/kilocode, web research

---

## 1. What Kilo.ai Is

**Product category**: Open-source AI coding agent (VS Code extension + JetBrains plugin + CLI).
**Core value prop**: A model-agnostic, transparent coding agent. Pay providers directly (BYOK / pay-as-you-go), no markup, no subscription. Apache-2.0 licensed.
**Target user**: Individual developers and engineering teams who want full visibility into prompts, model choice, and costs — vs. opinionated closed tools like Cursor or GitHub Copilot.
**Scale**: 2.3M+ active users, 500+ models supported, #1 on OpenRouter by token volume.
**Founders**: Scott Breitenother + Sid Sijbrandij (ex-GitLab CEO). $8M seed from General Catalyst.

---

## 2. Technical Approach

### Agent Mode Architecture

Kilo separates concerns into distinct specialized modes rather than one monolithic agent:

| Mode | Role |
|------|------|
| Ask | Read-only Q&A; no file edits |
| Architect (Plan) | Planning and system design only |
| Code | Full tool access; implements |
| Debug | Diagnosis and targeted fixes |
| Orchestrator | Deprecated — native subagent support replaced it |

Each mode has a different **tool permission set** (read-only vs. restricted edit vs. full). This is the key insight: mode = (system prompt + tool access level), not a separate process.

### Native Subagent Delegation

The Orchestrator mode was removed. Instead, any full-access agent (Code, Plan, Debug) can now delegate using a built-in `task` tool:

- Parent agent calls `task(agent="general"|"explore", instructions="...")`
- Subagent runs in an **isolated context** — separate conversation history, no shared state
- Subagent returns a summary to the parent, which continues
- Agents can launch **multiple subagents concurrently** (parallel execution via CLI)
- Git worktree isolation prevents parallel agents from conflicting on disk

### Context / Memory Management

Three-layer approach:
1. **Automatic relevance scan**: Pulls only files and error traces relevant to the task (no full-codebase dump)
2. **Context mentions**: Developer-controlled pointers to specific files/functions
3. **Memory Bank** (structured markdown files):
   - `context.md` — architecture and tech stack
   - `brief.md` — active goals
   - `history.md` — past decisions and constraints

   Kilo reads and synthesizes this at the start of every task. No vector DB required for baseline memory.

### Model Routing

Fully model-agnostic. At task time, users (or the agent) select model from 500+ options across Anthropic, OpenAI, Google, Mistral, Llama, self-hosted. No routing logic is baked in — it is user-driven BYOK with direct API calls. The gateway layer (their managed service) adds no markup.

### Implementation

- **Language**: TypeScript (91.7% of codebase), Kotlin (JetBrains plugin)
- **Pattern**: ReAct loop (reason → act → observe → repeat until tests pass)
- **Tool protocol**: MCP (Model Context Protocol) for external integrations (Figma, Git, Slack, etc.) + MCP marketplace for community servers
- **Parallelism**: Git worktree isolation per parallel agent session

---

## 3. Differentiating Features

- **Transparent prompts**: Every prompt sent to the model is visible. No hidden compression or silent model switching.
- **No Orchestrator mode**: Removed in favor of native subagent spawning from any full-access agent — simpler mental model.
- **Memory Bank pattern**: Persistent, human-readable markdown state files synthesized at task start. Cheap, inspectable, no infra.
- **Mode = tool permissions + prompt**: The mode system doubles as an access control layer, not just a UX affordance.
- **KiloClaw**: A managed OpenClaw agent deployable to Telegram/Discord/Slack — cloud-hosted, continuously running variant.

---

## 4. Applicable Ideas for open-mpm

### 4.1 Mode-Based Tool Permission Tiers

**What Kilo does**: Agents are distinguished not just by system prompt but by which tools they can call (read-only, restricted-edit, full).

**How open-mpm could adopt this**: Add a `[permissions]` section to agent TOML configs:
```toml
[permissions]
tool_access = "read-only"  # or "restricted", "full"
allowed_tools = ["read_file", "search_code"]
```
The PM dispatcher checks this before routing tool calls. This prevents a planning agent from accidentally writing files.

### 4.2 Memory Bank Pattern

**What Kilo does**: Three markdown files (`context.md`, `brief.md`, `history.md`) injected into every task.

**How open-mpm could adopt this**: open-mpm already has `.open-mpm/state/` and `project-index.md`. Formalize this as structured memory: `state/context.md` (tech stack), `state/brief.md` (current sprint goals), `state/history.md` (past decisions). Inject all three as a skills block in every agent prompt. Zero infra cost, human-editable, version-controlled.

### 4.3 Native Subagent Delegation via `task` Tool

**What Kilo does**: Removes the dedicated Orchestrator in favor of a `task` tool any full-access agent can call. Subagents run with isolated context; results returned as a summary.

**How open-mpm could adopt this**: open-mpm's PM already delegates via `delegate_to_agent`. The applicable idea is **context isolation per delegation**: when spawning a sub-agent, do not pass the full PM conversation history — pass only the task description plus relevant memory bank excerpts. This prevents context bloat in sub-agents and mirrors Kilo's isolation model.

### 4.4 Parallel Agents with Worktree Isolation

**What Kilo does**: Multiple agents work concurrently in separate git worktrees; results merged after.

**How open-mpm could adopt this**: open-mpm already has worktree support in `.open-mpm/state/worktrees/`. The gap is the PM dispatching multiple `delegate_to_agent` calls concurrently (tokio::join!) and then synthesizing results. Would be valuable for the bake-off challenge pattern where independent subtasks exist.

### 4.5 Relevant-Files-First Context (Not Full Codebase)

**What Kilo does**: Before each task, scans the project to identify which files are relevant; injects only those.

**How open-mpm could adopt this**: open-mpm has a `project-index.md`. Extend it with a lightweight relevance filter: before dispatching to a sub-agent, grep the task description for symbols/filenames, include only matched files in the task context. This is the manual equivalent of Kilo's automatic context scan and reduces sub-agent token spend.

### 4.6 Pay-Direct / BYOK Model Transparency

**What Kilo does**: No markup; users see exact API costs. This is partly a product decision but the architecture (direct API key pass-through) is the enabler.

**How open-mpm could adopt this**: open-mpm already has a credential priority chain (claude-code → anthropic-direct → openrouter). Logging the model name and approximate token count per dispatch to a structured log (`.open-mpm/state/usage.jsonl`) would give users the same cost transparency Kilo provides.

---

## 5. Links

- Homepage: https://kilo.ai/
- GitHub: https://github.com/Kilo-Org/kilocode
- Docs (agents): https://kilo.ai/docs/code-with-ai/agents/using-agents
- Orchestrator mode (deprecated): https://kilo.ai/docs/code-with-ai/agents/orchestrator-mode
- Technical deep-dive (tessl.io): https://tessl.io/blog/inside-kilo-code-an-open-source-ai-coding-agent-with-plans-to-reshape-software-development/
- Product Hunt: https://www.producthunt.com/products/kilocode
- Fortune profile: https://fortune.com/2026/03/17/how-cutting-out-product-management-enabled-kilo-to-compete-in-the-hyper-fast-ai-coding-market/
- Kilo vs Cline vs Roo Code comparison: https://adam.holter.com/kilo-code-the-hybrid-ai-coding-assistant-that-combines-cline-and-roo-code-for-cost-effective-development/
