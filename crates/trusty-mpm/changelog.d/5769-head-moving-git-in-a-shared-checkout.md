Added

- `git pull`, `git merge` and `git rebase` are now denied in a project's main checkout when the daemon reports another agent writing in that same directory (ADR-0048 decision 10). All three move the shared git HEAD under whoever else is standing on it, and git reports no error when they do. The rule binds the PM and every agent it dispatches. A pull in a worktree, a pull in a checkout nobody else is writing in, `git fetch`, and the `--abort`/`--continue` family stay allowed, and the denial names `git fetch` and a worktree as the two remedies.
