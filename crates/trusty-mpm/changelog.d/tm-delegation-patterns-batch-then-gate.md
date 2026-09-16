Changed

- `tm-delegation-patterns` skill strengthens same-crate bug-fix batching into a PM dispatch rule: one dispatch/PR per crate cluster (up to ~5 issues), one worktree, one gate run — per-issue dispatch only for file collisions or a High-risk fix needing its own critic round (owner ruling 2026-09-16)
