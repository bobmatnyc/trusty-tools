---
name: wave-planning
description: Decompose multi-file projects into assignments.json waves with dependency ordering
tags: [wave-planning, assignments, decomposition, workflow, topological-sort]
---

# Wave Planning Workflow Skill

## What is Wave Planning?

Wave planning is the decomposition of a multi-file project into ordered batches
(waves) of files, where each wave contains only files whose dependencies were
completed in earlier waves. This enables one code agent per file to run
sequentially within each wave, producing clean, well-scoped output.

## assignments.json Schema

Write this file FIRST, before any stub files. The wave loop engine reads it to
know what to build and in what order.

```json
{
  "error_convention": "exceptions",
  "waves": [
    {
      "wave": 1,
      "files": [
        {
          "path": "src/models.py",
          "stub": "models.py",
          "purpose": "Pydantic data models shared across the codebase",
          "max_lines": 120,
          "depends_on": []
        },
        {
          "path": "src/utils.py",
          "stub": "utils.py",
          "purpose": "Stateless utility functions (formatting, validation)",
          "max_lines": 80,
          "depends_on": []
        }
      ]
    },
    {
      "wave": 2,
      "files": [
        {
          "path": "src/repository.py",
          "stub": "repository.py",
          "purpose": "Database access layer using SQLAlchemy async session",
          "max_lines": 200,
          "depends_on": ["src/models.py"]
        }
      ]
    },
    {
      "wave": 3,
      "files": [
        {
          "path": "src/service.py",
          "stub": "service.py",
          "purpose": "Business logic layer orchestrating repository calls",
          "max_lines": 180,
          "depends_on": ["src/models.py", "src/repository.py"]
        },
        {
          "path": "tests/test_service.py",
          "stub": "test_service.py",
          "purpose": "pytest tests for the service layer",
          "max_lines": 150,
          "depends_on": ["src/service.py", "src/models.py"]
        }
      ]
    }
  ]
}
```

## Field Descriptions

| Field | Type | Required | Description |
|---|---|---|---|
| `error_convention` | string | no | "exceptions" or "Result" — agents use this to choose error style |
| `waves` | array | yes | Ordered list of wave objects |
| `wave` | integer | yes | 1-indexed wave ordinal |
| `files` | array | yes | Files to implement in this wave |
| `path` | string | yes | Destination path relative to out_dir (e.g. `src/foo.py`) |
| `stub` | string\|null | yes | Filename in `stubs/` directory, or null for no stub |
| `purpose` | string | yes | One-line intent shown to the code agent |
| `max_lines` | integer | no | Agent will self-constrain to this budget (default 300) |
| `depends_on` | array | no | Paths this file reads for context (must be from earlier waves) |

## Topological Ordering Rules

- Wave 1: Independent files only — no `depends_on` entries.
- Wave N: May depend on any file from waves 1 through N-1.
- Within a wave: files are implemented sequentially (later files cannot depend on earlier ones in the same wave).
- Tests for a module belong in the SAME wave as that module, or a later wave.

```
Wave 1: models.py, config.py, utils.py   (no deps)
Wave 2: repository.py (deps: models.py)
         client.py (deps: config.py)
Wave 3: service.py (deps: models.py, repository.py)
         tests/test_repository.py (deps: repository.py, models.py)
Wave 4: main.py (deps: service.py, config.py)
         tests/test_service.py (deps: service.py, models.py)
```

## Stub Files

Stubs define function signatures, class structures, and docstrings that the
code agent must implement verbatim. They live in `stubs/` relative to out_dir.

```python
# stubs/repository.py
from sqlalchemy.ext.asyncio import AsyncSession
from src.models import User, UserCreate

async def create_user(db: AsyncSession, payload: UserCreate) -> User:
    """INTENT: Persist a new user and return the created ORM instance."""
    ...

async def get_user_by_id(db: AsyncSession, user_id: int) -> User | None:
    """INTENT: Fetch one user by primary key; return None if absent."""
    ...
```

## Write assignments.json FIRST

Your very first `write_file` call in the plan phase must be `assignments.json`.
The engine detects its presence to trigger the wave loop. Without it, the code
phase falls back to a single monolithic invocation.

```
# Correct order in plan phase:
1. write_file("assignments.json", "...")    ← FIRST
2. write_file("stubs/models.py", "...")
3. write_file("stubs/repository.py", "...")
4. ...other stubs
```

## Max Lines Guidance

Use these defaults unless the task demands otherwise:

| File Type | max_lines |
|---|---|
| Type/model files | 80-150 |
| Utility functions | 60-120 |
| Repository/DAO layer | 150-250 |
| Service/business logic | 150-250 |
| API route handlers | 100-200 |
| Test files | 100-200 |
| Main/entrypoint | 60-100 |

## Anti-patterns

- Never put a file in wave N if it depends on another file in wave N.
- Never write stubs after `assignments.json` in a separate step — write all stubs before calling `finish_task` in the plan phase.
- Never omit `depends_on` for files that read other project files — the code agent reads each listed file for context.
- Never write more than 8-10 files total in one wave plan; prefer 2-4 files per wave.
