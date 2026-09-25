Fixed

- `tm hook --pm-guard` refuses a dispatched agent's `git checkout <branch>`,
  `git checkout -b`, `git switch`, `git stash` or `git bisect` step in a main
  checkout that holds uncommitted work. The refusal names the checkout and
  sends the agent to its own worktree. When `git status --porcelain` cannot
  read the checkout, the switch is refused rather than assumed clean. A clean
  main checkout, the PM, and path restores inside the agent's own worktree
  stay allowed. Refs #8572.
- `tm hook --pm-guard` refuses a HEAD switch or a whole-tree-destructive git
  command whose `cd`/`git -C` directory it cannot expand (a `$MAIN`, a `$(…)`
  or a backtick, quoted or not) from any working directory. Before, an agent
  in its own worktree could run `git -C $MAIN checkout <branch>` or
  `git -C "$(cat f)" reset --hard`, because the guard read the path as the
  worktree. The commit and worktree-removal rules now treat a `$(…)` or
  backtick directory as unresolved too. Refs #8572.
- `tm hook --pm-guard` judges every segment of a composed command for a
  whole-tree-destructive git verb, not only the first. Before,
  `git reset --hard && git -C <main> reset --hard` run from a worktree was
  allowed. Refs #8572.
- `tm pr open`'s refusal for a `--head` the changelog gate cannot judge no
  longer says to check the head out; it names the worktree that holds it.
  Refs #8572.
