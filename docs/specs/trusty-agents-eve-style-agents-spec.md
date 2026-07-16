# DOC-37 — Eve-Style Agent Framework for trusty-agents

**Status:** Draft
**Subsystem:** trusty-agents — agent definition / runtime / tool-calling / memory
**Owner:** Engineering (trusty-agents)
**Last-updated:** 2026-07-15
**Spec ID:** `SPEC-AGENTFW-01~draft` … `SPEC-AGENTFW-06~draft` (DOC-37)
**Builds on:** the existing `.toml` and `.md`+YAML-frontmatter agent-definition
loaders (`crates/trusty-agents/src/agents/registry/mod.rs`,
`crates/trusty-agents/src/agents/registry/md_agent.rs`,
`crates/trusty-agents/src/agents/loader.rs`), the workflow engine split
(#171/#172), the ctrl-plane split (#170), the trusty-memory Palace integration
(issue #379, `crates/trusty-agents/src/memory/trusty_backed.rs`), the
concurrency-safe state writer (issue #198,
`crates/trusty-agents/src/state_writer.rs`), and the planned unified
inference-provider adapter layer (fireworks.ai + more, landing in
`trusty-common`).
**Cross-ref:** `crates/trusty-agents/src/workflow/engine/executor/run.rs`,
`crates/trusty-agents/src/workflow/engine/state.rs`,
`crates/trusty-agents/src/workflow/config/mod.rs`,
`crates/trusty-agents/src/perf/mod.rs`,
`crates/trusty-agents-common/src/perf.rs`,
`crates/trusty-agents/src/tools/delegate.rs`,
`crates/trusty-agents-common/src/runner.rs`,
`crates/trusty-agents-common/src/lib.rs`,
`crates/trusty-agents/src/mcp/mod.rs`, `crates/trusty-agents/src/mcp/config/`,
`crates/trusty-agents/src/rpc/mod.rs`,
`crates/trusty-agents/src/tools/mcp_service_tools.rs`,
`crates/trusty-common/src/mcp/service.rs`,
`crates/trusty-agents/src/memory/store.rs`,
`crates/trusty-agents/src/memory/trusty_backed.rs`,
`crates/trusty-agents/src/events.rs`,
`crates/trusty-agents/src/api/server/events_sse.rs`,
`crates/trusty-agents/src/agents/config.rs`, `crates/trusty-agents/src/agents/model.rs`,
`crates/trusty-agents/src/env_compat.rs`.
**Not to be confused with:** trusty-mpm's PM → sub-agent delegation model,
which is a different product (a Claude Code harness orchestrator, not a
standalone agent runtime). This spec is scoped entirely to **trusty-agents**
(bin `tagent`) as a standalone, separately-installable, non-coding
personal-productivity agent product competing with OpenClaw-class frameworks.

> **Scope note (v2 rewrite).** The v1 revision of this document (merged as
> PR #2792) was a comparative review of Vercel's Eve agent framework plus
> design *directions* for a Rust equivalent — not a spec. Bob rejected it:
> "the eve research is NOT a spec, it refers to one but doesn't actually
> contain one." This revision makes every `SPEC-AGENTFW-NN` section
> **normative and implementable without further design work**: exact
> frontmatter schemas, an explicit run-state machine, an exact on-disk
> checkpoint format, exact trait/struct signatures, an exact config-key
> table, and testable **Conformance** bullets per section, matching the
> fidelity bar set by [DOC-29](./mpm-behavior-conformance.md) and
> [DOC-36](./tm-manager-vision.md). The Eve review and the comparative gap
> analysis are retained as background (§7–§8) but are no longer the body of
> the document. Every module/type/file cited below was re-read against
> `origin/main` @ `78868a62` (2026-07-15) for this revision; anything that
> does not exist in the current tree is explicitly marked **NEW**.

---

## 1. How to read this document

Sections §2–§7 are the normative spec, one `SPEC-AGENTFW-NN` per section.
Each follows the same shape:

- **Current state** — what exists today, with `file:line` citations, verified
  against the tree at authoring time.
- **Normative requirement** — the exact schema/struct/algorithm to implement.
  Anything not already in the tree is marked **NEW**.
- **Conformance** — testable bullets an implementer/QA can check off, in the
  spirit of [DOC-29](./mpm-behavior-conformance.md)'s per-behavior evidence
  rows.

§9 is the phased roadmap, §10 explicit non-goals, §11 the owner-decision
checklist (which SPEC sections stay `~draft` pending Bob's call), §12–§13 the
demoted Eve review and gap-analysis background, §14 the change log.

---

## 2. SPEC-AGENTFW-01 — Agent Definition Format

### 2.1 Current state

trusty-agents has **two independent agent-loading code paths** today, not
one:

1. **`AgentRegistry::load(search_paths: &[PathBuf])`**
   (`crates/trusty-agents/src/agents/registry/mod.rs:70-114`) — scans each
   directory in priority order with a **flat, single-level** `read_dir`
   (no recursion into subdirectories), parsing `*.toml` via `AgentConfig::load`
   and `*.md` via `parse_md_agent`. First occurrence of a given agent name
   wins (`agents.contains_key(&name)` guard, line 96); later directories are
   shadowed. Used for capability-scored `best_match` lookup and roster
   rendering — it does **not** see directory-package agents (below), because
   a directory has no `.toml`/`.extension()` match and falls through `_ =>
   continue` (line 91).
2. **`AgentConfig::by_name(name: &str)`**
   (`crates/trusty-agents/src/agents/loader.rs:48-56`) — resolves a single
   agent by short name for direct dispatch (subprocess re-invocation,
   `delegate_to_agent`). Per issue #482, it **prefers the directory-package
   format** `<agents_dir>/<name>/agent.toml` + `<name>/persona.md` (+ optional
   `<name>/skills.md`, appended with a `\n\n---\n\n` separator) when
   `<agents_dir>/<name>/` is a directory (`load_agent_package`,
   `loader.rs:321-341`), falling back to the flat `<agents_dir>/<name>.toml`
   otherwise. `agents_dir()` resolves via `TAGENT_CONFIG_DIR` when set, else
   CWD-relative `.trusty-agents/agents/` (with a warn log on fallback).

Both directory-package and flat-`.toml` agents are real and in use today —
e.g. `crates/trusty-agents/.trusty-agents/agents/{ctrl,izzie,cto-assistant}/`
(package format: `agent.toml` + `persona.md`) alongside 15+ flat `<name>.toml`
files in the same `agents/` directory.

**`.md`+YAML-frontmatter format** (`parse_md_agent`,
`crates/trusty-agents/src/agents/registry/md_agent.rs:58-153`) parses this
frontmatter shape (`MdAgentFrontmatter`, lines 34-44):

```rust
#[derive(Debug, Deserialize, Default)]
struct MdAgentFrontmatter {
    name: Option<String>,
    role: Option<String>,
    model: Option<String>,
    description: Option<String>,
    #[serde(default)]
    runner: Option<String>,           // "claude-code" | "inline" | "subprocess" (default) | "in-process"
    #[serde(default)]
    capabilities: Option<AgentCapabilities>,
}
```

Unknown YAML keys are silently ignored (no `deny_unknown_fields`) so richer
claude-mpm frontmatter doesn't fail parsing. Critically, **`parse_md_agent`
unconditionally sets `tools: ToolsConfig::default()`** (line 145, i.e.
`allowed: None`, `allow: None`) regardless of frontmatter content — `.md`
agents cannot declare a tool allowlist today even though **TOML agents
already can**, via the existing `[tools]` section on `AgentConfig`
(`crates/trusty-agents/src/agents/config.rs:118-172`):

```rust
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolsConfig {
    pub allowed: Option<Vec<String>>,       // exact tool-name allowlist
    pub allow: Option<Vec<String>>,          // glob patterns (#255), e.g. "mcp_*"
    pub native: NativeToolsConfig,           // opt-in native-tool flags (#133)
    pub ast_native: Option<bool>,            // shorthand for native.ast_native (#347)
    // + OpenRPC scope patterns (#455): memory.read, search.read, google.gmail.*
}
```

There is **no `extends`, `subagents`, or `memory` field anywhere** in either
loader today — confirmed by exhaustive grep across
`crates/trusty-agents/`, `crates/trusty-agents-common/`, and
`crates/trusty-agents-local/` (zero hits for `extends` as an
agent-inheritance concept; the only `extends`-substring hits are unrelated
doc-comment prose in `state_writer.rs` and `runner.rs`). Composition/handoff
declaration in an agent's own file is a **genuine gap**, not a
misunderstanding of existing code — this section defines it as **NEW**.

### 2.2 Normative requirement

**NEW field 1 — `extends: Option<String>`** on both `MdAgentFrontmatter` and
the TOML `[agent]` table (`AgentInfo`, `config.rs:287-`). Single-parent
inheritance by agent name.

*Resolution algorithm* (applies identically inside `AgentRegistry::load` and
`AgentConfig::by_name`/`load_agent_package`):

1. When `extends` is present, resolve the named base agent **first**,
   recursing through *its* own `extends` (if any), before applying the
   current agent's own fields as overrides.
2. Merge rules, per field:
   - Scalars (`model`, `description`, `role`, `runner`) — child overrides
     when present in the child's own file; otherwise inherits the resolved
     parent's value.
   - `system_prompt.content` — **child replaces parent by default.** An agent
     that wants to retain the parent's prose opts in explicitly by including
     the literal template token `{{extends_prompt}}` in its own body, which
     is substituted with the fully-resolved parent prompt before the
     `AgentConfig` is handed to the workflow engine. (Silent concatenation
     across an arbitrarily deep chain risks unbounded, un-auditable prompt
     growth — an explicit opt-in token keeps composition visible in the
     child's own file.)
   - `tools.allowed` / `tools.allow` — union (child ∪ parent), de-duplicated
     by exact string. A child may additionally narrow via **NEW**
     `tools.deny: Option<Vec<String>>`, subtracted from the union after
     merging.
   - `capabilities.{languages,frameworks,roles,tags}` — union, de-duplicated.
   - `subagents.allowed` (below) — union.
3. Error cases (structured load errors, never a panic):
   - **`ExtendsTargetNotFound { agent, base }`** — `extends` names an agent
     absent from the same search path / `agents_dir()`. Message lists
     available agent names, mirroring the existing "Unknown agent" pattern in
     `DelegateToAgentTool::execute` (`tools/delegate.rs:150-155`).
   - **`ExtendsCycle { chain: Vec<String> }`** — resolution walks a
     visited-name `HashSet<String>` top-down from the requested agent;
     re-encountering a name already in the set raises this error naming the
     full chain.
   - **`ExtendsTooDeep { agent, depth: 8 }`** — resolution aborts once the
     chain exceeds **8** levels, independent of cycle detection (guards
     against very long non-cyclic chains). Rationale: no known trusty-agents
     persona/role hierarchy needs more than 2–3 levels; 8 is a generous
     ceiling that still bounds worst-case load time. Not runtime-tunable
     (compile-time constant — a safety ceiling, not an operator preference).

**NEW field 2 — `tools: Vec<String>`** on `MdAgentFrontmatter` only (TOML
agents already have equivalent capability via `[tools] allowed`). Populates
`ToolsConfig.allowed` exactly as the TOML path already does, closing the
`md_agent.rs:145` gap described above. Purely additive: absent `tools:` still
yields `ToolsConfig::default()` (unrestricted), identical to today.

**NEW field 3 — `subagents: Vec<String>`** on both `MdAgentFrontmatter` and a
**NEW** `[subagents]` TOML table (mirroring `[tools]`'s shape:
`allowed: Option<Vec<String>>`, default `None`). Declares which agent names
*this* agent's own `delegate_to_agent` tool call may target. Wired into
`DelegateToAgentTool::execute` (`tools/delegate.rs:141-157`) as a **second,
narrower** check, additive to the existing "does the file exist at all"
validation: when the *calling* agent's own `AgentConfig.subagents.allowed` is
`Some(list)`, pre-flight validation restricts to that list; `None` (the
default, and the only behavior that exists today) preserves current
unrestricted-by-declaration behavior exactly.

**NEW field 4 — `memory:`** — see §6 (SPEC-AGENTFW-06).

### 2.3 Directory-convention layout (worked example)

Extends the **real, existing** #482 package format — no new directory
convention is invented:

```
.trusty-agents/agents/
  engineer.toml                      # flat format — AgentConfig::load / AgentRegistry::load
  billing-assistant/                 # directory-package format (#482) — AgentConfig::by_name
    agent.toml                       # [agent] extends="engineer" (NEW) + [subagents] allowed=[...] (NEW)
    persona.md                       # system_prompt.content (existing #482 behavior)
    skills.md                        # optional, appended after persona.md (existing #482 behavior)
  escalation-agent/
    agent.toml
    persona.md
```

`billing-assistant/agent.toml`:

```toml
[agent]
name = "billing-assistant"
role = "subagent"
extends = "engineer"                 # NEW — inherits engineer's model/tools/capabilities
description = "Handles billing and refund queries"

[tools]
allowed = ["search_orders", "issue_refund"]   # unioned with engineer's own tools.allowed

[subagents]                                    # NEW table
allowed = ["escalation-agent"]
```

### 2.4 Conformance

- `extends` resolves scalar override + list-union merge rules exactly as
  specified in §2.2 for a 2-level chain; a 3rd-party test fixture with a
  9-level chain is rejected with `ExtendsTooDeep`.
- A 2-cycle (`a extends b`, `b extends a`) is rejected with `ExtendsCycle`
  naming both agent names in `chain`.
- An `.md` agent with `tools: [...]` in frontmatter produces a populated
  `ToolsConfig.allowed` (not `ToolsConfig::default()`).
- `delegate_to_agent` from an agent whose `subagents.allowed = Some(["x"])`
  rejects a call targeting agent `"y"` even when `y.toml` exists on disk;
  an agent with `subagents.allowed = None` is unaffected (regression guard
  against the three existing tests in `tools/delegate.rs`'s own test module).

---

## 3. SPEC-AGENTFW-02 — Runtime State Machine & Checkpoint Journal

### 3.1 Current state

`WorkflowDef.phases: Vec<PhaseDef>`
(`crates/trusty-agents/src/workflow/config/mod.rs:110-193`), loaded from
`{config_dir}/{name}.json`. Each `PhaseDef` carries: `name`, `agent`,
optional `model`, `context_template`, `produces_files: Option<bool>`,
`parallel_subtasks: Option<Vec<ParallelSubtask>>`,
`worktree_protection: Option<bool>`, `skip: Option<bool>`,
`skills: Option<Vec<String>>`, `ast_native: Option<bool>`.

**There is no explicit state-machine enum.** Execution is a single imperative
`'phase_loop: for phase in &def.phases` loop inside
`WorkflowEngine::run_with_perf_and_dirs`
(`crates/trusty-agents/src/workflow/engine/executor/run.rs:149-465`). Per
phase: skip-check (`phase_should_skip`) → AST-native guard → progress-start
event (`emit_progress_event`) → `assemble_phase_prompt` → `dispatch_phase` →
on `Err`: `record_phase_failure` + `break 'phase_loop` (lines 290-301); on
`Ok`: perf record, ticket comment, `handle_qa_envelope` (may set
`qa_retry_count`/`qa_failure_feedback`, gating a **same-phase** one-shot QA
retry — the retry re-enters the code phase, not a new phase; important for
checkpoint granularity below), history-indexer record, `ctx.record_phase`,
goal-block parse, plan-output relocation, code-phase reconciliation, file
extraction.

The loop's exit (success or `break`) hands off to `finalize_run`
(`executor/finalize.rs`). **`perf.flush(perf_dir)`
(`finalize.rs:111-112`) is the only on-disk persistence of run state, and it
runs exactly once, at the very end**, regardless of success or failure.
**There is zero on-disk state during an in-flight run today** — a process
crash mid-loop loses the entire in-memory `WorkflowContext`, perf
accumulator, and session-manager state, with nothing recoverable.

Two existing on-disk conventions must not be conflated:

- `docs/performance/runs/<stamp>.json` + `runs.log`
  (`crates/trusty-agents/src/perf/mod.rs:254-263`, schema in
  `crates/trusty-agents-common/src/perf.rs:111-171`, `PerfRecord`/
  `PhaseRecord`/`PerfTotals`/`TokenUsage`) — a **developer-facing analytics
  artifact**, `out_dir`-relative (typically `<cwd>/docs/performance`),
  written via plain `tokio::fs::write` (not the atomic-write primitive
  below). This stays as-is; the new checkpoint journal is **not** colocated
  here.
- `.trusty-agents/state/` — the **project-relative runtime-state root**
  already holding `build.json`, `sessions.json`,
  `mistakes/<session_id>.jsonl`, `interactions/<session_id>.jsonl`, and
  `runs.jsonl` (confirmed via `build_info.rs`, `mistake_log.rs`,
  `interaction_log.rs`, `process_tracker.rs`, `state_writer.rs`). This is
  where the new checkpoint journal belongs.

**Existing atomic-write primitive to reuse verbatim** —
`crate::state_writer::atomic_write(path: &Path, contents: &[u8]) -> Result<()>`
(`crates/trusty-agents/src/state_writer.rs:80-117`): acquires an advisory
exclusive lock on a sibling `<path>.lock` file (via `fs4`), writes to
`<path>.tmp`, `sync_all()` (best-effort fsync), then `fs::rename`s into
place. Already battle-tested by `atomic_write_is_crash_safe` and
`concurrent_writes_no_corruption` (10 concurrent writer threads, zero
corruption). **No new I/O primitive is introduced by this spec.**

### 3.2 Normative requirement — `RunState` (NEW)

```rust
// NEW — crates/trusty-agents/src/workflow/engine/checkpoint.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Pending,
    PhaseRunning { phase_index: usize },
    PhaseComplete { phase_index: usize },
    Retrying { phase_index: usize, attempt: u32 },
    Failed { phase_index: usize },
    Done,
}
```

Transitions:

- `Pending -> PhaseRunning{0}` on entering the first non-skipped phase.
- `PhaseRunning{i} -> PhaseComplete{i}` immediately after `ctx.record_phase`
  for phase `i` (the last thing that happens for phase `i` before the loop
  advances) — the checkpoint write for `PhaseComplete{i}` is the very last
  step of iteration `i`, so a resumed run never re-executes a phase already
  marked complete.
- `PhaseComplete{i} -> PhaseRunning{i+1}` on loop advance (honoring
  `phase_should_skip`).
- `PhaseRunning{i} -> Retrying{i,1}` when `handle_qa_envelope` sets
  `qa_retry_count = 1` (the existing "Fix 2" one-shot code-phase retry
  triggered by a `qa`-phase failure signal, `run.rs:129-133`/`384-395`) —
  this is the one existing case where the loop re-enters an
  already-dispatched phase; `attempt` makes it observable/resumable.
- `Retrying{i,1} -> PhaseComplete{i}` or `-> Failed{i}` — same as non-retry.
- `PhaseRunning{i} -> Failed{i}` on `dispatch_phase` `Err`
  (`record_phase_failure`, `run.rs:292-301`).
- `PhaseComplete{last} -> Done` at `finalize_run` entry.
- `Failed{i}` and `Done` are terminal for the journal.

### 3.3 Normative requirement — checkpoint journal

**Location:** `.trusty-agents/state/runs/<run_id>/checkpoint.json`
(project-relative, matching the existing `.trusty-agents/state/` root).
`run_id` reuses the **existing** `TAGENT_RUN_ID`/`OPEN_MPM_RUN_ID` value
already threaded through `emit_progress_event` and `HistoryIndexer`
(`run.rs:399-400`, `state.rs:82`) — no new ID scheme.

```rust
// NEW — crates/trusty-agents/src/workflow/engine/checkpoint.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRecord {
    pub run_id: String,
    pub workflow: String,                       // WorkflowDef.name
    pub state: RunState,
    pub phase_names: Vec<String>,                // snapshot of def.phases[..].name at Pending — diagnostic only, see §3.4
    pub out_dir: PathBuf,
    pub code_dir: PathBuf,
    pub task: String,                             // ORIGINAL (uncleaned) task text
    pub phase_outputs: std::collections::BTreeMap<String, String>,
    pub goal_block: Option<crate::context::goals::GoalBlock>,
    pub qa_retry_count: u32,
    pub qa_failure_feedback: Option<String>,
    pub started_at: String,                        // ISO8601, same convention as PerfCollector::started_at_iso
    pub updated_at: String,                         // ISO8601, refreshed every write
}
```

**Write timing:** exactly at the `PhaseComplete{i}` / `Failed{i}` /
`Retrying{i,_}` transitions — once per phase boundary, **not** per LLM
turn/tool-call within a phase. Phase-level granularity is the deliberate MVP
boundary (see §10 non-goals). Write call:
`state_writer::atomic_write(&checkpoint_path, &serde_json::to_vec_pretty(&record)?)`
— the existing primitive, unmodified.

**Deletion:** on `RunState::Done`, `finalize_run` removes
`.trusty-agents/state/runs/<run_id>/` entirely
(`std::fs::remove_dir_all`, best-effort — logged on failure, non-fatal,
matching the existing "log and continue" pattern already used throughout
`finalize.rs` for ticket-manager/auto-push hooks). A `Failed` checkpoint is
**intentionally** left on disk — it is the resumable artifact.

### 3.4 Normative requirement — `tagent resume <run_id>`

- Resolves `.trusty-agents/state/runs/<run_id>/checkpoint.json`; missing file
  → error listing the run IDs that do have one (`ls
  .trusty-agents/state/runs/`).
- Reloads `WorkflowDef` **fresh** from `{config_dir}/{workflow}.json` — not
  from the snapshotted `phase_names`, which is diagnostic/display only.
  **If the on-disk workflow JSON's phase list no longer matches
  `phase_names` at the recorded index, resume MUST fail closed** with
  `WorkflowError::ResumeDefinitionChanged` rather than silently running a
  different pipeline than the one that was checkpointed.
- Resumes the `'phase_loop` immediately **after** `phase_index` for
  `PhaseComplete`/`Done`, or **at** `phase_index` for `Failed`/`Retrying`
  (the failed/in-flight phase re-runs from scratch — no sub-phase resume).
- **Replayed:** every phase from the resume point onward re-executes in full
  (fresh LLM call, fresh `dispatch_phase`), including a phase that failed.
  **Not replayed:** any already-`PhaseComplete` phase's LLM/tool calls — its
  `phase_outputs` entry is reused verbatim. No duplicate `PhaseDone` events
  fire for already-complete phases; a single **NEW** `Event::RunResumed
  { session_id, resumed_at_phase }` fires once at resume start (see §5.3).

### 3.5 Failure matrix

| Failure | Behavior |
|---|---|
| Process crash mid-phase (before the phase-boundary checkpoint write) | Checkpoint reflects the last `PhaseComplete` (or `Pending` if the crash was during phase 0). Resume re-runs the in-flight phase from scratch. At most one phase's work is lost, never more. |
| Checkpoint file present but corrupt (unparseable JSON / schema mismatch) | `tagent resume` fails closed with `WorkflowError::CheckpointCorrupt { path, source }` — does **not** silently start a new run under the same `run_id` (risk of clobbering partial `out_dir`/`code_dir` state). Operator fixes the file or starts a fresh run (new `run_id`). |
| Workspace (`out_dir`/`code_dir`) moved or deleted between checkpoint and resume | `resolve_dirs` re-validates both paths exactly as for a fresh run; a missing `out_dir` is recreated (existing #126/#153/#222 behavior). A missing `code_dir` with partial generated files is **not** treated as data loss by the framework — a known MVP limitation (see §11 owner-decision checklist). |
| Two `tagent resume` invocations racing on the same `run_id` | The checkpoint file is protected by the same `state_writer` advisory lock as every other `.trusty-agents/state/` file — a second resume attempting a phase-boundary write while the first holds the lock **blocks**, does not corrupt. The framework does **not** add a separate "run already resumed" mutex; concurrent resume of the same `run_id` beyond "the journal won't corrupt" is flagged as a real gap, not solved here. |

### 3.6 Conformance

- A synthetic 3-phase workflow killed (`SIGKILL`) mid-phase-2 leaves a
  checkpoint at `PhaseComplete{0}` or `PhaseRunning{1}`/`Failed{1}` (never
  claims phase 2 complete); `tagent resume` re-runs phase 1 (0-indexed) or 2
  and does not re-invoke the phase-0 agent.
- A hand-corrupted `checkpoint.json` (truncated JSON) causes `tagent resume`
  to exit non-zero with `CheckpointCorrupt`, not a panic, and not a silent
  fresh-run fallback.
- A completed (`Done`) run leaves no `.trusty-agents/state/runs/<run_id>/`
  directory on disk afterward.
- `atomic_write` is the only write path exercised — no direct
  `tokio::fs::write`/`OpenOptions` call is added for the checkpoint file.

---

## 4. SPEC-AGENTFW-03 — Unified Tool-Calling, MCP Mapping & Credential Brokering

### 4.1 Current state — correcting the v1 draft's framing

The v1 draft claimed trusty-agents needed a new unifying `ToolInvoker` trait
because MCP tool-calling was "split" between client consumption and
self-hosted exposure. Re-reading the actual code shows this framing was
**wrong on the client side**: a single unifying trait already exists and
external MCP tools already flow through it.

**`ToolExecutor`** (`crates/trusty-agents-common/src/lib.rs:276-313`) is
already the one trait every callable tool implements — native tools, the
five `mcp_*` management tools, and (critically) every tool advertised by an
enabled external MCP service:

```rust
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> Value;
    async fn execute(&self, args: Value) -> ToolResult;
    fn restricted_tiers(&self) -> &[ServiceTier] { &[] }
    fn execution_tier(&self) -> ToolExecutionTier { ToolExecutionTier::OnDemand }
}
```

```rust
#[derive(Debug)]
pub enum ToolResult {
    Success(String),
    Error { message: String, recoverable: bool },
}
```

**External MCP tool-calling is already live**, not a declarative-only
registry as the v1 draft asserted.
`crates/trusty-agents/src/tools/mcp_service_tools.rs:194-197`
(`mcp_service_tool_executors`) reads `GlobalConfig::load()`
(`crates/trusty-agents/src/mcp/mod.rs`), walks each **enabled** service
(`GlobalConfig.mcp.services: Vec<McpService>`,
`crates/trusty-agents/src/mcp/config/types.rs:148-163` — `name`,
`description`, `command`, `args`, optional `url`, `transport: "stdio"|"http"`,
`enabled`), and builds one `McpServiceTool` (lines 95-145) — a real
`ToolExecutor` impl — per advertised tool. `ServiceClient::get_or_spawn`
(lines 36-81) lazily spawns a `StdioMcpClient` (cached per-service via
`OnceCell<Arc<Mutex<StdioMcpClient>>>`), and `McpServiceTool::execute`
(lines 111-144) forwards to `client.call_tool(&self.name, args)`, rendering
MCP's `{content:[{type:"text",text:...}]}` response into a flat string
(`format_mcp_call_result`, lines 155-181). Failures (server not on `PATH`,
handshake failure, JSON-RPC error) surface as `ToolResult::err(...)` — the
existing graceful-degradation pattern, not a panic.

Two real, narrow gaps remain, **not** "unify two parallel paths":

1. **`build_executors_from_services` is stdio-only.** Non-`"stdio"`
   `transport` values are explicitly skipped (not errored) — "skipping
   non-stdio MCP service (only stdio transport supported)". HTTP-transport
   MCP servers are declarable in config (`McpService.url`) but never
   actually callable.
2. **`rpc/mod.rs`'s self-hosted `POST /rpc` endpoint only implements
   `rpc.discover`.** `rpc_handler` (`crates/trusty-agents/src/rpc/mod.rs:57-75`)
   returns `-32601 Method not found` for anything but `rpc.discover`.
   `build_unified_discovery` (lines 41-46) merges `ServiceDescriptor` impls
   for `MemoryMcpService`/`SearchMcpService` (from `trusty-memory`/
   `trusty-search`) into one OpenRPC document via
   `trusty_common::mcp::openrpc::OpenRpcBuilder`, tagging every method with
   `x-service`. Actually **executing** a discovered method (`tools/call`) is
   an explicit, named TODO in the file (lines 77-81, issue #460) — this
   endpoint serves *external* callers looking to invoke trusty-agents' own
   hosted services, and today it can only tell them what's callable, not
   call it.

**Important constraint discovered this pass:** `ServiceDescriptor`
(`crates/trusty-common/src/mcp/service.rs:26-44`) has **no execute/call
method at all** — only `name()`, `version()`, `tools()`, `scopes_for()`. It
is a pure discovery/metadata trait. Closing gap 2 is therefore **not** "add a
match arm" — it requires a genuinely new dispatch layer that calls into
`trusty-memory`'s and `trusty-search`'s own tool-execution entry points
directly (not through `ServiceDescriptor`), which this pass could not fully
verify the shape of. Flagged explicitly in §11 rather than asserting an
unverified signature.

No credential-brokering exists anywhere: `McpServiceTool::execute`
(lines 131-132) forwards `args` verbatim to `client.call_tool()`; there is no
secret-reference indirection on `McpService` or `GlobalConfig` today. This
**is** a genuine gap, confirmed absent, and the one Eve-derived security
property (§12) worth adding.

### 4.2 Normative requirement

**No new `ToolInvoker` trait.** `ToolExecutor` is retained as the single
unifying interface; every requirement below produces a `ToolExecutor` impl,
not a parallel abstraction.

1. **`tools/call` proxying for self-hosted services (closes issue #460
   TODO).** `POST /rpc` gains a `tools/call` method: request
   `{"jsonrpc":"2.0","id":<id>,"method":"tools/call","params":{"name":<tool
   name>,"arguments":<object>}}`. Dispatch routes on the tool's `x-service`
   annotation (already present on every merged method,
   `build_unified_discovery`) to a **NEW** per-service execution adapter —
   the exact call surface into `trusty-memory`'s/`trusty-search`'s own tool
   runners is an implementation-time verification item (§11), not asserted
   here. Error shape: standard JSON-RPC 2.0; unknown tool name →
   `{"code":-32602,"message":"Invalid params"}`; unknown method still
   `-32601` (existing behavior, `rpc/mod.rs:66-73`, unchanged).
2. **HTTP-transport MCP client support (closes the stdio-only gap).** **NEW**
   `HttpMcpClient` with the same `call_tool(name: &str, args: Value) ->
   Result<Value>` shape `StdioMcpClient` already exposes, so
   `McpServiceTool::execute` needs **no changes** — only
   `ServiceClient::get_or_spawn`'s match on `transport` gains an `"http"` arm
   constructing the client from `McpService.url`.
3. **Credential brokering (genuinely new).** `McpService` gains **NEW**
   `credential_ref: Option<String>`
   (`crates/trusty-agents/src/mcp/config/types.rs`, alongside the existing
   `command`/`args`/`url`/`transport`/`enabled` fields). When present, the
   referenced secret is resolved from local secret storage (no existing
   local secret store was found in this pass — see §11) and injected as:
   - **stdio transport:** an env var on the spawned subprocess
     (`StdioMcpClient::spawn`'s existing `command`/`args` call site gains an
     `envs: HashMap<String, String>` parameter, populated once at
     `get_or_spawn` time, not per-call).
   - **http transport:** an `Authorization` header per request (HTTP auth is
     typically per-request, not per-process env).
   The secret is **never** interpolated into the `args: Value` the LLM
   constructed — the model never sees the literal value. This is the
   concrete, code-grounded implementation of the "MCP with credential
   brokering" property Eve markets (§12.7).
4. **RBAC on new tools.** Every `ToolExecutor` produced by (1)-(3) MUST
   implement `restricted_tiers()` consistently with how
   `mcp_tool_executors()` (`tools/mcp_tools/executor.rs`) already gates the
   five `mcp_*` management tools — not silently inherit the trait default
   `&[]` (unrestricted).

**Timeout/retry contract (NEW — no existing documented contract found for
`StdioMcpClient::call_tool` in this pass):** default **30s** per-call
timeout (consistent with the existing `max_turns`-bounded LLM dispatch
budgets elsewhere in the crate); **no automatic retry** — a timeout surfaces
as `ToolResult::err(...)` exactly like today's handshake-failure path
(`mcp_service_tools.rs:112-128`), leaving the retry decision to the LLM,
consistent with `ToolResult`'s recoverable/fatal design intent
(`trusty-agents-common/src/lib.rs:151-175`).

### 4.3 Conformance

- `POST /rpc {"method":"tools/call","params":{"name":"memory_recall",...}}`
  against a running `tagent --api` instance returns a JSON-RPC result (not
  `-32601`), and an unknown tool name returns `-32602`.
- An `McpService` with `transport = "http"` and a reachable `url` produces a
  callable `ToolExecutor` (currently silently skipped).
- An `McpService` with `credential_ref` set spawns/calls with the resolved
  secret present in the subprocess env (stdio) or request header (http), and
  a `tracing`/debug capture of the LLM-visible `ToolResult` content never
  contains the literal secret value.
- A tool call exceeding 30s surfaces `ToolResult::Error { recoverable: true,
  .. }`, not a hang or panic.

---

## 5. SPEC-AGENTFW-04 — HandoffContext & Delegation Protocol

### 5.1 Current state

`DelegateToAgentTool` (`crates/trusty-agents/src/tools/delegate.rs:28-171`)
is the PM's tool for dispatching to sub-agents. Its schema
(`schema()`, lines 104-127) is flat:
`{"agent_name": string, "task": string}`, both required. Pre-flight
validation (lines 141-157) checks only that
`<config_dir>/<agent_name>.toml` **exists** — it has no concept of a
*declared* subagent set (that's the new `subagents.allowed` field from §2.2,
which this section wires in as a second check). Dispatch (lines 164-169):

```rust
let final_task = crate::agents::persona::assemble_task_with_context(agent_name, task);
match self.runner.run(agent_name, &final_task).await {
    Ok(out) => ToolResult::ok(out.content),
    Err(e) => ToolResult::err(format!("sub-agent '{agent_name}' failed: {e:#}")),
}
```

Today's "context" handed to a subagent is a **single flattened string** —
persona/skill prose prepended to the raw task by
`assemble_task_with_context`, then passed to the plain `run()` method of
`AgentRunner` (`crates/trusty-agents-common/src/runner.rs:201-249`):

```rust
#[async_trait]
pub trait AgentRunner: Send + Sync {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput>;
    async fn run_with_history(&self, agent_name: &str, task: &str,
        _history: &[HistoryMessage], ctx: &RunContext) -> Result<AgentOutput> { /* default -> run_with_context */ }
    async fn run_with_context(&self, agent_name: &str, task: &str,
        _ctx: &RunContext) -> Result<AgentOutput> { /* default -> run() */ }
}
```

`RunContext` (`runner.rs:100-115`) already carries `assigned_file:
Option<PathBuf>`, `max_turns_override: Option<u32>`, `working_dir:
Option<PathBuf>`, and a model override — but `DelegateToAgentTool::execute`
calls the plain `run()`, **not** `run_with_context`, so none of this richer
plumbing is exercised on the delegation path today. `AgentOutput`
(`runner.rs:146-158`) is `{content: String, summary: Option<String>, usage:
TokenUsage}` — already sufficient for a normative return protocol; no
changes needed there.

### 5.2 Normative requirement — `HandoffContext` (NEW)

```rust
// NEW — crates/trusty-agents/src/tools/delegate.rs (or a sibling handoff.rs)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffContext {
    pub summary: String,
    pub relevant_state: std::collections::BTreeMap<String, String>,
    pub constraints: Vec<String>,
}
```

**Size limit:** total serialized `HandoffContext` capped at **4 KiB**
(`serde_json::to_vec(&handoff)?.len() <= 4096`) — bounds prompt-tax the same
way perf/cost accounting already treats prompt bloat as a first-class
concern. Exceeding the cap returns `ToolResult::err(...)` (recoverable — the
LLM can shrink `relevant_state` and retry), never a panic.

**Schema change:** `delegate_to_agent`'s `schema()` gains an optional
`handoff: object` parameter alongside the existing required `agent_name` and
`task` (still `"required": ["agent_name", "task"]` — `handoff` is NOT
required). Absent `handoff` defaults to `HandoffContext{ summary: task.clone(),
relevant_state: {}, constraints: [] }`, so all three existing tests in
`tools/delegate.rs`'s own test module
(`unknown_agent_returns_helpful_error_with_available_list`,
`known_agent_reaches_runner`, `no_config_dir_skips_validation`) continue to
pass unmodified.

**Dispatch change:** `DelegateToAgentTool::execute` switches from
`self.runner.run(agent_name, &final_task)` to
`self.runner.run_with_context(agent_name, &final_task, &ctx)`, where `ctx:
RunContext` gains **NEW** field `handoff: Option<HandoffContext>` (additive —
every existing `RunContext` field is unchanged).

**Clean-context guarantee (normative, testable):** every `AgentRunner` impl
that spawns a subprocess (`RunnerKind::Subprocess`, the default) MUST start
that subprocess with **no** inherited conversation history unless
`persistent_session = true` on the **target** agent's own `AgentInfo`
(`config.rs` — existing field, already read at `run.rs:228-230`). Stated
precisely: the only way a subagent sees prior turns is (a) its own
`persistent_session` opt-in (existing mechanism, unchanged) or (b) the
explicit `HandoffContext.relevant_state`/`summary` the parent hands over
(new mechanism) — **never** ambient process/env state leaking from the
parent's own conversation.

**Return protocol:** unchanged — `AgentOutput{content, summary, usage}` flows
back through `ToolResult::ok(out.content)` exactly as today (`delegate.rs:167`).

### 5.3 Conformance

- A `delegate_to_agent` call omitting `handoff` behaves byte-for-byte
  identically to today (all three existing `delegate.rs` tests pass
  unmodified).
- A `HandoffContext` whose serialized size exceeds 4096 bytes returns a
  recoverable `ToolResult::err`, and the runner is **not** invoked (mirrors
  the existing "runner not invoked on validation failure" assertion pattern
  already used by `unknown_agent_returns_helpful_error_with_available_list`).
- A non-`persistent_session` subagent invoked via `delegate_to_agent` cannot
  observe any string from the parent's conversation history that was not
  explicitly present in `HandoffContext` or the `task` string — a
  regression test constructs a parent with private state (e.g. an
  unrelated prior tool result) and asserts the subagent's rendered prompt
  does not contain it.
- A **NEW** `Event::RunResumed { session_id, resumed_at_phase }` variant is
  added to the existing `Event` enum
  (`crates/trusty-agents/src/events.rs:56-`, alongside `PhaseStarted`/
  `PhaseDone`/`PhaseSkipped`) and fires exactly once per `tagent resume`
  invocation, over the existing SSE stream
  (`crates/trusty-agents/src/api/server/events_sse.rs`) — no new streaming
  transport is introduced; the current `GET /api/events?session_id=`
  contract (Server-Sent Events, 15s keepalive, `Lagged` handling) already
  covers this.

---

## 6. SPEC-AGENTFW-05 — Config Surface

Every new/extended config key introduced by §2–§5, following the existing
`TAGENT_*`-with-legacy-fallback convention
(`crates/trusty-agents/src/env_compat.rs`, `env_var(new, legacy)`;
`crates/trusty-agents/src/agents/model.rs:150-190`, `TAGENT_MODEL_<UPPER_SNAKE>`,
`TAGENT_DEFAULT_MODEL`, `TAGENT_CONFIG_DIR`):

| Section / file | Key | Type | Default | Env override |
|---|---|---|---|---|
| `[agent]` (per-agent TOML/frontmatter, §2.2) | `extends` | `Option<String>` | `None` | n/a (per-file) |
| `[subagents]` (**NEW** TOML table, per-agent, §2.2) | `allowed` | `Option<Vec<String>>` | `None` (unrestricted — current behavior) | n/a |
| `[tools]` (existing table, extended, §2.2) | `deny` | `Option<Vec<String>>` | `None` | n/a |
| `~/.trusty-agents/config.toml` `[[mcp.services]]` (existing `McpService`, extended, §4.2) | `credential_ref` | `Option<String>` | `None` | n/a |
| `~/.trusty-agents/config.toml` `[[mcp.services]]` (existing, extended, §4.2) | `timeout_secs` | `u64` | `30` | n/a |
| Workflow engine (process-level, **NEW**, §3.3) | checkpoint journal enabled | `bool` | `true` | `TAGENT_CHECKPOINT_DISABLE=1` — flagged in §11 as the one key in this table not yet grounded against a verified existing analogous flag; confirm exact naming/shape at implementation time. |
| Workflow engine (**NEW**, §3.3) | checkpoint state root | `PathBuf` | `.trusty-agents/state/runs/` (resolved the same way `agents_dir()`/`TAGENT_CONFIG_DIR` resolves, `loader.rs`) | `TAGENT_STATE_DIR` (**NEW**, mirrors the existing `TAGENT_CONFIG_DIR` pattern exactly) |
| `HandoffContext` (process-level constant, **NEW**, §5.2) | max size (bytes) | `usize` | `4096` | `TAGENT_HANDOFF_MAX_BYTES` (**NEW**) |
| `extends` resolution (**NEW**, §2.2) | max chain depth | `usize` | `8` | n/a — compile-time constant (safety ceiling, not an operator preference) |

### 6.1 Conformance

- Each new TOML key round-trips through parse/serialize exactly like the
  existing `tools_config_parses_allow_globs`-style tests in `config.rs`.
- `TAGENT_STATE_DIR` overrides the checkpoint root exactly as
  `TAGENT_CONFIG_DIR` overrides `agents_dir()` — same resolution order, same
  fallback-with-warn-log behavior.
- Every env var in this table is read through `env_compat::env_var(new,
  legacy)` if a legacy `OPEN_MPM_*` name is later requested for
  back-compat — none of the **NEW** keys need a legacy alias at
  introduction (they don't exist yet under any name), but the helper is the
  established pattern should one be needed.

---

## 7. SPEC-AGENTFW-06 — Memory Scope Declaration & Model/Provider Resolution

### 7.1 Current state

`MemoryStore` trait (`crates/trusty-agents/src/memory/store.rs:151-171+`):

```rust
#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn insert(&self, segment: Segment, id: &str, vector: &[f32], payload: serde_json::Value) -> Result<()>;
    async fn search(&self, segment: Segment, query_vec: &[f32], top_k: usize) -> Result<Vec<MemoryResult>>;
    async fn get(&self, segment: Segment, id: &str) -> Result<Option<serde_json::Value>>;
    // (+ delete, not re-quoted here)
}
```

`Segment` (`store.rs:22-37`) is a fixed, crate-defined enum: `AgentMemory |
CodeIndex | Context | Brief | History` — chosen entirely by whichever Rust
call site constructs the tool, **not** selectable from an agent's own
definition file today. `TrustyBackedMemoryStore`
(`crates/trusty-agents/src/memory/trusty_backed.rs:73-139`) backs this with
one `PalaceRegistry` + a SQLite `PayloadStore` sidecar (`payloads.db`) per
`data_root`, hydrating a per-`Segment` in-memory map on construction; each
segment maps to its own `PalaceHandle`, opened lazily.

`AgentInfo.model: String` (`config.rs`) is already an opaque, provider-
qualified string, resolved via `crate::agents::resolve_model` (called at
`md_agent.rs:94`: `resolve_model(&name, &agent_model_raw, None)`), which
layers — per `agents/model.rs` — `TAGENT_MODEL_<UPPER_SNAKE>` (agent-specific
env override) → the agent's own TOML/frontmatter `model` → `TAGENT_DEFAULT_MODEL`
(global env fallback), each tracked by a `ModelSource` variant
(`AgentEnv`/`DefaultEnv`, `model.rs:33/36`) for diagnostics. Once resolved,
`adapter_for_model(&resolved_model)` (`llm/adapter` module) selects the
provider adapter.

### 7.2 Normative requirement

**NEW `memory:` frontmatter/TOML block** — does **not** add a new storage
layer or a new `Segment` variant; it is a purely declarative default so an
agent's own file states which of the **five existing** segments it reads/
writes by default, instead of that choice living only in Rust call-site
code:

```yaml
memory:
  segment: brief        # one of: agent_memory | code_index | context | brief | history (serde snake_case)
  top_k: 5                # default search top_k for this agent's memory-querying tools
```

Validation: an unrecognized `segment` value is a load-time error listing the
five valid variants — the same "helpful list" pattern as the `extends`/
`delegate_to_agent` errors in §2.2/§5.

**Model/provider resolution — no redesign, one forward-looking constraint.**
This spec does **not** redesign `resolve_model`/`adapter_for_model`'s
internals — that work is gated on the planned unified inference-provider
adapter landing in `trusty-common` (fireworks.ai + more), which is out of
scope here. The **normative requirement** is narrower: **no new call site**
introduced by §2–§6 (the resume CLI re-resolving a phase's model, the
credential-brokering HTTP client selecting a provider, etc.) may bypass
`resolve_model`/`adapter_for_model` to hand-roll its own provider
special-case. When the commons adapter lands, `adapter_for_model` becomes a
thin call-through to it; every consumer added by this spec must already be
routed through that single choke point so the eventual swap is a one-file
change, not a multi-call-site migration.

### 7.3 Conformance

- An agent declaring `memory: { segment: brief, top_k: 5 }` causes its
  memory-querying tools to default to `Segment::Brief` with `top_k=5`
  without the tool's own Rust code hard-coding either value.
- An agent declaring `memory: { segment: nonexistent }` fails to load with
  an error listing the five valid segment names.
- `grep -rn "adapter_for_model\|resolve_model"` across every file touched by
  this spec's implementation shows no parallel/duplicate provider-detection
  logic introduced outside the existing `llm::adapter`/`agents::model`
  modules.

---

## 8. Phased Roadmap

**Phase 0 — Runtime durability (SPEC-AGENTFW-02).** The single largest gap:
`RunState`, `CheckpointRecord`, phase-boundary `atomic_write` calls,
`tagent resume`. No agent-definition-format changes yet. Ships as its own
issue/PR, API-first (an RPC/CLI method the `tagent resume` subcommand calls,
per the API→CLI→TUI layering).

**Phase 1 — Agent definition extensions (SPEC-AGENTFW-01).** `extends`
resolution + error cases, `tools`/`subagents` frontmatter fields, `[tools]
deny`/`[subagents] allowed` TOML tables. Ships independent of Phase 0.

**Phase 2 — Tool-calling gaps (SPEC-AGENTFW-03).** In priority order: (a)
`rpc/mod.rs` `tools/call` proxying — blocked on the implementation-time
verification of `trusty-memory`/`trusty-search`'s own execution entry
points (§11); (b) HTTP-transport MCP client; (c) credential brokering —
blocked on an owner decision for the secret-storage backend (§11).

**Phase 3 — Structured handoffs (SPEC-AGENTFW-04).** `HandoffContext`,
`RunContext.handoff`, `run_with_context` wiring, clean-context conformance
test. Depends on nothing else in this roadmap; can land in parallel with
Phase 2.

**Phase 4 — Memory/model declaration (SPEC-AGENTFW-06).** `memory:`
frontmatter block. Independent of every other phase; the model-resolution
choke-point constraint applies retroactively to whichever phases have
already landed.

**Phase 5 — Config surface (SPEC-AGENTFW-05).** Not a standalone phase —
each key ships alongside the phase that introduces it; this entry exists
only so the config table (§6) has a single place tracking "landed vs.
not yet."

Each phase is its own issue/PR chain off the epic issue this spec closes
out (#2791) — not a single mega-PR.

---

## 9. Explicit Non-Goals

- **No serverless/Vercel-Workflows-equivalent hosted durability engine.**
  trusty-agents remains a local-first tokio daemon; durability is
  phase-level on-disk checkpointing (§3), not a general-purpose
  durable-workflow platform.
- **No true mid-phase (sub-turn) checkpointing in the MVP.** Phase-level
  granularity only (§3.3) — a phase's LLM/tool-call sequence is the atomic
  unit of durability until there's evidence that's insufficient.
- **No wholesale replacement of the `.toml`/`.md`+frontmatter or
  directory-package (#482) agent formats.** §2's additions are strictly
  additive to both existing loaders.
- **No new `ToolInvoker` abstraction.** `ToolExecutor` (already unifying
  native, MCP-management, and MCP-external tools today) is retained; §4
  closes concrete gaps within it.
- **No commitment to multi-channel adapters (Slack/Discord/Telegram/etc.)
  in this spec.** Out of scope for trusty-agents vs. trusty-mpm's existing
  TELUI surface — a product-scope question, not designed here (see §11).
- **No OpenTelemetry adoption decision.** The existing `Event`
  enum/SSE stream (`events.rs`, `events_sse.rs`) already covers Eve's
  streaming use case functionally; OTel export is a separate,
  undecided question.
- **No redesign of `resolve_model`/`adapter_for_model` internals** — gated
  on the planned commons inference-provider adapter landing (§7.2).

---

## 10. Owner-Decision Checklist

Each item names the `SPEC-AGENTFW-NN` section(s) it gates. Until Bob
resolves an item, the gated section(s) stay `~draft` with the open question
inline (already noted in the relevant section above); this list is the
single place to track resolution status.

1. **Checkpoint granularity (gates SPEC-AGENTFW-02).** Confirm phase-level
   checkpointing — losing at most one in-flight phase on crash/restart — is
   sufficient, or specify a harder sub-turn requirement.
   - **RESOLVED:** _pending._
2. **Credential storage backend (gates SPEC-AGENTFW-03 item 3).** No
   existing local secret store was found in this pass. Is there a
   preferred trusty-tools-ecosystem secret store to build on, or does
   `credential_ref` resolution need a fresh design?
   - **RESOLVED:** _pending._
3. **`tools/call` proxy dispatch surface (gates SPEC-AGENTFW-03 item 1).**
   `ServiceDescriptor` has no execute method — confirmed this pass. Needs a
   research pass into `trusty-memory`'s/`trusty-search`'s actual
   tool-execution entry points before implementation; flagged here rather
   than asserting an unverified signature.
   - **RESOLVED:** _pending — research task, not strictly an owner
     decision, but tracked here so it isn't lost._
4. **Multi-channel scope.** Should trusty-agents grow its own channel
   adapters (Slack/Discord/etc.), or stay exclusively out of scope per §9?
   Not gating any SPEC-AGENTFW section directly — flagged in case the
   answer changes §9's non-goal.
   - **RESOLVED:** _pending._
5. **`extends` prompt-composition default (gates SPEC-AGENTFW-01 §2.2).**
   Confirm "child replaces, opt-in `{{extends_prompt}}` token" as the
   default `system_prompt.content` merge rule, or specify append-by-default
   instead.
   - **RESOLVED:** _pending._
6. **`HandoffContext` size cap (gates SPEC-AGENTFW-04 §5.2).** Confirm 4 KiB,
   or specify a different number.
   - **RESOLVED:** _pending._
7. **Checkpoint-disable env var naming (gates SPEC-AGENTFW-05 §6).**
   `TAGENT_CHECKPOINT_DISABLE` is proposed but not grounded against a
   verified existing analogous flag in this pass — confirm naming/shape.
   - **RESOLVED:** _pending._
8. **`code_dir` integrity on resume (gates SPEC-AGENTFW-02 §3.5).** Confirm
   no integrity check is needed for partial generated files in the MVP, or
   specify one.
   - **RESOLVED:** _pending._

---

## 11. Background — Eve Review (Appendix)

Vercel's **Eve** launched 2026-06-17 (Apache-2.0, public beta). Primary
sources: [vercel.com/blog/introducing-eve][eve-blog],
[vercel.com/docs/eve][eve-docs], [github.com/vercel/eve][eve-repo].

Core idea: **"a file's name and place in the tree is its definition."** An
agent is a directory: `agent.ts` (`defineAgent({model:...})`) +
`instructions.md` (system prompt) + `tools/*.ts` (one file per tool,
filename = tool name) + `subagents/*/` (full nested agent directories,
called like a tool, each with a clean context window) + `connections/*`
(`defineMcpClientConnection` — MCP-native, with credential brokering so the
model never sees a secret). Execution runs on **Vercel Workflows**: durable,
checkpointed, survives crash/deploy, zero-cost indefinite pauses for human
approval. Streaming: NDJSON + OpenTelemetry spans per turn. Deployment:
`eve dev` → `vercel deploy`, deploy-safe versioning of in-flight sessions,
multi-channel adapters (Slack/Discord/Teams/Telegram/Twilio/GitHub/Linear).

**Portable ideas (informed §2–§7 above):** convention-over-configuration
agent definition; MCP as the tool-calling backbone with credential
brokering; structural, clean-context subagent delegation.

**Platform-specific, not ported as-is:** durability-via-Vercel-Workflows (no
platform substrate exists for a self-hosted tokio daemon — §3 builds a
narrower, phase-level equivalent instead); the stateless request/session
HTTP shape (a resident daemon doesn't need it for intra-process durability,
only for surviving its own restarts, which §3 addresses directly);
zero-cost indefinite pause (a serverless billing property with no local
analog — removes an objection, not a design requirement).

---

## 12. Background — Gap Analysis (Appendix)

| Eve abstraction | trusty-agents today | Disposition |
|---|---|---|
| Directory-as-definition, one file per tool/subagent | Two loaders (flat `.toml`/`.md` + #482 directory-package); no `tools:`/`subagents:`/`extends:` declaration | Closed by SPEC-AGENTFW-01 |
| MCP-native tools, credential brokering | `ToolExecutor` already unifies native + MCP-external + MCP-management tools; external MCP calling is live (stdio-only); no credential brokering | Closed by SPEC-AGENTFW-03 |
| Subagent-as-directory, clean context per call | `DelegateToAgentTool` + `AgentRunner` exist; flat string context, no declared subagent set, no clean-context conformance test | Closed by SPEC-AGENTFW-01 (declaration) + SPEC-AGENTFW-04 (protocol) |
| Durable, checkpointed execution | Zero on-disk state during an in-flight run; `perf.flush` only at the very end | Closed by SPEC-AGENTFW-02 |
| NDJSON/OTel streaming | Full `Event` enum + SSE stream already in place, arguably richer (AST-operation, persona-detection, LLM-lifecycle events) | Not a gap — already present |
| State persistence delegated to platform | `TrustyBackedMemoryStore`/Palace integration (#379) is deeper than Eve's story, but not declaratively surfaced in the agent's own file | Closed by SPEC-AGENTFW-06 |
| Deploy-safe versioning of in-flight sessions | No equivalent — same root cause as the durability gap | Addressed indirectly by SPEC-AGENTFW-02's checkpoint/resume |
| Multi-channel deployment | No trusty-agents-owned channel adapters (trusty-mpm has a separate Telegram/TELUI surface) | Explicit non-goal (§9), flagged as an open question (§10 item 4) |

---

## 13. Change Log

- **2026-07-15 (v1)** — Initial draft merged as PR #2792. Comparative review
  of Vercel's Eve spec plus design *directions* for a Rust equivalent — no
  normative content. **Rejected by Bob:** "the eve research is NOT a spec,
  it refers to one but doesn't actually contain one."
- **2026-07-15 (v2)** — Full rewrite. Every `SPEC-AGENTFW-01..06` section
  made normative: exact frontmatter schema + `extends` resolution algorithm
  and error cases (§2), an explicit `RunState` machine + `CheckpointRecord`
  schema + failure matrix (§3), a corrected (not invented) unified
  tool-calling picture plus the three real remaining gaps (§4), an exact
  `HandoffContext` struct + clean-context conformance test (§5), a full
  config-key table (§6), and a declarative `memory:`/model-resolution
  constraint (§7) — each re-verified against `origin/main` @ `78868a62`
  with `file:line` citations, with anything not already in the tree marked
  **NEW**. Eve review and gap analysis demoted to background appendices
  (§11–§12). Owner-decision checklist (§10) added, gating specific SPEC
  sections rather than floating as generic open questions.

[eve-blog]: https://vercel.com/blog/introducing-eve
[eve-docs]: https://vercel.com/docs/eve
[eve-repo]: https://github.com/vercel/eve
