# trusty-code — System-Instruction Parity Spec

> **Status:** DRAFT — requires user sign-off before merge.
> **Issue:** [#1031](https://github.com/bobmatnyc/trusty-tools/issues/1031) (WI-12)
> **Milestone:** trusty-code article-ready model-comparison harness (#1).
> **Consumed by:** [#1032](https://github.com/bobmatnyc/trusty-tools/issues/1032)
> (prompt-assembly layer), [#1023](https://github.com/bobmatnyc/trusty-tools/issues/1023)
> (cross-provider tool-calling), [#1028](https://github.com/bobmatnyc/trusty-tools/issues/1028)
> (multi-turn agent loop).

This is a **decision document**. It defines the *parity floor*: the fixed
system-instruction surface that trusty-code assembles into **every** agent
prompt, identically across model slugs and providers, so that a cross-model
comparison measures the **model**, not differences in scaffolding. It does not
introduce code; it is the contract the assembler in #1032 implements and the
tool-calling layer in #1023 honors.

---

## 1. Purpose & the parity principle

The trusty-code harness drives the same task through many models
(`anthropic/claude-*`, `openai/gpt-*`, `qwen/*`, `deepseek/*`, `google/gemma-*`,
…) and compares the results. A comparison is only fair if the **only**
intentional variable across runs is the model itself.

**The parity principle:**

> For a given agent and task, every byte of the assembled system prompt — except
> the parts that are intrinsically model/provider-specific — **MUST be identical
> across all model slugs and providers.** Any per-model variation must be a
> deliberate, documented exception, not an accident of host configuration.

Concretely:

- The **BASE protocol preamble** (§2a) is **byte-identical** across all runs. It
  is the same constant for `claude-sonnet-4-6` and for `qwen2.5-coder`. No
  templating on model name, no provider branches.
- The **per-agent prompt** (§2b) and **tool schemas** (§2d) are derived purely
  from on-disk config and the tool registry — the same inputs for every model.
- The **only** sanctioned per-model variation is the **tool-use fallback
  guidance** (§4): extra text appended for models with weak or absent native
  function-calling. This is a documented exception with an explicit rationale,
  and the comparison report must disclose, per run, whether it was applied.

Anything that would let one model "see" different instructions than another for
the same task is a parity violation and is out of scope for the floor (§3).

This mirrors how `trusty-mpm` composes its PM prompt (a fixed-order merge of
bundled assets with a non-overridable `BASE_PM` floor — see
`crates/trusty-mpm/src/core/instruction_pipeline.rs`). The trusty-code parity
floor is the **cross-provider analogue**: where trusty-mpm guarantees prompt
consistency *across sessions*, trusty-code guarantees it *across models*.

---

## 2. In-scope — assembled into every prompt

The assembler (#1032) concatenates the following sections in this **stable,
fixed order**, joined by the section separator `\n\n---\n\n` (the same separator
`trusty-mpm` uses, for visual and diff consistency):

| Order | Section | Source | Varies per model? |
|------:|---------|--------|-------------------|
| 1 | BASE protocol preamble (§2a) | Compile-time constant | **No** (byte-identical) |
| 2 | Per-agent `system_prompt.content` (§2b) | `AgentConfig` TOML | No (same config for all models) |
| 3 | Project / `CLAUDE.md` context (§2c) | Project file, injected verbatim | No |
| 4 | Tool-use fallback guidance (§4) | Provider capability table | **Yes** (documented exception) |

Tool **schemas** (§2d) are *not* part of the system-prompt text. They travel in
the request's `tools` array (the OpenAI `tools` field on `ChatRequest`), and the
same schema set is sent to every model. They are described here because they are
part of the parity surface even though they ride a different channel.

> **Section 4 placement note:** the fallback guidance is ordered *last* in the
> system prompt so that, for the common case (native-tool-calling models), it is
> empty and the assembled prompt for those models is exactly sections 1→2→3. The
> floor for a strong model is therefore the strict prefix of the floor for a
> weak one. This is a genuine design choice — see Open Decision **D4**.

### 2a. BASE protocol preamble — *byte-identical across all models*

A single compile-time constant, the cross-provider analogue of trusty-mpm's
`BASE_PM`. It is the minimum claude-code-equivalent instruction surface that
every agent shares. It **MUST** contain exactly these blocks and nothing
host-specific:

1. **Identity & role framing** — "You are an autonomous coding agent operating
   inside the trusty-code harness." Establishes that the agent acts directly
   (via tools) rather than narrating intentions.
2. **Tool-use protocol** — the rules of engagement for calling tools, stated
   model-agnostically:
   - Tools are invoked by emitting a tool call; the harness executes it and
     returns the result before the conversation continues.
   - Call one logical step at a time and wait for the result; do not assume a
     tool's effect.
   - Tool arguments **MUST** validate against the provided JSON Schema.
   - A tool result may report an error (`is_error`); recover (retry with
     corrected arguments, choose another tool, or explain) rather than
     repeating the same failing call. (Mirrors `ToolResult { recoverable }` in
     `crates/trusty-code/src/tools/traits.rs`.)
3. **Filesystem-safety contract** — file paths are resolved relative to the
   project root and confined to it; never assume access outside the workspace.
   (Mirrors the path-confinement guard in `tools/fs/mod.rs`.)
4. **Output / answer convention** — when the task is complete, produce a final
   answer; an optional `## Summary` section is extracted downstream
   (`AgentOutput::summary`). This is the human-readable counterpart to the
   machine finish signal in §5.
5. **Finish convention pointer** — a one-line statement of how to signal
   completion to the loop (full definition in §5).

The BASE preamble **MUST NOT** contain: model names, provider names, host
paths, MCP server lists, skill catalogs, or anything that differs between two
runs of the same task. Those either belong to other sections or are excluded
(§3).

> **Naming note:** "claude-code-equivalent" means *functionally* equivalent —
> the same categories of guidance Claude Code's harness supplies (act-via-tools,
> path safety, completion signaling) — re-expressed natively. Per the milestone
> note, we **build this natively**; we do not extract or port open-mpm text.

### 2b. Per-agent `system_prompt.content`

Injected verbatim from the agent's `AgentConfig`
(`crates/trusty-code/src/agents/config.rs`): the `[system_prompt].content`
field of `<project>/.claude/agents/<name>.toml`. This is what differentiates
`engineer` from `python-engineer` from `qa`. Because it comes from on-disk
config, it is identical across models for a given agent — satisfying parity.

`system_prompt.append_skills` is **read but its expansion is excluded from the
parity floor** — see §3 (skills).

### 2c. Project / `CLAUDE.md` context injection

If the project root contains a `CLAUDE.md`, its contents are injected as
section 3. Default policy (pending **D1**):

- Injected **verbatim** (no summarization, no truncation), matching how
  trusty-mpm treats the project `CLAUDE.md` stub.
- Injected **identically for every model** (same bytes, same position).
- If absent, section 3 is empty (the separator collapses; no placeholder text).
- trusty-code **reads** an existing `CLAUDE.md` but, for the comparison harness,
  does **not** auto-create or mutate it (read-only, to keep runs reproducible).

This keeps the project context a controlled, parity-safe variable: every model
sees the same project instructions or none.

### 2d. Tool schemas — full OpenAI function format

Every registered tool contributes a schema in the canonical OpenAI
function-calling shape (`ToolDefinition` / `FunctionDefinition` in
`crates/trusty-code/src/llm/request.rs`, emitted by
`ToolExecutor::schema()` and collected by `ToolRegistry::schemas()`):

```json
{
  "type": "function",
  "function": {
    "name": "<tool_name>",
    "description": "<guidance for when/how to call>",
    "parameters": { "type": "object", "properties": { ... }, "required": [ ... ] }
  }
}
```

Parity rules for tool schemas:

- The **same set** of schemas, in a **stable order**, is sent to every model in
  the `tools` array of the request.
- Per-agent gating (`AgentConfig.tools.allowed`) filters which tools an agent
  may *call*; the **filtered set must still be identical across models** for the
  same agent (gating is a function of agent, not model).
- The baseline tool surface is the currently-registered set —
  `read_file`, `write_file`, `edit` (`tools/fs`), `bash` (`tools/bash`), and
  `delegate_to_agent` (`tools/delegate`) — plus any others registered at
  assembly time. The floor is "**whatever the registry emits**", so new tools
  inherit parity automatically.
- Schemas are model-agnostic JSON. Providers that cannot consume native tool
  schemas still receive the *same* schema set; the difference is handled by the
  fallback guidance (§4) and the extraction strategies in #1023, **not** by
  sending different schemas.

---

## 3. Out-of-scope — explicitly excluded from the parity floor

These are deliberately **not** part of the assembled parity prompt. Each would
introduce per-host or per-model variability that would confound the comparison.

| Excluded | One-line rationale |
|----------|--------------------|
| **Skills** (`system_prompt.append_skills`, skill bodies) | Skill expansion is host/catalog-dependent and would inflate the prompt unevenly; the floor measures the model, not its skill library. |
| **Hooks** (pre/post tool-call hooks, session hooks) | Hooks alter behavior out-of-band and are host-configured; including them breaks reproducibility across environments. |
| **MCP servers** (live MCP tool catalogs, e.g. trusty-search / trusty-memory) | MCP availability is environment-specific and stateful; the harness uses only the in-process tool registry so every run sees the identical tool surface. |
| **Host-specific settings** (`.claude/settings*.json`, env-var-driven toggles, model defaults, API keys) | These describe the *operator's* machine, not the task; they must never leak into a prompt that is supposed to be identical across machines and models. |
| **Per-session runtime state** (history from prior tasks, scratch context) | Each comparison run starts from the same clean system prompt + task; carried-over state would make runs non-comparable. |

Exclusion does **not** mean "unused" — skills, hooks, and MCP remain available
in production trusty-code. They are simply **outside the parity floor** used for
the model-comparison harness.

---

## 4. Provider fallback guidance (policy statement)

Some models have weak or absent native function-calling (the request's
`tool_calls` channel). For those, the assembler appends a **fallback guidance**
block (section 4) that teaches the model to emit tool calls in a text format
the extractor in #1023 can parse. This is the **single sanctioned per-model
variation** in the floor.

Policy (not code — #1032 implements the assembler, #1023 the extractor):

- **Capability tiers** drive whether the block is appended:
  - **Native** (e.g. current Anthropic/OpenAI tool-calling): **no** fallback
    block. Section 4 is empty; the prompt is sections 1→2→3 only.
  - **Text/JSON fallback** (e.g. Qwen, DeepSeek, Gemma per #1023's matrix): the
    block is appended, instructing the model to emit each tool call as a fenced
    JSON object (and, per the model's matrix entry, the angle-bracket
    `<tool_call>{…}</tool_call>` form) whose shape matches the tool's `parameters`
    schema.
- The fallback block **only** adds *format* guidance for emitting calls. It does
  **not** change the BASE protocol rules (§2a), the agent prompt (§2b), the
  project context (§2c), or the tool **schemas** (§2d) — those remain identical.
  Two models in the same tier receive the **same** fallback text.
- The fallback text is a **versioned constant per tier**, selected by the
  provider-capability table, so it is reproducible and disclosable. The
  comparison report **MUST** record, per run, which tier (and thus which
  fallback variant, if any) was applied.
- Validation and the bounded repair loop are **identical** across all tiers
  (#1023): regardless of how a call was emitted, its arguments are validated
  against the same JSON Schema and repaired through the same loop. Only the
  *extraction strategy* differs by provider.

The exact tier→text mapping is owned by #1023's per-model strategy matrix; this
spec fixes only the **policy**: fallback adds emission-format guidance, nothing
else, and its application is disclosed.

---

## 5. Finish convention

The multi-turn loop (#1028) iterates *"until final / limits"*. To terminate
**deterministically** (rather than guessing from prose), the loop needs an
unambiguous completion signal that works across all providers.

Decision (pending **D3**): an agent signals completion by **producing an
assistant turn that contains no tool call** (a pure final-answer turn). The loop
treats "model returned content with zero tool calls" as the terminal state and
returns that content as the `AgentOutput`.

Rationale:

- It is **provider-agnostic**: every model can produce a no-tool-call turn,
  including fallback-tier models (the extractor finds zero tool calls). It does
  not depend on a reserved tool name or sentinel string that a weak model might
  fail to emit.
- It is the natural OpenAI-style loop contract and matches the BASE output
  convention (§2a item 4): the final turn is the answer.

Loop termination is therefore the **disjunction**:

1. **Finish** — assistant turn with no tool call → return final answer
   (success). The optional `## Summary` section is extracted into
   `AgentOutput.summary`.
2. **Limit** — turn cap, wall-clock timeout, or token/cost budget exceeded
   (#1028) → abort and return the **partial transcript** (the run is marked
   incomplete, not silently truncated).

A reserved `finish`/`attempt_completion` tool was considered and rejected for
the floor because it would burden fallback-tier models with one more emission
format to get right — increasing variance in exactly the place parity must
protect. (Revisit under **D3** if a structured payload at finish proves
necessary.)

---

## 6. Open decisions for the user

These are genuine product/design choices the parity floor depends on. Please
confirm or override each; defaults reflect the draft above.

- **D1 — `CLAUDE.md` injection: verbatim vs. summarized?**
  Draft default: **verbatim, identical for every model, read-only** (no
  auto-create/mutate). Alternative: inject a length-capped summary to keep the
  floor small. *Verbatim maximizes parity fidelity; summarizing risks a
  non-reproducible, model-confounding step.* **Confirm verbatim?**

- **D2 — Cap the tool-schema count / total schema bytes?**
  Draft default: **no cap** — send the full registered (then agent-gated) set.
  A cap would help models with small context windows but means different models
  could see different tool sets, breaking parity. **Keep "no cap, full set"?**

- **D3 — Finish signal: no-tool-call turn vs. explicit `finish` tool?**
  Draft default: **no-tool-call turn** (§5). Choose explicit `finish` only if
  you want a structured completion payload (e.g. a self-reported success flag).
  **Confirm no-tool-call turn?**

- **D4 — Fallback guidance placement: last (prefix-compatible) vs. adjacent to
  the tool-use protocol?**
  Draft default: **last**, so a strong model's prompt is the strict prefix of a
  weak model's (clean diff, easy disclosure). Alternative: place it right after
  §2a item 2 for locality. **Confirm last?**

- **D5 — Does the parity report disclose the exact assembled prompt per run?**
  Draft default: **yes** — record the BASE version, the agent prompt hash, the
  `CLAUDE.md` hash, the tool-schema set hash, and the fallback tier, so any
  parity claim is auditable. **Confirm full disclosure in the report?**

Once these are signed off, #1032 implements the assembler to this contract and
#1023 supplies the per-tier fallback text and extraction matrix.
