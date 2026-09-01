Fixed

- `tm session prune-worktrees --merged-prs` now reclaims worktrees whose pull
  request was SQUASH-merged. A squash replays a branch as one new commit on
  `main`, and `gh pr merge --squash --delete-branch` then removes the remote
  branch, so every landed commit read as unpushed and the dirty-work gate
  refused the tree — the flag reclaimed nothing on a repository that
  squash-merges. The gate now discounts a commit only when `git cherry` proves
  its patch is already on a remote landing branch; a commit that landed nowhere
  is still refused, and an unanswerable comparison leaves the count untouched
  (#6507).
