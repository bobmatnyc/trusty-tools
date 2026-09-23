Fixed
- `tm session adopt-worktree` now leaves the adopted tree usable by the next agent: on a clean tree (no uncommitted, staged or untracked files) it detaches HEAD, so another worktree can check the branch out, and it clears the harness git lock when that lock names a pid that no longer exists (#8318).
- A dirty tree keeps its branch, and `adopt-worktree` then says why and exits non-zero; a clean check that cannot complete detaches nothing, and a lock held by a running pid or an operator is never touched (#8318).
