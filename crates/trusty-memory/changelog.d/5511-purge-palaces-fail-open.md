Fixed
- **`kg-rebuild`'s registry reads no longer fail open.** `purge_palaces` and
  `rebuild_palaces` both read the palace list through
  `PalaceRegistry::list_palaces(...).unwrap_or_default()`, so a failed read
  became zero palaces and each pass reported a clean, empty run over data it had
  never opened. A macOS TCC denial on the data dir reaches that arm —
  `read_dir` returns EPERM — so the exit-0 was reachable, not theoretical. Both
  now propagate the registry error with the path they could not read.

  What each one was costing differs, and the fix is worth the same weight for
  different reasons. `--purge-stale-subjects` is destructive: it printed a
  zero-count summary and exited 0 while the operator believed subjects had been
  deleted, and the next thing they would do is trust that the graph was cleaned.
  The plain back-fill only asserts, so nothing was destroyed — it just reported
  `0 drawers, 0 triples` about a rebuild that never opened a palace, and an
  operator watching for a triple count would see a real-looking zero.

  This completes the pattern #5401 started in `merge_palaces`; all three passes
  over the palace registry now fail the run rather than reporting an empty one.
