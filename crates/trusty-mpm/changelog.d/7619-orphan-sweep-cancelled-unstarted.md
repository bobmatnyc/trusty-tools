Breaking
- `OrphanSweepOutcome` gains a public `cancelled_unstarted` field, the worktrees a cancelled `prune-worktrees` pass never started (#7619). Code that builds an `OrphanSweepOutcome` with a struct literal must set it (an empty `Vec` for a pass that was not cancelled).
