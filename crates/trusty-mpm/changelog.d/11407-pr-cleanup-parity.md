Fixed
- `tm pr cleanup` now refuses whatever its `--dry-run` refuses: a head-branch worktree or branch whose tip moved past the merged head with work the merge did not carry is kept by both modes, and the remote head branch is deleted only after every tree is judged (#11407).
