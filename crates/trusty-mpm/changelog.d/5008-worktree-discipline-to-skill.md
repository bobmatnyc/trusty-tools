Changed

- `tm-workflow`'s "Worktree Discipline" section now carries the rest of the checkout rules that root `CLAUDE.md` used to state, so every project deploying the skill gets them rather than only this repo
  - the main checkout's forbidden-operation list is now explicit: any edit, any build or test run, any destructive git op (`git reset --hard`, `git checkout .`, `git stash`, `git restore .`), any file-mutating command (`sed`/`awk`/`patch`), and anything else that mutates the working tree, index, or build output
  - a worktree is a writer and the branch is the workstream — the branch is the durable unit, a worktree is ephemeral and recreatable, and losing one loses nothing the branch does not still hold
  - one branch and worktree per independently reviewable PR outcome, not per ticket, refactor step, or experiment
  - the stash-first escape hatch is labelled a narrow exception, not license for routine edits from the main checkout, and project-specific worktree hazards are directed to the project's own reference docs
- `docs/reference/worktree-discipline.md` gains this repo's Cargo/macOS specifics — installing a freshly built binary from the worktree, the `cargo install` cdhash-cache rationale with a cross-link to the release-workflow hazard, and the stash-first fallback for a post-merge install from the main checkout
