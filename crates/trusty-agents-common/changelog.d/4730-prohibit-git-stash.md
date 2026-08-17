Fixed

- `BASE-AGENT.md` now prohibits `git stash` outright and points at a throwaway
  worktree instead. The stash stack is repo-global rather than per-worktree, so
  a concurrent agent's `pop` can restore and drop another session's work while
  reporting success. Putting it here is the point: every bundled agent inherits
  this file, so the rule no longer depends on a dispatching PM remembering to
  include it — which is why documenting it three other places did not stop three
  live incidents ([#4730](https://github.com/bobmatnyc/trusty-tools/issues/4730)).
