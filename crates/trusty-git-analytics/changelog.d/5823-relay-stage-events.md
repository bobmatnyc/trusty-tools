Added

- `tga audit` writes its per-stage progress events to stderr as machine-readable
  lines when the parent process sets `TRUSTY_PROGRESS_RELAY`, so a spawning tool
  can show what the sweep is doing. Unset, nothing is emitted and the command
  behaves exactly as before; a parent that sets it against an older `tga` gets
  no events rather than a failed run
  ([#5823](https://github.com/bobmatnyc/trusty-tools/issues/5823))
- The two phases after the sweep — indexing every checkout, then rendering the
  report — announce themselves on the same bus. They had no instrumentation at
  all, so a watcher's display stopped while the process ran on for minutes
