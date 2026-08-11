Added

- `BASE-AGENT.md` now carries the agent-facing half of the shared-working-tree
  rule: a file-mutating agent works in its own worktree and never
  `git checkout`/`git switch` in a directory it was handed, because a
  concurrently-dispatched sibling shares that git HEAD. It names
  `tm hook --pm-guard` as the enforcement so the deny message and the prose point
  at each other (#4480).
