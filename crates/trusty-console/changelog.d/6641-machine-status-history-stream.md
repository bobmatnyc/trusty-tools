Added
- `GET /api/console/machine-status/history` returns the last 10 minutes of host
  samples (120 points at 5 s) plus the per-service transition log, oldest first.
  Before the first sample it answers 200 with empty arrays rather than the 503
  the point-in-time route returns — an empty window is a complete answer (#6641).
- `GET /api/console/machine-status/stream` is a `text/event-stream`. It opens
  with one `history` event carrying the current window, then sends a `sample`
  event per new sample and a `transition` event per service state change. A
  subscriber that falls behind the broadcast buffer gets a `lagged` event naming
  the dropped count instead of a silent gap (#6641).
- A per-service transition log records only the moments a service's derived state
  (`up` / `degraded` / `down` / `unknown`) changed — never a row per poll. A
  service whose report goes stale past a 60 s grace window, or that drops out of
  the report set for that long, transitions to `down`; a retained cache entry is
  not evidence a service is alive (#6641).
- `serve --host-sample-interval` sets the host sampling cadence, default 5 s. It
  is independent of `--poll-interval` (still 15 s), which drives the stdio-MCP
  service polls. The history payload advertises the configured value so the
  graph's x-axis follows it (#6641).
