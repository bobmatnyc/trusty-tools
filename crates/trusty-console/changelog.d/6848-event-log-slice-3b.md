Added

- A durable, day-rotated NDJSON event log for the console event-bus (DOC-73
  §4.3, issue #6848 slice 3b), in `crates/trusty-console/src/event_bus/log/`:
  console now assigns the single `seq` on every accepted frame (overwriting
  whatever the producer stamped), recovers that counter's high-water mark from
  the log's tail on restart (a truncated final line is skipped, not fatal),
  and retains a configurable number of days of history with rotation that
  keeps `seq` continuous across the boundary. All log I/O runs on a dedicated
  writer task behind a bounded channel, so a slow disk never blocks ingest — a
  full channel drops the write and counts it (`EventBusMetrics::log_dropped`)
  rather than blocking or failing. A reconnecting subscriber can replay
  everything persisted after a given `seq`; a request that predates retention,
  or a range a dropped write left missing, comes back as an explicit gap
  marker rather than silence. Live-delivered events on the broadcast channel
  now carry a `persisted` marker, `false` for everything fanned out on this
  path — durability is confirmed only for events read back from the log.
