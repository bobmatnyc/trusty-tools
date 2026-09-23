Fixed
- `tm pr merge` cleans up only the merged PR's own head worktree and head branch, prints that plan before removing anything, and reports any other tree on the merged head commit as left in place for `tm pr cleanup <n>` instead of removing it (#8301).
- `tm pr merge --no-cleanup` (alias `--keep-worktree`) skips the post-merge local cleanup entirely, and `--no-delete-branch` now skips it too, since that cleanup deletes branches (#8301).
- Both choices now persist in the cleanup registry: the daemon's periodic merged-PR sweep never touches a PR whose cleanup the operator deferred, and retries a blocked merge-chained cleanup with the same head-only scope instead of the full cleanup (#8301).
