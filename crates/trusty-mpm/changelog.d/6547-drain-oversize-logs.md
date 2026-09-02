Fixed

- The daemon's log-drain scheduler no longer re-logs the same "skipping oversize
  file" warning every 15-minute cycle. `trusty-common`'s drain records each skip
  decision in the manifest, so a file whose size and mtime have not moved is
  counted in silence; the drain produced 1,276 identical warnings in 48 hours
  over ~40 files that can never shrink (#6547). The `log_drain` doctor row now
  reads `N over the size ceiling (M newly recorded)`, where a zero `M` says the
  backlog is settled.
- `log_drain.max_wire_bytes` is a new config knob (default 64 MiB) bounding the
  COMPRESSED body handed to the destination. The collector streams since #6547,
  so the source size no longer bounds memory and `log_drain.max_file_bytes`
  defaults to 4 GiB — high enough that a daily-rotated daemon log drains rather
  than being skipped. A zero `max_wire_bytes` is a config error, not a drain
  that quietly sends nothing.
