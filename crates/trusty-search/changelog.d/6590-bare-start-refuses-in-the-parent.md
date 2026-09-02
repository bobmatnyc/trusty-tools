Fixed

- A bare `trusty-search start` now refuses when a daemon is already running,
  instead of reporting success (#6590). Without `--foreground`, `start` forks a
  detached `--foreground` child with its stdio on `/dev/null` and returns — and
  the already-running check ran only inside that child. The child raised the
  refusal into `/dev/null` while the parent had already printed "Daemon starting
  in background (pid N)" and exited 0, so an operator was told a daemon started
  when none had. The parent runs the check before forking now and exits non-zero
  with the same remedy text (the pid, `kill -TERM <pid>`, and the flush window).
  The `--foreground` path is unchanged: launchd and systemd never fork here and
  reach the same check where they always did.
