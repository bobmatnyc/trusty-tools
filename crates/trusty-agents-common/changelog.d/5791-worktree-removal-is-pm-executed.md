Changed

- `BASE-AGENT.md` no longer tells an agent to remove its worktree and delete its branch after a merge. Neither dispatch path could carry that out, and `tm hook --pm-guard` now denies the command outright, so the instruction produced a deadlock rather than cleanup. The bullet now says what to do instead: report the merged PR, the worktree path, and the branch, then stop — the PM confirms the work is done and reclaims the tree with `tm session prune-worktrees --merged-prs --force` (owner ruling 2026-08-19, Refs #5791).
