Fixed

- **`trusty-memory doctor` no longer reports HEALTHY while the daemon is
  wedged (issue #4001).** During the #3992 incident six threads sat parked in
  `concurrent_open::backoff_sleep_ms` with a `memory_remember` hung ~1800 s,
  and doctor reported healthy throughout — it checked HTTP liveness, fastembed
  cache state, and lock-file staleness, none of which can observe a wedged
  worker pool. The daemon-health check now reads the `/health` body and fails
  on a reported wedge, naming the age and in-flight count.

- **`trusty-memory doctor` distinguishes "could not determine" from "down"
  (issue #4005).** New `CheckStatus::Unknown`, rendered `❔` and counted in its
  own summary column rather than folded into `passed`. A probe that times out
  now reports Unknown instead of a hard failure, and a 2xx whose body carries
  no worker observation (an older daemon, or an unreadable body) is Unknown
  rather than a pass doctor cannot support. The `/health` probe budget also
  rose from 2 s to 10 s: the default handler samples RSS/CPU behind a mutex and
  enumerates open file descriptors, work the MCP request path never does, so
  under load it could miss a 2 s budget that real traffic never approached.

---
