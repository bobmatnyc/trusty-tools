Fixed

- The orphan-GC worktree sweep no longer stops the daemon from answering while a git
  call hangs. Its git subprocesses, path canonicalization and removals now run off the
  async runtime; each git call is killed after a wall-clock ceiling (60 s, or 15 min for
  `git worktree remove --force`); and a worktree whose check or removal timed out is
  kept and reported instead of removed
  ([#7965](https://github.com/bobmatnyc/trusty-tools/issues/7965))
