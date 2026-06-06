---
name: python-packaging
description: Best practices for Python pyproject.toml, setuptools, and pip packaging
tags: [python, packaging, pyproject, setuptools, pip, wheel, build]
---

# Python Packaging Best Practices

## Build backends

Two common, well-supported build backends:

```toml
# setuptools (most common, broadest tool compatibility)
[build-system]
requires = ["setuptools>=68.0", "wheel"]
build-backend = "setuptools.build_meta"

# hatchling (modern, simpler config, used by hatch)
[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"   # ✅ correct
# build-backend = "hatchling.backends"  # ❌ WRONG — module does not exist
```

Pick one and stick with it for the project. Do not mix backends.

## pyproject.toml minimal template

```toml
[build-system]
requires = ["setuptools>=68.0", "wheel"]
build-backend = "setuptools.build_meta"

[project]
name = "your_package"
version = "0.1.0"
requires-python = ">=3.11"

[project.scripts]
your-cli = "your_package.__main__:main"

[tool.setuptools.packages.find]
where = ["src"]
```

## Common mistakes

- Use `setuptools.build_meta`, not the internal `_legacy` backend
- Set `where = ["src"]` if using the src/ layout, `where = ["."]` for flat layout
- Always set `PYTHONPATH=src` when running pytest with a src/ layout
- CLI entry point format: `"module.submodule:function"`
- Do not forget to include `__init__.py` files for every package directory

## Editable installs

For development, use `pip install -e .` so edits show up immediately without
reinstalling. This requires `pyproject.toml` with a valid `[build-system]`
section and a build backend that supports PEP 660.

## src-layout Projects

For projects using the `src/` layout (`src/package_name/__init__.py`), pytest cannot find the package without explicit configuration. Always add to `pyproject.toml`:

```toml
[tool.pytest.ini_options]
pythonpath = ["src"]
testpaths = ["tests"]
asyncio_mode = "auto"
```

Without `pythonpath = ["src"]`, pytest imports the system-installed version of the package instead of the local one, causing stale-code bugs that are hard to diagnose.

## Test Dependencies

Always declare test dependencies explicitly in `pyproject.toml` so `uv sync --extra test` installs everything needed:

```toml
[project.optional-dependencies]
test = [
    "pytest>=8.0",
    "pytest-asyncio>=0.23",
    "httpx>=0.27",
    # add project-specific test deps here (fakeredis, respx, etc.)
]
```

Never rely on test dependencies being installed globally or transitively.
