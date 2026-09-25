Fixed

- Every worktree-removal refusal now tells the agent to hand the one worktree
  back to the PM (or to `version-control`) and stop. None of them suggests
  `tm session prune-worktrees --merged-prs --force` any more, which one agent
  ran over ~60 worktrees after a single refused removal. This covers the
  ADR-0057 re-check denies (the timeout included, and the no-merged-PR
  details), the #5791 `git worktree remove` deny a subagent gets, and the
  #4031 deny for `rm` on a worktree directory. Refs #8577.
- The merged-PR worktree reclaim logs one INFO line per surveyed worktree,
  naming its path, branch and verdict: the gate and reason for a refusal, the
  landing evidence for a grant. A reclaimed entry is now as auditable as a
  blocked one. Refs #8109.
