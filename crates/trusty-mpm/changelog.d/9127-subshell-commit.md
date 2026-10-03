Fixed

- `tm hook --pm-guard` now refuses a `git commit` aimed at a main checkout
  when it is wrapped in a `( … )` subshell, a `{ …; }` brace group, a nested
  group, a `sh -c`/`bash -c` string holding a group, or behind a reserved word
  such as `then`, `do` or `!`. These forms were allowed because the guard read
  `(git` or `{` as the program. The destructive-git and HEAD-move rules read
  the same walker, so a grouped `git reset --hard` is refused too.
- A `cd` inside a subshell or a `sh -c` string no longer moves the directory
  the guard resolves for the commands after it, so
  `sh -c 'cd <worktree>' && git commit -a` in a main checkout is refused.
- A `git commit` inside grouping that does not balance is refused from any
  directory, since the guard cannot tell where it lands. Grouped commits in a
  worktree or a scratchpad clone, and grouped reads such as `(git status)`,
  stay allowed.
