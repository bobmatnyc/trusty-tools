# Level 1 Bake-Off Performance Baseline

## Run 2 — First Successful Full Pipeline (Build #45)

**Date:** 2026-04-23
**Build:** open-mpm v0.1.0 build #45
**Workflow:** `prescriptive` (Research → Plan → Code → QA → Observe)
**Status:** ✅ SUCCESS — all 5 phases complete, QA passed 14/15 tests

### Task

> "Write a Python script that formats tabular data as a markdown table. Include a main() function that demonstrates the formatter with sample data."

### Per-Phase Results

| Phase    | Model                          | Duration   | Prompt Tokens | Completion Tokens | Cost      | Status  |
|----------|-------------------------------|------------|---------------|-------------------|-----------|---------|
| research | anthropic/claude-sonnet-4-6   | 12,617 ms  | 23,083        | 359               | $0.0746   | SUCCESS |
| plan     | anthropic/claude-sonnet-4-6   | 54,191 ms  | 13,174        | 2,731             | $0.4024   | SUCCESS |
| code     | anthropic/claude-sonnet-4-6   | 53,672 ms  | 3,848         | 4,036             | $0.3604   | SUCCESS |
| qa       | anthropic/claude-sonnet-4-6   | 17,222 ms  | 14,779        | 1,062             | $0.0603   | SUCCESS |
| observe  | anthropic/claude-sonnet-4-6   | 9,223 ms   | 4,044         | 370               | $0.0177   | SUCCESS |
| **Total**|                               | **146,925 ms** | **58,928** | **8,558**       | **$0.92** |         |

### QA Results

- **14 passed, 1 failed** (pytest)
- Failure: `test_should_handle_empty_string_header` — minimum column width when header is empty string not enforced
- Files extracted to disk mid-pipeline (fix #64): 4 files written before QA ran
  - `test_markdown_table.py` (15 test cases)
  - `markdown_table/core.py`
  - `markdown_table/__init__.py`
  - `markdown_table/__main__.py`

### Key Fixes Applied Since Run 1

| Issue | Fix | Commit |
|-------|-----|--------|
| plan-agent hitting max_turns | Switched opus→sonnet, raised max_turns to 20, added `finish_task` tool | #57 |
| `--workflow` path double-join | Detect `.json`/`/` → treat as literal path | #54 |
| Perf flush only on success | Flush on both success + failure; add `status`/`failed_phase` | #56 |
| CLAUDE_CODE_OAUTH_TOKEN 401 | Removed OAuth from direct API path | #62 |
| code-agent calls `finish_task` with no code | Set `tool_choice=auto`, `use_finish_task=false` | #63 |
| QA runs before files written | `produces_files` PhaseDef flag — extract mid-pipeline | #64 |

### Performance JSON

`docs/performance/runs/20260423-004831-build45.json`

---

## Run 3 — GPT-5.1-Codex A/B (Build #52)

**Date:** 2026-04-23
**Build:** open-mpm v0.1.0 build #52
**Workflow:** `prescriptive-gpt` (Research → Plan → Code[gpt-5.1-codex] → QA → Observe)
**Status:** FAILED — QA collection error (0 passed, 0 failed, 2 collection errors)

### Task

Same task as Run 2.

### Per-Phase Results

| Phase   | Model                        | Duration  | Prompt Tokens | Completion Tokens | Cost     | Status  |
|---------|------------------------------|-----------|---------------|-------------------|----------|---------|
| research | anthropic/claude-sonnet-4-6 | 9,000 ms  | 22,567        | 324               | $0.0726  | SUCCESS |
| plan    | anthropic/claude-opus-4-6    | 55,600 ms | 12,849        | 3,032             | $0.4201  | SUCCESS |
| code    | openai/gpt-5.1-codex         | 22,300 ms | 3,392         | 2,483             | $0.0474  | SUCCESS |
| qa      | anthropic/claude-sonnet-4-6  | 16,200 ms | 13,374        | 958               | $0.0545  | FAILED  |
| observe | anthropic/claude-sonnet-4-6  | 9,300 ms  | 3,578         | 442               | $0.0174  | SUCCESS |
| **Total**|                             | **112,400 ms (1:52)** | | | **$0.6120** |  |

### QA Results

- **0 passed, 0 failed, 2 collection errors**
- Root cause: GPT called `finish_task` on turn 0 with only 82 chars of content. No files were written to disk.
- `tool_choice=any` allowed GPT to treat `finish_task` as a valid first action before emitting any code.

### Performance JSON

`docs/performance/runs/20260423-011958-build52.json`

### Comparison: Run 2 vs Run 3

| Metric | Run 2: prescriptive (all sonnet) | Run 3: prescriptive-gpt (gpt-5.1-codex code) |
|--------|----------------------------------|-----------------------------------------------|
| Status | SUCCESS | FAILED (QA collection error) |
| Total cost | $0.92 | $0.61 |
| Total time | ~2.5 min | 1:52 |
| QA pass rate | 14/15 | 0/0 (no files on disk) |
| Code phase cost | $0.36 | $0.047 (-87%) |
| Code phase time | 53.7s | 22.3s (-58%) |
| Files on disk | 4 | 0 |

### Lessons Learned

- GPT-5.1-Codex is significantly cheaper and faster for code generation when it works (87% cost reduction, 58% time reduction for the code phase)
- It does not follow the `## File:` markdown output convention reliably with `tool_choice=any` — it invoked `finish_task` as its first action with no code
- A `write_file` tool approach (explicit tool call per file) would be more reliable than regex extraction of markdown blocks
- The plan phase (opus-4-6 at $0.42) is the dominant cost driver in both runs — switching plan to sonnet would save more than switching the code model
- `tool_choice=any` is the root cause: it lets the model treat any available tool, including `finish_task`, as a valid first action; fix is `tool_choice=auto` (same fix applied to code-agent in #63)
- Next step: implement a `write_file` tool and test whether GPT will use it correctly

---

## Run 1 — Partial Failure (Build #5)

**Date:** 2026-04-22
**Build:** open-mpm v0.1.0 build #5 (release binary)
**Workflow:** `prescriptive` (Research -> Plan -> Code -> QA -> Observe)

---

## Task

> "Write a Python script that takes tabular data and outputs a formatted markdown table.
> The script should accept data as a list of dicts and a list of column headers,
> then print a properly formatted markdown table with aligned columns."

Task file: `/tmp/bakeoff-l1-task.md`
Command: `./target/release/open-mpm --workflow prescriptive --task-file /tmp/bakeoff-l1-task.md --out-dir /tmp/bakeoff-l1-out`

---

## Run Outcome

**Status: PARTIAL FAILURE**

The run completed the `research` phase successfully but the `plan` phase aborted
with `chat_with_tools exceeded max_turns (12) without a final text response`.

Root cause: The `plan-agent` (using `anthropic/claude-opus-4-6`) repeatedly
produced plain-text responses mid-task instead of calling a tool or delivering
a final structured result. The engine injected retry errors at turns 3, 5, 7,
and 9 but the agent did not converge within the 12-turn budget.

The `code`, `qa`, and `observe` phases did not run.
No perf JSON was written to `docs/performance/runs/` because the perf flush
only executes on successful workflow completion (see `workflow/engine.rs:244`).

---

## Per-Phase Breakdown

| Phase    | Model                          | Duration  | Prompt Tokens | Completion Tokens | Cache Read | Cache Create | Status  |
|----------|-------------------------------|-----------|---------------|-------------------|------------|--------------|---------|
| research | anthropic/claude-sonnet-4-6   | 67,782 ms | 41,010        | 2,843             | 18,000     | 0            | SUCCESS |
| plan     | anthropic/claude-opus-4-6     | ~103,937 ms | 82,202      | 5,171             | unknown    | unknown      | FAILED  |
| code     | anthropic/claude-opus-4-6     | —         | —             | —                 | —          | —            | SKIPPED |
| qa       | anthropic/claude-sonnet-4-6   | —         | —             | —                 | —          | —            | SKIPPED |
| observe  | anthropic/claude-sonnet-4-6   | —         | —             | —                 | —          | —            | SKIPPED |

**Total wall-clock time (partial):** ~171,719 ms (~2 min 51 s)

---

## Token Totals (Partial Run)

| Metric                 | Value   |
|------------------------|---------|
| Total prompt tokens    | 123,212 |
| Total completion tokens| 8,014   |
| Total tokens           | 131,226 |
| Cache read tokens      | 18,000 (research phase only; plan not broken out) |
| Cache creation tokens  | 0       |

**Prompt caching observation:** Cache read tokens were captured for the
`research` phase (18,000 tokens read from cache out of 41,010 prompt tokens
= ~43.9% cache hit). The `cache_read` and `cache_creation` fields are
populated in the phase-complete log line, confirming the perf instrumentation
works correctly when a phase succeeds.

---

## Estimated Cost (Partial Run)

Pricing reference: OpenRouter published rates as of 2026-04 for Anthropic models.

| Phase    | Model            | Input Cost  | Cache Read Cost | Output Cost | Subtotal   |
|----------|-----------------|-------------|-----------------|-------------|------------|
| research | claude-sonnet-4-6 | $0.069030  | $0.005400       | $0.042645   | $0.117075  |
| plan     | claude-opus-4-6   | $1.233030  | —               | $0.387825   | $1.620855  |
| **Total**|                  |             |                 |             | **$1.737930** |

Pricing assumptions:
- `claude-sonnet-4-6`: $3.00/MTok input, $0.30/MTok cache read, $15.00/MTok output
- `claude-opus-4-6`: $15.00/MTok input, $75.00/MTok output

> Note: The opus-4-6 plan phase dominates cost (~93% of total) despite
> failing, highlighting that the max_turns budget burn is expensive at Opus pricing.

---

## Issues Encountered

1. **`--workflow` path vs name handling bug:** Passing the full config path
   (`config/workflows/prescriptive.json`) caused a double-path join error:
   `workflow file not found: config/workflows/config/workflows/prescriptive.json.json`.
   The workaround is to pass only the workflow name (`prescriptive`).
   The engine should strip path and extension from full-path inputs.

2. **plan-agent max_turns exceeded:** The `plan-agent` produced plain-text
   responses instead of tool calls across 12 turns. The engine correctly
   detected and retried (4 retries logged at turns 3, 5, 7, 9) but the
   agent did not recover. Possible causes:
   - Opus-4-6 prompt does not constrain the agent to call `phase_audit` first
   - The plan-agent's context template (Task + Research) may be producing a
     context that Opus is eager to respond to directly
   - The `max_turns` budget of 12 may be insufficient for Opus' more verbose
     reasoning style at this task complexity

3. **No perf JSON written on failure:** The `PerfCollector::flush()` is called
   only after the full workflow loop completes (`engine.rs:244`). A partial
   run leaves no artifact in `docs/performance/runs/`. Consider flushing
   partial data on failure for diagnostics.

---

## Recommendations for Next Run

1. Increase `max_turns` for `plan-agent` to 16-20, or investigate why
   Opus-4-6 does not tool-call reliably in the plan context.
2. Fix the `--workflow` path handling to accept both full path and bare name.
3. Add failure-mode perf flushing so partial runs are still recorded.
4. Once a full run completes, the `analyze.py` script will display the
   structured table; this baseline was captured manually from log output.

---

## Raw Log Reference

Full run log: `/tmp/bakeoff-l1-run.log`
Phase audit JSONL: `/tmp/bakeoff-l1-out/phase-audit.jsonl` (7 audit entries —
confirms plan-agent did reach `phase:complete` in its audit tool but never
emitted a proper IPC result message)

---

## Run 4 — Level 1 regression with new systems (Build #63)

- **Date:** 2026-04-23
- **Workflow:** `prescriptive`
- **Status:** SUCCESS (partial QA verdict — 44/50 tests, 6 CLI failures due to PYTHONPATH)
- **Per-phase:**
  - research: 17.4s, $0.15
  - plan: 6.8s, $0.30
  - code: 109.5s, $0.79
  - qa: 16.0s, $0.09
  - observe: 9.5s, $0.02
- **Total:** 159.3s, $1.35, tokens: 98,349p / 11,865c
- **Files:** `table_formatter/__init__.py`, `table_formatter/__main__.py`, `table_formatter/core.py`, `test_table_formatter.py` (38 tests), `README.md`
- **Notes:** Goal block fence not emitted by planner (fix #80). 6 CLI test failures = PYTHONPATH issue (fix #79). Context manager + memory indexer active but no evictions.
- **Perf JSON:** `docs/performance/runs/20260423-020518-build63.json`

---

## Run 5 — Level 2 Git Log Analyzer, first attempt (Build #61)

- **Date:** 2026-04-23
- **Workflow:** `prescriptive`
- **Status:** PARTIAL FAILURE — QA phase failed: `.open-mpm/` directory missing (fix #78)
- **Per-phase:**
  - research: 13.0s, $0.09
  - plan: 7.5s, $0.32
  - code: 118.2s, $0.86
  - qa: failed, $0.00
- **Total:** 139.2s, $1.26 (code phase only ran 3 phases)
- **Files extracted before QA failure:** `git_analyzer/{__init__,__main__,core,metrics,parser,reporter}.py`, `pyproject.toml`, `README.md`, `test_git_analyzer.py` (30 tests)
- **Manual pytest:** 17/30 pass — core parser bug (multi-commit fixture only parsed 2/5 commits), bus factor formula mismatch
- **Code quality:** strong structure, dataclasses, 4 output formats, subprocess git calls. Weaknesses: flat layout vs src/ spec, tests in root vs tests/, extra core.py
- **Perf JSON:** `docs/performance/runs/20260423-020514-build61.json`
