---
name: pytest
description: Fixture scoping, parametrize, respx for HTTP mocking, pytest-asyncio, monkeypatch
tags: [pytest, testing, fixtures, parametrize, mocking, python, asyncio]
---

# pytest Testing Framework Skill

## Test Quality Rules

- **Never write stub tests.** Every `test_*` function must contain real assertions that verify behavior.
- **Forbidden patterns** in test bodies: `raise NotImplementedError`, `pass`, `...` (ellipsis), or any placeholder that makes the test vacuously pass.
- **Minimum bar**: if you are unsure what to assert, at minimum verify the import works and the function returns without raising.
- **Before finishing**: scan all `test_*.py` files for `NotImplementedError`, `pass`, or `...` bodies and replace with real logic.

```python
# ❌ WRONG — stub test
def test_create_user():
    raise NotImplementedError

# ❌ WRONG — vacuous test
def test_create_user():
    pass

# ✅ CORRECT — real assertion
async def test_create_user(api_client):
    response = await api_client.post("/users", json={"name": "Alice"})
    assert response.status_code == 201
    assert response.json()["name"] == "Alice"
```

## Fixture Scoping

Choose the narrowest scope that keeps tests independent. Wider scopes share state
across tests — use them only when setup is expensive (database connections, servers).

```python
import pytest

# function scope (default) — recreated for every test function
@pytest.fixture
def user():
    return User(id=1, email="test@example.com")

# module scope — shared across all tests in the file
@pytest.fixture(scope="module")
def db_connection():
    conn = create_test_db()
    yield conn
    conn.close()

# session scope — shared for the entire test run
@pytest.fixture(scope="session")
def docker_compose_service():
    # spin up a test postgres container once
    service = start_postgres()
    yield service
    service.stop()
```

## @pytest.mark.parametrize with ids

Use `ids` to give parametrized cases human-readable names in the test output.
Avoid relying on the default (index-based) id.

```python
@pytest.mark.parametrize(
    "email, expected",
    [
        ("user@example.com", True),
        ("not-an-email", False),
        ("missing@", False),
        ("@nodomain", False),
    ],
    ids=["valid", "no-at-sign", "missing-domain", "missing-local"],
)
def test_email_validation(email: str, expected: bool) -> None:
    assert is_valid_email(email) == expected
```

## respx for Mocking httpx Calls

Use `respx` (not `responses` or `unittest.mock`) when your code uses `httpx`.
It integrates cleanly with pytest-asyncio and provides a fluent assertion API.

```python
import respx
import httpx
import pytest

@pytest.mark.asyncio
async def test_fetch_user_returns_parsed_model() -> None:
    with respx.mock:
        respx.get("https://api.example.com/users/1").mock(
            return_value=httpx.Response(200, json={"id": 1, "email": "a@b.com"})
        )
        result = await fetch_user(1)
    assert result.email == "a@b.com"

# Check that a request was made
@pytest.mark.asyncio
async def test_creates_user_sends_post() -> None:
    with respx.mock as mock:
        route = mock.post("https://api.example.com/users").mock(
            return_value=httpx.Response(201, json={"id": 99})
        )
        await create_user(UserCreate(email="new@example.com", name="New"))
    assert route.called
    assert route.call_count == 1
```

## pytest-asyncio with asyncio_mode="auto"

Configure `asyncio_mode = "auto"` in `pyproject.toml` so every `async def test_*`
function runs under asyncio without the explicit `@pytest.mark.asyncio` decorator.

```toml
# pyproject.toml
[tool.pytest.ini_options]
asyncio_mode = "auto"
```

```python
# No @pytest.mark.asyncio needed with asyncio_mode="auto"
async def test_async_operation() -> None:
    result = await some_async_call()
    assert result is not None
```

## conftest.py Patterns

Put shared fixtures in `conftest.py` at the highest scope where they are needed.
Never import from `conftest.py` directly — pytest discovers it automatically.

```python
# tests/conftest.py
import pytest
import pytest_asyncio
from httpx import AsyncClient
from app.main import app  # or create_app() factory

@pytest_asyncio.fixture
async def api_client() -> AsyncClient:
    # lifespan="on" is REQUIRED — triggers FastAPI startup events (DB init, scheduler, etc.)
    # Never use ASGITransport(app=app) — it bypasses lifespan and causes "no such table" failures
    async with AsyncClient(app=app, base_url="http://test", lifespan="on") as client:
        yield client
```

> **Warning:** `ASGITransport(app=app)` does NOT trigger FastAPI lifespan events.
> Always use `AsyncClient(app=app, lifespan="on")` instead.

## When to Use monkeypatch vs Custom Fixtures

- **monkeypatch**: temporary patches to environment variables, module-level
  attributes, or built-in functions within a single test. Reverted automatically.
- **Custom fixture**: when the same patch is needed across multiple tests, or
  when the patched object needs parameterization.

```python
def test_reads_env_var(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("API_KEY", "test-key-123")
    assert get_api_key() == "test-key-123"

def test_disables_network(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("httpx.AsyncClient.send", pytest.AsyncMock(side_effect=Exception("no network")))
    with pytest.raises(NetworkError):
        await call_external_api()
```

## tmp_path Fixture

Use the built-in `tmp_path` fixture instead of `tempfile.mkdtemp()`. It is
automatically cleaned up and is test-isolated.

```python
def test_writes_config_file(tmp_path: Path) -> None:
    config = {"key": "value"}
    dest = tmp_path / "config.json"
    write_config(config, dest)
    assert dest.exists()
    assert json.loads(dest.read_text()) == config
```

## capsys for Capturing stdout/stderr

```python
def test_prints_summary(capsys: pytest.CaptureFixture) -> None:
    print_summary({"total": 5, "passed": 4, "failed": 1})
    captured = capsys.readouterr()
    assert "4 passed" in captured.out
    assert "1 failed" in captured.out
```

## Assert Rewriting (No msg= Needed)

pytest rewrites `assert` statements to produce detailed failure messages
automatically. Never add a bare string message as `assert expr, "message"` unless
the message adds context the rewritten assert cannot provide.

```python
# Good — pytest shows the values of result and expected automatically
assert result == expected

# Only add msg= when the assert itself is ambiguous
assert len(items) > 0, f"expected non-empty items list, got: {items!r}"
```

## Test Naming Convention

Name tests `test_should_<behavior>_when_<condition>`. This makes the test report
readable as a specification.

```python
def test_should_return_404_when_user_not_found() -> None: ...
def test_should_hash_password_when_user_created() -> None: ...
def test_should_reject_duplicate_email_when_registering() -> None: ...
```

## Anti-patterns

- Never use `assert isinstance(x, SomeClass)` to test behavior — test the value.
- Never share mutable state between tests via module-level variables.
- Never silence expected exceptions with a bare `try/except` — use `pytest.raises`.
- Never use `time.sleep()` in tests — use `pytest-anyio` timeouts or mock time.
- Never test multiple independent behaviors in one test function.
