Added
- **`monitor::memory_client::ActivityFeed`** — a live subscription to `memory.activity_stream`, draining onto a background task so the TUI's render tick can take what arrived without blocking. The monitor's activity log consumes it instead of the 2-second `memory.activity` poll, so an event appears as it happens ([#6286](https://github.com/bobmatnyc/trusty-tools/issues/6286))
  - a stream that ends — a daemon restart, a socket that went away, a terminal error frame — flips `is_live()` and records why on `last_error()`. The TUI says so in the log and falls back to polling, then retries the stream on the next tick so a restarted daemon re-attaches with no operator action. Blanking the log instead would present a live daemon as an idle one
  - the poll's cursor advances on every tick whether or not the feed is live, so a fallback resumes where the stream left off rather than replaying

Changed
- The monitor's activity log no longer shows an event up to a tick late, and no longer misses one evicted from the activity log between two ticks. A daemon that predates `memory.activity_stream` still works: the failed open is reported and the poll carries the log
