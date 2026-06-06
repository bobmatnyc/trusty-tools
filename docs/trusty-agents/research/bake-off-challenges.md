# AI Coding Bake-Off: Challenges & Harness Architecture

**Research Scope:** Analysis of the AI Coding Bake-Off benchmark structure, challenge specifications, evaluation criteria, and harness architecture.

**Date:** 2026-04-22

---

## Executive Summary

The AI Coding Bake-Off is a structured benchmark for evaluating AI coding agents across 5 Python projects of increasing complexity. The benchmark tests agents on correctness, code quality, architecture, testing discipline, error handling, and documentation. Each challenge includes a problem specification, provided test suite, evaluation rubric, and test fixtures. The claude-mpm harness is designed to orchestrate multi-agent teams (Research, Code Analysis, Python Engineer, QA, Documentation) using the claude-mpm framework, with cross-level learning via MCP vector search and memory tools.

---

## Challenge Overview

### Level-by-Level Summary

| Level | Challenge | Time Budget | Complexity | Focus Areas |
|-------|-----------|------------|-----------|------------|
| 1 | Markdown Table Formatter | ~30 min | Low | CLI tools, file I/O, string formatting, type detection |
| 2 | Git Log Analyzer | ~1 hour | Low-Medium | Parsing, packaging, data aggregation, statistics |
| 3 | Weather Alerting Service | ~2 hours | Medium | REST APIs, external integrations, SQLite, scheduling, Docker |
| 4 | Document Processing Pipeline | ~3-4 hours | High | Architecture, extensibility, NLP, full-text search, plugin design |
| 5 | Team Task Board | ~6-8 hours | Very High | Full-stack, real-time (WebSocket), auth, migrations, Docker Compose, CI |

---

## Detailed Challenge Specifications

### Level 1: Markdown Table Formatter

**Problem:** Build a Python module that reads CSV files and outputs beautifully formatted Markdown tables, handling real-world data gracefully (mixed types, unicode, missing values).

**Language/Stack:**
- Python 3.12+ with type hints
- CSV input (standard comma-delimited)
- Markdown table output

**Core Requirements:**
1. CSV reading with header support
2. Markdown output with proper alignment syntax (`:---`, `---:`, `:---:`)
3. Automatic column type detection:
   - Numeric columns: right-aligned
   - Text columns: left-aligned
   - Mixed: left-aligned
4. CLI entry point: `python3 -m table_formatter input.csv`
5. Optional flags:
   - `--sort COLUMN` (ascending) / `--sort-desc COLUMN`
   - `--filter EXPRESSION` (e.g., `"age>30"`, `"name=Alice"`)
   - `--output FILE` (write to file)
   - `--max-width N` (truncate with ellipsis)

**Edge Cases:**
- Empty CSV (headers only)
- No headers
- Unicode (emoji, CJK, accented characters)
- Missing values
- Very long cell content
- Numeric strings that should remain strings (zip codes)
- Quoted fields with commas

**Deliverables:**
- `table_formatter/` package or single module
- CLI entry point
- 5+ additional tests beyond provided suite
- README.md with usage examples

**Evaluation Rubric (Weights):**
- **Correctness (30%):** All tests pass, valid Markdown, edge cases handled
- **Code Quality (25%):** Clean Python, type hints, ruff/mypy compliance
- **Testing (20%):** 10+ additional tests covering edge cases, error paths, CLI
- **Error Handling (15%):** Graceful handling with helpful messages
- **Architecture (5%):** Clean separation of concerns, reusable as library
- **Documentation (5%):** Complete README, docstrings

**Bonus:** HTML/RST output formats, config files, colorized output, progress bars

---

### Level 2: Git Log Analyzer

**Problem:** Create a CLI tool that analyzes git repository commit history and produces insightful metrics on per-author statistics, patterns, and bus factor.

**Language/Stack:**
- Python 3.12+ with type hints
- subprocess/gitpython for git access
- Proper Python project packaging with `pyproject.toml`
- Project structure:
  ```
  git_analyzer/
  ├── pyproject.toml
  ├── README.md
  ├── src/git_analyzer/
  │   ├── __init__.py
  │   ├── __main__.py     (CLI entry)
  │   ├── parser.py       (git log parsing)
  │   ├── metrics.py      (calculations)
  │   └── reporter.py     (output formatting)
  └── tests/
      ├── test_parser.py
      ├── test_metrics.py
      └── fixtures/
          └── sample_git_log.txt
  ```

**Core Metrics:**
1. **Per-Author Statistics:**
   - Total commits
   - Lines added/removed
   - Active days (unique days with commits)
   - First/last commit dates
   - Average commits per active day

2. **Commit Patterns:**
   - Time-of-day distribution (morning/afternoon/evening/night)
   - Weekend vs weekday commits
   - Weekly/monthly frequency
   - Longest consecutive-day streaks

3. **Bus Factor:**
   - Minimum developers owning 50% of codebase (by lines changed)
   - Top contributors by percentage

4. **Summary Statistics:**
   - Total commits, authors, active period
   - Average commit size
   - Most active day/hour

**CLI Interface:**
```bash
python3 -m git_analyzer [/path/to/repo]
  --format json|text
  --since DAYS
  --author NAME
```

**Output Formats:**
- Terminal: Well-formatted summary with sections and aligned columns
- JSON: Structured object with all metrics

**Deliverables:**
- Properly packaged Python project with `pyproject.toml`
- CLI runnable via `python3 -m git_analyzer`
- Terminal and JSON output formats
- Good test coverage
- README with installation and usage

**Evaluation Rubric (Weights):**
- **Correctness (25%):** Metrics computed accurately
- **Code Quality (20%):** Clean Python, type hints, linting compliance
- **Architecture (15%):** Modular design (parser, metrics, reporter)
- **Testing (15%):** Good coverage with fixtures
- **Error Handling (10%):** Handles missing repos, bad input
- **Documentation (10%):** Installation, usage examples
- **Bonus - Packaging (5%):** Proper pyproject.toml, installable

**Key Architecture Decisions:**
- Git invocation method (subprocess vs gitpython vs dulwich)
- Text parsing vs library usage
- Report layout and visualization (sparklines, charts optional)
- Caching strategy for repeated analysis

---

### Level 3: Weather Alerting Service

**Problem:** Build a REST API service that monitors weather for configured cities and triggers alerts when thresholds are exceeded, with persistent storage and scheduled checks.

**Language/Stack:**
- Python 3.12+ with type hints
- Web framework: FastAPI or Flask
- SQLite for persistence
- Background scheduler (APScheduler, schedule, or asyncio)
- Docker & docker-compose
- External API: OpenWeatherMap (free tier)

**Data Model:**
```
City:
  - id (auto-increment)
  - name, latitude, longitude
  - enabled (bool)
  - created_at

Threshold:
  - id (auto-increment)
  - city_id (FK)
  - metric (temperature|humidity|wind_speed)
  - operator (gt|lt|gte|lte)
  - value (float)
  - enabled (bool)

AlertLog:
  - id (auto-increment)
  - city_id, threshold_id (FKs)
  - triggered_at, metric_value
  - message
```

**REST API Endpoints:**
```
POST   /api/cities                  # Add city
GET    /api/cities                  # List all
GET    /api/cities/{id}             # Get details
DELETE /api/cities/{id}             # Remove
PATCH  /api/cities/{id}             # Update (enable/disable)

POST   /api/cities/{id}/thresholds  # Add threshold
GET    /api/cities/{id}/thresholds  # List thresholds
DELETE /api/thresholds/{id}         # Remove threshold

GET    /api/alerts                  # Recent alerts (paginated)
GET    /api/alerts?city_id=1        # Filter by city

GET    /api/weather/{city_id}       # Current weather (live fetch)
GET    /api/health                  # Health check
```

**Background Scheduler:**
- Check all enabled cities on configurable interval (default: 5 minutes)
- Compare values against configured thresholds
- Log alerts to database when thresholds exceeded
- Print alerts to stdout

**Docker Support:**
- `docker-compose.yml` that starts service
- Expose API on port 8000
- Mount volume for SQLite database
- Accept API key via environment variable

**Deliverables:**
- REST API application (FastAPI or Flask)
- SQLite database with schema
- Background scheduler implementation
- Dockerfile and docker-compose.yml
- Tests for endpoints and alert logic
- README with setup and usage

**Evaluation Rubric (Weights):**
- **Correctness (20%):** All endpoints work, alerts trigger correctly
- **Code Quality (15%):** Clean architecture, type hints
- **Architecture (20%):** Proper service design, separation of concerns
- **Testing (15%):** Good endpoint and logic coverage
- **Error Handling (15%):** Graceful API failures, missing data
- **Documentation (5%):** Setup instructions, API examples
- **Bonus - Docker (10%):** Working compose, clean Dockerfile

**Key Architecture Decisions:**
- Web framework choice (FastAPI preferred for async)
- ORM vs raw SQL
- Scheduler library (APScheduler most common)
- Mock/demo mode for testing without API key
- Database migrations approach

---

### Level 4: Document Processing Pipeline

**Problem:** Design a document processing pipeline with text extraction, NLP analysis, full-text search indexing, and REST API, with extensible plugin architecture.

**Language/Stack:**
- Python 3.12+ with type hints
- Web framework: FastAPI or Flask
- SQLite with FTS5 or Whoosh for search
- NLP library: spaCy, NLTK, or transformers
- Document types: PDF (.pdf), text (.txt, .md)
- PDF extraction: pypdf, pdfplumber, or similar

**Pipeline Stages (Extensible):**
1. **Ingestion:** Watch directory for new PDF/text files, route to extractors
2. **Text Extraction:**
   - `.txt`, `.md`: Read directly
   - `.pdf`: Extract using library
3. **NLP Processing:**
   - Entity extraction (people, organizations, locations)
   - Key phrase extraction
   - Summary generation (first N sentences or extractive)
   - Word count and reading time
4. **Indexing:** Index for full-text search with relevance ranking
5. **Storage:** Persist metadata in SQLite

**REST API Endpoints:**
```
POST   /api/documents/upload         # Upload document
GET    /api/documents                # List documents
GET    /api/documents/{id}           # Get details + metadata
DELETE /api/documents/{id}           # Remove document

GET    /api/search?q=query           # Full-text search
GET    /api/search?q=query&type=pdf  # Search with filters

GET    /api/entities                 # List all entities
GET    /api/entities?type=PERSON     # Filter by type

GET    /api/stats                    # Pipeline statistics
```

**Admin CLI:**
```bash
python3 -m doc_pipeline reprocess --id 123
python3 -m doc_pipeline reindex
python3 -m doc_pipeline stats
python3 -m doc_pipeline watch /path/to/incoming/
```

**Architecture Requirements:**
1. **Plugin Architecture:** New stages can be added without modifying existing code
   - Implement `PipelineStage` interface/ABC
   - Support stage ordering and dependencies
2. **Error Isolation:** Failed stages logged, others continue
3. **Architecture Diagram:** Mermaid or ASCII showing pipeline flow

**Example NLP Output:**
```json
{
  "document_id": 1,
  "filename": "q3-earnings.pdf",
  "file_type": "pdf",
  "word_count": 2450,
  "reading_time_minutes": 9.8,
  "summary": "Q3 revenue increased 12% YoY...",
  "entities": [
    {"text": "Amazon", "type": "ORGANIZATION", "count": 15},
    {"text": "Andy Jassy", "type": "PERSON", "count": 3},
    {"text": "Seattle", "type": "LOCATION", "count": 2}
  ],
  "key_phrases": ["cloud services growth", "operating margin"],
  "processed_at": "2026-04-01T14:30:00Z",
  "processing_time_ms": 1250
}
```

**Deliverables:**
- Pipeline with all 5 stages
- REST API with all endpoints
- Admin CLI for management
- Architecture diagram
- Comprehensive tests (unit and integration)
- README documenting architecture decisions

**Evaluation Rubric (Weights):**
- **Correctness (15%):** All endpoints work, NLP extraction accurate
- **Code Quality (15%):** Clean Python, type hints
- **Architecture (30%):** Extensible design, plugin system, modularity
- **Testing (15%):** Good test coverage
- **Error Handling (10%):** Graceful failures, missing data
- **Documentation (10%):** Architecture decisions, setup
- **Bonus - Extensibility (5%):** Easy to add new stages

**Key Architecture Decisions:**
- NLP library (spaCy recommended for entity extraction)
- Search engine (SQLite FTS5 vs Whoosh)
- Web framework (FastAPI preferred)
- Pipeline orchestration (custom vs Celery vs asyncio)
- Plugin discovery (ABC, entry points, decorators, directory scanning)
- File watching approach (watchdog vs polling)

---

### Level 5: Team Task Board

**Problem:** Full-stack team task management application with REST API, real-time WebSocket updates, JWT authentication, and complete Docker Compose deployment.

**Language/Stack:**
- **Backend:** Python 3.12+ (FastAPI, Django, or Flask)
- **Frontend:** HTMX (preferred), React, Svelte, or Vue
- **Database:** PostgreSQL (Docker)
- **Authentication:** JWT with refresh tokens
- **Real-time:** WebSocket for live updates
- **ORM:** SQLAlchemy, Django ORM, or Tortoise
- **Migrations:** Alembic or Django migrations
- **Deployment:** Docker Compose with all services

**Data Model:**
```
User:
  - id, email, password_hash, display_name
  - role (admin|member), created_at

Board:
  - id, name, description
  - created_by (FK User), created_at

Column:
  - id, board_id (FK), name, position (int)

Task:
  - id, title, description
  - column_id (FK), assignee_id (FK User, nullable)
  - priority (low|medium|high|urgent)
  - due_date, created_by (FK User)
  - created_at, updated_at

Activity:
  - id, board_id (FK), user_id (FK), task_id (FK, nullable)
  - action (created|updated|moved|deleted|commented)
  - details (JSON), created_at
```

**REST API Endpoints:**
```
# Authentication
POST   /api/auth/register             # Register user
POST   /api/auth/login                # Login → JWT
POST   /api/auth/refresh              # Refresh token
GET    /api/users/me                  # Current user
GET    /api/users                     # List (admin only)

# Boards
POST   /api/boards                    # Create board
GET    /api/boards                    # List boards
GET    /api/boards/{id}               # Get with columns/tasks
PUT    /api/boards/{id}               # Update board
DELETE /api/boards/{id}               # Delete (admin only)

# Columns
POST   /api/boards/{id}/columns       # Add column
PUT    /api/columns/{id}              # Update (name, position)
DELETE /api/columns/{id}              # Delete column

# Tasks
POST   /api/tasks                     # Create task
GET    /api/tasks                     # List (with filters)
GET    /api/tasks/{id}                # Get task details
PUT    /api/tasks/{id}                # Update task
DELETE /api/tasks/{id}                # Delete (admin only)
PATCH  /api/tasks/{id}/move           # Move to column

# Activity
GET    /api/activity                  # Recent activity
GET    /api/boards/{id}/activity      # Board activity

# Health
GET    /api/health                    # Health check
```

**WebSocket Real-Time Updates:**
- Endpoint: `/ws`
- Subscribe to board channels
- Events: `task.created`, `task.updated`, `task.moved`, `task.deleted`
- Broadcast to all connected clients on a board

**Frontend Requirements:**
- Login/register forms
- Board view with Kanban columns
- Drag-and-drop (or click-based) task movement
- Real-time updates without page refresh
- Task detail modal
- Create/edit task forms
- Activity feed

**Docker Compose:**
- Application server (Python backend)
- PostgreSQL database
- Frontend (HTMX in backend or SPA with nginx)
- Optional: Redis for WebSocket pub/sub in multi-worker setup

**Commands That Must Work:**
```bash
docker-compose up                       # Start everything
docker-compose exec app python3 manage.py seed  # Seed DB
docker-compose exec app pytest          # Run tests
```

**CI Configuration:**
- `.github/workflows/ci.yml`
- Run tests, linting, build Docker images

**API Documentation:**
- OpenAPI/Swagger at `/docs` or `/api/docs`

**Deliverables:**
- Backend with all API endpoints and WebSocket
- Frontend with Kanban UI
- PostgreSQL with migrations and seed data
- Docker Compose configuration
- GitHub Actions CI
- Comprehensive tests (unit, integration, API)
- OpenAPI/Swagger docs
- README with architecture decisions

**Evaluation Rubric (Weights):**
- **Correctness (15%):** All endpoints work, real-time updates functional
- **Code Quality (10%):** Clean architecture, type hints
- **Architecture (25%):** Proper API design, WebSocket handling, separation
- **Testing (15%):** Good endpoint and integration coverage
- **Error Handling (10%):** Graceful API failures, validation
- **Documentation (10%):** Architecture, setup, usage
- **Bonus - Real-time/Docker/CI (15%):** WebSocket, compose, Actions

**Key Architecture Decisions:**
- Backend framework (FastAPI most suitable for async WebSocket)
- Frontend approach (HTMX simpler, React more flexible)
- ORM choice (SQLAlchemy recommended)
- Migration tool (Alembic or Django migrations)
- WebSocket implementation (native, socket.io, or channels)
- CSS framework (Tailwind recommended)
- Session/WebSocket auth strategy

---

## Evaluation Framework

### Scoring Model

Each level has a rubric with dimensions weighted by importance:

**Dimension Definitions (1-5 Scale):**
- **5:** Excellent/Complete - Exceeds expectations
- **4:** Good - Meets all requirements well
- **3:** Acceptable - Meets core requirements
- **2:** Poor - Some functionality works
- **1:** Failing - Fundamental issues

**Cross-Level Scoring Consistency:**
- Level 1: 5 dimensions, simple weighting
- Level 2: 7 dimensions including packaging bonus
- Level 3: 7 dimensions including Docker bonus
- Level 4: 7 dimensions including extensibility bonus
- Level 5: 7 dimensions including real-time/docker/CI bonus

### Test Suite Structure

Each challenge includes:
1. **Provided Test Suite** (`challenges/level-N-*/test_suite/`)
   - Baseline tests agents must pass (not optional)
   - Covers core functionality
   - Example: Level 1 has `test_basic.py` with ~10 test cases
2. **Test Fixtures** (in same directory)
   - Sample data files, CSVs, git logs, JSON payloads
   - Real-world examples demonstrating edge cases
3. **Agent's Additional Tests**
   - Agents must write 5+ additional tests per level
   - Tests edge cases, error paths, CLI flags
   - Counted toward Testing dimension

### Rubric Examples

**Level 1 Testing Dimension:**
- 5 points: 10+ additional tests, edge cases, error paths, CLI flags
- 4 points: 5-9 additional tests, major functionality
- 3 points: 1-4 tests, basic cases only
- 2 points: No additional tests
- 1 point: Tests broken or absent

**Level 1 Correctness Dimension:**
- 5 points: All tests pass, handles all edge cases, valid Markdown
- 4 points: All tests pass, minor formatting on edge cases
- 3 points: 80%+ tests pass, some edge cases mishandled
- 2 points: Core works, multiple test failures
- 1 point: Fundamental issues, most tests fail

---

## Claude-MPM Harness Architecture

### Purpose

The claude-mpm harness demonstrates how to use the claude-mpm multi-agent orchestration framework to solve complex coding challenges through team-based delegation.

### Structure

```
harnesses/claude-mpm/
├── README.md                           # Harness profile and status table
├── instructions/
│   ├── CLAUDE.md                       # Main instructions (what agent does)
│   └── setup.md                        # Configuration instructions
├── .claude-mpm/
│   ├── PM_INSTRUCTIONS.md              # Project manager orchestration guide
│   ├── INSTRUCTIONS.md                 # Agent team instructions
│   ├── WORKFLOW.md                     # Workflow and phase definitions
│   ├── AGENT_DELEGATION.md             # How to delegate work between agents
│   ├── mcp-config.md                   # MCP tools configuration
│   ├── pm_skills_registry.yaml         # Available skills registry
│   └── templates/
│       ├── git-file-tracking.md
│       ├── pm-red-flags.md
│       ├── circuit-breakers.md
│       ├── structured-questions-examples.md
│       ├── pr-workflow-examples.md
│       ├── response-format.md
│       ├── validation-templates.md
│       └── pm-examples.md
└── output/
    └── level-N/
        ├── [solution files]
        └── metadata.json
```

### Workflow Overview

**Phase 1: Competition (Solve All 5 Levels)**

For each level:
1. Read prompt: `prompts/level-N-prompt.md`
2. Read problem: `challenges/level-N-*/PROBLEM.md`
3. Read rubric: `challenges/level-N-*/evaluation/rubric.md`
4. Build solution: `harnesses/claude-mpm/output/level-N/`
5. Run test suite: `pytest challenges/level-N-*/test_suite/ -v`
6. Write additional tests (5+)
7. Record timing: `output/level-N/metadata.json`

**Phase 2: Cross-Review (Evaluate Other Agents)**

1. Read review protocol: `evaluation/cross_review/review_prompt.md`
2. For each other agent's solution:
   - Score against rubric (1-5 per dimension)
   - Write review: `evaluation/results/claude-mpm-reviews-{agent}-level-{N}.md`
3. Be objective, score blind where possible

### Multi-Agent Orchestration Strategy

**Scaling by Complexity:**

- **Level 1-2:** Python Engineer directly
- **Level 3:** Research → Python Engineer → QA
- **Level 4:** Research → Code Analysis → Python Engineer → QA → Documentation
- **Level 5:** Full team (Research, Code Analysis, Python Engineer, QA, Documentation)

**MCP Tools Integration:**
- **kuzu-memory:** Recall patterns from previous levels, store architecture decisions
- **mcp-vector-search:** Semantic search across challenges and solutions

**Cross-Level Learning:**
After each level, store learnings:
- What architecture patterns worked
- Effective testing approaches
- Time estimates vs actuals
- Performance optimization insights

### Metadata Format

Record after each level in `output/level-N/metadata.json`:

```json
{
  "agent": "claude-mpm",
  "level": 1,
  "start_time": "2026-04-03T10:00:00Z",
  "end_time": "2026-04-03T10:28:00Z",
  "wall_clock_minutes": 28,
  "estimated_tokens": 15000,
  "model": "claude-sonnet-4-6",
  "notes": "Observations about the process"
}
```

### Key Rules

1. `challenges/` is READ-ONLY (never modify problems or test suites)
2. All solutions target Python 3.12+ with type hints and tests
3. Each level is a self-contained project in its own directory
4. Record timing from first prompt read to final commit
5. Do NOT look at other harnesses' output during Phase 1
6. Output directory: `harnesses/claude-mpm/output/level-{N}/`

### Harness Status

| Level | Status | Time (min) | Tokens | Notes |
|-------|--------|------------|--------|-------|
| 1 | Not started | - | - | - |
| 2 | Not started | - | - | - |
| 3 | Not started | - | - | - |
| 4 | Not started | - | - | - |
| 5 | Not started | - | - | - |

---

## How Harnesses Integrate with Tests

### Test Suite Invocation

Agents run the provided test suite after building solutions:

```bash
# From project root
pytest challenges/level-N-*/test_suite/ -v
```

**Example (Level 1):**
```bash
pytest challenges/level-1-table-formatter/test_suite/ -v
# Runs test_basic.py which:
# - Uses subprocess to invoke: python3 -m table_formatter {csv_path}
# - Tests both correct output and error cases
# - Validates Markdown formatting
```

### Test Discovery Pattern

Tests expect the solution to be importable/runnable as a module:
- **Level 1:** `python3 -m table_formatter`
- **Level 2:** `python3 -m git_analyzer`
- **Level 3:** `python3 -m weather_service` (with server running)
- **Level 4:** `python3 -m doc_pipeline`
- **Level 5:** Docker Compose + API endpoints

### Test Fixture Locations

All test fixtures stored relative to test file:
```
test_suite/
├── test_basic.py
├── fixtures/
│   ├── simple.csv
│   ├── edge_cases.csv
│   ├── unicode.csv
│   └── ... (other sample data)
└── __pycache__/
```

Tests reference via `Path(__file__).parent / "fixtures" / "filename"`.

---

## Key Insights for Agent Development

### 1. Progressive Complexity Curve

The challenges are intentionally progressive:
- **Levels 1-2:** Focus on correctness and code quality
- **Level 3:** Introduces external dependencies and operational concerns
- **Level 4:** Tests architectural thinking and extensibility
- **Level 5:** Full-stack with real-time, auth, and infrastructure

Agents can apply learnings from earlier levels to solve later ones.

### 2. Architecture Becomes Critical at Level 4+

- Level 1-3: Correctness and testing weighted heavily
- Level 4: Architecture weight jumps to 30%
- Level 5: Architecture weight stays at 25%

This reflects that at higher complexity, design decisions matter more than perfect test coverage.

### 3. Open Decisions Encourage Agency

Each level lists "Open Decisions" where agents choose:
- Framework/library selections
- Architecture patterns
- Testing approaches
- Deployment strategies

This prevents "one right answer" thinking and rewards thoughtful decision-making.

### 4. Test Coverage Expectations Increase

- Level 1: 5+ additional tests (10+ for 5-star)
- Level 2: Tests with fixtures and fixtures directory structure
- Level 3: Tests for API endpoints, background jobs, edge cases
- Level 4: Integration tests for pipeline orchestration
- Level 5: API, WebSocket, auth, and database tests

### 5. Bonus Points Reward Excellence

Each level offers bonus points (up to 0.5 or 5%) for:
- Level 1: HTML/RST output, config files, colors, progress bars
- Level 2: Proper packaging and installability
- Level 3: Docker and docker-compose
- Level 4: Plugin system extensibility
- Level 5: Real-time, Docker, CI/GitHub Actions

---

## Evaluation Process Design

### Blind Review Protocol

The benchmark includes a cross-review phase where agents evaluate other agents' solutions:

1. **Randomized Order:** Reviewers evaluate other agents' code
2. **Structured Scoring:** Use rubric dimensions consistently
3. **Evidence-Based:** Back scores with specific code examples
4. **Comparative Analysis:** Compare approaches without bias toward own solution

### Metrics Tracked

- **Wall Clock Time:** Minutes from prompt read to final commit
- **Token Usage:** Estimated API tokens consumed
- **Dimension Scores:** 1-5 for each evaluation dimension
- **Weighted Final Score:** Sum of (dimension_score × dimension_weight)
- **Agent Ranking:** Agents ranked by average score across all levels

### Success Factors

Agents that score well typically:
1. **Read Requirements Completely:** Before starting code
2. **Write Tests First:** Use TDD approach, especially at higher levels
3. **Document Decisions:** Explain "why" in code comments and READMEs
4. **Handle Edge Cases:** Go beyond happy-path testing
5. **Plan Architecture Early:** Especially critical for Levels 4-5
6. **Iterate on Feedback:** Run tests frequently, fix issues early

---

## Integration with Open-MPM Project

### Relevant Context

The bake-off harness demonstrates:
1. **Multi-agent coordination:** Delegating work across specialized agents
2. **Workflow definition:** Clear phases and completion criteria
3. **Progress tracking:** Metadata recording, status tables
4. **Knowledge capture:** Cross-level learning via memory systems
5. **Quality gates:** Test suites as objective evaluation criteria

### Applicable Patterns for Open-MPM

- **Research Agent Pattern:** Understanding problem scope before implementation
- **Delegation Pattern:** Orchestrating work across team members
- **Metadata Patterns:** Tracking progress and decisions
- **Testing Strategy:** Using provided test suites as acceptance criteria
- **Documentation Focus:** Architecture decisions documented in READMEs

---

## References

**Source Locations:**
- Challenge specs: `/Users/masa/Projects/ai-coding-bake-off/challenges/level-N-*/PROBLEM.md`
- Rubrics: `/Users/masa/Projects/ai-coding-bake-off/challenges/level-N-*/evaluation/rubric.md`
- Test suites: `/Users/masa/Projects/ai-coding-bake-off/challenges/level-N-*/test_suite/`
- Claude-MPM harness: `/Users/masa/Projects/ai-coding-bake-off/harnesses/claude-mpm/`

**Key Files:**
- `/Users/masa/Projects/ai-coding-bake-off/harnesses/claude-mpm/instructions/CLAUDE.md` - Primary instructions
- `/Users/masa/Projects/ai-coding-bake-off/harnesses/claude-mpm/.claude-mpm/PM_INSTRUCTIONS.md` - Orchestration guide
- `/Users/masa/Projects/ai-coding-bake-off/harnesses/README.md` - Harness overview

---

**Document Status:** Research complete. Ready for integration with open-mpm project documentation.

**Last Updated:** 2026-04-22

**Next Steps for Integration:**
1. Link this research to relevant open-mpm architecture documentation
2. Use bake-off patterns as reference implementations for agent delegation
3. Consider bake-off evaluation metrics for measuring open-mpm effectiveness
