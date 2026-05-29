# Bake-Off Rubric Gap Analysis

**Date:** 2026-04-24
**Sources:** bakeoff-v016-analysis.md (v0.1.6), bakeoff-analysis-v0118.md (v0.1.18), runs.log (v0.1.31)
**Analyst:** Research Agent

---

## Current Scorecard Summary (v0.1.31 baseline)

| Level | Tests | Notes |
|-------|-------|-------|
| L1 table_formatter | 14/14 | No fix needed |
| L2 git_analyzer | 14/14 | No fix needed |
| L3 weather_service | 35/35 | Required 1-line lifespan fix |
| L4 doc_pipeline | 71/71 | No fix needed |
| L5 task_manager | 57/57 | Required 1-line bcrypt pin |

All 5 levels pass with 191/191 tests, but L3 and L5 required manual QA fixes that should be prevented upstream.

---

## Q1: Consistently Low Rubric Dimensions

### Documentation (5%) — chronic underperformer
- v0.1.6: L1 scored 3/5 (no README), L2 scored 3/5 (no README)
- v0.1.6: L3-L5 scored 4/5 when README present, never 5/5
- Root cause: `docs` phase in prescriptive.json is `"skip": true`; nothing mandates README generation
- The plan agent's wave decomposition guidelines do not require README as a deliverable

### Error Handling (15%) — never 5/5 in any level
- L1: no test for missing-file CLI path
- L5: no explicit 422 unprocessable entity handler tests
- L3: no test for graceful scheduler error handling
- Root cause: plan agent test-case guidance does not enumerate required error paths

### Correctness (30%) — conditionally collapses
- L3 v0.1.6 and v0.1.18: 0/62 due to ASGITransport bypass
- L4 v0.1.6: 40/53 due to missing `lifespan="on"` in test client
- L5 v0.1.31: 6/57 due to bcrypt>=4.0 incompatibility with passlib
- These are the highest-weighted dimension failures with the largest score impact

### Testing (20%) — loses points via same root causes as Correctness
- Tests are written but fail to collect (exit code 4) or execute against uninitialized state

---

## Q2: Agent Prompt Changes for Documentation 5/5

### Plan agent — add to Wave decomposition guidelines
After the "Final wave" bullet, add:

```
- Final wave MUST include a `README.md` at the project root. The README must contain:
  1. Project description (1 paragraph)
  2. Installation: `pip install -e ".[test]"` or `uv sync --extra test`
  3. Usage: at least one CLI or Python invocation example
  4. Test instructions: `pytest -v`
  Rubric: a missing or empty README costs 2 rubric points. Do not omit it.
- For projects with Docker Compose, final wave also includes `.github/workflows/ci.yml`
  that runs `pytest` and `ruff check` on pull_request and push to main.
```

### QA context template — add README check
Insert as STEP 4 before results reporting:
```
STEP 4 — Documentation check:
  ls {{project_dir}}/README.md 2>&1
If missing, log "WARN: README.md not found at project root" in the details field.
```

---

## Q3: Skill Content for bcrypt/passlib and FastAPI Lifespan

**Create: `.open-mpm/skills/python-compat.md`** tagged `["bcrypt", "passlib", "fastapi", "httpx", "spacy"]`

Required sections:

### passlib + bcrypt
- Pin `bcrypt<4.0.0` whenever `passlib` is in dependencies
- passlib 1.7.x is incompatible with bcrypt 4.x API (AttributeError on CryptContext)
- Correct: `"passlib[bcrypt]>=1.7.4"`, `"bcrypt>=3.2.0,<4.0.0"` in pyproject.toml

### FastAPI lifespan in tests (httpx >= 0.23 / FastAPI >= 0.93)
- FORBIDDEN: `AsyncClient(transport=ASGITransport(app=app), base_url="http://test")`
  - Bypasses lifespan; app.state.* is uninitialized; all DB-touching tests fail
- CORRECT: `AsyncClient(app=app, base_url="http://test", lifespan="on")`
- No ASGITransport needed; httpx handles ASGI directly with lifespan="on"

### FastAPI DELETE with status_code=204
- Must use `-> None` return type and no `response_model` argument
- `response_model=SomeSchema` on a 204 route causes AssertionError at import time

### spaCy model download
- Models not installed with pip; required autouse fixture in conftest.py:
  ```python
  @pytest.fixture(scope="session", autouse=True)
  def download_nlp_models():
      import subprocess
      subprocess.run(["python", "-m", "spacy", "download", "en_core_web_sm"], check=False)
  ```

---

## Q4: QA Prompt Improvements

### New STEP 0 — sanity import check (add before STEP 1 in QA context_template)
```
STEP 0 — Sanity import check:
  cd {{project_dir}} && python3 -c "import {{module_name}}; print('import OK')" 2>&1
If ImportError or AttributeError, diagnose before running pytest. This check does not
count as a test — it only surfaces import-time crashes (e.g., FastAPI route 204 issue,
missing __init__.py, wrong PYTHONPATH).
```

### Enhanced exit code 4 recovery (replace current single-line fallback)
```
EXIT CODE 4 RECOVERY (no tests collected):
1. cd {{project_dir}} && python3 -m pytest --collect-only -q 2>&1
   Read every collection error line. Identify the root cause before guessing.
2. PYTHONPATH=src:. python3 -m pytest -v --import-mode=importlib 2>&1
3. If still 0 collected: report status="fail", details=exact collection traceback.
   Do NOT guess root cause without the traceback evidence.
```

### QA agent system prompt — one-line fix protocol
Add after "Skill Lookup on Test Failure":
```
## One-Line Fix Protocol
If a loaded skill documents a specific one-line fix (bcrypt pin, lifespan="on",
spaCy model download), you MAY apply it and re-run pytest ONCE:
- Only apply changes explicitly documented in a skill's "Fix" section
- Log: "Applied skill fix: <skill-name> — <description>" in the summary field
- Report final counts after the re-run
- Do NOT invent fixes not documented in a loaded skill
```

---

## Q5: Path to 5/5 on All Rubric Dimensions

| Dimension | Current Gap | Required Change | Effort |
|-----------|-------------|-----------------|--------|
| Documentation | 3/5 L1/L2, 4/5 L3-L5 | Plan agent README wave rule + CI workflow rule | S |
| Error Handling | 4/5 all levels | Plan agent: enumerate required error test paths | S |
| Correctness (ASGITransport) | Collapses on FastAPI lifespan | python-compat skill auto-loaded at plan time | S |
| Correctness (bcrypt) | Collapses on passlib+bcrypt>=4 | Same skill, bcrypt pin | S |
| Testing (collection failures) | 0 collected on L3 v0.1.18 | QA STEP 0 + exit code 4 recovery | S |
| L5 CI workflow | Missing bonus points | Plan agent wave rule for Docker+CI | S |

No architectural changes needed. All fixes are prompt engineering and skill content.

---

## Concrete Action List (priority order)

1. **Create `.open-mpm/skills/python-compat.md`** — passlib/bcrypt pin, ASGITransport
   FORBIDDEN/CORRECT pattern, FastAPI 204 rule, spaCy autouse fixture.
   Tags: `["bcrypt", "passlib", "fastapi", "httpx", "spacy"]`

2. **Plan agent** — add README mandate to Wave decomposition guidelines (final wave
   MUST include README.md with install/usage/test sections).

3. **Plan agent** — add CI workflow rule for Docker Compose projects (final wave
   includes `.github/workflows/ci.yml`).

4. **QA context template** — add STEP 0 sanity import check before dependency install.

5. **QA context template** — replace single-line exit code 4 fallback with 3-step
   recovery sequence (collect-only, importlib mode, then report with traceback).

6. **QA agent system prompt** — add one-line fix protocol: load skill, apply documented
   fix, re-run once, log the fix applied.

7. **Plan agent** — add to Test cases section: "Include at least one test per explicit
   error path mentioned in the task rubric: missing/invalid input, auth failure,
   422 response, resource not found."

8. **prescriptive.json docs phase** — set `"skip": false` with a minimal docs-agent
   prompt that checks for README presence and adds it if missing (rather than
   generating full docs from scratch).

---

**Document Status:** Complete
**Last Updated:** 2026-04-24
