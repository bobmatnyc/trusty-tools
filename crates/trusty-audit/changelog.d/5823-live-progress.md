Added

- `taudit install`, `taudit clone` and `taudit run` show live progress instead
  of waiting silently. The sweep reports the stages `tga audit` is actually
  running, relayed out of each child rather than swallowed into its log file —
  a sweep that used to show nothing for up to four hours per repository now
  names the stage in flight ([#5823](https://github.com/bobmatnyc/trusty-tools/issues/5823))
- `Session::with_progress` takes the sink a front end renders through, so
  `Session::execute` still runs with no terminal and the Tauri shell renders the
  same updates its own way. Absent, every update is discarded and the
  capabilities behave exactly as before
- Off a terminal — CI, a pipe, a captured run — the display degrades to one
  plain line per state change: no escape sequences, no repainting, and no line
  for a mid-flight counter
