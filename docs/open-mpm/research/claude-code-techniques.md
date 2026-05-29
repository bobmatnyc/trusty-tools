# Claude Code Techniques: Research for open-mpm

**Date**: 2026-04-22
**Issue**: #66 (bobmatnyc/open-mpm)
**Researcher**: Research Agent (claude-sonnet-4-6)

## Overview

This document synthesizes publicly available information about Claude Code's internal architecture gathered from the April 2026 npm source map leak, Anthropic's official documentation, and academic analysis. The goal is to identify patterns directly applicable to open-mpm's Rust agent orchestration harness.

---

## 1. What We Found About Claude Code's Internal Architecture

### 1.1 The Source Code Leak (April 2026)

Anthropic accidentally included source maps in Claude Code npm package version 2.1.88. A missing `.npmignore` entry and a Bun build toolchain bug (Bun issue #28001, which generates source maps in production by default) exposed the full 512,000-line TypeScript codebase across ~1,900 files. The source was archived to GitHub within hours of discovery.

Key components exposed:
- **`main.tsx`** — 785KB entry point with a custom React/Ink terminal renderer
- **`QueryEngine.ts`** — ~46,000 lines handling all LLM API calls, streaming, caching, and orchestration
- **`Tool.ts`** — Base tool definitions (~29,000 lines with schema validation, permission enforcement, error handling)
- **`tools/`** — 40+ discrete tools, each permission-gated
- **`coordinator/`** — Multi-agent orchestration ("swarm" architecture)

Sources:
- [Anthropic Accidentally Exposes Claude Code Source via npm Source Map File — InfoQ](https://www.infoq.com/news/2026/04/claude-code-source-leak/)
- [Claude Code's source reveals extent of system access — The Register](https://www.theregister.com/2026/04/01/claude_code_source_leak_privacy_nightmare/)
- [Claude Code architecture Deep Dive — WaveSpeedAI](https://wavespeed.ai/blog/posts/claude-code-architecture-leaked-source-deep-dive/)

### 1.2 The Agentic Loop (`queryLoop()`)

Claude Code's core loop is a simple reactive loop — **not** an explicit planning graph or state machine. From the academic analysis:

> "The `queryLoop()` function operates as an async generator that: (1) assembles context from prior turns, (2) calls the model with available tools, (3) routes tool requests through permission checks, (4) executes approved actions, and (5) feeds results back into the next iteration."

The architecture follows the ReAct pattern (reasoning + acting). Critical insight: **the core loop comprises roughly 1.6% of the codebase**; 98.4% is operational infrastructure — safety, context management, extensibility, persistence.

The loop terminates on `stop_reason == "end_turn"`. Text content alone does not indicate completion — Claude can return text alongside tool calls. The correct termination signal is always `stop_reason`.

Source: [Dive into Claude Code: The Design Space of Today's and Future AI Agent Systems — arXiv](https://arxiv.org/html/2604.14228v1)

### 1.3 System Prompt Assembly

System prompts are **dynamically assembled**, not static strings. Analysis of the leaked code shows:
- ~25+ conditional sections that activate based on user type, configuration, and available tools
- ~50 discrete tool definitions with multiple conditional variations
- Sections categorized as "always included" (intro, system rules, care in execution) vs. "conditional" (agent tools, skills, session features, user type)
- CLAUDE.md/AGENTS.md content injected as "user content" layer, not as system prompt
- Lazy loading: base CLAUDE.md hierarchy loads at session start; nested-directory files load only when the agent accesses those directories

Source: [How Claude Code Builds a System Prompt — dbreunig.com](https://www.dbreunig.com/2026/04/04/how-claude-code-builds-a-system-prompt.html)

### 1.4 Context Compression Pipeline

Rather than single-pass truncation, Claude Code implements a **five-layer progressive compaction pipeline** (cheapest first):

1. **Budget reduction** — Per-message size limits on tool results
2. **Snip** — Lightweight temporal trimming of older history segments
3. **MicroCompact** — Fine-grained compression; edits cached content locally with zero API calls (old tool outputs trimmed directly)
4. **Context collapse** — Read-time projection over history (doesn't mutate stored transcript)
5. **AutoCompact** — Full model-generated semantic summarization; fires when conversation approaches context window ceiling, reserves a 13,000-token buffer, generates up to a 20,000-token structured summary. Circuit breaker: stops retrying after three consecutive compression failures.

Source: [Claude Code Source Leak: Everything Found (2026) — claudefa.st](https://claudefa.st/blog/guide/mechanics/claude-code-source-leak)

### 1.5 Session Persistence

Sessions use **append-only JSONL transcript storage** at project-specific paths (`~/.claude/projects/`). Resume and fork operations reconstruct state from transcripts. Session-scoped permissions are **not restored on resume or fork** — a deliberate security choice.

### 1.6 Safety Architecture

Seven independent safety layers (deny-first evaluation):
1. Pre-filtering: blanket-denied tools removed from model's view before any call
2. Rule evaluation: deny rules take absolute precedence over allow rules
3. Permission modes: five external modes (plan, default, acceptEdits, dontAsk, bypassPermissions)
4. ML classifier: evaluates tool safety against conversation context when enabled
5. Shell sandboxing: filesystem/network isolation independent of application-level permissions
6. Hook interception: PreToolUse hooks can modify or reject requests
7. Not restoring permissions on resume

---

## 2. AGENTS.md / CLAUDE.md Specification — Full Details

### 2.1 File Hierarchy (Four Levels)

Claude Code reads from four locations, merged in order (most specific wins):

| Location | Scope | Sharing |
|---|---|---|
| `~/.claude/CLAUDE.md` | All projects, personal | Not shared |
| `./CLAUDE.md` | Project root | Commit to git |
| `./CLAUDE.local.md` | Project root, personal | Add to `.gitignore` |
| `./subdir/CLAUDE.md` | Subdirectory only | Loaded lazily on file access |

Additionally, a global managed settings layer (enterprise policy) loads before personal preferences.

### 2.2 AGENTS.md Relationship

Claude Code reads `AGENTS.md` as well — it is not a strict fallback; both files are respected. The emerging community convention is to maintain one canonical `AGENTS.md` for tool-agnostic shared context and tool-specific files (CLAUDE.md, .cursorrules) that reference it. Some sources describe `AGENTS.md` as the future standard analogous to README.md.

### 2.3 Import Syntax

CLAUDE.md files can import other files:

```markdown
See @README.md for project overview and @package.json for available npm commands.

# Additional Instructions
- Git workflow: @docs/git-instructions.md
- Personal overrides: @~/.claude/my-project-instructions.md
```

This is a feature we do not currently implement in open-mpm's `SystemPromptBuilder`.

### 2.4 Lazy Loading (Critical for Context Budget)

Subdirectory CLAUDE.md files load **on demand** when the agent reads files in those directories — they are not loaded at session start. This prevents unused instructions from consuming context. Only the base hierarchy is loaded eagerly.

### 2.5 Auto Memory

Beyond CLAUDE.md, Claude Code has an **auto-memory** system: the model writes persistent notes about the project during conversations. The first 200 lines or 25KB of `MEMORY.md` (whichever comes first) load at the start of each session. This is separate from CLAUDE.md.

### 2.6 Best Practices for CLAUDE.md Content

From Anthropic's official documentation:

Include:
- Bash commands Claude cannot guess (build, test, lint commands)
- Code style rules that differ from defaults
- Testing instructions and preferred test runners
- Repository etiquette (branch naming, PR conventions)
- Architectural decisions specific to the project
- Developer environment quirks (required env vars)
- Common gotchas or non-obvious behaviors

Exclude:
- Anything Claude can figure out by reading code
- Standard language conventions Claude already knows
- Detailed API documentation (link to docs instead)
- Information that changes frequently
- Long explanations or tutorials
- File-by-file descriptions of the codebase

Emphasis markers ("IMPORTANT", "YOU MUST") improve adherence for critical rules.

Source: [Best Practices for Claude Code — code.claude.com](https://code.claude.com/docs/en/best-practices)

### 2.7 What open-mpm May Be Missing

Our current CLAUDE.md/AGENTS.md hierarchy walk likely handles the four-level lookup, but we may be missing:
- **`@import` syntax** for file references within CLAUDE.md
- **Lazy loading** of subdirectory files (we may be loading all eagerly)
- **CLAUDE.local.md** personal override files
- **Auto-memory / MEMORY.md** as a separate memory layer
- **Compact instructions** section in CLAUDE.md for controlling summarization behavior

---

## 3. Tool Definition Patterns Worth Adopting

### 3.1 JSON Schema Format (Official Anthropic API)

Each tool definition has three required fields:

```json
{
  "name": "tool_name",
  "description": "Detailed plaintext description explaining: what the tool does, when it should be used (and when NOT), what each parameter means, important caveats. Aim for 3-4 sentences minimum.",
  "input_schema": {
    "type": "object",
    "properties": {
      "param_name": {
        "type": "string",
        "description": "What this parameter means and how it affects behavior"
      }
    },
    "required": ["param_name"]
  }
}
```

Optional properties on tool definitions:
- `cache_control` — Mark for prompt caching
- `strict` — Schema validation (enforce exact match)
- `defer_loading` — Load tool definition lazily (only name shown in context until needed)
- `allowed_callers` — Restrict which agents can call this tool
- `input_examples` — Array of example inputs for complex tools

Source: [Define tools — platform.claude.com](https://platform.claude.com/docs/en/agents-and-tools/tool-use/implement-tool-use)

### 3.2 Consolidation Principle

Rather than many narrow tools, consolidate related operations:

```json
// Avoid: separate tools for each action
{ "name": "create_pr" }
{ "name": "review_pr" }
{ "name": "merge_pr" }

// Prefer: one tool with an action parameter
{
  "name": "github_pr",
  "description": "Manage pull requests: create, review, or merge.",
  "input_schema": {
    "properties": {
      "action": { "type": "string", "enum": ["create", "review", "merge"] },
      "pr_number": { "type": "integer" }
    }
  }
}
```

Fewer tools reduce selection ambiguity. This is especially important when using tool search (deferred tool loading).

### 3.3 Meaningful Namespacing

Prefix tool names with service/category when tools span multiple domains:
- `github_list_prs`, `github_create_branch`
- `fs_read_file`, `fs_write_file`
- `shell_exec`, `shell_check`

This matches open-mpm's current naming (e.g., `fs_reader`, `shell_exec`) which is already well-aligned.

### 3.4 Return Only High-Signal Information

Tool responses should return semantic, stable identifiers and only the fields Claude needs for its next reasoning step. Bloated tool responses waste context. Prefer UUIDs/slugs over internal opaque references.

### 3.5 AgentTool Schema (From Leaked Source)

The `AgentTool` schema (reconstructed from the leak) uses a lazy schema pattern to break circular dependencies:

```typescript
// Base input schema fields (reconstructed from source analysis)
{
  description: "A short 3-5 word task description",
  prompt: "The full task for the agent to perform",
  subagent_type: "(optional) specialized agent type",
  model: "(optional) enum: sonnet | opus | haiku",
  run_in_background: "(optional) boolean"
  // When isolation features enabled:
  // isolation: "worktree" | "remote" | "in-process"
  // cwd: "(optional) working directory"
}
```

The `lazySchema()` pattern defers schema construction to break circular dependencies between tools that can spawn each other — relevant for open-mpm's `delegate_to_agent` tool definition.

Source: [Agent Tool and Sub-Agent Spawning — zread.ai analysis](https://zread.ai/instructkr/claude-code/8-agent-tool-and-sub-agent-spawning)

### 3.6 Tool Description Quality: Good vs. Poor

Good description example from official docs:
> "Retrieves the current stock price for a given ticker symbol. The ticker symbol must be a valid symbol for a publicly traded company on a major US stock exchange like NYSE or NASDAQ. The tool will return the latest trade price in USD. It should be used when the user asks about the current or most recent price of a specific stock. It will not provide any other information about the stock or company."

Poor description:
> "Gets the stock price for a ticker."

Our `finish_task` and `delegate_to_agent` tool descriptions should be audited against this standard.

---

## 4. Agentic Loop Improvements for open-mpm's `chat_with_tools`

### 4.1 Correct Termination Signal

The current loop likely checks for absence of tool calls. The correct pattern is:

```rust
// Terminate when stop_reason == "end_turn"
// DO NOT terminate based on: text content, no tool calls, etc.
// Claude can return text + tool calls in the same response

loop {
    let response = llm_call(&messages, &tools).await?;
    messages.push_assistant(&response);
    
    match response.stop_reason.as_str() {
        "end_turn" => break,
        "tool_use" => {
            // Execute tool calls, push results, continue
            let results = execute_tool_calls(&response.tool_calls).await?;
            messages.push_tool_results(results);
        }
        "max_tokens" => {
            // Handle context limit — trigger compaction
            compact_context(&mut messages).await?;
        }
        _ => break, // "stop_sequence", etc.
    }
}
```

### 4.2 `tool_choice` Selection

| Mode | When to Use |
|---|---|
| `auto` (default) | Normal operation — Claude decides whether to call a tool |
| `any` | When Claude must take an action (e.g., PM must delegate) — forces tool use |
| `{"type": "tool", "name": "..."}` | When a specific tool must be called |
| `none` | When you want text-only response |

For the PM orchestrator's delegation step, `tool_choice: any` is appropriate — the PM should always either delegate or use `finish_task`, never just return text. This is what Anthropic calls the "forced tool use" pattern.

Combine `any` with `strict: true` on tool definitions to guarantee both that a tool is called AND that inputs exactly match the schema.

Note: `tool_choice: any` and `tool_choice: {"type": "tool"}` are **not compatible** with extended thinking mode. Use `auto` if extended thinking is enabled.

### 4.3 Iteration Limit as Safety Guard, Not Control Logic

An iteration cap (e.g., 20 turns) is a safety guard — not the primary termination mechanism. The real termination signal is always `stop_reason == "end_turn"`. Do not conflate the two.

### 4.4 `finish_task` as Terminal Tool

The `finish_task` tool pattern allows the LLM to signal task completion in a structured way, carrying the final result as a parameter. This is more reliable than detecting end-of-task from natural language.

Recommended schema for open-mpm:

```json
{
  "name": "finish_task",
  "description": "Call this tool when you have completed the assigned task and are ready to return a final result to the PM orchestrator. Do not call other tools after this. The 'result' field should contain your complete, final response.",
  "input_schema": {
    "type": "object",
    "properties": {
      "result": {
        "type": "string",
        "description": "The complete final result of the task, including all relevant output, findings, or generated content."
      },
      "status": {
        "type": "string",
        "enum": ["success", "partial", "failed"],
        "description": "Whether the task was completed successfully, only partially, or failed."
      }
    },
    "required": ["result", "status"]
  }
}
```

When `finish_task` is called, extract the parameters and exit the loop — do not continue calling the LLM.

### 4.5 Context Budget Management

For sub-agents with limited turns, implement a budget-aware loop:

```rust
const MAX_TURNS: usize = 20;
const CONTEXT_WARNING_THRESHOLD: f32 = 0.8; // 80% of context window

for turn in 0..MAX_TURNS {
    let usage = response.usage;
    let context_fill = (usage.input_tokens as f32) / (max_context as f32);
    
    if context_fill > CONTEXT_WARNING_THRESHOLD {
        // Inject a compaction hint or truncate old tool results
        compact_tool_results(&mut messages);
    }
    
    // ... rest of loop
}
```

### 4.6 Message History Management

Always append the full assistant response to message history before the next iteration. Missing this causes hallucination and infinite loops. The standard pattern is:

```
messages: [
    {role: "user", content: initial_task},
    {role: "assistant", content: [text_block, tool_use_block]},  // full response
    {role: "user", content: [tool_result_block]},
    {role: "assistant", content: [tool_use_block]},
    ...
]
```

Role alternation (user/assistant) must be maintained. Tool results are sent as `role: "user"` messages with `type: "tool_result"` content.

---

## 5. Multi-Agent Subagent Spawning: What Claude Code Does

### 5.1 Three Spawn Paths

Claude Code's `AgentTool` supports three distinct delegation strategies:

| Path | Behavior | Use Case |
|---|---|---|
| **Fork** | Shares parent's prompt cache, independent execution | Performance optimization; same model required |
| **Teammate** | Collaborative via tmux or in-process | Persistent parallel workers |
| **Worktree** | Git worktree-based filesystem isolation | Parallel edits that must not collide |

The fork path is a performance optimization — it exploits prompt cache sharing between parent and child. A different model breaks cache compatibility, so `model` is ignored on the fork path.

### 5.2 Context Isolation: What Subagents See

Each subagent runs in its own context window with:
- Its own system prompt (role-specific)
- Tool access limited to its allowed tools
- Independent permissions (not inherited from parent)
- CLAUDE.md injected as background context

Subagents do **not** see:
- Parent's full conversation history
- Other subagents' conversations
- Intermediate tool calls from other agents

### 5.3 What Subagents Return

Subagents return **only text summaries** to the parent. The full conversation (sidechain transcript) is stored separately. This prevents inflation of parent context. This is the key design choice for keeping the PM's context manageable.

This is exactly what open-mpm's NDJSON IPC pattern implements: the sub-agent sends `{"type": "result", "content": "..."}` back to the PM. We are aligned with this design.

### 5.4 Permission Escalation (Bubble Mode)

When a subagent needs permissions it doesn't have, it can escalate to the parent terminal for approval. This is analogous to open-mpm's interrupt/confirmation mechanism.

### 5.5 When NOT to Use Subagents

From official Claude Code docs:

> "Do not spawn a subagent just because the task sounds important. Importance is not the trigger. Context isolation is the trigger."

Use a subagent when:
- The task will flood the parent context with file reads, search results, or logs
- The task needs specialized tool permissions the main agent should not have
- The task is independent and its intermediate work should not appear in main context

Do not use a subagent when the task is small enough to do inline.

---

## 6. Prompt Caching Patterns for Long-Running Agents

### 6.1 Cache Control Placement

Cache control is attached at the block level. The canonical placement for agentic use:

```json
{
  "tools": [
    { "name": "tool_1", ... },
    { "name": "tool_2", ..., "cache_control": { "type": "ephemeral" } }  // last tool
  ],
  "system": [
    { "type": "text", "text": "Static system prompt", "cache_control": { "type": "ephemeral" } }
  ],
  "messages": [...]
}
```

Cache prefix order: **tools → system → messages**. Each `cache_control` marker creates one cache entry covering everything up to that point.

### 6.2 TTL Options

- **5-minute TTL** (default `"type": "ephemeral"`) — Refreshed at no additional cost on each use
- **1-hour TTL** (`"type": "ephemeral", "ttl": "1h"`) — 2× base input token price; use when agents pause longer than 5 minutes between turns

For open-mpm's sub-agent spawning model (sub-agents exit after each task), the 5-minute TTL is sufficient for within-session calls. For the PM orchestrator across user interactions, consider 1-hour TTL on the system prompt.

### 6.3 What Invalidates the Cache

| Change | Invalidates |
|---|---|
| Tool definitions modified | All cached content |
| `tool_choice` parameter changes | Tools + system + messages |
| Images added/removed | Tools + system + messages |
| System prompt content changes | System + messages |
| Any character difference in static prefix | Entire cache miss |

Key: timestamps, user IDs, and other dynamic data must appear **after** the last `cache_control` marker, not before.

### 6.4 async-openai Caching in Rust

The `async-openai` crate supports `cache_control` via the `CreateMessageRequestContent` types. For open-mpm, apply caching on:
1. Tool definitions (mark the last tool)
2. System prompt content (mark the end of the static portion)

Cache reads cost 0.1× the base input token price — substantial savings for agents with long system prompts and stable tool definitions.

### 6.5 Cache Minimum Token Thresholds

Cache only activates when the prefix meets minimum tokens:
- Claude Sonnet 4.6: 2,048 tokens minimum
- Claude Opus 4.6: 4,096 tokens minimum
- Claude Haiku 3.5: 2,048 tokens minimum

Check `cache_creation_input_tokens` in usage response to verify caching is active.

---

## 7. Specific Rust Implementation Recommendations for open-mpm

### 7.1 Correct Loop Termination

Current `chat_with_tools` likely uses presence/absence of tool calls for termination. Migrate to `stop_reason` checking:

```rust
// In src/llm/ or equivalent
match response.choices[0].finish_reason {
    FinishReason::ToolCalls => { /* execute tools, continue */ }
    FinishReason::Stop => break,         // "end_turn" equivalent
    FinishReason::Length => {            // "max_tokens" equivalent
        tracing::warn!("context limit hit, compacting");
        compact_old_tool_results(&mut messages);
        // do NOT break — continue the loop
    }
    _ => break,
}
```

### 7.2 Add `tool_choice: "any"` for PM's Delegation Step

The PM should always produce a tool call (either `delegate_to_agent` or `finish_task`). Use `tool_choice: "any"` for the PM's LLM call to enforce this:

```rust
// In PM's LLM call
let request = CreateChatCompletionRequestArgs::default()
    .tool_choice(ChatCompletionToolChoiceOption::Required)  // "any" in API terms
    .tools(pm_tools)
    .build()?;
```

This eliminates the case where the PM returns a text response instead of delegating.

### 7.3 Improve `finish_task` Tool Description

The current `finish_task` tool description (in `src/tools/finish_task.rs`) should explicitly state:
- When to call it (task complete, all work done)
- When NOT to call it (still have pending tool calls, work not verified)
- What the `result` field should contain (complete, standalone answer)

### 7.4 CLAUDE.md Import Syntax Support

Add support for `@path/to/file` import directives in CLAUDE.md parsing in `src/agents/prompt_builder.rs`. When the prompt builder encounters `@filepath`, read and inline that file's content.

```rust
fn resolve_imports(content: &str, base_dir: &Path) -> Result<String> {
    let import_re = Regex::new(r"@([\w./~-]+)").unwrap();
    let mut result = content.to_string();
    for cap in import_re.captures_iter(content) {
        let import_path = base_dir.join(&cap[1]);
        if let Ok(imported) = std::fs::read_to_string(&import_path) {
            result = result.replace(&cap[0], &imported);
        }
    }
    Ok(result)
}
```

### 7.5 Lazy Loading for Subdirectory CLAUDE.md Files

Currently we likely load all CLAUDE.md files eagerly at session start. Implement lazy loading: track which directories the agent has accessed, and load CLAUDE.md files from those directories on first access rather than upfront. This is important for monorepo scenarios.

### 7.6 Prompt Cache on Tool Definitions

Apply `cache_control` to tool definitions in async-openai requests. The `async-openai` crate has support for this via the `ChatCompletionTool` struct. Mark the last tool in the array:

```rust
let mut tools = build_tool_definitions();
if let Some(last_tool) = tools.last_mut() {
    last_tool.cache_control = Some(CacheControl { type_: CacheControlType::Ephemeral });
}
```

This is especially valuable for agents with many tools (we have ~10 in the current tool registry).

### 7.7 Subagent Context: Return Summary Only

Our current NDJSON IPC already follows this pattern (sub-agents send a `result` field). Reinforce this: sub-agents should never return their full internal dialogue — only the final result. If a sub-agent's output is large (e.g., generated code), consider adding a `summary` field for the PM and a `content` field for the actual output.

### 7.8 Five-Phase Pipeline Alignment

Our current research→plan→code→qa→observe pipeline maps well to Claude Code's subagent architecture:
- Each phase is a subagent with isolated context (correct)
- Phase outputs should be summaries passed to the next phase (verify this is enforced)
- The PM orchestrator should not accumulate all phase outputs in its context — only the summary from each

Consider adding explicit "handoff document" format for inter-phase context passing, analogous to Claude Code's Full Compact format (9-section structured summary).

### 7.9 Session Append-Only Transcript

Our existing `session.rs` should implement append-only JSONL storage (write each turn immediately). This enables:
- Resume from last turn on crash
- Fork sessions from any checkpoint
- Audit trail for debugging bake-off runs

### 7.10 Tool Response Size Limits

Implement per-tool output size limits (Claude Code uses "budget reduction" as the first compaction layer). Long shell command outputs and file reads should be truncated with a marker indicating truncation occurred, rather than passed in full.

---

## 8. Key Design Principles to Internalize

From the academic analysis of Claude Code:

1. **Simplicity over sophistication**: The core loop is 1.6% of the codebase. The value is in the infrastructure around it. Do not over-engineer the orchestration logic itself.

2. **Context as the binding constraint**: Every design decision should be evaluated against its context window impact. Lazy loading, subagent isolation, and compaction all serve this master constraint.

3. **Deny-first for permissions**: Default to restricting tool access; explicitly grant when needed. Our `delegate_to_agent` should only delegate to agents that are explicitly configured.

4. **Append-only state**: Session transcripts are append-only. This enables recovery, forking, and auditing without complex transactional semantics.

5. **Summary-only subagent returns**: Subagents return text summaries, not conversation histories. This is the single most important pattern for keeping the PM's context manageable.

6. **Deterministic infrastructure, not decision scaffolding**: Invest in reliable tool execution, context management, and session persistence rather than explicit planning graphs. The model handles reasoning; your job is to give it a reliable environment.

---

## 9. Unshipped Claude Code Features Worth Watching

These features from the leaked source are not yet public but indicate Anthropic's direction:

- **KAIROS**: Always-on background agent daemon with 15-second decision cycles, append-only audit logging, and autonomous GitHub webhook subscriptions. Indicates direction toward persistent agent processes rather than ephemeral per-task spawning.
- **ULTRAPLAN**: Delegates complex planning to a remote Opus session with up to 30-minute thinking window and browser-based approval UI. Relevant to open-mpm's plan phase.
- **Fork subagent path**: Prompt cache sharing between parent and child agents for performance. Could significantly reduce token costs for our 5-phase pipeline.
- **autoDream / Memory consolidation**: Three-gate trigger (24+ hours, 5+ sessions, lock) for consolidating auto-memory entries. Relevant to open-mpm's memory subsystem.

---

## 10. Summary of Actionable Recommendations

| Priority | Recommendation | File(s) to Modify |
|---|---|---|
| High | Fix loop termination to use `stop_reason` not tool-call presence | `src/llm/` or wherever chat_with_tools lives |
| High | Add `tool_choice: "any"` for PM delegation step | `src/main.rs` or PM agent loop |
| High | Apply `cache_control` to tool definitions and system prompt | `src/llm/`, `src/agents/prompt_builder.rs` |
| High | Improve `finish_task` tool description with explicit when/when-not guidance | `src/tools/finish_task.rs` |
| Medium | Add `@import` syntax support in CLAUDE.md parser | `src/agents/prompt_builder.rs` |
| Medium | Add `CLAUDE.local.md` support for personal overrides | `src/agents/prompt_builder.rs` |
| Medium | Implement tool output size limits (truncation with marker) | `src/tools/` |
| Medium | Enforce summary-only inter-phase handoff in 5-phase pipeline | `src/workflow/` |
| Low | Implement lazy loading for subdirectory CLAUDE.md files | `src/agents/prompt_builder.rs` |
| Low | Add 1-hour cache TTL option for PM system prompt | `src/llm/` |
| Low | Structured inter-phase handoff document format | `src/workflow/` |

---

## Sources

- [How Claude Code Builds a System Prompt — dbreunig.com](https://www.dbreunig.com/2026/04/04/how-claude-code-builds-a-system-prompt.html)
- [Claude Code Source Leak: Everything Found (2026) — claudefa.st](https://claudefa.st/blog/guide/mechanics/claude-code-source-leak)
- [Claude Code architecture Deep Dive — WaveSpeedAI](https://wavespeed.ai/blog/posts/claude-code-architecture-leaked-source-deep-dive/)
- [Anthropic Accidentally Exposes Claude Code Source via npm — InfoQ](https://www.infoq.com/news/2026/04/claude-code-source-leak/)
- [Claude Code's source reveals extent of system access — The Register](https://www.theregister.com/2026/04/01/claude_code_source_leak_privacy_nightmare/)
- [Claude Code's Entire Source Code Was Just Leaked — DEV Community](https://dev.to/gabrielanhaia/claude-codes-entire-source-code-was-just-leaked-via-npm-source-maps-heres-whats-inside-cjo)
- [Dive into Claude Code: The Design Space of Today's and Future AI Agent Systems — arXiv 2604.14228](https://arxiv.org/html/2604.14228v1)
- [Create custom subagents — code.claude.com](https://code.claude.com/docs/en/sub-agents)
- [Best Practices for Claude Code — code.claude.com](https://code.claude.com/docs/en/best-practices)
- [How Claude Code works — code.claude.com](https://code.claude.com/docs/en/how-claude-code-works)
- [Define tools — platform.claude.com](https://platform.claude.com/docs/en/agents-and-tools/tool-use/implement-tool-use)
- [Prompt caching — platform.claude.com](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)
- [What Actually Is an "Agentic Loop" in Claude? — cloudedventures.com](https://www.cloudedventures.com/articles/what-actually-is-an-agentic-loop-in-claude-the-pattern-every-ai-engineer-needs-to-know)
- [CLAUDE.md Configuration Hierarchy — agentfactory.panaversity.org](https://agentfactory.panaversity.org/docs/General-Agents-Foundations/claude-code-teams-cicd/claude-md-configuration-hierarchy)
- [How to Configure AI Coding Assistants: CLAUDE.md, AGENTS.md and More — deployhq.com](https://www.deployhq.com/blog/ai-coding-config-files-guide)
- [system_prompts_leaks repository — github.com/asgeirtj](https://github.com/asgeirtj/system_prompts_leaks)
