Added

- `tm fleet init` registers a bound Architect with the daemon, so `tm ls`
  lists it as a protected `supervisor` record under a stable id, pinned first
  and tagged `[architect]`. Its poller (under the tmux name it actually runs
  as) and collector are registered as `supervisor_aux` records when their
  pane runs in the Architect directory, tagged `[architect-helper]`. A
  relaunch replaces the record and marks any other live record with the same
  tmux name deleted, record-only. With no daemon running, init prints `NOT
  registered` as a warning and still succeeds; a refused or failed
  registration fails init (#8942).
- `POST /api/v1/sessions/managed/supervisor` (socket method
  `mpm.managed.register_supervisor`) registers the Architect's sessions only
  for a `claude` the launch record binds to the directory: 200 with the
  report, 400 for a malformed request, 403 when the session is not the bound
  Architect, 500 when the store cannot be written.
- `tm fleet init` removes an Architect launch sidecar only when its pid is
  dead, no process has its recorded start time, and no tmux session with its
  name exists. When any of these cannot be determined, the sidecar is kept.
