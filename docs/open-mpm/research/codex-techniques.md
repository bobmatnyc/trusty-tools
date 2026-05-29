# GPT-5.1-Codex Techniques for open-mpm

**Date:** 2026-04-22
**Issue:** [#65 — Research: source analysis of GPT-5 Codex — techniques to adopt in open-mpm](https://github.com/bobmatnyc/open-mpm/issues/65)
**Scope:** GPT-5.1-Codex tool calling patterns, structured output strategies, prompt engineering for code generation, OpenAI Agents SDK patterns, and the immediate `finish_task` turn-0 failure.

---

## Executive Summary

GPT-5.1-Codex behaves fundamentally differently from Claude when placed inside a `tool_choice = "any"` + `finish_task` harness. Claude models interpret `tool_choice = "any"` (mapped to `"required"` on the wire for OpenAI) as "keep calling tools until your work is genuinely done." GPT-5.1-Codex treats `finish_task` as a valid first action whenever the task appears fully described in the system prompt — it reads the instructions, concludes it has already been told what to produce, summarizes them, and exits. The model is not broken; the harness assumption is wrong.

Three changes to `gpt5-codex-engineer.toml` and its system prompt will fix the turn-0 exit. Beyond the immediate fix, OpenAI's published guidance on Codex harness engineering reveals several techniques worth adopting project-wide: the `apply_patch` diff tool, parallel batch reads, milestone status tracking, and lightweight `AGENTS.md` files as a table-of-contents for per-agent context.

---

## 1. The Codex Model Lineage (Brief)

| Era | Model | Key characteristic |
|-----|-------|--------------------|
| 2021 | `code-davinci-002` (Codex) | Pure completion; fill-in-the-middle via `<fim_prefix>/<fim_suffix>/<fim_middle>` tokens. No chat, no tools. |
| 2023 | GPT-4 Turbo | Function calling introduced. Code generation via chat. |
| 2025 | `codex-1` (powers Codex app) | o3-based; RL-trained on real engineering tasks. Runs tests iteratively until they pass. |
| 2025–2026 | `gpt-5.1-codex`, `gpt-5.3-codex` | Agentic coding model. Parallel tool calls, `apply_patch`, milestone tracking, `phase` metadata. |

The fill-in-the-middle (FIM) technique from original Codex is no longer directly relevant — the GPT-5.x generation uses tool calls and structured outputs instead of prompt token tricks. What carried over from the FIM era is the principle of **giving the model explicit structural anchors** for where code begins and ends.

---

## 2. GPT-5.1-Codex Tool Calling Patterns

### 2.1 `tool_choice`: OpenAI vs Anthropic semantics

This is the root cause of the observed failure.

| Value | Anthropic semantics | OpenAI semantics |
|-------|--------------------|--------------------|
| `"auto"` | Model decides | Model decides |
| `"any"` | Must call some tool | **Not a valid value.** OpenAI uses `"required"` for this. |
| `"required"` | Not supported (Anthropic maps from `"any"`) | Must call at least one tool |
| Named function | Forces that specific tool | Same |

**The open-mpm adapter currently maps `ToolChoice::Any` → `"required"` on the OpenAI wire (correct), but the model receives `finish_task` in its tool list.** On turn 0, GPT-5.1-Codex reads the system prompt, sees it contains a complete coding spec, concludes the task is described rather than assigned, and calls `finish_task` with a paraphrase of the instructions. This is technically compliant behavior — `"required"` only mandates calling _some_ tool.

**Fix:** Remove `finish_task` from the tool list for the first turn, or restructure so the system prompt does not describe the output format in enough detail that the model mistakes the spec for completed work. See Section 6.

### 2.2 Parallel tool calls

GPT-5.1-Codex is specifically trained to parallelize tool calls. The official guidance states:

> "When multiple tool calls can be parallelized (e.g., todo updates with other actions, file searches, reading files), make these tool calls in parallel instead of sequential."

And more specifically:

> "If you need multiple files (even from different places), read them together using `multi_tool_use.parallel`. Only make sequential calls if you truly cannot know the next file without seeing a result first."

**Implication for open-mpm:** The current NDJSON IPC protocol sends one result back to the PM per agent invocation. If the gpt5-codex-engineer agent is reading multiple files via `fs_reader` calls, those should be issued as parallel tool calls in a single LLM turn, not as sequential single-tool-call turns. The harness loop already supports receiving multiple tool calls per turn; this is a prompting issue, not a protocol issue.

### 2.3 Tool hierarchy: prefer named tools over shell

OpenAI explicitly trains Codex to prefer dedicated tools over raw terminal commands:

> "Strictly avoid raw `cmd`/terminal when a dedicated tool exists. Default to solver tools: `git`, `rg`, `read_file`, `list_dir`, `glob_file_search`, `apply_patch`."

The existing `gpt5-codex-engineer.toml` system prompt says nothing about this preference. Adding a tool hierarchy statement to the prompt will reduce the model's tendency to call `shell_exec` for things `fs_reader` can handle.

### 2.4 The `apply_patch` tool

The most significant tool calling difference from Claude: GPT-5.1-Codex was trained specifically to use an `apply_patch` tool that accepts **unified diff format**, not full file content. OpenAI reports this reduced failure rates by 35% compared to returning complete files.

Schema pattern (Chat Completions compatible):

```json
{
  "type": "function",
  "function": {
    "name": "apply_patch",
    "description": "Apply a unified diff patch to create, update, or delete files. Use this instead of write_file for modifications to existing content.",
    "parameters": {
      "type": "object",
      "properties": {
        "operation": {
          "type": "string",
          "enum": ["create_file", "update_file", "delete_file"]
        },
        "path": { "type": "string" },
        "diff": {
          "type": "string",
          "description": "Unified diff format. For create_file, full file content with +++ prefix on every line."
        }
      },
      "required": ["operation", "path", "diff"],
      "additionalProperties": false
    }
  }
}
```

**Recommendation for open-mpm:** Add `apply_patch` as a new tool in `src/tools/` that applies unified diffs server-side. This is a better fit for GPT-5.x models than the current `## File:` extraction, and could replace or supplement `write_file` for all engineer agents.

---

## 3. Structured Output Approaches

### 3.1 Current approach: `## File:` markdown extraction

The current system prompt instructs agents to emit:

```
## File: path/to/file.py
```python
<complete file contents>
```
```

The harness then regex-extracts these blocks. This works reliably for Claude, which follows exact format instructions. It is fragile for GPT-5.1-Codex because:

1. The model may emit code blocks without the `## File:` header if it perceives the header as optional formatting.
2. GPT-5.1-Codex was trained on `apply_patch`-style tool use, not markdown extraction — the markdown format goes against its training distribution.
3. If the model calls `finish_task` with a summary instead of emitting files, the harness silently produces no files.

### 3.2 Structured outputs via `response_format: json_schema`

OpenAI's Chat Completions API supports strict JSON Schema output:

```json
{
  "response_format": {
    "type": "json_schema",
    "json_schema": {
      "name": "code_output",
      "strict": true,
      "schema": {
        "type": "object",
        "properties": {
          "files": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" },
                "language": { "type": "string" }
              },
              "required": ["path", "content", "language"],
              "additionalProperties": false
            }
          },
          "summary": { "type": "string" }
        },
        "required": ["files", "summary"],
        "additionalProperties": false
      }
    }
  }
}
```

With `strict: true`, GPT-5.x guarantees 100% schema conformance. The harness parses the JSON directly rather than regex-extracting markdown.

**Caveats via OpenRouter:** OpenRouter routes to `/v1/chat/completions`. The newer Responses API syntax (`text.format` instead of `response_format`) is not available through OpenRouter. The Chat Completions `response_format: json_schema` path is supported for OpenAI models on OpenRouter but `strict: true` support is model-dependent — verify with a test call. OpenRouter documentation confirms `response_format: json_object` works universally; strict schema enforcement is listed as partial.

**Compatibility issue:** `response_format` cannot be combined with `tool_choice = "required"` and active tool calls in the same request. If the agent uses tools during its work loop, structured output via `response_format` applies only to the _final_ response turn (when `finish_reason = "stop"`), not to intermediate tool-calling turns.

### 3.3 Recommended hybrid approach

The cleanest approach for GPT-5.1-Codex in open-mpm:

1. **During work turns** (while the agent is calling `fs_reader`, `shell_exec`, etc.): Use `tool_choice = "auto"`, let the model call tools freely.
2. **Final turn** (after all work tools return): Add `finish_task` to the tool list, switch to `tool_choice = "required"`, and require the model to call `finish_task(summary=..., files=[...])` with an extended schema that includes a `files` array.

This avoids the structured output / tool calling incompatibility and keeps file output explicit and parseable.

Alternatively: drop `finish_task` entirely and instead use `response_format: json_schema` on the final turn — trigger it by detecting that all previous tool calls have returned successfully. The harness switches `tool_choice` to `"none"` and adds `response_format` to force a structured final response.

---

## 4. Prompt Engineering Changes for `gpt5-codex-engineer.toml`

### 4.1 The turn-0 finish_task failure: root cause

The system prompt currently reads like a specification document: it describes the output format in exhaustive detail, lists required files, and states the TDD discipline. GPT-5.1-Codex interprets a detailed specification as evidence that the task is already planned — and since `finish_task` is available, it calls it with a summary of the spec.

This is the same failure pattern OpenAI documented with "one big AGENTS.md" files:

> "Too much guidance becomes non-guidance. When everything is 'important,' nothing is. Agents end up pattern-matching locally instead of navigating intentionally."

### 4.2 System prompt restructuring

**Problem with current prompt:** The `## Output Format (CRITICAL — follow EXACTLY)` section is so detailed that the model treats it as the answer rather than as instructions for producing the answer.

**Fix: separate "what to do" from "how to format what you do."**

Restructure the system prompt as follows:

```toml
[system_prompt]
content = """
You are an expert Python engineer operating inside an agentic coding loop.

## Your mission
Implement the plan you receive. Deliver working, tested Python code.
Do not stop until you have:
1. Written every file listed in the plan
2. Written tests that cover every test case in the plan
3. Verified the code is syntactically correct

You are an autonomous senior engineer. Once given a task, proceed through
implementation, testing, and verification without waiting for prompts.
Bias to action. If a detail is ambiguous, make a reasonable assumption and
implement — do not ask for clarification or call finish_task until you have
produced all required files.

## Tool use discipline
- Prefer write_file over shell_exec for creating files
- Read multiple files in one parallel batch, not sequentially
- Never call finish_task until all required files exist and tests pass

## Output file convention
Use write_file for each source file. Call finish_task last, with a summary
of what you produced. Do not embed file contents in the finish_task summary.

## Required file structure
Every task produces exactly these files (substitute <pkg> with the package name):
- <pkg>/__init__.py
- <pkg>/core.py
- <pkg>/__main__.py
- test_<pkg>.py (at least 5 pytest tests)

## Code standards
[... keep existing code standards section unchanged ...]
"""
```

Key changes:
- Move the output format section to the end, after the mission statement
- Lead with the "autonomous senior engineer" persona framing (this is the exact pattern OpenAI recommends in the Codex prompting guide)
- Make `finish_task` conditional on deliverables being complete, not a valid early escape

### 4.3 `tool_choice` for GPT-5.1-Codex agents

Change `gpt5-codex-engineer.toml`:

```toml
[llm]
temperature = 0.2
max_tokens = 16384
tool_choice = "auto"       # was "any" — avoid forcing finish_task on turn 0
use_finish_task = true     # keep finish_task in tool list but as auto choice
```

With `tool_choice = "auto"`, GPT-5.1-Codex will call tools when it has work to do and emit a natural-language final message when done. Set `use_finish_task = true` so the harness still has an explicit completion signal. The model will call `finish_task` naturally when done rather than being forced into it.

**If you want to keep `tool_choice = "any"`** (forced tool calls every turn), you must remove `finish_task` from the tool list for the first N turns. This requires a harness-level change: the agent loop passes `use_finish_task = false` for turns 1–3 and only adds it after turn 3. This prevents the model from escaping before doing any work.

### 4.4 Milestone tracking (adopt from Codex harness)

OpenAI's Codex prompting guide recommends a `todo_write`/`update_plan` tool pattern:

> "Maintain 2–5 milestone items with statuses (pending, in_progress, completed). Never more than ~8 tool calls without an update."

For open-mpm, this maps to adding a lightweight `update_status` tool or using the existing `write_file` to create a `PROGRESS.md` file. The key insight: GPT-5.1-Codex stays engaged in the loop when it has pending milestone items. Without explicit milestones, it may determine the task is "done" prematurely because nothing tells it otherwise.

Concretely, add to the system prompt:

```
## Progress tracking
Before writing any code, call write_file to create PROGRESS.md listing each
required file as a task item:
- [ ] test_<pkg>.py
- [ ] <pkg>/__init__.py
- [ ] <pkg>/core.py
- [ ] <pkg>/__main__.py

Update PROGRESS.md after each file is written. Call finish_task only when
all items show [x].
```

This creates a forcing function: the model must tick off files before it can legitimately call `finish_task`.

---

## 5. OpenAI Agents SDK Patterns vs. open-mpm NDJSON

### 5.1 SDK orchestration primitives

The OpenAI Agents SDK uses three primitives:

| SDK primitive | open-mpm equivalent |
|---------------|---------------------|
| `Agent` (LLM + instructions + tools) | `AgentConfig` TOML + tool registry |
| `Agent.as_tool()` (sub-agent as tool) | `delegate_to_agent` tool in PM |
| `handoff()` (transfer control to specialist) | Not implemented — PM always retains control |
| `Guardrails` (input/output validation) | `phase_audit.rs` (partial) |
| `Sessions` (persistent context) | `persistent_session` in `AgentInfo` |
| `asyncio.gather` (parallel agents) | Not implemented — sequential delegation |

### 5.2 Agents as tools vs. handoffs

The SDK's "agents as tools" pattern maps almost exactly to open-mpm's current `delegate_to_agent` design: the PM retains control and calls sub-agents as bounded tasks. This is the correct pattern for a workflow harness — maintain one thread of control.

The "handoffs" pattern (where the specialist takes over the conversation) is not appropriate for open-mpm's use case. The PM should always remain the orchestrator.

### 5.3 Code-driven orchestration vs. LLM-driven

The SDK documentation notes:

> "Orchestrating via code makes tasks more deterministic and predictable in terms of speed, cost, and performance."

open-mpm's prescriptive workflow JSON (`config/workflows/prescriptive-gpt.json`) is already implementing this insight — it specifies the agent sequence deterministically rather than letting the PM LLM choose. This is the right approach for a bake-off harness where reproducibility matters.

### 5.4 Evaluator loops

The SDK recommends an evaluator loop pattern:

```
while output quality < threshold:
    output = task_agent.run(task)
    feedback = evaluator_agent.evaluate(output)
    task = task + feedback
```

open-mpm does not implement this yet. For the bake-off, this would mean: after the engineer agent produces files, a QA agent evaluates them and feeds back failures. The `qa-agent.toml` exists but it's not wired into an evaluator loop. This is a high-value improvement that the Agents SDK makes easy; replicating it in open-mpm requires adding a retry loop in the workflow engine.

### 5.5 Parallel agent execution

The SDK uses `asyncio.gather` to run multiple agents simultaneously. open-mpm currently runs agents sequentially (one delegation at a time). For tasks that can be decomposed into independent subtasks, parallel delegation would reduce wall-clock time significantly. This requires:

1. The PM to emit multiple `delegate_to_agent` calls in one LLM turn (GPT-5.1 already does this with parallel tool calls)
2. The workflow engine to detect parallel tool calls and dispatch them concurrently via `tokio::join!` or `FuturesUnordered`
3. Collecting results and injecting all of them back into the PM's message history before the next turn

This is a non-trivial change to the PM loop but high-value for multi-phase tasks.

---

## 6. Immediate Fix: Turn-0 `finish_task` Failure

This is the most actionable item. The agent called `finish_task` with 82 characters and no code because:

1. `tool_choice = "any"` forced a tool call on turn 0
2. `finish_task` was the path of least resistance given the detailed spec in the system prompt
3. The system prompt reads as a completed plan, not as a task to execute

**Minimal fix (two changes to `gpt5-codex-engineer.toml`):**

Change 1: Switch `tool_choice` from `"any"` to `"auto"`.

```toml
[llm]
tool_choice = "auto"
use_finish_task = true
```

Change 2: Add the autonomy/persistence framing at the top of the system prompt, before any format instructions:

```
You are an autonomous senior Python engineer inside an agentic coding loop.
Your job is to produce working, tested Python code — not to describe what
code would be produced. You must write every required file before calling
finish_task. Do not call finish_task until all files exist.
```

**Why this works:** `tool_choice = "auto"` removes the pressure to call any tool on turn 0. The explicit prohibition on calling `finish_task` before writing files guards against the model using it as an escape hatch. The "produce code, not descriptions" framing counters the model's tendency to treat the system prompt's format spec as the deliverable.

**Stronger fix (harness-level):** Gate `finish_task` availability behind a turn counter. Only add `finish_task` to the tool registry after turn 3 of the agent loop. This is a one-line change in the loop that builds the tool list per-turn. This means the model physically cannot call `finish_task` on turns 0–2, forcing at least three tool calls before any exit is possible.

---

## 7. The `codex-1` Published System Message

OpenAI published the `codex-1` system message (the one that powers the Codex app) to help developers understand the model's defaults. Key patterns extracted:

1. **AGENTS.md as context map:** The model is instructed to read `AGENTS.md` at the start of every session. OpenAI now recommends keeping this file to ~100 lines as a table of contents pointing to a `docs/` directory, rather than a monolithic rulebook.

2. **Test-first discipline:** The system message instructs the model to run all tests listed in AGENTS.md. The model loops on test failures until they pass — similar to how open-mpm's `use_finish_task` loop is intended to work.

3. **Verification before completion:** The model is instructed not to mark tasks complete until linters and pre-commit checks pass. In open-mpm terms: don't call `finish_task` until `shell_exec("python -m pytest ...")` returns exit 0.

4. **AGENTS.md in open-mpm:** The project already has AGENTS.md files in the bake-off test fixture directory. These could be extended to include per-task test commands that the code agent should run before finishing.

---

## 8. Summary of Recommended Changes

### Immediate (fix the turn-0 failure)

| File | Change |
|------|--------|
| `config/agents/gpt5-codex-engineer.toml` | `tool_choice = "auto"` (was `"any"`) |
| `config/agents/gpt5-codex-engineer.toml` | Add autonomy/persistence framing at top of system prompt |
| `config/agents/gpt5-codex-engineer.toml` | Add explicit: "Do not call finish_task until all files are written" |

### Short-term (improve GPT-5.x reliability)

| Item | Effort | Impact |
|------|--------|--------|
| Add PROGRESS.md milestone tracking to system prompt | Small | Forces the model to enumerate work before starting |
| Add tool hierarchy statement (prefer `write_file` over `shell_exec`) | Trivial | Reduces shell escape attempts |
| Add parallel read instruction to system prompt | Trivial | Speeds up multi-file tasks |
| Gate `finish_task` availability behind turn 3 in the agent loop | Small | Hard blocks turn-0 exit for any model |

### Medium-term (architecture improvements from OpenAI patterns)

| Item | Effort | Impact |
|------|--------|--------|
| Add `apply_patch` tool to tool registry | Medium | Matches GPT-5.x training distribution; reduces failure rate |
| Implement structured JSON output (`response_format: json_schema`) as alternative to `## File:` extraction | Medium | More robust file extraction; eliminates regex parsing |
| Wire QA agent into an evaluator loop | Medium | Iterative quality improvement without human intervention |
| Implement parallel agent dispatch via `tokio::join!` | Large | Cuts wall-clock time for multi-agent workflows |

---

## Sources

- [GPT-5.1 Prompting Guide — OpenAI Cookbook](https://developers.openai.com/cookbook/examples/gpt-5/gpt-5-1_prompting_guide)
- [Codex Prompting Guide — OpenAI Cookbook](https://developers.openai.com/cookbook/examples/gpt-5/codex_prompting_guide)
- [Introducing Codex — OpenAI](https://openai.com/index/introducing-codex/)
- [Harness Engineering: Leveraging Codex in an Agent-First World — OpenAI](https://openai.com/index/harness-engineering/)
- [Unrolling the Codex Agent Loop — OpenAI](https://openai.com/index/unrolling-the-codex-agent-loop/)
- [Custom Instructions with AGENTS.md — OpenAI Developers](https://developers.openai.com/codex/guides/agents-md)
- [Codex Prompting — OpenAI Developers](https://developers.openai.com/codex/prompting)
- [Structured Model Outputs — OpenAI API](https://developers.openai.com/api/docs/guides/structured-outputs)
- [Function Calling — OpenAI API](https://platform.openai.com/docs/guides/function-calling)
- [Agent Orchestration — OpenAI Agents SDK](https://openai.github.io/openai-agents-python/multi_agent/)
- [Tools — OpenAI Agents SDK](https://openai.github.io/openai-agents-python/tools/)
- [Building More with GPT-5.1-Codex-Max — OpenAI](https://openai.com/index/gpt-5-1-codex-max/)
- [OpenRouter Structured Outputs](https://openrouter.ai/docs/guides/features/structured-outputs)
- [GPT-5.1-Codex Guide — DataCamp](https://www.datacamp.com/tutorial/gpt-5-1-codex-guide-with-hands-on-project)
- [Multi-Agent Portfolio Example — OpenAI Cookbook](https://cookbook.openai.com/examples/agents_sdk/multi-agent-portfolio-collaboration/multi_agent_portfolio_collaboration)
