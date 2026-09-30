Added

- `tm fleet init` registers a bound Architect with the daemon, so `tm ls`
  lists it as a protected `supervisor` record under a stable id, pinned first
  and tagged `[architect]`. Its poller (`<session>-poll`) and collector
  (`<session>-collector`) are registered as `supervisor_aux` records, tagged
  `[architect-helper]`, when their pane runs in the Architect directory and
  no `claude` runs in it, the pane's own process included. When that cannot
  be read, the helper is not registered. A registration replaces the Architect's record,
  and marks deleted, record-only, any other live record with the same tmux
  name and every other Architect record that is not already deleted or
  decommissioned, stopped and errored ones included, so a stale one no
  longer protects its name. With no daemon running, init prints `NOT
  registered` as a warning and still succeeds; a refused or failed
  registration fails init (#8942).
- `POST /api/v1/sessions/managed/supervisor` (socket method
  `mpm.managed.register_supervisor`) registers the Architect's sessions only
  for a `claude` the launch record binds to the directory: 200 with the
  report, 400 for a malformed request (including a helper name other than
  `<session>-poll` or `<session>-collector`), 403 when the session is not the bound
  Architect, 500 when the store cannot be written.
- `tm fleet init` removes an Architect launch sidecar only when its pid is
  dead, no process has its recorded start time, and no tmux session with its
  name exists. When any of these cannot be determined, the sidecar is kept.
