Changed

- `tm health`, `tm status`, `tm doctor`, `tm sessions …` (except `tui` and
  `disk`), `tm pr …`, `tm ticket` and `tm build-lease`'s decision log now
  reach the daemon only over its unix socket (`TRUSTY_MPM_SOCKET`, else the
  data-dir `trusty-mpm.sock`). They no longer probe the console gateway or
  any TCP address, and there is no HTTP fallback (ADR-0032, #6288).
- With the socket absent or refusing, `tm health` and `tm status` now exit
  non-zero with an error naming the socket path, instead of printing
  `daemon: unreachable` and exiting 0.
- `tm doctor`'s `trusty_mpm_daemon` row names the transport it probed, for
  example `reachable (via unix socket …)`.
