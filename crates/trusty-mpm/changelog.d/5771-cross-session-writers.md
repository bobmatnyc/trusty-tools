Fixed

- The concurrent shared-worktree guard is no longer blind across sessions. `live_shared_tree_writers` filtered on the asking session first, so agents dispatched from different sessions into one checkout could not see each other — the exact shape of the reported incident. It is now keyed by directory, which is what a shared git HEAD belongs to.
