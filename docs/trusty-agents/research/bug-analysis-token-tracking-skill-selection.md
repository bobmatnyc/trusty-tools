# Bug Analysis: Token Tracking and Skill Selection

**Date:** 2026-04-27
**Builds affected:** all builds post-e47a234 (claude-code migration)
**Build 1335 demonstrates both bugs simultaneously**

---

## Bug 1: Token/Cost Tracking Broken After claude-code Runner Migration

### Symptom

All perf runs since commit e47a234 show `prompt_tokens=0, completion_tokens=0, cost_usd=0.0`
despite runs completing successfully (build 1335: 962 seconds, 6 phases, all successful).

### Root Cause — Exact Location

**File:** `src/agents/claude_code_runner.rs`, lines 436–444

```rust
Ok(AgentOutput {
    content,
    summary,
    // claude CLI does not (currently) expose token usage in stream-json
    // in a form we can consume. Leaving this zero is consistent with
    // tool-only agents — the perf record will just lack per-phase
    // token counts for claude-code phases.
    usage: TokenUsage::default(),
})
```

`ClaudeCodeAgentRunner::run_with_config_ctx` always returns `TokenUsage::default()` (all zeros).
The comment acknowledges this is intentional but incorrect — it asserts the claude CLI does not
expose usage. This assertion is wrong (see below).

### What Data IS Available

The `claude` CLI's `--output-format stream-json` result event **does** include a `usage` field.
The `{"type":"result"}` event structure:

```json
{
  "type": "result",
  "result": "...",
  "is_error": false,
  "usage": {
    "input_tokens": 12345,
    "output_tokens": 678,
    "cache_read_input_tokens": 0,
    "cache_creation_input_tokens": 0
  }
}
```

The runner already parses the `result` event at lines 334–348 of `claude_code_runner.rs` but
only extracts `is_error`, `subtype`, and `result` — it skips `usage` entirely.

The `TokenUsage` struct in `src/perf.rs` (lines 34–37) holds exactly the right fields:
`prompt_tokens`, `completion_tokens`, `cache_read_tokens`, `cache_creation_tokens`.

### Where Token Counts Flow in the Engine

`src/workflow/engine.rs` line 1146:
```rust
perf.record_phase(&phase.name, duration_ms, &phase_model, &output.usage);
```

`output.usage` comes from `AgentOutput.usage`, which comes directly from
`ClaudeCodeAgentRunner::run_with_config_ctx`. Since that always returns `TokenUsage::default()`,
every phase record has zero tokens regardless of how much work the model did.

### Recommended Fix

In `src/agents/claude_code_runner.rs`, in the `Some("result")` match arm (lines 334–348),
add `usage` extraction alongside the existing fields:

```rust
Some("result") => {
    is_error = event.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
    subtype = event.get("subtype").and_then(|v| v.as_str()).map(String::from);
    final_result = event.get("result").and_then(|v| v.as_str()).map(String::from);

    // Extract token usage from the result event if present.
    if let Some(u) = event.get("usage") {
        result_usage = TokenUsage::new(
            u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            u.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        );
    }
    break;
}
```

Then return `usage: result_usage` instead of `usage: TokenUsage::default()`. Also update the
mock script in `run_parses_stream_json_result` to include a `usage` field and assert it
is parsed.

---

## Bug 2: Wrong Skill Selection — "rust" for Python Tasks

### Symptom

Build 1335 (Python CSV→Markdown task): `skills_used = ["rust", "pytest", "wave-planning"]`.
The `rust` skill was injected into a pure Python task's phase prompts.

`skills_considered = ["python-testing", "pytest", "python", "python-idiomatic", "tdd",
"fixture-quality", "python-packaging", "fastapi"]` — note `rust` is NOT in this list.
This means `rust` came from the `skills_loader` path, not from the `tag_skill_registry`
discovery path.

### Root Cause — Exact Location

**File:** `src/workflow/engine.rs`, lines 782–803
**File:** `src/skills/mod.rs`, lines 546–560 (`detect_languages`)

The engine resolves `project_dir` for skill detection using the **harness's own working
directory**, not the task's output directory:

```rust
// engine.rs line 783
let project_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
```

The harness's CWD is `/Users/masa/Projects/open-mpm/` — the open-mpm repo itself —
which contains `Cargo.toml`. `detect_languages` sees `Cargo.toml` and adds `"rust"`:

```rust
// skills/mod.rs line 548
if project_dir.join("Cargo.toml").exists() {
    langs.push("rust".to_string());
}
```

When `explicit_skills` is empty (no `skills = [...]` in the phase TOML, which is the
common case), `build_skills_prefix_tracked` receives an empty `explicit` slice and falls
into the `else` branch (line 746: `explicit.to_vec()`), returning an empty vec — so "auto"
mode is **not** triggered. Wait: if `explicit` is empty, `skill_names` is also empty and
the function returns early at line 749–751.

Re-examining: `explicit_skills = phase.skills.as_deref().unwrap_or(&[]).to_vec()`.

When the phase TOML has `skills = ["auto"]`, the auto-detection path runs. The `rust` skill
appears because `detect_languages(current_dir())` finds `Cargo.toml` in the harness repo.

**So the bug requires two conditions to be true simultaneously:**
1. Phase TOML has `skills = ["auto"]` (or the workflow config sets auto skill detection)
2. The harness is run from its own source directory (which contains `Cargo.toml`)

This is a **CWD contamination bug**: the harness's Rust build artifacts (`Cargo.toml`) bleed
into the language detection for a completely unrelated Python task being run by the harness.

### Why wave-planning is Also Spurious

`detect_workflow_skills` (lines 601–615) triggers `"wave-planning"` when the task contains
`"wave"`, `"assignments"`, or `"decompose"`. The Python CSV task likely contains
"assignments" (from the workflow JSON template referencing `assignments.json`), causing
false-positive wave-planning skill injection.

### What the Correct Behavior Should Be

`detect_languages` should be called with the **task output directory** (where the agent
is writing the Python project), not `std::env::current_dir()`. The harness source
directory is irrelevant to the task's language stack.

### Recommended Fix

In `src/workflow/engine.rs`, replace line 783:
```rust
// WRONG: uses harness CWD which contains Cargo.toml
let project_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
```

With the task output directory:
```rust
// RIGHT: use the out_dir where the agent is writing task files
let project_dir = out_dir.clone().unwrap_or_else(|| {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
});
```

`out_dir` is already in scope at the call site (it is set at line 617 as `ctx.out_dir`
or derived earlier in the `run` function). This scopes language detection to the task's
artifact directory rather than the harness process directory.

Additionally, for the `wave-planning` false-positive: `detect_workflow_skills` should
not match on `"assignments"` as a standalone word since that string appears in the
workflow engine's own template variables. Consider restricting the match to
`"wave-planning"` or `"decompose"` only, or requiring the word to appear in a
user-supplied task description before the template is rendered.

---

## Summary Table

| Bug | File | Line | Root Cause | Fix |
|-----|------|------|------------|-----|
| Token tracking | `src/agents/claude_code_runner.rs` | 439–443 | `usage: TokenUsage::default()` hardcoded; result event `usage` field never parsed | Parse `usage` from the `{"type":"result"}` JSON event |
| Rust skill in Python task | `src/workflow/engine.rs` | 783 | `current_dir()` is the harness repo root (has `Cargo.toml`); `detect_languages` adds "rust" | Use `out_dir` (task output dir) instead of `current_dir()` for language detection |
