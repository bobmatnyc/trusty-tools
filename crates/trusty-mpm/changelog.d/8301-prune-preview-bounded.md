Fixed

- `tm session prune-worktrees --merged-prs` previews now finish in bounded
  time. The daemon stops classifying after 10 minutes, and every `gh`/`git`
  call a worktree's inspection starts ends at that deadline. A worktree the
  preview did not reach, or whose inspection the deadline interrupted, is
  reported as not inspected and kept — never as removable. Before, a preview
  over ~240 worktrees ran 78 minutes, one bounded call at a time, and the
  client gave up first (#8301).
- The preview prints a heartbeat every 30 s while it waits on the daemon, in
  place of one line and then silence.
- The reclaim survey lists every pull request in one `gh pr list` call, so a
  branch outside the old 400-row page, or its review-round stem, no longer
  costs a `gh` lookup of its own.
- A `gh` call the survey deadline cuts short no longer counts toward the
  `gh` polling suspension, and `git merge-base --is-ancestor` in the
  head-commit match is now bounded like every other sweep git call.
