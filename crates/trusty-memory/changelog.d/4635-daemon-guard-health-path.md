Fixed

- **`trusty-memory monitor web` no longer reports a healthy daemon as down
  (issue #4635).** `commands::daemon_guard` probed `{base}/api/v1/health` in
  two places — `probe_health()` and the `health_url` field it handed to
  `spin_until_ready` — but `web::router` registers only `GET /health`
  (`src/web/mod.rs:190`). Against a live daemon on :7070, `/health` answers 200
  and `/api/v1/health` answers 404, so the guard always took the cold path: it
  printed `◉ Starting trusty-memory daemon…`, spawned a duplicate daemon (which
  self-exited via `single_instance_check`), then spun on the dead URL for the
  full 30 s startup budget and errored out, leaving the dashboard unreachable
  from its documented entry point. Both probe sites now derive the URL from a
  single `health_url()` helper — the drift between them is what allowed the two
  to disagree — matching the `/health` path already used by
  `commands::single_instance`, `commands::start`, and
  `trusty_common::monitor::memory_client`.

- **`trusty-memory port` help text no longer instructs a 404 route
  (issue #4635).** The `--help` example and two `commands::port` doc comments
  told users to `curl http://127.0.0.1:$(trusty-memory port)/api/v1/health`;
  they now name `/health`.
