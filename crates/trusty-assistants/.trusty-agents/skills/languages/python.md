---
name: python
description: Pythonic idioms, type hints, dataclasses, context managers, generators
tags: [python, idioms, typing, dataclass, pydantic, pathlib, generators]
---

# Python Language Skill

## Type Hints on All Public Functions (PEP 484)

Every public function and method must carry complete type annotations. Unannotated
public APIs are a maintenance hazard.

```python
# Good
def fetch_user(user_id: int, include_deleted: bool = False) -> Optional[User]:
    ...

# Bad — no annotations, caller has no contract
def fetch_user(user_id, include_deleted=False):
    ...
```

Use `from __future__ import annotations` at the top of every file to enable
forward references without quotes in Python 3.10+.

## Dataclasses vs Pydantic v2

**Prefer Pydantic v2 BaseModel for any data that crosses a trust boundary** (HTTP
request/response, config files, database rows). Use `@dataclass` only for pure
in-process value objects with no validation needs.

```python
# Pydantic v2 — for validated, serialisable models
from pydantic import BaseModel, field_validator, model_config, ConfigDict

class UserCreate(BaseModel):
    model_config = ConfigDict(str_strip_whitespace=True, str_min_length=1)
    email: str
    name: str
    role: Literal["admin", "user"] = "user"

    @field_validator("email")
    @classmethod
    def email_must_contain_at(cls, v: str) -> str:
        if "@" not in v:
            raise ValueError("must contain @")
        return v.lower()

# dataclass — for simple in-process value objects
from dataclasses import dataclass, field

@dataclass(frozen=True)
class Point:
    x: float
    y: float
    label: str = ""
```

**Never use dict literals** as the primary data representation in business logic.
Wrap them in a typed model immediately on ingestion.

## Context Managers with contextlib

Use `contextlib.contextmanager` for simple resource management rather than full
`__enter__`/`__exit__` classes.

```python
from contextlib import contextmanager, asynccontextmanager
from typing import Generator

@contextmanager
def managed_connection(dsn: str) -> Generator[Connection, None, None]:
    conn = connect(dsn)
    try:
        yield conn
    finally:
        conn.close()

@asynccontextmanager
async def lifespan(app: FastAPI):
    # startup
    await db.connect()
    yield
    # shutdown
    await db.disconnect()
```

## Generator Patterns

Use generators for lazy sequences that could be large. Prefer `yield from`
over explicit loops when delegating to another iterable.

```python
# Good — lazy, memory-efficient
def read_chunks(path: Path, chunk_size: int = 4096) -> Generator[bytes, None, None]:
    with open(path, "rb") as f:
        while chunk := f.read(chunk_size):
            yield chunk

# yield from — delegates to inner iterable cleanly
def flatten(nested: list[list[T]]) -> Generator[T, None, None]:
    for inner in nested:
        yield from inner
```

## When NOT to Use Classes

Use a plain function (not a class with a single `__call__`) for stateless
transforms. A class is justified only when it carries state between calls or
when you need subtype polymorphism.

```python
# Over-engineered — class adds zero value
class Doubler:
    def __call__(self, x: int) -> int:
        return x * 2

# Correct
def double(x: int) -> int:
    return x * 2
```

## pathlib over os.path

Always use `pathlib.Path` for file system operations. It is composable,
expressive, and avoids string concatenation bugs.

```python
# Good
from pathlib import Path

config_path = Path(__file__).parent / "config" / "settings.toml"
output = Path(out_dir) / "results.json"
output.parent.mkdir(parents=True, exist_ok=True)
output.write_text(json.dumps(results, indent=2))

# Bad
import os
config_path = os.path.join(os.path.dirname(__file__), "config", "settings.toml")
```

## f-strings over .format()

Use f-strings for all string interpolation. They are faster and more readable.

```python
# Good
name = "world"
msg = f"Hello, {name!r}!"
log.info(f"Processing {len(items)} items in {elapsed:.2f}s")

# Bad
msg = "Hello, {!r}!".format(name)
msg = "Hello, %r!" % (name,)
```

## Explicit is Better than Implicit

Avoid magic; be direct about what the code does.

```python
# Good — caller controls retry behaviour explicitly
def call_api(url: str, retries: int = 3, timeout: float = 5.0) -> dict:
    ...

# Avoid global mutable config that callers cannot see
_GLOBAL_RETRIES = 3
def call_api(url: str) -> dict:
    for _ in range(_GLOBAL_RETRIES):
        ...
```

## List Comprehensions vs map/filter

Prefer list comprehensions for simple transforms; use `map`/`filter` only when
passing an already-named function (avoids a redundant lambda).

```python
# Prefer comprehension for clarity
squares = [x * x for x in range(10) if x % 2 == 0]

# map is fine when you have a named function
import math
roots = list(map(math.sqrt, values))

# Avoid lambda with map — just use a comprehension
bad = list(map(lambda x: x * x, range(10)))  # worse than [x*x for x in range(10)]
```

## Proper __all__ Exports

Every module that is meant to be imported by other modules should define
`__all__` listing its public API. This prevents accidental re-export of
internal helpers.

```python
# my_module.py
__all__ = ["PublicClass", "public_function"]

class PublicClass:
    ...

def public_function() -> None:
    ...

def _internal_helper() -> None:  # NOT in __all__
    ...
```

## Anti-patterns

- Never use mutable default arguments: `def f(items=[])` — use `None` and assign inside.
- Never use bare `except:` — always catch a specific exception type.
- Never use `type()` for isinstance checks — use `isinstance()`.
- Never shadow built-ins (`list`, `dict`, `id`, `type`, `input`).
- Never use `print()` in library code — use `logging.getLogger(__name__)`.

## NLP Library Setup (spaCy, NLTK)

When using libraries that require downloaded models, always ensure models are available before tests run.

### spaCy

Add a session-scoped autouse fixture in `conftest.py`:

```python
@pytest.fixture(scope="session", autouse=True)
def download_nlp_models():
    import spacy
    if not spacy.util.is_package("en_core_web_sm"):
        spacy.cli.download("en_core_web_sm")
```

Document in README:
```bash
uv run python -m spacy download en_core_web_sm
```

### NLTK

```python
@pytest.fixture(scope="session", autouse=True)
def download_nltk_data():
    import nltk
    nltk.download("punkt", quiet=True)
    nltk.download("stopwords", quiet=True)
```
