Fixed
- The daemon's background sweeps no longer stall the request path. Every `git`,
  `gh` and `tmux` subprocess in the merged-PR worktree reclaim and the in-project
  hygiene pass now runs under a kill-on-timeout ceiling with
  `GIT_TERMINAL_PROMPT=0`, the two sweeps share one maintenance permit so they
  can never run concurrently, hygiene skips a base fetched within the last six
  hours and abandons a pass that outruns its budget, and the tmux liveness probe
  runs off the caller's thread. A claim probe that cannot answer now demotes the
  whole reclaim pass to a report instead of reading as "nothing is claimed".
  `tm doctor` gains a `background_sweeps` row reporting each sweep's switch and
  last pass duration.
