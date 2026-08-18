Fixed

- `tm services` reports the PID of the process bound to a service's port, not
  the first `pgrep -f` name match
  (closes [#5951](https://github.com/bobmatnyc/trusty-tools/issues/5951)).
  The manifest's `process_match: "trusty-mpm"` is a substring every sibling
  process shares, so `trusty-mpm-daemon` was reported with a
  `trusty-mpm serve --stdio` bridge's PID while `launchctl list`, `ps`, the
  daemon's own `/health` and the console gateway all named the daemon. Since
  this repo's guidance sends operators to `tm services` instead of raw
  `ps`/`lsof`, that PID is one somebody signals. Name matching remains the
  fallback for portless services such as `trusty-embedderd`, for a port nothing
  is listening on, and for a host without `lsof`.
