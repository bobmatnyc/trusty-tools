Fixed

- BASE-ENGINEER's WIP-squash guidance now names the captured merge-base or task-start SHA as the `git reset --soft` target, never `origin/main` directly, and requires a `git status --porcelain` check before committing (#7891).
