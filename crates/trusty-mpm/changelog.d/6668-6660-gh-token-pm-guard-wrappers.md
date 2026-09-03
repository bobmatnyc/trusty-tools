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
  call, and a binding that sets only `host`, still inherit the ambient
  environment unchanged (#6668).
- `pm_guard` classifies the command inside a `sh -c` / `bash -c` / `zsh -c` /
  `env -S` / `xargs` wrapper instead of the wrapper itself. The worktree-remove
  deny, the main-checkout HEAD-move rule and the destructive-delete rule all
  read their segments from one splitter, which now also emits the wrapped
  command; nested wrappers and quoting are followed up to eight layers. A
  wrapper whose inner command cannot be lexed is denied rather than allowed
  (#6660).
