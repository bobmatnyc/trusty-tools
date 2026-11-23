---
name: fastapi
description: Pydantic v2 models, async routes, dependency injection, HTTPException, lifespan
tags: [fastapi, pydantic, async, rest, api, python, http]
---

# FastAPI Framework Skill

## Pydantic v2 Models

Use `model_config = ConfigDict(...)` instead of the v1 inner `class Config`.
All models should be strict about extra fields.

```python
from pydantic import BaseModel, ConfigDict, Field, field_validator

class UserCreate(BaseModel):
    model_config = ConfigDict(
        str_strip_whitespace=True,
        str_min_length=1,
        extra="forbid",       # reject unknown fields
    )
    email: str = Field(..., examples=["user@example.com"])
    name: str = Field(..., min_length=2, max_length=100)
    role: Literal["admin", "user"] = "user"

    @field_validator("email")
    @classmethod
    def validate_email(cls, v: str) -> str:
        if "@" not in v:
            raise ValueError("invalid email")
        return v.lower()

class UserResponse(BaseModel):
    model_config = ConfigDict(from_attributes=True)  # allows ORM objects
    id: int
    email: str
    name: str
```

## Always Use async def for Route Handlers

FastAPI routes that do any IO (database, HTTP, file) MUST be `async def`.
Sync `def` routes are run in a threadpool and should only be used for
CPU-bound operations.

```python
from fastapi import APIRouter

router = APIRouter(prefix="/users", tags=["users"])

@router.get("/{user_id}", response_model=UserResponse)
async def get_user(user_id: int, db: AsyncSession = Depends(get_db)) -> UserResponse:
    user = await db.get(User, user_id)
    if user is None:
        raise HTTPException(status_code=404, detail=f"User {user_id} not found")
    return UserResponse.model_validate(user)
```

## Dependency Injection with Depends()

Use `Depends()` for any reusable logic: database sessions, authentication,
configuration, pagination parameters.

```python
from fastapi import Depends, HTTPException, status
from fastapi.security import HTTPBearer

security = HTTPBearer()

async def get_current_user(
    token: HTTPAuthorizationCredentials = Depends(security),
    db: AsyncSession = Depends(get_db),
) -> User:
    user = await verify_token(token.credentials, db)
    if user is None:
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="Invalid token",
            headers={"WWW-Authenticate": "Bearer"},
        )
    return user

# Reuse across routes
@router.get("/me", response_model=UserResponse)
async def get_me(current_user: User = Depends(get_current_user)) -> UserResponse:
    return UserResponse.model_validate(current_user)
```

## HTTPException Patterns

Always use named status codes from `fastapi.status` or `http.HTTPStatus`.
Include a `detail` that is helpful to the API consumer.

```python
from fastapi import HTTPException
from fastapi import status

# 404 Not Found
raise HTTPException(
    status_code=status.HTTP_404_NOT_FOUND,
    detail=f"Resource '{resource_id}' not found",
)

# 422 Validation error is raised automatically by Pydantic — don't re-raise it.

# 409 Conflict
raise HTTPException(
    status_code=status.HTTP_409_CONFLICT,
    detail="Email already registered",
)

# 500 with logging
import logging
log = logging.getLogger(__name__)
try:
    result = await risky_operation()
except Exception as exc:
    log.exception("unexpected error in risky_operation")
    raise HTTPException(
        status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
        detail="Internal server error",
    ) from exc
```

## Lifespan Context Manager for Startup/Shutdown

Use `lifespan` (not deprecated `on_event`) for startup and shutdown logic.

```python
from contextlib import asynccontextmanager
from fastapi import FastAPI

@asynccontextmanager
async def lifespan(app: FastAPI):
    # startup
    await database.connect()
    await cache.initialize()
    yield
    # shutdown
    await database.disconnect()
    await cache.close()

app = FastAPI(title="My API", lifespan=lifespan)
```

## DELETE Endpoints and 204 No Content

Never combine `status_code=204` with `response_model` — FastAPI raises `AssertionError` at startup:

```python
# ❌ WRONG — AssertionError: Status code 204 must not have a response body
@router.delete("/{id}", status_code=204, response_model=DeleteResponse)
async def delete_item(id: int): ...

# ✅ CORRECT — 204 with no response model
@router.delete("/{id}", status_code=204)
async def delete_item(id: int) -> None:
    service.delete(id)

# ✅ ALTERNATIVE — 200 with response model
@router.delete("/{id}", status_code=200, response_model=DeleteResponse)
async def delete_item(id: int) -> DeleteResponse:
    service.delete(id)
    return DeleteResponse(deleted=True)
```

## response_model Parameter

Always specify `response_model` on routes that return data. This controls
serialisation and strips internal fields from the response.

```python
@router.post("/", response_model=UserResponse, status_code=status.HTTP_201_CREATED)
async def create_user(
    payload: UserCreate,
    db: AsyncSession = Depends(get_db),
) -> UserResponse:
    user = User(**payload.model_dump())
    db.add(user)
    await db.commit()
    await db.refresh(user)
    return UserResponse.model_validate(user)
```

## Query, Path, and Body Parameters

```python
from fastapi import Query, Path, Body
from typing import Annotated

@router.get("/")
async def list_users(
    page: Annotated[int, Query(ge=1, description="Page number")] = 1,
    size: Annotated[int, Query(ge=1, le=100)] = 20,
    search: str | None = Query(default=None, max_length=100),
) -> list[UserResponse]:
    ...

@router.put("/{user_id}")
async def update_user(
    user_id: Annotated[int, Path(ge=1)],
    payload: Annotated[UserUpdate, Body(embed=False)],
    current_user: User = Depends(get_current_user),
) -> UserResponse:
    ...
```

## Background Tasks

Use `BackgroundTasks` for fire-and-forget work that should not block the response.

```python
from fastapi import BackgroundTasks

async def send_welcome_email(email: str) -> None:
    await mailer.send(to=email, subject="Welcome!", body="...")

@router.post("/register", response_model=UserResponse, status_code=201)
async def register(
    payload: UserCreate,
    background_tasks: BackgroundTasks,
    db: AsyncSession = Depends(get_db),
) -> UserResponse:
    user = await create_user_in_db(payload, db)
    background_tasks.add_task(send_welcome_email, user.email)
    return UserResponse.model_validate(user)
```

## Router Organization

Split routes into separate router files grouped by resource. Mount them in
`main.py` with a common prefix and tags.

```python
# routers/users.py
router = APIRouter(prefix="/users", tags=["users"])

# main.py
from fastapi import FastAPI
from routers import users, products, auth

app = FastAPI()
app.include_router(auth.router)
app.include_router(users.router)
app.include_router(products.router)
```

## Testing FastAPI Apps

### App Module Layout Convention

- The entry point file MUST be named `app.py` (not `main.py`) — tests and tooling expect `from <package>.app import app`.
- `app.py` should create and export `app = FastAPI(lifespan=lifespan)` directly at module level.
- Conventional import path: `from weather_alerter.app import app`.

### Module-Level State Pattern for TestClient Compatibility

**The problem:** `app.state.db` is only available after the lifespan fires. `TestClient(app)` used WITHOUT a context manager does NOT fire lifespan, so `request.app.state.db` raises `AttributeError`.

**The fix:** Always use module-level state with a lazy initializer so the app works with both `TestClient(app)` and `with TestClient(app)`:

```python
# ✅ CORRECT — module-level state, TestClient-compatible
_db_path: str = ""

def _get_db_path() -> str:
    return _db_path or os.environ.get("DATABASE_URL", "app.db")

@asynccontextmanager
async def lifespan(app: FastAPI) -> AsyncIterator[None]:
    global _db_path
    _db_path = _get_db_path()
    await init_db(_db_path)
    yield

# In route handlers: use _get_db_path() not app.state.db
@router.get("/items")
async def list_items(request: Request) -> list[ItemResponse]:
    db_path = _get_db_path()  # works even without lifespan
    ...
```

```python
# ❌ WRONG — fails when TestClient is used without context manager
@asynccontextmanager
async def lifespan(app: FastAPI) -> AsyncIterator[None]:
    app.state.db = await aiosqlite.connect(":memory:")  # only runs in context manager
    yield

@router.get("/items")
async def list_items(request: Request) -> list[ItemResponse]:
    db = request.app.state.db  # AttributeError if lifespan never ran
```

### Test Fixtures for Async FastAPI

```python
# For async tests (pytest-asyncio) — use asgi_lifespan for explicit lifespan control
import pytest
from asgi_lifespan import LifespanManager
from httpx import ASGITransport, AsyncClient

@pytest.fixture
async def client():
    from mypackage.app import app
    async with LifespanManager(app):
        async with AsyncClient(
            transport=ASGITransport(app=app),
            base_url="http://testserver",
        ) as ac:
            yield ac

# For sync tests — use TestClient as a context manager
from fastapi.testclient import TestClient
from mypackage.app import app

@pytest.fixture
def sync_client():
    with TestClient(app) as client:  # ← context manager triggers lifespan
        yield client

# ❌ WRONG — lifespan never fires, app.state.* unavailable
def sync_client_wrong():
    return TestClient(app)  # no context manager
```

### pyproject.toml Build Backend

```toml
# ✅ CORRECT
[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"

# ❌ WRONG — hatchling.backends does not exist
build-backend = "hatchling.backends"
```

### Test Dependencies for Async FastAPI

```toml
[project.optional-dependencies]
dev = [
    "pytest>=8.0",
    "pytest-asyncio>=0.23",
    "httpx>=0.27",
    "asgi-lifespan>=2.1",  # required for async test fixtures
]
```

Add `asyncio_mode = "auto"` to pytest config:

```toml
[tool.pytest.ini_options]
asyncio_mode = "auto"
```

## Anti-patterns

- Never use `sync def` for database or HTTP calls inside routes.
- Never return raw dicts — always use a `response_model` Pydantic model.
- Never swallow exceptions silently — log and re-raise as HTTPException.
- Never hardcode status codes as bare integers — use `status.HTTP_*` constants.
- Never use `app.on_event` (deprecated) — use `lifespan`.
- Never store request-scoped or app-scoped state on `app.state.*` if it must be readable by `TestClient(app)` without a context manager — use module-level state with a lazy initializer.
- Never name the FastAPI entry point `main.py` — convention is `<package>/app.py` exporting `app`.
