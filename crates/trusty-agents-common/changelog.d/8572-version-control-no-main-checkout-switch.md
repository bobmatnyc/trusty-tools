Changed

- The `version-control` agent never moves HEAD in a main checkout: no branch
  checkout, `switch`, `stash` or `reset --hard` there. It pushes with
  `git push origin <branch>`, runs `tm pr open` from the worktree that holds
  the branch, and asks the PM for an isolated worktree when none does.
  Refs #8572.
