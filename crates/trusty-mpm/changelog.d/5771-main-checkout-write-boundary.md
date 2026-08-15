Added
A dispatched agent that may write files is now granted its own worktree when the session is standing in a project's main checkout: `tm hook --pm-guard` rewrites the dispatch to declare `isolation: "worktree"`, so the harness provisions and reclaims the tree (ADR-0048). An agent this binary cannot classify is treated as a writer there rather than trusted.

Added
The ADR-0044 write boundary is enforced. In a main checkout, an edit tool targeting a source file and `git commit` are denied for the PM and for every agent it dispatches; documents, configuration, and everything in a worktree stay writable. Each denial names the remedy.

Fixed
The concurrent shared-worktree guard is no longer blind across sessions. `live_shared_tree_writers` filtered on the asking session first, so agents from different sessions in one checkout could not see each other — the exact shape of the reported incident. It is now keyed by directory, which is what a shared git HEAD belongs to.

Changed
A second session launching on the same main checkout logs at `info` rather than `warn`: with writers isolated and the boundary enforced, sharing a read-only checkout is expected.
