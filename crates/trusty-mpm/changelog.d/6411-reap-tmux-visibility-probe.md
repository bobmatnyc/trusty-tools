Fixed

- The tmux reap-parity test no longer predicts reap's behaviour from whether the
  `tmux` binary resolves
  (refs [#6411](https://github.com/bobmatnyc/trusty-tools/issues/6411)).
  `DaemonState::reap_dead_sessions` reaps nothing when `list-sessions` fails, and
  on a host where no tmux server has ever run that listing fails with
  `error connecting to <socket> (No such file or directory)` — a string
  `TmuxDriver::list_sessions` deliberately does not classify as an empty server,
  so a registry is never wiped by an unreachable tmux. `TmuxDriver::discover()`
  succeeds on that same host, because the binary is installed, so gating on it
  expected a removal that could not happen and turned `main` red. The probe now
  runs the listing itself.
- The `no server running` / `no sessions` stderr classification is one named
  function, `stderr_means_empty_server`, instead of the same pair of `contains`
  calls at three listing sites, so the boundary between an empty server and an
  unreachable one cannot drift between them.
