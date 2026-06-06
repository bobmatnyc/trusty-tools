---
name: python-idiomatic
tags: [language, idioms, python]
summary: Idiomatic Python coding guidelines — 2024-2025
---

# Idiomatic Python — 2024-2025

## Core Philosophy
Idiomatic Python in 2024-2025 is type-annotated, pathlib-native, f-string-formatted, Pydantic-shaped at boundaries, pytest-tested, and uv-managed. The community optimizes for explicit-over-implicit, machine-checkable contracts, and standard-library breadth — reach for stdlib before adding a dependency.

## Idioms: DO / DON'T / WHY

**DO** use `pathlib.Path` for every filesystem operation. **DON'T** use `os.path.join`, `os.getcwd`, or `+` on path strings. **WHY**: `Path` composes safely across platforms; `os.path` is stringly-typed and platform-fragile.

**DO** annotate all public function signatures including return types. Use `from __future__ import annotations` to allow forward references. **DON'T** leave `def f(x):` without annotations on non-trivial functions, and don't reach for `Any` to silence errors. **WHY**: mypy/pyright catch real bugs statically; annotations are machine-readable docs.

**DO** use Pydantic v2 (`pydantic>=2.0`) for any data crossing a boundary (HTTP, file, env vars, config). **DON'T** use Pydantic v1 patterns (`@validator`, `Config` class, `.dict()`, `parse_obj`) — they are deprecated. **WHY**: v2 has a Rust core (10-50x faster) and a different API: use `@field_validator`, `model_config`, `.model_dump()`, `.model_validate()`.

**DO** use `dataclasses.dataclass` for simple structured data without runtime validation. Reach for Pydantic only when you need validation. **DON'T** use bare dicts or `namedtuple` for structured records with more than two fields. **WHY**: dataclasses give `__repr__`, `__eq__`, and IDE support for free.

**DO** use f-strings for all string formatting. Use `f"{x=}"` for debugging output. **DON'T** use `%`-formatting or `.format()`. **WHY**: f-strings are faster, clearer, and the `=` debug form eliminates a whole category of print-debug boilerplate.

**DO** use the stdlib `tomllib` for parsing TOML on Python 3.11+. **DON'T** install the `toml` or `tomli` packages for new projects. **WHY**: `tomllib` is the standard since 3.11; third-party packages add a dependency for nothing.

**DO** use `uv` for dependency and venv management. Commit `uv.lock`. **DON'T** use Poetry for new projects, and don't run bare `pip install` without a lockfile. **WHY**: `uv` (Rust, 2024) is 10-100x faster than pip and Poetry and has converged as the new default.

**DO** use the `match` statement (PEP 634, Python 3.10+) for multi-branch dispatch on shape. **DON'T** chain `isinstance` checks. **WHY**: structural pattern matching is more readable and the compiler can flag missing cases.

**DO** raise specific exceptions (`ValueError`, `TypeError`, custom subclasses of `Exception`). **DON'T** `raise Exception("...")`, and never use bare `except:`. Always at minimum `except Exception`. **WHY**: callers can only handle errors selectively if you raise specific types.

**DO** use context managers (`with open(...) as f:`, `with lock:`) for every resource. **DON'T** call `.close()` manually. **WHY**: context managers guarantee cleanup on exception paths.

**DO** prefer `Protocol` and `TypeVar` (with bounds) for generic utilities. **DON'T** sprinkle `Any` to bypass the type checker. **WHY**: Protocols enable structural typing without inheritance; `Any` silently disables checking.

**DO** avoid mutable default arguments — use `None` and assign inside the body. **DON'T** write `def f(x=[]):`. **WHY**: mutable defaults are shared across all calls and cause subtle bugs.

## Toolchain
- **Package/venv manager**: `uv` (>= 0.4)
- **Formatter**: `ruff format` (replaces `black`)
- **Linter**: `ruff check` (replaces `flake8` + `isort` + `pylint`)
- **Type checker**: `mypy --strict` or `pyright`
- **Test runner**: `pytest` with `pytest-asyncio` for async; use `@pytest.mark.parametrize` instead of duplicate test functions
- **Build backend**: `hatchling` in `pyproject.toml` (no `setup.py`)

## Anti-Patterns to Reject
- `from module import *` — pollutes namespace, breaks tooling.
- `unittest.TestCase` for new tests — use plain pytest functions.
- `os.path.join` / `os.getcwd` — use `pathlib.Path`.
- Pydantic v1 patterns (`@validator`, `.dict()`, `parse_obj`) — they are deprecated.
- `type(x) == SomeClass` — use `isinstance(x, SomeClass)`.
- Bare `except:` — always specify the exception type.
- Mutable default arguments (`def f(x=[]):`).

## 2024-2025 Updates
- `uv` has replaced Poetry as the default dependency manager for new projects.
- Pydantic v2 is the standard; v1 patterns are deprecated and trigger runtime warnings.
- `tomllib` (stdlib, 3.11+) replaced `tomli`; the `toml` package is legacy.
- PEP 695 type-alias syntax (`type Vector = list[float]`) is stable in Python 3.12+.
- `ruff` replaced `black` + `isort` + `flake8` for most projects in 2024.
