Fixed

- A per-project `github.config_dir` binding now wins over the shell's own
  `GH_TOKEN`. `gh` reads an environment token ahead of `GH_CONFIG_DIR`, so a
  `tm` run from a shell exporting another account's token authenticated as that
  account and `tm session prune-worktrees --merged-prs` reported "Could not
  resolve to a Repository" for every repo that token could not see. A resolved
  binding now removes `GH_TOKEN`, `GITHUB_TOKEN`, `GH_ENTERPRISE_TOKEN`,
  `GITHUB_ENTERPRISE_TOKEN`, `GH_USER` and — under a `token_env` binding —
  `GH_CONFIG_DIR` from the child, keeping only what the binding itself sets.
  The removal applies ONLY when a binding resolves an identity: an unbound
  call, and a binding that sets only `host` or the informational `GH_USER`,
  still inherit the ambient environment unchanged (#6668).
- The env file every managed Claude Code / tmux session sources now carries
  `unset` lines ahead of its exports, so a daemon started from a shell that
  exports another account's `GH_TOKEN` no longer hands that token to the
  spawned session for its whole lifetime. Which vars are cleared comes from
  the same resolver the `gh` subprocess path uses (#6668).
- `pm_guard` classifies the command inside a `sh -c` / `bash -c` / `zsh -c` /
  `env -S` / `xargs` wrapper instead of the wrapper itself. The worktree-remove
  deny, the main-checkout HEAD-move rule and the destructive-delete rule all
  read their segments from one splitter, which now also emits the wrapped
  command; nested wrappers and quoting are followed up to eight layers.
  `pm_guard` refuses outright, ahead of every Bash rule and ahead of the
  subagent exemption, when it cannot establish what a command would run: a
  wrapper whose inner command will not lex, `$'…'`/`$"…"` quoting the lexer
  cannot decode (`sh -c $'git worktree remove x'` used to read as `$git` and
  match no rule), or nesting past the descent budget. Each of the three was a
  live bypass of the #5791 worktree-remove deny from a real subagent dispatch
  (#6660).
