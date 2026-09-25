Added

- `tm hook --pm-guard` governs a HEAD move into a linked worktree (#8161,
  #8494): `reset --keep`/`--hard`/`--merge`, `merge` (including `--ff-only`)
  or `rebase` whose target is a `.claude/worktrees/<name>` or
  `.worktrees/<name>` tree is denied while
  the daemon reports a live agent standing there, counting the asking session's
  own agents, and allowed when the tree is idle. An unanswered daemon or an
  unresolved target denies and names `tm repair delegation`. An agent moving
  its own tree's HEAD is exempt. This is the consolidation step of the
  "Resuming parked work" recipe in `docs/reference/worktree-discipline.md`.
