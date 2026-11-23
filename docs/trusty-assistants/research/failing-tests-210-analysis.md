# Failing Tests Analysis: t04/t05/t06/t07 (GitHub #210)

**Date**: 2026-04-26
**Analyst**: Research agent (Claude Sonnet 4.6)
**Scope**: Root-cause analysis of three test failures without code changes

---

## Issue 1 — t04/t05: `add project` and `list projects` crash (subprocess exit 1)

### Routing check — is_ctrl_command block

`src/api/server.rs` lines 628–634:

```rust
let normalized = req.task.trim().to_lowercase();
let is_ctrl_command = normalized.starts_with("add project ")
    || normalized.starts_with("remove project ")
    || normalized.starts_with("stop task ")
    || normalized.starts_with("set active ")
    || normalized == "list projects"
    || normalized == "list tasks";
```

**Verdict: routing is CORRECT.**

- `"add project /Users/masa/Projects/open-mpm"` → normalized = `"add project /users/masa/projects/open-mpm"` → matches `starts_with("add project ")`. Routes to `IntentClass::Research`.
- `"list projects"` → normalized = `"list projects"` → matches `== "list projects"`. Routes to `IntentClass::Research`.

Both flow into the `IntentClass::Conversational | IntentClass::Research` arm, which calls `crate::ctrl::run_pm_task_with_session`.

### Root cause — tool registry gap in run_pm_task_with_session

`src/ctrl/mod.rs` lines 663–673 show what `run_pm_task_with_session` registers:

```rust
let runner: Arc<dyn AgentRunner> =
    Arc::new(SubprocessAgentRunner::new().with_config_dir(Some(config_dir.clone())));

let mut registry = ToolRegistry::new();
registry.register(Arc::new(
    DelegateToAgentTool::new(runner).with_config_dir(config_dir),
));
let openai_tools = registry.openai_tools()?;
```

**Only `DelegateToAgentTool` is registered. `AddProjectTool` and `ListProjectsTool` are NOT in this registry.**

The full CTRL tool registry (`build_ctrl_registry`, lines 2495–2548) is only constructed inside `run_ctrl()` (the interactive CLI loop) and the per-turn LLM function at line 2586. It is **never called** from `run_pm_task_with_session`.

### What actually happens

1. Task arrives as Research (CTRL short-circuit fires correctly).
2. `run_pm_task_with_session` calls the CTRL agent LLM with only `delegate_to_agent` in its tool list.
3. The CTRL LLM (system prompt: `ctrl.toml`) sees `"add project /Users/masa/Projects/open-mpm"` and tries to call `add_project(path=...)` or `delegate_to_agent("some-agent", ...)`.
4. If it calls `delegate_to_agent`, the subprocess runner spawns `open-mpm --agent <name>`, which then exits 1 because either:
   - No agent name makes sense for this command (hallucinated agent name blocked by `#204` validation), OR
   - The agent gets a task with no real LLM call available (OpenRouter 402), see logs.
5. If the LLM produces no tool call (text-only), `run_pm_task_with_session` returns the text as content — but that is only a partial success; project is not actually registered.

The crash (subprocess exit 1) visible in the regression harness is the result of the LLM incorrectly routing to `delegate_to_agent` with a hallucinated agent name, because `add_project` is not available in its tool list.

### Supporting evidence from stderr log

`~/.open-mpm/logs/api-stderr.log` contains:
```
Error: failed to load agent config for 'code-searcher'
    0: failed to read agent config /Users/masa/Projects/open-mpm/.open-mpm/agents/code-searcher.toml
    1: No such file or directory (os error 2)
```
This confirms the #204 agent-name validation rejects hallucinated agent names (like `code-searcher`, and likely similar for `add_project` tasks), which propagates as `exit status: 1`.

### Fix required

`run_pm_task_with_session` must register the same CTRL tools that `build_ctrl_registry` provides — at minimum `AddProjectTool`, `ListProjectsTool`, `RemoveProjectTool`, `StopTaskTool`, `SetActiveProjectTool`. The function currently only registers `DelegateToAgentTool`, which is insufficient for CTRL management commands.

---

## Issue 2 — t07: `write tests for the intent classifier` fails immediately (0ms)

### Intent routing

The task text `"write tests for the intent classifier"` contains the word `"write"`, which is in `ACTION_VERBS` (`src/intent/mod.rs` line 43). Therefore `classify_intent` returns `IntentClass::Implementation`, bypassing the `is_ctrl_command` short-circuit (correct: it is not a CTRL command).

This routes to `run_task()` (line 698 in server.rs), which spawns:
```
open-mpm --workflow prescriptive --json --task "write tests for the intent classifier"
```

### Subprocess startup: 0ms failure

The 0ms failure is **not** a phase failure — it happens before any phase runs. The run log shows `dur_ms=301` (build 1101), `dur_ms=137` (build 1102), `dur_ms=167` (build 1106), `dur_ms=263` (build 1108) — all sub-second runs with `cost_usd=0.000000` and zero prompt/completion tokens. This pattern means the child process exited 1 before making any LLM call.

### Root cause: OpenRouter 402 (Insufficient Credits)

The stderr log contains many back-to-back entries:
```
Error: HTTP status client error (402 Payment Required) for url (https://openrouter.ai/api/v1/chat/completions)
[open-mpm] ✗ research   failed (0s) — phase 'research' failed: sub-agent 'research-agent' exited with status exit status: 1 and no valid result
Error: workflow execution failed
    0: phase 'research' failed: sub-agent 'research-agent' exited with status exit status: 1 and no valid result
```

The `0s` timing on the `research` phase failure (with 0 prompt tokens) confirms the child `open-mpm` process calls the LLM at `research-agent` startup, gets 402 from OpenRouter, and exits 1. The parent `run_task` sees `exit status: 1` and reports `subprocess exited with status Some(1)` (server.rs line 1033).

For t07 specifically: `write tests for the intent classifier` goes through `research` as the first phase of the prescriptive workflow. The `research-agent` subprocess makes its first LLM call, receives HTTP 402 (OpenRouter credit exhausted), and immediately exits 1 at 0ms wall time.

### Agent TOML inventory

All required agents exist in `.open-mpm/agents/`:
```
research-agent.toml, plan-agent.toml, engineer.toml, qa-agent.toml,
observe-agent.toml, docs-agent.toml, ctrl.toml, pm.toml
```
No missing TOML is a factor in this failure.

### Conclusion

t07's "subprocess exit 1, 0ms" is an **OpenRouter credit exhaustion (HTTP 402)** error, not a code bug. The `research-agent` subprocess is the first to call the API and fails immediately. This is an infrastructure/billing issue, not a routing or agent-config bug.

---

## Issue 3 — t06: Hacker persona still runs plan phase (269s, target <90s)

### phases_to_skip for hacker

`src/workflow/engine.rs` lines 1379–1393:

```rust
fn phases_to_skip(persona: &str) -> &'static [&'static str] {
    match persona {
        "hacker" => &["research", "qa", "docs"],
        "vibe-coder" => &["research", "plan", "qa", "docs"],
        "novice" => &[],
        _ => &[],
    }
}
```

**The `hacker` persona skips `research`, `qa`, `docs` — but NOT `plan`.**

### Workflow phases (prescriptive.json)

The prescriptive workflow defines these phases in order:
1. `research` — skipped by hacker
2. `plan` — **NOT skipped by hacker**
3. `code`
4. `qa` — skipped by hacker
5. `observe`
6. `docs` — skipped by hacker (also has `"skip": false` but persona overrides)

So a hacker persona run executes: `plan` + `code` + `observe` = **3 phases**.

### Why 269s exceeds 90s

The `plan` phase uses `plan-agent` with `model_override: "anthropic/claude-opus-4-6"` (prescriptive.json line 35). Claude Opus 4.6 is significantly slower and more expensive than Sonnet 4.6. A single Opus plan-agent call easily takes 60–120s on non-trivial tasks, pushing the total well past the 90s SLA.

The 269s timing for t06 is therefore explained by:
- `plan` phase using Opus 4.6 (~60–120s)
- `code` phase using Opus 4.6 (`model_override: "anthropic/claude-opus-4-6"` in prescriptive.json line 39) (~90–150s)
- `observe` phase using Sonnet 4.6 (~20–40s)

### The intent behind "hacker < 90s"

The test expectation of <90s for the hacker persona implies `plan` should also be skipped for hacker. The comment in `phases_to_skip` says:
> Hacker: code-only. Skip research (heavyweight), QA (no test suite for one-off scripts), and docs (no README for throwaway code).

But it does NOT say "skip plan". The `vibe-coder` persona *does* skip plan (`"vibe-coder" => &["research", "plan", "qa", "docs"]`). The `hacker` persona comment says "code-only" which implies plan should be skipped too, but the implementation does not match that intent.

### Fix required

To meet the <90s target for t06, `hacker` must skip `plan` as well:
```rust
"hacker" => &["research", "plan", "qa", "docs"],
```
This would leave only `code` + `observe` for the hacker path — matching the "code-only" description in the comment. With Opus on the code phase alone (~90–150s), even this may be tight; the test target of <90s may also require the hacker persona to use a faster model (Sonnet) for its `code` phase.

---

## Summary Table

| Test | Root Cause | File:Line | Fixable? |
|------|-----------|-----------|---------|
| t04/t05 | `run_pm_task_with_session` registers only `DelegateToAgentTool`; `AddProjectTool`/`ListProjectsTool` absent → LLM hallucinates `delegate_to_agent` call → subprocess exit 1 | `src/ctrl/mod.rs:663–673` | Yes — add CTRL tools to that registry |
| t07 | OpenRouter HTTP 402 (credit exhaustion) at `research-agent` first LLM call → 0ms exit 1 | Infrastructure issue, not code | Recharge OpenRouter credits |
| t06 | `phases_to_skip("hacker")` omits `"plan"` → plan-agent (Opus 4.6) runs, adding 60–120s | `src/workflow/engine.rs:1383` | Yes — add `"plan"` to hacker skip set |
