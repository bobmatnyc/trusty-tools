Fixed

- The supervisor's sweep loop can no longer wedge silently (#8335). The fleet
  sweep and the post-merge cleanup sweep now run on the blocking pool, so a
  `gh`, `git` or tmux call that blocks its thread no longer holds the loop.
  The loop stops waiting after `tick_timeout` (10 minutes by default; an
  await inside the sweep is dropped at that bound, a blocked call is given
  500 ms more) and logs at ERROR. A blocked call is not killed: its thread and
  child process run until the call returns, and later sweeps of that kind are
  skipped until it does. Each session's `resume_auto` and classification
  awaits for at most `step_timeout` (2 minutes by default) and is logged with
  the session id, name and step when it expires.
- Abandoned sweeps are visible. The published run stats carry
  `sweeps_abandoned`, `consecutive_sweeps_abandoned` and
  `cleanup_sweeps_abandoned`, and a snapshot whose last fleet sweep was
  abandoned reads `stale`, even though the loop still publishes it on time.
- A watchdog task logs at ERROR when the heartbeat goes stale, and at INFO
  when it recovers. It skips its verdict after a wall-clock gap of more than
  twice its period, so waking from system sleep does not log a false ERROR.
  The loop ending any way other than a shutdown signal (a dropped future or a
  panic) is logged at ERROR.
