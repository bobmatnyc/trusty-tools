Added

- `tm shell-init <zsh|bash|fish>` prints a `tm` wrapper function so your shell is left in the alias's repo after `tm run <alias>` exits ([#4986](https://github.com/bobmatnyc/trusty-tools/pull/4986))
  - install it yourself: `eval "$(tm shell-init zsh)"` in `~/.zshrc`, or `tm shell-init fish | source` in `config.fish`. The command is print-only — nothing in trusty-mpm writes to a shell rc file
  - a process cannot change its parent shell's cwd, so before this a session that ran in another repo left the shell behind and a later bare `tm` re-detected the wrong project
  - the wrapper resolves through the existing `tm path <alias>`, which derives the path from `(alias, root)` and never persists it; there is no state file or env-var channel
  - it passes the real exit status through unchanged, `cd`s only for `tm run`, and stays silent and stationary when `tm path` fails, prints nothing, or names a directory that is gone
