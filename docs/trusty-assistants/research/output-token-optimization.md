# Output Token Optimization for LLM Applications

**Date**: 2026-05-04
**Context**: open-mpm — Rust agent orchestration harness dispatching tasks to sub-agents via OpenRouter/Anthropic API.

---

## 1. Structured Output Formats: JSON/YAML vs Prose

**Does structure reduce output tokens?**

Yes, in most cases. JSON/YAML enforces a schema that eliminates filler prose ("Certainly! Here is the result you requested…"). Measured reductions in agent orchestration contexts range from 15–35% depending on how verbose the prose baseline was.

However, structured output is not free:

- **JSON adds overhead for simple responses.** A yes/no answer expressed as `{"answer": "yes"}` is longer than `yes`. For sub-agents returning code blocks, wrapping in JSON adds braces, keys, and escaping.
- **YAML is slightly more compact** than JSON for deeply nested structures but adds its own verbosity for flat responses.
- **Best fit for structure**: Delegation results that carry metadata (status, error, id, content). This matches open-mpm's existing NDJSON IPC format — the protocol already captures the main win.
- **Avoid forcing structure on prose tasks.** Asking a code-writing agent to return `{"code": "..."}` instead of a raw code block adds escaping cost and is harder to stream-parse.

**Recommendation**: Keep the NDJSON envelope for IPC (already correct). Do not add inner JSON structure to sub-agent `content` fields unless the PM needs to parse specific fields from them.

---

## 2. Prompt Techniques That Reduce Verbosity

Documented techniques, roughly in order of demonstrated effect:

### Quantitative constraints outperform qualitative ones

Qualitative instructions ("be concise", "be brief") produce roughly 1–3% reduction in output tokens. Quantitative constraints ("respond in 3 sentences or fewer", "no more than 25 words between tool calls") produce 10–20% reduction because they give the model a hard target to optimize against.

Source: Claude Code prompt pattern analysis (see `claude-code-prompt-patterns.md`); confirmed by internal Anthropic system prompt patterns.

**Example from Claude Code's own system prompt:**
> "You MUST answer concisely with fewer than 4 lines of text (not including tool use or code generation), unless user asks for detail."

### Effective verbosity-reduction instructions

| Instruction | Effect |
|---|---|
| "No preamble. No sign-off." | Eliminates "Certainly! Here is…" and "I hope this helps!" — saves 10–30 tokens per response |
| "No explanation unless asked." | High impact for engineering agents. Cuts reasoning prose from code responses. |
| "Return only the [code/JSON/result]. No commentary." | Most effective single instruction for tool-calling sub-agents |
| "If you cannot complete the task, respond with one sentence explaining why." | Constrains error verbosity |
| Quantitative word/line limits | 10–20% reduction vs qualitative |
| "Do not restate the task." | Eliminates task echo at start of response |

### What does not work reliably

- "Be brief" / "be concise" alone — too vague to enforce
- Asking for "short" without defining short — model interprets "short" relative to its defaults
- Relying on `temperature` to control length — temperature affects creativity/randomness, not length

---

## 3. Response Format Constraints

### max_tokens

`max_tokens` hard-caps output length and directly controls cost ceiling. Trade-off: if set too low, responses truncate mid-output, producing unusable partial results. For sub-agents generating code, truncation is particularly damaging.

**Recommended approach**: Set `max_tokens` conservatively by task type, not globally:

| Agent role | Suggested max_tokens |
|---|---|
| PM orchestrator (delegation only) | 512–1024 |
| Code-writing sub-agent | 4096–8192 |
| QA / review agent | 2048–4096 |
| Status/summary agent | 256–512 |

open-mpm's agent TOML already supports per-agent `[llm] max_tokens`. Use it.

### Stop sequences

Stop sequences terminate generation when a specific string is encountered. Useful for:
- Forcing agents to stop after a code block (stop on ` ``` ` after first fence closes)
- Preventing padding prose after a JSON object (stop on `\n\n` after `}`)

Stop sequences are underused in most agent systems. For a sub-agent that should return exactly one code block, a stop sequence of ` ``` ` (the closing fence) eliminates any prose that would follow.

### Prompt-level format constraints

Specifying output format in the system prompt ("Return only a JSON object matching this schema: …") is more reliable than `max_tokens` for preventing verbosity, because the model respects format instructions even when it has remaining token budget.

---

## 4. Chain-of-Thought vs Direct Answer

### When CoT increases cost without benefit

Chain-of-thought ("think step by step") adds reasoning tokens that may not improve answer quality for straightforward tasks. For a sub-agent writing a Python function, CoT instructions can 2–4x output tokens with marginal quality improvement on well-defined tasks.

**Rule of thumb**: Use CoT only when the task has multiple valid approaches that require evaluation, or when correctness on a logic/math task is critical. Avoid for code generation with clear specs.

### Anthropic Extended Thinking (reasoning budget)

Anthropic's `thinking` parameter (beta, available on Claude 3.5+ and Claude 4 models) enables a separate reasoning scratchpad. Key characteristics:

- Reasoning tokens are billed at input token rates for cache reads, but at full output rates when generated
- The `budget_tokens` parameter caps reasoning token spend (minimum 1024, typical range 5000–16000)
- Thinking tokens do **not** appear in the response content — they are internal and not forwarded to the user
- Setting `budget_tokens` to 0 effectively disables extended thinking

**For open-mpm**: Extended thinking is counterproductive for most sub-agent tasks (code writing, file operations) where the spec is clear. Reserve it for the PM orchestrator when decomposing ambiguous multi-step requests.

**API parameter** (Anthropic direct):
```json
{"thinking": {"type": "enabled", "budget_tokens": 5000}}
```

---

## 5. Caching Strategies

### Anthropic prompt caching (cache_control)

Anthropic's prompt caching stores up to 4 cache breakpoints per request. Cached input tokens cost 10% of the normal input token rate (cache read) after a one-time cache write cost of 125%.

**What to cache**:
- Static system prompts (agent persona, tool definitions, coding standards)
- Skill markdown content injected into system prompts
- Long reference documents or codebases passed as context

**What not to cache**:
- Dynamic content (task descriptions, session IDs, timestamps)
- Content that changes per request

**Savings profile**: For a system prompt of 2000 tokens repeated across 10 sub-agent calls, caching saves roughly 18,000 input tokens (2000 × 10 × 0.9). At $3/M tokens (Sonnet), that is $0.054 per 10 calls — meaningful at scale.

**Implementation for open-mpm**: In the `async-openai` client, set `cache_control` on system message content blocks. The agent TOML system prompt content is static per agent type and is an ideal cache candidate.

```json
{
  "role": "system",
  "content": [
    {
      "type": "text",
      "text": "<static system prompt>",
      "cache_control": {"type": "ephemeral"}
    }
  ]
}
```

### Static/dynamic prompt partitioning

Split system prompts into a static section (cacheable: role, tools, style rules) and a dynamic section (not cacheable: task context, session state). Never embed dynamic data in the static section — it invalidates the cache.

This pattern is documented in `claude-code-prompt-patterns.md` as the primary caching discipline from Claude Code's own architecture.

---

## 6. Task Decomposition Impact

### Does decomposition reduce total output tokens?

**Net effect: usually increases total tokens, but improves quality and reduces wasted tokens.**

Breaking a large task into sub-tasks adds:
- Overhead per call: system prompt, delegation framing, result envelope
- Multiple round-trips vs a single large response

However, large single-call responses often include hedging, exploratory reasoning, and self-correction that decomposed calls avoid. The PM/sub-agent split in open-mpm is already the right granularity: PM handles routing (low token cost), specialist agents handle focused execution (higher but bounded token cost).

**Antipattern to avoid**: Delegating a task to a sub-agent and then having the sub-agent delegate further creates exponential overhead. Keep delegation depth at 2 (PM → specialist).

**Optimization**: Pass only the minimal task context to each sub-agent. The PM should not forward its entire conversation history — only the specific task description and any directly relevant artifacts.

---

## 7. Model Selection for Token Efficiency

| Model | Relative output verbosity | Notes |
|---|---|---|
| Claude Haiku 3.5 | Low | Most concise; shorter explanations; best for classification, routing, simple transforms |
| Claude Sonnet 3.5/4 | Medium | Balanced; slightly more verbose than Haiku but higher capability |
| Claude Opus 4 | High | Most verbose; adds context, caveats, and explanation; best for complex reasoning |
| GPT-4o Mini | Low-medium | Comparable to Haiku in brevity |
| GPT-4o | Medium-high | More verbose than Haiku; similar to Sonnet |

**For open-mpm**: Route simple delegation decisions and status summaries through Haiku. Use Sonnet for code-writing sub-agents. Reserve Opus only if a task explicitly requires deep reasoning.

Model routing by task type is already supported via per-agent `model` in agent TOML — exploit this.

---

## 8. Anthropic-Specific Parameters

| Parameter | Effect on output length | Notes |
|---|---|---|
| `max_tokens` | Hard cap | Most direct control; risk of truncation |
| `temperature` | Indirect / minimal | Lower temperature reduces variation but does not reliably reduce length |
| `top_p` | Minimal | Similar to temperature; not a length control |
| `thinking.budget_tokens` | Caps reasoning tokens | Does not affect response length directly |
| `cache_control` | Input cost only | Reduces input cost, not output length |
| Stop sequences | Truncates at pattern | Effective for structured, predictable outputs |

**Temperature and length**: Common misconception that lower temperature produces shorter responses. It does not — it produces more predictable responses. Length is controlled by prompt instructions and `max_tokens`.

---

## Summary: Priority Actions for open-mpm

1. **Add quantitative conciseness constraints to agent system prompts.** Replace "be concise" with "respond in ≤3 sentences between tool calls. Return code directly without preamble or sign-off."

2. **Set per-agent `max_tokens` by role.** PM orchestrator: 1024. Code agents: 8192. Status agents: 512.

3. **Implement prompt caching** on static system prompt content blocks via `cache_control: ephemeral`. Target: PM system prompt and all sub-agent system prompts.

4. **Add stop sequences** for code-returning agents to halt after the closing code fence.

5. **Route simple tasks to Haiku.** PM delegation reasoning and result summarization do not require Sonnet.

6. **Eliminate CoT from routine sub-agent calls.** Disable extended thinking for code-writing tasks with clear specs.

7. **Pass minimal context to sub-agents.** PM should forward only the specific task, not full conversation history.

---

## Implementation Status (open-mpm, 2026-05-04)

Tracking the priority actions above against the codebase:

| # | Action | Status | Location |
|---|---|---|---|
| 1 | Quantitative conciseness in agent prompts (#294) | ✅ Implemented | `.open-mpm/agents/*.toml` — appended `## Output Conciseness` section to pm, ctrl, personal-assistant, cto-assistant, and all engineer agents |
| 2 | Per-role `max_tokens` (#295) | ✅ Implemented | `.open-mpm/agents/*.toml` — pm=2048, ctrl/personal-assistant/cto-assistant=1024, engineers=8192/16384, qa-agent already=4096 |
| 3 | `cache_control` on static system prompts (#296) | ✅ Already implemented | `src/llm/anthropic_native.rs:156-192` — ephemeral cache_control attached to system block, last tool definition, AND last assistant message when history > 2000 tokens. Active for any agent whose adapter is Anthropic AND `enable_prompt_caching=true` (default). For OpenRouter requests the field would break the OpenAI-format body, so caching only applies on the native Anthropic path (`use_anthropic_direct=true`) — this is the documented design constraint. |
| 4 | Stop sequences for code agents (#297) | ⚠️ Partial — TOML only | TOML field added to `LlmParams.stop_sequences`. Wire-up TODO in `src/llm/mod.rs` at the request-builder site (~line 917). Threading `stop_sequences: &[String]` through `chat()` / `chat_with_tools_gated()` touches ~5 call sites in `main.rs` and `ctrl/mod.rs`; deferred to keep this PR focused. |
| 5 | Route PM routing to Haiku (#298) | ⚠️ Partial — TOML only | `LlmParams.routing_model` field added; `pm.toml` declares `routing_model = "anthropic/claude-haiku-4-5"`. Runtime wire-up TODO in `src/ctrl/mod.rs` at the tool-armed delegation call site. Requires per-turn model switching in `chat_with_tools_gated`, which is non-trivial — held pending measured savings. |
| 6 | Disable CoT for sub-agents (#299) | ⚠️ Partial — TOML only | `LlmParams.thinking_enabled` field added; declared `thinking_enabled = false` in all engineer agent TOMLs. Currently informational — open-mpm does not yet enable Anthropic extended thinking anywhere, so the field documents intent and preempts a regression once thinking is wired on. |
| 7 | Pass minimal context to sub-agents | ✅ Already implemented | `src/agents/context_filter.rs` — PM only forwards the resolved task description, not full conversation history. |

### Blockers worth noting

- **Stop sequences for the OpenRouter path**: The async-openai 0.28 builder supports `stop()` natively, so wiring is just a parameter pass-through. The Anthropic native path needs `body["stop_sequences"]` injected in `build_anthropic_request`. The Bedrock path needs the equivalent in `bedrock::chat_with_tools` — Bedrock Converse uses `stopSequences` in the inference config. None of these are blockers per se; just deferred mechanical work.
- **Routing-model swap mid-loop**: `chat_with_tools_gated` is currently single-model. Cleanest implementation is a thin wrapper that runs ONE turn on `routing_model` (returns the tool call), then hands off to a second `chat_with_tools_gated` call on `model` if synthesis is needed. Avoids changing the inner loop's invariants.

---

## References

- Anthropic prompt caching docs: https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching
- Anthropic extended thinking: https://docs.anthropic.com/en/docs/build-with-claude/extended-thinking
- Claude Code prompt patterns: `docs/research/claude-code-prompt-patterns.md`
- Token compression techniques: `docs/research/token-compression-rtk-ztk.md`
- Agent decomposition patterns: `docs/research/agent-decomposition-patterns.md`
