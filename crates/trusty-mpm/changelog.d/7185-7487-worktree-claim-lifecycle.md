Fixed

- `tm pr merge`'s post-merge cleanup now clears the harness's own
  `.trusty-mpm-worktree` ownership marker before `git worktree remove`, so git's
  clean check agrees with the clean-tree decision tm already made instead of
  refusing every merged worktree in a project that does not gitignore the
  marker. The removal is still never forced. (Refs #7185)
- A shared-tree dispatch claim is released when the agent is cancelled with
  `TaskStop`, and a dispatch the guard denies no longer records one, so a
  stopped agent plus one refusal can no longer exhaust a working directory for
  the rest of a session. (Refs #7487)
