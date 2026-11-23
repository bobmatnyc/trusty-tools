Fixed
- `tm pr cleanup` no longer deletes `origin/<head>` while a worktree it refused or failed to remove still holds the head branch, or when the worktree listing could not be read. The `remote-branch` step reports the kept branch and the run exits non-zero.
