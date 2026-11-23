# Claude Code System Prompt Patterns: Research & Recommendations

**Date**: 2026-04-25
**Researcher**: Research Agent (claude-sonnet-4-6)
**Status**: Complete

---

## 1. Sources Found

### What Was Leaked and Where

**The March 2026 Source Map Leak**
Anthropic accidentally shipped source maps in Claude Code npm package v2.1.88 on March 31, 2026, exposing the full 512,000-line TypeScript codebase (~1,900 files). Anthropic issued DMCA takedowns, but the material was widely mirrored.

| Source | What It Contains |
|---|---|
| [Piebald-AI/claude-code-system-prompts](https://github.com/Piebald-AI/claude-code-system-prompts) | 110+ extracted system prompt components from the compiled source, updated per release (v2.1.120 as of April 2026). Highest fidelity; automated extraction, not inference. |
| [asgeirtj/system_prompts_leaks](https://github.com/asgeirtj/system_prompts_leaks/blob/main/Anthropic/claude-code.md) | Extracted runtime prompt including environment variables; confirms the opening identity text. |
| [How Claude Code Builds a System Prompt — dbreunig.com](https://www.dbreunig.com/2026/04/04/how-claude-code-builds-a-system-prompt.html) | Structural analysis of the ~25+ conditional sections; identifies static/dynamic boundary. |
| [Haseeb Qureshi GitHub Gist — Inside the Claude Code Source](https://gist.github.com/Haseeb-Qureshi/d0dc36844c19d26303ce09b42e7188c1) | Technical analysis including the `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` marker, A/B test evidence, and the 1.6%/98.4% split between loop and infrastructure. |
| [OutSight AI — Peeking Under the Hood](https://medium.com/@outsightai/peeking-under-the-hood-of-claude-code-70f5a94a9a62) | Quoted prompt sections; confirms verbatim text of identity and behavioral instructions. |
| [Dive into Claude Code — arXiv 2604.14228](https://arxiv.org/html/2604.14228v1) | Academic analysis of architecture; architectural not prompt-verbatim. |
| [Claude Code CHANGELOG.md — Piebald-AI](https://github.com/Piebald-AI/claude-code-system-prompts/blob/main/CHANGELOG.md) | 162-version changelog showing prompt evolution; reveals what was added and removed over time. |

**Note on verbatim reproduction**: Anthropic has issued DMCA takedowns for full prompt text. This document does not reproduce full prompt sections verbatim; it extracts patterns, structure, and design principles that are appropriate for adaptation.

---

## 2. Key Prompt Patterns

### 2.1 Identity and Framing

The Claude Code system prompt opens with two identity blocks:

```
"You are Claude Code, Anthropic's official CLI for Claude."
"You are an interactive CLI tool that helps users with software engineering tasks."
```

The identity is role-specific and precise. It does not say "helpful assistant" — it says what kind of agent and what context (CLI, software engineering). The second sentence is operational: it grounds the model in its execution environment.

**Pattern**: Open with a two-sentence identity block — role identity first, operational context second.

### 2.2 The Static/Dynamic Boundary

The prompt uses a marker `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` to split it into two halves:

- **Above the boundary**: Static behavioral instructions (identity, safety rules, coding style, tool policies). This is prompt-cached globally across all users.
- **Below the boundary**: Session-specific content (CLAUDE.md content, MCP server instructions, environment info like working directory, platform, today's date). Not cached.

The prompt is **re-sent every turn** in full, relying entirely on prompt caching to avoid token cost on the static half.

**Pattern**: Divide your system prompt into a static cacheable section (behavioral rules) and a dynamic session section (project context, env vars, date). Never mix dynamic data into the static section — it breaks caching.

### 2.3 Scope Minimalism ("Do What Was Asked")

The most-cited behavioral instruction:

> "Do what has been asked; nothing more, nothing less."

Supporting constraints:
> "Don't add features, refactor code, or make 'improvements' beyond what was asked."
> "NEVER create files unless they're absolutely necessary for achieving your goal."
> "ALWAYS prefer editing an existing file to creating a new one."
> "In general, do not propose changes to code you haven't read."

This is an explicit anti-feature-creep and anti-hallucination directive. It appears in the "Doing Tasks" section, which is always included.

**Pattern**: Include an explicit scope-bounding instruction in every agent that writes code. Agents without this instruction will spontaneously refactor, generalize, and add "improvements" the user did not request.

### 2.4 Verification Before Completion

From the "Doing Tasks" section:

> "Before reporting tasks complete, run the test, execute the script, check the output."
> "Never suppress failing tests to claim success."

This is a mandatory self-verification step — not optional and not delegated to another agent. The model must run and observe the outcome before calling finish.

**Pattern**: Add an explicit verification requirement to any agent that produces code or executes commands. "Complete" means "verified working", not "written".

### 2.5 Conciseness Instruction (Metric-Backed)

Internal engineers have the instruction:
> "Keep text between tool calls to <=25 words"

The source comment notes this was chosen over qualitative guidance like "be concise" because A/B testing showed "~1.2% output token reduction" from the quantitative constraint.

External users receive:
> "You MUST answer concisely with fewer than 4 lines of text (not including tool use or code generation), unless user asks for detail."

**Pattern**: Quantitative conciseness instructions outperform qualitative ones. Use specific line or word counts. Separate the instruction for text output from the instruction for code/tool-call output.

### 2.6 Risky Action Confirmation

> "Only take risky actions carefully, and when in doubt, ask before acting."

The "Executing Actions with Care" section lists categories requiring explicit confirmation before proceeding:
- Force-push, git reset
- File deletion
- Public API calls, external posts
- Production reads (even read-only) — added in v2.1.120 because "even read-only access pulls live credentials into the transcript"

**Pattern**: Enumerate specific categories of risky actions in the system prompt, not general warnings. Vague "be careful" is less effective than "these specific operations require user confirmation".

### 2.7 Tool Preference Over Shell

> "Do NOT use the Bash tool to run commands when a relevant dedicated tool is provided."

Claude Code instructs the model to use Read/Edit/Glob/Grep over shell equivalents, reserving shell for genuine system operations. This was later softened (v2.1.111 removed some prescriptive preferences) but the underlying principle remains: prefer the structured tool over the unstructured one when both can accomplish the task.

**Pattern**: When you have both a structured tool and a shell escape hatch, tell the model which to prefer and why.

### 2.8 System Reminders as Reinforcement

Claude Code injects `<system-reminder>` tags throughout the conversation — not just in the system prompt, but in tool results and between multi-step operations. These tags reinforce focus, re-state permission boundaries, and prevent behavioral drift over long conversations.

From the source analysis: "Claude Code sprinkles `<system-reminder>` tags everywhere to reduce drift."

The reminders include contextual content: todo list state, memory file contents, token budget warnings, hook execution results.

**Pattern**: Critical behavioral constraints that must survive multi-turn conversations should be reinforced mid-conversation via injected reminders, not relied on solely from the initial system prompt.

### 2.9 Memory: Retrieval-Only Discipline

The memory synthesis agent prompt (obtained verbatim from Piebald-AI):

> "Do not answer or solve the query yourself. You are a retrieval step, not the assistant: every fact must be lifted from a memory file body, not derived from general knowledge or your own reasoning about the query. If no memory covers it, return relevant_facts: [] and cited_memories: []."

This is explicit anti-hallucination instruction for a retrieval agent. The agent is forbidden from using its general knowledge to answer questions — only facts found in memory files are valid responses.

**Pattern**: Memory retrieval agents need an explicit "do not invent" constraint. Without it, models will fill gaps with plausible-sounding fabrications.

### 2.10 Explore Agent: Read-Only Mode with Parallel Search

The explore sub-agent opens with a hard constraint block before capability listing:

```
=== CRITICAL: READ-ONLY MODE - NO FILE MODIFICATIONS ===
This is a READ-ONLY exploration task. You are STRICTLY PROHIBITED from:
- Creating new files (no Write, touch, or file creation of any kind)
- Modifying existing files (no Edit operations)
...
```

Then adds a performance directive:

> "NOTE: You are meant to be a fast agent that returns output as quickly as possible. In order to achieve this you must... Wherever possible you should try to spawn multiple parallel tool calls for grepping and reading files."

**Pattern**: For read-only agents, lead with the constraint block before capabilities. For speed-critical agents, include explicit parallelism instruction. Both patterns are separate from the main behavioral instructions.

### 2.11 Agent Creation Prompt (Verbatim from Piebald-AI)

The agent architect prompt defines the required output format as a JSON object with three fields:

```json
{
  "identifier": "lowercase-hyphenated-name",
  "whenToUse": "Use this agent when... [with examples showing the assistant invoking the agent via the Task tool]",
  "systemPrompt": "Complete system prompt in second person ('You are...', 'You will...')"
}
```

The `whenToUse` field must include concrete examples showing the assistant using the Task tool — not just describing when to use the agent, but demonstrating the invocation pattern. This is critical for reliable delegation triggering.

**Pattern**: Agent definitions should include worked examples of the PM invoking the agent — not just capability descriptions. The examples teach the PM when and how to delegate.

### 2.12 Conversation Summarization Prompt (Verbatim Structure)

The compaction prompt explicitly requires nine named sections:

1. Primary Request and Intent
2. Key Technical Concepts
3. Files and Code Sections (with full snippets for recent edits)
4. Errors and fixes (including "specific user feedback that you received")
5. Problem Solving
6. All user messages (verbatim, not summarized)
7. Pending Tasks
8. Current Work (in detail, with file names and snippets)
9. Optional Next Step — "IMPORTANT: ensure that this step is DIRECTLY in line with the user's most recent explicit requests... This should be verbatim to ensure there's no drift in task interpretation."

The last item is particularly notable: the next-step instruction must quote the user verbatim to prevent task drift during context compaction.

**Pattern**: Compaction prompts should name and define every section explicitly. Unstructured "summarize the conversation" produces information loss. The next-step instruction must include verbatim quotes to prevent the compressed context from drifting.

### 2.13 Background Agent Autonomy Semantics

Added in v2.1.119 of the system prompt:

> "Narrate progress, restate results in message text (not just tool calls)"
> "Signal done/blocked/failed status explicitly"
> "Silence is not consent — repeated idle checks don't authorize further action"

This addresses autonomous agents operating without real-time oversight. The agent must explicitly signal its state, not just complete work silently.

**Pattern**: Background or autonomous agents need explicit state-signaling instructions. Define what "done", "blocked", and "failed" look like as explicit outputs — not just absence of error.

### 2.14 Security Monitor Pattern

The security monitor agent (added v2.1.85, expanded through v2.1.120) evaluates autonomous coding agent actions against a block/allow list before execution. Recent additions to the block list:

- "Production Reads" — even read-only shell access to production systems
- "Memory Poisoning" — writes that function as permission grants or fabricated authorization
- "Encoded/obfuscated commands" — commands must be decoded before execution is considered

The pattern is a separate judge agent, not inline safety checks. The judge operates on a deny-first basis with explicit allow exceptions.

**Pattern**: For autonomous agents, implement a separate security judge agent rather than embedding safety checks inline. The judge evaluates actions against a named block list before execution, not after. Deny-first with explicit exceptions is more reliable than allow-first with vague prohibitions.

---

## 3. Applicable Patterns for open-mpm

### 3.1 What open-mpm Does Well (Already Aligned)

Comparing Claude Code's patterns against open-mpm's current agent configs:

| Claude Code Pattern | open-mpm Status |
|---|---|
| Sub-agents return summary text only (not full conversation) | Aligned — NDJSON IPC sends `content` field only |
| Read-only agents have explicit constraint blocks | Aligned — `research-agent.toml` has "STRICT RULES (read-only)" block |
| Tiered tool preference (semantic → literal → read) | Aligned — `research-agent.toml` documents Tier 1–4 explicitly |
| `finish_task` as terminal signal | Aligned — `research-agent` uses `use_finish_task = true` |
| Separate per-role agent configs | Aligned — `.open-mpm/agents/*.toml` |
| Memory retrieval before codebase search | Aligned — `research-agent.toml` Tier 1 is `memory_recall` |

### 3.2 Gaps: What open-mpm Should Adopt

**Gap 1: PM lacks scope minimalism instruction**

`pm.toml` has no "do what was asked, nothing more" constraint. The PM can and will add scope to delegation tasks. This is the most common source of PM over-delegation (delegating a second agent to "improve" the first agent's output without being asked).

**Gap 2: PM lacks explicit task-completion criteria**

The PM's system prompt has no verification step. It routes tasks but does not define what "done" looks like before returning to the user. Claude Code's equivalent: "before reporting task complete, verify the output."

**Gap 3: No quantitative conciseness constraint**

`pm.toml` and `ctrl.toml` both lack a quantitative output length instruction. The PM produces verbose delegation explanations; the CTRL agent produces verbose status updates. Specifying "<=25 words between tool calls" and "status updates: 1-2 sentences" would reduce token cost measurably.

**Gap 4: CTRL agent lacks state-signaling vocabulary**

`ctrl.toml` has good behavioral intent ("signal blocked/done explicitly") but does not define the format of those signals. Claude Code's pattern: the agent outputs structured state tokens ("done", "blocked", "failed") that the harness can parse, not just human-readable prose.

**Gap 5: No `whenToUse` examples in agent definitions**

The TOML `[agent]` section has a `description` field but no worked delegation examples. The Claude Code agent architect pattern shows that `whenToUse` with concrete invocation examples is what triggers reliable delegation from the PM. The PM's description-based routing currently relies on keyword matching in the description, not demonstrated delegation patterns.

**Gap 6: Compaction prompt is unstructured**

open-mpm uses compaction (see `[compress]` in `engineer.toml`) but the compaction prompt (wherever it lives in Rust) likely produces unstructured summaries. The Claude Code pattern: nine named sections, verbatim user quotes for the next-step instruction, full code snippets for recent file edits.

**Gap 7: No system-reminder mid-conversation reinforcement**

The harness does not inject `<system-reminder>` style content mid-conversation. For long-running engineer agents (max_turns=40), behavioral constraints stated only at turn 0 can decay. The harness should re-inject the scope-minimalism and verification constraints at key moments (e.g., after the agent executes a shell command, after it writes a file).

---

## 4. Specific Recommendations

### Recommendation 1: Add Scope Minimalism to PM and Engineer Prompts

In `pm.toml` system_prompt, add after "## Delegation":

```
## Scope Discipline
Delegate exactly what the user asked. Do not add "improvements", enhancements, or refactors beyond the explicit request. If the user asked for X, delegate X. Do not pre-emptively delegate Y as a follow-up unless the user asked for Y.
```

In `engineer.toml` system_prompt, add after "## Core Principles":

```
### Scope: Do What Was Asked
Implement exactly what the task specifies. Do not add features, refactor unrelated code, or make "improvements" beyond the stated scope. If you identify an improvement opportunity, note it in your final summary but do not implement it unless asked.
```

### Recommendation 2: Add Verification Requirement to Engineer Prompt

In `engineer.toml` system_prompt, add a new section:

```
### Verification Before Completion
Before calling finish_task or stopping work:
- Run the tests (if any exist for the changed code).
- Execute the script or compile the binary and confirm it succeeds.
- Check the output matches the expected behavior.
Never report completion based on code appearance alone. "Works" means "runs and produces correct output."
```

### Recommendation 3: Quantify Conciseness in PM and CTRL

In `pm.toml` system_prompt:

```
## Output Style
Text between tool calls: <=25 words. Status updates: 1 sentence. Do not explain your reasoning unless asked. Delegate immediately.
```

In `ctrl.toml` system_prompt, replace the verbose status example with:

```
Status format: "Task X: [phase] [status]" — one line per task. When blocked: "BLOCKED: [task] — [specific reason] — tried: [what you tried]". Do not narrate.
```

### Recommendation 4: Add State Vocabulary to CTRL

In `ctrl.toml`, define explicit terminal states in the system_prompt:

```
## Status Tokens
When signaling task state, use these exact tokens in your output:
- `[DONE]` — task complete, evidence provided
- `[BLOCKED]` — cannot proceed without user input, specific reason follows
- `[FAILED]` — task failed after recovery attempts, postmortem follows
- `[RUNNING]` — task active, progress note follows
The harness parses these tokens. Prose-only status updates are not machine-readable.
```

### Recommendation 5: Add `whenToUse` Examples to Agent TOML Format

Add a `[agent.routing]` section to each TOML that provides delegation trigger examples. Example for `python-engineer.toml`:

```toml
[agent.routing]
when_to_use = """
Use this agent when:
- The task involves writing Python code, tests, or scripts
- The user asks for FastAPI endpoints, pytest fixtures, or pydantic models

Examples:
- User: "Add a /health endpoint to the API" → delegate_to_agent("python-engineer", "Add GET /health endpoint to the FastAPI app at src/api/main.py returning {status: ok}")
- User: "Write a test for the auth module" → delegate_to_agent("python-engineer", "Write pytest unit tests for src/auth/token.py covering the verify_token function")
"""
```

This directly addresses the PM making routing mistakes on ambiguous tasks — concrete examples outperform description matching.

### Recommendation 6: Define Structured Compaction Sections

If you have a configurable compaction prompt in the Rust harness, replace the current unstructured summary with this structure (adapted from Claude Code's conversation summarization agent):

```
Your summary MUST contain these sections in order:
1. Primary Request — what the user asked for, verbatim if possible
2. What Was Built — files created/modified, key function names, architecture decisions
3. Errors Encountered — what failed and how it was fixed
4. Test Results — test output, pass/fail counts, any failures
5. Current State — what is complete and what is not
6. Next Step — the single next action, quoted from the user's most recent explicit request

Keep each section to 2-5 sentences. Include file:line references for code changes.
```

### Recommendation 7: Add Mid-Conversation Reinforcement for Long-Running Agents

For agents with max_turns >= 20, the Rust harness should inject a brief reminder after key milestones. In `src/agents/runner.rs` or equivalent, after a file write tool result, inject:

```
<system-reminder>Scope reminder: complete only what the original task specified. Do not add features or refactors beyond the task. Verify before finishing.</system-reminder>
```

This is a low-cost intervention that significantly reduces scope creep in long sessions.

### Recommendation 8: Security Monitor for Autonomous Sessions

When CTRL spawns a PM for a long-running autonomous task (e.g., initiate_self_task), implement a lightweight security pre-check before executing shell commands that match high-risk patterns:

Block categories (from Claude Code's security monitor):
- Force-push, git reset --hard, branch -D
- rm -rf on paths outside `.open-mpm/state/`
- Any command with `prod` or `production` in path arguments
- pip/cargo install with `--break-system-packages` or `--force`

The check is a separate fast LLM call (Haiku) with a deny-first evaluation, not an inline guard in the tool handler.

---

## 5. Summary of Changes by File

| File | Change | Priority |
|---|---|---|
| `.open-mpm/agents/pm.toml` | Add scope minimalism + quantitative conciseness instruction | High |
| `.open-mpm/agents/engineer.toml` | Add verification-before-completion requirement + scope guard | High |
| `.open-mpm/agents/ctrl.toml` | Add state vocabulary tokens + quantify status format | Medium |
| All `*.toml` files | Add `[agent.routing]` with `when_to_use` examples | Medium |
| `src/agents/runner.rs` (or equivalent) | Mid-conversation scope reminder after file writes | Medium |
| Compaction prompt in Rust harness | Replace unstructured summary with 6-section structured prompt | Medium |
| `ctrl.toml` + harness | Security monitor pre-check for autonomous sessions | Low |

---

## 6. Design Principle Synthesis

Three meta-patterns emerge from analyzing Claude Code's prompt evolution across 162 versions:

**Prescriptive → Contextual**: Early versions had many "always use X tool" rules. Over time these were removed in favor of environmental branching (use Bash on Linux, PowerShell on Windows). The lesson: prescriptive tool rules break on edge cases; teach the agent the intent behind tool choice and let it adapt.

**Broad categories → Specific named threats**: Security guidance evolved from "be careful with destructive operations" to named threat categories with explicit examples. The lesson: vague safety instructions are ignored under pressure; named, specific threats with examples are consulted.

**Implicit done → Explicit state signals**: Background agents started returning prose status updates. Over time, structured state tokens ("done/blocked/failed") were added so the harness can parse them without LLM interpretation. The lesson: any multi-agent coordination that relies on natural language parsing for state management will fail at scale.

---

## Sources

- [Piebald-AI/claude-code-system-prompts](https://github.com/Piebald-AI/claude-code-system-prompts) — primary source for verbatim prompt text
- [Piebald-AI CHANGELOG.md](https://github.com/Piebald-AI/claude-code-system-prompts/blob/main/CHANGELOG.md) — 162-version prompt evolution history
- [asgeirtj/system_prompts_leaks — claude-code.md](https://github.com/asgeirtj/system_prompts_leaks/blob/main/Anthropic/claude-code.md) — extracted runtime prompt
- [How Claude Code Builds a System Prompt — dbreunig.com](https://www.dbreunig.com/2026/04/04/how-claude-code-builds-a-system-prompt.html)
- [Inside the Claude Code Source — Haseeb Qureshi](https://gist.github.com/Haseeb-Qureshi/d0dc36844c19d26303ce09b42e7188c1)
- [Peeking Under the Hood of Claude Code — OutSight AI](https://medium.com/@outsightai/peeking-under-the-hood-of-claude-code-70f5a94a9a62)
- [Dive into Claude Code — arXiv 2604.14228](https://arxiv.org/html/2604.14228v1)
- [Claude Code Source Code Leaked: What's Inside — The AI Corner](https://www.the-ai-corner.com/p/claude-code-source-code-leaked-2026)
- [Leonxlnx/claude-code-system-prompts — main system prompt](https://github.com/Leonxlnx/claude-code-system-prompts/blob/main/prompts/01_main_system_prompt.md)
