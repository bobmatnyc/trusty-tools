# Bug Analysis: #207, #208, #209

Date: 2026-04-26  
Analyst: Research agent  
Status: Root causes identified, minimal fixes documented

---

## Bug #207: Duplicate `OPEN_MPM_CONFIG_DIR` warnings

### Root Cause

The warning is emitted inside `agent_config_path()` at
`src/agents/mod.rs:734-737`:

```rust
fn agent_config_path(name: &str) -> PathBuf {
    let dir = match std::env::var("OPEN_MPM_CONFIG_DIR") {
        Ok(s) if !s.is_empty() => PathBuf::from(s),
        _ => {
            tracing::warn!(           // <-- fires once per call
                agent = %name,
                "OPEN_MPM_CONFIG_DIR not set; falling back to CWD-relative .open-mpm/agents/"
            );
            PathBuf::from(".open-mpm/agents")
        }
    };
    dir.join(format!("{name}.toml"))
}
```

`agent_config_path` is called by both `AgentConfig::by_name` (line 612) and
`AgentConfig::by_name_async` (line 646).  During a single workflow run the
engine calls one of these for every phase agent.  The callers across the
codebase are:

| Call site | File | Line |
|---|---|---|
| `by_name` | `src/main.rs` | 1661, 1668, 1880, 1965 |
| `by_name` | `src/inspection/mod.rs` | 126 |
| `by_name` | `src/workflow/engine.rs` | 762, 950, 1022, 1451 |
| `by_name_async` | `src/agents/in_process_runner.rs` | 213 |
| `by_name_async` | `src/agents/claude_code_runner.rs` | 453, 465, 536, 572, 621 |

With a standard 5-phase workflow (research → plan → code → qa → observe) each
phase calls `agent_config_path` at least once, producing 5 identical warnings.
Wave-loop retries in `engine.rs` (lines 950, 1022) can multiply this further
because the agent config is re-loaded on each wave iteration.

A second read site exists in `src/main.rs:655-669`
(`default_bundled_config_dir`) but it does NOT emit a warning — it silently
falls back — so that function is not a contributor.

### Minimal Fix

Gate the warning with a process-global `OnceLock<()>` (or `AtomicBool`)
so it fires at most once per process invocation:

```rust
use std::sync::OnceLock;

static CONFIG_DIR_WARNED: OnceLock<()> = OnceLock::new();

fn agent_config_path(name: &str) -> PathBuf {
    let dir = match std::env::var("OPEN_MPM_CONFIG_DIR") {
        Ok(s) if !s.is_empty() => PathBuf::from(s),
        _ => {
            CONFIG_DIR_WARNED.get_or_init(|| {
                tracing::warn!(
                    "OPEN_MPM_CONFIG_DIR not set; falling back to CWD-relative .open-mpm/agents/"
                );
            });
            PathBuf::from(".open-mpm/agents")
        }
    };
    dir.join(format!("{name}.toml"))
}
```

The `agent = %name` field is dropped from the deduplicated warning because it
would only reflect whichever agent happened to trigger the first call; that
context is less useful than suppressing the flood.  Operators who need the
full diagnostic can enable `RUST_LOG=debug`.

---

## Bug #208: `add project <path>` routed through full prescriptive workflow

### Root Cause — two-step misclassification

**Step 1: `classify_intent` returns `Implementation`**

`src/intent/mod.rs:205` (`classify_intent`) normalizes the input to lowercase
and splits on whitespace.  For the input `"add project /some/path"` the word
list is `["add", "project", "some", "path"]` (the slash is stripped by
`normalize`).

`"add"` appears in `ACTION_VERBS` at `src/intent/mod.rs:50`.  Line 243 sets
`has_action_verb = true`, and line 276-278 unconditionally returns
`IntentClass::Implementation`.

**Step 2: `submit_task` routes `Implementation` to the prescriptive workflow**

`src/api/server.rs:620-701`: the `match intent` block sends
`IntentClass::Implementation` to `run_task` (line 683), which spawns the full
prescriptive subprocess pipeline (60-90 s).

`IntentClass::Conversational | IntentClass::Research` (lines 623-675) both go
to `run_pm_task_with_session`, which uses CTRL's in-process tool registry
containing `AddProjectTool` (registered at `src/ctrl/mod.rs:2535`).

**Why `AddProjectTool` is never reached**

`AddProjectTool` is registered inside `run_pm_task_with_session`'s local
`ToolRegistry` (via `build_ctrl_tool_registry` called at line 2535).  That
function is only reached when the intent is `Conversational` or `Research`.
When intent is `Implementation`, the prescriptive pipeline is invoked instead
and `AddProjectTool` is never in scope.

### Minimal Fix

Add a pre-classification fast path in `submit_task` (or at the top of
`classify_intent`) that recognizes the literal pattern `"add project <path>"`:

**Option A — pattern check before classify_intent (preferred, zero LLM cost):**

In `src/api/server.rs` around line 619, before calling `classify_intent`:

```rust
let normalized_task = req.task.trim().to_lowercase();
if normalized_task.starts_with("add project ") {
    // Route directly to run_pm_task_with_session which has AddProjectTool.
    // Implementation identical to the Conversational/Research branch below.
    ...
    return ...;
}
let intent = classify_intent(&req.task);
```

**Option B — remove "add" from ACTION_VERBS, add it to a ctrl-command list:**

"add" is ambiguous: it is both a ctrl command prefix (`add project`) and an
implementation verb (`add authentication`).  Removing it from `ACTION_VERBS`
would misclassify legitimate implementation requests.  Option A is therefore
preferred because it keeps the verb list intact and is narrowly scoped to the
specific ctrl command phrase.

**Option C — detect ctrl commands in classify_intent:**

Add a `CTRL_COMMANDS` constant in `src/intent/mod.rs` listing known command
prefixes (`"add project"`, `"remove project"`, `"status"`, etc.) and return a
new `IntentClass::CtrlCommand` variant (or reuse `Conversational`) before the
action-verb scan.

---

## Bug #209: Observe agent cannot distinguish "skipped by persona" from "phase failed"

### Root Cause

**What observe receives for skipped phases**

The observe context template (from `.open-mpm/workflows/prescriptive.json`) is:

```
Task: {{task}}

Research:
{{research}}

Plan:
{{plan}}

Code:
{{code}}

QA:
{{qa}}
```

Template substitution lives in `src/workflow/context.rs:77` (`render_template`).
It resolves `{{key}}` by looking up `phase_summaries` then `phase_outputs`.  If
a key is absent from both maps it renders the literal string `(missing: <key>)`
(line 144).

When a phase is skipped (either `skip=true` at line 565 or persona opt-out at
line 575 of `src/workflow/engine.rs`), the engine calls `continue` immediately
— it does NOT call `ctx.record_phase(...)`.  Therefore `phase_summaries` and
`phase_outputs` have no entry for that phase name.

**Result:** observe receives `(missing: research)` (or similar) for every
skipped phase.  This is indistinguishable from a phase that ran but produced
empty output, or a phase that the workflow definition simply does not include.

**No PhaseStatus enum exists**

There is no `PhaseStatus` type anywhere in `src/workflow/`.  The engine emits
a `crate::events::Event::PhaseSkipped` event (line 582) for observability, but
this event is not written into `WorkflowContext` and is therefore invisible to
the observe agent's rendered prompt.

### Minimal Fix

**Step 1: Write a sentinel into `phase_summaries` when a phase is skipped.**

In `src/workflow/engine.rs`, replace both `continue` statements in the skip
blocks with a sentinel record before continuing:

```rust
// skip=true block (line 566):
if phase.skip.unwrap_or(false) {
    info!(phase = %phase.name, agent = %phase.agent, "skipping phase (skip=true)");
    ctx.record_phase(
        &phase.name,
        String::new(),
        Some(format!("(skipped: disabled in workflow config)")),
    );
    continue;
}

// persona opt-out block (line 575):
if phases_to_skip(persona).contains(&phase.name.as_str()) {
    info!(phase = %phase.name, persona = %persona, "skipping phase (persona opt-out)");
    crate::events::emit(crate::events::Event::PhaseSkipped { ... });
    ctx.record_phase(
        &phase.name,
        String::new(),
        Some(format!("(skipped: {persona} persona does not run this phase)")),
    );
    continue;
}
```

This ensures `{{research}}` renders as `(skipped: hacker persona does not run
this phase)` instead of `(missing: research)`, giving the observe agent
explicit signal to interpret the absence correctly.

**Step 2 (optional, stronger): Add `PhaseStatus` to `WorkflowContext`.**

Introduce a `HashMap<String, PhaseStatus>` (where `PhaseStatus` is
`Success | Failed | Skipped(String)`) in `WorkflowContext`.  The engine
populates it at the same points where `record_phase` is called.  The observe
prompt template can then be extended with a `{{phase_statuses}}` variable that
summarises the run shape, giving the LLM structured grounding rather than
prose sentinels embedded in template slots.

Step 1 is the minimal fix with no API surface change.  Step 2 is recommended
as a follow-on for richer observe prompts.

---

## Summary Table

| Bug | File(s) | Root cause | Fix location |
|---|---|---|---|
| #207 | `src/agents/mod.rs:730-742` | `agent_config_path` warns on every call, called N times per workflow run | Add `OnceLock` guard in `agent_config_path` |
| #208 | `src/intent/mod.rs:50`, `src/api/server.rs:619-701` | `"add"` in `ACTION_VERBS` → `Implementation` → prescriptive pipeline; `AddProjectTool` only reachable via Conversational/Research path | Pre-classify `"add project <path>"` before `classify_intent` in `submit_task` |
| #209 | `src/workflow/engine.rs:565-587`, `src/workflow/context.rs:144` | Skipped phases write nothing to `phase_summaries`; template renders `(missing: X)` indistinguishable from failure | Write sentinel string via `ctx.record_phase` in both skip branches |
