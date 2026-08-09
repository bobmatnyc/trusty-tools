Documentation

- the bundled `tm-workflow` skill now carries the full generic worktree discipline, so a project's `CLAUDE.md` no longer has to restate it ([#5269](https://github.com/bobmatnyc/trusty-tools/pull/5269))
  - moved in from this repo's `CLAUDE.md`: branch off `origin/main` never local `main`, "experiments stay session-local", the stash-first escape hatch for a one-off main-checkout command, `git push origin --delete` as the manual cleanup case, QA agents getting their own worktree, and the note that worktree cleanup never touches the main checkout
  - `tm-cli-operations`' `prune-worktrees` line keeps its `.worktrees/` path — that is `tm`'s own session provisioning, which still writes there — and now says so, so it is not mistaken for the canonical `.claude/worktrees/` home
