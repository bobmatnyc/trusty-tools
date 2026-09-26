Fixed

- The `python-engineer` agent's Development Workflow section spells the test gate `.venv/bin/python -m pytest` instead of `source .venv/bin/activate`, which a worktree agent's harness refuses ([#8514](https://github.com/bobmatnyc/trusty-tools/issues/8514)).
