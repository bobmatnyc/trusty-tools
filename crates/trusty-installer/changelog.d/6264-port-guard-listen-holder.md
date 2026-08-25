Fixed

- The #4470 foreign-port guard now names the process holding a daemon's port in
  LISTEN, not the first process `lsof` printed. A client connected TO the port
  is not a holder of it, and `lsof` lists in PID order, so any client older than
  the daemon sorted ahead of it — `tctl install` then refused to bootstrap
  trusty-search with "port 7878 is held by pid ..., which launchd does not
  supervise" while the launchd-supervised daemon was the actual listener. The
  probe now asks `lsof` for the per-file TCP state (`-FpT`) and confirms LISTEN
  itself instead of trusting the `-sTCP:LISTEN` selector to have filtered.
  - Fail-closed is unchanged: output naming no LISTEN holder is `Unknown` and
    still refuses, and an `lsof` build that reports no TCP state at all falls
    back to the previous every-PID reading rather than going blind.
  - The per-service port table is unchanged. Every daemon `tctl` guards
    (trusty-search, trusty-memory, trusty-analyze, trusty-review,
    trusty-console, the trusty-mpm supervisor) still binds a TCP port in its
    current source, so no entry was stale.
