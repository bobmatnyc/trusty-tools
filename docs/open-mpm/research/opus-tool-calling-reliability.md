# Opus Tool-Calling Reliability: Research Findings

**Date:** 2026-04-22
**Context:** Plan-agent hit max_turns (12) while phase-audit JSONL recorded 7 `advance_workflow_phase` calls.

---

## (a) Is Plain-Text-Instead-of-Tool-Call a Known Opus Bug?

**Short answer: Not a model bug — it is expected behavior under specific conditions.**

Web search found no community bug reports specific to `claude-opus-4` via OpenRouter producing
plain text instead of tool calls as a systematic defect. What is documented:

1. **Default `auto` behavior**: With `tool_choice: auto` (the open-mpm default), the model can
   and will decide not to call any tool when it believes the task is complete or when it
   is "confused" about what to do next. This is by design, not a regression.

2. **Opus 4.7 strict literalism note**: Anthropic's own migration guide says Opus 4.7
   "will not silently generalize an instruction from one item to another." If the system
   prompt or tool description doesn't make call requirements crystal clear, the model
   defaults to prose.

3. **No OpenRouter-specific reports found**: The search did not surface any GitHub issues,
   Reddit threads, or Discord reports attributing tool-calling failure to OpenRouter's
   claude-opus-4 routing specifically.

**Conclusion:** Occasional plain-text turns are normal with `tool_choice: auto`. The
open-mpm harness already handles this with the tool-discipline retry logic in
`chat_with_tools_gated` (one retry per consecutive plain-text block, then accept).

---

## (b) What Does the Phase-Audit Discrepancy Tell Us About the Harness?

The diagnostic: **7 audit entries written + max_turns (12) hit = the tool WAS called but the
loop never terminated.**

Reading `src/llm/mod.rs` (`chat_with_tools_gated`), the exit condition is:

```
tool_calls.is_empty() => accept text OR inject reminder
```

The loop only returns `Ok(text)` when:
- The model produces **zero tool calls in a turn**, AND
- `should_retry_plain_text_turn` returns false (already retried, or last turn).

The loop terminates with `bail!` (max_turns error) when `max_turns` is exhausted.

**The smoking gun:** `advance_workflow_phase` is documented in `phase_audit.rs` as:
> "Appends a JSONL audit entry; **does not itself change workflow state**."

The tool does NOT signal completion to `chat_with_tools_gated`. The model calls it and
gets back `"recorded phase 'X'"` as a tool result, but the loop has no hook to interpret
that string as a terminal signal. The loop keeps running until the model produces a
plain-text-only turn OR hits `max_turns`.

**Likely failure pattern across 12 turns:**

| Turn | Model action |
|------|-------------|
| 0-6  | Calls `advance_workflow_phase` (7 times) → gets back "recorded..." → confused, keeps looping |
| 7-10 | Alternates: plain text (1 retry each) + tool calls |
| 11   | Final turn exhausted → `bail!(max_turns)` |

The model was probably calling the tool correctly but receiving an ambiguous success
message that didn't signal "you're done — return a text summary now." This is a
**harness design bug**, not an Opus bug.

---

## (c) Recommended Fixes

### Fix 1: Make `advance_workflow_phase` explicitly signal completion

Change the tool's success response from `"recorded phase 'X'"` to something like:
```
"Phase 'X' recorded. Your task is complete — respond now with a plain-text summary for the workflow engine."
```

This nudges the model to produce a text turn immediately after calling the tool, which
the harness detects as the terminal condition.

### Fix 2: Add a completion sentinel in `chat_with_tools_gated`

After dispatching `advance_workflow_phase`, the harness could detect that tool by name
and immediately prompt one more LLM turn with a `"summarize your work and stop calling tools"` user message. Keeps the loop deterministic.

### Fix 3: Use `tool_choice: {"type": "any"}` + a final-answer tool

Anthropic's recommended pattern for loops that must always produce structured output:
define an explicit `finish_task(summary: string)` tool and set `tool_choice: any`.
The loop terminates when `finish_task` is called. Remove `advance_workflow_phase` or
make it not count toward tool-calling turns.

### Fix 4 (easiest): Reduce `max_turns`

12 turns for a single phase is high. If `advance_workflow_phase` is designed to be
called once per phase, cap at 3-4 turns. Hitting max_turns in 12 with 7 audit entries
means the model was cycling uselessly for ~5 turns.

### On `tool_choice: required / any`

Anthropic docs confirm `tool_choice: any` forces the model to always call one of the
registered tools. This eliminates plain-text mid-task turns entirely. Note: incompatible
with extended thinking. The open-mpm harness currently passes no `tool_choice` (defaults
to `auto`). Adding `tool_choice: any` in the builder at `src/llm/mod.rs:270` would
enforce tool usage every turn.

---

## References

- [Anthropic tool use overview](https://docs.anthropic.com/en/docs/agents-and-tools/tool-use/overview)
- [Anthropic tool_choice cookbook](https://platform.claude.com/cookbook/tool-use-tool-choice)
- [Anthropic implement tool use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/implement-tool-use)
- [OpenRouter Claude Opus 4.6](https://openrouter.ai/anthropic/claude-opus-4.6)
- [Claude Opus 4.7 best practices](https://claudefa.st/blog/guide/development/opus-4-7-best-practices)
- [Advanced tool use blog post](https://www.anthropic.com/engineering/advanced-tool-use)
