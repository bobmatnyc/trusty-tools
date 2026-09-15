Documentation

- `tm-delegation-patterns` PM Re-Engagement now tells the PM to check
  `git worktree list` for an isolated agent's tree before `SendMessage`-resuming
  it, and to re-dispatch with `isolation: "worktree"` when the tree is gone —
  a reclaimed tree otherwise resumed the agent in the main checkout, where it
  could not commit
  (refs [#8004](https://github.com/bobmatnyc/trusty-tools/issues/8004))
