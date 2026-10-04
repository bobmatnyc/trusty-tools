Fixed

- `tm hook --pm-guard` now refuses a `git commit` aimed at a main checkout
  when it is wrapped in a `( … )` subshell, a `{ …; }` brace group, a nested
  group, a `sh -c`/`bash -c` string holding a group, or behind a reserved word
  such as `then`, `do`, `!` or `coproc` (with or without a coproc NAME). These
  forms were allowed because the guard read `(git`, `{` or `coproc` as the
  program. The destructive-git and HEAD-move rules read the same walker, so a
  grouped `git reset --hard` is refused too.
- A `cd` inside a subshell, a coproc or a `sh -c`/`env -S`/`flock -c`/`xargs`
  string no longer moves the directory the guard resolves for the commands
  after it, so `sh -c 'cd <worktree>' && git commit -a` in a main checkout is
  refused. A `cd` inside `eval` does persist, as it does in bash, so
  `eval "cd <main checkout>"; git commit` from a worktree is refused. Behind a
  process wrapper such as `env` or `nohup`, `eval` runs in a child process, so
  `env eval "cd <worktree>"; git commit -a` in a main checkout is refused.
  Only assignments or a bare `builtin` keep `eval` in the current shell. Behind
  `command`, `command -p`, `builtin --`, `noglob` or `nocorrect`, bash and zsh
  disagree on where `eval` runs, so a `git commit` or destructive git command
  in that command is refused as one the guard cannot place.
- A `git commit` the guard cannot place is refused from any directory: inside
  grouping that does not balance (a stray `)` in a comment counts), a
  paren-form `case` arm, a function definition, or a coproc. The check reads
  `g''it`, `c''ommit` and `co\mmit` as `git` and `commit`, and a brace
  expansion such as `{git,-C,<dir>,commit}` as its words. A destructive git
  command (`reset --hard`, `clean -f`, `checkout -- .`, …) in such a command is
  refused the same way. Grouped commits in
  a worktree or a scratchpad clone, quoted commit messages containing
  parentheses, and grouped reads such as `(git status)` stay allowed.
- The HEAD-switch, main-checkout HEAD-move, linked-worktree HEAD-move and
  worktree-removal rules refuse a command the guard cannot place, as the
  commit and destructive rules do. From a dirty main checkout,
  `command -p eval "cd <worktree>"; git stash` (or `git checkout x`) was
  judged in the worktree and allowed, while zsh never runs that `cd`.
- A command whose program word is a brace expansion, such as
  `{git,-C,<dir>,commit}`, is refused as unclassifiable, like `$'…'` quoting.
- A source-scan test now fails CI when a pm-guard rule reads the shell-group
  walk without refusing a command the walk cannot place.
