Fixed

- Memory recall no longer hides a broken store: store-open and embedder-init
  failures log at `warn!` with the error chain instead of `debug!`. Recall stays
  best-effort and still returns an empty result (audit 2026-08-19, finding 19).
- `/provider bedrock` and `/provider local` get their own `LlmCredentials`
  variants instead of reusing `OpenRouter` as a placeholder, so the deployment
  footer and startup banner name the transport actually in use. Routing is
  unchanged — the adapter dispatches on the model prefix (audit 2026-08-19,
  finding 18).
- The code-index advisory file lock is acquired on `spawn_blocking`. The
  blocking `flock` call no longer parks a Tokio worker thread while another
  process holds the lock (audit 2026-08-19, finding 21).
- `GET /api/agents/:name/stores` validates the agent name against an allowlist
  (ASCII alphanumeric, `_`, `-`, bounded length) rather than a four-item
  denylist that admitted NUL bytes, control characters, and Unicode separator
  look-alikes into a filesystem path join (audit 2026-08-19, finding 12).
