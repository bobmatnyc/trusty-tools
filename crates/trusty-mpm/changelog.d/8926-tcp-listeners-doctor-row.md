Added

- `tm doctor` has a `tcp_listeners` row. It lists the trusty-* processes that
  listen on TCP, read from libproc on macOS and `/proc` on Linux, and grades
  them against `crates/trusty-mpm/src/daemon/tcp_listener_allowlist.tsv`. Only
  trusty-console may listen on TCP (ADR-0032). The row is OK when only the
  console listens, WARNs naming the issue while tm (#6288) or trusty-search
  (#6285) still listen, FAILs on any other trusty-* listener, and reports
  UNKNOWN, never OK, when the probe cannot run. The CI lint
  `scripts/check_no_tcp_listeners.sh` reads the same allowlist and fails on
  any TCP bind in source outside it.
