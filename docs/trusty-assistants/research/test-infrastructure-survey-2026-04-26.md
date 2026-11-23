# Test Infrastructure Survey

**Date**: 2026-04-26
**Scope**: End-to-end survey of all test infrastructure for the testing expansion sprint
**Author**: Research agent (Claude Sonnet 4.6)

---

## 1. What Are the "masa-persona tests t01-t07"?

The t01-t07 IDs are an **informal, manually-executed** test suite that Masa (Bob Matsuoka) runs as an end-user sanity check. There is **no single definition file** that enumerates all seven; they are referenced by convention in session notes and commit messages. The partial reconstruction from available evidence:

| ID | Task / Scenario | What It Exercises | Last Known Status |
|----|----------------|-------------------|-------------------|
| t01 | Regression: `py-simple.txt` (Python CLI word-count with pytest) | prescriptive workflow, python-engineer agent | Passing (v0.2.12) |
| t02 | Regression: `rs-simple.txt` (Rust async retry module with tokio tests) | prescriptive workflow, engineer agent | Passing (v0.2.12) |
| t03 | Regression: `ts-simple.txt` (TypeScript Result<T,E> with vitest) | prescriptive workflow, engineer agent | Passing (v0.2.12) |
| t04 | CTRL: `add project /Users/masa/Projects/open-mpm` | CTRL project management tool routing | Fixed in `f098bcd`; was failing due to missing `AddProjectTool` in `run_pm_task_with_session` |
| t05 | CTRL: `list projects` | CTRL list-projects tool routing | Fixed in `f098bcd`; same root cause as t04 |
| t06 | Persona: hacker persona run, expected <90s | `phases_to_skip("hacker")` must skip `plan` phase | Still failing — `plan` not in hacker skip set; Opus runs plan phase (~120s) |
| t07 | Implementation: `write tests for the intent classifier` | prescriptive workflow, full pipeline | Unblocked by Bedrock routing; was OpenRouter 402 credit exhaustion |

**Key caveat**: t01–t03 are inferred from the regression task files in `.open-mpm/tasks/regression/`. The session summary says "t01-t05 passing" and "t07 unblocked (Bedrock)" as of v0.2.12. t06 is explicitly documented as still failing in `docs/research/failing-tests-210-analysis.md`.

---

## 2. Test Runners and How to Invoke Them

### 2a. Rust unit tests (`cargo test`)

**Command**: `cargo test`
**What it runs**: ~700 unit tests in `src/` (inline `mod tests` blocks), plus integration tests in `tests/`.
**No API key required.** LLM-dependent tests are marked `#[ignore]`.

Specific integration test targets:
```bash
cargo test --test api_e2e        # HTTP API tests (4 non-ignored, 3 ignored)
cargo test --test cli_project    # CLI inspect dry-run tests (2 tests)
cargo test --test api_e2e -- --ignored   # Full API suite (requires OPENROUTER_API_KEY)
```

### 2b. Harness inspection script

**Command**: `./tests/harness/run_inspection.sh`
**What it runs**: 10 agent-routing dry-run assertions via `open-mpm inspect --dry-run`
**No API key required.**

```bash
./tests/harness/run_inspection.sh          # dry-run (no LLM)
./tests/harness/run_inspection.sh --live   # real LLM calls (requires API key)
```

Checks these routing scenarios: `python-csv`, `fastapi-crud`, `bash-backup`, `website-check`, `research-rust`, `api-docs`, `plan-weather`, `qa-pytest`, `docker-setup`, `readme-update`

### 2c. Integration bake-off script

**Command**: `./tests/integration/run_bakeoff.sh <test_dir> [level]`
**What it runs**: Full prescriptive workflow against a bake-off level task
**Requires API key + staged install dir (via `./tests/integration/install.sh`).**

### 2d. Makefile targets

```bash
make test        # cargo test (same as 2a)
make check       # cargo check (no tests)
make clippy      # lint
make lint        # clippy + fmt
```

No Makefile targets wrap the shell test scripts — they must be invoked directly.

### 2e. Playwright UI tests

**Command**: `make ui-test` (or `cd ui && pnpm test`)
**What it runs**: Browser smoke tests against a running `open-mpm --api` server
**Requires server running** at `OMPM_URL` (default `http://localhost:7654`)

---

## 3. Languages and Scenarios Currently Covered

### Regression task files (`.open-mpm/tasks/regression/`)

| File | Language | Task Description |
|------|----------|-----------------|
| `py-simple.txt` | Python | CLI word-count script + argparse + pytest |
| `rs-simple.txt` | Rust | Async retry with exponential backoff + tokio tests |
| `ts-simple.txt` | TypeScript | Result<T,E> discriminated union + vitest |

Only 3 files. No Go, no shell script, no multi-file project tasks.

### Harness inspection suite (`.open-mpm/tasks/harness-test-suite.toml`)

14 entries covering agent routing for:
- Python coding (CSV, FastAPI, git log analysis)
- Rust CLI coding
- TypeScript types
- Bash scripting
- Research / web check
- Documentation
- Planning (multi-file project)
- QA (pytest runner)
- Ops (Docker, backup)

These are **dry-run routing tests only** — they test `best_match` agent selection, not code generation output.

### Bake-off levels (`.open-mpm/tasks/level-1.txt` through `level-5.txt`)

Full prescriptive workflow tasks at escalating complexity. All Python-focused (bake-off is Python-centric).

### Persona scenarios

Three persona skill files exist: `hacker.md`, `vibe-coder.md`, `novice.md`. Only `hacker` and `vibe-coder` have phase skip behavior in the engine. The t06 scenario (hacker <90s) is the only active persona regression.

---

## 4. What the Test Runners DO NOT Cover (Gaps)

### No formal definition of t01-t07

The masa-persona tests are entirely manual and undocumented as a test spec. There is no script, TOML, or fixture file that says "run these 7 tasks in this order and verify these outcomes." Test results are determined by visual inspection of output.

### Regression suite is too small and Python-only

Three files in `.open-mpm/tasks/regression/`. Missing:
- Go
- Shell script output verification
- Multi-agent workflows (plan → code → qa)
- CTRL commands (add project, list projects, stop task)
- Persona variants (hacker/vibe-coder) as automated checks

### No automated persona timing assertions

t06 requires manual timing measurement. No test asserts `hacker` completes in <90s. The fix (`"plan"` into `phases_to_skip("hacker")`) is identified but not automated.

### Inspection test coverage is routing-only

The 10 harness inspection tests and 14 TOML entries only check which agent is selected. They do not:
- Assert the agent actually produces output
- Verify skill injection content
- Test multi-agent routing decisions

### No CTRL integration tests

There are zero automated tests for:
- `add project` / `list projects` / `remove project`
- `stop task`
- `set active`

These are exactly the commands that caused t04/t05 failures.

### No workflow phase-skip regression

No test verifies that persona X runs exactly phases Y (and not Z).

### Playwright tests require manual server setup

`make ui-test` needs `open-mpm --api` running separately — not wired into any CI-style auto-run.

### No CI pipeline

There is no `.github/workflows/` directory. The CI is entirely manual:
- Developer runs `cargo test` locally
- Developer runs shell harness scripts manually
- No automated PR gating

---

## 5. Current Pass/Fail Status (Static Analysis)

From session notes (v0.2.12, 2026-04-26):

| Test | Status | Notes |
|------|--------|-------|
| t01 (py-simple) | PASSING | Inferred; no test output file found |
| t02 (rs-simple) | PASSING | Inferred |
| t03 (ts-simple) | PASSING | Inferred |
| t04 (add project) | PASSING | Fixed in `f098bcd` |
| t05 (list projects) | PASSING | Fixed in `f098bcd` |
| t06 (hacker <90s) | FAILING | `phases_to_skip("hacker")` missing `"plan"` — documented fix in `src/workflow/engine.rs:1383` |
| t07 (write intent tests) | UNBLOCKED | Bedrock routing fix; no pass confirmation |
| `cargo test` | PASSING | ~700 unit tests |
| Harness inspection (dry-run) | UNKNOWN | No recent run output in repo |
| API e2e (non-ignored) | PASSING | Designed to pass without API keys |
| Playwright UI | UNKNOWN | No CI run |

---

## 6. Files Referenced

| Path | Purpose |
|------|---------|
| `tests/api_e2e.rs` | HTTP API end-to-end tests (4 structural + 3 LLM-ignored) |
| `tests/cli_project.rs` | CLI inspect dry-run tests (2 tests) |
| `tests/harness/run_inspection.sh` | Shell harness for 10 routing assertions |
| `tests/integration/run_bakeoff.sh` | Full bake-off workflow smoke test |
| `tests/integration/install.sh` | Stages bake-off test dir |
| `tests/support/project.rs` | `Project` fixture for Rust integration tests |
| `tests/support/api_server.rs` | `ApiServer` fixture for HTTP e2e tests |
| `.open-mpm/tasks/regression/py-simple.txt` | Python regression task |
| `.open-mpm/tasks/regression/rs-simple.txt` | Rust regression task |
| `.open-mpm/tasks/regression/ts-simple.txt` | TypeScript regression task |
| `.open-mpm/tasks/harness-test-suite.toml` | 14 routing assertions for inspection tests |
| `docs/developer/testing.md` | Test conventions and coverage targets |
| `docs/research/failing-tests-210-analysis.md` | Root-cause analysis of t04/t05/t06/t07 |
| `docs/research/cli-test-harness-gap-analysis.md` | Design brief for test infrastructure expansion |
| `src/workflow/engine.rs` (line 1383) | `phases_to_skip` — t06 fix location |
