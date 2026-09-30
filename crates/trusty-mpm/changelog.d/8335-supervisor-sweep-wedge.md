Fixed

- The supervisor's sweep loop can no longer wedge silently (#8335). Each fleet
  sweep, post-merge cleanup pass and metrics publish is bounded (10 minutes by
  default); a step that outlives it is abandoned, logged at ERROR, and the
  loop keeps ticking. A watchdog task logs at ERROR when the published
  heartbeat's `written_at` passes its staleness window, and at INFO when it
  recovers. The loop ending any way other than a shutdown signal (a dropped
  future or a panic) is now logged at ERROR.
