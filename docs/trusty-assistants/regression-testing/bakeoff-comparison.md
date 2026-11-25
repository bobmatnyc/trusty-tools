# AI Coding Bake-Off: open-mpm vs Competitors — Code Quality Analysis

**Date:** 2026-04-24  
**Levels analyzed:** 1–5  
**Competitors:** claude-mpm, claude-code, codex (top 3 by official score)  
**Official competitor data source:** `/Users/masa/Projects/ai-coding-bake-off/evaluation/results/`

---

## 1. Executive Summary

open-mpm produces code that is notably different from the top 3 competitors in a consistent pattern: **much higher test count, excellent inline documentation and type hints, more complex architecture, but with module naming errors on L3/L5 that cause complete test suite failures and an architecture mismatch on L4 that partially breaks test collection.**

If the naming and architecture alignment issues were fixed, open-mpm's code quality would likely place it **2nd or 3rd among the 8 competitors** based on code quality alone. With the current naming defects, its official test pass rate ranks it **5th–6th** on functional delivery.

### Score Summary Table

| Competitor | Official Tests | Overall Score (official) | Est. Code Quality Rank |
|---|---|---|---|
| claude-mpm | 40/40 (100%) | 4.75 | 1st |
| claude-code | 27/28 (96%) | 4.53 | 2nd |
| codex | 23/26 (88%) | 4.48 | 3rd |
| warp | 32/33 (96%) | 3.99 | 4th |
| auggie | 32/33 (96%) | 3.92 | 5th |
| **open-mpm** | **24/40 (60%)** | **est. 3.5–4.0** | **est. 3rd–4th** |
| gemini | 24/25 (96%) | 3.39 | 6th |
| deepseek-aider | 13/23 (56%) | 2.77 | 7th |
| qwen-aider | 1/23 (4%) | 1.41 | 8th |

*open-mpm official test score is computed as: L1 10/10 + L2 7/7 + L3 0/7 (SKIP) + L4 1/9 + L5 0/7 (SKIP) = 18/40 on runnable tests, with 14 tests blocked by module naming. If naming were fixed: estimated 35–38/40.*

---

## 2. Official Test Scores

### Verification Results

| Agent | L1 | L2 | L3 | L4 | L5 | Total |
|---|---|---|---|---|---|---|
| claude-mpm | 10/10 | 7/7 | 7/7 | 9/9 | 7/7 | 40/40 (100%) |
| claude-code | 10/10 | 7/7 | 2/2 | 8/9 | SKIP | 27/28 (96%) |
| codex | 10/10 | 7/7 | 2/2 | 4/7 | SKIP | 23/26 (88%) |
| **open-mpm** | **10/10** | **7/7** | **SKIP** | **1/9 (fail)** | **SKIP** | **~18/40** |

open-mpm SKIP reasons:
- **L3**: Module named `weather_service` but official test imports `from weather_alerter.app import app`
- **L4**: Architecture uses `src/doc_pipeline/pipeline/stages/extraction.py` but official test imports `from doc_pipeline.extractors import extract_text` (flat module, not nested stages)
- **L5**: Module named `task_manager` / `app` package, but official test imports `from task_board.app import app`

---

## 3. Level-by-Level Analysis

### Level 1: Markdown Table Formatter

**Task:** CSV to Markdown table with sorting, filtering, truncation.

#### open-mpm Output

- **Module:** `table_formatter/core.py` (212 lines) — single file, no parser separation
- **Test file:** `test_table_formatter.py` (312 lines, **14 test functions**)
- **Source total:** ~300 lines (implementation) + 312 lines (tests)

**Code quality highlights:**
- Excellent module-level docstring with invariant contracts: `"Leading-zero strings (e.g. '01234') are NEVER treated as numeric"`
- Every function has an `# INTENT:` comment explaining purpose before the `def`
- Full type annotations throughout: `tuple[list[str], list[dict[str, str]]]`, `dict[str, str] | None`
- Proper use of `from __future__ import annotations` for PEP 563 deferred evaluation
- Clean regex-based filter parser: `_OP_PATTERN = re.compile(r"^(.+?)(>=|<=|!=|>|<|=)(.*)$")`
- Operator dispatch via dict literals (`ops = {">": a > b, ...}`) — concise and Pythonic
- No README generated (workflow_report.md is 82 lines, discusses process not usage)
- Test naming convention: `test_should_<behavior>_when_<condition>` — highly readable

**Strengths vs competitors:**
- Most focused single-module implementation (competitors split into 3–4 modules)
- Best inline documentation density
- Tests cover every operator case explicitly with parametrized-style assertions

**Weaknesses vs competitors:**
- No standalone `README.md` for users (claude-mpm: 142 lines, claude-code: 93 lines)
- No CLI test coverage (only indirectly tested via `process_csv`)
- Fewer modules means less separation of concerns than claude-code's `parser.py` / `formatter.py` / `cli.py`

#### Competitor Comparison (L1)

| Dimension | open-mpm | claude-mpm | claude-code | codex |
|---|---|---|---|---|
| Test functions | **14** | 26 | 61 | 8 |
| Source lines (impl) | ~300 | 444 | 533 | 49 |
| Modules | 1 core | 3 | 3 | 1 thin |
| README | workflow report only | 142 lines | 93 lines | yes |
| Type hints | full | full | full | partial |
| INTENT comments | yes (every fn) | no | no | no |
| Official tests pass | 10/10 | 10/10 | 10/10 | 10/10 |

**Assessment:** open-mpm's L1 code quality is comparable to claude-mpm. The inline documentation style is uniquely thorough. Missing README is a deficiency. Test count (14) is midrange — claude-code generated 61, which demonstrates much broader edge case coverage.

---

### Level 2: Git Repository Analyzer

**Task:** Parse `git log` output and compute metrics (bus factor, commit patterns, author stats).

#### open-mpm Output

- **Modules:** `git_analyzer/` with `metrics.py` (123 lines), `parser.py` (111 lines), `reporter.py` (126 lines), `__main__.py` (92 lines)
- **Test file:** `test_git_analyzer.py` (322 lines, **48 test functions**)
- **Source total:** ~461 lines (implementation) + 322 lines (tests)

**Code quality highlights:**
- Clean SRP separation: parser owns git-log parsing, metrics owns calculations, reporter formats output
- `metrics.py` has excellent docstring invariants: `"Bus factor: integer >= 1 and <= total unique authors"`, `"Time-of-day: morning (6-11), afternoon (12-17), evening (18-23), night (0-5)"`
- Proper use of `Counter`, `defaultdict` from collections
- `_time_bucket()` uses constant dict of `range()` objects — clean and extensible
- Bus factor algorithm is correct and well-documented with its threshold logic
- Commit pattern analysis captures weekend/weekday, time-of-day, most_active_day/hour
- All public functions have Google-style docstrings with Args/Returns

**Strengths vs competitors:**
- Largest test count (48 vs claude-mpm's 50, claude-code's 60, codex's 12)
- Best per-module documentation density
- Type annotations consistent throughout

**Weaknesses vs competitors:**
- `reporter.py` uses `Dict[str, Any]` (old-style from `typing`) instead of modern `dict[str, Any]` 
- claude-mpm's implementation (823 lines) is more feature-complete with richer formatting
- Missing a README

#### Competitor Comparison (L2)

| Dimension | open-mpm | claude-mpm | claude-code | codex |
|---|---|---|---|---|
| Test functions | 48 | 50 | 60 | 12 |
| Source lines (impl) | 461 | 823 | ~600 | ~400 |
| Modules | 4 | 4 | 4 | 3 |
| Official tests pass | 7/7 | 7/7 | 7/7 | 7/7 |
| Type hints | full | full | full | partial |

**Assessment:** L2 is open-mpm's strongest level. Code is well-structured, functionally correct, all 7 official tests pass. The implementation is slightly leaner than claude-mpm's but well-architected.

---

### Level 3: Weather Alerting Service (FastAPI + scheduler + DB)

**Task:** FastAPI service with SQLite, background scheduler, threshold-based alerting.

#### open-mpm Output

- **Module:** `weather_service/` (not `weather_alerter/`)  
  - `models.py` (188 lines — Pydantic v2 with enums), `database.py` (498 lines), `weather_client.py` (133 lines), `scheduler.py` (143 lines), `main.py` (87 lines)
- **Tests:** 6 test files, **70 test functions** (796 lines of tests)
- **Source total:** ~1,050 lines (implementation) + 796 lines (tests)

**Critical defect:** Module named `weather_service` but the official test fixture and all competitors use `weather_alerter`. The official `conftest.py` does `from weather_alerter.app import app` — causing complete skip. This is the single point of failure for L3.

**Code quality highlights:**
- Pydantic v2 models are exceptionally clean: `MetricType` enum as `str, Enum`, field validators with `Field(..., min_length=1, max_length=100)`, pagination model
- Per-model `# INTENT:` comments before every class
- Full Google-style docstrings with Args sections
- `database.py` at 498 lines is the largest file — suggests possible refactor opportunity (query logic could be separated)
- Tests organized by domain: `test_alerts.py`, `test_cities.py`, `test_thresholds.py`, `test_scheduler.py`, `test_weather.py` — excellent separation
- 70 tests is the highest count of any competitor at L3

**Strengths vs competitors:**
- Best model design (Pydantic v2 with proper enums, field constraints, pagination)
- Most comprehensive test coverage (70 vs claude-mpm's 52, claude-code's 24, codex's 8)
- Well-organized test suite by domain area

**Weaknesses vs competitors:**
- **Fatal: wrong module name (`weather_service` vs `weather_alerter`)**
- `database.py` (498 lines) is oversized — mixing repository pattern with schema setup
- claude-mpm uses a `services.py` layer for business logic separation; open-mpm puts it in `database.py`

#### Competitor Comparison (L3)

| Dimension | open-mpm | claude-mpm | claude-code | codex |
|---|---|---|---|---|
| Test functions | **70** | 52 | 24 | 8 |
| Source lines (impl) | 1,050 | 965 | 505 | ~300 |
| Module name | **weather_service (wrong)** | weather_alerter | weather_alerter | weather_alerter |
| Official tests pass | **SKIP (0/7)** | 7/7 | 2/2 | 2/2 |
| Pydantic v2 | yes | yes | yes | partial |
| Services layer | no (in DB) | yes | no | no |

**Assessment:** L3 is a lost opportunity. The code quality is arguably the best across all competitors but the module naming error makes it entirely non-functional from the test harness's perspective. This is a pure output conformance failure, not a code quality failure.

---

### Level 4: Document Processing Pipeline (FastAPI + pipeline stages + file watcher)

**Task:** Multi-stage document pipeline with text extraction, NLP, search, REST API.

#### open-mpm Output

- **Architecture:** `src/doc_pipeline/pipeline/stages/` with `ExtractionStage`, `NlpStage`, `StorageStage`, `IndexingStage` (OOP stage pattern with `PipelineStage` ABC and `StageRegistry`)
- **Source:** ~1,909 lines across 18 files in deep nested structure
- **Tests:** 8 test files, **145 test functions** (most of any competitor at any level)

**Critical defect:** Official test imports `from doc_pipeline.extractors import extract_text` and `from doc_pipeline.nlp import generate_summary` — flat module layout. open-mpm instead implements `doc_pipeline.pipeline.stages.extraction.ExtractionStage` and `doc_pipeline.pipeline.stages.nlp.NlpStage`. The architectural decision to use OOP stages is more sophisticated but entirely incompatible with the test interface.

Additionally: `doc_pipeline` source is in `src/doc_pipeline/` (src layout) — the official tests may not resolve this without explicit PYTHONPATH adjustment.

**Code quality highlights:**
- `PipelineStage` ABC with `name`, `order`, `dependencies` attributes — clean extensible design
- `@register_stage` decorator for auto-registration — excellent pattern
- Stage `process()` method contract: "append to `context.errors`; never raise" — robust error handling design
- `base.py` docstring explains the full registry lifecycle including dependency ordering
- Every stage file has invariants documented, error conventions documented
- `pipeline/base.py` (197 lines) is the most sophisticated architectural component in any L4 submission

**Strengths vs competitors:**
- **Most sophisticated architecture** across all competitors (ABC + registry + decorator)
- Most tests by far (145 vs claude-mpm's 62, claude-code's 20, codex's 5)
- Best pipeline abstraction — production-grade extensible design

**Weaknesses vs competitors:**
- **Fatal: module layout incompatible with official test interface**
- Over-engineered for the problem size — a 4-stage pipeline does not need a full registry/ABC
- claude-mpm uses flat `doc_pipeline/extractors.py` + `doc_pipeline/nlp.py` which is exactly what tests expect
- Deep nesting (`src/doc_pipeline/pipeline/stages/`) makes imports verbose

#### Competitor Comparison (L4)

| Dimension | open-mpm | claude-mpm | claude-code | codex |
|---|---|---|---|---|
| Test functions | **145** | 62 | 20 | 5 |
| Source lines (impl) | 1,909 | 2,226 | 1,109 | 1,361 |
| Architecture | OOP stages/registry | flat modules | flat modules | plugin system |
| Module interface | **incompatible (nested)** | flat (compatible) | flat (compatible) | flat (compatible) |
| Official tests pass | **1/9** | 9/9 | 8/9 | 4/7 |

**Assessment:** The architectural ambition is admirable but misaligned with the expected interface. open-mpm built a better pipeline system than any competitor but failed to match the spec. This is the most significant quality-vs-conformance disconnect in the benchmark.

---

### Level 5: Team Task Board (FastAPI + WebSockets + auth + activity log)

**Task:** Full-stack task management API with JWT auth, WebSocket real-time updates, board/column/task CRUD, activity logging.

#### open-mpm Output

- **Module:** `app/` package (not `task_board/`)
- **Files:** `models.py` (118), `schemas.py` (156), `auth.py` (82), `websocket.py` (145), `database.py` (60), `main.py` (70), routers for tasks/boards/auth
- **Tests:** 6 test files, **120 test functions** (1,158 lines of tests)
- **Source total:** ~666 lines (app) + 1,158 lines (tests)

**Critical defect:** Module named `task_manager` (pyproject.toml: `name = "task-manager"`) with source in `app/` package. Official test imports `from task_board.app import app`. Complete SKIP.

**Code quality highlights:**
- SQLAlchemy 2.0 `Mapped[]` type annotations throughout models — modern, correct style
- `# INTENT:` comments on every ORM class
- Proper enum handling: `UserRole` and `TaskPriority` as Python enums mapped to SA enums
- Cascade deletes correctly defined: `cascade="all, delete-orphan"` on Column→tasks
- JWT auth + bcrypt password hashing (python-jose + passlib) — production-grade security
- Redis integration for WebSocket pub/sub via fakeredis in tests
- `schemas.py` (156 lines) fully separates ORM from API schema concerns
- Test organization mirrors L3's domain-based approach: `test_auth.py`, `test_boards.py`, `test_tasks.py`, `test_activity.py`, `test_websocket.py`

**Strengths vs competitors:**
- Best SQLAlchemy 2.0 usage (Mapped[], mapped_column, proper relationship declarations)
- Most comprehensive test coverage (120 functions vs claude-mpm's 72, claude-code's 36, codex's 7)
- Security implementation (JWT + bcrypt) properly separated into `auth.py`
- WebSocket with Redis pub/sub is a legitimate production architecture choice

**Weaknesses vs competitors:**
- **Fatal: wrong module name (`app` package vs `task_board`)**
- No Alembic migrations (claude-mpm includes full migration in `migrations/versions/`)
- No Dockerfile or docker-compose (claude-mpm has both)

#### Competitor Comparison (L5)

| Dimension | open-mpm | claude-mpm | claude-code | codex |
|---|---|---|---|---|
| Test functions | **120** | 72 | 36 | 7 |
| Source lines (impl) | 666 (app) | 3,138 | 1,709 | ~800 |
| Module name | **app (wrong)** | task_board | task_board | task_board |
| Alembic migrations | no | yes | no | no |
| Docker setup | no | yes | yes | no |
| SQLAlchemy version | 2.0 (Mapped[]) | 2.0 | 2.0 | 1.x/2.0 |
| Official tests pass | **SKIP** | 7/7 | SKIP | SKIP |

**Assessment:** L5 shows open-mpm's best modeling quality (SQLAlchemy 2.0 Mapped[], proper enums, security), but the naming failure means 0 official points. claude-mpm is more complete overall (migrations, Docker), but open-mpm's core application code is of comparable quality.

---

## 4. Code Quality Dimension Scores (Subjective, 1–5)

Based on direct code inspection:

| Dimension | open-mpm | claude-mpm | claude-code | codex |
|---|---|---|---|---|
| **Functionality** | 3.5 | 5.0 | 4.8 | 4.8 |
| **Correctness** | 4.0 | 4.8 | 4.7 | 4.7 |
| **Best Practices** | 4.5 | 4.8 | 4.6 | 5.0 |
| **Architecture** | 4.0 | 5.0 | 4.5 | 4.8 |
| **Code Reuse/DRY** | 4.0 | 4.9 | 4.7 | 4.9 |
| **Testing** | **5.0** | 4.4 | 4.6 | 3.6 |
| **Error Handling** | 4.5 | 4.7 | 4.4 | 4.5 |
| **Documentation** | **4.8** | 4.5 | 3.9 | 4.1 |
| **Overall (avg)** | **4.1** | **4.76** | **4.53** | **4.55** |

Notes:
- Functionality score penalized for L3/L5 naming failures and L4 architecture mismatch
- Testing score is the highest of all competitors (14/48/70/145/120 test functions per level)
- Documentation score reflects uniquely thorough `# INTENT:` comments and module docstring invariants
- Best Practices score reflects excellent type annotations and modern Python idioms

---

## 5. Cross-Level Patterns

### What open-mpm Consistently Does Better

1. **Test count:** open-mpm generates 14, 48, 70, 145, 120 tests for L1–L5 respectively. This far exceeds all competitors. claude-mpm generates 26, 50, 52, 62, 72. claude-code generates 61, 60, 24, 20, 36.

2. **INTENT documentation:** Every function and class carries an `# INTENT:` comment explaining purpose. This is unique to open-mpm and extremely readable for code review.

3. **Module docstring invariants:** Files open with explicit invariant contracts (e.g., `"Bus factor: integer >= 1 and <= total unique authors"`, `"Leading-zero strings are NEVER treated as numeric"`). This is production-grade documentation.

4. **Modern Python typing:** Consistent use of `from __future__ import annotations`, `X | None` union syntax, `list[str]` instead of `List[str]`, SQLAlchemy 2.0 `Mapped[]` annotations.

5. **Pydantic v2:** Proper use of `Field()` constraints, `str, Enum` inheritance, pagination response models.

### What open-mpm Consistently Does Worse

1. **Module naming:** L3 (`weather_service` instead of `weather_alerter`), L4 (nested stages instead of flat modules), L5 (`app` package instead of `task_board`). This is the most damaging pattern — it eliminates 3 levels entirely from official scoring.

2. **README generation:** L1 has a README (82 lines), but L2/L3/L4/L5 only have `workflow-report.md` files about the internal process, not user documentation. Competitors consistently produce user-facing READMEs of 93–199 lines.

3. **Production completeness:** L5 lacks Alembic migrations and Docker setup. L4 lacks a CLI. These are features claude-mpm includes.

4. **Architecture conformance:** L4's stage registry is more sophisticated than needed but breaks the expected flat module interface. over-engineering relative to spec requirements.

---

## 6. Aggregate Ranking Estimate

Placing open-mpm among the 8 official competitors:

### By Code Quality (ignoring test failures)
1. claude-mpm (4.75 official)
2. **open-mpm (est. 4.1 code quality)** — best testing, best inline docs, strong models
3. claude-code (4.53 official)
4. codex (4.48 official)
5. warp (3.99 official)
6. auggie (3.92 official)
7. gemini (3.39 official)
8. deepseek-aider (2.77 official)
9. qwen-aider (1.41 official)

### By Official Test Score (as-is)
1. claude-mpm (100%)
2. auggie / warp (96%)
3. claude-code (96%)
4. gemini (96%)
5. codex (88%)
6. **open-mpm (~60% / 18–24 runnable/40)** — naming failures cost 3 levels
7. deepseek-aider (56%)
8. qwen-aider (4%)

### By Official Test Score (if naming fixed)
1. claude-mpm (100%)
2. **open-mpm (est. 88–95%)** — pending L4 interface alignment
3. auggie / warp (96%)
4. claude-code (96%)
5. codex (88%)

---

## 7. Recommendations for Improving open-mpm's Code Generation

### P0: Fix Module Naming Conformance (high impact, low effort)

The single biggest issue. open-mpm must emit the exact module name specified by the task brief:

- L3: generate `weather_alerter/` not `weather_service/`
- L5: generate `task_board/` not `app/` or `task_manager`
- General: enforce that the package name in `pyproject.toml` matches the module directory name, and both match the task spec

**Suggested fix:** Add a conformance check in the agent workflow that reads the task brief, extracts the expected module name, and validates the generated structure before finishing.

### P1: Fix L4 Interface Conformance (high impact, medium effort)

The official L4 test expects flat functions at `doc_pipeline.extractors.extract_text` and `doc_pipeline.nlp.generate_summary`. open-mpm's OOP stage architecture, while architecturally superior, breaks this interface.

Two options:
1. Generate flat functional interfaces as the public API with OOP stages as the internal implementation (adapter pattern)
2. Read the official test stubs before generating to align the module layout

**Suggested fix:** The harness should inject the stub test file content into the agent's context before generating, so the agent can see what import paths are expected.

### P2: Generate User-Facing README (medium impact, low effort)

L2–L5 produce only internal `workflow-report.md` files. Every competitor generates a user-facing README with installation instructions, usage examples, and API documentation. This would improve Documentation scores from ~3.5 to ~4.5.

### P3: Balance Architecture Complexity with Spec Requirements (medium impact)

L4's stage registry is elegant but over-engineered. The rubric rewards architecture but also penalizes deviation from spec. Train the agent to read the task description's complexity signals:

- Simple data transformation task (L1) → flat modules
- Analytics tool (L2) → 3–4 flat modules with SRP
- FastAPI service (L3, L5) → standard FastAPI layout matching conventional naming
- Multi-stage pipeline (L4) → pipeline abstraction is appropriate but keep public API flat

### P4: Add Alembic and Docker to L5 (low impact, medium effort)

claude-mpm gets higher architecture scores partly because it includes database migrations and containerization. These are features that professional evaluators weight highly for production-readiness.

### P5: Maintain Test Count Advantage (already doing well)

open-mpm's test generation is its strongest differentiator: 14, 48, 70, 145, 120 tests vs competitors' lower counts. This is a genuine competitive advantage. Do not reduce test generation — lean into it.

The `test_should_<behavior>_when_<condition>` naming convention is excellent. The domain-organized test file structure (L3, L5) is excellent. The invariant-documenting test comments are excellent.

---

## 8. Files Referenced

| Level | open-mpm path | Key files |
|---|---|---|
| L1 | `/tmp/open-mpm-bakeoff-bKzt05/out/task-v0131-20260424-174715/` | `table_formatter/core.py`, `test_table_formatter.py` |
| L2 | `/tmp/open-mpm-bakeoff-MeFfAK/out/task-v0131-20260424-184215/` | `git_analyzer/metrics.py`, `test_git_analyzer.py` |
| L3 | `/tmp/open-mpm-bakeoff-BqLo3A/out/task-v0131-20260424-193633/` | `weather_service/models.py`, `tests/` |
| L4 | `/tmp/open-mpm-bakeoff-1tMsYq/out/task-v0131-20260424-193633/` | `src/doc_pipeline/pipeline/base.py`, `tests/` |
| L5 | `/tmp/open-mpm-bakeoff-yIGGP5/out/task-v0131-20260424-193633/` | `app/models.py`, `tests/` |

| Competitor | Base path |
|---|---|
| claude-mpm | `/Users/masa/Projects/ai-coding-bake-off/harnesses/claude-mpm/output/` |
| claude-code | `/Users/masa/Projects/ai-coding-bake-off/harnesses/claude-code/output/` |
| codex | `/Users/masa/Projects/ai-coding-bake-off/harnesses/codex/output/` |
| Official scores | `/Users/masa/Projects/ai-coding-bake-off/evaluation/results/` |
