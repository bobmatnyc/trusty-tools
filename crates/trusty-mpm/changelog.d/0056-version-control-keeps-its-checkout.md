Changed

- `tm hook --pm-guard` no longer diverts a `version-control` dispatch into an
  isolation worktree, and no longer denies one beside another writer. Its
  writes are the merge into main, the branch delete, and the merged-worktree
  reclaim, none of which can be done from inside a worktree. It still counts as
  an occupant, so an engineer dispatched into the same directory beside it is
  denied exactly as before, and engineer source-write confinement is unchanged.
  (ADR-0056; narrows #4480 and #5650 for one agent name)
