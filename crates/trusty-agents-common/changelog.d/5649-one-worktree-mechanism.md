Changed

- `BASE-AGENT.md` no longer tells agents to create their own worktree. Agents
  stay in the tree they were given and ask the PM to re-dispatch with
  `isolation: "worktree"` (or to serialize) when they have none — a self-made
  worktree is invisible to `tm hook --pm-guard` and gets the next dispatch
  wrongly denied (#5649).
