Added

- The Services list's `trusty-console` row now opens a details pane for the
  console itself: version, uptime, CPU and resident memory with the same
  side-by-side graphs the roster rows draw, and the count of browser streams
  attached to the machine-status SSE endpoint. Bus status, service connections
  and message rates are stated as not yet available — they wait on the
  event-bus transport (#6460).
- `GET /health` now reports `uptime_secs`, the whole seconds this console has
  been serving. Additive: a caller reading only `status` and `version` is
  unaffected.
- The SPA expected machine-status history schema 3 and logged a mismatch
  warning on every page load against a daemon serving schema 4 (#6915). It now
  expects 4, and a test asserts the constant against the Rust one.
