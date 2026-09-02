Fixed

- `tm session prune-worktrees --merged-prs` now reclaims the harness's own
  `.claude/worktrees/agent-*` worktrees. It reads git's lock reason, which tells
  the harness's agent-lifetime lock from an operator's `git worktree lock`: a
  tree the harness still holds is spared and reported as agent-owned instead of
  being dropped silently, and a tree the harness has released is reclaimable
  even when the delegation registry — rebuilt empty at every daemon restart —
  has never heard of its agent. The merged-PR and unsaved-work gates are
  unchanged, so an unmerged branch or an uncommitted file still refuses, and a
  tree carrying no ownership sentinel at all stays unreclaimable (#6561).
