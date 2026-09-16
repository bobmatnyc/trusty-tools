Fixed

- `tm doctor` `worktree_disk` no longer prints a budget-expired partial sum in the total's position. When the 3s probe budget expires the row now reads `total disk use UNKNOWN — at least <bytes> measured across <m> of <n> worktree(s)`, instead of `41.8 MiB across 212 worktree(s)` for a 146 GB store ([#7886](https://github.com/bobmatnyc/trusty-tools/issues/7886))
