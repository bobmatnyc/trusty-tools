Added

- A console-hosted event-bus core (DOC-73 §4.1-§4.3): a bounded in-memory ring
  (default capacity 8192, oldest-evicted), a UDS ingest listener at
  `daemon_socket_path("trusty-console")` accepting newline-delimited
  `HarnessEvent` JSON, dedup by event id, and a `tokio::sync::broadcast`
  subscriber seam for the SSE fan-out a later slice adds. A malformed or
  oversized line drops only its own connection; the listener keeps serving
  every other producer. The durable day-rotated NDJSON log and the
  console-assigned `seq` from the wider #6848 scope are deferred to a
  follow-up PR.
