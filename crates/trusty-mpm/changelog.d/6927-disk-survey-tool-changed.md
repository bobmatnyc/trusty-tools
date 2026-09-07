Changed

- `worktree_reclaim::classify` takes the owner keep-list and applies it as gate 0, ahead of every other gate: a keep-listed worktree reports `Blocked` naming the operator's own entry, so it stays visible in the survey instead of disappearing from it, and `tm session prune-worktrees --merged-prs` can never propose it. `tm doctor`'s worktree-disk probe applies the same list, so its reclaimable figure and the command it advertises agree with the sweep.
