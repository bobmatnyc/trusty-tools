Fixed

- `tm session prune-worktrees --merged-prs` now matches a worktree to its merged pull request by round stem (`fix/x` and `fix/x-r2` are one workstream) and by head-commit ancestry, not by exact branch name alone — a renamed or re-cut branch left 11 of 11 stale worktrees unreclaimable. A branch with no merged pull request under any route is still refused, and every `gh`/`git` failure leaves that refusal standing (#7267).
