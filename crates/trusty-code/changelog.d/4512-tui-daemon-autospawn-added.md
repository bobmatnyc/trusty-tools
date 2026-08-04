Added

- **`GET /health` (and the `health` JSON-RPC method) now report the daemon's
  project binding and pid
  ([#4512](https://github.com/bobmatnyc/trusty-tools/issues/4512)).**
  Additive only — `server`, `version`, and `status` are unchanged, so existing
  probes keep working. A daemon binds exactly one `ProjectBinding` at
  `serve::build_router` time and holds it for its whole life, but published
  nothing about it, so a client had no way to tell which project a daemon it
  found was serving. `binding` uses the same `{state, root}` wire shape
  `Session` already serialises.
