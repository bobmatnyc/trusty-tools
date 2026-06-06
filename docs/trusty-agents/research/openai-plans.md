# OpenAI "Plans" Feature Investigation — Research Report

**Date:** 2026-04-22  
**Scope:** ChatGPT/OpenAI structured planning APIs, extended thinking, plan output features; viability for use in open-mpm via OpenRouter

---

## Executive Summary

There is no OpenAI API endpoint named "plans" or a formal "plans API." However, OpenAI has introduced several related features in 2025-2026 that collectively address structured planning and extended thinking:

1. **GPT-5.x preambles** — visible planning messages before tool calls
2. **Reasoning models** (o-series) — extended internal thinking before output
3. **`reasoning_effort` parameter** — developer control over thinking depth
4. **Structured Outputs** — strict JSON Schema enforcement
5. **Responses API** — unified agentic endpoint replacing Assistants API
6. **Codex Plan Mode** — richer planning workflows in the coding agent

---

## 1. GPT-5.x Preambles ("Plans in Output")

GPT-5.4 introduced **preambles** — brief, user-visible explanations the model generates before invoking any tool or function, outlining its intent or plan.

- Appear after chain-of-thought (hidden reasoning) and before the actual tool call
- Provide transparency into the model's intent
- Example: When asked to build a website, GPT-5 shares a quick plan, then scaffolds the app, installs dependencies, creates site content, runs a build, then summarizes and suggests next steps
- Described as: "upfront plan of its thinking, so users can adjust course mid-response while it's working"

**API access:** These preamble messages appear as `assistant` role messages in the response. They are not a separate API feature — they are a behavior of the model when instructed to plan out loud.

**To use:** Instruct the model in its system prompt to output a plan before taking action. Example prompt:

```
Before using any tools, output a brief numbered plan of what you intend to do.
Format it as:
## Plan
1. [step 1]
2. [step 2]
...

Then execute the plan.
```

---

## 2. Reasoning Models (o-Series)

OpenAI's o-series models (o1, o3, o4-mini) are purpose-built as "planners":

- **Internal extended thinking** before producing output
- Visible as `reasoning_details` in the API response (when enabled)
- Excel at: strategic problem-solving, multi-step planning, complex code tasks, math/science

### API Access

Available via OpenRouter using model IDs like `openai/o4-mini`, `openai/o3`.

```json
{
  "model": "openai/o4-mini",
  "messages": [...],
  "reasoning": { "effort": "high" }
}
```

Response includes:
```json
{
  "choices": [{
    "message": {
      "role": "assistant",
      "content": "...",
      "reasoning_details": [
        { "type": "thinking", "thinking": "Let me analyze this step by step..." }
      ]
    }
  }]
}
```

### Reasoning Effort Levels

| Level | Use Case |
|-------|----------|
| `low` | Fast, lightweight responses |
| `medium` | Default balance |
| `high` | Complex, multi-step tasks |
| `xhigh` | Maximum quality (GPT-5.2 Pro and Thinking only) |

### Claude Sonnet 4.6 Reasoning

Claude Sonnet 4.6 also supports reasoning/extended thinking via OpenRouter:

```json
{
  "model": "anthropic/claude-sonnet-4.6",
  "thinking": { "type": "enabled", "budget_tokens": 10000 }
}
```

Response includes `thinking` blocks in the content array. This is Anthropic's parallel feature.

---

## 3. Structured Outputs (JSON Schema Enforcement)

OpenAI Structured Outputs forces model responses to conform to a developer-supplied JSON Schema. This is the most reliable way to get structured planning output:

```json
{
  "model": "anthropic/claude-sonnet-4.6",
  "messages": [
    { "role": "user", "content": "Plan the implementation of a JWT auth system" }
  ],
  "response_format": {
    "type": "json_schema",
    "json_schema": {
      "name": "implementation_plan",
      "strict": true,
      "schema": {
        "type": "object",
        "properties": {
          "summary": { "type": "string" },
          "steps": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "step_number": { "type": "integer" },
                "description": { "type": "string" },
                "files_affected": { "type": "array", "items": { "type": "string" } },
                "estimated_complexity": { "type": "string", "enum": ["low", "medium", "high"] }
              },
              "required": ["step_number", "description", "estimated_complexity"]
            }
          },
          "risks": { "type": "array", "items": { "type": "string" } },
          "dependencies": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["summary", "steps"]
      }
    }
  }
}
```

**Via OpenRouter:** OpenRouter supports `response_format: { "type": "json_object" }` for basic JSON mode. Strict JSON Schema enforcement (`strict: true`) availability depends on the specific model and provider.

For Claude models specifically, Anthropic's API supports structured outputs via tool definitions (use a single tool with the desired schema, force the model to call it).

---

## 4. OpenAI Responses API (Unified Agentic Endpoint)

Launched March 2025, replacing the Assistants API (deprecated Q1 2026).

Key features:
- Built-in tools: web search, code interpreter, file search, remote MCP servers
- Durable conversation state (Conversations API for multi-turn)
- Native text + image support
- Designed for multi-step agentic workflows

**Access via OpenRouter:** The Responses API is an OpenAI-specific endpoint (`/v1/responses`), not the standard `/v1/chat/completions`. OpenRouter routes to `/v1/chat/completions`. The Responses API features are not available through OpenRouter's OpenAI-compatible interface.

**Implication for open-mpm:** open-mpm should not depend on the Responses API. Stick to `/v1/chat/completions` with tool calling. This is actually advantageous — it keeps open-mpm provider-agnostic.

---

## 5. Codex Plan Mode

OpenAI's Codex coding agent introduced "Plan Mode" in 2025:

- The model outputs a structured plan before executing
- User can review and approve/modify the plan
- Execution proceeds after user confirms
- "Human-in-the-loop" planning gate

This is a UX pattern, not a specific API. Implemented via:
1. Two-phase prompt: first ask for plan only, then ask to execute
2. User review between phases
3. Second call with plan context + "proceed" instruction

**Applicability to open-mpm:**

The PM orchestrator could implement a "plan mode" where it:
1. First pass: generates a structured delegation plan (which agents, which tasks)
2. Optionally shows plan to user for approval
3. Second pass: executes the plan by delegating to sub-agents

```rust
enum ExecutionMode {
    Auto,     // execute immediately
    PlanReview, // show plan, wait for approval
    DryRun,   // show plan only, don't execute
}
```

---

## 6. What "Plans" Could Mean for open-mpm

Since there is no dedicated "plans" API, the best approach for open-mpm is a **structured planning pass**:

### Option A: Two-Phase PM Execution

```
Phase 1 — Planning:
  PM prompt: "Analyze this task and produce a structured plan.
              Output JSON: { agents: [...], tasks: [...], dependencies: [...] }"
  Model returns: structured plan JSON

Phase 2 — Execution:
  PM reads plan, spawns agents per plan
  Monitors execution, aggregates results
```

### Option B: Reasoning-Enhanced Planning (Claude Sonnet 4.6 Extended Thinking)

```
PM invokes Claude Sonnet 4.6 with extended thinking enabled
  → Model produces internal reasoning (thinking blocks)
  → Model produces final plan as structured output
  → PM parses plan and delegates
```

This gives the PM deep reasoning capability for complex task decomposition without the overhead of an explicit planning phase for simple tasks.

### Option C: ReAct-Style Planning (Built Into Agent Loop)

The PM agent naturally plans through its reasoning:
```
User: "Build a Rust web server with auth"
PM thinks: I need to [1] analyze requirements, [2] delegate auth to Engineer, 
           [3] delegate API design to Engineer, [4] QA with QA agent
PM: delegates task 1 → Research Agent
PM: (on result) delegates task 2 → Engineer Agent
...
```

No explicit planning API needed — emerges from the agent loop.

---

## 7. Availability via OpenRouter

| Feature | Available via OpenRouter | Notes |
|---------|--------------------------|-------|
| Chat completions (non-streaming) | Yes | Standard `/v1/chat/completions` |
| Streaming completions | Yes | SSE format |
| Tool/function calling | Yes | All major models |
| Structured outputs (JSON mode) | Yes | `response_format: json_object` |
| Strict JSON Schema (`strict: true`) | Partial | Model-dependent |
| Reasoning/extended thinking | Yes | Via `reasoning` or `thinking` params |
| GPT-5.x preambles | Yes (via prompt) | Not a special param, just model behavior |
| Responses API | No | OpenAI-specific, not in OpenRouter |
| o-series reasoning models | Yes | `openai/o3`, `openai/o4-mini` |
| Claude extended thinking | Yes | `anthropic/claude-sonnet-4.6` |
| Interleaved thinking + tools | Yes | Per OpenRouter docs |

---

## 8. Recommendations for open-mpm

1. **No dedicated "plans" API is needed.** The combination of structured outputs + extended thinking + a two-phase planning prompt covers all planning needs.

2. **Implement a PM planning prompt** that produces structured JSON output (using `response_format: json_object` or a tool-forcing trick) for task decomposition.

3. **Support `reasoning_effort`/extended thinking** as an optional parameter for the PM agent. Use `low` by default for fast orchestration, `high` for complex task analysis.

4. **Consider a plan-review mode** (like Codex Plan Mode) as an optional UX feature — show the delegation plan before spawning sub-agents.

5. **Claude Sonnet 4.6 extended thinking** is available via OpenRouter and is the right model for PM-level reasoning — it can hold 1M token context while reasoning about complex task structures.

---

## Sources

- [OpenAI for Developers in 2025](https://developers.openai.com/blog/openai-for-developers-2025)
- [Introducing GPT-5 for developers](https://openai.com/index/introducing-gpt-5-for-developers/)
- [Reasoning best practices — OpenAI API](https://developers.openai.com/api/docs/guides/reasoning-best-practices)
- [OpenAI API Changelog](https://developers.openai.com/api/docs/changelog)
- [ChatGPT Thinking Duration Controls](https://skywork.ai/blog/chatgpt-thinking-duration-controls/)
- [Claude Sonnet 4.6 on OpenRouter](https://openrouter.ai/anthropic/claude-sonnet-4.6)
- [OpenRouter Streaming Docs](https://openrouter.ai/docs/api/reference/streaming)
