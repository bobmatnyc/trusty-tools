---
name: tdd
description: Write failing test first, red-green-refactor, test naming conventions
tags: [tdd, testing, workflow, red-green-refactor, test-first]
---

# Test-Driven Development (TDD) Workflow Skill

## The Red-Green-Refactor Cycle

TDD is a design technique, not just a testing technique. The cycle forces you
to think about the API before the implementation.

1. **Red**: Write a test that fails because the feature does not exist yet.
   Run the test; confirm it fails for the right reason (not a syntax error).
2. **Green**: Write the minimum code to make the test pass. Do not over-engineer;
   ugly but correct is fine at this stage.
3. **Refactor**: Clean up both the implementation and the test while keeping the
   test green. Remove duplication, improve names, extract helpers.

Commit after each green step. Refactor is safe because tests are green.

## Write the Test File FIRST

Your first tool call must be `write_file` for the test file. Do not write any
implementation code until at least one test exists.

```
# Correct order:
1. write_file("tests/test_user_service.py", "...")  ← test file first
2. write_file("src/user_service.py", "...")          ← implementation second

# Wrong order:
1. write_file("src/user_service.py", "...")  ← implementation without a failing test
```

## One Assertion Focus per Test

Each test should verify one specific behavior. Multiple unrelated assertions
in one test make it hard to pinpoint failures.

```python
# Good — one behavior per test
def test_should_return_user_when_id_exists():
    user = repository.find(id=1)
    assert user.id == 1

def test_should_raise_not_found_when_id_missing():
    with pytest.raises(NotFoundError):
        repository.find(id=999)

# Bad — two behaviors in one test
def test_user_repository():
    user = repository.find(id=1)
    assert user.id == 1
    with pytest.raises(NotFoundError):
        repository.find(id=999)
```

## Test the Behavior, Not the Implementation

Tests should describe WHAT the code does, not HOW it does it. Avoid asserting
on internal state, private methods, or implementation details.

```python
# Good — tests external behavior
def test_should_send_welcome_email_when_user_registers():
    service.register(email="new@example.com", name="Alice")
    assert email_spy.sent_to == "new@example.com"

# Bad — tests internal state
def test_should_add_user_to_internal_list():
    service.register(email="new@example.com", name="Alice")
    assert len(service._users) == 1  # internal detail
```

## Naming Convention: test_should_<verb>_when_<condition>

This pattern makes the test suite a readable specification.

```
test_should_return_empty_list_when_no_users_exist
test_should_raise_validation_error_when_email_missing
test_should_hash_password_when_user_created
test_should_not_allow_duplicate_emails_when_registering
test_should_paginate_results_when_page_param_provided
```

## Triangulate Before Abstracting

Write at least three test cases covering different inputs/edge cases before
extracting shared logic. Premature abstraction based on two cases often
generalizes incorrectly.

```python
# Start with a specific case
def test_should_format_single_row():
    assert format_table([["Alice", "30"]]) == "| Alice | 30 |\n"

# Add another case
def test_should_format_multiple_rows():
    rows = [["Alice", "30"], ["Bob", "25"]]
    result = format_table(rows)
    assert "Alice" in result and "Bob" in result

# Add an edge case
def test_should_return_empty_string_for_empty_input():
    assert format_table([]) == ""

# NOW you have enough cases to abstract the header/separator logic safely
```

## Don't Mock What You Own

Only mock external dependencies (HTTP APIs, file system, databases in unit tests).
Do not mock your own classes just to isolate them — if they are hard to test
directly, that is a design signal to fix.

```python
# Good — mock the external HTTP call, use real service
@respx.mock
async def test_should_fetch_user_data():
    respx.get("https://api.github.com/users/alice").mock(
        return_value=httpx.Response(200, json={"login": "alice"})
    )
    result = await github_service.get_user("alice")
    assert result.login == "alice"

# Bad — mocking your own class defeats the purpose of testing it
def test_user_service():
    mock_service = MagicMock(spec=UserService)
    mock_service.find.return_value = User(id=1)
    result = mock_service.find(id=1)  # not testing anything real
    assert result.id == 1
```

## The "As-If" Principle

Write the test as if the API you wish existed already exists. Then implement
that API. This drives clean, caller-friendly interfaces.

```python
# Test written first — defines the desired API
def test_should_parse_config_from_env():
    with monkeypatch.context() as m:
        m.setenv("DB_URL", "postgresql://localhost/test")
        m.setenv("DEBUG", "true")
        cfg = Config.from_env()
    assert cfg.db_url == "postgresql://localhost/test"
    assert cfg.debug is True

# Now implement Config.from_env() to match exactly
```

## Anti-patterns

- Never write tests after the implementation is "done" — tests then mirror the
  implementation rather than driving the design.
- Never skip the red phase — a test that passes without any implementation is
  not testing the right thing.
- Never add implementation logic just to make multiple tests pass at once —
  work one test at a time.
- Never test framework/library behavior — test your code's use of it.
