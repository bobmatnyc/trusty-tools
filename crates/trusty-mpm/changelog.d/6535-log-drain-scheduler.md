Added

- The daemon can now periodically upload its own log files to an object store.
  A new `log_drain:` section in `~/.trusty-tools/trusty-mpm/config.yaml` names
  the destination URI (`s3://bucket/prefix` or `file:///path`), the interval,
  the per-source include globs, the per-file size ceiling, and any extra
  literal secrets to scrub before upload. The drain is OFF by default and the
  daemon spawns no scheduler without it; a malformed section is a hard error
  that refuses to start the drain rather than a silent skip. `config_read` now
  returns the section (#6535, Phase 3 of #6533).
- `tm doctor` gained a `log_drain` row reporting whether the drain is on, which
  destination scheme it points at, and how the last pass ended. A pass that
  errored — including one that completed with per-file failures — reports
  `Fail`, never a drained-looking `Ok` (#6535).
