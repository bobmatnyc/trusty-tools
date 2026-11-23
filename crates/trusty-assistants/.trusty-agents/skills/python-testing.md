---
name: python-testing
description: pytest best practices, fixtures, parametrize, coverage, and subprocess testing
tags: [python, pytest, testing, fixtures, coverage, subprocess, cli]
---

# Python Testing Best Practices

## Running tests correctly

```bash
# Always set PYTHONPATH when testing installed packages
PYTHONPATH=. pytest tests/ -v
PYTHONPATH=src pytest tests/ -v  # for src/ layout

# CLI subprocess tests — set PYTHONPATH in the test itself
import subprocess, sys, os
from pathlib import Path

result = subprocess.run(
    [sys.executable, "-m", "your_package", "--flag"],
    env={**os.environ, "PYTHONPATH": str(Path(__file__).parent)},
    capture_output=True, text=True
)
```

## Fixture patterns

```python
import pytest

@pytest.fixture
def sample_data():
    return [{"col": "val"}]

@pytest.mark.parametrize("input,expected", [
    ("a", "A"),
    ("b", "B"),
])
def test_something(input, expected):
    assert input.upper() == expected
```

## Test naming convention

Use `test_should_<behavior>_when_<condition>` so failures read as sentences:

- `test_should_return_empty_list_when_input_is_empty`
- `test_should_raise_value_error_when_input_is_negative`

## Coverage targets

- Business logic: 95%+
- Public APIs: 100%
- Overall: 80%+
