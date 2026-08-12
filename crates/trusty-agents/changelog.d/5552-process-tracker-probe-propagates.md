Fixed

- `ProcessTracker` no longer rewrites `processes.json` from a load it could not perform ([#5552](https://github.com/bobmatnyc/trusty-tools/issues/5552))
  - `load` probed the tracker file with `try_exists(..).unwrap_or(false)`, which collapsed "could not determine" into "absent" and returned an empty map. Every entry point is a read-modify-write around `load()`/`save()`, and `save()` renames over `processes.json`, so one transient probe error — `EIO`, `ETIMEDOUT`, `ESTALE` on a network mount — left the tracker holding only the PID being written. Those forgotten children could no longer be reaped by `cleanup_stale` or signalled by `shutdown_all`, and kept running until killed by hand.
  - The probe now propagates. `register`, `mark_completed`, `cleanup_stale` and `shutdown_all` dropped their `.unwrap_or_default()` and propagate too, so a failed load surfaces instead of being reported as `Ok(0)` reaped or a completed shutdown.
  - Genuine absence is unchanged and still benign: a first run, a missing parent directory (an unplugged volume gives `ENOENT`, not a probe failure), and a dangling symlink all still load as an empty map.
  - Governing rule: [ADR-0045](https://github.com/bobmatnyc/trusty-tools/pull/5559) — distinguish absent from undeterminable before a destructive filesystem operation.
