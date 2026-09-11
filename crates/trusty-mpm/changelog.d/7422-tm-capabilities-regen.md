Fixed

- Regenerated the `tm-capabilities` skill so it matches `run_doctor`'s actual
  check list again — the `session_scope` doctor row (#7422) was lost when
  #7471's branch-regenerated files merged over it, leaving `tm generate
  capabilities --check` red on every PR's merge ref. `doctor.md` now lists
  45 checks with `session_scope` back at position 42 and `tmux_options` /
  `pty_headroom` / `log_drain` renumbered after it.
