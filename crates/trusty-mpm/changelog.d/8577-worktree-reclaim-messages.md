Fixed

- A `version-control` worktree removal refused by an ADR-0057 re-check,
  including a re-check timeout, now tells the agent to hand the worktree back
  to the PM and retry only that single path. It no longer suggests
  `tm session prune-worktrees --merged-prs --force`, which one agent ran over
  ~60 worktrees after a single refused removal. Refs #8577.
- The merged-PR worktree reclaim logs one INFO line per surveyed worktree,
  naming its path, branch and verdict: the gate and reason for a refusal, the
  landing evidence for a grant. A reclaimed entry is now as auditable as a
  blocked one. Refs #8109.
