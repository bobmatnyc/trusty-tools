# Bake-Off v0.1.6 Comprehensive Analysis

**Date:** 2026-04-23
**Harness version:** v0.1.6
**Run type:** Legacy monolithic path (wave loop rejected for all 5 levels)
**Analyst:** Research Agent

---

## Executive Summary

The v0.1.6 harness achieved a 3/5 clean-pass rate (L1, L2, L5) with one environmental failure (L3) and one partial pass due to a test-fixture wiring gap (L4). The underlying code quality across all levels is high — the L3 and L4 failures are both one-line fixable problems, not architectural defects. The wave loop, intended to enable parallel multi-agent file generation, was attempted on every level but rejected every time due to same-wave dependency violations in the plan agent's output; the harness correctly fell back to legacy monolithic mode in all cases. Two systemic harness gaps explain most of the friction: (1) the QA phase does not run `uv sync` / `pip install -e .[test]` before invoking pytest, causing undeclared-dep collection failures; (2) there is no workspace isolation mechanism to prevent stale artifacts from prior runs from confusing the research phase.

---

## Per-Level Scorecard vs Rubric

### Level 1 — Markdown Table Formatter

**Result:** 73/73 PASS | Duration: ~13 min | Path: legacy monolithic

| Dimension | Weight | Score (1-5) | Weighted |
|---|---|---|---|
| Correctness | 30% | 5 — 73/73 tests pass, full edge case coverage | 1.50 |
| Code Quality | 25% | 5 — type hints throughout, CJK-aware padding, clean module separation | 1.25 |
| Testing | 20% | 5 — 4 test modules, 38 reader tests, CLI tests, filter tests, fixtures | 1.00 |
| Error Handling | 15% | 4 — graceful CLI errors; no evidence of missing-file path testing | 0.60 |
| Architecture | 5% | 5 — reader/formatter/filters/CLI clean separation; library-reusable | 0.25 |
| Documentation | 5% | 3 — no README found at output root (not confirmed present) | 0.15 |
| **Total** | | | **4.75 / 5.00** |

**Notes:** Package structure is src-layout with `setuptools`, `pythonpath` in `pytest.ini_options` — correctly avoids the system-package shadowing trap that bit L3. Architecture notably clean for a Level 1 challenge. Missing top-level README is the only observable gap.

---

### Level 2 — Multi-Repo Code Quality Analyzer

**Result:** 59/59 PASS | Duration: ~28 min | Path: legacy monolithic

| Dimension | Weight | Score (1-5) | Weighted |
|---|---|---|---|
| Correctness | 25% | 5 — 59/59 tests pass, lizard CSV flag bug self-corrected | 1.25 |
| Code Quality | 20% | 4 — clean modules, type hints; flat layout (not src/) | 0.80 |
| Architecture | 15% | 4 — analyzer/scorer/reporter/repo_access separation; ThreadPoolExecutor parallelism | 0.60 |
| Testing | 15% | 5 — 6 test modules, mocked subprocess, integration tests | 0.75 |
| Error Handling | 10% | 4 — shutil.which guards for missing tools | 0.40 |
| Documentation | 10% | 3 — no top-level README visible in output dir root | 0.30 |
| Packaging bonus | 5% | 3 — pyproject.toml present, flat layout not canonical src/ | 0.15 |
| **Total** | | | **4.25 / 5.00** |

**Notes:** Note the plan agent generated paths under `repo_quality/` but the code agent wrote to `code_quality_analyzer/` — a path-namespace drift that the wave loop would have caught (the wave loop rejection was partially caused by this). The L2 challenge spec calls for `src/git_analyzer/` canonical layout; the produced layout is flat. Functional correctness is excellent.

---

### Level 3 — Weather Alerting Service

**Result:** 0/0 FAIL (exit 4) | Duration: ~19 min | Path: legacy monolithic

| Dimension | Weight | Score (1-5) | Weighted |
|---|---|---|---|
| Correctness | 20% | 3 — code appears complete (62 tests written) but none executed | 0.60 |
| Code Quality | 15% | 4 — FastAPI app factory, Pydantic v2, clean routers, type hints | 0.60 |
| Architecture | 20% | 4 — app factory pattern, scheduler isolation, mock provider, lifespan | 0.80 |
| Testing | 15% | 4 — 62 tests written across 5 test files; zero executed | 0.60 |
| Error Handling | 15% | 4 — mock/demo mode for offline testing; graceful scheduler errors | 0.60 |
| Documentation | 5% | 4 — README.md present | 0.20 |
| Docker bonus | 10% | 5 — Dockerfile and docker-compose.yml both present | 0.50 |
| **Total** | | | **3.90 / 5.00** |

**Notes:** The score is artificially suppressed by the environmental failure. The fix is trivial: add `pythonpath = ["src"]` to `[tool.pytest.ini_options]` (or run `pip install -e .` before pytest). The code itself has no known correctness issues — it was designed from scratch without reference to the stale prior-run artifacts. Research agent confusion is recorded but the final code is a fresh implementation. Docker deliverable is fully present. The weather_alerter system-package conflict is a machine-specific issue; Docker CI would not be affected.

**Estimated corrected score if env fixed:** ~4.5/5.0

---

### Level 4 — Document Processing Pipeline

**Result:** 40/53 PASS (13 fail) | Duration: ~18.5 min | Path: legacy monolithic

| Dimension | Weight | Score (1-5) | Weighted |
|---|---|---|---|
| Correctness | 15% | 3 — 40/53 pass; 12 API failures from missing lifespan, 1 HTML extractor mismatch | 0.45 |
| Code Quality | 15% | 5 — src-layout, type hints, async throughout, clean module separation | 0.75 |
| Architecture | 30% | 5 — PipelineStage ABC, topological sort, entry-point plugin discovery, job queue | 1.50 |
| Testing | 15% | 4 — 9 test modules, unit + integration; lifespan gap reduces score | 0.60 |
| Error Handling | 10% | 4 — stage error isolation, FTS5 fallback, NLP model fallback | 0.40 |
| Documentation | 10% | 4 — README.md present | 0.40 |
| Extensibility bonus | 5% | 5 — importlib.metadata entry_points plugin registry | 0.25 |
| **Total** | | | **4.35 / 5.00** |

**Notes:** The architecture score is genuinely excellent for this level — topological stage ordering, async job queue, SQLite FTS5 with BM25+Porter stemming, and a proper entry-points plugin system are all present. The 13 failures are mechanical: `AsyncClient(transport=transport, base_url="http://test")` must become `AsyncClient(app=app, lifespan="on", base_url="http://test")` for 12 of them. One additional HTML extractor title-extraction mismatch requires ~5-line fix. Estimated corrected score: 4.8/5.0.

---

### Level 5 — Team Task Board

**Result:** 57/57 PASS | Duration: ~26 min | Path: legacy monolithic

| Dimension | Weight | Score (1-5) | Weighted |
|---|---|---|---|
| Correctness | 15% | 5 — 57/57 pass, auth/CRUD/WebSocket/activity all covered | 0.75 |
| Code Quality | 10% | 5 — clean routers/models/schemas/services layout, type hints | 0.50 |
| Architecture | 25% | 5 — JWT auth, SQLAlchemy async, Redis pub/sub, WebSocket broadcast, Docker Compose | 1.25 |
| Testing | 15% | 5 — 6 test modules, 1241 lines, auth + CRUD + WebSocket tests | 0.75 |
| Error Handling | 10% | 4 — JWT validation, auth middleware; no explicit 422 handler tests observed | 0.40 |
| Documentation | 10% | 4 — README.md present | 0.40 |
| Real-time/Docker/CI bonus | 15% | 4 — WebSocket + Docker Compose present; no .github/workflows CI file found | 0.60 |
| **Total** | | | **4.65 / 5.00** |

**Notes:** This is the harness's strongest result given task complexity. The QA phase had pre-flight friction (fakeredis, pydantic[email] not installed) — these are declared as `test` extras in pyproject.toml but the harness did not run `pip install -e .[test]`. GitHub Actions CI workflow not present (reduces bonus from 5 to 4). The L5 wave dependency graph has 9 violations (same-wave deps within schemas and tests waves), which triggered the legacy fallback.

---

## Aggregate Summary

| Level | Tests | Score | Key Blockers |
|---|---|---|---|
| L1 | 73/73 | 4.75/5.0 | None |
| L2 | 59/59 | 4.25/5.0 | Non-canonical layout, no README at root |
| L3 | 0/0 (env fail) | 3.90/5.0 | System package shadows local module; no `pythonpath` in pytest config |
| L4 | 40/53 | 4.35/5.0 | `AsyncClient` missing `lifespan="on"`; HTML extractor mismatch |
| L5 | 57/57 | 4.65/5.0 | Missing CI config; `uv sync` not run pre-QA |
| **Avg** | | **4.38/5.0** | Wave loop rejected on all 5 levels |

---

## Top 10 Improvement Opportunities

Ranked by impact/effort ratio (Impact H/M/L, Effort S/M/L).

### 1. QA Phase: Pre-flight `uv sync` / `pip install -e .[test]`

**Category:** Harness fix
**Impact:** High — directly caused L5 friction and would have fixed L3 if using editable install
**Effort:** S — add a single shell step before `pytest` invocation

**Root cause:** The QA agent invokes `pytest` without first ensuring the venv has all declared test dependencies. When a project uses `[project.optional-dependencies] test = [...]`, those extras are not auto-installed unless explicitly requested.

**Fix:** QA phase preamble should run:
```bash
cd <project_dir> && uv sync --extra test 2>/dev/null || pip install -e ".[test]"
```
This also installs the project in editable mode, which resolves the L3 module-shadowing issue for src-layout packages.

---

### 2. QA Phase: Enforce `pythonpath` in pytest config for src-layout projects

**Category:** Harness fix + instruction fix
**Impact:** High — L3 zero-collection failure is 100% attributable to this
**Effort:** S — add `pythonpath = ["src"]` to `[tool.pytest.ini_options]` via agent instruction or harness pre-check

**Root cause:** L3 `weather_alerter` package collided with a system-installed package at `/opt/homebrew/lib/python3.11/site-packages/`. The local `src/` was not on the Python path, so the system package resolved first. The fix is a single `pyproject.toml` line.

**Fix:** Add to python-engineer system prompt: "For any src-layout project (code lives in `src/`), always include `pythonpath = ['src']` in `[tool.pytest.ini_options]`." The harness QA phase should also detect src-layout and validate this config before running pytest.

---

### 3. Wave Loop: Plan Agent Same-Wave Dependency Violations

**Category:** Instruction fix (prompt engineering)
**Impact:** High — wave loop was rejected on ALL 5 levels; the intended parallel multi-agent code generation was never exercised
**Effort:** M — requires prompt update plus possibly a repair pass in the validator

**Root cause:** The plan agent consistently puts files with intra-wave dependencies in the same wave. Examples:
- L5 Wave 1: `database.py` depends on `config.py` (both in Wave 1)
- L5 Wave 3: `schemas/column.py` depends on `schemas/task.py` (both in Wave 3)
- L5 Wave 7: All test files depend on `conftest.py` (all in Wave 7)
- L1/L2/L4 each had at least one similar violation

**Fix options:**
- **Prompt fix:** Add explicit rule to plan agent: "A file may only `depends_on` files that appear in strictly earlier waves. Files in the same wave must have zero dependencies on each other. Tests depending on conftest must go in a later wave than conftest."
- **Validator repair:** Rather than rejecting the entire plan, the validator could attempt topological reordering: if a wave N file depends on a wave N file, move the dependency to wave N-1 and retry.
- **Stricter schema:** Include a `validator_notes` field in the plan output asking the agent to self-check before finalizing.

---

### 4. FastAPI Test Client: `lifespan="on"` Standard

**Category:** Instruction fix
**Impact:** High — caused 12 of 13 L4 failures; will recur for any FastAPI app using lifespan-managed state
**Effort:** S — one instruction line addition

**Root cause:** The python-engineer writes tests using `AsyncClient(transport=ASGITransport(app=app), base_url="http://test")` which does not invoke the `@asynccontextmanager lifespan` hook. Any `app.state.X` set during lifespan is uninitialized, causing `AttributeError`.

**Fix:** Add to python-engineer system prompt: "When writing FastAPI integration tests, always use `AsyncClient(app=app, base_url='http://test', lifespan='on')`. Never use the `transport=ASGITransport(...)` pattern for tests against apps with lifespan hooks, as this bypasses startup/shutdown."

---

### 5. Research Phase: Workspace Isolation / Stale Artifact Detection

**Category:** Harness fix
**Impact:** High — L3 research phase was confused by a pre-existing `weather_service/` directory from a prior run
**Effort:** M — requires output directory management before each run

**Root cause:** Prior runs leave `out/l3-*/weather_service/` directories visible in the filesystem. The research agent may scan the working directory and find artifacts from a previous attempt, causing it to report "already complete" when it should build fresh.

**Fix:** Before each level run, the harness should either:
1. Create a fresh timestamped output dir (already partially done, but the agent may still glob parent dirs)
2. Run in a clean Git worktree (`git worktree add tmp-run-l3`)
3. Add a `[research] scope = "out/<current_dir_only>"` constraint so the research agent does not walk parent directories
4. Emit a warning if a prior-run artifact directory is found in the workspace

---

### 6. Agent Instructions: Declare All Test Dependencies in pyproject.toml

**Category:** Instruction fix
**Impact:** Medium — caused L5 pre-flight friction; would affect any level using optional test deps
**Effort:** S — one instruction sentence

**Root cause:** L5 `fakeredis` and `pydantic[email]` were used in tests but not declared in `[project.optional-dependencies] test`. The QA agent had to install manually. (Note: post-fix the L5 pyproject.toml does correctly declare these — but the issue recurs whenever a new test dep is added without updating the manifest.)

**Fix:** Add to python-engineer: "Ensure ALL packages imported in test files are declared in `[project.optional-dependencies] test = [...]`. Run a final check: for each `import X` in `tests/`, verify X is either stdlib or listed in test extras."

---

### 7. Wave Loop: Validator Should Attempt Topological Repair Before Fallback

**Category:** Harness fix (new feature)
**Impact:** Medium — would enable the wave loop to engage even when the plan agent makes minor ordering mistakes
**Effort:** M — topological sort on dependency graph is straightforward Rust code

**Root cause:** The current validator rejects the entire plan on the first violation. A smarter approach would be to attempt to repair the wave assignment by running Kahn's algorithm on the declared dependency graph, then re-validating. Only reject if the graph has a cycle.

**Fix:** Implement `plan_validator::repair_waves(assignments)` that:
1. Builds a DAG from all `depends_on` edges
2. Runs topological sort to determine correct wave order
3. Reassigns each file to the wave corresponding to its topological position
4. Returns the repaired plan (and logs a warning about the original violations)

---

### 8. L2: Canonical src-layout and Correct Package Name

**Category:** Instruction fix
**Impact:** Medium — the L2 challenge spec requires `src/git_analyzer/` layout; the agent produced a flat `code_quality_analyzer/` layout
**Effort:** S — add explicit structure requirement to L2 prompt or python-engineer instructions

**Root cause:** The L2 challenge spec explicitly shows the expected directory structure as `src/git_analyzer/`. The agent chose its own package name (`code_quality_analyzer`) and flat layout. This would fail structure-based rubric checks even though functional tests pass.

**Fix:** Either (a) pass the target package name and layout explicitly in the task description, or (b) add to python-engineer instructions: "If the problem specifies a directory layout, reproduce it exactly including package name."

---

### 9. L5: GitHub Actions CI Workflow Missing

**Category:** Agent quality (instruction fix)
**Impact:** Medium — L5 bonus rubric allocates points for CI; not generating it leaves points on the table
**Effort:** S — the L5 spec explicitly requires `.github/workflows/ci.yml`

**Root cause:** The code agent did not generate the CI workflow file. Given L5 runs ~26 min and produces 40+ files, the CI file may have been de-prioritized or dropped.

**Fix:** Add to the L5 (or general) system prompt: "For full-stack projects: always generate `.github/workflows/ci.yml` that runs `pytest` and linting. This is required for the Docker/CI bonus points."

---

### 10. QA Phase: Retry After `pip install -e .[test]` on Collection Error (exit 4)

**Category:** Harness fix
**Impact:** Medium — L3's exit 4 was not retried; a smart retry loop would have recovered
**Effort:** M — requires QA agent to parse exit code and attempt recovery

**Root cause:** Pytest exit code 4 means "no tests collected." When this occurs, the QA agent should attempt a recovery sequence before declaring failure: (1) run `pip install -e .[test]`, (2) check for `pythonpath` config, (3) retry pytest with `--import-mode=importlib`.

**Fix:** Add QA agent recovery protocol: "If pytest exits with code 4 (no tests collected): (1) run `pip install -e .` and retry; (2) if still failing, add `--import-mode=importlib` and retry; (3) if still failing, run `pip show <package-name>` to check which installation is being used and report the conflict."

---

## Proposed GitHub Issue Titles and Descriptions

The following are ready to file. The PM should review and approve before submission.

---

### Issue 1: `[harness] QA phase must run uv sync / pip install -e .[test] before pytest`

**Labels:** bug, harness, qa
**Priority:** High

**Description:**
The QA phase currently invokes `pytest` without first ensuring declared test dependencies are installed. This caused two failures in the v0.1.6 bake-off run:
- L3: system-installed `weather_alerter` package shadowed the local module because the local package was never installed in editable mode
- L5: `fakeredis` and `pydantic[email]` (declared as test extras) were not installed; QA agent had to install manually before collection succeeded

**Proposed fix:** Add a mandatory pre-flight step to the QA phase:
```bash
cd <project_dir>
uv sync --extra test 2>/dev/null || pip install -e ".[test]" || pip install -e "."
```
This should run before any `pytest` invocation and should be logged so failures are diagnosable.

**Acceptance criteria:** L3 re-run with this fix produces non-zero test collection and all 62 tests pass.

---

### Issue 2: `[harness] Wave validator should repair wave ordering before rejecting plan`

**Labels:** enhancement, wave-loop, plan-agent
**Priority:** High

**Description:**
The wave loop validator rejected all 5 levels in the v0.1.6 bake-off run due to same-wave dependency violations. The violations are consistently mechanical (e.g., `conftest.py` and its test files placed in the same wave, schema files with intra-wave deps), not logical cycles. The validator currently rejects the entire plan on first violation and falls back to legacy monolithic mode.

**Proposed fix:** Implement a `repair_waves()` function in the plan validator that:
1. Extracts all files and their `depends_on` edges
2. Runs Kahn's topological sort
3. Reassigns each file to the correct wave based on its topo-sort depth
4. Returns the repaired plan with a warning log

Only reject outright if a true cycle is detected (unresolvable).

**Acceptance criteria:** L1-L5 wave plans are repaired and the wave code-generation path is exercised at least once end-to-end.

---

### Issue 3: `[instruction] FastAPI lifespan not wired in test client — add pythonpath and lifespan="on" to prompt`

**Labels:** bug, agent-instructions, qa
**Priority:** High

**Description:**
Two recurring patterns cause avoidable test failures:

1. **FastAPI lifespan not wired:** The python-engineer writes `AsyncClient(transport=ASGITransport(app=app))` which bypasses the lifespan context manager. Any state initialized in lifespan (e.g., `app.state.db`) is unset, causing `AttributeError`. This caused 12 of 13 L4 failures.

2. **Missing `pythonpath` for src-layout:** The python-engineer does not consistently add `pythonpath = ["src"]` to `[tool.pytest.ini_options]` for src-layout projects. This caused L3's 0-test-collection failure.

**Proposed fix:** Update the python-engineer system prompt to include:
- "For FastAPI apps with a lifespan hook, write integration tests using `AsyncClient(app=app, base_url='http://test', lifespan='on')`. Never use the `ASGITransport` pattern for apps with lifespan state."
- "For any src-layout project (source under `src/`), always include `pythonpath = ['src']` in `[tool.pytest.ini_options]`."

**Acceptance criteria:** L3 and L4 re-runs with updated instructions produce 0 lifespan-related failures.

---

### Issue 4: `[harness] Research phase must not scan prior-run output directories`

**Labels:** bug, harness, research-phase
**Priority:** Medium

**Description:**
The L3 research phase found a pre-existing `weather_service/` directory from a prior run in the output tree. This caused the agent to conclude the work was already complete and reuse stale context rather than performing fresh analysis. The final code was rebuilt correctly (per the workflow-report), but the confusion added latency and risk.

**Proposed fix:**
1. The harness should pass the research agent an explicit scope constraint: "Only analyze files under `<current_output_dir>/`. Ignore all other paths."
2. Alternatively, the harness should run `rm -rf <output_dir>` before starting a new run for the same level, or use Git worktrees for isolation.
3. Add a research-phase warning: "If you find an existing implementation in the target directory from a prior run, do NOT reuse it. Always start fresh from the problem specification."

**Acceptance criteria:** L3 re-run with pre-existing `weather_service/` still present does not reuse stale context.

---

### Issue 5: `[instruction] Plan agent produces same-wave intra-wave dependencies — add explicit rule`

**Labels:** bug, agent-instructions, plan-agent, wave-loop
**Priority:** High

**Description:**
The plan agent consistently produces wave plans with same-wave dependency violations. In the v0.1.6 run, all 5 levels had at least one violation:
- L1: `test_cli.py` (wave 3) depends on `__main__.py` (wave 3)
- L2: `test_main.py` (wave 4) depends on `main.py` (wave 4)
- L4: `plugins.py` (wave 2) depends on `pipeline.py` (wave 2)
- L5: 9 violations including all test files depending on conftest in the same wave

This causes the wave validator to reject every plan and fall back to legacy monolithic mode, defeating the purpose of the wave loop.

**Proposed fix:** Add explicit rules to the plan agent system prompt:
- "A file in wave N may ONLY have `depends_on` entries pointing to files in waves 1 through N-1. Never list a file from the same wave as a dependency."
- "Tests always go in the last wave, one wave after all non-test source files."
- "conftest.py must be in an earlier wave than all test_*.py files."
- "Schema files that import from other schema files must be in later waves."

**Acceptance criteria:** At least 3 of 5 levels pass wave validation without repair on the next bake-off run.

---

## Harness vs Agent Quality Issues Summary

### Harness Fixes Required (infrastructure problems)

| Issue | Affected Levels | Fix Complexity |
|---|---|---|
| No `uv sync` before pytest | L3, L5 | S |
| Wave validator rejects instead of repairing | L1-L5 | M |
| Research phase scans prior-run artifacts | L3 | M |
| No QA recovery on pytest exit 4 | L3 | M |

### Agent Instruction Fixes Required (prompt engineering)

| Issue | Affected Levels | Fix Complexity |
|---|---|---|
| Plan agent same-wave dep violations | L1-L5 | S (prompt) |
| FastAPI `lifespan="on"` not used in tests | L4 | S (prompt) |
| `pythonpath = ["src"]` for src-layout | L3 | S (prompt) |
| Test deps not declared in pyproject.toml | L5 | S (prompt) |
| CI workflow not generated for L5 | L5 | S (prompt) |
| Non-canonical package layout/name | L2 | S (prompt) |

---

## Wave Loop Analysis

The wave loop was attempted on all levels where a plan existed. Every plan was rejected by the validator due to same-wave dependency violations. The violation counts by level:

| Level | Violations | Primary Type |
|---|---|---|
| L1 | 1 | test → source in same wave |
| L2 | 1 | integration test → main.py in same wave |
| L3 | N/A | No assignments.json found (legacy path from start) |
| L4 | 1 | plugins.py → pipeline.py in same wave |
| L5 | 9 | schema→schema, test→conftest, database→config in same wave |

The L5 violations pattern is particularly instructive: the plan agent treats `conftest.py + test_*.py` as a single cohesive unit (which they logically are), but fails to recognize that conftest must be written *before* tests can import from it. This is a learnable pattern.

**Recommendation:** Implement Issue 2 (topological repair) as the highest-leverage engineering change. It would immediately unblock all 5 levels' wave paths without requiring perfect plan agent output.

---

## Deliverable Assessment vs Rubric

| Deliverable | L1 | L2 | L3 | L4 | L5 |
|---|---|---|---|---|---|
| Core implementation | Yes | Yes | Yes | Yes | Yes |
| Test suite (5+) | Yes (73) | Yes (59) | Yes (62 written) | Yes (53 written) | Yes (57) |
| pyproject.toml | Yes | Yes | Yes | Yes | Yes |
| README.md | No | No | Yes | Yes | Yes |
| CLI entry point | Yes | Yes | Yes | Yes | Yes |
| Docker/Compose | N/A | N/A | Yes | N/A | Yes (partial — no CI) |
| Architecture diagram | N/A | N/A | N/A | Not confirmed | N/A |
| Additional tests (5+) | Yes | Yes | Yes | Yes | Yes |

---

**Document Status:** Complete
**Last Updated:** 2026-04-23
**Next Actions:** File GitHub issues 1-5 after PM review; re-run L3 and L4 with targeted one-line fixes to confirm scores improve to ~4.5-4.8.
