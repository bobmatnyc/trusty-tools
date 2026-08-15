Added

- A dispatched agent that may write files is now granted its own worktree when the session is standing in a project's main checkout: `tm hook --pm-guard` rewrites the dispatch to declare `isolation: "worktree"`, so the harness provisions and reclaims the tree (ADR-0048). An agent this binary cannot classify is treated as a writer there rather than trusted.
- The ADR-0044 write boundary is enforced. In a main checkout, an edit tool targeting a source file and `git commit` are denied for the PM and for every agent it dispatches; documents, configuration, and everything in a worktree stay writable. Each denial names the remedy.
