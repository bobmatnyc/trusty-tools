Fixed

- The `version-control` agent no longer falls back to a fleet-wide
  `prune-worktrees` sweep when the guard refuses one worktree removal. It
  reports the path and the refusal to the PM and stops. Refs #8577.
