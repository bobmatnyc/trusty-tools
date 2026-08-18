Changed

- The daemon lock file carries a `product = "trusty-mpm"` field and readers
  reject any record without it
  (see [#1731](https://github.com/bobmatnyc/trusty-tools/issues/1731)).
  `daemon.lock` is not a unique filename — Claude Code writes one too, at a path
  containing "trusty-mpm" — and a reader that trusts the path alone can hand an
  operator an unrelated PID. A daemon started by an older binary writes no such
  field, so its record is ignored until that daemon restarts; discovery falls
  back to the console gateway probe and the default URL meanwhile, and
  `tm daemon` now also probes the address it is about to bind so it cannot spawn
  a duplicate during that window.
