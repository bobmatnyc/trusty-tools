Fixed

- `git stash` is now prohibited in `BASE-AGENT.md`, so every bundled agent
  inherits the rule unconditionally instead of depending on a dispatching PM to
  remember it. The stash stack is repo-global rather than per-worktree, so a
  concurrent agent's `pop` can restore and drop another session's work while
  reporting success — three live incidents, the most recent recoverable only via
  `git fsck --unreachable`
  ([#4730](https://github.com/bobmatnyc/trusty-tools/issues/4730)).
- The `tm-workflow` and `git-workflow` skills stopped recommending the operation
  that caused those incidents. `tm-workflow`'s "Escape hatch — stash first" and
  `git-workflow`'s "Stashing Work" both taught a bare stash followed by a blind
  `pop`; both now teach a throwaway worktree
  (`git worktree add /tmp/baseline-$$ origin/main`), and `tm-workflow`'s
  subagent-dispatch rule no longer narrows the ban to the main checkout
  ([#4730](https://github.com/bobmatnyc/trusty-tools/issues/4730)).
