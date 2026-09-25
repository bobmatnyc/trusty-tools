Fixed

- `tm hook` no longer wraps a Bash command in `| tm compress` when the call
  runs inside a `.claude/worktrees/` isolation worktree, or when the hook cannot
  read the call's working directory. Claude Code's worktree-isolation
  classifier refused the wrapped shape, so `git diff`, `ls -la` and
  `cargo test` never ran for an isolated agent. Outside isolation worktrees the
  rewrite is unchanged. Refs #7477.
