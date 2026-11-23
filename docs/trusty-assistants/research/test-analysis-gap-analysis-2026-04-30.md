# Test Analysis Capabilities: Gap Analysis

**Date**: 2026-04-30
**Scope**: What open-mpm currently has for test running/analysis, what is missing,
and the top 3 additions to reach parity with a mature PM orchestrator.

---

## What open-mpm Currently HAS

### 1. QA agent with workflow integration

`.open-mpm/agents/qa-agent.toml` defines a `runner = "claude-code"` agent that:
- Is wired into the prescriptive workflow as the fourth phase (research → plan → code → **qa** → observe → docs).
- Detects the project stack automatically (Cargo.toml → `cargo test`, package.json → vitest/jest, etc.).
- Returns a structured JSON result: `{passed, failed, errors, status, summary, details}`.
- Has access to `pytest_exec`, `load_skill`, `list_skills`, and `advance_workflow_phase` tools.

### 2. `pytest_exec` tool — sandboxed shell executor

`src/tools/shell_exec.rs` provides a narrowly scoped `ShellExecTool` registered as `pytest_exec`.
It runs commands via `tokio::process::Command` and returns `stdout + stderr + exit code`.

**Critical problem**: The allowlist in `is_allowed_pytest()` (line 106) only accepts
`/opt/homebrew/bin/python3.11 -m pytest` and `python3.11 -m pytest`. The QA agent
system prompt says "despite the tool name, you may invoke ANY shell command", but
that is aspirational documentation — the Rust implementation unconditionally rejects
`cargo test`, `vitest`, `go test`, etc. with an error.

### 3. QA summary captured in workflow engine

`src/workflow/engine.rs` (lines 1225–1232) grabs the first non-empty line of QA output
(up to 80 chars) as `qa_summary`, which is passed to the ticket manager's success hook.

### 4. Performance run JSON with `status` / `failed_phase`

`src/perf.rs` records per-run status and which phase failed. `docs/performance/runs.log`
tracks `build`, `workflow`, `dur_ms`, token counts, and `cost_usd` per run, but
contains **no test pass/fail counts** — only workflow-level timing and cost.

### 5. `shell_exec` tool for local-ops agent

`src/tools/shell.rs` provides a broader safe-prefix allowlisted shell executor for the
`local-ops-agent`. It does NOT include `cargo test` or any test runner in its allowlist
and is not exposed to the QA agent.

---

## What is MISSING

### A. `pytest_exec` allowlist blocks all non-Python test runners

The QA agent's system prompt correctly handles Rust/TS/Go detection, but
`src/tools/shell_exec.rs:106–113` silently rejects every non-Python command.
A Rust bake-off task (`cargo test`) will always receive "pytest_exec refused" from
the agent, causing a false QA failure with no useful feedback to the engineer.

### B. No structured QA result parsed by the workflow engine

The engine (line 1226) takes only the first 80 chars of raw agent output.
It does not parse the JSON `{passed, failed, errors, status}` that the QA agent
is supposed to emit. This means:
- `failed_phase` in `perf.rs` is set only if the agent process itself errors out,
  not if tests fail.
- The PM has no machine-readable signal to block advancement or re-delegate to the
  engineer with failure context.
- Token counts in `runs.log` are zero for many runs, suggesting the QA phase is
  aborting before any LLM call.

### C. No failure-context injection back to engineer

When QA returns `status: "fail"`, the workflow engine continues to the next phase
(observe) with no mechanism to:
- Pass test failure details back to the engineer for a fix-and-retest loop.
- Retry the code phase with a "your tests failed: `<details>`" prompt.
The engineer only sees failures through the observe-agent's retrospective.

### D. No test coverage tracking

No tool, workflow step, or perf record captures coverage percentages.
`runs.log` has columns for token cost but none for `tests_passed`, `tests_failed`,
or `coverage_pct`. There is no trending view.

### E. No CI automation

No `.github/workflows/` directory. `cargo test` and the shell harness scripts
(`tests/harness/run_inspection.sh`) are manual-only. The t06 (hacker persona timing)
regression is known-failing with no automated guard.

---

## Top 3 Highest-Value Additions

### 1. Fix `pytest_exec` allowlist to accept all documented test runners

**File**: `src/tools/shell_exec.rs`, function `is_allowed_pytest` (line 106).
**Change**: Replace the Python-only prefix check with a broader allowlist:
`cargo test`, `npm test`, `npx vitest`, `npx jest`, `go test`, `make test`,
and the existing `python3.11 -m pytest` variants.
**Why highest priority**: The QA agent already has correct multi-language logic in its
system prompt. The blocking is entirely in the Rust allowlist. This single function
fix unlocks QA for every non-Python bake-off run with no agent or workflow changes.

### 2. Parse QA JSON result in `engine.rs` and block on failure

**File**: `src/workflow/engine.rs`, near line 1225 (QA phase output handling).
**Change**: After capturing QA output, attempt `serde_json::from_str` to extract
`{status, passed, failed, details}`. If `status == "fail"`:
- Set `failed_phase = Some("qa")` in the perf record.
- Re-inject `details` into the engineer phase prompt for a retry loop (one retry max).
- Gate `advance_workflow_phase` past QA on `status == "pass"` only.
**Why second**: Structural QA JSON already exists; the engine just ignores it. This
gives the PM the machine-readable signal to block, retry, and surface failure context
to the engineer — the core loop that claude-mpm's QA integration provides.

### 3. Add `tests_passed` / `tests_failed` columns to `perf.rs` and `runs.log`

**File**: `src/perf.rs` — add fields to `RunRecord` and `PerfCollector`.
**File**: `src/workflow/engine.rs` — set these fields from the parsed QA JSON.
**Why third**: With fix 2 in place, the parsed counts are available. Writing them to
`runs.log` (one additional tab-separated column) and the per-run JSON enables
trend analysis across builds without any external tooling.

---

## Related Files

| Path | Role |
|------|------|
| `src/tools/shell_exec.rs` | `pytest_exec` allowlist — gap #A lives here |
| `src/workflow/engine.rs:1225` | QA output capture — gap #B lives here |
| `src/perf.rs` | Per-run metrics — gap #D lives here |
| `.open-mpm/agents/qa-agent.toml` | QA agent system prompt + tool allowlist |
| `docs/performance/runs.log` | Per-run perf log (no test counts today) |
| `docs/research/test-infrastructure-survey-2026-04-26.md` | Broader test infra survey |
