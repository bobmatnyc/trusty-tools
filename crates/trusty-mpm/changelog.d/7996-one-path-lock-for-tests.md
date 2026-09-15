Fixed

- Test-only: `$PATH` mutation and PATH-resolved `tmux` spawns now share one
  lock per test target, so `gh_identity`'s fake-`gh` override can no longer
  tear the `$PATH` read of a concurrent `tmux` exec in the
  `test_support::tmux_session` fixture (#7996).
