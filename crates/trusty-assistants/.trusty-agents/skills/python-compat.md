---
name: python-compat
tags: [bcrypt, passlib, fastapi, httpx, spacy, pypdf, python-compat]
summary: Python library compatibility fixes for known breaking changes
---

# Python Compatibility Fixes

## passlib + bcrypt (passlib 1.7.x incompatible with bcrypt >=4.0.0)

**Problem**: `AttributeError: module 'bcrypt' has no attribute '__about__'` at runtime.

**Fix**: Pin bcrypt in pyproject.toml:
```toml
dependencies = [
    "passlib[bcrypt]>=1.7.4",
    "bcrypt>=3.2.0,<4.0.0",
]
```

## FastAPI lifespan in tests (httpx + ASGITransport bypasses startup)

**Problem**: `AsyncClient(transport=ASGITransport(app=app))` skips ASGI lifespan.
`app.state.*` and database connections are uninitialized → tests fail with AttributeError.

**Fix**: Use httpx's built-in ASGI support with lifespan enabled:
```python
# conftest.py
import pytest
from httpx import AsyncClient, ASGITransport

@pytest.fixture
async def client(app):
    async with AsyncClient(app=app, base_url="http://test") as ac:
        yield ac
```
httpx >=0.24 passes lifespan events automatically when `app=` is used directly.

## FastAPI DELETE 204 response_model

**Problem**: `AssertionError` at import when `@router.delete(..., status_code=204, response_model=SomeModel)`.

**Fix**: For 204 No Content, use `response_class=Response` and `-> None`:
```python
from fastapi import Response

@router.delete("/items/{id}", status_code=204, response_class=Response)
async def delete_item(id: int) -> None:
    ...
```

## spaCy model download in tests

**Problem**: Tests fail with `OSError: [E050] Can't find model 'en_core_web_sm'`.

**Fix**: Add autouse session fixture in conftest.py:
```python
import subprocess
import pytest

@pytest.fixture(scope="session", autouse=True)
def download_spacy_model():
    subprocess.run(["python", "-m", "spacy", "download", "en_core_web_sm"], check=True)
```

## setuptools build backend (LLM hallucinates internal identifiers)

**Problem**: `pyproject.toml` with `build-backend = "setuptools.backends._legacy:_Backend"` causes
`pip install` to fail — this is an internal, non-public identifier.

**Fix**: Always use the public build backend:
```toml
[build-system]
requires = ["setuptools>=68", "wheel"]
build-backend = "setuptools.build_meta"
```

## TestClient lifespan in Starlette 1.0+ (context manager required)

**Problem**: In Starlette 1.0+, `return TestClient(app)` from a pytest fixture does NOT
trigger the ASGI lifespan. `app.state.*` attributes set in `@asynccontextmanager` lifespan
are uninitialized → `AttributeError: 'State' object has no attribute 'db'`.

**Fix**: Use the TestClient as a context manager in the fixture:
```python
# conftest.py
import pytest
from starlette.testclient import TestClient

@pytest.fixture
def client(app):
    with TestClient(app) as c:
        yield c
```
This ensures `startup` and `shutdown` lifespan events fire before/after tests.

## pypdf AnnotationBuilder removed (pypdf 4.x)

**Problem**: `ImportError: cannot import name 'AnnotationBuilder' from 'pypdf.generic'`
in test fixtures or code that builds annotated PDFs. `AnnotationBuilder` was part of the
public API in pypdf 3.x but removed entirely in pypdf 4.0.

**Fix**: Use `PdfWriter.add_annotation()` with a dict-style annotation object, or use
the high-level helpers in `pypdf.annotations`:
```python
from pypdf import PdfWriter
from pypdf.annotations import FreeText

writer = PdfWriter()
writer.add_blank_page(width=200, height=200)
annotation = FreeText(
    text="Hello",
    rect=(50, 100, 150, 150),
)
writer.add_annotation(page_number=0, annotation=annotation)

with open("output.pdf", "wb") as f:
    writer.write(f)
```

For test fixtures that just need a valid PDF with some content (not annotations), avoid
`AnnotationBuilder` entirely and use a simpler approach:
```python
import io
from pypdf import PdfWriter

@pytest.fixture
def sample_pdf(tmp_path):
    writer = PdfWriter()
    writer.add_blank_page(width=200, height=200)
    path = tmp_path / "sample.pdf"
    with open(path, "wb") as f:
        writer.write(f)
    return path
```

**Version note**: Check installed version with `python -m pypdf --version` or
`pip show pypdf`. If pypdf >=4.0 is installed, `AnnotationBuilder` does not exist.
Pin to `pypdf>=3.0,<4.0` only if you need legacy annotation API; prefer the new
`pypdf.annotations` module for forward compatibility.
