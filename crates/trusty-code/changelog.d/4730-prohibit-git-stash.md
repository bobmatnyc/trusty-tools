Fixed

- The bundled `git-workflow` skill's "Stashing Work" section taught a bare
  `git stash` followed by a blind `git stash pop`. The stash stack is
  repo-global rather than per-worktree, so that recipe is how a concurrent
  agent's `pop` restores and drops another session's work. It now leads with the
  hazard, recommends a throwaway worktree, and shows the labelled push /
  `list`-then-apply-by-ref form as the fallback — kept byte-identical to
  trusty-mpm's copy of the same skill
  ([#4730](https://github.com/bobmatnyc/trusty-tools/issues/4730)).
