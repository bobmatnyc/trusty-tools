Fixed
- `tm pr merge` cleans up only the merged PR's own head worktree and head branch, prints that plan before removing anything, and reports any other tree on the merged head commit as left in place for `tm pr cleanup <n>` instead of removing it (#8301).
- `tm pr merge --no-cleanup` (alias `--keep-worktree`) skips the post-merge local cleanup entirely, and `--no-delete-branch` now skips it too, since that cleanup deletes branches (#8301).
