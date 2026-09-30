Changed
- `tm-workflow` skill (worktree provisioning): Python projects under worktree isolation use a per-worktree venv with `pip install -e --no-cache-dir --force-reinstall`, or `PYTHONPATH=<worktree>/src`, so pip's wheel cache cannot bind an editable install to a sibling worktree (Refs #8386).
